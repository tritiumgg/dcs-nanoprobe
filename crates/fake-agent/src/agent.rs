//! The fake agent: the agent's side of the transport, in plain Rust, with a
//! frame pump driven by a timer instead of DCS (spec/transport.md).
//!
//! It keeps the agent's rules that the transport needs: one socket call on
//! few frames, attaching on the attach ID, the two queues with their `GAP`s,
//! replayed stamps, `HELLO` as a barrier, `in_head` stored before a command
//! runs, and detaching on a stalled heartbeat. Its records carry values the
//! stub can check (see [`crate::content`]). [`Misbehavior`]s break it on
//! purpose.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nanoprobe_transport::codec::{Lv, Reader};
use nanoprobe_transport::golden::LAYOUT_HASH;
use nanoprobe_transport::handshake::{Line, LineBuffer, Refusal, VERSION, tokens_equal};
use nanoprobe_transport::layout::{
    Class, flags, gap_class, kind, lifecycle, record_class, result, status as s,
};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::{CommandHeader, CommandResult, Frame, Gap, Hello, Lifecycle};
use nanoprobe_transport::ring::{Consumer, Producer, Ring, record_size};
use nanoprobe_transport::status::{self, Header, Metrics};

use crate::content;

/// A way to break the fake agent on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Misbehavior {
    /// Write a wrong `magic` after the header.
    BadMagic,
    /// Leave `ready` at 0.
    NeverReady,
    /// Send an `OK` line with a wrong layout hash.
    WrongOkHash,
    /// Read the `HELLO` and never answer it.
    NoOk,
    /// Close the connection right after sending `OK`.
    CloseAfterOk,
    /// Send an `OK` line naming another agent boot than the file's.
    OkOtherBoot,
    /// On this attached frame, write a `FRAME` whose number is one too high.
    FrameNumber(u64),
    /// On this attached frame, make an event over the payload limit.
    Oversize(u64),
    /// On this attached frame, drop the next control record made.
    LoseControl(u64),
    /// On this attached frame, detach right after publishing.
    DetachAfterPublish(u64),
    /// On this attached frame, publish a record whose `len` is over the limit.
    BadLength(u64),
    /// On this attached frame, publish a tail more than a capacity past the
    /// head.
    TailPastCapacity(u64),
    /// On this attached frame, write a `LOG` whose `str` is not UTF-8.
    BadUtf8(u64),
    /// On this attached frame, stop for this many milliseconds.
    Stall(u64, u64),
}

impl Misbehavior {
    /// Parse `bad-magic`, `never-ready`, `wrong-ok-hash`, `no-ok`,
    /// `close-after-ok`, `ok-other-boot`, `frame-number@F`, `oversize@F`, `lose-control@F`,
    /// `detach-after-publish@F`, `bad-length@F`, `tail-past@F`, `bad-utf8@F`,
    /// `stall@F:MS`.
    pub fn parse(text: &str) -> Option<Misbehavior> {
        let (name, at) = text.split_once('@').unwrap_or((text, ""));
        let frame = || at.parse().ok();
        Some(match name {
            "bad-magic" => Misbehavior::BadMagic,
            "never-ready" => Misbehavior::NeverReady,
            "wrong-ok-hash" => Misbehavior::WrongOkHash,
            "no-ok" => Misbehavior::NoOk,
            "close-after-ok" => Misbehavior::CloseAfterOk,
            "ok-other-boot" => Misbehavior::OkOtherBoot,
            "frame-number" => Misbehavior::FrameNumber(frame()?),
            "oversize" => Misbehavior::Oversize(frame()?),
            "lose-control" => Misbehavior::LoseControl(frame()?),
            "detach-after-publish" => Misbehavior::DetachAfterPublish(frame()?),
            "bad-length" => Misbehavior::BadLength(frame()?),
            "tail-past" => Misbehavior::TailPastCapacity(frame()?),
            "bad-utf8" => Misbehavior::BadUtf8(frame()?),
            "stall" => {
                let (f, ms) = at.split_once(':')?;
                Misbehavior::Stall(f.parse().ok()?, ms.parse().ok()?)
            }
            _ => return None,
        })
    }
}

/// How the fake agent runs.
#[derive(Clone, Debug)]
pub struct Options {
    /// The mapped file.
    pub map_path: PathBuf,
    /// The loopback port of the handshake.
    pub port: u16,
    /// The token the sidecar must present.
    pub token: String,
    /// Frames per second.
    pub fps: u32,
    /// Bytes in the outbound ring.
    pub out_capacity: u64,
    /// Bytes in the inbound ring.
    pub in_capacity: u64,
    /// Bytes in the event and log queue.
    pub event_queue: usize,
    /// Records in the control queue.
    pub control_queue: usize,
    /// Commands taken per frame at most.
    pub commands_per_frame: usize,
    /// An event every this many frames.
    pub event_every: u64,
    /// A log record every this many frames.
    pub log_every: u64,
    /// Ways to misbehave.
    pub misbehave: Vec<Misbehavior>,
}

impl Options {
    /// Defaults for `map_path`, `port` and `token`.
    pub fn new(map_path: PathBuf, port: u16, token: String) -> Options {
        Options {
            map_path,
            port,
            token,
            fps: 100,
            out_capacity: 256 << 10,
            in_capacity: 256 << 10,
            event_queue: 64 << 10,
            control_queue: 64,
            commands_per_frame: 4,
            event_every: 3,
            log_every: 7,
            misbehave: Vec::new(),
        }
    }
}

/// The interface version the fake agent reports.
pub const INTERFACE: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Stamps {
    frame: u64,
    model: f64,
    real: f64,
    mission_gen: u32,
    lifecycle: u32,
    campaign: u64,
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
        since: Instant,
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

/// The fake agent.
pub struct Agent {
    opts: Options,
    map: MappedFile,
    boot: u64,
    start: Instant,
    listener: Option<TcpListener>,
    conn: Conn,
    next_syscall: Instant,
    out: Producer,
    inbound: Consumer,
    frame: u64,
    /// Frames pumped while attached: when a frame-numbered misbehavior fires.
    attached_frames: u64,
    stamps: Stamps,
    queue: VecDeque<Item>,
    event_bytes: usize,
    control_count: usize,
    gaps: [PendingGap; 2],
    flags: u32,
    hello_due: bool,
    reading_commands: bool,
    /// The ring had no room for the queue this frame.
    blocked: bool,
    /// Drop the next control record (the `lose-control` misbehavior).
    lose_control: bool,
    beat: u64,
    beat_at: Instant,
    last_served: u64,
    epoch: u64,
    last_applied_seq: u64,
    event_seq: u64,
    log_seq: u64,
    counters: Counters,
    metrics: Metrics,
    last_pump: Option<Instant>,
    payload: Vec<u8>,
    log: Vec<String>,
}

#[derive(Default)]
struct Counters {
    samples_dropped: u64,
    events_dropped: u64,
    logs_dropped: u64,
    gaps: u64,
    commands_applied: u64,
}

impl Agent {
    /// Open the file and the listener, and write the header (spec/transport.md,
    /// "Start").
    pub fn open(opts: Options) -> std::io::Result<Agent> {
        let header = Header {
            interface: INTERFACE,
            agent_boot: nanoprobe_transport::random_nonzero(),
            build: 0xfa4e,
            created_ms: content::now_ms(),
            clock_hz: 1_000_000_000,
            out_capacity: opts.out_capacity,
            in_capacity: opts.in_capacity,
        };
        let map = MappedFile::create(&opts.map_path, header.file_size())?;
        status::start(&map, &header, lifecycle::STARTING as u32);
        if opts.misbehave.contains(&Misbehavior::BadMagic) {
            map.store(s::MAGIC, 0xbad, Ordering::Relaxed);
        }
        if opts.misbehave.contains(&Misbehavior::NeverReady) {
            map.store(s::READY, 0, Ordering::Release);
        }
        let mut flag_bits = 0;
        let listener = match TcpListener::bind(("127.0.0.1", opts.port)) {
            Ok(l) => {
                l.set_nonblocking(true)?;
                Some(l)
            }
            Err(_) => {
                flag_bits |= flags::NO_HANDSHAKE as u32;
                None
            }
        };
        let now = Instant::now();
        let stamps = Stamps {
            frame: 0,
            model: 0.0,
            real: 0.0,
            mission_gen: 1,
            lifecycle: lifecycle::RUNNING as u32,
            campaign: 0,
        };
        let mut agent = Agent {
            out: Producer::new(Ring::outbound(opts.out_capacity), 0),
            inbound: Consumer::new(Ring::inbound(opts.out_capacity, opts.in_capacity), 0),
            boot: header.agent_boot,
            opts,
            map,
            start: now,
            listener,
            conn: Conn::None,
            next_syscall: now,
            frame: 0,
            attached_frames: 0,
            stamps,
            queue: VecDeque::new(),
            event_bytes: 0,
            control_count: 0,
            gaps: Default::default(),
            flags: flag_bits,
            hello_due: false,
            reading_commands: false,
            blocked: false,
            lose_control: false,
            beat: 0,
            beat_at: now,
            last_served: 0,
            epoch: 0,
            last_applied_seq: 0,
            event_seq: 0,
            log_seq: 0,
            counters: Counters::default(),
            metrics: Metrics::default(),
            last_pump: None,
            payload: Vec::new(),
            log: Vec::new(),
        };
        agent.map.store32(s::MISSION_GEN, 1, Ordering::Relaxed);
        let l = Lifecycle {
            state: lifecycle::RUNNING as u32,
            mission_gen: 1,
            theater: "Fake".into(),
            mission: "fake.miz".into(),
        };
        agent.enqueue(kind::LIFECYCLE as u16, l.encode());
        Ok(agent)
    }

    /// The agent's boot.
    pub fn boot(&self) -> u64 {
        self.boot
    }

    /// Whether a sidecar is attached.
    pub fn attached(&self) -> bool {
        self.flags & flags::ATTACHED as u32 != 0
    }

    /// Take the diagnostic lines written since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn note(&mut self, line: String) {
        self.log.push(line);
    }

    /// Pump frames until `stop` is set.
    pub fn run(&mut self, stop: &AtomicBool, mut on_log: impl FnMut(&str)) {
        let period = Duration::from_secs_f64(1.0 / f64::from(self.opts.fps.max(1)));
        let mut next = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            self.pump();
            for l in self.take_log() {
                on_log(&l);
            }
            next += period;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    }

    /// One frame.
    pub fn pump(&mut self) {
        let began = Instant::now();
        self.frame += 1;
        let real = began.duration_since(self.start).as_secs_f64();
        self.stamps.frame = self.frame;
        self.stamps.real = real;
        self.stamps.model = real;
        let m = &self.map;
        m.store(s::FRAME, self.frame, Ordering::Relaxed);
        m.store(
            s::CLOCK,
            began.duration_since(self.start).as_nanos() as u64,
            Ordering::Relaxed,
        );
        m.store(s::MODEL_TIME, real.to_bits(), Ordering::Relaxed);
        m.store(s::REAL_TIME, real.to_bits(), Ordering::Relaxed);
        m.store32(s::LIFECYCLE, self.stamps.lifecycle, Ordering::Relaxed);
        if self.attached() {
            self.attached_frames += 1;
        }

        for b in self.opts.misbehave.clone() {
            if let Misbehavior::Stall(f, ms) = b
                && f == self.attached_frames
            {
                std::thread::sleep(Duration::from_millis(ms));
            }
        }

        if self.attached() {
            // Timed from when a change is seen, not from the frame's start: a
            // frame that stalled would otherwise date the change too early.
            let now = Instant::now();
            let beat = self.map.load(s::SIDECAR_HEARTBEAT, Ordering::Acquire);
            if beat != self.beat {
                self.beat = beat;
                self.beat_at = now;
            } else if now.duration_since(self.beat_at) >= Duration::from_millis(500) {
                self.detach("the sidecar's heartbeat stopped for 0.5 s");
            }
        }
        if began >= self.next_syscall {
            self.syscall(began);
        }
        self.check_attach(began);

        self.produce();
        for b in self.opts.misbehave.clone() {
            if let Misbehavior::LoseControl(f) = b
                && f == self.attached_frames
            {
                self.lose_control = true;
            }
            if let Misbehavior::Oversize(f) = b
                && f == self.attached_frames
            {
                let len = self.out.ring().max_payload() + 8;
                self.enqueue(kind::AUTHOR_EVENT as u16, vec![0; len]);
            }
        }
        if self.attached() && self.reading_commands {
            self.commands();
        }
        if self.attached() {
            self.write_out();
            self.out.publish(&self.map);
            self.misbehave_ring();
            if self
                .opts
                .misbehave
                .contains(&Misbehavior::DetachAfterPublish(self.attached_frames))
            {
                self.detach("the detach-after-publish misbehavior");
            }
        }
        self.store_status(began);
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
                    (Some(()), Err(_)) => self.answer(stream, b"", now),
                    (Some(()), Ok(Some(line))) => self.answer(stream, &line, now),
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
                since,
                ok,
            } => match stream.write(&line) {
                Ok(n) if n == line.len() => match ok {
                    Some(_) if self.opts.misbehave.contains(&Misbehavior::CloseAfterOk) => {
                        Conn::Closing(stream)
                    }
                    Some((sidecar_boot, attach_id)) => {
                        // No socket call until the attach timeout.
                        self.next_syscall = now + Duration::from_secs(2);
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
                    Conn::Replying {
                        stream,
                        line,
                        since,
                        ok,
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => Conn::Replying {
                    stream,
                    line,
                    since,
                    ok,
                },
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
                    self.next_syscall = since + Duration::from_secs(2);
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

    fn answer(&mut self, stream: TcpStream, line: &[u8], now: Instant) -> Conn {
        if self.opts.misbehave.contains(&Misbehavior::NoOk) {
            return Conn::Reading {
                stream,
                buf: LineBuffer::new(),
                since: now,
            };
        }
        let reply = match Line::parse(line) {
            Ok(Line::Hello {
                version,
                token,
                sidecar_boot,
            }) => {
                if version != VERSION {
                    Err(Refusal::Version)
                } else if !tokens_equal(token.as_bytes(), self.opts.token.as_bytes()) {
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
                let hash = if self.opts.misbehave.contains(&Misbehavior::WrongOkHash) {
                    LAYOUT_HASH ^ 1
                } else {
                    LAYOUT_HASH
                };
                let l = Line::Ok {
                    layout_hash: hash,
                    agent_boot: if self.opts.misbehave.contains(&Misbehavior::OkOtherBoot) {
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
                since: now,
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
        let m = &self.map;
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
        if let Err(e) = out.refresh(&self.map) {
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
        if sidecar_boot != self.last_served {
            // A new sidecar: drop what the old one set (subscriptions, rates).
            self.last_served = sidecar_boot;
        }
        self.map
            .store(s::SIDECAR_BOOT_SEEN, sidecar_boot, Ordering::Release);
        self.flags |= flags::ATTACHED as u32;
        self.beat = self.map.load(s::SIDECAR_HEARTBEAT, Ordering::Acquire);
        // Timed from the read, as in pump (spec/transport.md, "Detaching").
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

    fn detach(&mut self, why: &str) {
        // The connection closes on the next frame, as that frame's one system
        // call; the accept after it follows 100 ms later.
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
        self.map.store(s::SIDECAR_BOOT_SEEN, 0, Ordering::Release);
        self.flags &= !(flags::ATTACHED as u32);
        self.hello_due = false;
        self.reading_commands = false;
        self.note(format!("detached: {why}"));
    }

    fn produce(&mut self) {
        if self.frame.is_multiple_of(self.opts.event_every) {
            self.event_seq += 1;
            let e = content::event(self.boot, self.event_seq);
            self.enqueue(kind::AUTHOR_EVENT as u16, e);
        }
        if self.frame.is_multiple_of(self.opts.log_every) {
            self.log_seq += 1;
            let l = content::log(self.boot, self.log_seq);
            self.enqueue(kind::LOG as u16, l.encode());
        }
    }

    /// Queue an event-class or control-class record, by the delivery rules
    /// of spec/records.md.
    fn enqueue(&mut self, kind: u16, payload: Vec<u8>) {
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
            // Of spec/records.md, "Oversized records", the fake agent keeps
            // only the count in the pending GAP of events, for every kind; it
            // writes no replacement record and no LOG at error.
            self.note(format!("a record of kind {kind} over the payload limit"));
            let event = Item { log: false, ..item };
            self.lose(&event);
            return;
        }
        match class {
            Class::Control => {
                let lose = std::mem::take(&mut self.lose_control);
                if lose || self.control_count >= self.opts.control_queue {
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
        let item = self.queue.remove(i).expect("an item");
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

    fn commands(&mut self) {
        if let Err(e) = self.inbound.refresh(&self.map) {
            self.detach(&format!("inbound ring: {e}"));
            return;
        }
        for _ in 0..self.opts.commands_per_frame {
            if self.control_count >= self.opts.control_queue {
                self.flags |= flags::COMMANDS_HELD as u32;
                return;
            }
            self.flags &= !(flags::COMMANDS_HELD as u32);
            let r = match self.inbound.next(&self.map, &mut self.payload) {
                Ok(Some(r)) => r,
                Ok(None) => return,
                Err(e) => {
                    self.detach(&format!("inbound ring: {e}"));
                    return;
                }
            };
            // Taken: in_head moves past it before it runs.
            self.map.store(s::IN_HEAD, r.next, Ordering::Release);
            let payload = std::mem::take(&mut self.payload);
            self.run_command(r.kind, &payload);
            self.payload = payload;
        }
    }

    fn run_command(&mut self, opcode: u16, payload: &[u8]) {
        let mut rd = Reader::new(payload);
        let Ok(h) = CommandHeader::read(&mut rd) else {
            self.note(format!(
                "a command of opcode {opcode} too short for its header"
            ));
            return;
        };
        if h.seq == 0 {
            // The fake agent handles no control opcode, so every one is
            // unknown to it (spec/commands.md, "Unknown opcodes").
            self.counters.commands_applied += 1;
            let r = CommandResult {
                seq: 0,
                epoch: 0,
                campaign: h.campaign,
                entity: h.entity,
                rev: h.rev,
                object: 0,
                cost_us: 0,
                opcode,
                code: result::UNKNOWN_OPCODE as u16,
                message: String::new(),
                value: Lv::Nil,
            };
            self.enqueue(kind::RESULT as u16, r.encode());
            return;
        }
        let code = if h.epoch < self.epoch {
            result::STALE_EPOCH
        } else {
            if h.epoch > self.epoch {
                self.epoch = h.epoch;
                self.last_applied_seq = 0;
            }
            if h.seq <= self.last_applied_seq {
                result::DUPLICATE
            } else {
                self.last_applied_seq = h.seq;
                if u64::from(opcode) == content::OPCODE {
                    result::APPLIED
                } else {
                    result::UNKNOWN_OPCODE
                }
            }
        };
        self.counters.commands_applied += 1;
        let r = CommandResult {
            seq: h.seq,
            epoch: h.epoch,
            campaign: h.campaign,
            entity: h.entity,
            rev: h.rev,
            object: 0,
            cost_us: 1,
            opcode,
            code: code as u16,
            message: String::new(),
            value: Lv::Number(content::digest(payload)),
        };
        self.enqueue(kind::RESULT as u16, r.encode());
    }

    /// Write what the ring has room for: pending gaps, the queue in order,
    /// the `HELLO` once the queue is empty, then this frame's samples.
    fn write_out(&mut self) {
        // The head is loaded and checked once a batch (spec/transport.md,
        // "Producing"), not only when the ring looks full.
        if let Err(e) = self.out.refresh(&self.map) {
            self.detach(&format!("outbound ring: {e}"));
            return;
        }
        let mut last: Option<Stamps> = None;
        self.write_queued(&mut last);
        // The agent samples once it has written its HELLO; a sample the ring
        // has no room for is counted (spec/records.md, "Delivery classes").
        if self.reading_commands {
            let p = content::positions(self.boot, self.frame);
            if self.blocked || !self.put(&mut last, self.stamps, kind::POSITIONS as u16, &p) {
                self.counters.samples_dropped += 1;
            }
        }
        for b in self.opts.misbehave.clone() {
            if let Misbehavior::BadUtf8(f) = b
                && f == self.attached_frames
            {
                let mut bad = vec![3, 0, 0, 0, 0, 0, 0, 0];
                bad.extend([2, 0, 0xff, 0xfe, 0, 0, 0]);
                self.put(&mut last, self.stamps, kind::LOG as u16, &bad);
            }
        }
    }

    /// The pending gaps, the queue in order, and the `HELLO` once the queue
    /// is empty. Sets `blocked` when the ring had no room.
    fn write_queued(&mut self, last: &mut Option<Stamps>) {
        self.blocked = true;
        for class in [gap_class::EVENTS, gap_class::LOGS] {
            let g = &self.gaps[class as usize - 1];
            if g.count == 0 {
                continue;
            }
            let (from, to) = (g.from.expect("a start"), g.to.expect("an end"));
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
            if !self.put(last, to, kind::GAP as u16, &gap.encode()) {
                return;
            }
            self.gaps[class as usize - 1] = PendingGap::default();
            self.counters.gaps += 1;
        }
        while let Some(item) = self.queue.front() {
            let (k, stamps) = (item.kind, item.stamps);
            let payload = std::mem::take(&mut self.queue[0].payload);
            if !self.put(last, stamps, k, &payload) {
                self.queue[0].payload = payload;
                return;
            }
            let item = self.queue.pop_front().expect("an item");
            match item.class {
                Class::Control => self.control_count -= 1,
                _ => self.event_bytes -= payload.len(),
            }
        }
        if self.flags & flags::CONTROL_LOST as u32 != 0 {
            self.hello_due = true;
        }
        if self.hello_due {
            let Conn::Attached { sidecar_boot, .. } = self.conn else {
                return;
            };
            let hello = Hello {
                agent_boot: self.boot,
                sidecar_boot,
                build: 0xfa4e,
                campaign: self.stamps.campaign,
                epoch: self.epoch,
                last_applied_seq: self.last_applied_seq,
                in_head: self.inbound.head(),
                interface: INTERFACE,
                flags: self.flags & !(flags::CONTROL_LOST as u32),
                dcs_version: "fake".into(),
            };
            if !self.put(last, self.stamps, kind::HELLO as u16, &hello.encode()) {
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
    fn put(
        &mut self,
        last: &mut Option<Stamps>,
        stamps: Stamps,
        kind: u16,
        payload: &[u8],
    ) -> bool {
        let saved = self.out.clone();
        if *last != Some(stamps) {
            let own = self.out.records() + self.out.records_for(56);
            let f = Frame {
                frame: stamps.frame,
                model_time: stamps.model,
                real_time: stamps.real,
                campaign: stamps.campaign,
                number: own + 1 + u64::from(self.misbehaving_frame()),
                mission_gen: stamps.mission_gen,
                lifecycle: stamps.lifecycle,
                replay: u32::from(stamps.frame != self.frame),
            };
            match self.out.push(
                &self.map,
                nanoprobe_transport::layout::kind::FRAME as u16,
                &[&f.encode()],
            ) {
                Ok(true) => {}
                Ok(false) => return false,
                Err(e) => {
                    self.detach(&format!("outbound ring: {e}"));
                    return false;
                }
            }
        }
        match self.out.push(&self.map, kind, &[payload]) {
            Ok(true) => {
                *last = Some(stamps);
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

    fn misbehaving_frame(&self) -> bool {
        self.opts
            .misbehave
            .iter()
            .any(|b| matches!(b, Misbehavior::FrameNumber(f) if *f == self.attached_frames))
    }

    fn misbehave_ring(&mut self) {
        for b in self.opts.misbehave.clone() {
            match b {
                Misbehavior::BadLength(f) if f == self.attached_frames => {
                    let ring = self.out.ring();
                    let off = ring.data + (self.out.tail() & (ring.capacity - 1)) as usize;
                    let mut h = [0u8; 8];
                    h[..4].copy_from_slice(&u32::MAX.to_le_bytes());
                    h[4..6].copy_from_slice(&(kind::LOG as u16).to_le_bytes());
                    self.map.write(off, &h);
                    let tail = self.out.tail() + record_size(8);
                    self.map.store(s::OUT_TAIL, tail, Ordering::Release);
                    self.note("published a record over the length limit".into());
                }
                Misbehavior::TailPastCapacity(f) if f == self.attached_frames => {
                    let head = self.map.load(s::OUT_HEAD, Ordering::Acquire);
                    let tail = head + self.out.ring().capacity + 8;
                    self.map.store(s::OUT_TAIL, tail, Ordering::Release);
                    self.note("published a tail past the capacity".into());
                }
                _ => {}
            }
        }
    }

    fn store_status(&mut self, began: Instant) {
        let m = &self.map;
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
        self.metrics.slice_us = (0.003 * self.metrics.frame_interval_us).min(250.0);
        self.last_pump = Some(began);
        status::write_metrics(m, &self.metrics);
    }

    /// The event queue's bytes and the control queue's records.
    pub fn queued(&self) -> (usize, usize) {
        (self.event_bytes, self.control_count)
    }

    /// The event queue's cap and the control queue's.
    pub fn queue_caps(&self) -> (usize, usize) {
        (self.opts.event_queue, self.opts.control_queue)
    }
}
