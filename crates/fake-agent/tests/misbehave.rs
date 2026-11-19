//! Each misbehavior against the stub sidecar, both peers in this process on
//! threads. A broken peer must be refused or survived with a line that says
//! what was wrong; nothing panics.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::agent::{Agent, Misbehavior, Options};
use nanoprobe_fake_agent::stub::Stub;
use nanoprobe_fake_agent::verify;
use nanoprobe_sidecar::link::{self, Command, Config, Sink};
use nanoprobe_transport::layout::{flags, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::Hello;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The stub, with its reports copied where the test can read them, and an
/// optional delay in every `record` to play a stalled sidecar.
struct Watched {
    stub: Stub,
    reports: Arc<Mutex<Vec<String>>>,
    stall: Option<(Arc<AtomicBool>, Duration)>,
    /// Commands left to send; `None` for no limit.
    limit: Option<u64>,
    /// Send one control command (seq 0) before the others.
    control_first: bool,
    /// Panic in `record`.
    panic: bool,
}

impl Sink for Watched {
    fn attached(&mut self, a: u64, b: u64) {
        self.stub.attached(a, b)
    }
    fn last_number(&self, a: u64) -> u64 {
        self.stub.last_number(a)
    }
    fn record(&mut self, a: u64, n: u64, k: u16, p: &[u8]) -> Result<(), String> {
        assert!(!self.panic, "a sink that panics");
        if let Some((flag, d)) = &self.stall
            && flag.swap(false, Ordering::Relaxed)
        {
            std::thread::sleep(*d);
        }
        self.stub.record(a, n, k, p)
    }
    fn durable(&mut self) -> Result<(), String> {
        self.stub.durable()
    }
    fn hello(&mut self, h: &Hello, number: u64) {
        self.stub.hello(h, number)
    }
    fn next_command(&mut self) -> Option<Command> {
        if std::mem::take(&mut self.control_first) {
            let mut w = nanoprobe_transport::codec::Writer::new();
            nanoprobe_transport::records::CommandHeader::default().write(&mut w);
            return Some(Command {
                opcode: 2,
                payload: w.finish(),
            });
        }
        match &mut self.limit {
            Some(0) => None,
            Some(n) => {
                *n -= 1;
                self.stub.next_command()
            }
            None => self.stub.next_command(),
        }
    }
    fn sent(&mut self, c: &Command, end: u64, wrapped: bool) {
        self.stub.sent(c, end, wrapped)
    }
    fn reread(&mut self, a: u64, n: u64, k: u16, p: &[u8]) {
        self.stub.reread(a, n, k, p)
    }
    fn report(&mut self, line: &str) {
        self.reports.lock().expect("reports").push(line.to_owned());
        self.stub.report(line)
    }
}

/// How a test's sidecar behaves.
#[derive(Default)]
struct Sidecar {
    /// Delay one `record` call by this much, when `Pair::stall` is set.
    stall: Duration,
    /// Commands to send at most.
    limit: Option<u64>,
    /// Send one control command first.
    control_first: bool,
    /// Panic in the first `record`.
    panic: bool,
}

/// What a test changes from the defaults.
#[derive(Default)]
struct Setup {
    /// Send one control command first.
    control_first: bool,
    /// Agent: records in the control queue.
    control_queue: Option<usize>,
    /// The sidecar's handshake timeout, when not 3 s.
    handshake_timeout: Option<Duration>,
    /// Agent: bytes in the event queue, when not the default.
    event_queue: Option<usize>,
    misbehave: Vec<Misbehavior>,
    /// Delay one `record` call by this much, when `Pair::stall` is set.
    stall: Duration,
    /// The token the sidecar presents, when not the agent's.
    token: Option<&'static str>,
    /// Commands the agent takes per frame, when not the default.
    commands_per_frame: Option<usize>,
    /// Frames per second, when not the default.
    fps: Option<u32>,
}

struct Pair {
    dir: PathBuf,
    cfg: Config,
    stop: Arc<AtomicBool>,
    sidecar_stop: Arc<AtomicBool>,
    agent: Option<JoinHandle<()>>,
    sidecar: Option<JoinHandle<link::End>>,
    reports: Arc<Mutex<Vec<String>>>,
    agent_log: Arc<Mutex<Vec<String>>>,
    stall: Arc<AtomicBool>,
}

/// The folder goes even when a test fails before `finish`.
impl Drop for Pair {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.sidecar_stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Pair {
    fn start(name: &str, misbehave: &[Misbehavior], stall: Duration) -> Pair {
        Pair::with(
            name,
            Setup {
                misbehave: misbehave.to_vec(),
                stall,
                ..Setup::default()
            },
        )
    }

    fn with(name: &str, setup: Setup) -> Pair {
        let dir =
            std::env::temp_dir().join(format!("nanoprobe-misbehave-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a port")
            .port();
        let mut o = Options::new(dir.join("map"), port, TOKEN.into());
        o.misbehave = setup.misbehave.clone();
        if let Some(n) = setup.commands_per_frame {
            o.commands_per_frame = n;
        }
        if let Some(n) = setup.fps {
            o.fps = n;
        }
        if let Some(n) = setup.control_queue {
            o.control_queue = n;
        }
        if let Some(n) = setup.event_queue {
            o.event_queue = n;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let agent_log = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::open(o).expect("open the fake agent");
        let agent = {
            let (stop, log) = (stop.clone(), agent_log.clone());
            std::thread::spawn(move || {
                agent.run(&stop, |l| log.lock().expect("log").push(l.to_owned()))
            })
        };
        let cfg = Config {
            map_path: dir.join("map"),
            handshake: ([127, 0, 0, 1], port).into(),
            token: setup.token.unwrap_or(TOKEN).into(),
            poll: Duration::from_millis(2),
            // Over the agent's 1 s accept cadence and its two 100 ms steps.
            handshake_timeout: setup.handshake_timeout.unwrap_or(Duration::from_secs(3)),
        };
        let mut pair = Pair {
            dir,
            cfg,
            stop,
            sidecar_stop: Arc::new(AtomicBool::new(false)),
            agent: Some(agent),
            sidecar: None,
            reports: Arc::new(Mutex::new(Vec::new())),
            agent_log,
            stall: Arc::new(AtomicBool::new(false)),
        };
        pair.start_sidecar(Sidecar {
            stall: setup.stall,
            control_first: setup.control_first,
            ..Sidecar::default()
        });
        pair
    }

    fn start_sidecar(&mut self, o: Sidecar) {
        let mut sink = Watched {
            stub: Stub::open(&self.dir.join("journal"), false).expect("journal"),
            reports: self.reports.clone(),
            stall: (!o.stall.is_zero()).then(|| (self.stall.clone(), o.stall)),
            limit: o.limit,
            control_first: o.control_first,
            panic: o.panic,
        };
        self.sidecar_stop = Arc::new(AtomicBool::new(false));
        let (cfg, stop) = (self.cfg.clone(), self.sidecar_stop.clone());
        self.sidecar = Some(std::thread::spawn(move || {
            link::run(&cfg, &mut sink, &stop)
        }));
    }

    /// Stop the sidecar as a kill would: it stops polling, with its
    /// connection open until its thread ends.
    fn stop_sidecar(&mut self) {
        self.sidecar_stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.sidecar.take() {
            h.join().expect("the sidecar did not panic");
        }
    }

    fn map(&self) -> MappedFile {
        MappedFile::open(&self.dir.join("map")).expect("map")
    }

    fn attached(&self) -> bool {
        u64::from(self.map().load32(s::FLAGS, Ordering::Acquire)) & flags::ATTACHED != 0
    }

    /// Wait up to `limit` for `f`.
    fn until(&self, limit: Duration, f: impl Fn(&Pair) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn reported(&self, text: &str) -> bool {
        self.reports
            .lock()
            .expect("reports")
            .iter()
            .any(|r| r.contains(text))
    }

    fn agent_said(&self, text: &str) -> bool {
        self.agent_log
            .lock()
            .expect("log")
            .iter()
            .any(|r| r.contains(text))
    }

    fn finish(mut self) -> verify::Report {
        self.stop.store(true, Ordering::Relaxed);
        self.sidecar_stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.agent.take() {
            h.join().expect("the agent did not panic");
        }
        if let Some(h) = self.sidecar.take() {
            h.join().expect("the sidecar did not panic");
        }
        verify::verify(&self.dir.join("journal")).expect("verify")
    }
}

const WAIT: Duration = Duration::from_secs(8);

#[test]
fn a_good_pair_attaches_and_its_journal_verifies() {
    let p = Pair::start("good", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached), "attached");
    std::thread::sleep(Duration::from_millis(1500));
    let r = p.finish();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.records > 100 && r.results > 10, "{r:?}");
}

#[test]
fn a_bad_magic_is_refused() {
    let p = Pair::start("magic", &[Misbehavior::BadMagic], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("header: magic")),
        "{:?}",
        p.reports
    );
    assert!(!p.attached());
    p.finish();
}

#[test]
fn a_header_never_ready_is_refused() {
    let p = Pair::start("ready", &[Misbehavior::NeverReady], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("not ready")),
        "{:?}",
        p.reports
    );
    assert!(!p.attached());
    p.finish();
}

#[test]
fn a_wrong_layout_hash_in_ok_is_refused() {
    let p = Pair::start("hash", &[Misbehavior::WrongOkHash], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("layout hash")),
        "{:?}",
        p.reports
    );
    assert!(!p.attached());
    p.finish();
}

#[test]
fn no_answer_to_hello_times_out_and_is_tried_again() {
    let p = Pair::start("nook", &[Misbehavior::NoOk], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("no answer to HELLO")),
        "{:?}",
        p.reports
    );
    p.finish();
}

#[test]
fn a_connection_closed_after_ok_ends_the_session() {
    let p = Pair::start("close", &[Misbehavior::CloseAfterOk], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("closed the connection")),
        "{:?}",
        p.reports
    );
    assert!(!p.attached());
    p.finish();
}

#[test]
fn a_record_over_the_length_limit_fails_the_check() {
    let p = Pair::start("len", &[Misbehavior::BadLength(50)], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("len over an eighth")),
        "{:?}",
        p.reports
    );
    // The next attach to the same boot skips the bad record instead of
    // meeting it again.
    assert!(
        p.until(WAIT, |p| p.reported("skipped from")),
        "{:?}",
        p.reports
    );
    p.finish();
}

#[test]
fn a_tail_past_the_capacity_fails_the_check() {
    let p = Pair::start("tail", &[Misbehavior::TailPastCapacity(50)], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("outbound ring: tail")),
        "{:?}",
        p.reports
    );
    // The agent puts its own tail back when it detaches, so the next attach
    // to the same boot skips to it.
    assert!(
        p.until(WAIT, |p| p.reported("skipped from")),
        "{:?}",
        p.reports
    );
    p.finish();
}

#[test]
fn a_str_that_is_not_utf8_is_found() {
    let p = Pair::start("utf8", &[Misbehavior::BadUtf8(50)], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    std::thread::sleep(Duration::from_millis(1200));
    let r = p.finish();
    assert!(
        r.errors.iter().any(|e| e.contains("not UTF-8")),
        "{:?}",
        r.errors
    );
}

#[test]
fn an_agent_that_stalls_stays_attached() {
    let p = Pair::start("stall", &[Misbehavior::Stall(50, 1000)], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !p.agent_said("detached"),
        "{:?} {:?}",
        p.agent_log,
        p.reports
    );
    assert!(p.attached());
    let r = p.finish();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

/// The heartbeat runs apart from the journal (spec/transport.md,
/// "Consuming"): a journal that blocks for longer than the agent's 0.5 s
/// limit does not get the sidecar detached.
#[test]
fn a_slow_journal_does_not_detach_the_agent() {
    let p = Pair::start("slow-journal", &[], Duration::from_millis(800));
    assert!(p.until(WAIT, Pair::attached));
    std::thread::sleep(Duration::from_millis(300));
    p.stall.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!p.agent_said("detached"), "{:?}", p.agent_log);
    assert!(p.attached());
    let r = p.finish();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

#[test]
fn an_impossible_in_tail_detaches_the_agent() {
    let p = Pair::start("intail", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    // The stub stores its own in_tail on its next batch of commands, so the
    // bad one is stored again until the agent has read it.
    let m = p.map();
    let seen = p.until(WAIT, |p| {
        let head = m.load(s::IN_HEAD, Ordering::Acquire);
        m.store(s::IN_TAIL, head + (1 << 30), Ordering::Release);
        p.agent_said("inbound ring")
    });
    assert!(seen, "{:?}", p.agent_log);
    p.finish();
}

#[test]
fn a_wrong_token_is_refused() {
    let p = Pair::with(
        "token",
        Setup {
            token: Some("ff"),
            ..Setup::default()
        },
    );
    assert!(
        p.until(WAIT, |p| p.reported("the agent refused: token")),
        "{:?}",
        p.reports
    );
    assert!(!p.attached());
    p.finish();
}

#[test]
fn an_ok_line_naming_another_boot_is_refused() {
    let p = Pair::start("okboot", &[Misbehavior::OkOtherBoot], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.reported("is not the OK line's")),
        "{:?}",
        p.reports
    );
    assert!(!p.attached());
    p.finish();
}

#[test]
fn a_frame_numbered_wrong_fails_the_check() {
    let p = Pair::start("number", &[Misbehavior::FrameNumber(50)], Duration::ZERO);
    assert!(p.until(WAIT, |p| p.reported("expected")), "{:?}", p.reports);
    p.finish();
}

#[test]
fn another_sidecar_in_the_file_ends_the_session() {
    let p = Pair::start("foreign", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    p.map().store(s::SIDECAR_BOOT, 0x5eed, Ordering::Release);
    assert!(
        p.until(WAIT, |p| p.reported("another sidecar holds the file")),
        "{:?}",
        p.reports
    );
    p.finish();
}

/// A new sidecar sets in_tail to in_head, dropping commands the old one left
/// unread. The agent must read on from in_head, even when the new sidecar
/// then writes less than was dropped.
#[test]
fn a_new_sidecar_after_unread_commands_is_not_detached() {
    let mut p = Pair::with(
        "unread",
        // One command a frame at 10 frames a second: the agent cannot take
        // the stub's 16 commands in the 0.5 s before it detaches.
        Setup {
            commands_per_frame: Some(1),
            fps: Some(10),
            ..Setup::default()
        },
    );
    assert!(p.until(WAIT, Pair::attached));
    let m = p.map();
    let unread = |m: &MappedFile| {
        m.load(s::IN_TAIL, Ordering::Acquire) > m.load(s::IN_HEAD, Ordering::Acquire)
    };
    assert!(
        p.until(WAIT, |_| unread(&m)),
        "commands waiting in the ring"
    );
    p.stop_sidecar();
    assert!(
        p.until(WAIT, |p| p.agent_said("detached")),
        "{:?}",
        p.agent_log
    );
    p.start_sidecar(Sidecar {
        limit: Some(1),
        ..Sidecar::default()
    });
    assert!(
        p.until(WAIT, |p| p
            .agent_log
            .lock()
            .expect("log")
            .iter()
            .filter(|l| l.contains("attached to"))
            .count()
            >= 2),
        "{:?}",
        p.agent_log
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(!p.agent_said("inbound ring"), "{:?}", p.agent_log);
    assert!(p.attached());
    let r = p.finish();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

fn journal_text(p: &Pair) -> String {
    let mut names: Vec<_> = std::fs::read_dir(p.dir.join("journal"))
        .expect("journal")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|n| n.to_string_lossy().contains("journal-"))
        .collect();
    names.sort();
    names
        .iter()
        .map(|n| std::fs::read_to_string(n).unwrap_or_default())
        .collect()
}

fn attaches(p: &Pair) -> usize {
    p.agent_log
        .lock()
        .expect("log")
        .iter()
        .filter(|l| l.contains("attached to"))
        .count()
}

fn detaches(p: &Pair) -> usize {
    p.agent_log
        .lock()
        .expect("log")
        .iter()
        .filter(|l| l.contains("detached"))
        .count()
}

#[test]
fn a_sidecar_that_panics_stops_its_heartbeat_and_is_detached() {
    let mut p = Pair::start("panic", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    p.stop_sidecar();
    assert!(p.until(WAIT, |p| detaches(p) >= 1));
    p.start_sidecar(Sidecar {
        panic: true,
        ..Sidecar::default()
    });
    let handle = p.sidecar.take().expect("the sidecar thread");
    assert!(p.until(WAIT, |_| handle.is_finished()), "the session ended");
    assert!(handle.join().is_err(), "the sidecar thread panicked");
    assert!(p.until(WAIT, |p| detaches(p) >= 2), "{:?}", p.agent_log);
    assert!(!p.attached());
    p.finish();
}

#[test]
fn an_impossible_kept_out_head_is_skipped_to_the_tail() {
    let mut p = Pair::start("kept-head", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    p.stop_sidecar();
    assert!(p.until(WAIT, |p| detaches(p) >= 1));
    let m = p.map();
    let tail = m.load(s::OUT_TAIL, Ordering::Acquire);
    m.store(s::OUT_HEAD, tail + 8, Ordering::Release);
    p.start_sidecar(Sidecar::default());
    assert!(
        p.until(WAIT, |p| p.reported("is impossible for out_tail")),
        "{:?}",
        p.reports
    );
    assert!(p.until(WAIT, |p| attaches(p) >= 2), "{:?}", p.agent_log);
    p.finish();
}

#[test]
fn the_sidecar_stays_while_the_file_keeps_the_dead_agents_boot_and_leaves_for_a_new_one() {
    let mut p = Pair::with(
        "gone",
        Setup {
            // Over the agent's accept cadence; the waits below are over twice it.
            handshake_timeout: Some(Duration::from_millis(1500)),
            ..Setup::default()
        },
    );
    assert!(p.until(WAIT, Pair::attached));
    // DCS dies and is not started again: the file keeps the old boot.
    p.stop.store(true, Ordering::Relaxed);
    p.agent
        .take()
        .expect("agent")
        .join()
        .expect("the agent did not panic");
    std::thread::sleep(Duration::from_millis(3500));
    let handle = p.sidecar.take().expect("the sidecar thread");
    assert!(!handle.is_finished(), "the sidecar stays for the old boot");
    // DCS starts again and writes a new boot, but its agent never listens.
    let m = p.map();
    let header = nanoprobe_transport::status::Header {
        interface: 1,
        agent_boot: 0x0dd,
        build: 1,
        created_ms: 1,
        clock_hz: 1,
        out_capacity: 256 << 10,
        in_capacity: 256 << 10,
    };
    nanoprobe_transport::status::start(&m, &header, 1);
    assert!(
        p.until(WAIT, |_| handle.is_finished()),
        "the sidecar leaves"
    );
    assert_eq!(handle.join().expect("no panic"), link::End::Gone);
    p.finish();
}

#[test]
fn an_impossible_out_head_detaches_the_agent() {
    let p = Pair::start("outhead", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    let m = p.map();
    let seen = p.until(WAIT, |p| {
        let tail = m.load(s::OUT_TAIL, Ordering::Acquire);
        m.store(s::OUT_HEAD, tail + 8, Ordering::Release);
        p.agent_said("outbound ring")
    });
    assert!(seen, "{:?}", p.agent_log);
    p.finish();
}

#[test]
fn a_full_control_queue_holds_commands_and_the_journal_stays_whole() {
    let p = Pair::with(
        "held",
        Setup {
            control_queue: Some(1),
            ..Setup::default()
        },
    );
    assert!(p.until(WAIT, Pair::attached));
    let m = p.map();
    let held = p.until(WAIT, |_| {
        u64::from(m.load32(s::FLAGS, Ordering::Acquire)) & flags::COMMANDS_HELD != 0
    });
    assert!(held, "COMMANDS_HELD was seen");
    std::thread::sleep(Duration::from_millis(500));
    let r = p.finish();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.results > 0);
}

#[test]
fn a_lost_control_record_is_followed_by_a_hello() {
    let p = Pair::with(
        "lost",
        Setup {
            control_queue: Some(0),
            ..Setup::default()
        },
    );
    assert!(
        p.until(WAIT, |p| p.agent_said("dropped a control record")),
        "{:?}",
        p.agent_log
    );
    assert!(
        p.until(WAIT, |p| journal_text(p)
            .lines()
            .any(|l| l.starts_with("H "))),
        "a HELLO resyncs"
    );
    p.finish();
}

#[test]
fn a_control_opcode_the_agent_lacks_is_answered_unknown() {
    let p = Pair::with(
        "seq0",
        Setup {
            control_first: true,
            ..Setup::default()
        },
    );
    assert!(p.until(WAIT, Pair::attached));
    std::thread::sleep(Duration::from_millis(500));
    let r = p.finish();
    // The stub journals no S line for it, so the verifier names the result.
    assert!(
        r.errors
            .iter()
            .any(|e| e.contains("(0, 0)") && e.contains("answered 20")),
        "{:?}",
        r.errors
    );
}

#[test]
fn an_oversized_event_is_dropped_and_the_pair_stays_attached() {
    let p = Pair::start("oversize", &[Misbehavior::Oversize(30)], Duration::ZERO);
    assert!(
        p.until(WAIT, |p| p.agent_said("over the payload limit")),
        "{:?}",
        p.agent_log
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(p.attached());
    // The GAP counts a record that has no event number, so the verifier finds
    // one more loss counted than lost; that is the only error.
    let r = p.finish();
    assert!(
        !r.errors.is_empty() && r.errors.iter().all(|e| e.contains("GAPs say 1")),
        "{:?}",
        r.errors
    );
}

#[test]
fn a_lost_control_record_while_attached_is_resynced_by_a_second_hello() {
    let p = Pair::start(
        "lose-control",
        &[Misbehavior::LoseControl(30)],
        Duration::ZERO,
    );
    assert!(
        p.until(WAIT, |p| p.agent_said("dropped a control record")),
        "{:?}",
        p.agent_log
    );
    let hellos = |p: &Pair| {
        journal_text(p)
            .lines()
            .filter(|l| l.starts_with("H "))
            .count()
    };
    assert!(
        p.until(WAIT, |p| hellos(p) >= 2),
        "a resync HELLO in the same attach"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(attaches(&p), 1, "{:?}", p.agent_log);
    let r = p.finish();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

/// A failed inbound check after a padding record leaves the agent's private
/// head past the padding; the next sidecar copies the stored in_head, and the
/// agent must take it.
#[test]
fn an_inbound_failure_after_padding_does_not_refuse_the_next_attach() {
    let mut p = Pair::start("in-padding", &[], Duration::ZERO);
    assert!(p.until(WAIT, Pair::attached));
    let m = p.map();
    let cap = 256u64 << 10;
    let data = 4096 + (256 << 10);
    // The agent takes what the stopped sidecar left within a few frames,
    // well before it detaches at 0.5 s.
    p.stop_sidecar();
    assert!(p.until(Duration::from_millis(300), |_| {
        m.load(s::IN_HEAD, Ordering::Acquire) == m.load(s::IN_TAIL, Ordering::Acquire)
    }));
    let head = m.load(s::IN_HEAD, Ordering::Acquire);
    // Control commands (seq 0) up to the last eighth of the ring, so that a
    // padding record to the end is within the length limit.
    let header = |len: u64, kind: u16| {
        let mut h = [0u8; 8];
        h[..4].copy_from_slice(&(len as u32).to_le_bytes());
        h[4..6].copy_from_slice(&kind.to_le_bytes());
        h
    };
    let mut pos = head;
    while cap - (pos & (cap - 1)) > cap / 8 {
        let rem = cap - (pos & (cap - 1));
        let len = (cap / 8).min(rem - 64) & !7;
        let at = data + (pos & (cap - 1)) as usize;
        m.write(at, &header(len, 2));
        m.zero(at + 8, len as usize);
        pos += 8 + len;
    }
    let off = pos & (cap - 1);
    let pad = cap - off;
    m.write(data + off as usize, &header(pad - 8, 0));
    m.write(data, &header(u64::from(u32::MAX), 49));
    let head = pos;
    m.store(s::IN_TAIL, head + pad + 16, Ordering::Release);
    assert!(
        p.until(WAIT, |p| p.agent_said("record passes")
            || p.agent_said("len over")),
        "{:?}",
        p.agent_log
    );
    let before = attaches(&p);
    p.start_sidecar(Sidecar::default());
    assert!(p.until(WAIT, |p| attaches(p) > before), "{:?}", p.agent_log);
    assert!(!p.agent_said("refused to attach"), "{:?}", p.agent_log);
    p.finish();
}

#[test]
fn a_long_detach_keeps_the_event_queue_within_its_cap() {
    let mut p = Pair::with(
        "detached",
        Setup {
            event_queue: Some(2048),
            ..Setup::default()
        },
    );
    assert!(p.until(WAIT, Pair::attached));
    p.stop_sidecar();
    std::thread::sleep(Duration::from_millis(3000));
    // The agent publishes what its queue holds, counted from the queue.
    let m = p.map();
    let bytes = m.load(s::EVENT_QUEUE_BYTES, Ordering::Acquire);
    assert!(bytes > 0 && bytes <= 2048, "{bytes}");
    assert!(m.load(s::EVENTS_DROPPED, Ordering::Acquire) > 0);
    p.finish();
}
