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
    self as pb, CaptureRequest, CaptureResponse, DematerializeRequest, DematerializeResponse,
    DoEffectRequest, DoEffectResponse, ErrorReason, GetOutcomeRequest, GetOutcomeResponse,
    GroupCategory, MaterializeRequest, MaterializeResponse, PutGroupRequest, PutGroupResponse,
    PutStaticRequest, PutStaticResponse, RemoveEntityRequest, RemoveEntityResponse,
    SetAirbaseOwnerRequest, SetAirbaseOwnerResponse, SetLostPolicyRequest, SetLostPolicyResponse,
    SetOptionsRequest, SetOptionsResponse, SetOrderRequest, SetOrderResponse,
    SetSampleRatesRequest, SetSampleRatesResponse, SetWarehouseStockRequest,
    SetWarehouseStockResponse, Write,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::store::{
    Authority, Kind, LostPolicy, NewGroup, NewUnit, Point, Store, StoreError, UnitState,
};
use prost::Message;
use tonic::{Code, Request, Response, Status};

use crate::auth::{Access, require};
use crate::convert::{dcs_category, finite, intent, invalid, not_built, not_found};
use crate::hub::{Call, Hub, Inner, Need, Slot};
use crate::lua::extra_to_lua;
use nanoprobe_transport::codec::{Lv, Writer};

/// The service.
pub struct Commands(pub Arc<Hub>);

fn store_error(e: StoreError) -> Status {
    match e {
        StoreError::NoSuchEntity(id) => not_found(id.0),
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
            format!("attacking {id}, which is not a group, is not built yet"),
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
fn unleased(w: &Option<Write>) -> Option<Write> {
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

/// A checked position on one of the campaign's theaters.
fn place(p: Option<&pb::Position>, slot: &Slot, field: &str) -> Result<Point, Status> {
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
fn lv_len(v: &Lv) -> usize {
    let mut w = Writer::new();
    w.lv(v);
    w.len()
}

/// Bytes a command may take besides the values counted: the command header,
/// names, numbers and table keys. Generous, so an estimate never falls
/// short of what the reconciler builds.
const OVERHEAD: usize = 512;

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
fn fits(bytes: usize, limit: usize, field: &str) -> Result<(), Status> {
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
    let grp = slot.store.group(id).ok_or_else(|| not_found(r.group_id))?;
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

fn authority(g: &mut Inner, c: u64, ids: &[u64], a: Authority) -> Result<u64, Status> {
    if ids.is_empty() {
        return Err(invalid("entity_ids", "name at least one group or static"));
    }
    // The loaded theater, once the agent's frames carry this campaign.
    let loaded =
        (g.link.frame_campaign == c && !g.link.theater.is_empty()).then(|| g.link.theater.clone());
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    for i in ids {
        let Some(grp) = slot.store.group(EntityId(*i)) else {
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
        _req: Request<SetOptionsRequest>,
    ) -> Result<Response<SetOptionsResponse>, Status> {
        Err(not_built("SetOptions"))
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
        _req: Request<RemoveEntityRequest>,
    ) -> Result<Response<RemoveEntityResponse>, Status> {
        Err(not_built("RemoveEntity"))
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
        _req: Request<SetAirbaseOwnerRequest>,
    ) -> Result<Response<SetAirbaseOwnerResponse>, Status> {
        Err(not_built("SetAirbaseOwner"))
    }

    async fn set_warehouse_stock(
        &self,
        _req: Request<SetWarehouseStockRequest>,
    ) -> Result<Response<SetWarehouseStockResponse>, Status> {
        Err(not_built("SetWarehouseStock"))
    }

    async fn capture(
        &self,
        _req: Request<CaptureRequest>,
    ) -> Result<Response<CaptureResponse>, Status> {
        Err(not_built("Capture"))
    }

    async fn do_effect(
        &self,
        _req: Request<DoEffectRequest>,
    ) -> Result<Response<DoEffectResponse>, Status> {
        Err(not_built("DoEffect"))
    }

    async fn set_sample_rates(
        &self,
        _req: Request<SetSampleRatesRequest>,
    ) -> Result<Response<SetSampleRatesResponse>, Status> {
        Err(not_built("SetSampleRates"))
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
        let done = self.0.lock().idem.get(Instant::now(), key).ok_or_else(|| {
            status(
                Code::NotFound,
                ErrorReason::OutcomeNotFound,
                "the key is unknown or no longer remembered",
                0,
                "idempotency_key",
            )
        })?;
        // Only effects and queries have a command outcome, and none is built
        // yet. A set-state call's outcome is its entity's revisions in the
        // World service.
        Ok(Response::new(GetOutcomeResponse {
            result: None,
            value: None,
            entity: None,
            campaign_id: done.campaign,
        }))
    }
}
