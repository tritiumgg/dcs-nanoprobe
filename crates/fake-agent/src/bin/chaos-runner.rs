//! `chaos-runner --map PATH --dir DIR --cycles N [--seed HEX]
//! (--dcs-start CMD | --dcs-kill CMD) [--gate PATH] [--dcs-limit S]
//! [--dcs-gap S] -- SIDECAR [ARGS...]`
//!
//! The S8 chaos driver's inner loop (spec/faults.md, "Chaos"). It runs next to
//! DCS: on the host beside the mock, or in the game container. It starts the
//! sidecar (`SIDECAR ARGS`, normally `link-probe`), waits for the pair to
//! attach, then for each of N cycles keeps the pair attached for 0.1 to 2 s
//! and kills one side, chosen at random from the seed:
//!
//! * the sidecar: SIGKILL (TerminateProcess on Windows), then start it again;
//!   the reconnect is timed from the kill to the pair attached with the new
//!   sidecar boot;
//! * DCS: with `--dcs-start`, DCS is the runner's child, killed and started
//!   again (on Unix the command runs in its own process group, and the kill
//!   reaches the whole group); with `--dcs-kill`, the command kills it and DCS comes back by
//!   itself (the host restarts it). The reconnect is timed from the new
//!   agent's first frame to the pair attached with its boot: an agent can
//!   attach only on a frame, and none comes before a mission runs. The first
//!   frame is the first change of the frame number once the status block
//!   shows a loaded mission, generation 1 or later, running or paused. When the agent cannot listen yet,
//!   because the port the killed DCS held is still taken, the reconnect is
//!   timed from when it listens, and the wait is printed as a `listen` line. With
//!   `--gate`, the runner asks before each DCS kill (an `ask` line) and waits
//!   until the file PATH exists, then removes it. With `--dcs-gap`, DCS
//!   runs at least S seconds between its reconnect and the next DCS kill, so
//!   a host watchdog that checks once a minute sees it up.
//!
//! It reads the status block every millisecond. It prints one JSON object per
//! line, each with `t`, wall milliseconds since the Unix epoch:
//!
//! ```text
//! {"t":..,"ev":"start","seed":"<hex>","cycles":N}
//! {"t":..,"ev":"attached","agent_boot":"<hex>","sidecar_boot":"<hex>"}
//! {"t":..,"ev":"ask","cycle":N}
//! {"t":..,"ev":"kill","cycle":N,"victim":"dcs"|"sidecar"}
//! {"t":..,"ev":"pump","cycle":N,"agent_boot":"<hex>","after_kill_ms":N}
//! {"t":..,"ev":"listen","cycle":N,"wait_ms":N}
//! {"t":..,"ev":"reconnect","cycle":N,"victim":"..","ms":N}
//! {"t":..,"ev":"fail","cycle":N,"text":".."}
//! {"t":..,"ev":"report","sidecar":N,"text":".."}
//! {"t":..,"ev":"end","cycles":N,"dcs_kills":N,"sidecar_kills":N,"dcs_max_ms":N,"sidecar_max_ms":N,"listen_wait_max_ms":N,"failures":N}
//! ```
//!
//! A cycle fails when the reconnect takes 2 s or more; when the pair detaches,
//! DCS stops pumping for 3 s, or a new agent boots, with no kill; when one
//! DCS kill is followed by more than one new agent boot; or when a sidecar
//! reports anything but an end a kill explains (the list the S6 journal
//! verifier uses, `verify::EXPECTED_ENDS`). Each sidecar's output is kept as
//! `DIR/sidecar-N.jsonl`, and with `--dcs-start` each DCS's as `DIR/dcs-N.log`.
//!
//! Test tooling: built from source, never released.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::content::{mix, now_ms};
use nanoprobe_fake_agent::verify::EXPECTED_ENDS;
use nanoprobe_fake_agent::{arg, json_string};
use nanoprobe_transport::layout::{flags, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;

/// The pass limit on one reconnect.
const LIMIT: Duration = Duration::from_secs(2);
/// How long a new agent may wait for its port after its first frame.
const LISTEN_LIMIT: Duration = Duration::from_secs(120);
/// How long a reconnect may take before the runner stops waiting.
const GIVE_UP: Duration = Duration::from_secs(10);
/// How long the runner waits for an answer to an ask. Longer than the
/// asker's own wait for an empty server.
const GATE_LIMIT: Duration = Duration::from_secs(45 * 60);
/// DCS that has not advanced its frame for this long is not running.
const STALL: Duration = Duration::from_secs(3);
/// Sidecar reports that are not session ends and say nothing is wrong.
const BENIGN: &[&str] = &["a HELLO of an earlier attach"];

/// Whether a sidecar report is a failure: anything but an end a kill or a
/// restart explains.
fn bad_report(text: &str) -> bool {
    !EXPECTED_ENDS
        .iter()
        .chain(BENIGN)
        .any(|e| text.starts_with(e))
}

enum Field {
    Num(u64),
    Str(String),
}

fn line(ev: &str, fields: &[(&str, Field)]) {
    let mut text = format!("{{\"t\":{},\"ev\":\"{ev}\"", now_ms());
    for (k, v) in fields {
        text.push_str(&format!(",\"{k}\":"));
        match v {
            Field::Num(n) => text.push_str(&n.to_string()),
            Field::Str(s) => text.push_str(&json_string(s)),
        }
    }
    text.push('}');
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn hex(v: u64) -> Field {
    Field::Str(format!("{v:016x}"))
}

/// How DCS is killed and comes back.
enum Dcs {
    /// A child of the runner, from this shell command.
    Child { cmd: String, child: Option<Child> },
    /// Killed by this shell command; it comes back by itself.
    External { kill: String },
}

struct Options {
    map: PathBuf,
    dir: PathBuf,
    cycles: u64,
    seed: u64,
    gate: Option<PathBuf>,
    dcs_limit: Duration,
    dcs_gap: Duration,
    sidecar: Vec<String>,
}

struct Runner {
    o: Options,
    dcs: Dcs,
    sidecar: Option<Child>,
    sidecars: u64,
    dcs_starts: u64,
    failures: u64,
    /// The last frame number and agent boot read, and when either changed.
    last: (u64, u64),
    changed: Instant,
    /// Every agent boot seen ready, in order.
    boots: Vec<u64>,
}

/// The fields the runner watches.
#[derive(Clone, Copy, Debug, Default)]
struct View {
    ready: bool,
    agent_boot: u64,
    frame: u64,
    sidecar_boot: u64,
    seen: u64,
    agent_boot_seen: u64,
    attached: bool,
    /// The frame has not advanced for [`STALL`]; set by the runner.
    stalled: bool,
    /// The `lifecycle` code.
    lifecycle: u32,
    /// The mission generation: 0 before the first mission load.
    mission_gen: u32,
    /// The agent's handshake socket is open: `NO_HANDSHAKE` is clear.
    listening: bool,
}

impl View {
    /// A snapshot, as the sidecar reads the status block (spec/transport.md,
    /// "Detaching"): `ready` and `agent_boot` before and after the fields, the
    /// same both times and `ready` 1. A read that overlaps an agent's Start is
    /// taken again; one that never settles reads as not ready.
    fn read(m: &MappedFile) -> View {
        for _ in 0..100 {
            let ready = m.load(s::READY, Ordering::Acquire);
            let boot = m.load(s::AGENT_BOOT, Ordering::Acquire);
            let v = View {
                ready: ready == 1,
                agent_boot: boot,
                frame: m.load(s::FRAME, Ordering::Acquire),
                lifecycle: m.load32(s::LIFECYCLE, Ordering::Acquire),
                mission_gen: m.load32(s::MISSION_GEN, Ordering::Acquire),
                sidecar_boot: m.load(s::SIDECAR_BOOT, Ordering::Acquire),
                seen: m.load(s::SIDECAR_BOOT_SEEN, Ordering::Acquire),
                agent_boot_seen: m.load(s::AGENT_BOOT_SEEN, Ordering::Acquire),
                attached: u64::from(m.load32(s::FLAGS, Ordering::Acquire)) & flags::ATTACHED != 0,
                listening: u64::from(m.load32(s::FLAGS, Ordering::Acquire)) & flags::NO_HANDSHAKE
                    == 0,
                stalled: false,
            };
            m.fence(Ordering::Acquire);
            if m.load(s::READY, Ordering::Acquire) == ready
                && m.load(s::AGENT_BOOT, Ordering::Acquire) == boot
            {
                return v;
            }
        }
        View::default()
    }

    /// The agent and a sidecar are attached to each other, and DCS pumps.
    fn pair(&self) -> bool {
        self.ready
            && !self.stalled
            && self.attached
            && self.seen != 0
            && self.seen == self.sidecar_boot
            && self.agent_boot_seen == self.agent_boot
    }
}

/// `RUNNING` or `PAUSED`: a mission runs (spec/transport.md, "Codes:
/// lifecycle").
fn running(lifecycle: u32) -> bool {
    lifecycle == 3 || lifecycle == 4
}

fn shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", cmd]);
    c
}

fn kill(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Kill DCS and every process its command started. On Unix the command runs
/// in its own process group (`start_dcs`), so a program it left running in
/// the background dies with it and holds no inherited file past the run.
fn kill_group(child: &mut Option<Child>) {
    #[cfg(unix)]
    if let Some(c) = child {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", c.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    kill(child);
}

impl Runner {
    fn fail(&mut self, cycle: u64, text: &str) {
        self.failures += 1;
        line(
            "fail",
            &[
                ("cycle", Field::Num(cycle)),
                ("text", Field::Str(text.into())),
            ],
        );
    }

    fn file(&self, name: &str) -> Result<std::fs::File, String> {
        std::fs::File::create(self.o.dir.join(name)).map_err(|e| format!("{name}: {e}"))
    }

    fn start_sidecar(&mut self) -> Result<(), String> {
        self.sidecars += 1;
        let n = self.sidecars;
        let (program, args) = self.o.sidecar.split_first().ok_or("no sidecar command")?;
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(self.file(&format!("sidecar-{n}.jsonl"))?)
            .stderr(self.file(&format!("sidecar-{n}.err"))?)
            .spawn()
            .map_err(|e| format!("start the sidecar: {e}"))?;
        self.sidecar = Some(child);
        Ok(())
    }

    /// Start DCS when it is the runner's child; otherwise it comes back by
    /// itself.
    fn start_dcs(&mut self) -> Result<(), String> {
        if let Dcs::Child { .. } = self.dcs {
            self.dcs_starts += 1;
        } else {
            return Ok(());
        }
        let n = self.dcs_starts;
        let log = self.file(&format!("dcs-{n}.log"))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        if let Dcs::Child { cmd, child } = &mut self.dcs {
            // exec, so the kill reaches the program and not only the shell.
            let mut c = shell(&format!("exec {cmd}"));
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut c, 0);
            let started = c
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(err)
                .spawn()
                .map_err(|e| format!("start DCS: {e}"))?;
            *child = Some(started);
        }
        Ok(())
    }

    fn kill_dcs(&mut self) -> Result<(), String> {
        match &mut self.dcs {
            Dcs::Child { child, .. } => {
                kill_group(child);
                Ok(())
            }
            Dcs::External { kill } => {
                let status = shell(kill)
                    .stdin(Stdio::null())
                    .status()
                    .map_err(|e| format!("kill DCS: {e}"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("the DCS kill command failed: {status}"))
                }
            }
        }
    }

    /// An error when a child exited by itself.
    fn alive(&mut self) -> Result<(), String> {
        if let Some(c) = &mut self.sidecar
            && let Ok(Some(code)) = c.try_wait()
        {
            return Err(format!("the sidecar exited by itself: {code}"));
        }
        if let Dcs::Child { child: Some(c), .. } = &mut self.dcs
            && let Ok(Some(code)) = c.try_wait()
        {
            return Err(format!("DCS exited by itself: {code}"));
        }
        Ok(())
    }

    /// Map the file once the agent made it and stored `ready`.
    fn open_map(&mut self) -> Result<MappedFile, String> {
        let start = Instant::now();
        loop {
            if let Ok(m) = MappedFile::open_read_only(&self.o.map)
                && m.load(s::READY, Ordering::Acquire) == 1
            {
                return Ok(m);
            }
            if start.elapsed() > self.o.dcs_limit {
                return Err(format!(
                    "no ready file at {} within {:?}",
                    self.o.map.display(),
                    self.o.dcs_limit
                ));
            }
            self.alive()?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Read the file, note each new agent boot, and mark the view stalled
    /// when the frame has not advanced for [`STALL`].
    fn observe(&mut self, m: &MappedFile) -> View {
        let mut v = View::read(m);
        if (v.frame, v.agent_boot) != self.last {
            self.last = (v.frame, v.agent_boot);
            self.changed = Instant::now();
        }
        v.stalled = self.changed.elapsed() > STALL;
        if v.ready && v.agent_boot != 0 && self.boots.last() != Some(&v.agent_boot) {
            self.boots.push(v.agent_boot);
        }
        v
    }

    /// Read the file every millisecond until `done` holds. `None` after
    /// `limit`.
    fn wait(
        &mut self,
        m: &MappedFile,
        limit: Duration,
        done: impl Fn(&View) -> bool,
    ) -> Result<Option<View>, String> {
        let start = Instant::now();
        loop {
            let v = self.observe(m);
            if done(&v) {
                return Ok(Some(v));
            }
            if start.elapsed() > limit {
                return Ok(None);
            }
            self.alive()?;
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Wait for the pair to break, for `limit`. A break is a failure, and
    /// the runner waits for the pair to attach again.
    fn hold(&mut self, m: &MappedFile, cycle: u64, limit: Duration) -> Result<(), String> {
        let boot = self.observe(m).agent_boot;
        let boots = self.boots.len();
        let Some(v) = self.wait(m, limit, |v| !v.pair())? else {
            return Ok(());
        };
        // Why the pair broke shows once it attaches again: a new boot is a
        // restart, whatever the first broken read showed.
        let again = self.wait(m, self.o.dcs_limit, View::pair)?;
        let restarted = self.boots.len() > boots || again.is_some_and(|a| a.agent_boot != boot);
        self.fail(
            cycle,
            if restarted {
                "DCS restarted with no kill"
            } else if v.stalled {
                "DCS stopped pumping with no kill"
            } else {
                "the pair detached with no kill"
            },
        );
        match again {
            Some(_) => Ok(()),
            None => Err(format!("cycle {cycle}: the pair did not attach again")),
        }
    }

    /// Wait for the gate file, then remove it, holding the pair meanwhile.
    fn ask(&mut self, m: &MappedFile, cycle: u64) -> Result<(), String> {
        let Some(gate) = self.o.gate.clone() else {
            return Ok(());
        };
        line("ask", &[("cycle", Field::Num(cycle))]);
        let asked = Instant::now();
        while !gate.exists() {
            if asked.elapsed() > GATE_LIMIT {
                return Err(format!(
                    "cycle {cycle}: no answer to the ask in {GATE_LIMIT:?}"
                ));
            }
            self.hold(m, cycle, Duration::from_millis(200))?;
        }
        std::fs::remove_file(&gate).map_err(|e| format!("remove the gate: {e}"))
    }

    fn run(&mut self) -> Result<(), String> {
        line(
            "start",
            &[
                ("seed", hex(self.o.seed)),
                ("cycles", Field::Num(self.o.cycles)),
            ],
        );
        self.start_dcs()?;
        let m = self.open_map()?;
        // The file may still show an attach from before this run; only this
        // run's own sidecar counts.
        let stale = View::read(&m).sidecar_boot;
        self.start_sidecar()?;
        let Some(v) = self.wait(&m, self.o.dcs_limit, |v| {
            v.pair() && v.sidecar_boot != stale
        })?
        else {
            return Err("the pair did not attach at the start".into());
        };
        line(
            "attached",
            &[
                ("agent_boot", hex(v.agent_boot)),
                ("sidecar_boot", hex(v.sidecar_boot)),
            ],
        );
        let mut rng = self.o.seed;
        let mut dcs_up = Instant::now();
        let mut max = [Duration::ZERO; 2];
        let mut listen_max = Duration::ZERO;
        let mut kills = [0u64; 2];
        for cycle in 0..self.o.cycles {
            rng = mix(rng);
            let steady = Duration::from_millis(100 + rng % 1900);
            self.hold(&m, cycle, steady)?;
            let victim = (mix(rng) % 2) as usize;
            let name = ["dcs", "sidecar"][victim];
            if victim == 0 {
                let rest = self.o.dcs_gap.saturating_sub(dcs_up.elapsed());
                self.hold(&m, cycle, rest)?;
                self.ask(&m, cycle)?;
            }
            let before = self.observe(&m);
            let boots = self.boots.len();
            line(
                "kill",
                &[
                    ("cycle", Field::Num(cycle)),
                    ("victim", Field::Str(name.into())),
                ],
            );
            let killed = Instant::now();
            let took = if victim == 0 {
                self.kill_dcs()?;
                self.start_dcs()?;
                // The first frame: a new agent with a loaded mission running
                // or paused, then its frame number moving. DCS runs a few
                // frames before it loads its first mission, at generation 0,
                // and none during the load that follows (LIVE, 2026-11-13).
                let Some(v) = self.wait(&m, self.o.dcs_limit, |v| {
                    v.ready
                        && v.agent_boot != before.agent_boot
                        && v.mission_gen >= 1
                        && running(v.lifecycle)
                })?
                else {
                    return Err(format!(
                        "cycle {cycle}: no new agent ran a mission within {:?} of the kill",
                        self.o.dcs_limit
                    ));
                };
                let (boot, frame) = (v.agent_boot, v.frame);
                let Some(v) = self.wait(&m, self.o.dcs_limit, |v| {
                    v.agent_boot == boot && v.frame > frame
                })?
                else {
                    return Err(format!(
                        "cycle {cycle}: the new agent ran no frame within {:?}",
                        self.o.dcs_limit
                    ));
                };
                let pumped = Instant::now();
                line(
                    "pump",
                    &[
                        ("cycle", Field::Num(cycle)),
                        ("agent_boot", hex(v.agent_boot)),
                        (
                            "after_kill_ms",
                            Field::Num(killed.elapsed().as_millis() as u64),
                        ),
                    ],
                );
                let boot = v.agent_boot;
                // The port the killed DCS held can stay taken for a while
                // (spec/transport.md, "Handshake"). The agent tries again
                // once a second; the reconnect is timed from when it listens.
                let Some(_) =
                    self.wait(&m, LISTEN_LIMIT, |v| v.agent_boot == boot && v.listening)?
                else {
                    return Err(format!(
                        "cycle {cycle}: the new agent did not listen within {LISTEN_LIMIT:?}"
                    ));
                };
                let wait = pumped.elapsed();
                listen_max = listen_max.max(wait);
                line(
                    "listen",
                    &[
                        ("cycle", Field::Num(cycle)),
                        ("wait_ms", Field::Num(wait.as_millis() as u64)),
                    ],
                );
                let listened = Instant::now();
                let took = self
                    .wait(&m, GIVE_UP, |v| v.pair() && v.agent_boot == boot)?
                    .map(|_| listened.elapsed());
                dcs_up = Instant::now();
                took
            } else {
                kill(&mut self.sidecar);
                self.start_sidecar()?;
                self.wait(&m, GIVE_UP, |v| {
                    v.pair() && v.sidecar_boot != before.sidecar_boot
                })?
                .map(|_| killed.elapsed())
            };
            let Some(took) = took else {
                return Err(format!(
                    "cycle {cycle}: no reconnect within {GIVE_UP:?} after killing {name}"
                ));
            };
            let started = self.boots.len() - boots;
            let expected = usize::from(victim == 0);
            if started != expected {
                self.fail(
                    cycle,
                    &format!("{started} new agent boot(s) after killing {name}, not {expected}"),
                );
            }
            kills[victim] += 1;
            max[victim] = max[victim].max(took);
            line(
                "reconnect",
                &[
                    ("cycle", Field::Num(cycle)),
                    ("victim", Field::Str(name.into())),
                    ("ms", Field::Num(took.as_millis() as u64)),
                ],
            );
            if took >= LIMIT {
                self.fail(
                    cycle,
                    &format!("{took:?} to reconnect after killing {name}"),
                );
            }
        }
        kill(&mut self.sidecar);
        self.reports();
        line(
            "end",
            &[
                ("cycles", Field::Num(self.o.cycles)),
                ("dcs_kills", Field::Num(kills[0])),
                ("sidecar_kills", Field::Num(kills[1])),
                ("dcs_max_ms", Field::Num(max[0].as_millis() as u64)),
                ("sidecar_max_ms", Field::Num(max[1].as_millis() as u64)),
                (
                    "listen_wait_max_ms",
                    Field::Num(listen_max.as_millis() as u64),
                ),
                ("failures", Field::Num(self.failures)),
            ],
        );
        Ok(())
    }

    /// Print every sidecar's report lines; a bad one is a failure.
    fn reports(&mut self) {
        for n in 1..=self.sidecars {
            let path = self.o.dir.join(format!("sidecar-{n}.jsonl"));
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let reports = text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["ev"] == "report");
            for v in reports {
                let what = v["text"].as_str().unwrap_or_default().to_owned();
                line(
                    "report",
                    &[
                        ("sidecar", Field::Num(n)),
                        ("text", Field::Str(what.clone())),
                    ],
                );
                if bad_report(&what) {
                    self.fail(u64::MAX, &format!("sidecar {n} reported: {what}"));
                }
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        kill(&mut self.sidecar);
        if let Dcs::Child { child, .. } = &mut self.dcs {
            kill_group(child);
        }
    }
}

fn options(args: &[String]) -> Result<(Options, Dcs), String> {
    let split = args
        .iter()
        .position(|a| a == "--")
        .ok_or("-- SIDECAR is required")?;
    let (own, sidecar) = (&args[..split], &args[split + 1..]);
    if sidecar.is_empty() {
        return Err("-- SIDECAR is required".into());
    }
    let need = |name: &str| arg(own, name).ok_or(format!("{name} is required"));
    let num = |name: &str, default: u64| {
        arg(own, name).map_or(Ok(default), |v| {
            v.parse().map_err(|_| format!("{name} takes a number"))
        })
    };
    let seed = match arg(own, "--seed") {
        Some(v) => u64::from_str_radix(&v, 16).map_err(|_| "--seed takes hex digits")?,
        None => nanoprobe_transport::random_nonzero(),
    };
    let dcs = match (arg(own, "--dcs-start"), arg(own, "--dcs-kill")) {
        (Some(cmd), None) => Dcs::Child { cmd, child: None },
        (None, Some(kill)) => Dcs::External { kill },
        _ => return Err("give one of --dcs-start and --dcs-kill".into()),
    };
    Ok((
        Options {
            map: need("--map")?.into(),
            dir: need("--dir")?.into(),
            cycles: num("--cycles", 100)?,
            seed,
            gate: arg(own, "--gate").map(PathBuf::from),
            dcs_limit: Duration::from_secs(num("--dcs-limit", 600)?),
            dcs_gap: Duration::from_secs(num("--dcs-gap", 0)?),
            sidecar: sidecar.to_vec(),
        },
        dcs,
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (o, dcs) = match options(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("chaos-runner: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&o.dir) {
        eprintln!("chaos-runner: {}: {e}", o.dir.display());
        return ExitCode::from(2);
    }
    let mut runner = Runner {
        o,
        dcs,
        sidecar: None,
        sidecars: 0,
        dcs_starts: 0,
        failures: 0,
        last: (0, 0),
        changed: Instant::now(),
        boots: Vec::new(),
    };
    match runner.run() {
        Ok(()) if runner.failures == 0 => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(e) => {
            runner.fail(u64::MAX, &e);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kill_explains_an_end_and_a_failed_check_is_bad() {
        assert!(!bad_report("the agent closed the connection"));
        assert!(!bad_report("the agent restarted"));
        assert!(!bad_report(
            "a HELLO of an earlier attach of this sidecar at 0x8, before 0x10; not this attach's"
        ));
        for text in [
            "outbound ring: tail 0x9 impossible for head 0x0",
            "record at 0x8 (len 9, kind 1): record passes the tail",
            "FRAME at 0x10: payload too short",
            "a FRAME numbered 0",
            "a record of kind 3 before any FRAME",
            "HELLO at 0x8: payload too short",
            "the last session ended on a failed record check; skipped from 0x8 to out_tail 0x10, records between are lost",
            "the kept out_head 0x9 is impossible for out_tail 0x10; records between are lost",
        ] {
            assert!(bad_report(text), "{text}");
        }
    }
}
