//! `fake-agent --map PATH --port N --token-file PATH [--fps N]
//! [--out-kib N] [--in-kib N] [--event-queue-kib N] [--control-queue N]
//! [--misbehave NAME]...`

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use nanoprobe_fake_agent::agent::{Agent, Misbehavior, Options};
use nanoprobe_fake_agent::{arg, args_all, read_token};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let need = |name: &str| arg(&args, name).ok_or(format!("{name} is required"));
    let run = || -> Result<(), String> {
        let token =
            read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
        let port = need("--port")?
            .parse()
            .map_err(|_| "--port takes a number")?;
        let mut o = Options::new(need("--map")?.into(), port, token);
        let num = |name: &str, default: u64| -> Result<u64, String> {
            arg(&args, name).map_or(Ok(default), |v| {
                v.parse().map_err(|_| format!("{name} takes a number"))
            })
        };
        o.fps = num("--fps", o.fps.into())? as u32;
        o.out_capacity = num("--out-kib", o.out_capacity >> 10)? << 10;
        o.in_capacity = num("--in-kib", o.in_capacity >> 10)? << 10;
        o.event_queue = (num("--event-queue-kib", (o.event_queue >> 10) as u64)? << 10) as usize;
        o.control_queue = num("--control-queue", o.control_queue as u64)? as usize;
        for m in args_all(&args, "--misbehave") {
            o.misbehave
                .push(Misbehavior::parse(&m).ok_or(format!("unknown misbehavior {m}"))?);
        }
        let mut agent = Agent::open(o).map_err(|e| format!("open: {e}"))?;
        eprintln!("fake-agent: boot {:x}", agent.boot());
        agent.run(&AtomicBool::new(false), |l| eprintln!("fake-agent: {l}"));
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fake-agent: {e}");
            ExitCode::from(2)
        }
    }
}
