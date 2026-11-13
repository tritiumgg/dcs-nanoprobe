//! The link: the agent's side of the mapped file and the handshake socket
//! (spec/transport.md), and the delivery rules of spec/records.md.
//!
//! The host drives one frame as:
//!
//! 1. [`Link::begin`]: stamps, the heartbeat check, the frame's one socket
//!    call when one is due, and the attach check. It says whether this was
//!    the frame for system calls, on which the host checks its files and
//!    flushes its log.
//! 2. [`Link::take_command`] for each command the host will run in this
//!    frame, then [`Link::admit`] and [`Link::answer`].
//! 3. [`Link::flush`], which writes what the queue holds, then
//!    [`Link::sample`] for each sample record.
//! 4. [`Link::end`], which publishes the batch and stores the status block.
//!
//! Events, logs and lifecycle changes go in with [`Link::enqueue`] or
//! [`Link::lifecycle`] at any time. Nothing here starts a thread or blocks.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::golden::LAYOUT_HASH;
use nanoprobe_transport::handshake::{Line, LineBuffer, Refusal, VERSION, tokens_equal};
use nanoprobe_transport::layout::{
    Class, flags, gap_class, kind, lifecycle, record_class, result, status as s,
};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::{CommandHeader, CommandResult, Frame, Gap, Hello, Lifecycle};
use nanoprobe_transport::ring::{Consumer, Producer, Ring};
use nanoprobe_transport::status::{self, Header, Metrics};

use crate::config::AgentToml;

/// How the link runs.
#[derive(Clone, Debug)]
pub struct Options {
    /// The mapped file.
    pub map_path: PathBuf,
    /// The loopback port of the handshake.
    pub port: u16,
    /// The token a sidecar must present. Without one the agent does not
    /// listen, and sets `NO_HANDSHAKE`.
    pub token: Option<String>,
    /// Bytes in the outbound ring.
    pub out_capacity: u64,
    /// Bytes in the inbound ring.
    pub in_capacity: u64,
    /// Bytes in the event and log queue.
    pub event_queue: usize,
    /// Records in the control queue.
    pub control_queue: usize,
    /// The install's build, as a number.
    pub build: u64,
    /// The interface version.
    pub interface: u32,
    /// Raw Lua is on.
    pub raw_lua: bool,
    /// The agent's overhead as a share of real time.
    pub overhead_share: f64,
    /// The DCS version, for `HELLO`.
    pub dcs_version: String,
}

impl Options {
    /// Options from `agent.toml` and the token.
    pub fn from_toml(t: &AgentToml, token: Option<String>) -> Options {
        Options {
            map_path: t.map_path(),
            port: t.transport.port,
            token,
            out_capacity: t.transport.out_ring_kib << 10,
            in_capacity: t.transport.in_ring_kib << 10,
            event_queue: (t.transport.event_queue_kib << 10) as usize,
            control_queue: t.transport.control_queue as usize,
            build: 0,
            interface: 0,
            raw_lua: t.lua.raw,
            overhead_share: t.budget.overhead_share,
            dcs_version: String::new(),
        }
    }
}

/// The stamps every record carries (spec/records.md, "Stamps").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamps {
    /// Frame sequence number.
    pub frame: u64,
    /// Model time, seconds.
    pub model: f64,
    /// Real time, seconds.
    pub real: f64,
    /// Mission generation.
    pub mission_gen: u32,
    /// A `lifecycle` code.
    pub lifecycle: u32,
    /// The active campaign.
    pub campaign: u64,
}

/// Ways to break the link on purpose, for the fake agent's tests.
#[cfg(feature = "faults")]
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// Send an `OK` line with a wrong layout hash.
    pub wrong_ok_hash: bool,
    /// Read the `HELLO` and never answer it.
    pub no_ok: bool,
    /// Close the connection right after sending `OK`.
    pub close_after_ok: bool,
    /// Send an `OK` line naming another agent boot.
    pub ok_other_boot: bool,
    /// Number every `FRAME` one too high while set.
    pub frame_number: bool,
    /// Drop the next control record made; cleared when it fires.
    pub lose_control: bool,
}

/// What [`Link::admit`] decided for a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// A control command (`seq` 0): the host runs it, and answers only a
    /// failure.
    Control,
    /// Run the handler. `last_applied_seq` already names the command.
    Run,
    /// Answer with this result code without running the handler.
    Answer(u64),
}

#[derive(Debug)]
struct Item {
    kind: u16,
    class: Class,
    log: bool,
    payload: Vec<u8>,
    stamps: Stamps,
}

#[derive(Debug, Default)]
struct PendingGap {
    count: u32,
    from: Option<Stamps>,
    to: Option<Stamps>,
}

enum Conn {
    None,
    /// To close on the next system-call frame.
    Closing(TcpStream),
    Reading {
        stream: TcpStream,
        buf: LineBuffer,
        since: Instant,
    },
    Replying {
        stream: TcpStream,
        line: Vec<u8>,
        ok: Option<(u64, u64)>,
    },
    Waiting {
        stream: TcpStream,
        since: Instant,
        sidecar_boot: u64,
        attach_id: u64,
    },
    Attached {
        stream: TcpStream,
        sidecar_boot: u64,
    },
}

#[derive(Default)]
struct Counters {
    samples_dropped: u64,
    events_dropped: u64,
    logs_dropped: u64,
    gaps: u64,
    commands_applied: u64,
    commands_over_budget: u64,
}

/// Listen on the handshake port, non-blocking.
fn listen(port: u16) -> Result<TcpListener, String> {
    let l = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("could not listen on 127.0.0.1:{port}: {e}"))?;
    l.set_nonblocking(true)
        .map_err(|e| format!("the handshake socket is not non-blocking: {e}"))?;
    Ok(l)
}

/// Whether the fault `name` is set; always false without the `faults`
/// feature.
macro_rules! fault {
    ($link:expr, $name:ident) => {{
        #[cfg(feature = "faults")]
        let set = $link.faults.$name;
        #[cfg(not(feature = "faults"))]
        let set = false;
        set
    }};
}

/// The bytes of a `FRAME` payload.
const FRAME_LEN: usize = 56;

/// Diagnostic lines kept for the host at most; more are dropped.
const NOTES: usize = 256;

/// The agent's side of the transport.
pub struct Link {
    opts: Options,
    map: Arc<MappedFile>,
    boot: u64,
    start: Instant,
    listener: Option<TcpListener>,
    /// The listen failed and is tried again on each system-call frame, with
    /// the number of tries so far.
    listen_retry: Option<u64>,
    conn: Conn,
    next_syscall: Instant,
    out: Producer,
    inbound: Consumer,
    /// The inbound tail was loaded this frame.
    inbound_fresh: bool,
    frame: u64,
    began: Instant,
    stamps: Stamps,
    /// The stamps of the last record written in this batch.
    last: Option<Stamps>,
    queue: VecDeque<Item>,
    event_bytes: usize,
    control_count: usize,
    gaps: [PendingGap; 2],
    flags: u32,
    hello_due: bool,
    reading_commands: bool,
    /// The ring had no room for the queue this batch.
    blocked: bool,
    disabled: bool,
    /// [`Link::disable`] is writing the last records.
    closing: bool,
    beat: u64,
    beat_at: Instant,
    last_served: u64,
    epoch: u64,
    last_applied_seq: u64,
    counters: Counters,
    metrics: Metrics,
    last_pump: Option<Instant>,
    notes: Vec<String>,
    /// The theater and mission of the last `LIFECYCLE`, for the last one
    /// [`Link::disable`] writes.
    names: (String, String),
    #[cfg(feature = "faults")]
    faults: Faults,
}

impl Link {
    /// Create or open the file, write its header (spec/transport.md,
    /// "Start"), and open the listener.
    pub fn open(opts: Options) -> std::io::Result<Link> {
        let now = Instant::now();
        let header = Header {
            interface: opts.interface,
            agent_boot: nanoprobe_transport::random_nonzero(),
            build: opts.build,
            created_ms: nanoprobe_transport::log::now_ms(),
            clock_hz: 1_000_000_000,
            out_capacity: opts.out_capacity,
            in_capacity: opts.in_capacity,
        };
        let map = Arc::new(MappedFile::create(&opts.map_path, header.file_size())?);
        status::start(&*map, &header, lifecycle::STARTING as u32);
        let mut notes = Vec::new();
        let mut flag_bits = 0;
        if opts.raw_lua {
            flag_bits |= flags::RAW_LUA as u32;
        }
        let mut listen_retry = None;
        let listener = match &opts.token {
            None => {
                notes.push("no token, so no handshake: nothing listens".to_owned());
                None
            }
            Some(_) => match listen(opts.port) {
                Ok(l) => Some(l),
                Err(e) => {
                    // A port the last DCS process held can stay taken for a
                    // while after it dies (LIVE under Wine, 2026-11-13).
                    notes.push(format!("{e}; trying again once a second"));
                    listen_retry = Some(1);
                    None
                }
            },
        };
        if listener.is_none() {
            flag_bits |= flags::NO_HANDSHAKE as u32;
        }
        map.store32(s::FLAGS, flag_bits, Ordering::Release);
        Ok(Link {
            out: Producer::new(Ring::outbound(opts.out_capacity), 0),
            inbound: Consumer::new(Ring::inbound(opts.out_capacity, opts.in_capacity), 0),
            inbound_fresh: false,
            boot: header.agent_boot,
            opts,
            map,
            start: now,
            listener,
            listen_retry,
            conn: Conn::None,
            next_syscall: now,
            frame: 0,
            began: now,
            stamps: Stamps {
                frame: 0,
                model: 0.0,
                real: 0.0,
                mission_gen: 0,
                lifecycle: lifecycle::STARTING as u32,
                campaign: 0,
            },
            last: None,
            queue: VecDeque::new(),
            event_bytes: 0,
            control_count: 0,
            gaps: Default::default(),
            flags: flag_bits,
            hello_due: false,
            reading_commands: false,
            blocked: false,
            disabled: false,
            closing: false,
            beat: 0,
            beat_at: now,
            last_served: 0,
            epoch: 0,
            last_applied_seq: 0,
            counters: Counters::default(),
            metrics: Metrics::default(),
            last_pump: None,
            notes,
            names: (String::new(), String::new()),
            #[cfg(feature = "faults")]
            faults: Faults::default(),
        })
    }

    /// The mapped file.
    pub fn map(&self) -> &Arc<MappedFile> {
        &self.map
    }

    /// The agent's boot.
    pub fn boot(&self) -> u64 {
        self.boot
    }

    /// The mapped file's path.
    pub fn map_path(&self) -> &std::path::Path {
        &self.opts.map_path
    }

    /// Whether a sidecar is attached.
    pub fn attached(&self) -> bool {
        self.flags & flags::ATTACHED as u32 != 0
    }

    /// The `agent_flags` bits.
    pub fn flags(&self) -> u32 {
        self.flags
    }

    /// Whether [`Link::disable`] ran.
    pub fn disabled(&self) -> bool {
        self.disabled
    }

    /// The current stamps.
    pub fn stamps(&self) -> Stamps {
        self.stamps
    }

    /// The command epoch and `last_applied_seq`.
    pub fn watermarks(&self) -> (u64, u64) {
        (self.epoch, self.last_applied_seq)
    }

    /// The outbound producer, as far as this batch got.
    pub fn outbound(&self) -> &Producer {
        &self.out
    }

    /// The faults to inject.
    #[cfg(feature = "faults")]
    pub fn faults(&mut self) -> &mut Faults {
        &mut self.faults
    }

    /// Take the diagnostic lines written since the last call.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn note(&mut self, line: String) {
        if self.notes.len() < NOTES {
            self.notes.push(line);
        }
    }

    /// The agent's clock: nanoseconds since the link opened.
    pub fn clock(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.start).as_nanos() as u64
    }

    /// Start a frame that began at `now`, with DCS's model and real time.
    /// Returns true when this frame made the system call due (spec/transport.md,
    /// "Handshake"): the host checks its files and flushes its log on such a
    /// frame and no other. A disabled link does nothing and returns false.
    pub fn begin(&mut self, now: Instant, model: f64, real: f64) -> bool {
        if self.disabled {
            return false;
        }
        self.began = now;
        self.frame += 1;
        self.stamps.frame = self.frame;
        self.stamps.model = model;
        self.stamps.real = real;
        self.last = None;
        self.inbound_fresh = false;
        let m = &*self.map;
        m.store(s::FRAME, self.frame, Ordering::Relaxed);
        m.store(s::CLOCK, self.clock(now), Ordering::Relaxed);
        m.store(s::MODEL_TIME, model.to_bits(), Ordering::Relaxed);
        m.store(s::REAL_TIME, real.to_bits(), Ordering::Relaxed);
        m.store32(s::LIFECYCLE, self.stamps.lifecycle, Ordering::Relaxed);
        if self.attached() {
            // Timed from when a change is seen, not from the frame's start: a
            // frame that stalled would otherwise date the change too early.
            let seen = Instant::now();
            let beat = self.map.load(s::SIDECAR_HEARTBEAT, Ordering::Acquire);
            if beat != self.beat {
                self.beat = beat;
                self.beat_at = seen;
            } else if seen.duration_since(self.beat_at) >= Duration::from_millis(500) {
                self.detach("the sidecar's heartbeat stopped for 0.5 s");
            }
        }
        let syscall = now >= self.next_syscall;
        if syscall {
            self.syscall(now);
        }
        self.check_attach(now);
        syscall
    }

    /// The frame's one socket call (spec/transport.md, "Handshake"). A
    /// connection to close becomes `Closing` and is closed on the next such
    /// frame, as that frame's call.
    fn syscall(&mut self, now: Instant) {
        let every = Duration::from_millis(100);
        let second = Duration::from_secs(1);
        let conn = std::mem::replace(&mut self.conn, Conn::None);
        self.next_syscall = now + every;
        self.conn = match conn {
            Conn::None if self.listener.is_none() && self.listen_retry.is_some() => {
                // The frame's one call is the listen, tried again.
                self.next_syscall = now + second;
                let tries = self.listen_retry.unwrap_or(0) + 1;
                match listen(self.opts.port) {
                    Ok(l) => {
                        self.listener = Some(l);
                        self.listen_retry = None;
                        self.flags &= !(flags::NO_HANDSHAKE as u32);
                        self.note(format!(
                            "listening on 127.0.0.1:{} after {tries} tries",
                            self.opts.port
                        ));
                    }
                    Err(_) => self.listen_retry = Some(tries),
                }
                Conn::None
            }
            Conn::None => {
                self.next_syscall = now + second;
                match self.listener.as_ref().map(|l| l.accept()) {
                    Some(Ok((stream, _))) => {
                        // The accepted connection is made non-blocking with
                        // the accept; it is not a call on the connection.
                        let _ = stream.set_nonblocking(true);
                        self.next_syscall = now + every;
                        Conn::Reading {
                            stream,
                            buf: LineBuffer::new(),
                            since: now,
                        }
                    }
                    _ => Conn::None,
                }
            }
            Conn::Closing(stream) => {
                drop(stream);
                Conn::None
            }
            Conn::Reading {
                mut stream,
                mut buf,
                since,
            } => {
                let mut chunk = [0u8; 1024];
                let got = match stream.read(&mut chunk) {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.push(&chunk[..n]);
                        Some(())
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => Some(()),
                    Err(_) => None,
                };
                match (got, buf.line()) {
                    (None, _) => Conn::Closing(stream),
                    // A line over the limit is not a well-formed HELLO.
                    (Some(()), Err(_)) => self.answer_hello(stream, b"", now),
                    (Some(()), Ok(Some(line))) => self.answer_hello(stream, &line, now),
                    (Some(()), Ok(None)) if now.duration_since(since) >= Duration::from_secs(2) => {
                        self.note("closed a connection that sent no HELLO within 2 s".into());
                        Conn::Closing(stream)
                    }
                    (Some(()), Ok(None)) => Conn::Reading { stream, buf, since },
                }
            }
            Conn::Replying {
                mut stream,
                mut line,
                ok,
            } => match stream.write(&line) {
                Ok(n) if n == line.len() => match ok {
                    Some(_) if fault!(self, close_after_ok) => Conn::Closing(stream),
                    Some((sidecar_boot, attach_id)) => {
                        // No socket call until the attach timeout, but a
                        // system-call frame each second for the host's files.
                        self.next_syscall = now + second;
                        Conn::Waiting {
                            stream,
                            since: now,
                            sidecar_boot,
                            attach_id,
                        }
                    }
                    None => Conn::Closing(stream),
                },
                Ok(n) => {
                    line.drain(..n);
                    Conn::Replying { stream, line, ok }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => Conn::Replying { stream, line, ok },
                Err(_) => Conn::Closing(stream),
            },
            Conn::Waiting {
                stream,
                since,
                sidecar_boot,
                attach_id,
            } => {
                if now.duration_since(since) >= Duration::from_secs(2) {
                    // The close is this frame's call.
                    self.note("closed a connection that did not attach within 2 s of OK".into());
                    drop(stream);
                    Conn::None
                } else {
                    // No socket call; this frame and the next ones until the
                    // timeout are system-call frames only for the host's
                    // files, once a second (spec/install.md, "Kill switch").
                    self.next_syscall = (since + Duration::from_secs(2)).min(now + second);
                    Conn::Waiting {
                        stream,
                        since,
                        sidecar_boot,
                        attach_id,
                    }
                }
            }
            Conn::Attached {
                mut stream,
                sidecar_boot,
            } => {
                self.next_syscall = now + second;
                let mut b = [0u8; 64];
                let why = match stream.read(&mut b) {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => None,
                    Ok(0) => Some("the sidecar closed the connection"),
                    _ => Some("the sidecar sent bytes after OK, or the connection failed"),
                };
                self.conn = Conn::Attached {
                    stream,
                    sidecar_boot,
                };
                if let Some(why) = why {
                    self.detach(why);
                }
                return;
            }
        };
        if matches!(self.conn, Conn::Closing(_)) {
            // A connection to close is closed on the next frame.
            self.next_syscall = now;
        }
    }

    fn answer_hello(&mut self, stream: TcpStream, line: &[u8], now: Instant) -> Conn {
        if fault!(self, no_ok) {
            return Conn::Reading {
                stream,
                buf: LineBuffer::new(),
                since: now,
            };
        }
        let token = self.opts.token.as_deref().unwrap_or_default();
        let reply = match Line::parse(line) {
            Ok(Line::Hello {
                version,
                token: offered,
                sidecar_boot,
            }) => {
                if version != VERSION {
                    Err(Refusal::Version)
                } else if token.is_empty() || !tokens_equal(offered.as_bytes(), token.as_bytes()) {
                    Err(Refusal::Token)
                } else if sidecar_boot == 0 {
                    Err(Refusal::Version)
                } else {
                    Ok(sidecar_boot)
                }
            }
            _ => Err(Refusal::Version),
        };
        let (text, ok) = match reply {
            Ok(sidecar_boot) => {
                let attach_id = nanoprobe_transport::random_nonzero();
                let l = Line::Ok {
                    layout_hash: if fault!(self, wrong_ok_hash) {
                        LAYOUT_HASH ^ 1
                    } else {
                        LAYOUT_HASH
                    },
                    agent_boot: if fault!(self, ok_other_boot) {
                        self.boot ^ 1
                    } else {
                        self.boot
                    },
                    attach_id,
                    path: self.opts.map_path.display().to_string(),
                };
                (l.encode(), Some((sidecar_boot, attach_id)))
            }
            Err(r) => {
                self.note(format!("refused a HELLO: {}", r.as_str()));
                (Line::Err(r).encode(), None)
            }
        };
        match text {
            Some(t) => Conn::Replying {
                stream,
                line: t.into_bytes(),
                ok,
            },
            None => {
                self.note("an answer that does not fit a line; closing".into());
                Conn::Closing(stream)
            }
        }
    }

    fn check_attach(&mut self, now: Instant) {
        let Conn::Waiting {
            sidecar_boot,
            attach_id,
            ..
        } = self.conn
        else {
            return;
        };
        let m = &*self.map;
        if m.load(s::ATTACH_SEEN, Ordering::Acquire) != attach_id
            || m.load(s::SIDECAR_BOOT, Ordering::Acquire) != sidecar_boot
            || m.load(s::AGENT_BOOT_SEEN, Ordering::Acquire) != self.boot
        {
            return;
        }
        let Conn::Waiting { stream, .. } = std::mem::replace(&mut self.conn, Conn::None) else {
            return;
        };
        // The sidecar set its positions before attach_seen; take them now.
        let mut out = self.out.clone();
        // A refusal closes the connection on the next frame, and the next
        // accept follows 100 ms after (spec/transport.md, "Attaching").
        if let Err(e) = out.refresh(&*self.map) {
            self.note(format!("refused to attach: outbound ring: {e}"));
            self.conn = Conn::Closing(stream);
            self.next_syscall = Instant::now();
            return;
        }
        let in_tail = self.map.load(s::IN_TAIL, Ordering::Acquire);
        if in_tail != self.inbound.head() {
            self.note(format!(
                "refused to attach: in_tail {in_tail:#x} is not in_head {:#x}",
                self.inbound.head()
            ));
            self.conn = Conn::Closing(stream);
            self.next_syscall = Instant::now();
            return;
        }
        self.out = out;
        // The new sidecar set in_tail to in_head, dropping what the old one
        // left unread: read on from there, with no tail cached from before.
        self.inbound = Consumer::new(self.inbound.ring(), in_tail);
        self.inbound_fresh = false;
        if sidecar_boot != self.last_served {
            // A new sidecar: what the old one set (subscriptions, rates) is
            // dropped here once the agent has any.
            self.last_served = sidecar_boot;
        }
        self.map
            .store(s::SIDECAR_BOOT_SEEN, sidecar_boot, Ordering::Release);
        self.flags |= flags::ATTACHED as u32;
        self.beat = self.map.load(s::SIDECAR_HEARTBEAT, Ordering::Acquire);
        // Timed from the read, as in begin (spec/transport.md, "Detaching").
        self.beat_at = Instant::now();
        self.hello_due = true;
        self.reading_commands = false;
        self.conn = Conn::Attached {
            stream,
            sidecar_boot,
        };
        self.next_syscall = now + Duration::from_secs(1);
        self.note(format!("attached to sidecar {sidecar_boot:x}"));
    }

    /// Detach from the sidecar (spec/transport.md, "Detaching"). The
    /// connection closes on the next frame, as that frame's one system call.
    pub fn detach(&mut self, why: &str) {
        self.conn = match std::mem::replace(&mut self.conn, Conn::None) {
            Conn::Attached { stream, .. } | Conn::Waiting { stream, .. } => Conn::Closing(stream),
            other => other,
        };
        self.next_syscall = Instant::now();
        // Read on from the in_head last stored, which is what the next
        // sidecar copies to in_tail; the private head may be past padding
        // before a record that failed a check.
        let in_head = self.map.load(s::IN_HEAD, Ordering::Relaxed);
        self.inbound = Consumer::new(self.inbound.ring(), in_head);
        self.inbound_fresh = false;
        self.map.store(s::SIDECAR_BOOT_SEEN, 0, Ordering::Release);
        self.flags &= !(flags::ATTACHED as u32);
        self.hello_due = false;
        self.reading_commands = false;
        self.note(format!("detached: {why}"));
    }

    /// Start a new mission generation: one more than the last. Returns it.
    pub fn new_generation(&mut self) -> u32 {
        self.stamps.mission_gen = self.stamps.mission_gen.wrapping_add(1);
        self.map
            .store32(s::MISSION_GEN, self.stamps.mission_gen, Ordering::Relaxed);
        self.stamps.mission_gen
    }

    /// Set the lifecycle state, store it in the status block, and queue a
    /// `LIFECYCLE` record.
    pub fn lifecycle(&mut self, state: u32, theater: &str, mission: &str) {
        if self.disabled {
            return;
        }
        self.stamps.lifecycle = state;
        self.map.store32(s::LIFECYCLE, state, Ordering::Relaxed);
        self.names = (theater.into(), mission.into());
        let l = Lifecycle {
            state,
            mission_gen: self.stamps.mission_gen,
            theater: theater.into(),
            mission: mission.into(),
        };
        self.enqueue(kind::LIFECYCLE as u16, l.encode());
    }

    /// Take the real and model time of a callback outside a frame, such as
    /// `onMissionLoadBegin`, for the records it makes. The frame number stays
    /// the last frame's (spec/records.md, "Stamps").
    pub fn set_time(&mut self, model: f64, real: f64) {
        self.stamps.model = model;
        self.stamps.real = real;
    }

    /// Publish the queue outside a frame, as `onMissionLoadBegin` and
    /// `onSimulationStop` do (spec/transport.md, "Producing"): when attached
    /// and no `HELLO` is due, write what the queue holds, in order, as far as
    /// the ring has room. What does not fit waits for the next frame.
    pub fn publish_now(&mut self) {
        // A HELLO, including the one that resyncs after a lost control
        // record, is written only on a frame.
        if self.disabled
            || !self.attached()
            || self.hello_due
            || self.flags & flags::CONTROL_LOST as u32 != 0
        {
            return;
        }
        self.last = None;
        self.flush();
        if self.attached() {
            self.out.publish(&*self.map);
        }
        self.store_counters();
    }

    /// Set the active campaign, for the stamps of later records.
    pub fn set_campaign(&mut self, campaign: u64) {
        self.stamps.campaign = campaign;
        self.map.store(s::CAMPAIGN, campaign, Ordering::Relaxed);
    }

    /// Queue an event-class or control-class record, by the delivery rules
    /// of spec/records.md. A sample-class record is not queued.
    pub fn enqueue(&mut self, kind: u16, payload: Vec<u8>) {
        if self.disabled {
            return;
        }
        let class = record_class(kind.into()).unwrap_or(Class::Event);
        let log = u64::from(kind) == nanoprobe_transport::layout::kind::LOG;
        let item = Item {
            kind,
            class,
            log,
            payload,
            stamps: self.stamps,
        };
        if item.payload.len() > self.out.ring().max_payload() {
            // Of spec/records.md, "Oversized records", the link keeps only the
            // count in the pending GAP of events. The host makes the
            // replacement records it can make before calling this.
            self.note(format!("a record of kind {kind} over the payload limit"));
            let event = Item { log: false, ..item };
            self.lose(&event);
            return;
        }
        match class {
            Class::Control => {
                #[cfg(feature = "faults")]
                let lose = std::mem::take(&mut self.faults.lose_control);
                #[cfg(not(feature = "faults"))]
                let lose = false;
                if lose || (self.control_count >= self.opts.control_queue && !self.closing) {
                    // No more commands until the HELLO that resyncs
                    // (spec/records.md, "Delivery classes").
                    self.flags |= flags::CONTROL_LOST as u32;
                    self.reading_commands = false;
                    self.note(format!("dropped a control record of kind {kind}"));
                    return;
                }
                self.control_count += 1;
            }
            Class::Event => {
                let len = item.payload.len();
                while self.event_bytes + len > self.opts.event_queue {
                    if !self.drop_oldest(true) && !self.drop_oldest(false) {
                        break;
                    }
                }
                if self.event_bytes + len > self.opts.event_queue {
                    self.lose(&item);
                    return;
                }
                self.event_bytes += len;
            }
            Class::Sample => return,
        }
        self.queue.push_back(item);
    }

    fn drop_oldest(&mut self, logs: bool) -> bool {
        let Some(i) = self
            .queue
            .iter()
            .position(|it| it.class == Class::Event && it.log == logs)
        else {
            return false;
        };
        let Some(item) = self.queue.remove(i) else {
            return false;
        };
        self.event_bytes -= item.payload.len();
        self.lose(&item);
        true
    }

    fn lose(&mut self, item: &Item) {
        let (g, counter) = if item.log {
            (&mut self.gaps[1], &mut self.counters.logs_dropped)
        } else {
            (&mut self.gaps[0], &mut self.counters.events_dropped)
        };
        *counter += 1;
        g.count += 1;
        g.from.get_or_insert(item.stamps);
        g.to = Some(item.stamps);
    }

    /// Take the next command, its opcode and payload, if the agent reads
    /// commands now and the control queue has room for its result. `in_head`
    /// is stored past it before this returns, so it counts as taken whether
    /// or not it then runs (spec/transport.md, "Consuming").
    pub fn take_command(&mut self, payload: &mut Vec<u8>) -> Option<u16> {
        if self.disabled || !self.attached() || !self.reading_commands {
            return None;
        }
        if !self.inbound_fresh {
            if let Err(e) = self.inbound.refresh(&*self.map) {
                self.detach(&format!("inbound ring: {e}"));
                return None;
            }
            self.inbound_fresh = true;
        }
        if self.control_count >= self.opts.control_queue {
            self.flags |= flags::COMMANDS_HELD as u32;
            return None;
        }
        self.flags &= !(flags::COMMANDS_HELD as u32);
        let r = match self.inbound.next(&*self.map, payload) {
            Ok(Some(r)) => r,
            Ok(None) => return None,
            Err(e) => {
                self.detach(&format!("inbound ring: {e}"));
                return None;
            }
        };
        // Taken: in_head moves past it before it runs.
        self.map.store(s::IN_HEAD, r.next, Ordering::Release);
        self.counters.commands_applied += 1;
        Some(r.kind)
    }

    /// The rows of spec/commands.md that need no opcode table: the epoch,
    /// the duplicate and the campaign. The host applies the rows that depend
    /// on the opcode after a [`Admit::Run`].
    pub fn admit(&mut self, h: &CommandHeader) -> Admit {
        if h.seq == 0 {
            return Admit::Control;
        }
        if h.epoch < self.epoch {
            return Admit::Answer(result::STALE_EPOCH);
        }
        if h.epoch > self.epoch {
            self.epoch = h.epoch;
            self.last_applied_seq = 0;
        }
        if h.seq <= self.last_applied_seq {
            return Admit::Answer(result::DUPLICATE);
        }
        self.last_applied_seq = h.seq;
        if h.campaign != self.stamps.campaign {
            return Admit::Answer(result::WRONG_CAMPAIGN);
        }
        Admit::Run
    }

    /// Queue the `RESULT` of a command.
    #[allow(clippy::too_many_arguments, reason = "one argument per RESULT field")]
    pub fn answer(
        &mut self,
        opcode: u16,
        h: &CommandHeader,
        code: u64,
        object: u64,
        cost_us: u32,
        message: &str,
        value: Lv,
    ) {
        let r = CommandResult {
            seq: h.seq,
            epoch: h.epoch,
            campaign: h.campaign,
            entity: h.entity,
            rev: h.rev,
            object,
            cost_us,
            opcode,
            code: code as u16,
            message: message.into(),
            value,
        };
        self.enqueue(kind::RESULT as u16, r.encode());
    }

    /// Count a command's time in `hist_command`, and in
    /// `commands_over_budget` when it passed the commanded-work budget.
    pub fn record_command(&mut self, cost_us: f64, over_budget: bool) {
        self.metrics.hist[2][status::bucket(cost_us)] += 1;
        if over_budget {
            self.counters.commands_over_budget += 1;
        }
    }

    /// Count the agent's time for one world event in `hist_event`.
    pub fn record_event(&mut self, cost_us: f64) {
        self.metrics.hist[3][status::bucket(cost_us)] += 1;
    }

    /// Write what the ring has room for: pending gaps, the queue in order,
    /// and the `HELLO` once the queue is empty. Loads and checks the head
    /// first, once per batch (spec/transport.md, "Producing").
    pub fn flush(&mut self) {
        if self.disabled || !self.attached() {
            return;
        }
        if let Err(e) = self.out.refresh(&*self.map) {
            self.detach(&format!("outbound ring: {e}"));
            return;
        }
        self.write_queued();
    }

    /// Write one sample record after [`Link::flush`]. A sample the ring has
    /// no room for, or that would pass records still queued, is dropped and
    /// counted. The agent samples only once it has written its `HELLO`.
    pub fn sample(&mut self, kind: u16, payload: &[u8]) -> bool {
        if self.disabled || !self.attached() || !self.reading_commands {
            return false;
        }
        if self.blocked || !self.put(self.stamps, kind, payload) {
            self.counters.samples_dropped += 1;
            return false;
        }
        true
    }

    /// Write any record with this frame's stamps, whatever its kind, for a
    /// test that needs a record the link would not make.
    #[cfg(feature = "faults")]
    pub fn put_raw(&mut self, kind: u16, payload: &[u8]) -> bool {
        self.put(self.stamps, kind, payload)
    }

    /// Publish the frame's batch and store the status block.
    pub fn end(&mut self) {
        if self.disabled {
            return;
        }
        if self.attached() {
            self.out.publish(&*self.map);
        }
        self.store_status();
    }

    /// The kill switch, or a caught panic: store `DISABLED`, publish a last
    /// `LIFECYCLE` if attached, close the connection and the listener, and
    /// do nothing more in this process.
    pub fn disable(&mut self, why: &str) {
        if self.disabled {
            return;
        }
        let reason = nanoprobe_transport::records::Log {
            level: 2,
            target: "agent".into(),
            msg: format!("disabled: {why}"),
            data: Lv::Nil,
        };
        // The LIFECYCLE is the last record: no HELLO after it, even one owed
        // for a lost control record, whose flag stays set; it passes the
        // control queue's limit; and no command is read.
        self.closing = true;
        self.hello_due = false;
        self.reading_commands = false;
        self.enqueue(kind::LOG as u16, reason.encode());
        let (theater, mission) = self.names.clone();
        self.lifecycle(lifecycle::DISABLED as u32, &theater, &mission);
        if self.attached() {
            self.last = None;
            self.flush();
            if self.attached() {
                self.out.publish(&*self.map);
            }
            if !self.queue.is_empty() {
                self.note(format!(
                    "{} records, the last LIFECYCLE among them, did not fit the ring",
                    self.queue.len()
                ));
            }
        }
        if self.attached() {
            self.flags &= !(flags::ATTACHED as u32);
            self.map.store(s::SIDECAR_BOOT_SEEN, 0, Ordering::Release);
        }
        self.conn = Conn::None;
        self.listener = None;
        self.listen_retry = None;
        self.store_counters();
        self.map
            .store32(s::LIFECYCLE, lifecycle::DISABLED as u32, Ordering::Release);
        self.disabled = true;
        self.note(format!("disabled: {why}"));
    }

    /// The pending gaps, the queue in order, and the `HELLO` once the queue
    /// is empty. Sets `blocked` when the ring had no room.
    fn write_queued(&mut self) {
        self.blocked = true;
        for class in [gap_class::EVENTS, gap_class::LOGS] {
            let g = &self.gaps[class as usize - 1];
            if g.count == 0 {
                continue;
            }
            let (Some(from), Some(to)) = (g.from, g.to) else {
                continue;
            };
            let gap = Gap {
                class: class as u32,
                count: g.count,
                from_real: from.real,
                to_real: to.real,
                from_model: from.model,
                to_model: to.model,
                from_gen: from.mission_gen,
                to_gen: to.mission_gen,
            };
            // A record the queue dropped is older than every queued one; an
            // oversized record may be newer, which spec/records.md allows (a
            // GAP's span may hold records that were kept). The GAP goes
            // first, under the stamps of the last record it counts.
            if !self.put(to, kind::GAP as u16, &gap.encode()) {
                return;
            }
            self.gaps[class as usize - 1] = PendingGap::default();
            self.counters.gaps += 1;
        }
        while let Some(item) = self.queue.front_mut() {
            let (k, stamps) = (item.kind, item.stamps);
            let payload = std::mem::take(&mut item.payload);
            if !self.put(stamps, k, &payload) {
                if let Some(item) = self.queue.front_mut() {
                    item.payload = payload;
                }
                return;
            }
            if let Some(item) = self.queue.pop_front() {
                match item.class {
                    Class::Control => self.control_count -= 1,
                    _ => self.event_bytes -= payload.len(),
                }
            }
        }
        if self.flags & flags::CONTROL_LOST as u32 != 0 && !self.closing {
            self.hello_due = true;
        }
        if self.hello_due {
            let Conn::Attached { sidecar_boot, .. } = self.conn else {
                return;
            };
            let hello = Hello {
                agent_boot: self.boot,
                sidecar_boot,
                build: self.opts.build,
                campaign: self.stamps.campaign,
                epoch: self.epoch,
                last_applied_seq: self.last_applied_seq,
                in_head: self.inbound.head(),
                interface: self.opts.interface,
                flags: self.flags & !(flags::CONTROL_LOST as u32),
                dcs_version: self.opts.dcs_version.clone(),
            };
            if !self.put(self.stamps, kind::HELLO as u16, &hello.encode()) {
                return;
            }
            self.flags &= !(flags::CONTROL_LOST as u32);
            self.hello_due = false;
            self.reading_commands = true;
        }
        self.blocked = false;
    }

    /// Write one record, after a `FRAME` when its stamps are not the last
    /// ones written. The `FRAME` and the record go in together or not at all.
    fn put(&mut self, stamps: Stamps, kind: u16, payload: &[u8]) -> bool {
        let saved = self.out.clone();
        if self.last != Some(stamps) {
            let own = self.out.records() + self.out.records_for(FRAME_LEN);
            let f = Frame {
                frame: stamps.frame,
                model_time: stamps.model,
                real_time: stamps.real,
                campaign: stamps.campaign,
                number: own + 1 + u64::from(fault!(self, frame_number)),
                mission_gen: stamps.mission_gen,
                lifecycle: stamps.lifecycle,
                replay: u32::from(stamps.frame != self.frame),
            };
            match self
                .out
                .push(&*self.map, kind::FRAME as u16, &[&f.encode()])
            {
                Ok(true) => {}
                Ok(false) => return false,
                Err(e) => {
                    self.detach(&format!("outbound ring: {e}"));
                    return false;
                }
            }
        }
        match self.out.push(&*self.map, kind, &[payload]) {
            Ok(true) => {
                self.last = Some(stamps);
                true
            }
            Ok(false) => {
                self.out = saved;
                false
            }
            Err(e) => {
                self.out = saved;
                self.note(format!("a record of kind {kind} not written: {e}"));
                false
            }
        }
    }

    fn store_counters(&self) {
        let m = &*self.map;
        m.store(s::EPOCH, self.epoch, Ordering::Relaxed);
        m.store(
            s::LAST_APPLIED_SEQ,
            self.last_applied_seq,
            Ordering::Relaxed,
        );
        m.store32(s::FLAGS, self.flags, Ordering::Release);
        // What the queue holds, counted from the queue itself rather than
        // from the counters the caps are enforced with.
        let (event_bytes, control) = self.queue.iter().fold((0, 0), |(b, c), it| match it.class {
            Class::Control => (b, c + 1),
            _ => (b + it.payload.len(), c),
        });
        debug_assert_eq!(
            (event_bytes, control),
            (self.event_bytes, self.control_count)
        );
        m.store32(s::CONTROL_QUEUED, control as u32, Ordering::Relaxed);
        m.store(
            s::SAMPLES_DROPPED,
            self.counters.samples_dropped,
            Ordering::Relaxed,
        );
        m.store(
            s::EVENTS_DROPPED,
            self.counters.events_dropped,
            Ordering::Relaxed,
        );
        m.store(
            s::LOGS_DROPPED,
            self.counters.logs_dropped,
            Ordering::Relaxed,
        );
        m.store(s::GAPS, self.counters.gaps, Ordering::Relaxed);
        m.store(
            s::COMMANDS_APPLIED,
            self.counters.commands_applied,
            Ordering::Relaxed,
        );
        m.store(s::EVENT_QUEUE_BYTES, event_bytes as u64, Ordering::Relaxed);
        m.store(
            s::COMMANDS_OVER_BUDGET,
            self.counters.commands_over_budget,
            Ordering::Relaxed,
        );
    }

    fn store_status(&mut self) {
        self.store_counters();
        let began = self.began;
        let pump_us = began.elapsed().as_secs_f64() * 1e6;
        self.metrics.hist[0][status::bucket(pump_us)] += 1;
        if let Some(prev) = self.last_pump {
            let interval = began.duration_since(prev).as_secs_f64() * 1e6;
            self.metrics.hist[1][status::bucket(interval)] += 1;
            let avg = &mut self.metrics.frame_interval_us;
            *avg = if *avg == 0.0 {
                interval
            } else {
                *avg * 0.95 + interval * 0.05
            };
        }
        self.metrics.slice_us =
            (self.opts.overhead_share * self.metrics.frame_interval_us).min(250.0);
        self.last_pump = Some(began);
        status::write_metrics(&*self.map, &self.metrics);
    }

    /// The event queue's bytes and the control queue's records.
    pub fn queued(&self) -> (usize, usize) {
        (self.event_bytes, self.control_count)
    }

    /// The event queue's cap and the control queue's.
    pub fn queue_caps(&self) -> (usize, usize) {
        (self.opts.event_queue, self.opts.control_queue)
    }

    /// Whether the agent reads commands: attached, its `HELLO` written, and
    /// no control record lost since.
    pub fn reading_commands(&self) -> bool {
        self.reading_commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead as _;

    #[test]
    fn frame_len_is_the_encoded_length() {
        assert_eq!(Frame::default().encode().len(), FRAME_LEN);
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A link on a free port in its own folder, and the folder.
    fn open(name: &str) -> (Link, PathBuf) {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        open_on(name, port)
    }

    /// A link on `port` in its own folder, and the folder.
    fn open_on(name: &str, port: u16) -> (Link, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "nanoprobe-link-{name}-{}",
            nanoprobe_transport::random_nonzero()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let link = Link::open(Options {
            map_path: dir.join("map"),
            port,
            token: Some(TOKEN.into()),
            out_capacity: 256 << 10,
            in_capacity: 256 << 10,
            event_queue: 64 << 10,
            control_queue: 64,
            build: 1,
            interface: 2,
            raw_lua: false,
            overhead_share: 0.003,
            dcs_version: "test".into(),
        })
        .unwrap();
        (link, dir)
    }

    /// Pump frames every 10 ms until `done`, at most `limit`. Returns the
    /// instants of the frames that made the system calls.
    fn pump(
        link: &mut Link,
        limit: Duration,
        mut done: impl FnMut(&mut Link) -> bool,
    ) -> Vec<Instant> {
        let start = Instant::now();
        let mut syscalls = Vec::new();
        while start.elapsed() < limit {
            let now = Instant::now();
            if link.begin(now, 0.0, 0.0) {
                syscalls.push(now);
            }
            link.flush();
            link.end();
            if done(link) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        syscalls
    }

    /// Connect, send a HELLO, and pump until the OK line arrives.
    fn hello(link: &mut Link) -> (TcpStream, u64, u64) {
        let port = link.opts.port;
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let sidecar_boot = 0x5eed;
        let line = Line::Hello {
            version: VERSION,
            token: TOKEN.into(),
            sidecar_boot,
        }
        .encode()
        .unwrap();
        (&stream).write_all(line.as_bytes()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut text = String::new();
        pump(link, Duration::from_secs(5), |_| {
            reader.read_line(&mut text).is_ok() && text.ends_with('\n')
        });
        match Line::parse(text.trim_end().as_bytes()) {
            Ok(Line::Ok { attach_id, .. }) => (stream, sidecar_boot, attach_id),
            other => panic!("no OK: {other:?} from {text:?}"),
        }
    }

    #[test]
    fn a_connection_waiting_to_attach_keeps_a_system_call_frame_each_second() {
        let (mut link, dir) = open("waiting");
        let (stream, _, _) = hello(&mut link);
        let since = Instant::now();
        let syscalls = pump(&mut link, Duration::from_millis(2600), |_| false);
        let gaps: Vec<_> = std::iter::once(since)
            .chain(syscalls.iter().copied())
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| w[1].duration_since(w[0]))
            .collect();
        assert!(
            gaps.iter().all(|g| *g <= Duration::from_millis(1050)),
            "system-call frames {gaps:?} apart"
        );
        // The attach timeout closed the connection.
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut b = [0u8; 8];
        assert_eq!((&stream).read(&mut b).unwrap_or(0), 0, "still open");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(feature = "faults")]
    #[test]
    fn publish_now_writes_no_hello_after_a_lost_control_record() {
        let (mut link, dir) = open("lost");
        let (_stream, sidecar_boot, attach_id) = hello(&mut link);
        let boot = link.boot();
        let m = link.map().clone();
        m.store(s::SIDECAR_BOOT, sidecar_boot, Ordering::Release);
        m.store(s::AGENT_BOOT_SEEN, boot, Ordering::Release);
        m.store(s::ATTACH_SEEN, attach_id, Ordering::Release);
        // The heartbeat moves on every frame, as a live sidecar's does.
        pump(&mut link, Duration::from_secs(2), |l| {
            let beat = l.map().load(s::SIDECAR_HEARTBEAT, Ordering::Relaxed);
            l.map()
                .store(s::SIDECAR_HEARTBEAT, beat + 1, Ordering::Release);
            l.reading_commands()
        });
        assert!(link.reading_commands(), "the HELLO was not written");
        link.faults().lose_control = true;
        link.lifecycle(lifecycle::STOPPING as u32, "", "");
        assert_ne!(link.flags() & flags::CONTROL_LOST as u32, 0);
        let tail = m.load(s::OUT_TAIL, Ordering::Acquire);
        link.publish_now();
        assert_eq!(
            m.load(s::OUT_TAIL, Ordering::Acquire),
            tail,
            "wrote outside a frame"
        );
        assert!(!link.reading_commands());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Each event's time is counted in `hist_event` and in no other histogram.
    #[test]
    fn an_event_s_time_is_counted_in_hist_event() {
        let (mut link, dir) = open("hist-event");
        link.begin(Instant::now(), 0.0, 0.0);
        link.record_event(3.0);
        link.record_event(3.0);
        link.end();
        let m = status::read_metrics(&**link.map()).expect("metrics");
        assert_eq!(m.hist[3].iter().sum::<u64>(), 2);
        assert_eq!(m.hist[3][status::bucket(3.0)], 2);
        assert_eq!(m.hist[2].iter().sum::<u64>(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A port another socket holds is tried again once a second, and the
    /// link listens once it is free.
    #[test]
    fn a_taken_port_is_tried_again_until_it_is_free() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let (mut link, dir) = open_on("retry", port);
        let no_handshake = |l: &Link| l.flags() & flags::NO_HANDSHAKE as u32 != 0;
        assert!(no_handshake(&link));
        assert!(
            link.take_notes()
                .iter()
                .any(|n| n.contains("trying again once a second")),
        );
        pump(&mut link, Duration::from_millis(1500), |_| false);
        assert!(no_handshake(&link), "listening while the port is held");
        drop(held);
        pump(&mut link, Duration::from_secs(3), |l| !no_handshake(l));
        assert!(!no_handshake(&link), "not listening once the port is free");
        assert!(
            link.take_notes()
                .iter()
                .any(|n| n.starts_with(&format!("listening on 127.0.0.1:{port} after"))),
        );
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("the link listens");
        let _ = std::fs::remove_dir_all(dir);
    }
}
