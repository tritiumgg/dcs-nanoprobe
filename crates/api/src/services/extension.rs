//! `ExtensionService`: Lua extensions and raw Lua. Schemas, author events
//! and data arrive with their own steps.
//!
//! An extension is desired state: the store holds it, and the reconciler
//! registers it with the agent and again after every DCS restart. Invoking an
//! extension and evaluating source are effects. A server whose agent has raw
//! Lua off answers them `RAW_LUA_OFF` in the result.

use std::sync::Arc;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::extension_service_server::ExtensionService;
use nanoprobe_proto::v1alpha1::{
    ErrorReason, EvalLuaRequest, EvalLuaResponse, ExtensionStatus, GetDataRequest, GetDataResponse,
    InvokeLuaRequest, InvokeLuaResponse, ListLuaRequest, ListLuaResponse, PublishEventRequest,
    PublishEventResponse, PutDataRequest, PutDataResponse, RegisterLuaRequest, RegisterLuaResponse,
    RegisterSchemaRequest, RegisterSchemaResponse, RemoveLuaRequest, RemoveLuaResponse,
};
use tonic::{Code, Request, Response, Status};

use super::commands::{OVERHEAD, dcs_not_ready, fits, unleased};
use crate::auth::{Access, require};
use crate::convert::{invalid, not_built};
use crate::hub::{Call, Hub, Inner, Need};
use crate::idem::EffectKind;
use crate::lua::to_lua;

/// The service.
pub struct Extension(pub Arc<Hub>);

fn no_extension(name: &str) -> Status {
    status(
        Code::NotFound,
        ErrorReason::ExtensionNotFound,
        format!("no extension named `{name}`"),
        0,
        "name",
    )
}

fn name_ok(name: &str) -> Result<(), Status> {
    if name.is_empty() || name.len() > 128 {
        Err(invalid("name", "1 to 128 bytes"))
    } else {
        Ok(())
    }
}

fn register_lua(
    g: &mut Inner,
    c: u64,
    r: &RegisterLuaRequest,
) -> Result<RegisterLuaResponse, Status> {
    name_ok(&r.name)?;
    if r.source.is_empty() {
        return Err(invalid("source", "an extension needs its source"));
    }
    fits(
        2 * OVERHEAD + r.name.len() + r.source.len(),
        g.backend.max_command(),
        "source",
    )?;
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    Ok(RegisterLuaResponse {
        revision: slot.store.set_extension(&r.name, &r.source),
    })
}

fn remove_lua(g: &mut Inner, c: u64, r: &RemoveLuaRequest) -> Result<RemoveLuaResponse, Status> {
    let slot = g.campaigns.get_mut(&c).expect("resolved campaign");
    if slot.store.remove_extension(&r.name) {
        Ok(RemoveLuaResponse {})
    } else {
        Err(no_extension(&r.name))
    }
}

fn invoke_start(g: &mut Inner, c: u64, r: &InvokeLuaRequest) -> Result<(u64, u64), Status> {
    let slot = g.campaigns.get(&c).expect("resolved campaign");
    if slot.store.extension(&r.name).is_none() {
        return Err(no_extension(&r.name));
    }
    let args = to_lua(&prost_types::Value {
        kind: Some(prost_types::value::Kind::ListValue(
            r.args.clone().unwrap_or_default(),
        )),
    });
    fits(
        2 * OVERHEAD + r.name.len() + super::commands::lv_len(&args),
        g.backend.max_command(),
        "args",
    )?;
    g.with_rec(c, |rec, store, sim| rec.invoke(store, sim, &r.name, args))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

fn eval_start(g: &mut Inner, c: u64, r: &EvalLuaRequest) -> Result<(u64, u64), Status> {
    if r.source.is_empty() {
        return Err(invalid("source", "source is required"));
    }
    fits(
        2 * OVERHEAD + r.source.len(),
        g.backend.max_command(),
        "source",
    )?;
    g.with_rec(c, |rec, store, sim| rec.eval(store, sim, &r.source))
        .flatten()
        .ok_or_else(dcs_not_ready)
}

#[tonic::async_trait]
impl ExtensionService for Extension {
    async fn register_schema(
        &self,
        _req: Request<RegisterSchemaRequest>,
    ) -> Result<Response<RegisterSchemaResponse>, Status> {
        Err(not_built("RegisterSchema"))
    }

    async fn publish_event(
        &self,
        _req: Request<PublishEventRequest>,
    ) -> Result<Response<PublishEventResponse>, Status> {
        Err(not_built("PublishEvent"))
    }

    async fn put_data(
        &self,
        _req: Request<PutDataRequest>,
    ) -> Result<Response<PutDataResponse>, Status> {
        Err(not_built("PutData"))
    }

    async fn get_data(
        &self,
        _req: Request<GetDataRequest>,
    ) -> Result<Response<GetDataResponse>, Status> {
        Err(not_built("GetData"))
    }

    async fn register_lua(
        &self,
        req: Request<RegisterLuaRequest>,
    ) -> Result<Response<RegisterLuaResponse>, Status> {
        require(&req, Access::Admin)?;
        super::commands::changing!(self, req, "RegisterLua", Need::Campaign, register_lua)
    }

    async fn remove_lua(
        &self,
        req: Request<RemoveLuaRequest>,
    ) -> Result<Response<RemoveLuaResponse>, Status> {
        require(&req, Access::Admin)?;
        super::commands::changing!(self, req, "RemoveLua", Need::Campaign, remove_lua)
    }

    async fn list_lua(
        &self,
        req: Request<ListLuaRequest>,
    ) -> Result<Response<ListLuaResponse>, Status> {
        require(&req, Access::Observer)?;
        let mut g = self.0.lock();
        let c = g.resolve_read(req.get_ref().campaign_id)?;
        let slot = g.campaigns.get(&c).expect("resolved campaign");
        // The agent's answers and records are about the active campaign.
        let live = g.active == Some(c);
        let extensions = slot
            .store
            .extensions()
            .map(|e| {
                let stats = live.then(|| g.link.extensions.get(&e.name)).flatten();
                ExtensionStatus {
                    name: e.name.clone(),
                    revision: e.spec_rev,
                    applied_revision: g
                        .rec
                        .as_ref()
                        .filter(|_| live)
                        .map_or(0, |r| r.extension_applied(&e.name)),
                    calls: stats.map_or(0, |s| s.calls),
                    total_us: stats.map_or(0, |s| s.total_us),
                    max_us: stats.map_or(0, |s| s.max_us),
                    events_skipped: stats.map_or(0, |s| s.events_skipped),
                    stopped: stats.is_some_and(|s| s.stopped != 0),
                }
            })
            .collect();
        Ok(Response::new(ListLuaResponse { extensions }))
    }

    async fn invoke_lua(
        &self,
        req: Request<InvokeLuaRequest>,
    ) -> Result<Response<InvokeLuaResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "InvokeLua",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Invoke,
                |g: &mut Inner, c| invoke_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn eval_lua(
        &self,
        req: Request<EvalLuaRequest>,
    ) -> Result<Response<EvalLuaResponse>, Status> {
        require(&req, Access::Admin)?;
        let r = req.into_inner();
        let mut hashed = r.clone();
        hashed.write = unleased(&r.write);
        let w = r.write.clone().unwrap_or_default();
        let resp = self
            .0
            .effect(
                Call {
                    method: "EvalLua",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                EffectKind::Eval,
                |g: &mut Inner, c| eval_start(g, c, &r),
            )
            .await?;
        Ok(Response::new(resp))
    }
}
