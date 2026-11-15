//! `CommandsService`: desired state, controller only (plan 4.5).
//!
//! A set-state call returns once the store holds the change; the reconciler
//! carries it to DCS. Every call carries the lease and an idempotency key
//! ([`Hub::write`]).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::commands_service_server::CommandsService;
use nanoprobe_proto::v1alpha1::{
    self as pb, AdjustWarehouseStockRequest, AdjustWarehouseStockResponse, CaptureRequest,
    CaptureResponse, DematerializeRequest, DematerializeResponse, DoEffectRequest,
    DoEffectResponse, ErrorReason, GetOutcomeRequest, GetOutcomeResponse, GroupCategory,
    MaterializeRequest, MaterializeResponse, PutGroupRequest, PutGroupResponse, PutStaticRequest,
    PutStaticResponse, RemoveEntityRequest, RemoveEntityResponse, SetAirbaseOwnerRequest,
    SetAirbaseOwnerResponse, SetLostPolicyRequest, SetLostPolicyResponse, SetOptionsRequest,
    SetOptionsResponse, SetOrderRequest, SetOrderResponse, SetSampleRatesRequest,
    SetSampleRatesResponse, SetWarehouseStockRequest, SetWarehouseStockResponse, Write,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::reconciler::EffectResult;
use nanoprobe_sidecar::sim::wire::Body;
use nanoprobe_sidecar::stock::{Item, Stock};
use nanoprobe_sidecar::store::{
    Authority, Kind, LostPolicy, NewGroup, NewUnit, Outcome, Point, Store, StoreError, UnitState,
};
use nanoprobe_transport::records::Rates;
use prost::Message;
use tonic::{Code, Request, Response, Status};

use crate::auth::{Access, require};
use crate::convert::{
    dcs_category, dcs_coalition, entity, finite, intent, invalid, not_built, not_found,
};
use crate::hub::{Call, Hub, Inner, Need, Slot, settle};
use crate::idem::EffectKind;
use crate::lua::{extra_to_lua, from_lua, to_lua};
use nanoprobe_transport::codec::{Lv, Writer};

/// The service.
pub struct Commands(pub Arc<Hub>);

pub(crate) fn store_error(e: StoreError) -> Status {
    match e {
        StoreError::NoSuchEntity(id) => not_found(id.0),
        StoreError::TooLarge(why) => {
            invalid("spec", format!("a Lua value cannot be stored: {why}"))
        }
        StoreError::Exhausted => status(
            Code::Internal,
            ErrorReason::Internal,
            "the campaign has used every entity ID",
            0,
            "",
        ),
        StoreError::UnsupportedTarget(id) => status(
            Code::InvalidArgument,
            ErrorReason::InvalidField,
            format!("{id} is not a group, a unit or a static of the campaign"),
            id.0,
            "order.attack.target_id",
        ),
        StoreError::NoTransition(id, from, t) => status(
            Code::Internal,
            ErrorReason::Internal,
            format!("no transition from {} on {t:?}", from.name()),
            id.0,
            "",
        ),
    }
}

/// The request with the lease left out, for the idempotency digest.
pub(crate) fn unleased(w: &Option<Write>) -> Option<Write> {
    w.clone().map(|mut w| {
        w.lease = None;
        w
    })
}

macro_rules! changing {
    ($self:ident, $req:ident, $method:literal, $need:expr, $f:expr) => {{
        require(&$req, Access::Controller)?;
        let r = $req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let f = $f;
        let resp = $self
            .0
            .write(
                Call {
                    method: $method,
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: $need,
                },
                &hashed,
                |g: &mut Inner, c: u64| f(g, c, &r).map(|x| (x, c)),
            )
            .await?;
        Ok(Response::new(resp))
    }};
}

pub(crate) use changing;

/// A checked position on one of the campaign's theaters.
pub(crate) fn place(p: Option<&pb::Position>, slot: &Slot, field: &str) -> Result<Point, Status> {
    let p = p.ok_or_else(|| invalid(field, "a position is required"))?;
    if p.theater.is_empty() {
        return Err(invalid(
            &format!("{field}.theater"),
            "every position names its theater",
        ));
    }
    if !slot.meta.theaters.contains(&p.theater) {
        return Err(invalid(
            &format!("{field}.theater"),
            format!("the campaign does not use the theater `{}`", p.theater),
        ));
    }
    Ok(Point {
        x: finite(p.north, &format!("{field}.north"))?,
        y: finite(p.alt, &format!("{field}.alt"))?,
        z: finite(p.east, &format!("{field}.east"))?,
    })
}

/// Bytes of a value in the agent's `lv` encoding.
pub(crate) fn lv_len(v: &Lv) -> usize {
    let mut w = Writer::new();
    w.lv(v);
    w.len()
}

/// Bytes a command may take besides the values counted: the command header,
/// names, numbers and table keys. Generous, so an estimate never falls
/// short of what the reconciler builds.
pub(crate) const OVERHEAD: usize = 512;

/// An upper bound on the payload of the spawn the reconciler builds for a
/// spec, and of the order for a route.
fn spawn_bound(spec: &pb::GroupSpec) -> usize {
    OVERHEAD
        + lv_len(&extra_to_lua(spec.extra.as_ref()))
        + spec
            .units
            .iter()
            .map(|u| {
                OVERHEAD + u.r#type.len() + u.skill.len() + lv_len(&extra_to_lua(u.extra.as_ref()))
            })
            .sum::<usize>()
}

fn order_bound(o: Option<&pb::Order>) -> usize {
    match o.and_then(|o| o.intent.as_ref()) {
        Some(pb::order::Intent::Route(r)) => OVERHEAD + r.waypoints.len() * OVERHEAD,
        _ => OVERHEAD,
    }
}

/// `INVALID_FIELD` when a command for the call would be over the agent's
/// limit (spec/errors.md, "Command outcomes").
pub(crate) fn fits(bytes: usize, limit: usize, field: &str) -> Result<(), Status> {
    if bytes > limit {
        Err(invalid(
            field,
            format!("the command would take up to {bytes} bytes; the agent takes {limit}"),
        ))
    } else {
        Ok(())
    }
}

fn set_components(
    store: &mut Store,
    id: EntityId,
    want: &std::collections::BTreeMap<String, prost_types::Struct>,
) -> Result<(), Status> {
    let have = store
        .group(id)
        .ok_or_else(|| not_found(id.0))?
        .components
        .clone();
    for name in have.keys() {
        if !want.contains_key(name) {
            store.set_component(id, name, None).map_err(store_error)?;
        }
    }
    for (name, v) in want {
        let bytes = v.encode_to_vec();
        if have.get(name) != Some(&bytes) {
            store
                .set_component(id, name, Some(bytes))
                .map_err(store_error)?;
        }
    }
    Ok(())
}

fn put_group(g: &mut Inner, c: u64, r: &PutGroupRequest) -> Result<PutGroupResponse, Status> {
    let spec = r
        .spec
        .as_ref()
        .ok_or_else(|| invalid("spec", "a spec is required"))?;
    let category = GroupCategory::try_from(spec.category)
        .ok()
        .and_then(dcs_category)
        .ok_or_else(|| invalid("spec.category", "a group category is required"))?;
    if spec.units.is_empty() {
        return Err(invalid("spec.units", "a group has at least one unit"));
    }
    for (i, u) in spec.units.iter().enumerate() {
        let f = format!("spec.units[{i}]");
        if u.r#type.is_empty() {
            return Err(invalid(&format!("{f}.type"), "a unit needs its DCS type"));
        }
        if u.life_percent.is_some() || u.fuel.is_some() || !u.ammo.is_empty() {
            return Err(invalid(
                &f,
                "spawning with a captured life, fuel or ammunition is not built yet",
            ));
        }
        finite(u.heading, &format!("{f}.heading"))?;
    }
    // Every check comes before the first store write: a call that fails
    // changes nothing, so a repeat under its key cannot make a second group.
    if spec.components.keys().any(String::is_empty) {
        return Err(invalid("spec.components", "a component needs a name"));
    }
    let limit = g.backend.max_command();
    fits(spawn_bound(spec), limit, "spec")?;
    fits(order_bound(r.order.as_ref()), limit, "order")?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    if r.group_id != 0 {
        return replace_group(slot, r, category);
    }
    let theater = spec.units[0]
        .position
        .as_ref()
        .map(|p| p.theater.clone())
        .unwrap_or_default();
    let mut units = Vec::with_capacity(spec.units.len());
    for (i, u) in spec.units.iter().enumerate() {
        let f = format!("spec.units[{i}]");
        if u.id != 0 {
            return Err(invalid(&format!("{f}.id"), "a new group's units have ID 0"));
        }
        let at = place(u.position.as_ref(), slot, &format!("{f}.position"))?;
        if u.position.as_ref().is_some_and(|p| p.theater != theater) {
            return Err(invalid(
                &format!("{f}.position.theater"),
                "every unit of a group is on one theater",
            ));
        }
        units.push(NewUnit {
            label: u.label.clone(),
            skill: u.skill.clone(),
            extra: extra_to_lua(u.extra.as_ref()),
            ..NewUnit::new(
                &u.r#type,
                UnitState {
                    at,
                    heading: u.heading,
                    life: 0.0,
                    fuel: 0.0,
                },
            )
        });
    }
    let order = match &r.order {
        Some(o) => Some(intent(Some(o), &theater, "order", &slot.store)?),
        None => None,
    };
    let id = slot
        .store
        .create_group(NewGroup {
            theater,
            country: spec.country,
            category,
            // DCS takes a group's coalition from its country.
            coalition: 0,
            units,
            authority: if spec.r#virtual {
                Authority::Campaign
            } else {
                Authority::Sim
            },
            label: spec.label.clone(),
            extra: extra_to_lua(spec.extra.as_ref()),
        })
        .map_err(store_error)?;
    set_components(&mut slot.store, id, &spec.components)?;
    if let Some(o) = order {
        slot.store.set_order(id, o).map_err(store_error)?;
    }
    let grp = slot.store.group(id).expect("created");
    Ok(PutGroupResponse {
        group_id: id.0,
        unit_ids: grp.members.iter().map(|m| m.0).collect(),
        spec_revision: grp.spec_rev,
        revision: slot.store.revision(),
    })
}

fn replace_group(
    slot: &mut Slot,
    r: &PutGroupRequest,
    category: u32,
) -> Result<PutGroupResponse, Status> {
    let spec = r.spec.as_ref().expect("checked");
    let id = EntityId(r.group_id);
    let grp = slot
        .store
        .group(id)
        .filter(|x| x.kind == Kind::Group)
        .ok_or_else(|| not_found(r.group_id))?
        .clone();
    let members: BTreeSet<u64> = grp.members.iter().map(|m| m.0).collect();
    let named: BTreeSet<u64> = spec.units.iter().map(|u| u.id).collect();
    if named != members || named.len() != spec.units.len() {
        return Err(invalid(
            "spec.units",
            "a replacement names exactly the group's units by ID; adding or removing units is not built yet",
        ));
    }
    if grp.category != category || grp.country != spec.country {
        return Err(invalid(
            "spec",
            "changing a group's category or country is not built yet",
        ));
    }
    let wanted = if spec.r#virtual {
        Authority::Campaign
    } else {
        Authority::Sim
    };
    if wanted != grp.authority {
        return Err(invalid(
            "spec.virtual",
            "use Materialize or Dematerialize to change who simulates a group",
        ));
    }
    for (i, u) in spec.units.iter().enumerate() {
        if u.position.is_some() {
            return Err(invalid(
                &format!("spec.units[{i}].position"),
                "a replacement keeps each unit's position as DCS has it; leave it out",
            ));
        }
    }
    let order = match &r.order {
        Some(o) => Some(intent(Some(o), &grp.theater, "order", &slot.store)?),
        None => None,
    };
    // Checked: apply. Each setter stamps the store revision; the spec ones
    // also raise the spec revision.
    let s = &mut slot.store;
    if grp.label != spec.label {
        s.set_label(id, &spec.label).map_err(store_error)?;
    }
    let extra = extra_to_lua(spec.extra.as_ref());
    if grp.extra != extra {
        s.set_extra(id, extra).map_err(store_error)?;
    }
    for u in &spec.units {
        let uid = EntityId(u.id);
        let cur = s.unit(uid).ok_or_else(|| not_found(u.id))?.clone();
        if cur.label != u.label {
            s.set_label(uid, &u.label).map_err(store_error)?;
        }
        if cur.skill != u.skill {
            s.set_skill(uid, &u.skill).map_err(store_error)?;
        }
        let extra = extra_to_lua(u.extra.as_ref());
        if cur.extra != extra {
            s.set_extra(uid, extra).map_err(store_error)?;
        }
        if cur.type_name != u.r#type {
            s.set_unit_type(uid, &u.r#type).map_err(store_error)?;
        }
    }
    set_components(s, id, &spec.components)?;
    if let Some(o) = order {
        s.set_order(id, o).map_err(store_error)?;
    }
    let grp = s.group(id).expect("exists");
    Ok(PutGroupResponse {
        group_id: id.0,
        unit_ids: grp.members.iter().map(|m| m.0).collect(),
        spec_revision: grp.spec_rev,
        revision: s.revision(),
    })
}

fn put_static(g: &mut Inner, c: u64, r: &PutStaticRequest) -> Result<PutStaticResponse, Status> {
    if r.static_id != 0 {
        return Err(invalid(
            "static_id",
            "replacing a static's spec is not built yet",
        ));
    }
    let spec = r
        .spec
        .as_ref()
        .ok_or_else(|| invalid("spec", "a spec is required"))?;
    if spec.r#type.is_empty() {
        return Err(invalid("spec.type", "a static needs its DCS type"));
    }
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let at = place(spec.position.as_ref(), slot, "spec.position")?;
    let theater = spec.position.as_ref().expect("placed").theater.clone();
    let heading = finite(spec.heading, "spec.heading")?;
    fits(
        2 * OVERHEAD + spec.r#type.len() + lv_len(&extra_to_lua(spec.extra.as_ref())),
        g.backend.max_command(),
        "spec",
    )?;
    let id = slot
        .store
        .create_static(NewGroup {
            theater,
            country: spec.country,
            category: 0,
            coalition: 0,
            units: vec![NewUnit {
                extra: extra_to_lua(spec.extra.as_ref()),
                ..NewUnit::new(
                    &spec.r#type,
                    UnitState {
                        at,
                        heading,
                        life: 0.0,
                        fuel: 0.0,
                    },
                )
            }],
            authority: Authority::Sim,
            label: spec.label.clone(),
            extra: nanoprobe_transport::codec::Lv::Nil,
        })
        .map_err(store_error)?;
    Ok(PutStaticResponse {
        static_id: id.0,
        spec_revision: slot.store.group(id).map_or(0, |x| x.spec_rev),
        revision: slot.store.revision(),
    })
}

fn set_order(g: &mut Inner, c: u64, r: &SetOrderRequest) -> Result<SetOrderResponse, Status> {
    let limit = g.backend.max_command();
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let id = EntityId(r.group_id);
    let grp = slot
        .store
        .group(id)
        .filter(|g| !g.removed)
        .ok_or_else(|| not_found(r.group_id))?;
    if grp.kind != Kind::Group {
        return Err(invalid("group_id", "only a group takes an order"));
    }
    let theater = grp.theater.clone();
    let o = intent(r.order.as_ref(), &theater, "order", &slot.store)?;
    fits(order_bound(r.order.as_ref()), limit, "order")?;
    let order_revision = slot.store.set_order(id, o).map_err(store_error)?;
    Ok(SetOrderResponse {
        order_revision,
        revision: slot.store.revision(),
    })
}

fn set_options(g: &mut Inner, c: u64, r: &SetOptionsRequest) -> Result<SetOptionsResponse, Status> {
    let limit = g.backend.max_command();
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let id = EntityId(r.group_id);
    match slot.store.group(id).filter(|g| !g.removed) {
        Some(x) if x.kind == Kind::Group => {}
        Some(_) => return Err(invalid("group_id", "only a group has controller options")),
        None => return Err(not_found(r.group_id)),
    }
    if r.options.is_empty() {
        return Err(invalid("options", "name at least one option"));
    }
    let mut values = Vec::with_capacity(r.options.len());
    for (option, v) in &r.options {
        let value = to_lua(v);
        fits(
            OVERHEAD + lv_len(&value),
            limit,
            &format!("options[{option}]"),
        )?;
        values.push((*option, value));
    }
    for (option, value) in values {
        slot.store
            .set_option(id, option, value)
            .map_err(store_error)?;
    }
    Ok(SetOptionsResponse {
        revision: slot.store.revision(),
    })
}

fn remove_entity(
    g: &mut Inner,
    c: u64,
    r: &RemoveEntityRequest,
) -> Result<RemoveEntityResponse, Status> {
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let id = EntityId(r.entity_id);
    if slot.store.marker(id).is_some() {
        // A submenu takes the items under it.
        slot.store
            .remove_marker(id)
            .map_err(|_| not_found(r.entity_id))?;
        return Ok(RemoveEntityResponse {
            revision: slot.store.revision(),
        });
    }
    if slot.store.group(id).is_none() {
        return Err(if slot.store.unit(id).is_some() {
            invalid("entity_id", "a unit leaves with its group")
        } else {
            not_found(r.entity_id)
        });
    }
    slot.store.remove_entity(id).map_err(store_error)?;
    Ok(RemoveEntityResponse {
        revision: slot.store.revision(),
    })
}

/// The theater of a warehouse or airbase call, one the campaign uses.
fn base_theater(slot: &Slot, theater: &str, airbase: &str) -> Result<(), Status> {
    if airbase.is_empty() {
        return Err(invalid("airbase", "an airbase name is required"));
    }
    if !slot.meta.theaters.iter().any(|t| t == theater) {
        return Err(invalid(
            "theater",
            format!("the campaign does not use the theater `{theater}`"),
        ));
    }
    Ok(())
}

/// A stock from the three maps of a warehouse call. `signed` lets amounts
/// be negative, as an adjustment's are.
fn stock_of(
    weapon: &std::collections::BTreeMap<String, f64>,
    aircraft: &std::collections::BTreeMap<String, f64>,
    liquids: &std::collections::BTreeMap<u32, f64>,
    signed: bool,
) -> Result<Stock, Status> {
    let mut s = Stock::default();
    let mut put = |item: Item, v: f64, field: String| {
        if !v.is_finite() || (!signed && v < 0.0) {
            return Err(invalid(
                &field,
                if signed {
                    "an amount is finite"
                } else {
                    "a count is finite and 0 or more"
                },
            ));
        }
        s.set(item, v);
        Ok(())
    };
    for (k, v) in weapon {
        put(Item::Weapon(k.clone()), *v, format!("weapon[{k}]"))?;
    }
    for (k, v) in aircraft {
        put(Item::Aircraft(k.clone()), *v, format!("aircraft[{k}]"))?;
    }
    for (k, v) in liquids {
        put(Item::Liquid(*k), *v, format!("liquids[{k}]"))?;
    }
    if s.is_empty() {
        return Err(invalid("stock", "name at least one item"));
    }
    Ok(s)
}

fn set_airbase_owner(
    g: &mut Inner,
    c: u64,
    r: &SetAirbaseOwnerRequest,
) -> Result<SetAirbaseOwnerResponse, Status> {
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    base_theater(slot, &r.theater, &r.airbase)?;
    let owner =
        dcs_coalition(r.owner).ok_or_else(|| invalid("owner", "a coalition is required"))?;
    slot.store.set_airbase_owner(&r.theater, &r.airbase, owner);
    Ok(SetAirbaseOwnerResponse {
        revision: slot.store.revision(),
    })
}

fn set_warehouse_stock(
    g: &mut Inner,
    c: u64,
    r: &SetWarehouseStockRequest,
) -> Result<SetWarehouseStockResponse, Status> {
    let limit = g.backend.max_command();
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    base_theater(slot, &r.theater, &r.airbase)?;
    // `stock` is the deprecated alias of `weapon`; `weapon` wins.
    let mut weapons = r.stock.clone();
    weapons.extend(r.weapon.clone());
    let stock = stock_of(&weapons, &r.aircraft, &r.liquids, false)?;
    fits(
        OVERHEAD + lv_len(&stock.to_lv()) + r.airbase.len(),
        limit,
        "stock",
    )?;
    slot.store.set_stock(&r.theater, &r.airbase, &stock);
    Ok(SetWarehouseStockResponse {
        revision: slot.store.revision(),
    })
}

fn set_sample_rates(
    g: &mut Inner,
    c: u64,
    r: &SetSampleRatesRequest,
) -> Result<SetSampleRatesResponse, Status> {
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    for (v, f) in [
        (r.position_period, "position_period"),
        (r.vitals_period, "vitals_period"),
        (r.ammo_period, "ammo_period"),
        (r.warehouse_period, "warehouse_period"),
        (r.enumeration_period, "enumeration_period"),
    ] {
        if !(v.is_finite() && v >= 0.0) {
            return Err(invalid(f, "a period is finite and 0 or more"));
        }
    }
    slot.store.set_rates(Rates {
        position_s: r.position_period,
        vitals_s: r.vitals_period,
        ammo_s: r.ammo_period,
        warehouse_s: r.warehouse_period,
        enumeration_s: r.enumeration_period,
    });
    Ok(SetSampleRatesResponse {})
}

/// The API's command result of an effect's or a query's end: none is
/// `pending`.
pub(crate) fn command_result(r: Option<&EffectResult>) -> pb::CommandResult {
    let Some(r) = r else {
        return pb::CommandResult {
            outcome: pb::Outcome::Pending as i32,
            ..pb::CommandResult::default()
        };
    };
    let outcome = match r.outcome {
        Outcome::Applied => pb::Outcome::Applied,
        Outcome::Refused => pb::Outcome::Refused,
        Outcome::Rejected => pb::Outcome::Rejected,
        Outcome::Unknown => pb::Outcome::Unknown,
        Outcome::NotSent => pb::Outcome::NotSent,
    };
    pb::CommandResult {
        outcome: outcome as i32,
        message: r
            .result
            .as_ref()
            .map(|x| x.message.clone())
            .unwrap_or_default(),
        cost_us: r.result.as_ref().map_or(0, |x| x.cost_us),
        applied: None,
        // The numbers are the agent's codes plus one.
        code: r.result.as_ref().map_or(0, |x| i32::from(x.code) + 1),
    }
}

/// The value a Lua call returned, when the agent answered `APPLIED` with one.
fn lua_value(r: Option<&EffectResult>) -> Option<prost_types::Value> {
    let r = r?.result.as_ref()?;
    (u64::from(r.code) == nanoprobe_transport::layout::result::APPLIED && r.value != Lv::Nil)
        .then(|| from_lua(&r.value))
}

/// The encoded response of an effect or a query call from its result, none
/// being `pending`. A capture's entity is the store's after it took what DCS
/// read.
pub fn effect_response(
    g: &mut Inner,
    c: u64,
    kind: EffectKind,
    result: Option<&EffectResult>,
) -> Vec<u8> {
    let result_pb = Some(command_result(result));
    match kind {
        EffectKind::Adjust => AdjustWarehouseStockResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Damage => DoEffectResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Message => pb::SendMessageResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Chat => pb::SendChatResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Invoke => pb::InvokeLuaResponse {
            result: result_pb,
            value: lua_value(result),
        }
        .encode_to_vec(),
        EffectKind::Eval => pb::EvalLuaResponse {
            result: result_pb,
            value: lua_value(result),
        }
        .encode_to_vec(),
        EffectKind::Kick => pb::KickResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Ban => pb::BanResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Unban => pb::UnbanResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Move => pb::MovePlayerResponse { result: result_pb }.encode_to_vec(),
        EffectKind::Bans => pb::ListBansResponse {
            bans: if result.is_some_and(|r| r.outcome == Outcome::Applied) {
                super::moderation::bans_of(g, result)
            } else {
                Vec::new()
            },
            result: result_pb,
        }
        .encode_to_vec(),
        EffectKind::Capture(id) => {
            let applied = result.is_some_and(|r| r.outcome == Outcome::Applied);
            let entity = applied
                .then(|| {
                    let store = &g.campaigns.get(&c)?.store;
                    store.group(EntityId(id)).map(|grp| entity(store, grp))
                })
                .flatten();
            CaptureResponse {
                result: result_pb,
                entity,
            }
            .encode_to_vec()
        }
    }
}

/// The error for a command that cannot be sent now.
pub(crate) fn dcs_not_ready() -> Status {
    status(
        Code::Unavailable,
        ErrorReason::DcsNotReady,
        "DCS is not attached, or no mission of the campaign runs",
        0,
        "",
    )
}

fn adjust_start(
    g: &mut Inner,
    c: u64,
    r: &AdjustWarehouseStockRequest,
) -> Result<(u64, u64), Status> {
    let limit = g.backend.max_command();
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    base_theater(slot, &r.theater, &r.airbase)?;
    let delta = stock_of(&r.weapon, &r.aircraft, &r.liquids, true)?;
    fits(
        OVERHEAD + lv_len(&delta.to_lv()) + r.airbase.len(),
        limit,
        "delta",
    )?;
    if let Some(l) = slot.store.loaded()
        && !l.theater.is_empty()
        && l.theater != r.theater
    {
        return Err(status(
            Code::FailedPrecondition,
            ErrorReason::TheaterNotLoaded,
            format!("`{}` is loaded, not `{}`", l.theater, r.theater),
            0,
            "theater",
        ));
    }
    g.with_rec(c, |rec, store, sim| {
        rec.adjust_warehouse(store, sim, &r.airbase, &delta)
    })
    .flatten()
    .ok_or_else(dcs_not_ready)
}

/// What the effect sends, for the effects `DoEffect` builds.
fn effect_start(g: &mut Inner, c: u64, e: &pb::effect::Effect) -> Result<(u64, u64), Status> {
    match e {
        pb::effect::Effect::Damage(d) => damage_start(g, c, d),
        pb::effect::Effect::Sound(s) => sound_start(g, c, s),
        pb::effect::Effect::Picture(p) => picture_start(g, c, p),
        pb::effect::Effect::PictureStop(_) => g
            .with_rec(c, |rec, store, sim| rec.picture_stop(store, sim))
            .flatten()
            .ok_or_else(dcs_not_ready),
        _ => Err(not_built("DoEffect for this effect")),
    }
}

/// The scope code of a sound or picture, with the target checked: a group
/// or a unit of the campaign that DCS holds, or a number that fits a DCS
/// coalition or country number.
pub(crate) fn scope_of(
    g: &mut Inner,
    c: u64,
    scope: i32,
    target: u64,
    field: &str,
) -> Result<u32, Status> {
    let code = match pb::EffectScope::try_from(scope) {
        Ok(pb::EffectScope::All) => 1,
        Ok(pb::EffectScope::Coalition) => 2,
        Ok(pb::EffectScope::Group) => 3,
        Ok(pb::EffectScope::Unit) => 4,
        Ok(pb::EffectScope::Country) => 5,
        _ => return Err(invalid(&format!("{field}.scope"), "a scope is required")),
    };
    match code {
        2 | 5 if target > u64::from(u32::MAX) => Err(invalid(
            &format!("{field}.target"),
            "a coalition or country number fits 32 bits",
        )),
        3 | 4 => {
            let id = EntityId(target);
            let slot = g.campaigns.get(&c).expect("resolved campaign");
            let known = if code == 3 {
                slot.store.group(id).is_some_and(|x| !x.removed)
            } else {
                slot.store.unit(id).is_some()
            };
            // An effect may name an adopted unit or group, which the store
            // holds no row of.
            let adopted = g
                .with_rec(c, |rec, _, _| rec.is_adopted(id))
                .unwrap_or(false);
            if !known && !adopted {
                return Err(not_found(target));
            }
            let bound = g
                .with_rec(c, |rec, _, _| rec.is_bound(id))
                .ok_or_else(dcs_not_ready)?;
            if !bound {
                return Err(status(
                    Code::FailedPrecondition,
                    ErrorReason::EntityNotFound,
                    format!("DCS holds no object of entity {target}"),
                    target,
                    &format!("{field}.target"),
                ));
            }
            Ok(code)
        }
        _ => Ok(code),
    }
}

fn sound_start(g: &mut Inner, c: u64, s: &pb::Sound) -> Result<(u64, u64), Status> {
    let scope = scope_of(g, c, s.scope, s.target, "effect.sound")?;
    if s.file.is_empty() {
        return Err(invalid("effect.sound.file", "a file name is required"));
    }
    g.with_rec(c, |rec, store, sim| {
        rec.sound(store, sim, scope, s.target, &s.file)
    })
    .flatten()
    .ok_or_else(dcs_not_ready)
}

fn picture_start(g: &mut Inner, c: u64, p: &pb::Picture) -> Result<(u64, u64), Status> {
    if !(p.seconds.is_finite() && (0.0..=86_400.0).contains(&p.seconds)) {
        return Err(invalid("effect.picture.seconds", "finite, 0 to 86400"));
    }
    if !(p.size.is_finite() && (0.0001..=10_000.0).contains(&p.size)) {
        return Err(invalid("effect.picture.size", "finite, 0.0001 to 10000"));
    }
    if !nanoprobe_transport::picture_file_ok(&p.file) {
        return Err(invalid(
            "effect.picture.file",
            "1 to 255 bytes of letters, digits, space and _.-/()+,#",
        ));
    }
    let scope = scope_of(g, c, p.scope, p.target, "effect.picture")?;
    let body = Body::Picture {
        scope,
        target: p.target,
        clear_view: u32::from(p.clear_view),
        seconds: p.seconds,
        size: p.size,
        horizontal: p.horizontal,
        vertical: p.vertical,
        size_units: p.size_units,
        file: p.file.clone(),
    };
    g.with_rec(c, |rec, store, sim| rec.picture(store, sim, body))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

fn damage_start(g: &mut Inner, c: u64, d: &pb::Damage) -> Result<(u64, u64), Status> {
    if !(d.life_percent.is_finite() && (0.0..=100.0).contains(&d.life_percent)) {
        return Err(invalid(
            "effect.damage.life_percent",
            "a life is a percentage, 0 to 100",
        ));
    }
    let unit = EntityId(d.unit_id);
    let adopted = g
        .with_rec(c, |rec, _, _| rec.is_adopted(unit))
        .unwrap_or(false);
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    if slot.store.unit(unit).is_none() && !adopted {
        return Err(not_found(d.unit_id));
    }
    let bound = g
        .with_rec(c, |rec, _, _| rec.is_bound(unit))
        .ok_or_else(dcs_not_ready)?;
    if !bound {
        return Err(status(
            Code::FailedPrecondition,
            ErrorReason::EntityNotFound,
            format!("DCS holds no object of unit {}", d.unit_id),
            d.unit_id,
            "effect.damage.unit_id",
        ));
    }
    g.with_rec(c, |rec, store, sim| {
        rec.damage(store, sim, unit, d.life_percent)
    })
    .flatten()
    .ok_or_else(dcs_not_ready)
}

fn capture_start(g: &mut Inner, c: u64, r: &CaptureRequest) -> Result<(u64, u64), Status> {
    let id = EntityId(r.entity_id);
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    if slot.store.group(id).filter(|g| !g.removed).is_none() {
        return Err(not_found(r.entity_id));
    }
    let bound = g
        .with_rec(c, |rec, _, _| rec.is_bound(id))
        .ok_or_else(dcs_not_ready)?;
    if !bound {
        return Err(status(
            Code::FailedPrecondition,
            ErrorReason::EntityNotFound,
            format!("DCS holds no object of entity {}", r.entity_id),
            r.entity_id,
            "entity_id",
        ));
    }
    g.with_rec(c, |rec, store, sim| rec.capture(store, sim, id))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

fn authority(g: &mut Inner, c: u64, ids: &[u64], a: Authority) -> Result<u64, Status> {
    if ids.is_empty() {
        return Err(invalid("entity_ids", "name at least one group or static"));
    }
    // The loaded theater, once the agent's frames carry this campaign.
    let loaded =
        (g.link.frame_campaign == c && !g.link.theater.is_empty()).then(|| g.link.theater.clone());
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    for i in ids {
        let Some(grp) = slot.store.group(EntityId(*i)).filter(|g| !g.removed) else {
            return Err(not_found(*i));
        };
        if a == Authority::Sim
            && let Some(t) = &loaded
            && grp.theater != *t
        {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::TheaterNotLoaded,
                format!("entity {i} is on `{}`; `{t}` is loaded", grp.theater),
                *i,
                "entity_ids",
            ));
        }
    }
    for i in ids {
        slot.store
            .set_authority(EntityId(*i), a)
            .map_err(store_error)?;
    }
    Ok(slot.store.revision())
}

fn lost_policy(
    g: &mut Inner,
    c: u64,
    r: &SetLostPolicyRequest,
    cfg: &crate::config::Config,
) -> Result<SetLostPolicyResponse, Status> {
    let p = match pb::LostPolicy::try_from(r.policy) {
        Ok(pb::LostPolicy::Report) => LostPolicy::Report,
        Ok(pb::LostPolicy::Respawn) => LostPolicy::Respawn,
        Ok(pb::LostPolicy::Dead) => LostPolicy::Dead,
        _ => return Err(invalid("policy", "a policy is required")),
    };
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    if r.group_id == 0 {
        slot.on_lost = p;
        let revision = slot.store.revision();
        if g.active == Some(c) {
            // The reconciler takes its default policy when it is built.
            g.restart(cfg);
        }
        return Ok(SetLostPolicyResponse { revision });
    }
    let id = EntityId(r.group_id);
    match slot.store.group(id) {
        Some(x) if x.kind == Kind::Group => {}
        Some(_) => return Err(invalid("group_id", "only a group has a lost policy")),
        None => return Err(not_found(r.group_id)),
    }
    slot.store.set_on_lost(id, Some(p)).map_err(store_error)?;
    Ok(SetLostPolicyResponse {
        revision: slot.store.revision(),
    })
}

#[tonic::async_trait]
impl CommandsService for Commands {
    async fn put_group(
        &self,
        req: Request<PutGroupRequest>,
    ) -> Result<Response<PutGroupResponse>, Status> {
        changing!(self, req, "PutGroup", Need::Campaign, put_group)
    }

    async fn put_static(
        &self,
        req: Request<PutStaticRequest>,
    ) -> Result<Response<PutStaticResponse>, Status> {
        changing!(self, req, "PutStatic", Need::Campaign, put_static)
    }

    async fn set_order(
        &self,
        req: Request<SetOrderRequest>,
    ) -> Result<Response<SetOrderResponse>, Status> {
        changing!(self, req, "SetOrder", Need::Campaign, set_order)
    }

    async fn set_options(
        &self,
        req: Request<SetOptionsRequest>,
    ) -> Result<Response<SetOptionsResponse>, Status> {
        changing!(self, req, "SetOptions", Need::Campaign, set_options)
    }

    async fn materialize(
        &self,
        req: Request<MaterializeRequest>,
    ) -> Result<Response<MaterializeResponse>, Status> {
        changing!(
            self,
            req,
            "Materialize",
            Need::Active,
            |g: &mut Inner, c, r: &MaterializeRequest| {
                authority(g, c, &r.entity_ids, Authority::Sim)
                    .map(|revision| MaterializeResponse { revision })
            }
        )
    }

    async fn dematerialize(
        &self,
        req: Request<DematerializeRequest>,
    ) -> Result<Response<DematerializeResponse>, Status> {
        changing!(
            self,
            req,
            "Dematerialize",
            Need::Active,
            |g: &mut Inner, c, r: &DematerializeRequest| {
                authority(g, c, &r.entity_ids, Authority::Campaign)
                    .map(|revision| DematerializeResponse { revision })
            }
        )
    }

    async fn remove_entity(
        &self,
        req: Request<RemoveEntityRequest>,
    ) -> Result<Response<RemoveEntityResponse>, Status> {
        changing!(self, req, "RemoveEntity", Need::Campaign, remove_entity)
    }

    async fn set_lost_policy(
        &self,
        req: Request<SetLostPolicyRequest>,
    ) -> Result<Response<SetLostPolicyResponse>, Status> {
        let cfg = self.0.cfg.clone();
        changing!(
            self,
            req,
            "SetLostPolicy",
            Need::Campaign,
            move |g: &mut Inner, c, r: &SetLostPolicyRequest| { lost_policy(g, c, r, &cfg) }
        )
    }

    async fn set_airbase_owner(
        &self,
        req: Request<SetAirbaseOwnerRequest>,
    ) -> Result<Response<SetAirbaseOwnerResponse>, Status> {
        changing!(
            self,
            req,
            "SetAirbaseOwner",
            Need::Campaign,
            set_airbase_owner
        )
    }

    async fn set_warehouse_stock(
        &self,
        req: Request<SetWarehouseStockRequest>,
    ) -> Result<Response<SetWarehouseStockResponse>, Status> {
        changing!(
            self,
            req,
            "SetWarehouseStock",
            Need::Campaign,
            set_warehouse_stock
        )
    }

    async fn adjust_warehouse_stock(
        &self,
        req: Request<AdjustWarehouseStockRequest>,
    ) -> Result<Response<AdjustWarehouseStockResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "AdjustWarehouseStock",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Adjust,
                |g: &mut Inner, c| adjust_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn capture(
        &self,
        req: Request<CaptureRequest>,
    ) -> Result<Response<CaptureResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "Capture",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Capture(r.entity_id),
                |g: &mut Inner, c| capture_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn do_effect(
        &self,
        req: Request<DoEffectRequest>,
    ) -> Result<Response<DoEffectResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let effect = match r.effect.as_ref().and_then(|e| e.effect.as_ref()) {
            Some(
                e @ (pb::effect::Effect::Damage(_)
                | pb::effect::Effect::Sound(_)
                | pb::effect::Effect::Picture(_)
                | pb::effect::Effect::PictureStop(_)),
            ) => e.clone(),
            Some(_) => return Err(not_built("DoEffect for this effect")),
            None => return Err(invalid("effect", "an effect is required")),
        };
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "DoEffect",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Damage,
                |g: &mut Inner, c| effect_start(g, c, &effect),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn set_sample_rates(
        &self,
        req: Request<SetSampleRatesRequest>,
    ) -> Result<Response<SetSampleRatesResponse>, Status> {
        changing!(
            self,
            req,
            "SetSampleRates",
            Need::Campaign,
            set_sample_rates
        )
    }

    async fn get_outcome(
        &self,
        req: Request<GetOutcomeRequest>,
    ) -> Result<Response<GetOutcomeResponse>, Status> {
        require(&req, Access::Controller)?;
        let key = &req.get_ref().idempotency_key;
        if key.is_empty() {
            return Err(invalid("idempotency_key", "a key is required"));
        }
        let mut g = self.0.lock();
        let done = g.idem.get(Instant::now(), key).ok_or_else(|| {
            status(
                Code::NotFound,
                ErrorReason::OutcomeNotFound,
                "the key is unknown or no longer remembered",
                0,
                "idempotency_key",
            )
        })?;
        // A pending effect or query settles here once its result is in.
        let done = settle(&mut g, key, done);
        let mut value = None;
        let (result, entity) = match done.method {
            "AdjustWarehouseStock" => (
                AdjustWarehouseStockResponse::decode(done.response.as_slice())
                    .ok()
                    .and_then(|r| r.result),
                None,
            ),
            "DoEffect" => (
                DoEffectResponse::decode(done.response.as_slice())
                    .ok()
                    .and_then(|r| r.result),
                None,
            ),
            "Capture" => match CaptureResponse::decode(done.response.as_slice()) {
                Ok(r) => (r.result, r.entity),
                Err(_) => (None, None),
            },
            "SendMessage" => (
                pb::SendMessageResponse::decode(done.response.as_slice())
                    .ok()
                    .and_then(|r| r.result),
                None,
            ),
            "SendChat" => (
                pb::SendChatResponse::decode(done.response.as_slice())
                    .ok()
                    .and_then(|r| r.result),
                None,
            ),
            "InvokeLua" => {
                let r = pb::InvokeLuaResponse::decode(done.response.as_slice()).ok();
                value = r.as_ref().and_then(|r| r.value.clone());
                (r.and_then(|r| r.result), None)
            }
            "EvalLua" => {
                let r = pb::EvalLuaResponse::decode(done.response.as_slice()).ok();
                value = r.as_ref().and_then(|r| r.value.clone());
                (r.and_then(|r| r.result), None)
            }
            // A set-state call has no outcome of its own.
            _ => (None, None),
        };
        Ok(Response::new(GetOutcomeResponse {
            result,
            value,
            entity,
            campaign_id: done.campaign,
        }))
    }
}
