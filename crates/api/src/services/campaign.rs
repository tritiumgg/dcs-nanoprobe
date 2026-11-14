//! `CampaignService` (plan 4.6). Campaigns are held in memory; step S18
//! gives each its store file, and steps S18 and S19 build finish, export and
//! delete and the mission load that activation implies.

use std::sync::Arc;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::campaign_service_server::CampaignService;
use nanoprobe_proto::v1alpha1::{
    ActivateCampaignRequest, ActivateCampaignResponse, Campaign as PbCampaign, CampaignState,
    CreateCampaignRequest, CreateCampaignResponse, DeleteCampaignRequest, DeleteCampaignResponse,
    ErrorReason, ExportCampaignRequest, ExportCampaignResponse, FinishCampaignRequest,
    FinishCampaignResponse, GetCampaignRequest, GetCampaignResponse, ListCampaignsRequest,
    ListCampaignsResponse, SuspendCampaignRequest, SuspendCampaignResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Out;
use crate::auth::{Access, require};
use crate::convert::{invalid, not_built};
use crate::hub::{Call, Hub, Inner, Need, wall_ms};

/// The service.
pub struct Campaigns(pub Arc<Hub>);

fn shown(g: &Inner, id: u64) -> PbCampaign {
    let mut c = g.campaigns[&id].meta.clone();
    // The loaded theater once the agent's frames carry this campaign; until
    // then the mission loading for it has not reported.
    if g.active == Some(id) && g.link.frame_campaign == id {
        c.loaded_theater = g.link.theater.clone();
    }
    c
}

fn timestamp_now() -> prost_types::Timestamp {
    let ms = wall_ms();
    prost_types::Timestamp {
        seconds: i64::try_from(ms / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((ms % 1000) * 1_000_000).unwrap_or(0),
    }
}

#[tonic::async_trait]
impl CampaignService for Campaigns {
    type ExportCampaignStream = Out<ExportCampaignResponse>;

    async fn create_campaign(
        &self,
        req: Request<CreateCampaignRequest>,
    ) -> Result<Response<CreateCampaignResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.lease = None;
        let cfg = self.0.cfg.clone();
        let resp = self
            .0
            .write(
                Call {
                    method: "CreateCampaign",
                    key: r.idempotency.as_ref(),
                    lease: r.lease.as_ref(),
                    campaign: 0,
                    need: Need::Nothing,
                },
                &hashed,
                |g: &mut Inner, _| {
                    if r.name.is_empty() {
                        return Err(invalid("name", "a campaign needs a name"));
                    }
                    if r.theaters.is_empty() || r.theaters.iter().any(String::is_empty) {
                        return Err(invalid("theaters", "a campaign names the maps it uses"));
                    }
                    if !r.first_theater.is_empty() && !r.theaters.contains(&r.first_theater) {
                        return Err(invalid(
                            "first_theater",
                            "the first theater is one of the theaters",
                        ));
                    }
                    if g.campaigns.values().any(|s| s.meta.name == r.name) {
                        return Err(status(
                            Code::AlreadyExists,
                            ErrorReason::CampaignExists,
                            format!("a campaign named `{}` exists", r.name),
                            0,
                            "name",
                        ));
                    }
                    let id = g.new_campaign_id();
                    g.add_campaign(
                        &cfg,
                        PbCampaign {
                            id,
                            name: r.name.clone(),
                            state: CampaignState::Suspended as i32,
                            theaters: r.theaters.clone(),
                            loaded_theater: String::new(),
                            clock: r.clock,
                            platform_schema: 1,
                            component_schemas: Default::default(),
                            created: Some(timestamp_now()),
                            outcome: None,
                        },
                        if r.first_theater.is_empty() {
                            r.theaters[0].clone()
                        } else {
                            r.first_theater.clone()
                        },
                    );
                    if r.activate {
                        g.activate(&cfg, id);
                    }
                    Ok((
                        CreateCampaignResponse {
                            campaign: Some(shown(g, id)),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn list_campaigns(
        &self,
        req: Request<ListCampaignsRequest>,
    ) -> Result<Response<ListCampaignsResponse>, Status> {
        require(&req, Access::Observer)?;
        let g = self.0.lock();
        Ok(Response::new(ListCampaignsResponse {
            campaigns: g.campaigns.keys().map(|id| shown(&g, *id)).collect(),
        }))
    }

    async fn get_campaign(
        &self,
        req: Request<GetCampaignRequest>,
    ) -> Result<Response<GetCampaignResponse>, Status> {
        require(&req, Access::Observer)?;
        let g = self.0.lock();
        let id = g.resolve_read(req.get_ref().campaign_id)?;
        Ok(Response::new(GetCampaignResponse {
            campaign: Some(shown(&g, id)),
        }))
    }

    async fn activate_campaign(
        &self,
        req: Request<ActivateCampaignRequest>,
    ) -> Result<Response<ActivateCampaignResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        if r.campaign_id == 0 {
            return Err(invalid("campaign_id", "name the campaign to activate"));
        }
        let mut hashed = r.clone();
        hashed.lease = None;
        let cfg = self.0.cfg.clone();
        let resp = self
            .0
            .write(
                Call {
                    method: "ActivateCampaign",
                    key: r.idempotency.as_ref(),
                    lease: r.lease.as_ref(),
                    campaign: r.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.activate(&cfg, id);
                    Ok((
                        ActivateCampaignResponse {
                            campaign: Some(shown(g, id)),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn suspend_campaign(
        &self,
        req: Request<SuspendCampaignRequest>,
    ) -> Result<Response<SuspendCampaignResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.lease = None;
        let cfg = self.0.cfg.clone();
        let resp = self
            .0
            .write(
                Call {
                    method: "SuspendCampaign",
                    key: r.idempotency.as_ref(),
                    lease: r.lease.as_ref(),
                    campaign: r.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.suspend(&cfg, id);
                    Ok((
                        SuspendCampaignResponse {
                            campaign: Some(shown(g, id)),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn finish_campaign(
        &self,
        _req: Request<FinishCampaignRequest>,
    ) -> Result<Response<FinishCampaignResponse>, Status> {
        Err(not_built("FinishCampaign"))
    }

    async fn export_campaign(
        &self,
        _req: Request<ExportCampaignRequest>,
    ) -> Result<Response<Self::ExportCampaignStream>, Status> {
        Err(not_built("ExportCampaign"))
    }

    async fn delete_campaign(
        &self,
        _req: Request<DeleteCampaignRequest>,
    ) -> Result<Response<DeleteCampaignResponse>, Status> {
        Err(not_built("DeleteCampaign"))
    }
}
