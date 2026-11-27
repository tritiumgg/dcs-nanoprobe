//! `nanoprobe-fake-sidecar`: the API server over the fake sim, for
//! controller development. Released beside the sidecar.

use std::process::ExitCode;
use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_fake_sidecar::{fake_config, start_with};
use nanoprobe_sim_fake::FakeSim;

const USAGE: &str = "\
Usage: nanoprobe-fake-sidecar [OPTIONS] [--run PROGRAM [ARGS...]]

Serve the real nanoprobe API over a simulated DCS, to develop and test a
controller without a DCS server. The simulation follows what the project
has observed of DCS. A controller that works here can still behave
differently against a real server.

Options:
  --listen ADDR   Where to listen (default 127.0.0.1:47311; port 0 picks a
                  free port).
  --tokens PATH   A tokens file, in the sidecar's format; required when ADDR
                  is not a loopback address.
  --store DIR     Keep campaigns in DIR. Without it they are in memory and
                  end with the process.
  --theater NAME  Start with a mission on theater NAME loaded, as a DCS that
                  started its own mission. Without it no mission is loaded
                  until a campaign is activated.
  --frame-ms MS   Milliseconds per simulated frame (default 20).
  --run PROGRAM [ARGS...]
                  Run PROGRAM with every argument after it, and with
                  NANOPROBE_ADDR set to the server's URL. When it exits,
                  stop and exit with its exit code. Without --run, serve
                  until Ctrl-C.
  -h, --help      Print this text.

Options for the project's own tests:
  --removed-keep-s S, --dead-keep-s S, --backup-interval-s S
                  The sidecar's removed_keep_h, dead_keep_h and
                  backup_interval_h, in seconds. The backup interval needs
                  --store.
  --leak-kib-per-s N
                  Leak N KiB of memory each second.

Prints `listening on http://ADDR` once it accepts connections.";

struct Args {
    cfg: Config,
    theater: Option<String>,
    run: Vec<String>,
    leak_kib_per_s: usize,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut cfg = Config {
        step: Duration::from_millis(20),
        ..Config::default()
    };
    let mut run = Vec::new();
    let mut theater = None;
    let mut leak_kib_per_s = 0;
    let mut backup_interval = None;
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
            "--theater" => {
                theater = Some(value(i)?);
                i += 2;
            }
            "--leak-kib-per-s" => {
                leak_kib_per_s = value(i)?
                    .parse()
                    .map_err(|e| format!("--leak-kib-per-s: {e}"))?;
                i += 2;
            }
            // Seconds where sidecar.toml has hours, so a rehearsal of
            // minutes sees the purge, the expiry and scheduled backups.
            "--removed-keep-s" | "--dead-keep-s" | "--backup-interval-s" => {
                let s: u64 = value(i)?.parse().map_err(|e| format!("{}: {e}", args[i]))?;
                let d = Duration::from_secs(s);
                match args[i].as_str() {
                    "--removed-keep-s" => cfg.removed_keep = d,
                    "--dead-keep-s" => cfg.dead_keep = d,
                    _ => backup_interval = Some(d),
                }
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
    if let Some(d) = backup_interval {
        let Some(store) = cfg.store.as_mut() else {
            return Err("--backup-interval-s needs --store".into());
        };
        store.backup_interval = d;
    }
    Ok(Args {
        cfg,
        run,
        theater,
        leak_kib_per_s,
    })
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
    // Each process is a DCS boot of its own, as a real agent's is: a
    // sidecar started again over a store must not take it for the boot
    // the store last saw.
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x1000, |d| u64::try_from(d.as_millis()).unwrap_or(0x1000));
    let mut sim = FakeSim::new(nanoprobe_sim_fake::Config {
        first_boot: boot,
        ..fake_config()
    });
    if let Some(t) = &a.theater {
        sim.load_mission_on(t);
    }
    let server = match start_with(a.cfg, sim).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nanoprobe-fake-sidecar: {e}");
            return ExitCode::from(1);
        }
    };
    println!("listening on {}", server.endpoint());
    if a.leak_kib_per_s > 0 {
        // A planted leak: each block is written, so it is resident, and is
        // never freed.
        let kib = a.leak_kib_per_s;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                std::mem::forget(vec![0xA5u8; kib * 1024]);
            }
        });
    }
    // The lives the fake DCS holds, for a test that reads them from the log
    // (`cargo xtask persistence`): one line when they change.
    let hub = std::sync::Arc::clone(&server.hub);
    tokio::spawn(async move {
        let mut last: Vec<(u64, f64)> = Vec::new();
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut now = {
                let mut g = hub.lock();
                let Some(any) = g.backend.as_any() else {
                    continue;
                };
                let Some(sim) = any.downcast_mut::<FakeSim>() else {
                    continue;
                };
                sim.lives()
            };
            now.sort_by_key(|l| l.0);
            if now != last {
                let lives: Vec<String> = now.iter().map(|(e, p)| format!("\"{e}\":{p}")).collect();
                println!(
                    "{{\"ev\":\"fake_lives\",\"lives\":{{{}}}}}",
                    lives.join(",")
                );
                last = now;
            }
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Args, String> {
        parse(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn the_keep_windows_and_the_backup_interval_are_set_in_seconds() {
        let a = args(&[
            "--store",
            "d",
            "--removed-keep-s",
            "0",
            "--dead-keep-s",
            "7",
            "--backup-interval-s",
            "5",
        ])
        .expect("args");
        assert_eq!(a.cfg.removed_keep, Duration::ZERO);
        assert_eq!(a.cfg.dead_keep, Duration::from_secs(7));
        assert_eq!(
            a.cfg.store.expect("store").backup_interval,
            Duration::from_secs(5)
        );
        assert!(
            args(&["--backup-interval-s", "5"]).is_err(),
            "needs --store"
        );
        assert!(args(&["--dead-keep-s", "x"]).is_err());
    }
}
