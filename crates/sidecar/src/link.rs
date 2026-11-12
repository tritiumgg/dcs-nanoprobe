//! The sidecar's side of the link to the agent: the handshake, attaching,
//! polling the mapped file, and detaching (spec/transport.md).
//!
//! [`run`] loops over sessions. A session is one handshake and the polls
//! that follow it; it ends when a check fails, the connection closes, or the
//! agent restarts. What the sidecar does with the records it reads and which
//! commands it writes is the [`Sink`]'s business.

use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nanoprobe_transport::golden::LAYOUT_HASH;
use nanoprobe_transport::handshake::{Line, LineBuffer, VERSION};
use nanoprobe_transport::layout::{kind, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::{Frame, Hello};
use nanoprobe_transport::ring::{Consumer, Producer, Ring};
use nanoprobe_transport::status::{self, Geometry, Snapshot};

/// How the sidecar reaches the agent.
#[derive(Clone, Debug)]
pub struct Config {
    /// The mapped file, as the sidecar opens it.
    pub map_path: PathBuf,
    /// The agent's handshake address.
    pub handshake: SocketAddr,
    /// The token from the token file.
    pub token: String,
    /// How often the sidecar polls the file, 1 to 5 ms.
    pub poll: Duration,
    /// How long one handshake attempt waits; after a DCS restart, how long a
    /// refused connect goes on before [`run`] gives up.
    pub handshake_timeout: Duration,
}

/// Why a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// [`run`] was asked to stop.
    Stopped,
    /// Something to retry from the handshake: a refusal, a failed check, a
    /// closed connection, a restarted agent. The text says which.
    Retry(String),
    /// A connect was refused for the whole handshake timeout after the agent
    /// restarted. The caller exits so whatever runs it starts it again.
    Gone,
}

/// A command for the inbound ring: an opcode and its whole payload, command
/// header included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The opcode.
    pub opcode: u16,
    /// The payload.
    pub payload: Vec<u8>,
}

/// What an attached session tells the sidecar, and asks of it.
pub trait Sink {
    /// The session attached to `agent_boot`. The heartbeat is already
    /// running.
    fn attached(&mut self, agent_boot: u64, sidecar_boot: u64);
    /// The highest record number already journalled for `agent_boot`, 0 for
    /// none. Records at or below it are dropped.
    fn last_number(&self, agent_boot: u64) -> u64;
    /// A record at or below [`Sink::last_number`], read again after a restart
    /// and not journalled again. The sink may check it against its journal.
    fn reread(&mut self, agent_boot: u64, number: u64, kind: u16, payload: &[u8]);
    /// A new record. `number` names it for good with `agent_boot`.
    fn record(
        &mut self,
        agent_boot: u64,
        number: u64,
        kind: u16,
        payload: &[u8],
    ) -> Result<(), String>;
    /// Make every record so far durable. `out_head` is stored after it.
    fn durable(&mut self) -> Result<(), String>;
    /// A `HELLO` of this attach was read, the record numbered `number`:
    /// commands may follow. A second one in an attach is a resync after the
    /// agent lost a control record.
    fn hello(&mut self, hello: &Hello, number: u64);
    /// The next command to write, or `None`. Called only after
    /// [`Sink::hello`].
    fn next_command(&mut self) -> Option<Command>;
    /// `command` was written and ends at inbound position `end`; `wrapped`
    /// when a padding record went before it. It becomes visible to the agent
    /// when this poll stores `in_tail`, after [`Sink::durable`].
    fn sent(&mut self, command: &Command, end: u64, wrapped: bool);
    /// A diagnostic line: a refusal, a failed check, a detach.
    fn report(&mut self, line: &str);
}

/// Run sessions until `stop` is set or the agent is gone.
pub fn run(cfg: &Config, sink: &mut impl Sink, stop: &AtomicBool) -> End {
    let sidecar_boot = nanoprobe_transport::random_nonzero();
    let mut refused_since: Option<Instant> = None;
    let mut last_boot: Option<u64> = None;
    // The agent boot whose last session ended on a failed record check.
    let mut corrupt: Option<u64> = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            return End::Stopped;
        }
        let mut session = Session {
            cfg,
            sidecar_boot,
            refused: false,
            corrupt: corrupt.take(),
        };
        let end = session.run(sink, stop, &mut last_boot);
        corrupt = session.corrupt;
        match &end {
            End::Stopped | End::Gone => return end,
            End::Retry(why) => sink.report(why),
        }
        // A refused connect counts toward the timeout only while the file
        // shows an agent boot other than the last one attached: DCS
        // restarted and its new agent does not answer. While the file keeps
        // the old boot, DCS is dead or not yet restarted, and the sidecar
        // stays to keep what it knows of that boot (spec/transport.md,
        // "Handshake").
        let restarted = match (file_boot(&cfg.map_path), last_boot) {
            (Some(now), Some(last)) => now != last,
            _ => false,
        };
        if session.refused && restarted {
            let since = *refused_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= cfg.handshake_timeout {
                return End::Gone;
            }
        } else {
            refused_since = None;
        }
        std::thread::sleep(Duration::from_millis(if session.refused {
            20
        } else {
            100
        }));
    }
}

/// The agent boot in the file's header, when the header is readable and
/// ready.
fn file_boot(path: &std::path::Path) -> Option<u64> {
    let map = MappedFile::open_read_only(path).ok()?;
    let snap = Snapshot::read(&map)?;
    Some(snap.u64(s::AGENT_BOOT)).filter(|&b| b != 0)
}

struct Session<'a> {
    cfg: &'a Config,
    sidecar_boot: u64,
    refused: bool,
    /// On entry, the boot whose last session ended on a failed record check;
    /// on return, this session's boot if it did.
    corrupt: Option<u64>,
}

/// Sets its flag when dropped, also while a panic unwinds.
struct Raise<'a>(&'a AtomicBool);

impl Drop for Raise<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Session<'_> {
    fn run(&mut self, sink: &mut impl Sink, stop: &AtomicBool, last_boot: &mut Option<u64>) -> End {
        let retry = |why: String| End::Retry(why);
        let mut stream =
            match TcpStream::connect_timeout(&self.cfg.handshake, self.cfg.handshake_timeout) {
                Ok(s) => s,
                Err(e) => {
                    self.refused = e.kind() == ErrorKind::ConnectionRefused;
                    return retry(format!("connect {}: {e}", self.cfg.handshake));
                }
            };
        let (agent_boot, attach_id) = match self.handshake(&mut stream, stop) {
            Ok(v) => v,
            Err(end) => return end,
        };
        let map = match MappedFile::open(&self.cfg.map_path) {
            Ok(m) => m,
            Err(e) => return retry(format!("map {}: {e}", self.cfg.map_path.display())),
        };
        let geometry = match status::check(&map, map.file_len()) {
            Ok(g) => g,
            Err(e) => return retry(format!("header: {e}")),
        };
        if geometry.agent_boot != agent_boot {
            return retry(format!(
                "the file's agent boot {:x} is not the OK line's {agent_boot:x}",
                geometry.agent_boot
            ));
        }
        *last_boot = Some(agent_boot);
        let attach_tail = self.attach(&map, &geometry, attach_id, sink);
        // The heartbeat starts as soon as attach_seen is stored, and runs on
        // its own thread so that a slow journal never stops it
        // (spec/transport.md, "Attaching" and "Consuming"). It stops when the
        // session ends, a panic included, and the connection closes with it.
        let done = AtomicBool::new(false);
        let poll = self.cfg.poll;
        let sidecar_boot = self.sidecar_boot;
        let (end, corrupt) = std::thread::scope(|scope| {
            let _stop = Raise(&done);
            scope.spawn(|| heartbeat(&map, sidecar_boot, attach_id, poll, &done));
            sink.attached(agent_boot, sidecar_boot);
            let mut polls = Polls {
                map: &map,
                geometry,
                sidecar_boot,
                attach_id,
                attach_tail,
                stream,
                out: Consumer::new(
                    Ring::outbound(geometry.out_capacity),
                    map.load(s::OUT_HEAD, Ordering::Relaxed),
                ),
                inbound: Producer::new(
                    Ring::inbound(geometry.out_capacity, geometry.in_capacity),
                    map.load(s::IN_TAIL, Ordering::Relaxed),
                ),
                expected: None,
                seen_own_boot: false,
                hello: false,
                held: None,
                in_head: 0,
                payload: Vec::new(),
                corrupt: false,
            };
            let end = polls.run(sink, stop, poll);
            (end, polls.corrupt)
        });
        self.corrupt = corrupt.then_some(agent_boot);
        end
    }

    /// Send `HELLO` and read the answer. The agent answers on its own frames,
    /// at most every 100 ms, so this waits up to the handshake timeout.
    fn handshake(&self, stream: &mut TcpStream, stop: &AtomicBool) -> Result<(u64, u64), End> {
        let retry = |why: String| Err(End::Retry(why));
        let hello = Line::Hello {
            version: VERSION,
            token: self.cfg.token.clone(),
            sidecar_boot: self.sidecar_boot,
        };
        let Some(line) = hello.encode() else {
            return retry("the token does not fit a HELLO line".into());
        };
        let _ = stream.set_nodelay(true);
        if let Err(e) = stream.write_all(line.as_bytes()) {
            return retry(format!("send HELLO: {e}"));
        }
        let deadline = Instant::now() + self.cfg.handshake_timeout;
        let mut buf = LineBuffer::new();
        let mut chunk = [0u8; 1024];
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(End::Stopped);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return retry("no answer to HELLO within the handshake timeout".into());
            }
            let _ = stream.set_read_timeout(Some(left.min(Duration::from_millis(100))));
            match stream.read(&mut chunk) {
                Ok(0) => {
                    return retry("the agent closed the connection during the handshake".into());
                }
                Ok(n) => buf.push(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    continue;
                }
                Err(e) => return retry(format!("read the answer to HELLO: {e}")),
            }
            match buf.line() {
                Ok(None) => continue,
                Err(e) => return retry(format!("the answer to HELLO: {e}")),
                Ok(Some(l)) => {
                    return match Line::parse(&l) {
                        Ok(Line::Ok {
                            layout_hash,
                            agent_boot,
                            attach_id,
                            ..
                        }) => {
                            if layout_hash != LAYOUT_HASH {
                                retry(format!(
                                    "the agent's layout hash {layout_hash:016x} is not {LAYOUT_HASH:016x}"
                                ))
                            } else if agent_boot == 0 || attach_id == 0 {
                                retry("an OK line with a zero boot or attach ID".into())
                            } else if !buf.is_empty() {
                                retry("the agent sent more after OK".into())
                            } else {
                                Ok((agent_boot, attach_id))
                            }
                        }
                        Ok(Line::Err(r)) => retry(format!("the agent refused: {}", r.as_str())),
                        Ok(other) => retry(format!("an unexpected line: {other:?}")),
                        Err(e) => retry(format!("the answer to HELLO: {e}")),
                    };
                }
            }
        }
    }

    /// Steps 2 and 3 of "Attaching". Returns `out_tail` as it was before the
    /// agent attached: the agent writes nothing while detached, so every
    /// record before it is from an earlier attach.
    fn attach(&self, m: &impl Memory, g: &Geometry, attach_id: u64, sink: &mut impl Sink) -> u64 {
        let out_tail = m.load(s::OUT_TAIL, Ordering::Acquire);
        if m.load(s::AGENT_BOOT_SEEN, Ordering::Relaxed) != g.agent_boot {
            m.store(s::OUT_HEAD, 0, Ordering::Release);
            m.store(s::IN_TAIL, 0, Ordering::Release);
        } else {
            // A kept head the agent's checks would refuse, left by a sidecar
            // that made a late store, would refuse every attach: skip to the
            // tail and say what was lost.
            let head = m.load(s::OUT_HEAD, Ordering::Relaxed);
            if !head.is_multiple_of(8)
                || head > out_tail
                || head < out_tail.saturating_sub(g.out_capacity)
            {
                sink.report(&format!(
                    "the kept out_head {head:#x} is impossible for out_tail {out_tail:#x}; records between are lost"
                ));
                m.store(s::OUT_HEAD, out_tail, Ordering::Release);
            } else if self.corrupt == Some(g.agent_boot) {
                // Reading from the kept head would meet the same bad record.
                sink.report(&format!(
                    "the last session ended on a failed record check; skipped from {head:#x} to out_tail {out_tail:#x}, records between are lost"
                ));
                m.store(s::OUT_HEAD, out_tail, Ordering::Release);
            }
            let in_head = m.load(s::IN_HEAD, Ordering::Acquire);
            m.store(s::IN_TAIL, in_head, Ordering::Release);
        }
        m.store(s::AGENT_BOOT_SEEN, g.agent_boot, Ordering::Release);
        m.store(s::SIDECAR_BOOT, self.sidecar_boot, Ordering::Release);
        let beat = m.load(s::SIDECAR_HEARTBEAT, Ordering::Relaxed);
        m.store(
            s::SIDECAR_HEARTBEAT,
            beat.wrapping_add(1),
            Ordering::Release,
        );
        m.store(s::SIDECAR_MS, now_ms(), Ordering::Relaxed);
        m.store(s::ATTACH_SEEN, attach_id, Ordering::Release);
        out_tail
    }
}

/// Store the heartbeat every `poll` until `done`, while the file still names
/// this sidecar and attach. It checks before every store, like every write.
fn heartbeat(
    m: &impl Memory,
    sidecar_boot: u64,
    attach_id: u64,
    poll: Duration,
    done: &AtomicBool,
) {
    while !done.load(Ordering::Relaxed) {
        if m.load(s::SIDECAR_BOOT, Ordering::Acquire) != sidecar_boot
            || m.load(s::ATTACH_SEEN, Ordering::Acquire) != attach_id
        {
            return;
        }
        let beat = m.load(s::SIDECAR_HEARTBEAT, Ordering::Relaxed);
        m.store(
            s::SIDECAR_HEARTBEAT,
            beat.wrapping_add(1),
            Ordering::Release,
        );
        m.store(s::SIDECAR_MS, now_ms(), Ordering::Relaxed);
        std::thread::sleep(poll);
    }
}

struct Polls<'a, M: Memory> {
    map: &'a M,
    geometry: Geometry,
    sidecar_boot: u64,
    attach_id: u64,
    /// `out_tail` when this attach began: a `HELLO` before it is an earlier
    /// attach's.
    attach_tail: u64,
    stream: TcpStream,
    out: Consumer,
    inbound: Producer,
    /// The number of the next outbound record, once a `FRAME` gave it.
    expected: Option<u64>,
    /// The agent stored this sidecar's boot in `sidecar_boot_seen`.
    seen_own_boot: bool,
    /// The `HELLO` of this attach was read.
    hello: bool,
    /// A command that did not fit, written first on the next poll.
    held: Option<Command>,
    /// The largest `in_head` read.
    in_head: u64,
    payload: Vec<u8>,
    /// The session ended on a failed check of an outbound record.
    corrupt: bool,
}

/// Why reading the outbound ring stopped.
enum Fault {
    /// A record or a position failed a check: the agent is broken.
    Corrupt(String),
    /// Anything else: the sink, or a check of the sidecar's own state.
    Other(String),
}

/// Records read in one poll before the head is stored.
const RECORDS_PER_POLL: usize = 4096;
/// Commands written in one poll.
const COMMANDS_PER_POLL: usize = 64;

impl<M: Memory> Polls<'_, M> {
    fn run(&mut self, sink: &mut impl Sink, stop: &AtomicBool, poll: Duration) -> End {
        let _ = self.stream.set_nonblocking(true);
        let mut last = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return End::Stopped;
            }
            std::thread::sleep(poll);
            // A sidecar stopped for over 0.25 s was detached, and another may
            // hold the file. Every poll checks before it writes, so that case
            // needs no check of its own; it is reported.
            if last.elapsed() > Duration::from_millis(250) {
                sink.report(&format!("a poll came {:?} late", last.elapsed()));
            }
            last = Instant::now();
            if let Err(why) = self.check() {
                return End::Retry(why);
            }
            if let Err(why) = self.read(sink) {
                return End::Retry(why);
            }
            if let Err(why) = self.check() {
                return End::Retry(why);
            }
            if self.hello
                && let Err(why) = self.write(sink)
            {
                return End::Retry(why);
            }
        }
    }

    /// The checks of "Detaching": the connection, the sidecar's own fields,
    /// the agent's `sidecar_boot_seen`, and the agent's boot.
    fn check(&mut self) -> Result<(), String> {
        let mut b = [0u8; 64];
        match self.stream.read(&mut b) {
            Ok(0) => return Err("the agent closed the connection".into()),
            Ok(_) => return Err("the agent sent bytes after OK".into()),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("the connection: {e}")),
        }
        let m = self.map;
        if m.load(s::SIDECAR_BOOT, Ordering::Acquire) != self.sidecar_boot
            || m.load(s::ATTACH_SEEN, Ordering::Acquire) != self.attach_id
        {
            return Err("another sidecar holds the file".into());
        }
        let Some(snap) = Snapshot::read(m) else {
            return Err("the agent is rewriting the header".into());
        };
        if snap.u64(s::AGENT_BOOT) != self.geometry.agent_boot {
            return Err("the agent restarted".into());
        }
        let seen = snap.u64(s::SIDECAR_BOOT_SEEN);
        if seen == self.sidecar_boot {
            self.seen_own_boot = true;
        } else if self.seen_own_boot {
            return Err(format!("the agent detached (sidecar_boot_seen {seen:x})"));
        }
        let in_head = snap.u64(s::IN_HEAD);
        if in_head < self.in_head {
            return Err(format!("in_head moved back to {in_head:#x}"));
        }
        self.in_head = in_head;
        Ok(())
    }

    fn read(&mut self, sink: &mut impl Sink) -> Result<(), String> {
        self.read_records(sink).map_err(|f| match f {
            Fault::Corrupt(why) => {
                self.corrupt = true;
                why
            }
            Fault::Other(why) => why,
        })
    }

    fn read_records(&mut self, sink: &mut impl Sink) -> Result<(), Fault> {
        use Fault::{Corrupt, Other};
        let boot = self.geometry.agent_boot;
        if let Err(e) = self.out.refresh(self.map) {
            // A new agent writes out_tail 0: a restart, not a broken peer.
            return Err(if self.map.load(s::AGENT_BOOT, Ordering::Acquire) != boot {
                Other("the agent restarted".into())
            } else {
                Corrupt(format!("outbound ring: {e}"))
            });
        }
        let last = sink.last_number(boot);
        let mut n = 0;
        let mut frame_at = None;
        while n < RECORDS_PER_POLL {
            let Some(r) = self
                .out
                .next(self.map, &mut self.payload)
                .map_err(|e| Corrupt(format!("outbound ring: {e}")))?
            else {
                break;
            };
            n += 1;
            let number = if u64::from(r.kind) == kind::FRAME {
                frame_at = Some(r.position);
                let f = Frame::decode(&self.payload)
                    .map_err(|e| Corrupt(format!("FRAME at {:#x}: {e}", r.position)))?;
                let own = f
                    .number
                    .checked_sub(1)
                    .ok_or_else(|| Corrupt("a FRAME numbered 0".into()))?;
                if let Some(e) = self.expected {
                    let at = e + u64::from(r.padding);
                    if own != at {
                        return Err(Corrupt(format!(
                            "FRAME at {:#x} is record {own}, expected {at}",
                            r.position
                        )));
                    }
                }
                own
            } else {
                let Some(e) = self.expected else {
                    return Err(Corrupt(format!(
                        "a record of kind {} before any FRAME",
                        r.kind
                    )));
                };
                e + u64::from(r.padding)
            };
            self.expected = Some(number + 1);
            // Padding records have numbers too (spec/records.md, "Stamps").
            for p in number.saturating_sub(u64::from(r.padding))..number {
                if p > last {
                    sink.record(boot, p, kind::PADDING as u16, &[])
                        .map_err(Other)?;
                } else {
                    sink.reread(boot, p, kind::PADDING as u16, &[]);
                }
            }
            if u64::from(r.kind) == kind::HELLO {
                let h = Hello::decode(&self.payload)
                    .map_err(|e| Corrupt(format!("HELLO at {:#x}: {e}", r.position)))?;
                if h.sidecar_boot == self.sidecar_boot && h.agent_boot == boot {
                    if r.position >= self.attach_tail {
                        self.hello = true;
                        sink.hello(&h, number);
                    } else {
                        sink.report(&format!(
                            "a HELLO of an earlier attach of this sidecar at {:#x}, before {:#x}; not this attach's",
                            r.position, self.attach_tail
                        ));
                    }
                }
            }
            if number > last {
                sink.record(boot, number, r.kind, &self.payload)
                    .map_err(Other)?;
            } else {
                sink.reread(boot, number, r.kind, &self.payload);
            }
        }
        if n > 0 {
            sink.durable().map_err(Other)?;
            self.check().map_err(Other)?;
            // Every record up to the private head is durable. The head is
            // stored only at the tail or at a FRAME, so a sidecar that starts
            // there reads the stamps first; the records after that FRAME are
            // read again and dropped by their numbers.
            if self.out.head() == self.out.tail() {
                self.out.commit(self.map);
            } else if let Some(at) = frame_at {
                self.out.commit_at(self.map, at);
            }
        }
        Ok(())
    }

    fn write(&mut self, sink: &mut impl Sink) -> Result<(), String> {
        self.inbound
            .refresh(self.map)
            .map_err(|e| format!("inbound ring: {e}"))?;
        let mut wrote = false;
        for _ in 0..COMMANDS_PER_POLL {
            let Some(c) = self.held.take().or_else(|| sink.next_command()) else {
                break;
            };
            let wrapped = self.inbound.records_for(c.payload.len()) == 2;
            match self.inbound.push(self.map, c.opcode, &[&c.payload]) {
                Ok(true) => {
                    sink.sent(&c, self.inbound.tail(), wrapped);
                    wrote = true;
                }
                Ok(false) => {
                    self.held = Some(c);
                    break;
                }
                Err(e) => return Err(format!("a command the sidecar built: {e}")),
            }
        }
        if wrote {
            // Journalled before the agent can see them (spec/commands.md).
            sink.durable()?;
            self.check()?;
            self.inbound.publish(self.map);
            self.check()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_transport::mem::Heap;
    use nanoprobe_transport::status::Header;

    #[derive(Default)]
    struct Taken {
        hellos: Vec<u64>,
        reports: Vec<String>,
    }

    impl Sink for Taken {
        fn attached(&mut self, _: u64, _: u64) {}
        fn last_number(&self, _: u64) -> u64 {
            0
        }
        fn reread(&mut self, _: u64, _: u64, _: u16, _: &[u8]) {}
        fn record(&mut self, _: u64, _: u64, _: u16, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn durable(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn hello(&mut self, _: &Hello, number: u64) {
            self.hellos.push(number);
        }
        fn next_command(&mut self) -> Option<Command> {
            None
        }
        fn sent(&mut self, _: &Command, _: u64, _: bool) {}
        fn report(&mut self, line: &str) {
            self.reports.push(line.to_owned());
        }
    }

    /// A HELLO naming this sidecar that lies before the tail at attach is an
    /// earlier attach's, left unread: it is not taken as this attach's.
    #[test]
    fn a_hello_before_the_attach_tail_is_not_this_attachs() {
        const BOOT: u64 = 0xa6e;
        const SIDECAR: u64 = 0x51de;
        let header = Header {
            interface: 1,
            agent_boot: BOOT,
            build: 1,
            created_ms: 1,
            clock_hz: 1,
            out_capacity: 256 << 10,
            in_capacity: 256 << 10,
        };
        let m = Heap::new(header.file_size() as usize);
        status::start(&m, &header, 3);
        let mut out = Producer::new(Ring::outbound(header.out_capacity), 0);
        let batch = |out: &mut Producer, first: u64| {
            let f = Frame {
                number: first + 1,
                ..Frame::default()
            };
            let h = Hello {
                agent_boot: BOOT,
                sidecar_boot: SIDECAR,
                ..Hello::default()
            };
            assert_eq!(out.push(&m, kind::FRAME as u16, &[&f.encode()]), Ok(true));
            assert_eq!(out.push(&m, kind::HELLO as u16, &[&h.encode()]), Ok(true));
        };
        batch(&mut out, 1);
        let attach_tail = out.tail();
        batch(&mut out, 3);
        out.publish(&m);
        m.store(s::SIDECAR_BOOT, SIDECAR, Ordering::Relaxed);
        m.store(s::ATTACH_SEEN, 7, Ordering::Relaxed);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
        let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let _server = listener.accept().expect("accept");
        client.set_nonblocking(true).expect("nonblocking");
        let geometry = status::check(&m, header.file_size()).expect("a good header");
        let mut polls = Polls {
            map: &m,
            geometry,
            sidecar_boot: SIDECAR,
            attach_id: 7,
            attach_tail,
            stream: client,
            out: Consumer::new(Ring::outbound(header.out_capacity), 0),
            inbound: Producer::new(Ring::inbound(header.out_capacity, header.in_capacity), 0),
            expected: None,
            seen_own_boot: false,
            hello: false,
            held: None,
            in_head: 0,
            payload: Vec::new(),
            corrupt: false,
        };
        let mut sink = Taken::default();
        polls.read(&mut sink).expect("read");
        assert_eq!(sink.hellos, [4], "only the HELLO after the attach tail");
        assert!(
            sink.reports.iter().any(|r| r.contains("earlier attach")),
            "{:?}",
            sink.reports
        );
    }
}
