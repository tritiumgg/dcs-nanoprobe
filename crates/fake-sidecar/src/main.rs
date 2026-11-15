//! `nanoprobe-fake-sidecar`: the API server over the fake sim.

use std::process::ExitCode;
use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_fake_sidecar::{fake_config, start};

const USAGE: &str = "\
Usage: nanoprobe-fake-sidecar [--listen ADDR] [--tokens PATH] [--frame-ms MS]
                              [--store DIR] [--run PROGRAM [ARGS...]]

Serve the nanoprobe API over a fake DCS, for development and tests.

--listen ADDR   Where to listen (default 127.0.0.1:47311; port 0 picks one).
--tokens PATH   A tokens file (spec/config.md); required off loopback.
--frame-ms MS   Milliseconds per fake frame (default 20).
--store DIR     Keep the campaigns in DIR (spec/persistence.md). Without it
                they are in memory and end with the process.
--run PROGRAM   Run PROGRAM with NANOPROBE_ADDR set to the server's URL, then
                stop and exit with its exit code. Without it, serve until
                Ctrl-C.

The server prints `listening on http://ADDR` once it accepts connections.";

struct Args {
    cfg: Config,
    run: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut cfg = Config {
        step: Duration::from_millis(20),
        ..Config::default()
    };
    let mut run = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let value = |i: usize| {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--listen" => {
                cfg.listen = value(i)?.parse().map_err(|e| format!("--listen: {e}"))?;
                i += 2;
            }
            "--tokens" => {
                cfg.tokens_path = Some(value(i)?.into());
                i += 2;
            }
            "--frame-ms" => {
                let ms: u64 = value(i)?.parse().map_err(|e| format!("--frame-ms: {e}"))?;
                if ms == 0 {
                    return Err("--frame-ms is at least 1".into());
                }
                cfg.step = Duration::from_millis(ms);
                i += 2;
            }
            "--store" => {
                cfg.store = Some(StoreFiles::new(value(i)?.into()));
                i += 2;
            }
            "--run" => {
                run = args[i + 1..].to_vec();
                if run.is_empty() {
                    return Err("--run needs a program".into());
                }
                break;
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Args { cfg, run })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = match parse(&args) {
        Ok(a) => a,
        Err(e) if e.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("nanoprobe-fake-sidecar: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let server = match start(a.cfg, fake_config()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nanoprobe-fake-sidecar: {e}");
            return ExitCode::from(1);
        }
    };
    println!("listening on {}", server.endpoint());
    let code = if a.run.is_empty() {
        let _ = tokio::signal::ctrl_c().await;
        ExitCode::SUCCESS
    } else {
        let status = tokio::process::Command::new(&a.run[0])
            .args(&a.run[1..])
            .env("NANOPROBE_ADDR", server.endpoint())
            .status()
            .await;
        match status {
            Ok(s) => match s.code() {
                Some(0) => ExitCode::SUCCESS,
                Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1).max(1)),
                None => ExitCode::from(1),
            },
            Err(e) => {
                eprintln!("nanoprobe-fake-sidecar: cannot run {}: {e}", a.run[0]);
                ExitCode::from(1)
            }
        }
    };
    if let Err(e) = server.stop().await {
        eprintln!("nanoprobe-fake-sidecar: {e}");
        return ExitCode::from(1);
    }
    code
}
