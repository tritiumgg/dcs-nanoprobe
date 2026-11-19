//! The fake agent: the agent's [`Link`], the code the DLL runs, driven by a
//! timer instead of DCS (spec/transport.md).
//!
//! Its records carry values the stub can check (see [`crate::content`]), it
//! applies the one opcode the stub sends, and its [`Misbehavior`]s break the
//! link on purpose through the link's `faults` feature.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nanoprobe_agent_link::{Admit, Link};
use nanoprobe_transport::codec::{Lv, Reader};
use nanoprobe_transport::layout::{kind, lifecycle, result, status as s};
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::CommandHeader;
use nanoprobe_transport::ring::record_size;

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
    /// It stays in the ring: later records go after it.
    BadLength(u64),
    /// From this attached frame, store a tail more than a capacity past the
    /// head after every publish, until the link detaches; then put the
    /// link's own tail back.
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
    /// `stall@F:MS`. `F` is the attached frame a misbehavior fires on;
    /// `tail-past@F` holds its bad tail from `F` until the link detaches.
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

/// The build the fake agent reports.
const BUILD: u64 = 0xfa4e;

/// The fake agent.
pub struct Agent {
    opts: Options,
    link: Link,
    start: Instant,
    /// Frames pumped while attached: when a frame-numbered misbehavior fires.
    attached_frames: u64,
    /// The tail-past misbehavior fired in this attach: its bad tail is
    /// stored after every publish until the link detaches.
    tail_past: bool,
    frame: u64,
    event_seq: u64,
    log_seq: u64,
    payload: Vec<u8>,
    log: Vec<String>,
}

impl Agent {
    /// Open the link (spec/transport.md, "Start") and queue the mission's
    /// `LIFECYCLE`.
    pub fn open(opts: Options) -> std::io::Result<Agent> {
        let mut link = Link::open(nanoprobe_agent_link::Options {
            map_path: opts.map_path.clone(),
            port: opts.port,
            token: Some(opts.token.clone()),
            out_capacity: opts.out_capacity,
            in_capacity: opts.in_capacity,
            event_queue: opts.event_queue,
            control_queue: opts.control_queue,
            build: BUILD,
            interface: INTERFACE,
            raw_lua: false,
            overhead_share: 0.003,
            dcs_version: "fake".into(),
        })?;
        let map = link.map().clone();
        if opts.misbehave.contains(&Misbehavior::BadMagic) {
            map.store(s::MAGIC, 0xbad, Ordering::Relaxed);
        }
        if opts.misbehave.contains(&Misbehavior::NeverReady) {
            map.store(s::READY, 0, Ordering::Release);
        }
        let f = link.faults();
        f.wrong_ok_hash = opts.misbehave.contains(&Misbehavior::WrongOkHash);
        f.no_ok = opts.misbehave.contains(&Misbehavior::NoOk);
        f.close_after_ok = opts.misbehave.contains(&Misbehavior::CloseAfterOk);
        f.ok_other_boot = opts.misbehave.contains(&Misbehavior::OkOtherBoot);
        link.new_generation();
        link.lifecycle(lifecycle::RUNNING as u32, "Fake", "fake.miz");
        let mut agent = Agent {
            opts,
            link,
            start: Instant::now(),
            attached_frames: 0,
            tail_past: false,
            frame: 0,
            event_seq: 0,
            log_seq: 0,
            payload: Vec::new(),
            log: Vec::new(),
        };
        agent.take_notes();
        Ok(agent)
    }

    /// The agent's boot.
    pub fn boot(&self) -> u64 {
        self.link.boot()
    }

    /// Whether a sidecar is attached.
    pub fn attached(&self) -> bool {
        self.link.attached()
    }

    /// Take the diagnostic lines written since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        self.take_notes();
        std::mem::take(&mut self.log)
    }

    fn take_notes(&mut self) {
        let notes = self.link.take_notes();
        self.log.extend(notes);
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

    fn at(&self, f: impl Fn(Misbehavior) -> Option<u64>) -> bool {
        self.opts
            .misbehave
            .iter()
            .any(|b| f(*b) == Some(self.attached_frames))
    }

    /// One frame.
    pub fn pump(&mut self) {
        let began = Instant::now();
        self.frame += 1;
        let real = began.duration_since(self.start).as_secs_f64();
        if self.link.attached() {
            self.attached_frames += 1;
        }
        for b in self.opts.misbehave.clone() {
            if let Misbehavior::Stall(f, ms) = b
                && f == self.attached_frames
            {
                // After the frame's start and before the heartbeat check, as
                // a frame that stalls inside DCS would.
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
        self.link.begin(began, real, real);
        self.link.faults().frame_number = self.at(|b| match b {
            Misbehavior::FrameNumber(f) => Some(f),
            _ => None,
        });

        self.produce();
        if self.at(|b| match b {
            Misbehavior::LoseControl(f) => Some(f),
            _ => None,
        }) {
            self.link.faults().lose_control = true;
        }
        if self.at(|b| match b {
            Misbehavior::Oversize(f) => Some(f),
            _ => None,
        }) {
            let len = self.link.outbound().ring().max_payload() + 8;
            self.link.enqueue(kind::AUTHOR_EVENT as u16, vec![0; len]);
        }
        let mut taken = 0;
        let mut payload = std::mem::take(&mut self.payload);
        while taken < self.opts.commands_per_frame {
            let Some(opcode) = self.link.take_command(&mut payload) else {
                break;
            };
            taken += 1;
            self.run_command(opcode, &payload);
        }
        self.payload = payload;
        if self.link.attached() {
            self.link.flush();
            let p = content::positions(self.link.boot(), self.frame);
            self.link.sample(kind::POSITIONS as u16, &p);
            if self.at(|b| match b {
                Misbehavior::BadUtf8(f) => Some(f),
                _ => None,
            }) {
                let mut bad = vec![3, 0, 0, 0, 0, 0, 0, 0];
                bad.extend([2, 0, 0xff, 0xfe, 0, 0, 0]);
                self.link.put_raw(kind::LOG as u16, &bad);
            }
            if self.at(|b| match b {
                Misbehavior::BadLength(f) => Some(f),
                _ => None,
            }) {
                self.bad_length();
            }
        }
        let attached = self.link.attached();
        self.link.end();
        self.misbehave_ring(attached);
        if attached
            && self.at(|b| match b {
                Misbehavior::DetachAfterPublish(f) => Some(f),
                _ => None,
            })
        {
            self.link.detach("the detach-after-publish misbehavior");
        }
        self.take_notes();
    }

    fn produce(&mut self) {
        let boot = self.link.boot();
        if self.frame.is_multiple_of(self.opts.event_every) {
            self.event_seq += 1;
            let e = content::event(boot, self.event_seq);
            self.link.enqueue(kind::AUTHOR_EVENT as u16, e);
        }
        if self.frame.is_multiple_of(self.opts.log_every) {
            self.log_seq += 1;
            let l = content::log(boot, self.log_seq);
            self.link.enqueue(kind::LOG as u16, l.encode());
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
        let code = match self.link.admit(&h, opcode, None) {
            // The fake agent handles no control opcode, so every one is
            // unknown to it (spec/commands.md, "Unknown opcodes").
            Admit::Control => {
                self.link
                    .answer(opcode, &h, result::UNKNOWN_OPCODE, 0, 0, "", Lv::Nil);
                return;
            }
            Admit::Answer(code) => code,
            Admit::Run if u64::from(opcode) == content::OPCODE => result::APPLIED,
            Admit::Run => result::UNKNOWN_OPCODE,
        };
        self.link.ran(code);
        self.link.answer(
            opcode,
            &h,
            code,
            0,
            1,
            "",
            Lv::Number(content::digest(payload)),
        );
    }

    /// Write a `LOG` through the link and then set its `len` over the limit.
    /// The frame's publish carries it, and later records go after it, so the
    /// sidecar meets it however late it polls. A record written past the
    /// link's private tail would be overwritten by the next frame's publish
    /// before a sidecar that polls slower than the frame rate could read it.
    fn bad_length(&mut self) {
        if !self.link.put_raw(kind::LOG as u16, &[0; 8]) {
            self.note("no room for the record over the length limit".into());
            return;
        }
        let out = self.link.outbound();
        let ring = out.ring();
        let at = out.tail() - record_size(8);
        let off = ring.data + (at & (ring.capacity - 1)) as usize;
        self.link.map().write(off, &u32::MAX.to_le_bytes());
        self.note("published a record over the length limit".into());
    }

    /// Store a tail more than a capacity past the head after the frame's
    /// publish. Every publish stores the link's own tail, so the bad one is
    /// stored again after each, for as long as the attach lasts: a sidecar
    /// that polls slower than the frame rate still reads it. On the frame the
    /// link is found detached, the tail the link published is put back, so
    /// the next sidecar's attach finds a sane ring. A detach and the next
    /// accept are at least two frames apart (`Link::begin`).
    fn misbehave_ring(&mut self, attached: bool) {
        if attached
            && self.at(|b| match b {
                Misbehavior::TailPastCapacity(f) => Some(f),
                _ => None,
            })
        {
            self.tail_past = true;
            self.note("published a tail past the capacity".into());
        }
        if !self.tail_past {
            return;
        }
        let map = self.link.map();
        if attached {
            // Past the published tail, not the stored head: the sidecar
            // checks against its own head, which may be ahead of the stored
            // one but is never past the published tail.
            let out = self.link.outbound();
            let tail = out.tail() + out.ring().capacity + 8;
            map.store(s::OUT_TAIL, tail, Ordering::Release);
        } else {
            // Nothing is written while detached, so the private tail is the
            // one the last publish stored.
            map.store(s::OUT_TAIL, self.link.outbound().tail(), Ordering::Release);
            self.tail_past = false;
        }
    }

    /// The event queue's bytes and the control queue's records.
    pub fn queued(&self) -> (usize, usize) {
        self.link.queued()
    }

    /// The event queue's cap and the control queue's.
    pub fn queue_caps(&self) -> (usize, usize) {
        self.link.queue_caps()
    }
}
