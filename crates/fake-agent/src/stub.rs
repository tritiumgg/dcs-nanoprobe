//! The stub sidecar's [`Sink`]: it journals every record and every command
//! to text files that [`crate::verify`] checks afterwards, and sends `MESSAGE`
//! commands with checkable payloads.
//!
//! Each stub process writes its own `journal-NNNNNN.txt` in the directory and
//! keeps `state` (the agent boot, its last record number and the command
//! epoch) beside them, so a stub that was killed starts where the last one's
//! durable state ended. A journal line is one of:
//!
//! ```text
//! A <agent_boot> <sidecar_boot> <wall_ms>        attached
//! H <agent_boot> <epoch> <last_applied_seq> <in_head> <number>
//!                                                took the HELLO numbered <number>; the new epoch
//! R <agent_boot> <number> <kind> <payload hex>   a record
//! D <agent_boot> <number> <kind> <digest>        a record read again, not journalled again
//! S <agent_boot> <epoch> <seq> <end> <digest> <w>  a command, before it was visible;
//!                                                w is 1 when padding went before it
//! X <text>                                       a report
//! ```

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use nanoprobe_sidecar::link::{Command, Sink};
use nanoprobe_transport::codec::Writer;
use nanoprobe_transport::layout::kind;
use nanoprobe_transport::records::{CommandHeader, Hello};

use crate::content;

/// Commands sent and not yet answered, at most.
pub const UNANSWERED: u64 = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    boot: u64,
    last: u64,
    epoch: u64,
}

/// The stub's journal and command source.
pub struct Stub {
    dir: PathBuf,
    journal: BufWriter<File>,
    state: State,
    boot: u64,
    seq: u64,
    sent: u64,
    answered: u64,
    seed: u64,
    echo: bool,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl Stub {
    /// Open the next journal in `dir`. `echo` copies reports to stderr.
    pub fn open(dir: &Path, echo: bool) -> std::io::Result<Stub> {
        std::fs::create_dir_all(dir)?;
        let n = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("journal-"))
            .count();
        let path = dir.join(format!("journal-{:06}.txt", n + 1));
        let journal = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(path)?,
        );
        let state = std::fs::read_to_string(dir.join("state"))
            .ok()
            .and_then(|t| {
                let v: Vec<u64> = t
                    .split_whitespace()
                    .filter_map(|w| u64::from_str_radix(w, 16).ok())
                    .collect();
                (v.len() == 3).then(|| State {
                    boot: v[0],
                    last: v[1],
                    epoch: v[2],
                })
            })
            .unwrap_or_default();
        Ok(Stub {
            dir: dir.to_owned(),
            journal,
            state,
            boot: 0,
            seq: 0,
            sent: 0,
            answered: 0,
            seed: nanoprobe_transport::random_nonzero(),
            echo,
        })
    }

    fn line(&mut self, text: &str) {
        // A failed journal write is caught by `durable`, which flushes.
        let _ = writeln!(self.journal, "{text}");
    }

    fn save(&mut self) -> std::io::Result<()> {
        let tmp = self.dir.join("state.tmp");
        let s = self.state;
        std::fs::write(&tmp, format!("{:x} {:x} {:x}\n", s.boot, s.last, s.epoch))?;
        std::fs::rename(tmp, self.dir.join("state"))
    }
}

impl Sink for Stub {
    fn attached(&mut self, agent_boot: u64, sidecar_boot: u64) {
        self.boot = agent_boot;
        self.sent = 0;
        self.answered = 0;
        self.seq = 0;
        self.line(&format!(
            "A {agent_boot:x} {sidecar_boot:x} {}",
            content::now_ms()
        ));
    }

    fn last_number(&self, agent_boot: u64) -> u64 {
        if self.state.boot == agent_boot {
            self.state.last
        } else {
            0
        }
    }

    fn reread(&mut self, agent_boot: u64, number: u64, kind: u16, payload: &[u8]) {
        let d = content::digest(payload) as u64;
        self.line(&format!("D {agent_boot:x} {number:x} {kind} {d:x}"));
    }

    fn record(
        &mut self,
        agent_boot: u64,
        number: u64,
        kind: u16,
        payload: &[u8],
    ) -> Result<(), String> {
        if u64::from(kind) == kind::RESULT {
            self.answered += 1;
        }
        self.line(&format!(
            "R {agent_boot:x} {number:x} {kind} {}",
            hex(payload)
        ));
        self.state.boot = agent_boot;
        self.state.last = number;
        Ok(())
    }

    fn durable(&mut self) -> Result<(), String> {
        self.journal.flush().map_err(|e| format!("journal: {e}"))?;
        self.save().map_err(|e| format!("state: {e}"))
    }

    fn hello(&mut self, hello: &Hello, number: u64) {
        let epoch = self.state.epoch.max(hello.epoch) + 1;
        self.state.epoch = epoch;
        self.seq = 0;
        // Commands of an earlier epoch now have their answers, or never will.
        self.sent = 0;
        self.answered = 0;
        self.line(&format!(
            "H {:x} {epoch:x} {:x} {:x} {number:x}",
            hello.agent_boot, hello.last_applied_seq, hello.in_head
        ));
        // The epoch must be durable before a command of it is visible.
        if let Err(e) = self.durable() {
            self.report(&format!("the new epoch is not durable: {e}"));
        }
    }

    fn next_command(&mut self) -> Option<Command> {
        if self.sent - self.answered.min(self.sent) >= UNANSWERED {
            return None;
        }
        self.seq += 1;
        let header = CommandHeader {
            seq: self.seq,
            epoch: self.state.epoch,
            ..CommandHeader::default()
        };
        let mut w = Writer::new();
        header.write(&mut w);
        let body = content::command_body(self.seed ^ self.state.epoch << 20 ^ self.seq);
        let mut payload = w.finish();
        payload.extend(body);
        Some(Command {
            opcode: content::OPCODE as u16,
            payload,
        })
    }

    fn sent(&mut self, command: &Command, end: u64, wrapped: bool) {
        self.sent += 1;
        let d = content::digest(&command.payload) as u64;
        // From the command itself: a command held back for a full ring may be
        // written after a resync moved the epoch on.
        let header = CommandHeader::read(&mut nanoprobe_transport::codec::Reader::new(
            &command.payload,
        ))
        .unwrap_or_default();
        let (boot, epoch, seq) = (self.boot, header.epoch, header.seq);
        let w = u8::from(wrapped);
        self.line(&format!("S {boot:x} {epoch:x} {seq:x} {end:x} {d:x} {w}"));
    }

    fn report(&mut self, line: &str) {
        if self.echo {
            eprintln!("stub: {line}");
        }
        self.line(&format!("X {line}"));
    }
}
