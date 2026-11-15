//! `CampaignService` (plan 4.6; spec/persistence.md, "Campaign
//! operations"). With `[store]` on disk each campaign is a folder; step S19
//! builds export and delete and the mission load that activation implies.

use std::sync::Arc;

use nanoprobe_sidecar::campaigns::{Backup, BackupReason};
use nanoprobe_sidecar::ids::CampaignId;
use prost::Message;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::campaign_service_server::CampaignService;
use nanoprobe_proto::v1alpha1::{
    ActivateCampaignRequest, ActivateCampaignResponse, BackupCampaignRequest,
    BackupCampaignResponse, Campaign as PbCampaign, CampaignState, CreateCampaignRequest,
    CreateCampaignResponse, DeleteCampaignRequest, DeleteCampaignResponse, ErrorReason,
    ExportCampaignRequest, ExportCampaignResponse, FinishCampaignRequest, FinishCampaignResponse,
    GetCampaignRequest, GetCampaignResponse, ListCampaignBackupsRequest,
    ListCampaignBackupsResponse, ListCampaignsRequest, ListCampaignsResponse,
    SuspendCampaignRequest, SuspendCampaignResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Out;
use crate::auth::{Access, require};
use crate::convert::{invalid, not_built};
use crate::hub::{Call, Hub, Inner, Need, disk_status, wall_ms};

/// The service.
pub struct Campaigns(pub Arc<Hub>);

/// The refusal of a backup call with the stores in memory.
fn in_memory() -> Status {
    status(
        Code::FailedPrecondition,
        ErrorReason::Unspecified,
        "the stores are in memory (`[store] enabled = false`): there are no backups",
        0,
        "",
    )
}

fn backup_pb(b: &Backup) -> nanoprobe_proto::v1alpha1::Backup {
    use nanoprobe_proto::v1alpha1::BackupReason as R;
    nanoprobe_proto::v1alpha1::Backup {
        id: b.number,
        created: Some(prost_types::Timestamp {
            seconds: i64::try_from(b.at_ms / 1000).unwrap_or(i64::MAX),
            nanos: i32::try_from((b.at_ms % 1000) * 1_000_000).unwrap_or(0),
        }),
        size: b.size,
        schema: b.schema,
        reason: match b.reason {
            BackupReason::Scheduled => R::Scheduled,
            BackupReason::Manual => R::Manual,
            BackupReason::PreMigration => R::PreMigration,
        } as i32,
    }
}

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
        if r.activate {
            self.0
                .capture_live(r.idempotency.as_ref(), r.lease.as_ref(), 0)
                .await;
        }
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
                            last_checkpoint: None,
                        },
                        if r.first_theater.is_empty() {
                            r.theaters[0].clone()
                        } else {
                            r.first_theater.clone()
                        },
                    )?;
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
        let mut g = self.0.lock();
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
        // A switch captures the campaign it suspends.
        let switch = self.0.lock().active.is_some_and(|a| a != r.campaign_id);
        if switch {
            self.0
                .capture_live(r.idempotency.as_ref(), r.lease.as_ref(), 0)
                .await;
        }
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
        self.0
            .capture_live(r.idempotency.as_ref(), r.lease.as_ref(), r.campaign_id)
            .await;
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
                    g.suspend(&cfg, id, None);
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
        req: Request<FinishCampaignRequest>,
    ) -> Result<Response<FinishCampaignResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.lease = None;
        let cfg = self.0.cfg.clone();
        self.0
            .capture_live(r.idempotency.as_ref(), r.lease.as_ref(), r.campaign_id)
            .await;
        let resp = self
            .0
            .write(
                Call {
                    method: "FinishCampaign",
                    key: r.idempotency.as_ref(),
                    lease: r.lease.as_ref(),
                    campaign: r.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    let outcome = r.outcome.clone().unwrap_or_default().encode_to_vec();
                    g.suspend(&cfg, id, Some(outcome));
                    Ok((
                        FinishCampaignResponse {
                            campaign: Some(shown(g, id)),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
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

    async fn backup_campaign(
        &self,
        req: Request<BackupCampaignRequest>,
    ) -> Result<Response<BackupCampaignResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.lease = None;
        // The copy runs on a worker thread, without the lock; the call
        // answers when it is done (spec/persistence.md, "Backups"). A call
        // whose key is answered skips it and gets the first answer.
        let taken = match self
            .0
            .backup_start(r.idempotency.as_ref(), r.lease.as_ref(), r.campaign_id)
            .await?
        {
            None => None,
            Some((files, id, revision)) => {
                let at = wall_ms();
                let b = tokio::task::spawn_blocking(move || {
                    files.backup(CampaignId(id), at, revision, BackupReason::Manual)
                })
                .await;
                self.0.lock().backing_up.remove(&id);
                Some(match b {
                    Ok(b) => b.map_err(|e| disk_status(id, &e)),
                    Err(_) => Err(disk_status(
                        id,
                        &nanoprobe_sidecar::store::DiskError::Corrupt(
                            "the backup worker panicked".into(),
                        ),
                    )),
                })
            }
        };
        let resp = self
            .0
            .write(
                Call {
                    method: "BackupCampaign",
                    key: r.idempotency.as_ref(),
                    lease: r.lease.as_ref(),
                    campaign: r.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.files().ok_or_else(in_memory)?;
                    let b = taken.ok_or_else(|| {
                        status(
                            Code::Aborted,
                            ErrorReason::Unspecified,
                            "the backup was not taken; call again",
                            0,
                            "",
                        )
                    })??;
                    Ok((
                        BackupCampaignResponse {
                            backup: Some(backup_pb(&b)),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn list_campaign_backups(
        &self,
        req: Request<ListCampaignBackupsRequest>,
    ) -> Result<Response<ListCampaignBackupsResponse>, Status> {
        require(&req, Access::Observer)?;
        let (files, id) = {
            let g = self.0.lock();
            // Listing reads the folder; it does not open the store.
            let id = match req.get_ref().campaign_id {
                0 => g.active,
                n => g.campaigns.contains_key(&n).then_some(n),
            }
            .ok_or_else(|| {
                status(
                    Code::NotFound,
                    ErrorReason::CampaignNotFound,
                    "no such campaign, or none is active",
                    0,
                    "campaign_id",
                )
            })?;
            (g.files().cloned().ok_or_else(in_memory)?, id)
        };
        let backups = files
            .backups(CampaignId(id))
            .map_err(|e| disk_status(id, &e))?;
        Ok(Response::new(ListCampaignBackupsResponse {
            backups: backups.iter().map(backup_pb).collect(),
        }))
    }
}
