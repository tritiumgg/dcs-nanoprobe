//! `MissionService`. Status is served; the shell writer and reload arrive
//! with step S19.

use std::sync::Arc;

use nanoprobe_proto::v1alpha1::mission_service_server::MissionService;
use nanoprobe_proto::v1alpha1::{
    GetServerStatusRequest, GetServerStatusResponse, ReloadRequest, ReloadResponse, ServerStatus,
    SetNextMissionRequest, SetNextMissionResponse, SimState,
};
use tonic::{Request, Response, Status};

use crate::auth::{Access, require};
use crate::convert::not_built;
use crate::hub::{Hub, server_sim_state};

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
                ..ServerStatus::default()
            }),
        }))
    }

    async fn set_next_mission(
        &self,
        _req: Request<SetNextMissionRequest>,
    ) -> Result<Response<SetNextMissionResponse>, Status> {
        Err(not_built("SetNextMission"))
    }

    async fn reload(
        &self,
        _req: Request<ReloadRequest>,
    ) -> Result<Response<ReloadResponse>, Status> {
        Err(not_built("Reload"))
    }
}
