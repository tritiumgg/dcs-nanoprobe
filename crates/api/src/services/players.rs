//! `PlayersService`: messages, chat, marks, drawings and menu items.
//!
//! A mark, drawing or menu item is desired state: the call returns once the
//! store holds it, and the reconciler carries it to DCS. A message and a
//! chat line are effects, as `Capture` is. Players, occupancy, sounds and
//! erasure arrive with their own steps. A player is named by key; a ucid never
//! appears in a request or a response.

use std::sync::Arc;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::players_service_server::PlayersService;
use nanoprobe_proto::v1alpha1::{
    self as pb, ErasePlayerRequest, ErasePlayerResponse, ErrorReason, ListOccupancyRequest,
    ListOccupancyResponse, ListPlayersRequest, ListPlayersResponse, PlaySoundRequest,
    PlaySoundResponse, PutDrawingRequest, PutDrawingResponse, PutMarkRequest, PutMarkResponse,
    PutMenuItemRequest, PutMenuItemResponse, SendChatRequest, SendChatResponse, SendMessageRequest,
    SendMessageResponse,
};
use nanoprobe_proto::v1alpha1::{
    ListMarkupGroupsRequest, ListMarkupGroupsResponse, PutMarkupGroupRequest,
    PutMarkupGroupResponse, RemoveMarkupGroupRequest, RemoveMarkupGroupResponse,
    RemovePlayerMarkRequest, RemovePlayerMarkResponse, ReplaceMarkupGroupRequest,
    ReplaceMarkupGroupResponse,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::sim::wire::Body;
use nanoprobe_sidecar::store::{
    DrawingShape, Kind, Layer, MarkerError, MarkerPut, MarkerSpec, scope,
};
use nanoprobe_transport::codec::Lv;
use tonic::{Code, Request, Response, Status};

use super::commands::{OVERHEAD, dcs_not_ready, fits, lv_len, place, scope_of, unleased};
use crate::auth::{Access, require};
use crate::convert::{coalition_of_dcs, dcs_coalition, invalid, not_built, not_found};
use crate::hub::{Call, Hub, Inner, Need};
use crate::idem::EffectKind;

/// The service.
pub struct Players(pub Arc<Hub>);

/// Who a call is for.
#[derive(Clone, Copy, Debug)]
enum Who {
    All,
    Coalition(u32),
    Group(u64),
    Unit(u64),
}

fn who(a: Option<&pb::Audience>, field: &str) -> Result<Who, Status> {
    match a.and_then(|a| a.to.as_ref()) {
        Some(pb::audience::To::All(true)) => Ok(Who::All),
        Some(pb::audience::To::All(false)) | None => Err(invalid(field, "an audience is required")),
        Some(pb::audience::To::Coalition(c)) => dcs_coalition(*c)
            .map(Who::Coalition)
            .ok_or_else(|| invalid(field, "a coalition is required")),
        Some(pb::audience::To::GroupId(g)) => Ok(Who::Group(*g)),
        Some(pb::audience::To::UnitId(u)) => Ok(Who::Unit(*u)),
    }
}

/// The scope code and target a mark, drawing or menu item is set for. A
/// group is one the campaign holds or an adopted one; DCS need not hold it
/// yet.
fn pin_scope(
    g: &Inner,
    c: u64,
    a: Option<&pb::Audience>,
    field: &str,
    group_ok: bool,
) -> Result<(u32, u64), Status> {
    match who(a, field)? {
        Who::All => Ok((scope::ALL, 0)),
        Who::Coalition(n) => Ok((scope::COALITION, u64::from(n))),
        Who::Group(id) if group_ok => {
            let store = &g.campaigns.get(&c).expect("resolved campaign").store;
            // An adopted group, one of the mission's own, has no row in the
            // store; the agent binds it.
            let adopted = g.active == Some(c)
                && g.rec
                    .as_ref()
                    .is_some_and(|r| r.adopted().values().any(|b| b.group.0 == id));
            match store.group(EntityId(id)) {
                Some(x) if !x.removed && x.kind == Kind::Group => Ok((scope::GROUP, id)),
                _ if adopted => Ok((scope::GROUP, id)),
                _ => Err(not_found(id)),
            }
        }
        Who::Group(_) => Err(invalid(field, "a drawing is for everyone or one coalition")),
        Who::Unit(_) => Err(invalid(field, "a unit is not an audience here")),
    }
}

fn marker_error(e: MarkerError, field: &str) -> Status {
    match e {
        MarkerError::NoSuchMarker(id) => not_found(id.0),
        MarkerError::WrongKind(id) => status(
            Code::InvalidArgument,
            ErrorReason::InvalidField,
            format!("{id} is not an entity of this kind"),
            id.0,
            field,
        ),
        MarkerError::BadScope(_) => invalid(field, "the audience does not suit this kind"),
        MarkerError::BadParent(p) => status(
            Code::InvalidArgument,
            ErrorReason::InvalidField,
            format!(
                "{p} is not a submenu with the same audience, or the item would be its own ancestor"
            ),
            p.0,
            "parent_id",
        ),
        MarkerError::HasChildren(id) => status(
            Code::InvalidArgument,
            ErrorReason::InvalidField,
            format!("{id} holds items: its audience cannot change and it stays a submenu"),
            id.0,
            field,
        ),
        MarkerError::Exhausted => status(
            Code::Internal,
            ErrorReason::Internal,
            "the campaign has used every entity ID, or every DCS mark ID of a range",
            0,
            "",
        ),
        MarkerError::DcsIdsFull(_) => status(
            Code::ResourceExhausted,
            ErrorReason::DcsIdsFull,
            "the DCS ID pool has too few unused slots in this DCS process for the adds this change needs; nothing was written. Items already shown stay, and a change that adds none still applies. A new DCS process starts with all slots free. GetServerStatus reports dcs_ids_left".to_string(),
            0,
            field,
        ),
        MarkerError::LabelTaken(l) => status(
            Code::AlreadyExists,
            ErrorReason::InvalidField,
            format!("the label `{l}` is held by another item, or by one in another markup group"),
            0,
            "label",
        ),
        MarkerError::BadLabel => invalid(
            "label",
            "1 to 128 bytes with no control character, unique in the call; marks and drawings only",
        ),
        MarkerError::BadLayer => invalid(
            "markup_group",
            "1 to 64 bytes with no control character; marks and drawings only",
        ),
        MarkerError::NoSuchLayer(n) => status(
            Code::NotFound,
            ErrorReason::InvalidField,
            format!("the campaign has no markup group `{n}`"),
            0,
            "name",
        ),
        MarkerError::BadExpiry => invalid("expire_after_s", "a finite number above 0"),
        MarkerError::BadCap => invalid(
            "cap",
            "above 0, and no smaller than the items a replacement gives",
        ),
    }
}

fn put_mark(g: &mut Inner, c: u64, r: &PutMarkRequest) -> Result<PutMarkResponse, Status> {
    let limit = g.backend.max_command();
    let (scope, target) = pin_scope(g, c, r.audience.as_ref(), "audience", true)?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let at = place(r.position.as_ref(), slot, "position")?;
    let theater = r.position.as_ref().expect("placed").theater.clone();
    fits(OVERHEAD + r.text.len(), limit, "text")?;
    let put = slot
        .store
        .put_marker_with(MarkerPut {
            id: EntityId(r.mark_id),
            scope,
            target,
            spec: MarkerSpec::Mark {
                theater,
                at,
                text: r.text.clone(),
                read_only: r.read_only,
            },
            drawing: None,
            label: Some(r.label.clone()),
            layer: Some(r.markup_group.clone()),
            expire_after_s: r.expire_after_s,
        })
        .map_err(|e| marker_error(e, "mark_id"))?;
    Ok(PutMarkResponse {
        mark_id: put.id.0,
        revision: slot.store.revision(),
    })
}

/// A put's drawing: its typed shape, checked against the campaign's theaters,
/// or the drawing its deprecated `shape` and `args` describe (spec/world.md,
/// "Drawings"). Exactly one form is set. Its lowering must fit a command.
/// Every DCS markup call is made from the result's lowering, so no
/// caller-made arguments reach DCS.
fn put_shape(
    g: &Inner,
    c: u64,
    typed: Option<&pb::DrawingShape>,
    legacy: Option<(u32, Option<&prost_types::ListValue>)>,
    field: &str,
) -> Result<DrawingShape, Status> {
    // A size refusal names the field of the form the put set.
    let set = if legacy.is_some() {
        "args"
    } else {
        "typed_shape"
    };
    let d = match (typed, legacy) {
        (Some(_), Some(_)) => {
            return Err(invalid(
                &format!("{field}typed_shape"),
                "set typed_shape or the deprecated shape and args, not both",
            ));
        }
        (None, None) => {
            return Err(invalid(
                &format!("{field}typed_shape"),
                "a drawing is required",
            ));
        }
        (Some(t), None) => {
            let theaters = &g.campaigns[&c].meta.theaters;
            crate::drawing::from_pb(t, &format!("{field}typed_shape"), theaters)?
        }
        (None, Some((shape, args))) => {
            crate::drawing::from_legacy(shape, args, &format!("{field}args"))?
        }
    };
    let (_, args) = d.lower();
    fits(
        OVERHEAD + lv_len(&args),
        g.backend.max_command(),
        &format!("{field}{set}"),
    )?;
    Ok(d)
}

/// The deprecated pair of a put, when either is set.
#[allow(deprecated)]
fn legacy_of(r: &PutDrawingRequest) -> Option<(u32, Option<&prost_types::ListValue>)> {
    (r.shape != 0 || r.args.is_some()).then_some((r.shape, r.args.as_ref()))
}

fn put_drawing(g: &mut Inner, c: u64, r: &PutDrawingRequest) -> Result<PutDrawingResponse, Status> {
    let (scope, target) = pin_scope(g, c, r.audience.as_ref(), "audience", false)?;
    let d = put_shape(g, c, r.typed_shape.as_ref(), legacy_of(r), "")?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let put = slot
        .store
        .put_marker_with(MarkerPut {
            id: EntityId(r.drawing_id),
            scope,
            target,
            spec: MarkerSpec::Drawing {
                shape: 0,
                args: Lv::Nil,
            },
            drawing: Some(d),
            label: Some(r.label.clone()),
            layer: Some(r.markup_group.clone()),
            expire_after_s: r.expire_after_s,
        })
        .map_err(|e| marker_error(e, "drawing_id"))?;
    Ok(PutDrawingResponse {
        drawing_id: put.id.0,
        revision: slot.store.revision(),
    })
}

fn put_menu_item(
    g: &mut Inner,
    c: u64,
    r: &PutMenuItemRequest,
) -> Result<PutMenuItemResponse, Status> {
    let limit = g.backend.max_command();
    let (scope, target) = pin_scope(g, c, r.audience.as_ref(), "audience", true)?;
    if r.text.is_empty() {
        return Err(invalid("text", "an item needs its text"));
    }
    fits(OVERHEAD + r.text.len(), limit, "text")?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let (id, _) = slot
        .store
        .put_marker(
            EntityId(r.item_id),
            scope,
            target,
            MarkerSpec::MenuItem {
                parent: EntityId(r.parent_id),
                submenu: r.submenu,
                text: r.text.clone(),
            },
        )
        .map_err(|e| marker_error(e, "item_id"))?;
    Ok(PutMenuItemResponse {
        item_id: id.0,
        revision: slot.store.revision(),
    })
}

/// A markup group as the API shows it.
pub(crate) fn markup_group_pb(l: &Layer) -> pb::MarkupGroup {
    pb::MarkupGroup {
        name: l.name.clone(),
        visible: l.visible,
        expire_after_s: l.expire_after_s,
        cap: l.cap,
    }
}

fn put_markup_group(
    g: &mut Inner,
    c: u64,
    r: &PutMarkupGroupRequest,
) -> Result<PutMarkupGroupResponse, Status> {
    let l = r
        .markup_group
        .as_ref()
        .ok_or_else(|| invalid("markup_group", "a markup group is required"))?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    slot.store
        .put_layer(Layer {
            name: l.name.clone(),
            visible: l.visible,
            expire_after_s: l.expire_after_s,
            cap: l.cap,
        })
        .map_err(|e| match e {
            MarkerError::BadLayer => invalid(
                "markup_group.name",
                "1 to 64 bytes with no control character",
            ),
            MarkerError::BadExpiry => {
                invalid("markup_group.expire_after_s", "a finite number above 0")
            }
            MarkerError::BadCap => invalid("markup_group.cap", "above 0"),
            e => marker_error(e, "markup_group"),
        })?;
    Ok(PutMarkupGroupResponse {
        revision: slot.store.revision(),
    })
}

fn remove_markup_group(
    g: &mut Inner,
    c: u64,
    r: &RemoveMarkupGroupRequest,
) -> Result<RemoveMarkupGroupResponse, Status> {
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    slot.store
        .remove_layer(&r.name)
        .map_err(|e| marker_error(e, "name"))?;
    Ok(RemoveMarkupGroupResponse {
        revision: slot.store.revision(),
    })
}

fn replace_markup_group(
    g: &mut Inner,
    c: u64,
    r: &ReplaceMarkupGroupRequest,
) -> Result<ReplaceMarkupGroupResponse, Status> {
    let limit = g.backend.max_command();
    let mut puts = Vec::with_capacity(r.items.len());
    for (i, item) in r.items.iter().enumerate() {
        let field = format!("items[{i}]");
        let put = match &item.kind {
            Some(pb::markup_group_item::Kind::Mark(m)) => {
                let (scope, target) = pin_scope(
                    g,
                    c,
                    item.audience.as_ref(),
                    &format!("{field}.audience"),
                    true,
                )?;
                let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
                let at = place(m.position.as_ref(), slot, &format!("{field}.position"))?;
                let theater = m.position.as_ref().expect("placed").theater.clone();
                fits(OVERHEAD + m.text.len(), limit, &format!("{field}.text"))?;
                (
                    scope,
                    target,
                    MarkerSpec::Mark {
                        theater,
                        at,
                        text: m.text.clone(),
                        read_only: m.read_only,
                    },
                    None,
                )
            }
            Some(pb::markup_group_item::Kind::Drawing(d)) => {
                let (scope, target) = pin_scope(
                    g,
                    c,
                    item.audience.as_ref(),
                    &format!("{field}.audience"),
                    false,
                )?;
                #[allow(deprecated)]
                if d.shape != 0 || d.args.is_some() {
                    return Err(invalid(
                        &format!("{field}.drawing.shape"),
                        "unused: set typed_shape",
                    ));
                }
                let shape = put_shape(
                    g,
                    c,
                    Some(d.typed_shape.as_ref().ok_or_else(|| {
                        invalid(
                            &format!("{field}.drawing.typed_shape"),
                            "a drawing is required",
                        )
                    })?),
                    None,
                    &format!("{field}.drawing."),
                )?;
                (
                    scope,
                    target,
                    MarkerSpec::Drawing {
                        shape: 0,
                        args: Lv::Nil,
                    },
                    Some(shape),
                )
            }
            None => return Err(invalid(&field, "a mark or a drawing is required")),
        };
        let (scope, target, spec, drawing) = put;
        puts.push(MarkerPut {
            id: EntityId(0),
            scope,
            target,
            spec,
            drawing,
            label: Some(item.label.clone()),
            layer: Some(r.name.clone()),
            expire_after_s: item.expire_after_s,
        });
    }
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let (ids, _) = slot
        .store
        .replace_layer(&r.name, puts)
        .map_err(|e| match e {
            MarkerError::BadLayer => invalid("name", "1 to 64 bytes with no control character"),
            MarkerError::BadLabel => invalid(
                "items.label",
                "every item needs a label of 1 to 128 bytes with no control character, unique in the call",
            ),
            MarkerError::BadCap => invalid("items", "more items than the markup group's cap"),
            e => marker_error(e, "items"),
        })?;
    Ok(ReplaceMarkupGroupResponse {
        ids: ids.into_iter().map(|(_, id)| id.0).collect(),
        revision: slot.store.revision(),
    })
}

fn player_mark_start(
    g: &mut Inner,
    c: u64,
    r: &RemovePlayerMarkRequest,
) -> Result<(u64, u64), Status> {
    if r.mark_id == 0 || r.mark_id >= 1 << 53 {
        return Err(invalid("mark_id", "1 to 2^53 - 1"));
    }
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    if slot.store.markers().any(|m| m.dcs_id == r.mark_id) || slot.store.dcs_issued_now(r.mark_id) {
        return Err(invalid(
            "mark_id",
            "the platform issued this mark ID in this DCS process: remove the item with RemoveEntity",
        ));
    }
    // The event names the mark: it must be held, be a mark event with this
    // ID, and come from the DCS process and mission generation now running,
    // since DCS gives the ID to another mark elsewhere.
    let (held_event, boot) = slot.events.get(r.event_sequence).ok_or_else(|| {
        status(
            Code::FailedPrecondition,
            ErrorReason::InvalidField,
            "the log no longer holds this event: act on a mark event while it is held",
            0,
            "event_sequence",
        )
    })?;
    let marks_this = match &held_event.body {
        Some(pb::event::Body::World(w)) => {
            (25..=27).contains(&w.dcs_id) && w.mark_id == Some(r.mark_id)
        }
        _ => false,
    };
    if !marks_this {
        return Err(invalid(
            "event_sequence",
            "a mark event (DCS event 25 to 27) with this mark_id",
        ));
    }
    let at = held_event.stamp.unwrap_or_default();
    let running = slot.store.loaded().map(|l| (l.agent_boot, l.mission_gen));
    if running != Some((boot, at.mission_generation)) {
        return Err(status(
            Code::FailedPrecondition,
            ErrorReason::InvalidField,
            "the event is from another DCS process or mission generation: DCS gives the ID to another mark",
            0,
            "event_sequence",
        ));
    }
    let id = r.mark_id;
    g.with_rec(c, |rec, store, sim| rec.mark_clear(store, sim, id))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

fn message_start(g: &mut Inner, c: u64, r: &SendMessageRequest) -> Result<(u64, u64), Status> {
    if !(r.seconds.is_finite() && (0.0..=86_400.0).contains(&r.seconds)) {
        return Err(invalid("seconds", "finite, 0 to 86400"));
    }
    if r.text.is_empty() {
        return Err(invalid("text", "a message needs its text"));
    }
    fits(OVERHEAD + r.text.len(), g.backend.max_command(), "text")?;
    let (kind, target) = match who(r.audience.as_ref(), "audience")? {
        Who::All => (pb::EffectScope::All, 0),
        Who::Coalition(n) => (pb::EffectScope::Coalition, u64::from(n)),
        Who::Group(id) => (pb::EffectScope::Group, id),
        Who::Unit(id) => (pb::EffectScope::Unit, id),
    };
    let scope = scope_of(g, c, kind as i32, target, "audience")?;
    let body = Body::Message {
        scope,
        clear: r.clear,
        target,
        seconds: r.seconds,
        text: r.text.clone(),
    };
    g.with_rec(c, |rec, store, sim| rec.message(store, sim, body))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

/// A player the call names, who must be connected: the DCS connection ID.
/// An unknown key is `NOT_FOUND`, an offline player `FAILED_PRECONDITION`.
pub(crate) fn connected(g: &Inner, key: &str, field: &str) -> Result<u32, Status> {
    if !g.roster.knows(key) {
        return Err(status(
            Code::NotFound,
            ErrorReason::PlayerNotFound,
            "the server has no player with this key",
            0,
            field,
        ));
    }
    g.roster.conn_of(key).ok_or_else(|| {
        status(
            Code::FailedPrecondition,
            ErrorReason::PlayerNotFound,
            "the player is not connected",
            0,
            field,
        )
    })
}

fn chat_start(g: &mut Inner, c: u64, r: &SendChatRequest) -> Result<(u64, u64), Status> {
    if r.text.is_empty() || r.text.len() > 1024 {
        return Err(invalid("text", "1 to 1024 bytes"));
    }
    let conn = if r.player_key.is_empty() {
        0
    } else {
        connected(g, &r.player_key, "player_key")?
    };
    g.with_rec(c, |rec, store, sim| rec.chat(store, sim, conn, &r.text))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

#[tonic::async_trait]
impl PlayersService for Players {
    async fn list_players(
        &self,
        req: Request<ListPlayersRequest>,
    ) -> Result<Response<ListPlayersResponse>, Status> {
        require(&req, Access::Observer)?;
        let r = req.into_inner();
        let g = self.0.lock();
        let players = g
            .roster
            .players()
            .filter(|p| p.connected || r.include_offline)
            .map(|p| {
                let seat = g
                    .roster
                    .occupancies()
                    .filter(|o| o.key == p.key && o.to.is_none())
                    .min_by_key(|o| o.seat);
                // The loadout of the seat's unit, when the unit has an ID.
                let loadout = seat
                    .filter(|o| o.unit != 0)
                    .and_then(|o| g.loadouts.get(o.object));
                pb::Player {
                    ammo: loadout
                        .map_or_else(Default::default, |l| l.ammo.clone().into_iter().collect()),
                    ammo_read: loadout.map(crate::convert::ammo_read),
                    key: p.key.clone(),
                    name: p.name().to_owned(),
                    connected: p.connected,
                    coalition: coalition_of_dcs(p.side) as i32,
                    unit_id: seat.map_or(0, |o| o.unit),
                    seat: seat.map_or(0, |o| o.seat),
                }
            })
            .collect();
        Ok(Response::new(ListPlayersResponse { players }))
    }

    async fn list_occupancy(
        &self,
        req: Request<ListOccupancyRequest>,
    ) -> Result<Response<ListOccupancyResponse>, Status> {
        require(&req, Access::Observer)?;
        let r = req.into_inner();
        let mut g = self.0.lock();
        let campaign = g.resolve_read(r.campaign_id)?;
        let secs = |t: &Option<prost_types::Timestamp>| {
            t.as_ref()
                .map(|t| t.seconds as f64 + f64::from(t.nanos) / 1e9)
        };
        let (from, to) = (secs(&r.from), secs(&r.to));
        let ts = |s: f64| prost_types::Timestamp {
            seconds: s.floor() as i64,
            nanos: ((s - s.floor()) * 1e9) as i32,
        };
        let occupancy = g
            .roster
            .occupancies()
            .filter(|o| o.campaign == campaign)
            .filter(|o| r.player_key.is_empty() || o.key == r.player_key)
            .filter(|o| r.unit_id == 0 || o.unit == r.unit_id)
            .filter(|o| from.is_none_or(|f| o.to.is_none_or(|t| t >= f)))
            .filter(|o| to.is_none_or(|t| o.from <= t))
            .map(|o| pb::Occupancy {
                player_key: o.key.clone(),
                unit_id: o.unit,
                seat: o.seat,
                from: Some(ts(o.from)),
                to: o.to.map(ts),
            })
            .collect();
        Ok(Response::new(ListOccupancyResponse { occupancy }))
    }

    async fn send_message(
        &self,
        req: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "SendMessage",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Message,
                |g: &mut Inner, c| message_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn play_sound(
        &self,
        _req: Request<PlaySoundRequest>,
    ) -> Result<Response<PlaySoundResponse>, Status> {
        Err(not_built("PlaySound"))
    }

    async fn send_chat(
        &self,
        req: Request<SendChatRequest>,
    ) -> Result<Response<SendChatResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "SendChat",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Chat,
                |g: &mut Inner, c| chat_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn put_markup_group(
        &self,
        req: Request<PutMarkupGroupRequest>,
    ) -> Result<Response<PutMarkupGroupResponse>, Status> {
        super::commands::changing!(
            self,
            req,
            "PutMarkupGroup",
            Need::Campaign,
            put_markup_group
        )
    }

    async fn remove_markup_group(
        &self,
        req: Request<RemoveMarkupGroupRequest>,
    ) -> Result<Response<RemoveMarkupGroupResponse>, Status> {
        super::commands::changing!(
            self,
            req,
            "RemoveMarkupGroup",
            Need::Campaign,
            remove_markup_group
        )
    }

    async fn replace_markup_group(
        &self,
        req: Request<ReplaceMarkupGroupRequest>,
    ) -> Result<Response<ReplaceMarkupGroupResponse>, Status> {
        super::commands::changing!(
            self,
            req,
            "ReplaceMarkupGroup",
            Need::Campaign,
            replace_markup_group
        )
    }

    async fn list_markup_groups(
        &self,
        req: Request<ListMarkupGroupsRequest>,
    ) -> Result<Response<ListMarkupGroupsResponse>, Status> {
        require(&req, Access::Observer)?;
        let r = req.into_inner();
        let mut g = self.0.lock();
        let campaign = g.resolve_read(r.campaign_id)?;
        g.open_archive(campaign)?;
        let store = &g.campaigns[&campaign].store;
        let markup_groups = store.layers().map(markup_group_pb).collect();
        Ok(Response::new(ListMarkupGroupsResponse { markup_groups }))
    }

    async fn remove_player_mark(
        &self,
        req: Request<RemovePlayerMarkRequest>,
    ) -> Result<Response<RemovePlayerMarkResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "RemovePlayerMark",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::PlayerMark,
                |g: &mut Inner, c| player_mark_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn put_mark(
        &self,
        req: Request<PutMarkRequest>,
    ) -> Result<Response<PutMarkResponse>, Status> {
        super::commands::changing!(self, req, "PutMark", Need::Campaign, put_mark)
    }

    async fn put_drawing(
        &self,
        req: Request<PutDrawingRequest>,
    ) -> Result<Response<PutDrawingResponse>, Status> {
        super::commands::changing!(self, req, "PutDrawing", Need::Campaign, put_drawing)
    }

    async fn put_menu_item(
        &self,
        req: Request<PutMenuItemRequest>,
    ) -> Result<Response<PutMenuItemResponse>, Status> {
        super::commands::changing!(self, req, "PutMenuItem", Need::Campaign, put_menu_item)
    }

    async fn erase_player(
        &self,
        _req: Request<ErasePlayerRequest>,
    ) -> Result<Response<ErasePlayerResponse>, Status> {
        Err(not_built("ErasePlayer"))
    }
}
