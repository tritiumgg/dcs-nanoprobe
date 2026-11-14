//! `SessionService`: hello and the controller lease.

use std::sync::Arc;
use std::time::Instant;

use nanoprobe_proto::v1alpha1::session_service_server::SessionService;
use nanoprobe_proto::v1alpha1::{
    AcquireLeaseRequest, AcquireLeaseResponse, Capabilities, HelloRequest, HelloResponse, Lease,
    ReleaseLeaseRequest, ReleaseLeaseResponse, RenewLeaseRequest, RenewLeaseResponse,
};
use tonic::{Request, Response, Status};

use crate::auth::{Access, access_of, require};
use crate::hub::{Hub, lease_status, wall_ms};

/// The service.
pub struct Session(pub Arc<Hub>);

fn presented(l: Option<&Lease>) -> Option<(u64, &str)> {
    l.map(|l| (l.epoch, l.lease_id.as_str()))
}

fn lease_id() -> Result<String, Status> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| {
        nanoprobe_proto::errors::status(
            tonic::Code::Internal,
            nanoprobe_proto::v1alpha1::ErrorReason::Internal,
            format!("no randomness for a lease ID: {e}"),
            0,
            "",
        )
    })?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn duration(d: std::time::Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
    }
}

#[tonic::async_trait]
impl SessionService for Session {
    async fn hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloResponse>, Status> {
        let role = access_of(&req).role();
        Ok(Response::new(HelloResponse {
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version: nanoprobe_proto::SCHEMA_VERSION.to_owned(),
            capabilities: Some(Capabilities {
                raw_lua: false,
                persistence: false,
                min_position_period: Some(duration(self.0.cfg.position_period_min)),
            }),
            role: role as i32,
        }))
    }

    async fn acquire_lease(
        &self,
        req: Request<AcquireLeaseRequest>,
    ) -> Result<Response<AcquireLeaseResponse>, Status> {
        require(&req, Access::Controller)?;
        let id = lease_id()?;
        let cfg = self.0.cfg.clone();
        let (grant, ttl) = self.0.guarded(|g| {
            let grant = g
                .leases
                .acquire(Instant::now(), wall_ms(), id)
                .map_err(lease_status)?;
            // The lease changed hands: a new command epoch for the agent
            // (spec/commands.md, "Sequence, epoch and revisions").
            if g.active.is_some() {
                g.restart(&cfg);
            }
            Ok((grant, g.leases.ttl()))
        })?;
        self.0.bump();
        Ok(Response::new(AcquireLeaseResponse {
            lease: Some(Lease {
                epoch: grant.epoch,
                lease_id: grant.lease_id,
            }),
            ttl: Some(duration(ttl)),
        }))
    }

    async fn renew_lease(
        &self,
        req: Request<RenewLeaseRequest>,
    ) -> Result<Response<RenewLeaseResponse>, Status> {
        require(&req, Access::Controller)?;
        let ttl = self.0.guarded(|g| {
            g.leases
                .renew(Instant::now(), presented(req.get_ref().lease.as_ref()))
                .map_err(lease_status)?;
            Ok(g.leases.ttl())
        })?;
        Ok(Response::new(RenewLeaseResponse {
            ttl: Some(duration(ttl)),
        }))
    }

    async fn release_lease(
        &self,
        req: Request<ReleaseLeaseRequest>,
    ) -> Result<Response<ReleaseLeaseResponse>, Status> {
        require(&req, Access::Controller)?;
        self.0.guarded(|g| {
            g.leases
                .release(Instant::now(), presented(req.get_ref().lease.as_ref()))
                .map_err(lease_status)
        })?;
        Ok(Response::new(ReleaseLeaseResponse {}))
    }
}
