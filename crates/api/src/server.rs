//! The gRPC server: every service behind the token check, plus reflection.

use std::future::Future;
use std::sync::Arc;

use nanoprobe_proto::v1alpha1::campaign_service_server::CampaignServiceServer;
use nanoprobe_proto::v1alpha1::commands_service_server::CommandsServiceServer;
use nanoprobe_proto::v1alpha1::events_service_server::EventsServiceServer;
use nanoprobe_proto::v1alpha1::extension_service_server::ExtensionServiceServer;
use nanoprobe_proto::v1alpha1::mission_service_server::MissionServiceServer;
use nanoprobe_proto::v1alpha1::moderation_service_server::ModerationServiceServer;
use nanoprobe_proto::v1alpha1::players_service_server::PlayersServiceServer;
use nanoprobe_proto::v1alpha1::session_service_server::SessionServiceServer;
use nanoprobe_proto::v1alpha1::world_service_server::WorldServiceServer;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::service::Routes;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;

use crate::auth::Tokens;
use crate::config::Config;
use crate::hub::Hub;
use crate::services::{
    campaign::Campaigns, commands::Commands, events::Events, extension::Extension,
    mission::Mission, moderation::Moderation, players::Players, session::Session, world::World,
};

/// The tokens a listener uses: the file when one is configured; none on
/// loopback; and a refusal for any other address without a file
/// (spec/config.md, `[api] listen`). Call it before binding, so a bad
/// setting refuses to start.
pub fn tokens_for(cfg: &Config) -> Result<Tokens, String> {
    match &cfg.tokens_path {
        Some(p) => Tokens::read(p),
        None if cfg.listen.ip().is_loopback() => Ok(Tokens::open()),
        None => Err(format!(
            "listener {} is not loopback and no tokens_path is set",
            cfg.listen
        )),
    }
}

/// Every service, each behind the token check, reflection included.
pub fn routes(hub: &Arc<Hub>, tokens: Tokens) -> Result<Routes, String> {
    let t = Arc::new(tokens);
    macro_rules! guarded {
        ($server:ident, $svc:expr) => {{
            let t = Arc::clone(&t);
            $server::with_interceptor($svc, move |r| t.check(r))
        }};
    }
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(nanoprobe_proto::FILE_DESCRIPTOR_SET)
        .build_v1()
        .map_err(|e| format!("reflection: {e}"))?;
    let reflection = {
        let t = Arc::clone(&t);
        InterceptedService::new(reflection, move |r| t.check(r))
    };
    let mut b = Routes::builder();
    b.add_service(guarded!(SessionServiceServer, Session(Arc::clone(hub))))
        .add_service(guarded!(WorldServiceServer, World(Arc::clone(hub))))
        .add_service(guarded!(EventsServiceServer, Events(Arc::clone(hub))))
        .add_service(guarded!(CommandsServiceServer, Commands(Arc::clone(hub))))
        .add_service(guarded!(CampaignServiceServer, Campaigns(Arc::clone(hub))))
        .add_service(guarded!(MissionServiceServer, Mission(Arc::clone(hub))))
        .add_service(guarded!(PlayersServiceServer, Players(Arc::clone(hub))))
        .add_service(guarded!(ExtensionServiceServer, Extension(Arc::clone(hub))))
        .add_service(guarded!(
            ModerationServiceServer,
            Moderation(Arc::clone(hub))
        ))
        .add_service(reflection);
    Ok(b.routes())
}

/// How long open streams may run after `shutdown` before they are dropped.
pub const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Attach, start the driver and serve `routes` on `listener` until
/// `shutdown`. At `shutdown` every open stream ends with `UNAVAILABLE`, and
/// calls in flight get [`SHUTDOWN_GRACE`] to finish. A driver
/// that stops (a panic in a step) ends the server with an error, so the
/// process exits and its supervisor can restart it. A clean stop closes
/// every open store ([`crate::hub::Inner::close_all`]).
pub async fn serve(
    hub: Arc<Hub>,
    routes: Routes,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), String> {
    hub.start();
    let mut driver = tokio::spawn(Arc::clone(&hub).drive());
    let (fired, on_fire) = tokio::sync::oneshot::channel::<()>();
    let server = Server::builder()
        .add_routes(routes)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), {
            let hub = Arc::clone(&hub);
            async move {
                shutdown.await;
                // Streams never end by themselves: end them, so the
                // graceful shutdown can finish.
                hub.close();
                let _ = fired.send(());
            }
        });
    let grace = async move {
        if on_fire.await.is_ok() {
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::select! {
        r = server => r.map_err(|e| format!("server: {e}")),
        () = grace => Ok(()),
        d = &mut driver => {
            // The streams end with their status before the server goes.
            hub.close();
            tokio::time::sleep(SHUTDOWN_GRACE / 4).await;
            match d {
                Ok(Ok(())) => Err("the driver stopped".into()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!("the driver task failed: {e}")),
            }
        }
    };
    driver.abort();
    // A clean stop closes the stores cleanly; after a panic the state may
    // be half-changed, and the next open treats the stop as unclean.
    if result.is_ok() {
        hub.lock().close_all()?;
    }
    result
}
