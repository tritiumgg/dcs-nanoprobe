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
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::sim::wire::Body;
use nanoprobe_sidecar::store::{Kind, MarkerError, MarkerSpec, scope};
use tonic::{Code, Request, Response, Status};

use super::commands::{OVERHEAD, dcs_not_ready, fits, lv_len, place, scope_of, unleased};
use crate::auth::{Access, require};
use crate::convert::{coalition_of_dcs, dcs_coalition, invalid, not_built, not_found};
use crate::hub::{Call, Hub, Inner, Need};
use crate::idem::EffectKind;
use crate::lua::to_lua;

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
            "the campaign has used every entity ID",
            0,
            "",
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
    let (id, _) = slot
        .store
        .put_marker(
            EntityId(r.mark_id),
            scope,
            target,
            MarkerSpec::Mark {
                theater,
                at,
                text: r.text.clone(),
                read_only: r.read_only,
            },
        )
        .map_err(|e| marker_error(e, "mark_id"))?;
    Ok(PutMarkResponse {
        mark_id: id.0,
        revision: slot.store.revision(),
    })
}

fn put_drawing(g: &mut Inner, c: u64, r: &PutDrawingRequest) -> Result<PutDrawingResponse, Status> {
    let limit = g.backend.max_command();
    let (scope, target) = pin_scope(g, c, r.audience.as_ref(), "audience", false)?;
    let args = to_lua(&prost_types::Value {
        kind: Some(prost_types::value::Kind::ListValue(
            r.args.clone().unwrap_or_default(),
        )),
    });
    fits(OVERHEAD + lv_len(&args), limit, "args")?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    let (id, _) = slot
        .store
        .put_marker(
            EntityId(r.drawing_id),
            scope,
            target,
            MarkerSpec::Drawing {
                shape: r.shape,
                args,
            },
        )
        .map_err(|e| marker_error(e, "drawing_id"))?;
    Ok(PutDrawingResponse {
        drawing_id: id.0,
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
        let campaign = g.active.unwrap_or(0);
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
                pb::Player {
                    key: p.key.clone(),
                    name: p.name().to_owned(),
                    connected: p.connected,
                    coalition: coalition_of_dcs(p.side) as i32,
                    unit_id: seat.map_or(0, |o| o.unit),
                    seat: seat.map_or(0, |o| o.seat),
                    fuel_used: g.roster.fuel_used(&p.key, campaign),
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
