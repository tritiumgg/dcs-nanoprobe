//! `ModerationService`: kick, ban, unban, the ban list and slot moves.
//! Admin only (spec/commands.md, "Moderation").
//!
//! A player is named by key. The roster maps the key to the ucid and the DCS
//! connection ID the `MODERATE` command carries; the ucid goes to the agent
//! and no further. A ucid enters the API only as an admin's input, to ban a
//! connected player or to unban one; a ban of a player who is not connected is
//! refused with `UNVERIFIED`. Each applied call is a `ModerationEvent` with the
//! role of the caller's token.

use std::sync::Arc;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::moderation_service_server::ModerationService;
use nanoprobe_proto::v1alpha1::{
    self as pb, BanRequest, BanResponse, ErrorReason, KickRequest, KickResponse, ListBansRequest,
    ListBansResponse, ModerationAction, MovePlayerRequest, MovePlayerResponse, UnbanRequest,
    UnbanResponse, Write,
};
use nanoprobe_sidecar::reconciler::EffectResult;
use nanoprobe_sidecar::sim::wire::{Body, lv, moderation};
use tonic::{Code, Request, Response, Status};

use super::commands::{dcs_not_ready, unleased};
use super::players::connected;
use crate::auth::{Access, require};
use crate::convert::invalid;
use crate::hub::{Call, Hub, Inner, Need, PendingModeration};
use crate::idem::EffectKind;

/// The service.
pub struct Moderation(pub Arc<Hub>);

/// The longest reason, in bytes.
const REASON_MAX: usize = 1024;

fn unknown_player(field: &str) -> Status {
    status(
        Code::NotFound,
        ErrorReason::PlayerNotFound,
        "the server has no player with this key",
        0,
        field,
    )
}

fn check_text(v: &str, field: &str, max: usize) -> Result<(), Status> {
    if v.len() > max {
        Err(invalid(field, format!("at most {max} bytes")))
    } else {
        Ok(())
    }
}

/// Who a ban or unban names: the key and the ucid, with `learn` for a
/// ucid an admin gave.
fn target(
    g: &mut Inner,
    key: Option<&str>,
    ucid: Option<&str>,
) -> Result<(String, String), Status> {
    match (key, ucid) {
        (Some(k), _) => {
            let ucid = g
                .roster
                .ucid_of(k)
                .ok_or_else(|| unknown_player("player_key"))?
                .to_owned();
            Ok((k.to_owned(), ucid))
        }
        (None, Some(u)) => {
            check_ucid(u)?;
            let k = g.roster.learn(u);
            Ok((k, u.to_owned()))
        }
        (None, None) => Err(invalid("target", "name a player key or a ucid")),
    }
}

/// Refuse a ucid that is not 1 to 128 printable characters.
fn check_ucid(u: &str) -> Result<(), Status> {
    if u.is_empty() || u.len() > 128 || !u.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(invalid("ucid", "a ucid is 1 to 128 printable characters"));
    }
    Ok(())
}

/// The refusal for a path E34 did not verify on DCS.
fn unverified(field: &str, what: &str) -> Status {
    status(
        Code::Unimplemented,
        ErrorReason::Unverified,
        format!("{what} is not verified on DCS (E34) and is not offered"),
        0,
        field,
    )
}

/// Send a `MODERATE` and remember who asked for it.
fn send(
    g: &mut Inner,
    c: u64,
    body: Body,
    role: pb::Role,
    action: ModerationAction,
    key: String,
    reason: String,
) -> Result<(u64, u64), Status> {
    let cmd = g
        .with_rec(c, |rec, store, sim| rec.moderate(store, sim, body))
        .flatten()
        .ok_or_else(dcs_not_ready)?;
    g.moderation.push(PendingModeration {
        cmd,
        action,
        player_key: key,
        reason,
        role,
    });
    Ok(cmd)
}

#[allow(clippy::too_many_arguments)]
fn moderate(
    g: &mut Inner,
    c: u64,
    role: pb::Role,
    action: ModerationAction,
    code: u32,
    key: String,
    ucid: String,
    player: u32,
    side: u32,
    seconds: f64,
    slot: &str,
    reason: &str,
) -> Result<(u64, u64), Status> {
    let body = Body::Moderate {
        action: code,
        player,
        side,
        seconds,
        ucid,
        slot: slot.to_owned(),
        reason: reason.to_owned(),
    };
    send(g, c, body, role, action, key, reason.to_owned())
}

/// The ban list a `BANS` result holds, by player key, with no ucid. An entry
/// whose ucid maps to no key gets the key derived from it, and the map
/// remembers it, so the key can be unbanned.
pub(crate) fn bans_of(g: &mut Inner, result: Option<&EffectResult>) -> Vec<pb::Ban> {
    let Some(v) = result.and_then(|r| r.result.as_ref()).map(|r| &r.value) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in lv::items(v) {
        let Some(ucid) = lv::str_at(row, "ucid").filter(|u| !u.is_empty()) else {
            continue;
        };
        let until = lv::num_at(row, "banned_until")
            .filter(|t| t.is_finite() && *t > 0.0 && *t < 1e12)
            .map(|t| prost_types::Timestamp {
                seconds: t.floor() as i64,
                nanos: ((t - t.floor()) * 1e9) as i32,
            });
        out.push(pb::Ban {
            player_key: g.roster.learn(&ucid),
            name: lv::str_at(row, "name").unwrap_or_default(),
            until,
            reason: lv::str_at(row, "reason").unwrap_or_default(),
        });
    }
    out
}

/// Run a moderation effect: the shared path of every call.
macro_rules! moderating {
    ($self:ident, $req:ident, $method:literal, $kind:expr, $resp:ty, $start:expr) => {{
        let role = require(&$req, Access::Admin)?.role();
        let r = $req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w: Write = r.write.clone().unwrap_or_default();
        let start = $start;
        let resp: $resp = $self
            .0
            .effect(
                Call {
                    method: $method,
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                $kind,
                |g: &mut Inner, c: u64| start(g, c, role, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }};
}

#[tonic::async_trait]
impl ModerationService for Moderation {
    async fn kick(&self, req: Request<KickRequest>) -> Result<Response<KickResponse>, Status> {
        moderating!(
            self,
            req,
            "Kick",
            EffectKind::Kick,
            KickResponse,
            |g: &mut Inner, c, role, r: &KickRequest| {
                check_text(&r.reason, "reason", REASON_MAX)?;
                let ucid = g
                    .roster
                    .ucid_of(&r.player_key)
                    .ok_or_else(|| unknown_player("player_key"))?
                    .to_owned();
                let conn = connected(g, &r.player_key, "player_key")?;
                moderate(
                    g,
                    c,
                    role,
                    ModerationAction::Kick,
                    moderation::KICK,
                    r.player_key.clone(),
                    ucid,
                    conn,
                    0,
                    0.0,
                    "",
                    &r.reason,
                )
            }
        )
    }

    async fn ban(&self, req: Request<BanRequest>) -> Result<Response<BanResponse>, Status> {
        moderating!(
            self,
            req,
            "Ban",
            EffectKind::Ban,
            BanResponse,
            |g: &mut Inner, c, role, r: &BanRequest| {
                check_text(&r.reason, "reason", REASON_MAX)?;
                if !(r.seconds.is_finite() && r.seconds >= 0.0) {
                    return Err(invalid("seconds", "finite, at least 0"));
                }
                if r.seconds == 0.0 {
                    return Err(unverified(
                        "seconds",
                        "a ban of DCS's default length (seconds 0)",
                    ));
                }
                // The refusals come before the keyring learns a ucid, so a
                // refused call writes nothing.
                let (key, ucid) = match &r.target {
                    Some(pb::ban_request::Target::PlayerKey(k)) => target(g, Some(k), None)?,
                    Some(pb::ban_request::Target::Ucid(u)) => {
                        check_ucid(u)?;
                        (g.roster.keyring().key_of(u), u.to_owned())
                    }
                    None => target(g, None, None)?,
                };
                // A player who is connected is banned by connection, which
                // also removes them; any other by ucid.
                let conn = g.roster.conn_of(&key).unwrap_or(0);
                if conn == 0 {
                    return Err(unverified(
                        "target",
                        "a ban of a player who is not connected (net.banlist_add_by_ucid)",
                    ));
                }
                g.roster.learn(&ucid);
                moderate(
                    g,
                    c,
                    role,
                    ModerationAction::Ban,
                    moderation::BAN,
                    key,
                    ucid,
                    conn,
                    0,
                    r.seconds,
                    "",
                    &r.reason,
                )
            }
        )
    }

    async fn unban(&self, req: Request<UnbanRequest>) -> Result<Response<UnbanResponse>, Status> {
        moderating!(
            self,
            req,
            "Unban",
            EffectKind::Unban,
            UnbanResponse,
            |g: &mut Inner, c, role, r: &UnbanRequest| {
                let (key, ucid) = match &r.target {
                    Some(pb::unban_request::Target::PlayerKey(k)) => target(g, Some(k), None)?,
                    Some(pb::unban_request::Target::Ucid(u)) => target(g, None, Some(u))?,
                    None => target(g, None, None)?,
                };
                moderate(
                    g,
                    c,
                    role,
                    ModerationAction::Unban,
                    moderation::UNBAN,
                    key,
                    ucid,
                    0,
                    0,
                    0.0,
                    "",
                    "",
                )
            }
        )
    }

    async fn list_bans(
        &self,
        req: Request<ListBansRequest>,
    ) -> Result<Response<ListBansResponse>, Status> {
        moderating!(
            self,
            req,
            "ListBans",
            EffectKind::Bans,
            ListBansResponse,
            |g: &mut Inner, c, _role, _r: &ListBansRequest| {
                g.with_rec(c, |rec, store, sim| rec.query_bans(store, sim))
                    .flatten()
                    .ok_or_else(dcs_not_ready)
            }
        )
    }

    async fn move_player(
        &self,
        req: Request<MovePlayerRequest>,
    ) -> Result<Response<MovePlayerResponse>, Status> {
        moderating!(
            self,
            req,
            "MovePlayer",
            EffectKind::Move,
            MovePlayerResponse,
            |g: &mut Inner, c, role, r: &MovePlayerRequest| {
                check_text(&r.slot, "slot", 128)?;
                if !r.slot.is_empty() {
                    return Err(unverified(
                        "slot",
                        "a move into an aircraft slot (net.force_player_slot)",
                    ));
                }
                let ucid = g
                    .roster
                    .ucid_of(&r.player_key)
                    .ok_or_else(|| unknown_player("player_key"))?
                    .to_owned();
                let conn = connected(g, &r.player_key, "player_key")?;
                moderate(
                    g,
                    c,
                    role,
                    ModerationAction::Move,
                    moderation::SLOT,
                    r.player_key.clone(),
                    ucid,
                    conn,
                    0,
                    0.0,
                    &r.slot,
                    "",
                )
            }
        )
    }
}
