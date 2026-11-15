//! `conformance-probe --map PATH --port N --token-file PATH
//! [--handshake-host IP] [--unit-type T] [--spawn X,Z] [--mission PATH]
//! [--frame-ms N] [--patience N] [--seconds N]`
//!
//! The conformance suite ([`nanoprobe_sidecar::conformance`]) through
//! [`RealSim`], the product's adapter, against an agent in DCS or in the
//! stock-Lua mock (step S17). Test tooling only.
//!
//! A frame of the suite is `--frame-ms` (20) of wall time. The suite's
//! mission load is a `MISSION_LOAD` of `--mission`, the running mission's
//! file as DCS opens it; without it the check fails, since nothing else can
//! load a mission. A reattach is a new link session, which the agent sees
//! as a new sidecar boot.
//!
//! It prints one JSON object per line: a `check` line for each check, then
//! `{"ev":"verdict","pass":B,...}`, and exits 0 on a pass. It gives up after
//! `--seconds` (600) with an `error` line.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::{arg, json_string, read_token};
use nanoprobe_sidecar::conformance::{self, Target};
use nanoprobe_sidecar::link::Config;
use nanoprobe_sidecar::sim::real::{Limits, RealSim};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};

struct Real {
    sim: RealSim,
    frame: Duration,
    patience: usize,
    unit_type: String,
    spawn: (f64, f64),
    mission: Option<String>,
    deadline: Instant,
}

impl SimAdapter for Real {
    fn position_after(&self, opcode: u16, payload: &[u8]) -> u64 {
        self.sim.position_after(opcode, payload)
    }
    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached> {
        self.sim.send(opcode, payload)
    }
    fn read(&mut self) -> Vec<RawRecord> {
        self.sim.read()
    }
    fn flush(&mut self) {
        self.sim.flush();
    }
}

impl Target for Real {
    fn advance(&mut self) {
        if Instant::now() > self.deadline {
            println!("{{\"ev\":\"error\",\"error\":\"past --seconds\"}}");
            std::process::exit(1);
        }
        std::thread::sleep(self.frame);
        // The suite keeps nothing on disk: what it read is stored once read.
        self.sim.flush();
    }

    fn reload(&mut self) {}

    fn mission_path(&self) -> Option<String> {
        self.mission.clone()
    }

    fn reattach(&mut self) {
        self.sim.attach();
    }

    fn unit_type(&self) -> &str {
        &self.unit_type
    }

    fn spawn_point(&self) -> (f64, f64) {
        self.spawn
    }

    fn patience(&self) -> usize {
        self.patience
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            println!("{{\"ev\":\"error\",\"error\":{}}}", json_string(&e));
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let need = |n: &str| arg(&args, n).ok_or_else(|| format!("{n} is required"));
    let num = |n: &str, d: u64| -> Result<u64, String> {
        arg(&args, n).map_or(Ok(d), |v| v.parse().map_err(|e| format!("{n}: {e}")))
    };
    let host: IpAddr = arg(&args, "--handshake-host")
        .map_or(Ok(IpAddr::V4(Ipv4Addr::LOCALHOST)), |v| v.parse())
        .map_err(|e| format!("--handshake-host: {e}"))?;
    let port = u16::try_from(num("--port", 0)?).map_err(|e| format!("--port: {e}"))?;
    if port == 0 {
        return Err("--port is required".into());
    }
    let token_file = need("--token-file")?;
    let token = read_token(token_file.as_ref()).map_err(|e| format!("{token_file}: {e}"))?;
    let spawn = match arg(&args, "--spawn") {
        Some(v) => {
            let (x, z) = v.split_once(',').ok_or("--spawn is X,Z")?;
            (
                x.trim().parse().map_err(|e| format!("--spawn: {e}"))?,
                z.trim().parse().map_err(|e| format!("--spawn: {e}"))?,
            )
        }
        None => (-290_000.0, 680_000.0),
    };
    let cfg = Config {
        map_path: need("--map")?.into(),
        handshake: SocketAddr::new(host, port),
        token,
        poll: Duration::from_millis(2),
        handshake_timeout: Duration::from_secs(15),
    };
    let mut sim = RealSim::new(cfg, Limits::default());
    sim.attach();
    let mut t = Real {
        sim,
        frame: Duration::from_millis(num("--frame-ms", 20)?.max(1)),
        patience: usize::try_from(num("--patience", 500)?).unwrap_or(500),
        unit_type: arg(&args, "--unit-type").unwrap_or_else(|| "M-113".into()),
        spawn,
        mission: arg(&args, "--mission"),
        deadline: Instant::now() + Duration::from_secs(num("--seconds", 600)?),
    };
    let started = Instant::now();
    let checks = conformance::run(&mut t);
    let mut pass = true;
    for c in &checks {
        let (ok, error) = match &c.result {
            Ok(()) => (true, String::new()),
            Err(e) => (false, e.clone()),
        };
        pass &= ok;
        println!(
            "{{\"ev\":\"check\",\"name\":{},\"ok\":{ok},\"error\":{}}}",
            json_string(c.name),
            json_string(&error)
        );
    }
    let passed = checks.iter().filter(|c| c.result.is_ok()).count();
    println!(
        "{{\"ev\":\"verdict\",\"pass\":{pass},\"checks\":{},\"passed\":{passed},\"mispredicted\":{},\"seconds\":{:.1}}}",
        checks.len(),
        t.sim.mispredicted(),
        started.elapsed().as_secs_f64()
    );
    Ok(pass && checks.len() > 1)
}
