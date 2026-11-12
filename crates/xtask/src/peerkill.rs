//! `cargo xtask peerkill`: the S6 exit test. The fake agent and the stub
//! sidecar run as two processes; each cycle kills one of them with SIGKILL
//! (TerminateProcess on Windows), starts it again, and times how long until
//! the two are attached again. The stub's journals are then verified.
//!
//! Pass: every reconnect under 2 s, the verifier finds nothing, and the
//! queues and rings never held more than their caps.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::content::mix;
use nanoprobe_fake_agent::verify;
use nanoprobe_transport::layout::{flags, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;

/// The limit on one reconnect (spec/transport.md, "Detaching").
const LIMIT: Duration = Duration::from_secs(2);
const OUT_KIB: u64 = 256;
const IN_KIB: u64 = 256;
// Small, so that events are dropped while a peer is down and the GAP
// accounting is exercised.
const EVENT_QUEUE_KIB: u64 = 2;
const CONTROL_QUEUE: u64 = 64;

struct Options {
    cycles: u64,
    seed: u64,
    keep: Option<PathBuf>,
    fps: u32,
}

fn options(rest: &[&str]) -> Result<Options, String> {
    let mut o = Options {
        cycles: 1000,
        seed: nanoprobe_transport::random_nonzero(),
        keep: None,
        fps: 100,
    };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("{a} takes a value"));
        match *a {
            "--cycles" => o.cycles = value()?.parse().map_err(|_| "--cycles takes a number")?,
            "--seed" => {
                o.seed = u64::from_str_radix(value()?, 16).map_err(|_| "--seed takes hex")?
            }
            "--fps" => o.fps = value()?.parse().map_err(|_| "--fps takes a number")?,
            "--keep" => o.keep = Some(value()?.into()),
            other => return Err(format!("peerkill: unknown option {other}")),
        }
    }
    Ok(o)
}

struct Peers {
    bin: PathBuf,
    dir: PathBuf,
    port: u16,
    fps: u32,
    agent: Option<Child>,
    stub: Option<Child>,
}

impl Peers {
    fn common(&self, cmd: &mut Command) {
        cmd.arg("--map")
            .arg(self.dir.join("map"))
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--token-file")
            .arg(self.dir.join("token"))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
    }

    fn log(&self, name: &str) -> Result<std::fs::File, String> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(name))
            .map_err(|e| format!("{name}: {e}"))
    }

    fn start_agent(&mut self) -> Result<(), String> {
        let mut cmd = Command::new(self.bin.join(exe("fake-agent")));
        self.common(&mut cmd);
        cmd.args(["--fps", &self.fps.to_string()])
            .args([
                "--out-kib",
                &OUT_KIB.to_string(),
                "--in-kib",
                &IN_KIB.to_string(),
            ])
            .args(["--event-queue-kib", &EVENT_QUEUE_KIB.to_string()])
            .args(["--control-queue", &CONTROL_QUEUE.to_string()])
            .stderr(self.log("agent.log")?);
        self.agent = Some(
            cmd.spawn()
                .map_err(|e| format!("start the fake agent: {e}"))?,
        );
        Ok(())
    }

    fn start_stub(&mut self) -> Result<(), String> {
        let mut cmd = Command::new(self.bin.join(exe("stub-sidecar")));
        self.common(&mut cmd);
        cmd.arg("--dir")
            .arg(self.dir.join("journal"))
            .stderr(self.log("stub.log")?);
        self.stub = Some(cmd.spawn().map_err(|e| format!("start the stub: {e}"))?);
        Ok(())
    }

    fn kill(child: &mut Option<Child>) {
        if let Some(mut c) = child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// An error when a peer exited by itself.
    fn alive(&mut self) -> Result<(), String> {
        for (name, c) in [("fake agent", &mut self.agent), ("stub", &mut self.stub)] {
            if let Some(c) = c
                && let Ok(Some(code)) = c.try_wait()
            {
                return Err(format!("the {name} exited by itself: {code}"));
            }
        }
        Ok(())
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        Peers::kill(&mut self.agent);
        Peers::kill(&mut self.stub);
    }
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The fields the harness watches, read tail before head so that a fill it
/// computes is one the ring had.
#[derive(Clone, Copy, Debug, Default)]
struct View {
    agent_boot: u64,
    sidecar_boot: u64,
    seen: u64,
    agent_boot_seen: u64,
    attached: bool,
    out_fill: u64,
    in_fill: u64,
    event_bytes: u64,
    control: u64,
}

fn view(m: &MappedFile) -> View {
    let out_tail = m.load(s::OUT_TAIL, Ordering::Acquire);
    let out_head = m.load(s::OUT_HEAD, Ordering::Acquire);
    let in_tail = m.load(s::IN_TAIL, Ordering::Acquire);
    let in_head = m.load(s::IN_HEAD, Ordering::Acquire);
    View {
        agent_boot: m.load(s::AGENT_BOOT, Ordering::Acquire),
        sidecar_boot: m.load(s::SIDECAR_BOOT, Ordering::Acquire),
        seen: m.load(s::SIDECAR_BOOT_SEEN, Ordering::Acquire),
        agent_boot_seen: m.load(s::AGENT_BOOT_SEEN, Ordering::Acquire),
        attached: u64::from(m.load32(s::FLAGS, Ordering::Acquire)) & flags::ATTACHED != 0,
        out_fill: out_tail.saturating_sub(out_head),
        in_fill: in_tail.saturating_sub(in_head),
        event_bytes: m.load(s::EVENT_QUEUE_BYTES, Ordering::Relaxed),
        control: u64::from(m.load32(s::CONTROL_QUEUED, Ordering::Relaxed)),
    }
}

#[derive(Default)]
struct Bounds {
    out_fill: u64,
    in_fill: u64,
    event_bytes: u64,
    control: u64,
}

impl Bounds {
    fn note(&mut self, v: &View) {
        // Between a restart and the next attach, one side's position is from
        // another boot; a fill is a fill only while both name the same one.
        if attached_pair(v) && v.agent_boot_seen == v.agent_boot {
            self.out_fill = self.out_fill.max(v.out_fill);
            self.in_fill = self.in_fill.max(v.in_fill);
        }
        self.event_bytes = self.event_bytes.max(v.event_bytes);
        self.control = self.control.max(v.control);
    }

    fn broken(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (name, got, cap) in [
            ("outbound ring", self.out_fill, OUT_KIB << 10),
            ("inbound ring", self.in_fill, IN_KIB << 10),
            ("event queue", self.event_bytes, EVENT_QUEUE_KIB << 10),
            ("control queue", self.control, CONTROL_QUEUE),
        ] {
            if got > cap {
                out.push(format!("the {name} held {got}, over its cap of {cap}"));
            }
        }
        out
    }
}

/// Watch the file until `done` holds, noting bounds. `None` after `limit`.
fn wait(
    m: &MappedFile,
    peers: &mut Peers,
    bounds: &mut Bounds,
    limit: Duration,
    done: impl Fn(&View) -> bool,
) -> Result<Option<Duration>, String> {
    let start = Instant::now();
    loop {
        let v = view(m);
        bounds.note(&v);
        if done(&v) {
            return Ok(Some(start.elapsed()));
        }
        if start.elapsed() > limit {
            return Ok(None);
        }
        peers.alive()?;
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn attached_pair(v: &View) -> bool {
    v.attached && v.seen != 0 && v.seen == v.sidecar_boot
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Run the exit test, or with `--verify DIR` only verify the journals a
/// kept run left in DIR.
pub fn command(repository: &Path, rest: &[&str]) -> Result<ExitCode, String> {
    if let ["--verify", dir] = rest {
        let dir = Path::new(dir);
        let r = verify::verify(&dir.join("journal")).map_err(|e| format!("verify: {e}"))?;
        // Each stub start opens a journal: the first, then one per sidecar kill.
        let journals = std::fs::read_dir(dir.join("journal"))
            .map_err(|e| format!("journal: {e}"))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("journal-"))
            .count();
        let mut failures = r.errors.clone();
        failures.extend(log_failures(dir, journals.saturating_sub(1)));
        let run = std::fs::read_to_string(dir.join("run.txt")).unwrap_or_default();
        let field = |name: &str| {
            run.lines()
                .find_map(|l| l.strip_prefix(name)?.trim().parse::<u64>().ok())
        };
        match (field("cycles "), field("agent_kills ")) {
            (Some(cycles), Some(agent_kills)) => {
                failures.extend(gates(&r, cycles));
                failures.extend(boot_gate(&r, agent_kills));
            }
            _ => {
                println!("no run.txt: the attach and boot counts are not checked");
                failures.extend(gates(&r, r.attaches.saturating_sub(1)));
            }
        }
        println!(
            "records {} ({} padding), agent boots {}, attaches {}, commands taken {}, results {}",
            r.records, r.padding, r.boots, r.attaches, r.taken, r.results
        );
        for e in failures.iter().take(50) {
            println!("FAIL {e}");
        }
        return Ok(if failures.is_empty() {
            println!(
                "peerkill: journals and logs pass; reconnect times, ring and queue bounds and resident memory are not in them"
            );
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let o = options(rest)?;
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "nanoprobe-fake-agent", "--bins"])
        .current_dir(repository)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".into());
    }
    let dir = match &o.keep {
        Some(d) => d.clone(),
        None => std::env::temp_dir().join(format!("nanoprobe-peerkill-{}", std::process::id())),
    };
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let token: String = (0..4)
        .map(|i| format!("{:016x}", mix(o.seed ^ i)))
        .collect();
    std::fs::write(dir.join("token"), format!("{token}\n")).map_err(|e| e.to_string())?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("pick a port: {e}"))?
        .port();
    let mut peers = Peers {
        bin: std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| repository.join("target"))
            .join("release"),
        dir: dir.clone(),
        port,
        fps: o.fps,
        agent: None,
        stub: None,
    };
    println!(
        "peerkill: {} cycles, seed {:x}, files in {}",
        o.cycles,
        o.seed,
        dir.display()
    );
    peers.start_agent()?;
    let map_path = dir.join("map");
    let start = Instant::now();
    while !map_path.exists() || std::fs::metadata(&map_path).map(|m| m.len()).unwrap_or(0) < 4096 {
        if start.elapsed() > Duration::from_secs(5) {
            return Err("the fake agent wrote no file".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let map = MappedFile::open_read_only(&map_path).map_err(|e| format!("map: {e}"))?;
    peers.start_stub()?;
    let mut bounds = Bounds::default();
    if wait(
        &map,
        &mut peers,
        &mut bounds,
        Duration::from_secs(10),
        attached_pair,
    )?
    .is_none()
    {
        return Err("the peers did not attach at the start".into());
    }

    let mut times: [Vec<Duration>; 2] = [Vec::new(), Vec::new()];
    let mut slow = Vec::new();
    let mut rss = [0u64; 2];
    let mut rng = o.seed;
    for cycle in 0..o.cycles {
        rng = mix(rng);
        // Run attached for 100 to 600 ms, so records and commands are in
        // flight when the kill comes. The pair must stay attached throughout.
        let steady = Duration::from_millis(100 + rng % 500);
        if wait(&map, &mut peers, &mut bounds, steady, |v| !attached_pair(v))?.is_some() {
            slow.push(format!("cycle {cycle}: the pair detached with no kill"));
            if wait(
                &map,
                &mut peers,
                &mut bounds,
                Duration::from_secs(10),
                attached_pair,
            )?
            .is_none()
            {
                return Err(format!(
                    "cycle {cycle}: the pair detached by itself and did not attach again"
                ));
            }
        }
        for (i, c) in [&peers.agent, &peers.stub].into_iter().enumerate() {
            if let Some(kib) = c.as_ref().and_then(|c| rss_kib(c.id())) {
                rss[i] = rss[i].max(kib);
            }
        }
        let victim = (mix(rng) % 2) as usize;
        let before = view(&map);
        let t0 = Instant::now();
        let took = if victim == 0 {
            Peers::kill(&mut peers.agent);
            peers.start_agent()?;
            wait(
                &map,
                &mut peers,
                &mut bounds,
                Duration::from_secs(10),
                // agent_boot_seen moves only when the new sidecar attaches; the
                // new agent's header may still show the old flags.
                |v| {
                    attached_pair(v)
                        && v.agent_boot != before.agent_boot
                        && v.agent_boot_seen == v.agent_boot
                },
            )?
        } else {
            Peers::kill(&mut peers.stub);
            peers.start_stub()?;
            wait(
                &map,
                &mut peers,
                &mut bounds,
                Duration::from_secs(10),
                |v| attached_pair(v) && v.sidecar_boot != before.sidecar_boot,
            )?
        };
        let name = ["agent", "sidecar"][victim];
        let Some(_) = took else {
            return Err(format!(
                "cycle {cycle}: no reconnect within 10 s after killing the {name}"
            ));
        };
        let took = t0.elapsed();
        times[victim].push(took);
        if took >= LIMIT {
            slow.push(format!("cycle {cycle}: {took:?} after killing the {name}"));
        }
        if (cycle + 1) % 50 == 0 {
            println!("peerkill: {} cycles done", cycle + 1);
        }
    }
    wait(
        &map,
        &mut peers,
        &mut bounds,
        Duration::from_millis(500),
        |_| false,
    )?;
    Peers::kill(&mut peers.stub);
    Peers::kill(&mut peers.agent);
    let report = verify::verify(&dir.join("journal")).map_err(|e| format!("verify: {e}"))?;
    let mut failures = slow;
    failures.extend(bounds.broken());
    failures.extend(report.errors.iter().cloned());
    failures.extend(log_failures(&dir, times[1].len()));
    let kills = (times[0].len() as u64, times[1].len() as u64);
    // What `--verify` needs to apply the same gates to the kept files.
    let _ = std::fs::write(
        dir.join("run.txt"),
        format!(
            "cycles {}\nagent_kills {}\nsidecar_kills {}\n",
            o.cycles, kills.0, kills.1
        ),
    );
    failures.extend(boot_gate(&report, kills.0));
    failures.extend(gates(&report, o.cycles));
    for (name, kib) in ["fake agent", "stub"].iter().zip(rss) {
        if kib > RSS_LIMIT_KIB {
            failures.push(format!(
                "the {name} grew to {kib} KiB, over {RSS_LIMIT_KIB} KiB"
            ));
        }
    }

    let mut out = String::new();
    for (i, name) in ["agent", "sidecar"].iter().enumerate() {
        let t = &mut times[i];
        t.sort();
        let _ = writeln!(
            out,
            "kills of the {name}: {}, reconnect p50 {:?} p99 {:?} max {:?}",
            t.len(),
            percentile(t, 0.5),
            percentile(t, 0.99),
            t.last().copied().unwrap_or_default()
        );
    }
    let _ = writeln!(
        out,
        "records {} ({} padding, {} journalled twice, {} read again, {} lines cut by a kill), agent boots {}, \
         attaches {}, events {} ({} accounted by GAP), samples {}, commands sent {} ({} after inbound padding), \
         taken {} ({} unknown after a resync), results {} ({} applied)",
        report.records,
        report.padding,
        report.repeats,
        report.rereads,
        report.cut,
        report.boots,
        report.attaches,
        report.events,
        report.gapped,
        report.samples,
        report.sent,
        report.inbound_wraps,
        report.taken,
        report.unknown,
        report.results,
        report.applied
    );
    let _ = writeln!(
        out,
        "largest resident memory: fake agent {} KiB, stub {} KiB",
        rss[0], rss[1]
    );
    let _ = writeln!(
        out,
        "most held: outbound ring {} of {} B, inbound ring {} of {} B, event queue {} of {} B, control queue {} of {}",
        bounds.out_fill,
        OUT_KIB << 10,
        bounds.in_fill,
        IN_KIB << 10,
        bounds.event_bytes,
        EVENT_QUEUE_KIB << 10,
        bounds.control,
        CONTROL_QUEUE
    );
    print!("{out}");
    for f in failures.iter().take(50) {
        println!("FAIL {f}");
    }
    if failures.len() > 50 {
        println!("FAIL ... {} more", failures.len() - 50);
    }
    if o.keep.is_none() && failures.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(if failures.is_empty() {
        println!("peerkill: pass");
        ExitCode::SUCCESS
    } else {
        println!("peerkill: FAIL ({} problems)", failures.len());
        ExitCode::FAILURE
    })
}

/// The most resident memory either peer may reach. Both hold bounded queues
/// and buffers; growth past this is a leak.
const RSS_LIMIT_KIB: u64 = 64 << 10;

/// A process's resident memory in KiB, from `ps`. `None` where `ps` is not.
fn rss_kib(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Lines in the peers' logs that only a failed check writes, and agent
/// detaches beyond the sidecar kills that explain them.
fn log_failures(dir: &Path, sidecar_kills: usize) -> Vec<String> {
    let mut out = Vec::new();
    let agent = std::fs::read_to_string(dir.join("agent.log")).unwrap_or_default();
    for line in agent.lines() {
        if [
            "refused",
            "inbound ring",
            "outbound ring",
            "dropped a control record",
            "over the payload limit",
            "not written",
        ]
        .iter()
        .any(|w| line.contains(w))
        {
            out.push(format!("agent.log: {line}"));
        }
    }
    let detaches = agent.lines().filter(|l| l.contains("detached:")).count();
    if detaches > sidecar_kills {
        out.push(format!(
            "the agent detached {detaches} times for {sidecar_kills} sidecar kills"
        ));
    }
    out
}

/// What every run must have exercised, and journalled.
fn gates(report: &verify::Report, cycles: u64) -> Vec<String> {
    let mut out = Vec::new();
    if report.attaches < cycles + 1 {
        out.push(format!(
            "{} attaches journalled, for {cycles} cycles",
            report.attaches
        ));
    }
    for (what, n) in [
        ("APPLIED results", report.applied),
        ("events accounted by a GAP", report.gapped),
        ("padding records (outbound ring wraps)", report.padding),
        (
            "commands after padding (inbound ring wraps)",
            report.inbound_wraps,
        ),
        ("commands taken before a HELLO", report.taken),
    ] {
        if n == 0 {
            out.push(format!("no {what}: the run did not exercise them"));
        }
    }
    out
}

/// One agent boot per agent kill, and the first.
fn boot_gate(report: &verify::Report, agent_kills: u64) -> Option<String> {
    (report.boots as u64 != agent_kills + 1).then(|| {
        format!(
            "{} agent boots journalled, for {agent_kills} agent kills",
            report.boots
        )
    })
}
