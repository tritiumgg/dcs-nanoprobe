//! `stub-sidecar --map PATH --port N --token-file PATH --dir PATH
//! [--poll-ms N] [--handshake-timeout-s N]`
//!
//! Exits 3 when the agent stays gone for the handshake timeout.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use nanoprobe_fake_agent::stub::Stub;
use nanoprobe_fake_agent::{arg, read_token};
use nanoprobe_sidecar::link::{self, Config, End};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let need = |name: &str| arg(&args, name).ok_or(format!("{name} is required"));
    let run = || -> Result<End, String> {
        let token =
            read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
        let port: u16 = need("--port")?
            .parse()
            .map_err(|_| "--port takes a number")?;
        let num = |name: &str, default: u64| -> Result<u64, String> {
            arg(&args, name).map_or(Ok(default), |v| {
                v.parse().map_err(|_| format!("{name} takes a number"))
            })
        };
        let cfg = Config {
            map_path: need("--map")?.into(),
            handshake: ([127, 0, 0, 1], port).into(),
            token,
            poll: Duration::from_millis(num("--poll-ms", 2)?),
            handshake_timeout: Duration::from_secs(num("--handshake-timeout-s", 15)?),
        };
        let mut stub =
            Stub::open(need("--dir")?.as_ref(), true).map_err(|e| format!("journal: {e}"))?;
        Ok(link::run(&cfg, &mut stub, &AtomicBool::new(false)))
    };
    match run() {
        Ok(End::Gone) => {
            eprintln!("stub-sidecar: the agent is gone");
            ExitCode::from(3)
        }
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("stub-sidecar: {e}");
            ExitCode::from(2)
        }
    }
}
