//! `link-probe --map PATH --port N --token-file PATH --seconds N [--poll-ms N]
//! [--short-command] [--bad-record past-tail|over-limit|misaligned|past-capacity|padding]`,
//! or `link-probe --now-ms`, which prints the wall clock as the probe's `t`
//! reads it and exits, so a test can compare its own times with the probe's.
//!
//! It runs for `--seconds`, or until one second after the status block reads
//! `DISABLED`, whichever comes first.
//!
//! A sidecar for the in-game test of the agent's link (step S7). It attaches
//! through `nanoprobe_sidecar::link` and prints one JSON object per line, each
//! with `t`, wall milliseconds since the Unix epoch:
//!
//! ```text
//! {"t":..,"ev":"attached","agent_boot":"<hex>"}
//! {"t":..,"ev":"hello","number":N,"epoch":N,"in_head":N,"interface":N,"dcs":".."}
//! {"t":..,"ev":"lifecycle","state":N,"gen":N,"theater":"..","mission":".."}
//! {"t":..,"ev":"result","opcode":N,"code":N,"seq":N}
//! {"t":..,"ev":"log","level":N,"target":"..","msg":".."}
//! {"t":..,"ev":"gap","class":N,"count":N}
//! {"t":..,"ev":"report","text":".."}
//! {"t":..,"ev":"status","lifecycle":N,"gen":N,"flags":N,"frame":N,"boot":"<hex>"}
//! {"t":..,"ev":"final","lifecycle":N,"frame":N,"commands":N,"timed":N,"over_budget":N}
//! {"t":..,"ev":"reconnect","gen":N,"ready":T,"read":T,"ms":N}
//! {"t":..,"ev":"end","records":N}
//! ```
//!
//! A `status` line comes from a second thread that reads the status block
//! every millisecond and prints when `lifecycle`, `mission_gen`, `flags` or
//! `agent_boot` changes: it shows when DCS reached a state, and the `lifecycle` lines show
//! when the sidecar read it. The `final` line is its last reading. At the end, one `reconnect` line per mission
//! generation that ran: `ready` is when the status block first showed it
//! running or paused, `read` when the sidecar first read its `LIFECYCLE`
//! saying so, and `ms` the difference. After each `HELLO` the probe sends
//! `CAMPAIGN` 7, then under a new epoch a `MESSAGE` for campaign 7, which the
//! agent answers `UNKNOWN_OPCODE` (20), and one for campaign 8, which it
//! answers `WRONG_CAMPAIGN` (24). With `--short-command`, after the first
//! `HELLO` only, it then sends a command shorter than its header, which the
//! agent answers by detaching.
//!
//! With `--bad-record`, once the first `HELLO`'s commands are answered, the
//! probe breaks the inbound ring itself, writing to the mapped file past the
//! link (spec/faults.md): `past-tail`, a record longer than what lies before
//! the tail; `over-limit`, a record over the payload limit; `misaligned`, a
//! tail that is not a multiple of 8; `past-capacity`, a tail more than the
//! capacity ahead of the head; `padding`, a padding record that does not end
//! at the end of the ring. It prints
//! `{"t":..,"ev":"injected","what":".."}`. The agent detaches, and the probe
//! attaches again.
//!
//! Test tooling: built from source, never released.

use std::collections::HashMap;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::{arg, content, json_string, read_token};
use nanoprobe_sidecar::link::{self, Command, Config, End, Sink};
use nanoprobe_transport::codec::Writer;
use nanoprobe_transport::layout::{kind, record as r, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::{CommandHeader, CommandResult, Gap, Hello, Lifecycle, Log};
use nanoprobe_transport::ring::{Ring, record_size};

/// One JSON line on standard output, under a lock so the two threads' lines
/// never mix.
#[derive(Clone)]
struct Out(Arc<Mutex<()>>);

impl Out {
    fn line(&self, ev: &str, fields: &[(&str, Json)]) {
        let mut text = format!("{{\"t\":{},\"ev\":\"{ev}\"", content::now_ms());
        for (k, v) in fields {
            text.push_str(&format!(",\"{k}\":"));
            match v {
                Json::Num(n) => text.push_str(&n.to_string()),
                Json::Str(s) => text.push_str(&json_string(s)),
            }
        }
        text.push('}');
        let _guard = self.0.lock();
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{text}");
        let _ = stdout.flush();
    }
}

enum Json {
    Num(u64),
    Str(String),
}

/// Per mission generation: when the status block first showed it running
/// or paused, and when the sidecar first read a `LIFECYCLE` saying so.
type Times = Arc<Mutex<HashMap<u32, (Option<u64>, Option<u64>)>>>;

/// Running or paused.
fn runs(state: u32) -> bool {
    state == 3 || state == 4
}

/// The campaign the probe announces.
const CAMPAIGN: u64 = 7;

/// A way to break the inbound ring (`--bad-record`).
#[derive(Clone, Copy, Debug)]
enum Bad {
    PastTail,
    OverLimit,
    Misaligned,
    PastCapacity,
    Padding,
}

impl Bad {
    fn parse(text: &str) -> Option<Bad> {
        Some(match text {
            "past-tail" => Bad::PastTail,
            "over-limit" => Bad::OverLimit,
            "misaligned" => Bad::Misaligned,
            "past-capacity" => Bad::PastCapacity,
            "padding" => Bad::Padding,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Bad::PastTail => "past-tail",
            Bad::OverLimit => "over-limit",
            Bad::Misaligned => "misaligned",
            Bad::PastCapacity => "past-capacity",
            Bad::Padding => "padding",
        }
    }

    /// Break the inbound ring of the file at `path`: write the record, if
    /// any, at the tail and store a tail the agent's checks refuse. The
    /// probe sends nothing more on this session, so the link's own tail
    /// is never stored after this.
    fn write(self, path: &std::path::Path) -> Result<(), String> {
        let m = MappedFile::open(path).map_err(|e| format!("map: {e}"))?;
        let ring = Ring::inbound(
            m.load(s::OUT_CAPACITY, Ordering::Acquire),
            m.load(s::IN_CAPACITY, Ordering::Acquire),
        );
        let tail = m.load(s::IN_TAIL, Ordering::Acquire);
        let at = ring.data + (tail & (ring.capacity - 1)) as usize;
        let header = |len: u32, kind: u16| {
            let mut h = [0u8; r::HEADER];
            h[r::LEN..r::LEN + 4].copy_from_slice(&len.to_le_bytes());
            h[r::KIND..r::KIND + 2].copy_from_slice(&kind.to_le_bytes());
            h
        };
        let opcode = content::OPCODE as u16;
        let new_tail = match self {
            Bad::PastTail => {
                m.write(at, &header(64, opcode));
                tail + r::HEADER as u64
            }
            Bad::OverLimit => {
                let len = ring.max_payload() + 8;
                m.write(at, &header(len as u32, opcode));
                tail + record_size(len)
            }
            Bad::Padding => {
                let mut end = tail;
                for (pos, len) in padding(tail, ring.capacity) {
                    let off = ring.data + (pos & (ring.capacity - 1)) as usize;
                    m.write(off, &header(len, kind::PADDING as u16));
                    end = pos + record_size(len as usize);
                }
                end
            }
            Bad::Misaligned => tail + 4,
            Bad::PastCapacity => tail + ring.capacity + 8,
        };
        m.store(s::IN_TAIL, new_tail, Ordering::Release);
        Ok(())
    }
}

/// Padding records, as positions and lengths, that end in one a consumer
/// refuses because it does not end at the end of the ring. An empty padding
/// record at `tail` is refused unless it ends exactly at the end; there a
/// valid one goes first, and the refused one follows at the ring's start.
fn padding(tail: u64, capacity: u64) -> Vec<(u64, u32)> {
    let size = record_size(0);
    if (tail & (capacity - 1)) + size == capacity {
        vec![(tail, 0), (tail + size, 0)]
    } else {
        vec![(tail, 0)]
    }
}

struct Probe {
    /// Send a command shorter than its header after the first `HELLO`.
    short: bool,
    /// Break the inbound ring once the first `HELLO`'s commands are
    /// answered.
    bad: Option<Bad>,
    /// The answers are in: break the ring on the next poll.
    fire: bool,
    map_path: std::path::PathBuf,
    hellos: u64,
    out: Out,
    times: Times,
    last: HashMap<u64, u64>,
    records: u64,
    epoch: u64,
    queue: Vec<Command>,
}

impl Sink for Probe {
    fn attached(&mut self, agent_boot: u64, _sidecar_boot: u64) {
        self.out.line(
            "attached",
            &[("agent_boot", Json::Str(format!("{agent_boot:016x}")))],
        );
    }

    fn last_number(&self, agent_boot: u64) -> u64 {
        self.last.get(&agent_boot).copied().unwrap_or(0)
    }

    fn reread(&mut self, _agent_boot: u64, _number: u64, _kind: u16, _payload: &[u8]) {}

    fn record(
        &mut self,
        agent_boot: u64,
        number: u64,
        kind: u16,
        payload: &[u8],
    ) -> Result<(), String> {
        self.last.insert(agent_boot, number);
        self.records += 1;
        match u64::from(kind) {
            kind::LIFECYCLE => {
                if let Ok(l) = Lifecycle::decode(payload) {
                    if runs(l.state)
                        && let Ok(mut times) = self.times.lock()
                    {
                        let e = times.entry(l.mission_gen).or_default();
                        e.1.get_or_insert(content::now_ms());
                    }
                    self.out.line(
                        "lifecycle",
                        &[
                            ("state", Json::Num(l.state.into())),
                            ("gen", Json::Num(l.mission_gen.into())),
                            ("theater", Json::Str(l.theater)),
                            ("mission", Json::Str(l.mission)),
                        ],
                    );
                }
            }
            kind::RESULT => {
                if let Ok(r) = CommandResult::decode(payload) {
                    if r.seq == 2 && self.bad.is_some() && self.hellos == 1 {
                        self.fire = true;
                    }
                    self.out.line(
                        "result",
                        &[
                            ("opcode", Json::Num(r.opcode.into())),
                            ("code", Json::Num(r.code.into())),
                            ("seq", Json::Num(r.seq)),
                        ],
                    );
                }
            }
            kind::LOG => {
                if let Ok(l) = Log::decode(payload) {
                    self.out.line(
                        "log",
                        &[
                            ("level", Json::Num(l.level.into())),
                            ("target", Json::Str(l.target)),
                            ("msg", Json::Str(l.msg)),
                        ],
                    );
                }
            }
            kind::GAP => {
                if let Ok(g) = Gap::decode(payload) {
                    self.out.line(
                        "gap",
                        &[
                            ("class", Json::Num(g.class.into())),
                            ("count", Json::Num(g.count.into())),
                        ],
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn durable(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn hello(&mut self, hello: &Hello, number: u64) {
        self.out.line(
            "hello",
            &[
                ("number", Json::Num(number)),
                ("epoch", Json::Num(hello.epoch)),
                ("in_head", Json::Num(hello.in_head)),
                ("interface", Json::Num(hello.interface.into())),
                ("build", Json::Str(format!("{:016x}", hello.build))),
                ("dcs", Json::Str(hello.dcs_version.clone())),
            ],
        );
        // A new epoch above the agent's (spec/commands.md), the campaign
        // first, then an effect the agent has no handler for yet, for that
        // campaign and for another.
        self.epoch = self.epoch.max(hello.epoch) + 1;
        let mut w = Writer::new();
        CommandHeader::default().write(&mut w);
        w.u64(CAMPAIGN);
        let campaign = Command {
            opcode: 1,
            payload: w.finish(),
        };
        let message = |seq: u64, campaign: u64| {
            let mut w = Writer::new();
            CommandHeader {
                seq,
                epoch: self.epoch,
                campaign,
                ..CommandHeader::default()
            }
            .write(&mut w);
            Command {
                opcode: content::OPCODE as u16,
                payload: w.finish(),
            }
        };
        self.queue = vec![message(2, CAMPAIGN + 1), message(1, CAMPAIGN), campaign];
        self.hellos += 1;
        if self.short && self.hellos == 1 {
            self.queue.insert(
                0,
                Command {
                    opcode: content::OPCODE as u16,
                    payload: vec![0; 10],
                },
            );
        }
    }

    fn next_command(&mut self) -> Option<Command> {
        if self.fire {
            self.fire = false;
            if let Some(bad) = self.bad.take() {
                let what = match bad.write(&self.map_path) {
                    Ok(()) => bad.name().to_owned(),
                    Err(e) => format!("{}: not written: {e}", bad.name()),
                };
                self.out.line("injected", &[("what", Json::Str(what))]);
            }
            return None;
        }
        self.queue.pop()
    }

    fn sent(&mut self, _command: &Command, _end: u64, _wrapped: bool) {}

    fn report(&mut self, line: &str) {
        self.out
            .line("report", &[("text", Json::Str(line.to_owned()))]);
    }
}

/// Print the status block's lifecycle, generation and flags whenever one
/// changes, until `stop`.
fn watch(path: std::path::PathBuf, out: Out, times: Times, stop: Arc<AtomicBool>) {
    let mut map = None;
    let mut last = None;
    let mut disabled_at: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        if map.is_none() {
            map = MappedFile::open_read_only(&path).ok();
        }
        if let Some(m) = &map {
            let now = (
                m.load32(s::LIFECYCLE, Ordering::Acquire),
                m.load32(s::MISSION_GEN, Ordering::Acquire),
                m.load32(s::FLAGS, Ordering::Acquire),
                m.load(s::AGENT_BOOT, Ordering::Acquire),
            );
            if now.0 == 6 && disabled_at.is_none() {
                disabled_at = Some(Instant::now());
            }
            if disabled_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) {
                stop.store(true, Ordering::Relaxed);
            }
            if last != Some(now) {
                last = Some(now);
                if runs(now.0)
                    && let Ok(mut times) = times.lock()
                {
                    let e = times.entry(now.1).or_default();
                    e.0.get_or_insert(content::now_ms());
                }
                out.line(
                    "status",
                    &[
                        ("lifecycle", Json::Num(now.0.into())),
                        ("gen", Json::Num(now.1.into())),
                        ("flags", Json::Num(now.2.into())),
                        ("frame", Json::Num(m.load(s::FRAME, Ordering::Acquire))),
                        ("boot", Json::Str(format!("{:016x}", now.3))),
                    ],
                );
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if let Some(m) = &map {
        out.line(
            "final",
            &[
                (
                    "lifecycle",
                    Json::Num(m.load32(s::LIFECYCLE, Ordering::Acquire).into()),
                ),
                ("frame", Json::Num(m.load(s::FRAME, Ordering::Acquire))),
                (
                    "commands",
                    Json::Num(m.load(s::COMMANDS_APPLIED, Ordering::Acquire)),
                ),
                (
                    "timed",
                    Json::Num(
                        (0..s::HIST_BUCKETS)
                            .map(|i| m.load(s::HIST_COMMAND + 8 * i, Ordering::Relaxed))
                            .sum(),
                    ),
                ),
                (
                    "over_budget",
                    Json::Num(m.load(s::COMMANDS_OVER_BUDGET, Ordering::Acquire)),
                ),
            ],
        );
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--now-ms") {
        println!("{}", content::now_ms());
        return ExitCode::SUCCESS;
    }
    let need = |name: &str| arg(&args, name).ok_or(format!("{name} is required"));
    let run = || -> Result<(), String> {
        let token =
            read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
        let port: u16 = need("--port")?
            .parse()
            .map_err(|_| "--port takes a number")?;
        let seconds: u64 = need("--seconds")?
            .parse()
            .map_err(|_| "--seconds takes a number")?;
        let poll: u64 = arg(&args, "--poll-ms")
            .map_or(Ok(2), |v| v.parse().map_err(|_| "--poll-ms takes a number"))?;
        let cfg = Config {
            map_path: need("--map")?.into(),
            handshake: ([127, 0, 0, 1], port).into(),
            token,
            poll: Duration::from_millis(poll),
            handshake_timeout: Duration::from_secs(15),
        };
        let out = Out(Arc::new(Mutex::new(())));
        let stop = Arc::new(AtomicBool::new(false));
        let times: Times = Arc::default();
        let watcher = {
            let (path, out, times, stop) = (
                cfg.map_path.clone(),
                out.clone(),
                times.clone(),
                stop.clone(),
            );
            std::thread::spawn(move || watch(path, out, times, stop))
        };
        let timer = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_secs(seconds);
                while Instant::now() < end && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                stop.store(true, Ordering::Relaxed);
            })
        };
        let bad = match arg(&args, "--bad-record") {
            Some(v) => Some(Bad::parse(&v).ok_or(format!("no --bad-record named {v}"))?),
            None => None,
        };
        let mut probe = Probe {
            short: args.iter().any(|a| a == "--short-command"),
            bad,
            fire: false,
            map_path: cfg.map_path.clone(),
            hellos: 0,
            out: out.clone(),
            times: times.clone(),
            last: HashMap::new(),
            records: 0,
            epoch: 0,
            queue: Vec::new(),
        };
        let end = loop {
            match link::run(&cfg, &mut probe, &stop) {
                // The agent may be restarted on purpose during a test: keep
                // trying until the time is up.
                End::Gone if !stop.load(Ordering::Relaxed) => continue,
                end => break end,
            }
        };
        stop.store(true, Ordering::Relaxed);
        let _ = watcher.join();
        let _ = timer.join();
        let mut gens: Vec<_> = times
            .lock()
            .map(|t| t.iter().map(|(g, v)| (*g, *v)).collect())
            .unwrap_or_default();
        gens.sort_unstable();
        for (generation, (ready, read)) in gens {
            if let (Some(ready), Some(read)) = (ready, read) {
                out.line(
                    "reconnect",
                    &[
                        ("gen", Json::Num(generation.into())),
                        ("ready", Json::Num(ready)),
                        ("read", Json::Num(read)),
                        ("ms", Json::Num(read.saturating_sub(ready))),
                    ],
                );
            }
        }
        out.line(
            "end",
            &[
                ("records", Json::Num(probe.records)),
                ("how", Json::Str(format!("{end:?}"))),
            ],
        );
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("link-probe: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_transport::mem::Heap;
    use nanoprobe_transport::ring::{Consumer, RingError};

    /// At every tail position, the records `padding` gives fail the padding
    /// check and no other.
    #[test]
    fn padding_is_refused_at_every_position() {
        const CAP: u64 = 256;
        for tail in (0..2 * CAP).step_by(8) {
            let m = Heap::new(16 + CAP as usize);
            let ring = Ring::new(16, CAP, 0, 8);
            let mut end = tail;
            for (pos, len) in padding(tail, CAP) {
                let mut h = [0u8; r::HEADER];
                h[r::LEN..r::LEN + 4].copy_from_slice(&len.to_le_bytes());
                h[r::KIND..r::KIND + 2].copy_from_slice(&(kind::PADDING as u16).to_le_bytes());
                m.write(16 + (pos % CAP) as usize, &h);
                end = pos + record_size(len as usize);
            }
            m.store(0, end, Ordering::Release);
            let mut c = Consumer::new(ring, tail);
            c.refresh(&m).expect("tail");
            match c.next(&m, &mut Vec::new()) {
                Err(RingError::Record { rule, .. }) => assert_eq!(
                    rule, "padding does not end at the end of the ring",
                    "tail {tail}"
                ),
                other => panic!("tail {tail}: {other:?}"),
            }
        }
    }
}
