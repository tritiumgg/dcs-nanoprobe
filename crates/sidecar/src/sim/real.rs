//! `RealSim`: the adapter over the real link to an agent in DCS
//! (spec/transport.md), and the health verdict read from the mapped file
//! (spec/errors.md, "Health").
//!
//! [`crate::link::run`] runs on its own thread and calls a [`Sink`] that
//! shares a queue with the adapter. The driver calls the adapter from its own
//! thread.
//!
//! # Positions
//!
//! The reconciler journals the inbound position just after each command
//! before the command is written ([`SimAdapter::position_after`]). The adapter
//! predicts it with a private producer that starts where the attach put
//! `in_tail` (the `HELLO`'s `in_head`) and moves by each queued command's
//! cost, padding included. Sending opens when the driver reads the session's
//! first `HELLO`, not when the link sees it: a command the reconciler sent
//! before it read that `HELLO` belongs to the session before, and it
//! resolves those at the `HELLO` as never sent. The link writes the queue in
//! order, so the
//! prediction holds; [`RealSim::mispredicted`] counts any write that ends
//! elsewhere.
//!
//! # Group commit
//!
//! A command sent is held until [`SimAdapter::flush`]; only then does the link
//! write it. A record goes to the driver at [`SimAdapter::read`], and the link
//! acknowledges it to the agent (stores `out_head`) only after the flush that
//! follows that read: [`Sink::durable`] waits for it. So a command is on disk
//! before DCS sees it, and a record is on disk before the agent learns it was
//! read (spec/persistence.md, "Writes"). The wait is at most one driver step;
//! it gives up when the link is asked to stop, and the records are then read
//! again by the next session and dropped by their numbers.
//!
//! Commands still queued when a session ends are dropped: they never reached
//! the ring, and the reconciler resolves them at the next `HELLO` as never
//! taken.

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nanoprobe_transport::layout::{kind, lifecycle as lc, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::records::Hello;
use nanoprobe_transport::ring::{Producer, Ring};
use nanoprobe_transport::status::{self, Snapshot};

use crate::link::{self, Command, Config, End, Sink};
use crate::sim::{Detached, RawRecord, SimAdapter};

/// How long [`Sink::durable`] waits between checks of the stop flag.
const DURABLE_WAIT: Duration = Duration::from_millis(50);
/// How often the status block is read for the verdict.
const STATUS_EVERY: Duration = Duration::from_millis(100);
/// How often the handshake port is tried for the verdict, when the verdict
/// needs it.
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// How long one try of the handshake port waits.
const PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// The health verdict (spec/errors.md, "Health").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The heartbeat advances.
    Healthy,
    /// The heartbeat stands still and something listens.
    Hung,
    /// Nothing listens.
    Gone,
    /// The kill switch holds the agent.
    Disabled,
}

/// The limits of the verdict (spec/config.md, `[health]`).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// `heartbeat_stale_s`.
    pub heartbeat_stale: Duration,
    /// `loading_limit_s`.
    pub loading_limit: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            heartbeat_stale: Duration::from_secs(10),
            loading_limit: Duration::from_secs(900),
        }
    }
}

#[derive(Default)]
struct Shared {
    /// The inbound ring of the attached agent, once its geometry is read.
    ring: Option<Ring>,
    /// The predicted producer, from the driver's read of the `HELLO` on;
    /// `None` while commands may not be sent.
    predict: Option<Producer>,
    /// A session's first `HELLO`, by record number, with its producer: it
    /// becomes `predict` when [`SimAdapter::read`] passes that `HELLO` on.
    /// Until the reconciler has read it, a command it sends belongs to the
    /// session before, and it resolves those as never sent.
    armed: Option<(u64, Producer)>,
    /// Commands released to the link, with the end each was predicted at.
    queue: VecDeque<(Command, u64)>,
    /// Commands sent and held back until [`SimAdapter::flush`].
    held: VecDeque<(Command, u64)>,
    /// Records read, for [`SimAdapter::read`].
    records: Vec<RawRecord>,
    /// The highest record number read, per agent boot.
    last: HashMap<u64, u64>,
    /// Records handed to the adapter, ever.
    handed: u64,
    /// Records taken by [`SimAdapter::read`], ever.
    taken: u64,
    /// Records taken before the last [`SimAdapter::flush`]: these are on
    /// disk.
    released: u64,
    /// Whether a session is attached.
    attached: bool,
    mispredicted: u64,
    /// The link's last diagnostic line, for the attach error.
    last_report: String,
    /// `HELLO`s read in this session.
    hellos: u32,
    /// The number of a resync `HELLO` passed to [`Sink::hello`] and not yet
    /// to [`Sink::record`].
    resync: Option<u64>,
}

/// What the status block said, for the verdict.
#[derive(Default)]
struct Watch {
    map: Option<MappedFile>,
    opened_at: Option<Instant>,
    read_at: Option<Instant>,
    /// Whether the status block was read once.
    seen: bool,
    boot: u64,
    frame: u64,
    /// When `frame` was last seen to change.
    beat_at: Option<Instant>,
    lifecycle: u32,
    flags: u32,
    in_capacity: u64,
    periods: Option<[f64; 4]>,
    probe_at: Option<Instant>,
    /// Whether the last connect to the handshake port was refused.
    refused: bool,
    /// Whether a session was attached at the last read.
    attached: bool,
}

/// The adapter over the real link. Dropping it stops the link thread.
pub struct RealSim {
    cfg: Config,
    limits: Limits,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<End>>,
    watch: Watch,
}

struct RealSink {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    stop: Arc<AtomicBool>,
    map_path: PathBuf,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl RealSim {
    /// An adapter for the agent `cfg` names, detached. [`RealSim::attach`]
    /// starts the link.
    pub fn new(cfg: Config, limits: Limits) -> RealSim {
        RealSim {
            cfg,
            limits,
            shared: Arc::new((Mutex::new(Shared::default()), Condvar::new())),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            watch: Watch::default(),
        }
    }

    /// Start a session loop: a new sidecar boot. What was read and not yet
    /// taken is kept, and so are the highest record numbers read, so no
    /// record is passed on twice.
    pub fn attach(&mut self) {
        self.detach();
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let mut sink = RealSink {
            shared: self.shared.clone(),
            stop: stop.clone(),
            map_path: self.cfg.map_path.clone(),
        };
        let cfg = self.cfg.clone();
        self.thread = Some(std::thread::spawn(move || {
            link::run(&cfg, &mut sink, &stop)
        }));
    }

    /// Stop the session loop and return why it ended; `None` when none ran.
    pub fn detach(&mut self) -> Option<End> {
        self.stop.store(true, Ordering::Relaxed);
        self.shared.1.notify_all();
        let end = self.thread.take().and_then(|t| t.join().ok());
        let mut s = lock(&self.shared.0);
        s.attached = false;
        s.predict = None;
        s.armed = None;
        s.queue.clear();
        s.held.clear();
        end
    }

    /// Whether the session loop ended by itself: the agent is gone for good
    /// ([`End::Gone`]) or the loop stopped. The caller exits so that whatever
    /// runs the sidecar starts it again.
    pub fn ended(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Whether a `HELLO` was read and commands may be sent.
    pub fn ready(&self) -> bool {
        lock(&self.shared.0).predict.is_some()
    }

    /// Whether a session is attached.
    pub fn attached(&self) -> bool {
        lock(&self.shared.0).attached
    }

    /// Commands released and not yet written.
    pub fn queued(&self) -> usize {
        lock(&self.shared.0).queue.len()
    }

    /// Writes that ended where the prediction did not.
    pub fn mispredicted(&self) -> u64 {
        lock(&self.shared.0).mispredicted
    }

    /// The highest record number read, per agent boot.
    pub fn last_numbers(&self) -> HashMap<u64, u64> {
        lock(&self.shared.0).last.clone()
    }

    /// Why the last attach failed, as spec/errors.md names it; empty while
    /// attached or before any attempt.
    pub fn attach_error(&self) -> String {
        let s = lock(&self.shared.0);
        if s.attached {
            return String::new();
        }
        if u64::from(self.watch.flags) & nanoprobe_transport::layout::flags::NO_HANDSHAKE != 0 {
            return "no_listener".into();
        }
        reason(&s.last_report).to_owned()
    }

    /// Read the status block for the verdict, at most every 100 ms, and try
    /// the handshake port when the verdict needs it, at most once a second.
    /// The driver calls it every step.
    pub fn watch(&mut self) {
        let now = Instant::now();
        let w = &mut self.watch;
        if w.read_at.is_some_and(|t| now - t < STATUS_EVERY) {
            return;
        }
        w.read_at = Some(now);
        // The file outlives DCS, and a new DCS may make it again: open it
        // again once a second while the heartbeat stands still.
        let stale = w.beat_at.is_none_or(|t| now - t >= Duration::from_secs(1));
        if w.map.is_none() || (stale && w.opened_at.is_none_or(|t| now - t >= PROBE_EVERY)) {
            w.map = MappedFile::open_read_only(&self.cfg.map_path).ok();
            w.opened_at = Some(now);
        }
        if let Some(snap) = w.map.as_ref().and_then(Snapshot::read) {
            let boot = snap.u64(s::AGENT_BOOT);
            let frame = snap.u64(s::FRAME);
            // The first sight is no beat: an old file with no DCS behind it
            // is not healthy. A change after it is.
            if w.seen && (boot != w.boot || frame != w.frame) {
                w.beat_at = Some(now);
            }
            w.seen = true;
            w.boot = boot;
            w.frame = frame;
            w.lifecycle = snap.u32(s::LIFECYCLE);
            w.flags = snap.u32(s::FLAGS);
            w.in_capacity = snap.u64(s::IN_CAPACITY);
            w.periods = w
                .map
                .as_ref()
                .and_then(status::read_metrics)
                .map(|m| m.periods)
                .filter(|p| p.iter().any(|v| *v > 0.0));
        }
        // An attached session's connection shows that the agent's process
        // holds its socket: no probe then, and no probe connection waits in
        // the agent's accept queue.
        w.attached = lock(&self.shared.0).attached;
        let needs_probe = !w.attached
            && (u64::from(w.lifecycle) == lc::DISABLED || !beat_within(w, now, &self.limits));
        if needs_probe && w.probe_at.is_none_or(|t| now - t >= PROBE_EVERY) {
            w.probe_at = Some(now);
            w.refused = refused(self.cfg.handshake);
        }
    }

    /// The verdict from what [`RealSim::watch`] read last.
    pub fn verdict(&self) -> Verdict {
        let w = &self.watch;
        let now = Instant::now();
        if u64::from(w.lifecycle) == lc::DISABLED && w.probe_at.is_some() && !w.refused {
            return Verdict::Disabled;
        }
        if beat_within(w, now, &self.limits) {
            return Verdict::Healthy;
        }
        if w.attached {
            return Verdict::Hung;
        }
        if w.probe_at.is_none() || w.refused {
            return Verdict::Gone;
        }
        Verdict::Hung
    }

    /// The time since the agent's heartbeat last advanced; `None` before one
    /// was seen to advance.
    pub fn heartbeat_age(&self) -> Option<Duration> {
        self.watch.beat_at.map(|t| t.elapsed())
    }

    /// The largest command payload the agent takes: an eighth of the
    /// inbound ring; `None` before the file was read.
    pub fn max_command(&self) -> Option<usize> {
        (self.watch.in_capacity > 0).then_some((self.watch.in_capacity / 8) as usize)
    }

    /// The periods the sampling tiers achieve now, seconds; `None` before
    /// the agent wrote them.
    pub fn achieved(&self) -> Option<[f64; 4]> {
        self.watch.periods
    }

    /// The lifecycle state the status block shows, 0 before it was read.
    pub fn lifecycle(&self) -> u32 {
        self.watch.lifecycle
    }

    /// The mapped file's path, as the sidecar opens it.
    pub fn map_path(&self) -> &Path {
        &self.cfg.map_path
    }
}

/// Whether the heartbeat advanced within the limit for this lifecycle state.
fn beat_within(w: &Watch, now: Instant, limits: &Limits) -> bool {
    let limit = if u64::from(w.lifecycle) == lc::LOADING {
        limits.loading_limit
    } else {
        limits.heartbeat_stale
    };
    w.beat_at.is_some_and(|t| now - t < limit)
}

/// Whether a connect to `addr` is refused. A connect that is accepted is
/// closed at once; one that times out counts as not refused.
fn refused(addr: SocketAddr) -> bool {
    match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::ConnectionRefused,
    }
}

/// The attach error's name (spec/errors.md, "Health") for a link diagnostic.
fn reason(report: &str) -> &'static str {
    const AGENT: [&str; 4] = ["version", "token", "disabled", "no_file"];
    // The link names the agent's refusal by one of its codes
    // (crates/transport/src/handshake.rs); anything else is not a refusal.
    if let Some(r) = report.strip_prefix("the agent refused: ")
        && let Some(a) = AGENT.into_iter().find(|a| r.trim() == *a)
    {
        return a;
    }
    if report.is_empty() {
        ""
    } else if report.contains("layout hash") {
        "layout_hash"
    } else if report.contains("agent boot") {
        "boot_mismatch"
    } else if report.starts_with("connect ") {
        "no_listener"
    } else if report.contains("answer to HELLO") || report.contains("send HELLO") {
        "no_answer"
    } else if report.starts_with("map ") || report.starts_with("header") {
        "bad_file"
    } else {
        ""
    }
}

impl Drop for RealSim {
    fn drop(&mut self) {
        self.detach();
    }
}

impl SimAdapter for RealSim {
    fn position_after(&self, _opcode: u16, payload: &[u8]) -> u64 {
        let s = lock(&self.shared.0);
        match &s.predict {
            Some(p) => p.tail() + p.cost(payload.len()),
            None => 0,
        }
    }

    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached> {
        let mut s = lock(&self.shared.0);
        let Some(p) = s.predict.as_mut() else {
            return Err(Detached);
        };
        let end = p.tail() + p.cost(payload.len());
        *p = Producer::new(p.ring(), end);
        s.held.push_back((
            Command {
                opcode,
                payload: payload.to_vec(),
            },
            end,
        ));
        Ok(end)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        let mut s = lock(&self.shared.0);
        let v = std::mem::take(&mut s.records);
        s.taken += v.len() as u64;
        if let Some((n, _)) = s.armed
            && v.iter()
                .any(|r| u64::from(r.kind) == kind::HELLO && r.number == n)
        {
            s.predict = s.armed.take().map(|(_, p)| p);
        }
        v
    }

    fn flush(&mut self) {
        let (m, cv) = &*self.shared;
        let mut s = lock(m);
        let held: Vec<_> = s.held.drain(..).collect();
        s.queue.extend(held);
        s.released = s.taken;
        cv.notify_all();
    }
}

impl Sink for RealSink {
    fn attached(&mut self, _agent_boot: u64, _sidecar_boot: u64) {
        let ring = MappedFile::open_read_only(&self.map_path)
            .ok()
            .and_then(|m| status::check(&m, m.file_len()).ok())
            .map(|g| Ring::inbound(g.out_capacity, g.in_capacity));
        let mut s = lock(&self.shared.0);
        // A new session: what the last one released never reached the ring.
        s.queue.clear();
        s.held.clear();
        s.predict = None;
        s.armed = None;
        s.ring = ring;
        s.attached = true;
        s.last_report.clear();
        s.hellos = 0;
        s.resync = None;
    }

    fn last_number(&self, agent_boot: u64) -> u64 {
        lock(&self.shared.0)
            .last
            .get(&agent_boot)
            .copied()
            .unwrap_or(0)
    }

    fn reread(&mut self, _: u64, _: u64, _: u16, _: &[u8]) {}

    fn record(
        &mut self,
        agent_boot: u64,
        number: u64,
        kind: u16,
        payload: &[u8],
    ) -> Result<(), String> {
        let mut s = lock(&self.shared.0);
        s.last.insert(agent_boot, number);
        let resync = s.resync.take_if(|n| *n == number).is_some();
        s.records.push(RawRecord {
            agent_boot,
            number,
            kind,
            payload: payload.to_vec(),
            resync,
        });
        s.handed += 1;
        Ok(())
    }

    fn durable(&mut self) -> Result<(), String> {
        let (m, cv) = &*self.shared;
        let mut s = lock(m);
        let want = s.handed;
        while s.released < want {
            if self.stop.load(Ordering::Relaxed) {
                return Err("the sidecar stopped before the records were stored".into());
            }
            s = cv
                .wait_timeout(s, DURABLE_WAIT)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Ok(())
    }

    fn hello(&mut self, hello: &Hello, number: u64) {
        let mut s = lock(&self.shared.0);
        s.hellos += 1;
        if s.hellos > 1 {
            s.resync = Some(number);
        }
        let Some(ring) = s.ring else {
            s.last_report = "a HELLO before the ring's geometry was read".into();
            return;
        };
        // A second HELLO in an attach is a resync: the ring is untouched and
        // the queue goes on (spec/commands.md, "After CONTROL_LOST").
        if s.predict.is_none() && s.armed.is_none() {
            s.armed = Some((number, Producer::new(ring, hello.in_head)));
        }
    }

    fn next_command(&mut self) -> Option<Command> {
        let s = lock(&self.shared.0);
        s.queue.front().map(|(c, _)| c.clone())
    }

    fn sent(&mut self, _command: &Command, end: u64, _wrapped: bool) {
        let mut s = lock(&self.shared.0);
        if let Some((_, want)) = s.queue.pop_front()
            && want != end
        {
            s.mispredicted += 1;
            s.last_report = format!("a command ended at {end}, predicted {want}");
        }
    }

    fn report(&mut self, line: &str) {
        let mut s = lock(&self.shared.0);
        // The link reports two things during a session that goes on: a late
        // poll and a `HELLO` of an earlier attach. Every other line is
        // reported at a session's end, or before an attach stores
        // `attach_seen` (crates/sidecar/src/link.rs).
        if !line.starts_with("a poll came") && !line.starts_with("a HELLO of an earlier attach") {
            s.attached = false;
            s.predict = None;
            s.armed = None;
        }
        s.last_report = line.to_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_transport::mem::Memory as _;
    use nanoprobe_transport::status::Header;

    /// A mapped file with a status block, the agent's side of what
    /// [`RealSim::watch`] reads.
    fn status_file(path: &Path, lifecycle: u64) -> MappedFile {
        let header = Header {
            interface: 1,
            agent_boot: 0xb007,
            build: 1,
            created_ms: 1,
            clock_hz: 1,
            out_capacity: 64 << 10,
            in_capacity: 64 << 10,
        };
        let m = MappedFile::create(path, header.file_size()).expect("map");
        status::start(&m, &header, lifecycle as u32);
        m
    }

    fn sim_for(map: &Path, handshake: SocketAddr, stale_ms: u64) -> RealSim {
        RealSim::new(
            Config {
                map_path: map.to_path_buf(),
                handshake,
                token: String::new(),
                poll: Duration::from_millis(2),
                handshake_timeout: Duration::from_secs(1),
            },
            Limits {
                heartbeat_stale: Duration::from_millis(stale_ms),
                loading_limit: Duration::from_millis(stale_ms),
            },
        )
    }

    /// Read the status block now, past the 100 ms read interval.
    fn watch_now(sim: &mut RealSim) {
        sim.watch.read_at = None;
        sim.watch.probe_at = None;
        sim.watch();
    }

    #[test]
    fn the_verdict_follows_the_heartbeat_and_the_listener() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("a.map");
        let m = status_file(&path, lc::RUNNING);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut sim = sim_for(&path, listener.local_addr().expect("addr"), 300);
        watch_now(&mut sim);
        // A first sight is no beat.
        assert_ne!(sim.verdict(), Verdict::Healthy);
        m.store(s::FRAME, 1, std::sync::atomic::Ordering::Release);
        watch_now(&mut sim);
        assert_eq!(sim.verdict(), Verdict::Healthy);
        assert!(sim.heartbeat_age().is_some());
        std::thread::sleep(Duration::from_millis(350));
        watch_now(&mut sim);
        assert_eq!(sim.verdict(), Verdict::Hung, "stale, and something listens");
        drop(listener);
        watch_now(&mut sim);
        assert_eq!(sim.verdict(), Verdict::Gone, "stale, and nothing listens");
    }

    #[test]
    fn a_disabled_agent_with_its_socket_is_disabled() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("a.map");
        let _m = status_file(&path, lc::DISABLED);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut sim = sim_for(&path, listener.local_addr().expect("addr"), 300);
        watch_now(&mut sim);
        assert_eq!(sim.verdict(), Verdict::Disabled);
        drop(listener);
        watch_now(&mut sim);
        assert_eq!(sim.verdict(), Verdict::Gone);
    }

    #[test]
    fn a_record_is_durable_only_after_the_flush_that_follows_its_read() {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut sink = RealSink {
            shared: shared.clone(),
            stop: stop.clone(),
            map_path: PathBuf::new(),
        };
        sink.record(1, 1, kind::FRAME as u16, &[]).expect("record");
        let waiter = std::thread::spawn(move || {
            let r = sink.durable();
            (sink, r)
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!waiter.is_finished(), "durable waits for the flush");
        let mut sim = RealSim {
            cfg: Config {
                map_path: PathBuf::new(),
                handshake: "127.0.0.1:1".parse().expect("addr"),
                token: String::new(),
                poll: Duration::from_millis(2),
                handshake_timeout: Duration::from_secs(1),
            },
            limits: Limits::default(),
            shared,
            stop,
            thread: None,
            watch: Watch::default(),
        };
        // A flush before the read releases nothing.
        sim.flush();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!waiter.is_finished(), "nothing was read yet");
        assert_eq!(sim.read().len(), 1);
        sim.flush();
        let (mut sink, r) = waiter.join().expect("join");
        r.expect("durable after read and flush");
        // A stop ends the wait with an error.
        sink.record(1, 2, kind::FRAME as u16, &[]).expect("record");
        sim.stop.store(true, Ordering::Relaxed);
        assert!(sink.durable().is_err());
    }

    #[test]
    fn sending_opens_when_the_driver_reads_the_hello() {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        lock(&shared.0).ring = Some(Ring::inbound(64 << 10, 64 << 10));
        let mut sink = RealSink {
            shared: shared.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            map_path: PathBuf::new(),
        };
        let mut sim = RealSim::new(
            Config {
                map_path: PathBuf::new(),
                handshake: "127.0.0.1:1".parse().expect("addr"),
                token: String::new(),
                poll: Duration::from_millis(2),
                handshake_timeout: Duration::from_secs(1),
            },
            Limits::default(),
        );
        sim.shared = shared;
        let hello = Hello::default();
        sink.hello(&hello, 7);
        assert!(sim.send(1, &[0; 8]).is_err(), "the HELLO is not read yet");
        sink.record(1, 7, kind::HELLO as u16, &[]).expect("record");
        assert_eq!(sim.read().len(), 1);
        assert!(sim.send(1, &[0; 8]).is_ok());
    }

    #[test]
    fn reasons_name_the_spec_codes() {
        assert_eq!(reason("the agent refused: token"), "token");
        assert_eq!(reason("the agent refused: disabled"), "disabled");
        assert_eq!(
            reason("the agent's layout hash 0000000000000001 is not 0000000000000002"),
            "layout_hash"
        );
        assert_eq!(
            reason("the file's agent boot 1 is not the OK line's 2"),
            "boot_mismatch"
        );
        assert_eq!(
            reason("connect 127.0.0.1:47310: Connection refused"),
            "no_listener"
        );
        assert_eq!(reason("read the answer to HELLO: timed out"), "no_answer");
        assert_eq!(reason("map /x: not found"), "bad_file");
        assert_eq!(reason(""), "");
        assert_eq!(reason("the agent refused: bogus"), "");
    }

    #[test]
    fn an_attached_session_with_a_stale_heartbeat_is_hung_and_not_probed() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("a.map");
        let _m = status_file(&path, lc::RUNNING);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut sim = sim_for(&path, listener.local_addr().expect("addr"), 300);
        lock(&sim.shared.0).attached = true;
        watch_now(&mut sim);
        assert_eq!(sim.verdict(), Verdict::Hung);
        assert!(sim.watch.probe_at.is_none(), "no probe while attached");
    }

    /// A sink attached, its inbound ring set and a `HELLO` read, with the
    /// adapter that sends through it.
    fn sending_session() -> (RealSim, RealSink) {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let mut sink = RealSink {
            shared: shared.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            map_path: PathBuf::new(),
        };
        let mut sim = RealSim::new(
            Config {
                map_path: PathBuf::new(),
                handshake: "127.0.0.1:1".parse().expect("addr"),
                token: String::new(),
                poll: Duration::from_millis(2),
                handshake_timeout: Duration::from_secs(1),
            },
            Limits::default(),
        );
        sim.shared = shared;
        sink.attached(1, 2);
        lock(&sink.shared.0).ring = Some(Ring::inbound(64 << 10, 64 << 10));
        sink.hello(&Hello::default(), 7);
        sink.record(1, 7, kind::HELLO as u16, &[]).expect("record");
        assert_eq!(sim.read().len(), 1);
        assert!(sim.attached());
        assert!(sim.send(1, &[0; 8]).is_ok());
        (sim, sink)
    }

    #[test]
    fn the_two_reports_of_a_session_that_goes_on_keep_it_attached() {
        for line in [
            "a poll came 300ms late",
            "a HELLO of an earlier attach of this sidecar at 0x0, before 0x10; not this attach's",
        ] {
            let (mut sim, mut sink) = sending_session();
            sink.report(line);
            assert!(sim.attached(), "{line}");
            assert!(sim.ready(), "{line}");
            assert!(sim.send(1, &[0; 8]).is_ok(), "{line}");
        }
    }

    #[test]
    fn any_other_report_ends_the_session() {
        for line in [
            "connect 127.0.0.1:1: refused",
            "the agent refused: token",
            "",
        ] {
            let (mut sim, mut sink) = sending_session();
            sink.report(line);
            assert!(!sim.attached(), "{line:?}");
            assert!(!sim.ready(), "{line:?}");
            assert!(sim.send(1, &[0; 8]).is_err(), "{line:?}");
        }
    }

    #[test]
    fn a_missing_file_with_nothing_listening_is_gone() {
        let dir = tempfile::tempdir().expect("dir");
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr")
        };
        let mut sim = RealSim::new(
            Config {
                map_path: dir.path().join("none.map"),
                handshake: port,
                token: String::new(),
                poll: Duration::from_millis(2),
                handshake_timeout: Duration::from_secs(1),
            },
            Limits::default(),
        );
        sim.watch();
        assert_eq!(sim.verdict(), Verdict::Gone);
        assert_eq!(sim.heartbeat_age(), None);
        assert!(sim.send(1, &[]).is_err());
    }
}
