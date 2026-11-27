//! The API server over the fake sim: a [`Backend`] for
//! [`FakeSim`](nanoprobe_sim_fake::FakeSim), and helpers to run it in a test
//! or as a process. The process, `nanoprobe-fake-sidecar`, ships in the release
//! archives for controller development (spec/install.md).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use nanoprobe_api::config::Config;
use nanoprobe_api::hub::{Backend, Hub};
use nanoprobe_proto::v1alpha1::Health;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sim_fake::{Config as FakeConfig, FakeSim};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint, Server};

/// The fake sim as the server's backend.
pub struct Fake(pub FakeSim);

impl Backend for Fake {
    fn sim(&mut self) -> &mut dyn SimAdapter {
        &mut self.0
    }

    fn frame(&mut self) {
        self.0.frame();
    }

    fn attach(&mut self, sidecar_boot: u64) {
        self.0.attach(sidecar_boot);
    }

    fn detach(&mut self) {
        self.0.detach();
    }

    fn load_mission(&mut self, theater: &str) {
        if theater.is_empty() {
            self.0.load_mission();
        } else {
            self.0.load_mission_on(theater);
        }
    }

    fn health(&self) -> Health {
        if self.0.alive() {
            Health::Healthy
        } else {
            Health::Gone
        }
    }

    fn heartbeat_age(&self) -> Option<Duration> {
        // The fake's heartbeat is its frame, which the driver runs.
        self.0.alive().then_some(Duration::ZERO)
    }

    fn attach_error(&self) -> String {
        String::new()
    }

    fn as_any(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut self.0)
    }

    fn achieved(&self) -> Option<[f64; 4]> {
        // The fake samples at the rates it was told, never starved.
        let (r, received) = self.0.rates();
        (received > 0).then_some([r.position_s, r.vitals_s, r.warehouse_s, r.enumeration_s])
    }
}

/// The fake sim's settings for the server: a quick mission load, and units
/// that move 10 m per frame.
pub fn fake_config() -> FakeConfig {
    FakeConfig {
        step_m: 10.0,
        ..FakeConfig::default()
    }
}

/// A running server.
pub struct Running {
    /// The address it listens on.
    pub addr: SocketAddr,
    /// The hub, to inspect or to drive faults on the fake sim.
    pub hub: Arc<Hub>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), String>>>,
}

impl Running {
    /// The endpoint URL a client connects to.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop serving and wait for the server to end.
    pub async fn stop(mut self) -> Result<(), String> {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        match self.task.take() {
            Some(t) => t.await.map_err(|e| e.to_string())?,
            None => Ok(()),
        }
    }
}

/// Serve the API over a fresh fake sim on `cfg.listen` (port 0 picks one).
/// A setting the server refuses (a non-loopback listener without tokens, a
/// bad tokens file) fails here, before anything is bound.
pub async fn start(cfg: Config, fake: FakeConfig) -> Result<Running, String> {
    start_with(cfg, FakeSim::new(fake)).await
}

/// [`start`] over a fake sim that already runs: a sidecar that starts
/// again while DCS goes on.
pub async fn start_with(cfg: Config, sim: FakeSim) -> Result<Running, String> {
    let tokens = nanoprobe_api::server::tokens_for(&cfg)?;
    let listener = TcpListener::bind(cfg.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", cfg.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let hub = Hub::open(
        Config {
            listen: addr,
            ..cfg
        },
        Box::new(Fake(sim)),
    )?;
    let routes = nanoprobe_api::server::routes(&hub, tokens)?;
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(nanoprobe_api::server::serve(
        Arc::clone(&hub),
        routes,
        listener,
        async move {
            let _ = stopped.await;
        },
    ));
    Ok(Running {
        addr,
        hub,
        stop: Some(stop),
        task: Some(task),
    })
}

/// Serve the API over a fresh fake sim inside this process, with no socket:
/// each client connection is an in-memory pipe. Returns the hub and a
/// channel to it. The server and its driver stop when the runtime does.
pub async fn in_process(cfg: Config, fake: FakeConfig) -> Result<(Arc<Hub>, Channel), String> {
    let tokens = nanoprobe_api::server::tokens_for(&cfg)?;
    let hub = Hub::open(cfg, Box::new(Fake(FakeSim::new(fake))))?;
    let routes = nanoprobe_api::server::routes(&hub, tokens)?;
    hub.start();
    tokio::spawn(Arc::clone(&hub).drive());
    let (conns, incoming) = mpsc::channel::<Result<tokio::io::DuplexStream, std::io::Error>>(16);
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_routes(routes)
            .serve_with_incoming(ReceiverStream::new(incoming))
            .await;
    });
    let channel = Endpoint::from_static("http://in-process.invalid")
        .connect_with_connector(tower::service_fn(move |_| {
            let conns = conns.clone();
            async move {
                let (client, server) = tokio::io::duplex(64 * 1024);
                conns
                    .send(Ok(server))
                    .await
                    .map_err(|_| std::io::Error::other("the in-process server stopped"))?;
                Ok::<_, std::io::Error>(TokioIo::new(client))
            }
        }))
        .await
        .map_err(|e| format!("in-process connect: {e}"))?;
    Ok((hub, channel))
}
