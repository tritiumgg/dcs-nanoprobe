//! `nanoprobe-sidecar`: the sidecar process. It attaches to the agent in
//! DCS and serves the API (plan 4.4, spec/config.md).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use nanoprobe_api::hub::Hub;
use nanoprobe_api::real::Real;
use nanoprobe_api::sidecar_toml::SidecarToml;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::{GetServerStatusRequest, Health, ServerStatus};
use nanoprobe_sidecar::sim::real::RealSim;
use tokio::net::TcpListener;
use tonic::transport::Endpoint;

const USAGE: &str = "\
Usage: nanoprobe-sidecar run --config PATH
       nanoprobe-sidecar health --config PATH

run      Attach to the agent in DCS and serve the API, until stopped (SIGTERM
         or Ctrl-C) or until the agent is gone after a DCS restart.
health   Ask the running sidecar for its verdict and print it with the
         seconds since the agent's last heartbeat, and why scheduled backups
         fail while the last one failed. A token, if the listener
         needs one, is read from NANOPROBE_TOKEN. Exit code: 0 healthy,
         1 hung, 2 gone, 3 disabled, 4 no answer.

PATH is sidecar.toml (spec/config.md).";

/// How often `run` checks whether the link ended by itself.
const ENDED_EVERY: Duration = Duration::from_secs(1);

fn config_path(args: &[String]) -> Result<PathBuf, String> {
    match args {
        [flag, path] if flag == "--config" => Ok(PathBuf::from(path)),
        _ => Err("expected --config PATH".into()),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.split_first() {
        Some((c, rest)) => (c.as_str(), rest),
        None => ("", &[][..]),
    };
    let result = match (command, config_path(rest)) {
        ("-h" | "--help", _) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        ("run", Ok(path)) => run(path).await,
        ("health", Ok(path)) => return health(path).await,
        (_, Err(e)) if command == "run" || command == "health" => Err(e),
        _ => Err(format!("unknown command `{command}`")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nanoprobe-sidecar: {e}");
            if e.starts_with("expected") || e.starts_with("unknown command") {
                eprintln!("\n{USAGE}");
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}

async fn run(path: PathBuf) -> Result<(), String> {
    let t = SidecarToml::read(&path)?;
    let tokens = nanoprobe_api::server::tokens_for(&t.api)?;
    let listener = TcpListener::bind(t.api.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", t.api.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let sim = RealSim::new(t.link.clone(), t.limits);
    let hub = Hub::open(
        nanoprobe_api::config::Config {
            listen: addr,
            ..t.api.clone()
        },
        Box::new(Real(sim, t.missions.clone())),
    )?;
    let routes = nanoprobe_api::server::routes(&hub, tokens)?;
    println!(
        "listening on http://{addr}; agent at {} through {}",
        t.link.handshake,
        t.link.map_path.display()
    );
    let (gone_tx, gone_rx) = tokio::sync::oneshot::channel::<()>();
    let watcher = tokio::spawn(link_ended(Arc::clone(&hub), gone_tx));
    let stopped = async move {
        tokio::select! {
            () = signal() => {}
            _ = gone_rx => eprintln!("nanoprobe-sidecar: the agent is gone after a DCS restart; exiting"),
        }
    };
    let r = nanoprobe_api::server::serve(Arc::clone(&hub), routes, listener, stopped).await;
    watcher.abort();
    let ended = hub
        .lock()
        .backend
        .as_any()
        .and_then(|a| a.downcast_mut::<RealSim>())
        .is_some_and(|s| s.ended());
    r?;
    if ended {
        return Err("the agent is gone".into());
    }
    Ok(())
}

/// Resolve when the link's session loop ended by itself.
async fn link_ended(hub: Arc<Hub>, tx: tokio::sync::oneshot::Sender<()>) {
    let mut every = tokio::time::interval(ENDED_EVERY);
    loop {
        every.tick().await;
        let ended = hub
            .lock()
            .backend
            .as_any()
            .and_then(|a| a.downcast_mut::<RealSim>())
            .is_some_and(|s| s.ended());
        if ended {
            let _ = tx.send(());
            return;
        }
    }
}

/// SIGTERM or Ctrl-C.
async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn health(path: PathBuf) -> ExitCode {
    const NO_ANSWER: u8 = 4;
    let t = match SidecarToml::read(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nanoprobe-sidecar: {e}");
            return ExitCode::from(NO_ANSWER);
        }
    };
    let mut addr: SocketAddr = t.api.listen;
    if addr.ip().is_unspecified() {
        addr.set_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    match status(addr).await {
        Ok((health, age, backup)) => {
            let (word, code) = match health {
                Health::Healthy => ("healthy", 0),
                Health::Hung => ("hung", 1),
                Health::Gone => ("gone", 2),
                Health::Disabled => ("disabled", 3),
                Health::Unspecified => ("unknown", NO_ANSWER),
            };
            match age {
                Some(a) => println!("{word} heartbeat {:.1} s ago", a.as_secs_f64()),
                None => println!("{word} no heartbeat seen"),
            }
            // Reported, not part of the verdict (spec/persistence.md,
            // "Backups").
            if let Some(why) = backup {
                println!("scheduled backups failing: {why}");
            }
            ExitCode::from(code)
        }
        Err(e) => {
            eprintln!("nanoprobe-sidecar: no answer from {addr}: {e}");
            ExitCode::from(NO_ANSWER)
        }
    }
}

/// The verdict, the heartbeat's age, and why scheduled backups fail when the
/// last one failed.
async fn status(addr: SocketAddr) -> Result<(Health, Option<Duration>, Option<String>), String> {
    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .map_err(|e| e.to_string())?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(|e| e.to_string())?;
    let mut req = tonic::Request::new(GetServerStatusRequest {});
    if let Ok(token) = std::env::var("NANOPROBE_TOKEN") {
        let v = format!("Bearer {token}")
            .parse()
            .map_err(|_| "NANOPROBE_TOKEN is not valid header text".to_owned())?;
        req.metadata_mut().insert("authorization", v);
    }
    let s = MissionServiceClient::new(channel)
        .get_server_status(req)
        .await
        .map_err(|e| e.message().to_owned())?
        .into_inner()
        .status
        .ok_or("an answer with no status")?;
    let age = s
        .heartbeat_age
        .map(|d| Duration::new(d.seconds.max(0) as u64, d.nanos.max(0) as u32));
    Ok((
        Health::try_from(s.health).unwrap_or(Health::Unspecified),
        age,
        backup_failing(&s),
    ))
}

/// Why scheduled backups fail, when the last one failed: a failure after
/// the last success, or with none.
fn backup_failing(s: &ServerStatus) -> Option<String> {
    let failing = match (&s.last_backup_failure, &s.last_backup) {
        (Some(f), Some(ok)) => (f.seconds, f.nanos) > (ok.seconds, ok.nanos),
        (Some(_), None) => true,
        _ => false,
    };
    failing.then(|| s.backup_error.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Option<prost_types::Timestamp> {
        Some(prost_types::Timestamp { seconds, nanos: 0 })
    }

    #[test]
    fn a_backup_fails_only_while_the_last_one_failed() {
        let s = |ok: i64, failed: i64| ServerStatus {
            last_backup: (ok > 0).then(|| at(ok)).flatten(),
            last_backup_failure: (failed > 0).then(|| at(failed)).flatten(),
            backup_error: if failed > 0 {
                "why".into()
            } else {
                String::new()
            },
            ..ServerStatus::default()
        };
        assert_eq!(backup_failing(&s(0, 0)), None, "no backup yet");
        assert_eq!(backup_failing(&s(10, 0)), None, "a success only");
        assert_eq!(backup_failing(&s(10, 20)).as_deref(), Some("why"));
        assert_eq!(backup_failing(&s(0, 20)).as_deref(), Some("why"));
        assert_eq!(backup_failing(&s(30, 20)), None, "a success after it");
    }
}
