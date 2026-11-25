//! `CommandsService`: desired state, controller only (plan 4.5).
//!
//! A set-state call returns once the store holds the change; the reconciler
//! carries it to DCS. Every call carries the lease and an idempotency key
//! ([`Hub::write`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::commands_service_server::CommandsService;
use nanoprobe_proto::v1alpha1::{
    self as pb, AdjustWarehouseStockRequest, AdjustWarehouseStockResponse, CaptureRequest,
    CaptureResponse, DematerializeRequest, DematerializeResponse, DoEffectRequest,
    DoEffectResponse, ErrorReason, GetOutcomeRequest, GetOutcomeResponse, GroupCategory,
    MaterializeRequest, MaterializeResponse, PutFromStaticTemplateRequest,
    PutFromStaticTemplateResponse, PutGroupFromTemplateRequest, PutGroupFromTemplateResponse,
    PutGroupRequest, PutGroupResponse, PutStaticRequest, PutStaticResponse, RemoveEntityRequest,
    RemoveEntityResponse, Resolution, ResolveMemberRequest, ResolveMemberResponse,
    SetAirbaseOwnerRequest, SetAirbaseOwnerResponse, SetLostPolicyRequest, SetLostPolicyResponse,
    SetOptionsRequest, SetOptionsResponse, SetOrderRequest, SetOrderResponse,
    SetSampleRatesRequest, SetSampleRatesResponse, SetWarehouseStockRequest,
    SetWarehouseStockResponse, Write,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::intents::Intent;
use nanoprobe_sidecar::reconciler::EffectResult;
use nanoprobe_sidecar::sim::wire::{Body, Debris};
use nanoprobe_sidecar::stock::{Item, Stock};
use nanoprobe_sidecar::store::{
    Authority, Kind, LostPolicy, NewGroup, NewUnit, Outcome, Point, Store, StoreError, UnitState,
    check_lv,
};
use nanoprobe_transport::records::Rates;
use prost::Message;
use tonic::{Code, Request, Response, Status};

use crate::auth::{Access, require};
use crate::convert::{
    dcs_category, dcs_coalition, entity, finite, intent, invalid, not_built, not_found,
};
use crate::group_template;
use crate::hub::{Call, Hub, Inner, Need, Slot, settle};
use crate::idem::EffectKind;
use crate::lua::{extra_to_lua, from_lua, to_lua};
use crate::payload_preset;
use crate::static_template;
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

/// Check a unit's spawn life (spec/world.md, "Spawn state"): a percentage
/// above 0 and up to 100. At 0 a unit leaves DCS with no death fact.
fn spawn_state_ok(u: &pb::UnitSpec, f: &str) -> Result<(), Status> {
    if let Some(p) = u.life_percent
        && !(p.is_finite() && p > 0.0 && p <= 100.0)
    {
        return Err(invalid(
            &format!("{f}.life_percent"),
            "a percentage above 0 and up to 100",
        ));
    }
    Ok(())
}

/// Store a unit's spawn life, when the spec gives one. A replacement that
/// gives none keeps what the store knows.
fn set_spawn_life(s: &mut Store, uid: EntityId, u: &pb::UnitSpec) -> Result<(), Status> {
    if let Some(p) = u.life_percent {
        s.set_spawn_life(uid, p).map_err(store_error)?;
    }
    Ok(())
}

fn put_group(g: &mut Inner, c: u64, r: &PutGroupRequest) -> Result<PutGroupResponse, Status> {
    let spec = r
        .spec
        .as_ref()
        .ok_or_else(|| invalid("spec", "a spec is required"))?;
    let category = check_group(g, spec, r.order.as_ref())?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    if r.group_id != 0 {
        return replace_group(slot, r, category);
    }
    let plan = plan_group(slot, spec, r.order.as_ref(), category)?;
    write_group(&mut slot.store, plan)
}

/// The most distinct preset paths one `PutGroup` may name. Each is read
/// and parsed, up to 1 MiB and its tree, before the call takes the lock;
/// a group has one type per unit, and a handful of files covers any real
/// group.
pub(crate) const PRESET_FILES_MAX: usize = 16;

/// What a unit asks of a preset file: its path, the unit's type and the
/// preset's name.
type Wanted = (String, String, String);

/// The pylons of each distinct preset a `PutGroup` names, converted before
/// the call takes the lock, or why it is refused. `Err` with the number of
/// paths when the request names more than `PRESET_FILES_MAX`, and nothing
/// was read.
type Presets =
    Result<BTreeMap<Wanted, Result<payload_preset::Pylons, payload_preset::Refusal>>, usize>;

/// The distinct presets a request names.
fn preset_wants(r: &PutGroupRequest) -> BTreeSet<Wanted> {
    r.spec
        .iter()
        .flat_map(|s| &s.units)
        .filter_map(|u| {
            u.payload_preset
                .as_ref()
                .map(|p| (p.path.clone(), u.r#type.clone(), p.name.clone()))
        })
        .collect()
}

/// Read each preset file of `wants` once, as the sidecar opens it, and
/// convert every preset asked of it. A file is parsed and dropped before
/// the next is read, so at most one parsed file is held at a time.
fn read_presets(dirs: &[PathBuf], wants: BTreeSet<Wanted>) -> Presets {
    let paths: BTreeSet<&String> = wants.iter().map(|w| &w.0).collect();
    if paths.len() > PRESET_FILES_MAX {
        return Err(paths.len());
    }
    let mut out = BTreeMap::new();
    for path in paths {
        let file = crate::template_file::read_for(
            "PutGroup",
            payload_preset::FILE_WHAT,
            payload_preset::FILE_MAX,
            dirs,
            path,
        )
        .map_err(|text| payload_preset::Refusal {
            field: "path",
            text,
        })
        .and_then(payload_preset::read);
        for w in wants.iter().filter(|w| &w.0 == path) {
            let got = match &file {
                Ok(f) => f.pylons(&w.1, &w.2),
                Err(e) => Err(e.clone()),
            };
            out.insert(w.clone(), got);
        }
    }
    Ok(out)
}

/// Put a preset's pylons into a unit's `extra.payload.pylons`. `f` is the
/// unit's field.
fn set_pylons(u: &mut pb::UnitSpec, pylons: prost_types::Struct, f: &str) -> Result<(), Status> {
    use prost_types::value::Kind as V;
    let extra = u.extra.get_or_insert_default();
    let payload = extra
        .fields
        .entry("payload".into())
        .or_insert_with(|| prost_types::Value {
            kind: Some(V::StructValue(prost_types::Struct::default())),
        });
    let Some(V::StructValue(payload)) = &mut payload.kind else {
        return Err(invalid(
            &format!("{f}.extra.payload"),
            "a unit that names a payload preset has an object `payload` or none",
        ));
    };
    if payload.fields.contains_key("pylons") {
        return Err(invalid(
            &format!("{f}.extra.payload.pylons"),
            "a unit that names a payload preset carries no `pylons`; remove them to use the preset",
        ));
    }
    payload.fields.insert(
        "pylons".into(),
        prost_types::Value {
            kind: Some(V::StructValue(pylons)),
        },
    );
    Ok(())
}

/// `PutGroup` (spec/world.md, "Payload presets"): each unit that names a
/// payload preset gets the preset's pylons in `extra.payload.pylons`, from
/// `presets` as they were read before the call took the lock. The spec
/// then goes through `put_group` as written, so the group is the one a
/// `PutGroup` with the pylons written by hand makes. The store keeps the
/// pylons alone; the log names the files and presets.
fn put_group_with_presets(
    g: &mut Inner,
    c: u64,
    r: &PutGroupRequest,
    presets: &Presets,
) -> Result<PutGroupResponse, Status> {
    let Some(spec) = r
        .spec
        .as_ref()
        .filter(|s| s.units.iter().any(|u| u.payload_preset.is_some()))
    else {
        return put_group(g, c, r);
    };
    // A spec with no category, no units or a unit with no type is refused
    // for that, not for a preset that cannot match it.
    check_shape(spec)?;
    let presets = presets.as_ref().map_err(|n| {
        invalid(
            "spec.units",
            format!(
                "a PutGroup names at most {PRESET_FILES_MAX} payload preset files; this one names {n}"
            ),
        )
    })?;
    let mut spec = spec.clone();
    let mut used = Vec::new();
    for (i, u) in spec.units.iter_mut().enumerate() {
        let Some(p) = u.payload_preset.take() else {
            continue;
        };
        let f = format!("spec.units[{i}]");
        let want = (p.path.clone(), u.r#type.clone(), p.name.clone());
        let got = presets
            .get(&want)
            .expect("every named preset was read")
            .clone()
            .map_err(|e| invalid(&format!("{f}.payload_preset.{}", e.field), e.text))?;
        let note = got.note();
        set_pylons(u, got.pylons, &f)?;
        used.push(format!(
            "unit {i} the preset {:?} of {:?}{note}",
            p.name, p.path
        ));
    }
    let made = put_group(
        g,
        c,
        &PutGroupRequest {
            spec: Some(spec),
            ..r.clone()
        },
    )?;
    eprintln!(
        "PutGroup gave group {} payload presets: {}",
        made.group_id,
        used.join("; ")
    );
    Ok(made)
}

/// The checks of a group spec's category and units that come before any
/// other, a payload preset's included. Returns the DCS category.
fn check_shape(spec: &pb::GroupSpec) -> Result<u32, Status> {
    let category = GroupCategory::try_from(spec.category)
        .ok()
        .and_then(dcs_category)
        .ok_or_else(|| invalid("spec.category", "a group category is required"))?;
    if spec.units.is_empty() {
        return Err(invalid("spec.units", "a group has at least one unit"));
    }
    for (i, u) in spec.units.iter().enumerate() {
        if u.r#type.is_empty() {
            return Err(invalid(
                &format!("spec.units[{i}].type"),
                "a unit needs its DCS type",
            ));
        }
    }
    Ok(category)
}

/// The checks of a group spec that need no campaign: its category, units
/// and size. Returns the DCS category.
fn check_group(g: &Inner, spec: &pb::GroupSpec, order: Option<&pb::Order>) -> Result<u32, Status> {
    let category = check_shape(spec)?;
    for (i, u) in spec.units.iter().enumerate() {
        let f = format!("spec.units[{i}]");
        spawn_state_ok(u, &f)?;
        finite(u.heading, &format!("{f}.heading"))?;
        // `PutGroup` converts a preset before this; any other way here has
        // none to convert.
        if u.payload_preset.is_some() {
            return Err(invalid(
                &format!("{f}.payload_preset"),
                "a payload preset is taken by PutGroup only",
            ));
        }
    }
    // Every check comes before the first store write: a call that fails
    // changes nothing, so a repeat under its key cannot make a second group.
    if spec.components.keys().any(String::is_empty) {
        return Err(invalid("spec.components", "a component needs a name"));
    }
    let limit = g.backend.max_command();
    fits(spawn_bound(spec), limit, "spec")?;
    fits(order_bound(order), limit, "order")?;
    Ok(category)
}

/// A new group, checked against its campaign and ready to write.
struct GroupPlan<'a> {
    spec: &'a pb::GroupSpec,
    group: NewGroup,
    order: Option<Intent>,
}

/// The checks of a new group against its campaign, which write nothing.
fn plan_group<'a>(
    slot: &Slot,
    spec: &'a pb::GroupSpec,
    order: Option<&pb::Order>,
    category: u32,
) -> Result<GroupPlan<'a>, Status> {
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
                },
            )
        });
    }
    let order = match order {
        Some(o) => Some(intent(Some(o), &theater, "order", &slot.store)?),
        None => None,
    };
    Ok(GroupPlan {
        spec,
        group: NewGroup {
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
        },
        order,
    })
}

/// The entity IDs a new group takes: its own and one per unit.
fn ids_of(group: &NewGroup) -> u64 {
    1 + group.units.len() as u64
}

/// Write a planned group.
fn write_group(store: &mut Store, plan: GroupPlan) -> Result<PutGroupResponse, Status> {
    let GroupPlan { spec, group, order } = plan;
    let id = store.create_group(group).map_err(store_error)?;
    set_components(store, id, &spec.components)?;
    let members = store.group(id).expect("created").members.clone();
    for (u, uid) in spec.units.iter().zip(members) {
        set_spawn_life(store, uid, u)?;
    }
    if let Some(o) = order {
        store.set_order(id, o).map_err(store_error)?;
    }
    let grp = store.group(id).expect("created");
    Ok(PutGroupResponse {
        group_id: id.0,
        unit_ids: grp.members.iter().map(|m| m.0).collect(),
        spec_revision: grp.spec_rev,
        revision: store.revision(),
    })
}

/// `PutGroupFromTemplate`: a new group from a template the caller's file
/// holds (spec/world.md, "Group templates"). `file` is the template as it
/// was read before the call took the lock, or why it was refused.
///
/// Every check comes before the first store write. The converted spec then
/// goes through `put_group`, so the group is the one a `PutGroup` of that
/// spec makes.
fn put_group_from_template(
    g: &mut Inner,
    c: u64,
    r: &PutGroupFromTemplateRequest,
    file: Result<group_template::Template, Status>,
) -> Result<PutGroupFromTemplateResponse, Status> {
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    place(r.anchor.as_ref(), slot, "anchor")?;
    finite(r.heading, "heading")?;
    let t = file?;
    let refused = |e: group_template::Refusal| invalid(e.field, e.text);
    let anchor = r.anchor.as_ref().expect("placed");
    let spec = group_template::spec(&t, anchor, r.heading, r.country, &r.label).map_err(refused)?;
    fits(spawn_bound(&spec), g.backend.max_command(), "template")?;
    let made = put_group(
        g,
        c,
        &PutGroupRequest {
            write: r.write.clone(),
            group_id: 0,
            spec: Some(spec),
            order: r.order.clone(),
        },
    )?;
    // The store keeps the spec alone; the log is where the file is named.
    eprintln!(
        "PutGroupFromTemplate made group {} from the template {:?} of {:?}{}",
        made.group_id,
        t.name,
        r.path,
        if t.dropped.is_empty() {
            String::new()
        } else {
            format!("; keys not used: {}", t.dropped.join(", "))
        }
    );
    Ok(PutGroupFromTemplateResponse {
        group_id: made.group_id,
        unit_ids: made.unit_ids,
        spec_revision: made.spec_revision,
        revision: made.revision,
    })
}

/// `PutFromStaticTemplate`: every vehicle group and static of a static
/// template the caller's file holds (spec/world.md, "Static templates").
/// `file` is the layout as it was read before the call took the lock, or
/// why it was refused.
///
/// Every check of every entity, entity IDs and value sizes included, comes
/// before the first store write, so the call makes all of them or none.
/// The converted specs go through the checks and writes of `PutGroup` and
/// `PutStatic`.
fn put_from_static_template(
    g: &mut Inner,
    c: u64,
    r: &PutFromStaticTemplateRequest,
    file: Result<static_template::Layout, Status>,
) -> Result<PutFromStaticTemplateResponse, Status> {
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    if r.anchor.is_some() {
        place(r.anchor.as_ref(), slot, "anchor")?;
    }
    finite(r.heading, "heading")?;
    let l = file?;
    let specs = static_template::specs(&l, r.anchor.as_ref(), r.heading, r.country, &r.label)
        .map_err(|e| invalid(e.field, e.text))?;
    if !slot.meta.theaters.contains(&specs.theater) {
        return Err(invalid(
            "path",
            format!("the campaign does not use the theater `{}`", specs.theater),
        ));
    }
    let limit = g.backend.max_command();
    let mut categories = Vec::with_capacity(specs.groups.len());
    for spec in &specs.groups {
        fits(spawn_bound(spec), limit, "path")?;
        categories.push(check_group(g, spec, None)?);
    }
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    let mut groups = Vec::with_capacity(specs.groups.len());
    for (spec, category) in specs.groups.iter().zip(categories) {
        groups.push(plan_group(slot, spec, None, category)?);
    }
    let mut statics = Vec::with_capacity(specs.statics.len());
    for spec in &specs.statics {
        fits(static_bound(spec), limit, "path")?;
        statics.push(plan_static(slot, spec, limit)?);
    }
    // The two writes that can still fail, checked for every entity first.
    let news = groups
        .iter()
        .map(|p| &p.group)
        .chain(statics.iter().map(|p| &p.group));
    // The reader refuses such a value with its place; this keeps the writes
    // below from failing whatever the reader lets through.
    let stored = |e: String| {
        invalid(
            "path",
            format!("the file is not a static template file: a value cannot be stored: {e}"),
        )
    };
    for n in news {
        check_lv(&n.extra).map_err(stored)?;
        for u in &n.units {
            check_lv(&u.extra).map_err(stored)?;
        }
    }
    let ids = groups.iter().map(|p| ids_of(&p.group)).sum::<u64>() + statics.len() as u64;
    if ids > slot.store.ids_left() {
        return Err(store_error(StoreError::Exhausted));
    }
    let store = &mut g.campaigns.get_mut(&c).expect("resolved campaign").store;
    let checked = "checked before the first write";
    let groups: Vec<_> = groups
        .into_iter()
        .map(|p| {
            let made = write_group(store, p).expect(checked);
            pb::StaticTemplateGroup {
                group_id: made.group_id,
                unit_ids: made.unit_ids,
                spec_revision: made.spec_revision,
            }
        })
        .collect();
    let statics: Vec<_> = statics
        .into_iter()
        .map(|p| {
            let made = write_static(store, p).expect(checked);
            pb::StaticTemplateStatic {
                static_id: made.static_id,
                spec_revision: made.spec_revision,
            }
        })
        .collect();
    // The store keeps the specs alone; the log is where the file is named.
    eprintln!(
        "PutFromStaticTemplate made {} groups and {} statics from {:?}{}",
        groups.len(),
        statics.len(),
        r.path,
        if l.dropped.is_empty() {
            String::new()
        } else {
            format!("; keys not used: {}", l.dropped.join(", "))
        }
    );
    Ok(PutFromStaticTemplateResponse {
        groups,
        statics,
        revision: store.revision(),
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
        set_spawn_life(s, uid, u)?;
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
    let limit = g.backend.max_command();
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let plan = plan_static(slot, spec, limit)?;
    write_static(&mut slot.store, plan)
}

/// A new static, checked and ready to write.
struct StaticPlan {
    group: NewGroup,
    dead: bool,
}

/// The bytes of a static's spawn command, at most.
fn static_bound(spec: &pb::StaticSpec) -> usize {
    2 * OVERHEAD + spec.r#type.len() + lv_len(&extra_to_lua(spec.extra.as_ref()))
}

/// The checks of a new static, which write nothing.
fn plan_static(slot: &Slot, spec: &pb::StaticSpec, limit: usize) -> Result<StaticPlan, Status> {
    if spec.r#type.is_empty() {
        return Err(invalid("spec.type", "a static needs its DCS type"));
    }
    let at = place(spec.position.as_ref(), slot, "spec.position")?;
    let theater = spec.position.as_ref().expect("placed").theater.clone();
    let heading = finite(spec.heading, "spec.heading")?;
    // Whether a static spawns destroyed is the platform's to say, from its
    // state in the store: a `dead` key among the further keys would spawn a
    // static the store calls alive as a ruin, or be dropped.
    if spec
        .extra
        .as_ref()
        .is_some_and(|e| e.fields.contains_key("dead"))
    {
        return Err(invalid(
            "spec.extra",
            "set spec.dead to create a static destroyed; extra takes no dead key",
        ));
    }
    fits(static_bound(spec), limit, "spec")?;
    let extra = extra_to_lua(spec.extra.as_ref());
    Ok(StaticPlan {
        group: NewGroup {
            theater,
            country: spec.country,
            category: 0,
            coalition: 0,
            units: vec![NewUnit {
                extra,
                ..NewUnit::new(
                    &spec.r#type,
                    UnitState {
                        at,
                        heading,
                        life: 0.0,
                    },
                )
            }],
            authority: Authority::Sim,
            label: spec.label.clone(),
            extra: nanoprobe_transport::codec::Lv::Nil,
        },
        dead: spec.dead,
    })
}

/// Write a planned static.
fn write_static(store: &mut Store, plan: StaticPlan) -> Result<PutStaticResponse, Status> {
    let create = if plan.dead {
        Store::create_ruin
    } else {
        Store::create_static
    };
    let id = create(store, plan.group).map_err(store_error)?;
    Ok(PutStaticResponse {
        static_id: id.0,
        spec_revision: store.group(id).map_or(0, |x| x.spec_rev),
        revision: store.revision(),
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

/// Release a member held out of spawns (spec/world.md, "Respawn gate").
pub(crate) fn resolve_member(
    g: &mut Inner,
    c: u64,
    r: &ResolveMemberRequest,
) -> Result<ResolveMemberResponse, Status> {
    let dead = match Resolution::try_from(r.resolution) {
        Ok(Resolution::Dead) => true,
        Ok(Resolution::Restore) => false,
        _ => return Err(invalid("resolution", "DEAD or RESTORE")),
    };
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let unit = EntityId(r.unit);
    if slot.store.unit(unit).is_none() {
        return Err(if slot.store.group(unit).is_some() {
            invalid("unit", "a member is a unit, not a group")
        } else {
            not_found(r.unit)
        });
    }
    if !slot.store.resolve_member(unit, dead).map_err(store_error)? {
        return Err(status(
            Code::FailedPrecondition,
            ErrorReason::Unspecified,
            format!(
                "unit {} is not held: its left is not MISSING or REMOVED, or it is dead",
                r.unit
            ),
            r.unit,
            "unit",
        ));
    }
    Ok(ResolveMemberResponse {
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
        EffectKind::Damage => DoEffectResponse {
            result: result_pb,
            debris_removed: None,
        }
        .encode_to_vec(),
        EffectKind::Debris => DoEffectResponse {
            result: result_pb,
            debris_removed: debris_removed(result),
        }
        .encode_to_vec(),
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
        pb::effect::Effect::ClearDebris(d) => clear_debris_start(g, c, d),
        _ => Err(not_built("DoEffect for this effect")),
    }
}

/// The farthest from 0 a clear-debris coordinate or radius may be: 10,000 km
/// (spec/commands.md, "Layout: CLEAR_DEBRIS").
const DEBRIS_REACH_M: f64 = 10_000_000.0;

/// How many junk objects a clear removed, from its result's value: a
/// whole number, at least 0. None when the result holds no count.
fn debris_removed(result: Option<&EffectResult>) -> Option<u64> {
    match result.and_then(|r| r.result.as_ref()).map(|r| &r.value) {
        Some(Lv::Number(n)) if n.is_finite() && *n >= 0.0 => Some(n.round() as u64),
        _ => None,
    }
}

/// A clear-debris point: on the campaign's loaded theater, within reach.
fn debris_point(slot: &Slot, p: Option<&pb::Position>, field: &str) -> Result<[f64; 3], Status> {
    let at = place(p, slot, field)?;
    let theater = &p.expect("placed").theater;
    if let Some(l) = slot.store.loaded()
        && !l.theater.is_empty()
        && &l.theater != theater
    {
        return Err(status(
            Code::FailedPrecondition,
            ErrorReason::TheaterNotLoaded,
            format!("`{}` is loaded, not `{theater}`", l.theater),
            0,
            &format!("{field}.theater"),
        ));
    }
    for (v, axis) in [(at.x, "north"), (at.y, "alt"), (at.z, "east")] {
        if v.abs() > DEBRIS_REACH_M {
            return Err(invalid(&format!("{field}.{axis}"), "within 10,000 km of 0"));
        }
    }
    Ok([at.x, at.y, at.z])
}

fn clear_debris_start(g: &mut Inner, c: u64, d: &pb::ClearDebris) -> Result<(u64, u64), Status> {
    use pb::clear_debris::Volume;
    const F: &str = "effect.clear_debris";
    /// What the reconciler is asked for.
    enum Clear {
        One(Debris),
        Column {
            north: f64,
            east: f64,
            half_width: f64,
        },
        Map,
    }
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    let volume = match &d.volume {
        Some(Volume::Sphere(s)) => {
            let center = debris_point(slot, s.center.as_ref(), &format!("{F}.sphere.center"))?;
            if !(s.radius.is_finite() && s.radius > 0.0 && s.radius <= DEBRIS_REACH_M) {
                return Err(invalid(
                    &format!("{F}.sphere.radius"),
                    "above 0, at most 10,000 km",
                ));
            }
            Clear::One(Debris::Sphere {
                center,
                radius: s.radius,
            })
        }
        Some(Volume::Box(b)) => {
            let min = debris_point(slot, b.min.as_ref(), &format!("{F}.box.min"))?;
            let max = debris_point(slot, b.max.as_ref(), &format!("{F}.box.max"))?;
            if b.min.as_ref().map(|p| &p.theater) != b.max.as_ref().map(|p| &p.theater) {
                return Err(invalid(
                    &format!("{F}.box.max.theater"),
                    "a box's corners name the same theater",
                ));
            }
            for (i, axis) in ["north", "alt", "east"].into_iter().enumerate() {
                if min[i] > max[i] {
                    return Err(invalid(
                        &format!("{F}.box.max.{axis}"),
                        "each of max's coordinates is at least min's",
                    ));
                }
            }
            Clear::One(Debris::Box { min, max })
        }
        Some(Volume::Column(col)) => {
            // `alt` is ignored, whatever it holds: the point is checked
            // with 0 in its place.
            let center = col.center.clone().map(|mut p| {
                p.alt = 0.0;
                p
            });
            let [north, _, east] =
                debris_point(slot, center.as_ref(), &format!("{F}.column.center"))?;
            let half_width = col.half_width;
            if !(half_width.is_finite() && half_width > 0.0 && half_width <= DEBRIS_REACH_M) {
                return Err(invalid(
                    &format!("{F}.column.half_width"),
                    "above 0, at most 10,000 km",
                ));
            }
            // The agent refuses a box with a corner beyond its reach.
            if north.abs() + half_width > DEBRIS_REACH_M || east.abs() + half_width > DEBRIS_REACH_M
            {
                return Err(invalid(
                    &format!("{F}.column.half_width"),
                    "each edge of the column is within 10,000 km of 0",
                ));
            }
            Clear::Column {
                north,
                east,
                half_width,
            }
        }
        Some(Volume::WholeMap(_)) => Clear::Map,
        None => return Err(invalid(&format!("{F}.volume"), "a volume is required")),
    };
    g.with_rec(c, |rec, store, sim| match volume {
        Clear::One(v) => rec.clear_debris(store, sim, v),
        Clear::Column {
            north,
            east,
            half_width,
        } => rec.clear_debris_column(store, sim, north, east, half_width),
        Clear::Map => rec.clear_debris_everywhere(store, sim),
    })
    .flatten()
    .ok_or_else(dcs_not_ready)
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
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        // The preset files are read and converted here, on a worker thread
        // and without the lock, as a template's file is. A refusal is given
        // inside the call, after the lease and the campaign are checked, so
        // a repeat under an answered key gets its first answer whatever has
        // become of a file.
        let wants = preset_wants(&r);
        let presets = if wants.is_empty() {
            Ok(BTreeMap::new())
        } else {
            let dirs = self.0.cfg.template_dirs.clone();
            let named = wants.clone();
            tokio::task::spawn_blocking(move || read_presets(&dirs, named))
                .await
                .unwrap_or_else(|_| {
                    Ok(wants
                        .into_iter()
                        .map(|w| {
                            let text = format!("{:?} could not be read", w.0);
                            (
                                w,
                                Err(payload_preset::Refusal {
                                    field: "path",
                                    text,
                                }),
                            )
                        })
                        .collect())
                })
        };
        let resp = self
            .0
            .write(
                Call {
                    method: "PutGroup",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, c: u64| put_group_with_presets(g, c, &r, &presets).map(|x| (x, c)),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn put_group_from_template(
        &self,
        req: Request<PutGroupFromTemplateRequest>,
    ) -> Result<Response<PutGroupFromTemplateResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        // The file is read and parsed here, on a worker thread and without
        // the lock. Its refusal is given inside the call, after the lease
        // and the campaign are checked, so a repeat under an answered key
        // gets its first answer whatever has become of the file.
        let dirs = self.0.cfg.template_dirs.clone();
        let (path, name) = (r.path.clone(), r.template.clone());
        let file = tokio::task::spawn_blocking(move || {
            let bytes = crate::template_file::read_for(
                "PutGroupFromTemplate",
                group_template::FILE_WHAT,
                group_template::FILE_MAX,
                &dirs,
                &path,
            )
            .map_err(|e| invalid("path", e))?;
            group_template::read(&bytes, &name).map_err(|e| invalid(e.field, e.text))
        })
        .await
        .unwrap_or_else(|_| Err(invalid("path", format!("{:?} could not be read", r.path))));
        let resp = self
            .0
            .write(
                Call {
                    method: "PutGroupFromTemplate",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, c: u64| put_group_from_template(g, c, &r, file).map(|x| (x, c)),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn put_from_static_template(
        &self,
        req: Request<PutFromStaticTemplateRequest>,
    ) -> Result<Response<PutFromStaticTemplateResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        // Read and parsed on a worker thread without the lock, as
        // PutGroupFromTemplate reads its file; the refusal is given inside
        // the call, so a repeat under an answered key gets its first answer.
        let dirs = self.0.cfg.template_dirs.clone();
        let path = r.path.clone();
        let file = tokio::task::spawn_blocking(move || {
            let bytes = crate::template_file::read_for(
                "PutFromStaticTemplate",
                static_template::FILE_WHAT,
                static_template::FILE_MAX,
                &dirs,
                &path,
            )
            .map_err(|e| invalid("path", e))?;
            static_template::read(&bytes).map_err(|e| invalid(e.field, e.text))
        })
        .await
        .unwrap_or_else(|_| Err(invalid("path", format!("{:?} could not be read", r.path))));
        let resp = self
            .0
            .write(
                Call {
                    method: "PutFromStaticTemplate",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, c: u64| put_from_static_template(g, c, &r, file).map(|x| (x, c)),
            )
            .await?;
        Ok(Response::new(resp))
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

    async fn resolve_member(
        &self,
        req: Request<ResolveMemberRequest>,
    ) -> Result<Response<ResolveMemberResponse>, Status> {
        changing!(self, req, "ResolveMember", Need::Campaign, resolve_member)
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
                | pb::effect::Effect::PictureStop(_)
                | pb::effect::Effect::ClearDebris(_)),
            ) => e.clone(),
            Some(_) => return Err(not_built("DoEffect for this effect")),
            None => return Err(invalid("effect", "an effect is required")),
        };
        let kind = match effect {
            pb::effect::Effect::ClearDebris(_) => EffectKind::Debris,
            _ => EffectKind::Damage,
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
                kind,
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
        let mut debris_removed = None;
        let (result, entity) = match done.method {
            "AdjustWarehouseStock" => (
                AdjustWarehouseStockResponse::decode(done.response.as_slice())
                    .ok()
                    .and_then(|r| r.result),
                None,
            ),
            "DoEffect" => {
                let r = DoEffectResponse::decode(done.response.as_slice()).ok();
                debris_removed = r.as_ref().and_then(|r| r.debris_removed);
                (r.and_then(|r| r.result), None)
            }
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
            debris_removed,
        }))
    }
}
