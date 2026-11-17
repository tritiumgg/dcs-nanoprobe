//! `MissionService`: the server status, the next mission's settings and
//! the reload that writes a campaign's shell and loads it (plan 4.4).

use std::sync::Arc;

use nanoprobe_proto::v1alpha1::mission_service_server::MissionService;
use nanoprobe_proto::v1alpha1::{
    CommandResult, GetServerStatusRequest, GetServerStatusResponse, NextMission, Outcome,
    ReloadRequest, ReloadResponse, ServerStatus, SetNextMissionRequest, SetNextMissionResponse,
    SimState,
};
use tonic::{Request, Response, Status};

use crate::auth::{Access, require};
use crate::convert::{invalid, ms_of_timestamp};
use crate::hub::{Call, Inner, Need};
use crate::hub::{Hub, server_sim_state};
use crate::next::Next;

/// The service.
pub struct Mission(pub Arc<Hub>);

#[tonic::async_trait]
impl MissionService for Mission {
    async fn get_server_status(
        &self,
        req: Request<GetServerStatusRequest>,
    ) -> Result<Response<GetServerStatusResponse>, Status> {
        require(&req, Access::Observer)?;
        let g = self.0.lock();
        let health = g.backend.health();
        let space = g.space(g.active);
        let age = g.backend.heartbeat_age().map(|d| prost_types::Duration {
            seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
        });
        let state: SimState = server_sim_state(health, g.link.lifecycle);
        Ok(Response::new(GetServerStatusResponse {
            status: Some(ServerStatus {
                health: health as i32,
                heartbeat_age: age,
                sim_state: state as i32,
                theater: g.link.theater.clone(),
                now: Some(g.link.stamp()),
                campaign_id: g.active.unwrap_or(0),
                commands_queued: g.rec.as_ref().map_or(0, |r| {
                    u32::try_from(r.inflight().count()).unwrap_or(u32::MAX)
                }),
                dcs_version: g.link.dcs_version.clone(),
                agent_build: if g.link.agent_build == 0 {
                    String::new()
                } else {
                    format!("{:016x}", g.link.agent_build)
                },
                event_gaps: g.link.event_gaps,
                attached: g.link.attached,
                attach_error: g.backend.attach_error(),
                agent_interface: g.link.agent_interface,
                store_free_bytes: space.map(|(free, _)| free),
                store_space_low: space.is_some_and(|(free, held)| free < held),
                ..ServerStatus::default()
            }),
        }))
    }

    async fn set_next_mission(
        &self,
        req: Request<SetNextMissionRequest>,
    ) -> Result<Response<SetNextMissionResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let w = r.write.clone().unwrap_or_default();
        let next = next_of(r.next.as_ref())?;
        let mut hashed = r.clone();
        if let Some(h) = hashed.write.as_mut() {
            h.lease = None;
        }
        let resp = self
            .0
            .write(
                Call {
                    method: "SetNextMission",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.set_next(id, &next)?;
                    Ok((SetNextMissionResponse {}, id))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn reload(
        &self,
        req: Request<ReloadRequest>,
    ) -> Result<Response<ReloadResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let w = r.write.clone().unwrap_or_default();
        let mut hashed = r.clone();
        if let Some(h) = hashed.write.as_mut() {
            h.lease = None;
        }
        let cfg = self.0.cfg.clone();
        // The refusals come first; then the groups DCS holds are captured:
        // the load ends them.
        self.0
            .lock()
            .reload_check(w.idempotency.as_ref(), w.lease.as_ref(), w.campaign_id)?;
        self.0
            .capture_live(w.idempotency.as_ref(), w.lease.as_ref(), w.campaign_id)
            .await;
        let resp = self
            .0
            .write(
                Call {
                    method: "Reload",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.reload(&cfg, id)?;
                    Ok((
                        ReloadResponse {
                            result: Some(CommandResult {
                                outcome: Outcome::Pending as i32,
                                ..CommandResult::default()
                            }),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }
}

/// The next mission a request names. A weather table must be a table.
pub(crate) fn next_of(n: Option<&NextMission>) -> Result<Next, Status> {
    let Some(n) = n else {
        return Err(invalid("next", "the next mission is required"));
    };
    let weather = n.weather.as_ref().map(crate::lua::struct_to_lua);
    if let Some(w) = &weather {
        nanoprobe_sidecar::store::check_lv(w).map_err(|e| invalid("next.weather", e))?;
        nanoprobe_sidecar::shell::lua::write(w, 0).map_err(|e| invalid("next.weather", e))?;
    }
    if let Some(t) = &n.start {
        crate::convert::mission_date(t, "next.start")?;
    }
    Ok(Next {
        theater: n.theater.clone(),
        start_ms: n.start.as_ref().map(ms_of_timestamp),
        weather,
        dynamic: n
            .dynamic_spawn
            .iter()
            .map(|d| nanoprobe_sidecar::shell::DynamicSpawn {
                airbase: d.airbase_id,
                enabled: d.enabled,
                hot_start: d.hot_start,
            })
            .collect(),
    })
}
