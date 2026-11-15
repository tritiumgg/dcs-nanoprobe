//! A [`SimAdapter`] over the real link, for the command-path test (step S14):
//! the reconciler's own send and resolve code against an agent in DCS or in
//! the stock-Lua mock. Test tooling only; the product's adapter is step S17's.
//!
//! [`nanoprobe_sidecar::link::run`] runs on its own thread and calls a
//! [`Sink`] that shares a queue with the adapter. The reconciler calls the
//! adapter from its own thread.
//!
//! The reconciler journals the inbound position just after each command
//! before the command is written ([`SimAdapter::position_after`]). The adapter
//! predicts it with a private producer that starts where the attach put
//! `in_tail` (the `HELLO`'s `in_head`) and moves by each queued command's
//! cost, padding included. The link writes the queue in order, so the
//! prediction holds; [`LinkSim::mispredicted`] counts any write that ends
//! elsewhere, and a run with one fails.
//!
//! Commands still queued when a session ends are dropped: they never reached
//! the ring, and the reconciler resolves them at the next `HELLO` as never
//! taken. Records reach the reconciler in ring order. The link calls
//! [`Sink::hello`] before it passes the `HELLO` on as a record, so the
//! prediction is set before the reconciler reads the `HELLO`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use nanoprobe_sidecar::link::{self, Command, Config, End, Sink};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::records::Hello;
use nanoprobe_transport::ring::{Producer, Ring};
use nanoprobe_transport::status;

#[derive(Default)]
struct Shared {
    /// The inbound ring of the attached agent, once its geometry is read.
    ring: Option<Ring>,
    /// The predicted producer, from the `HELLO` on; `None` while commands
    /// may not be sent.
    predict: Option<Producer>,
    /// Commands queued for the link, with the end each was predicted at.
    queue: VecDeque<(Command, u64)>,
    /// Records read, for [`SimAdapter::read`].
    records: Vec<RawRecord>,
    /// The highest record number read, per agent boot.
    last: HashMap<u64, u64>,
    /// The agent boot of the session.
    agent_boot: u64,
    mispredicted: u64,
    reports: Vec<String>,
    /// `HELLO`s read in this session.
    hellos: u32,
    /// The number of a resync `HELLO` passed to [`Sink::hello`] and not yet
    /// to [`Sink::record`].
    resync: Option<u64>,
}

/// The adapter. Dropping it stops the link thread.
pub struct LinkSim {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<End>>,
}

struct LinkSink {
    shared: Arc<Mutex<Shared>>,
    map_path: std::path::PathBuf,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LinkSim {
    /// Start a sidecar session loop with `cfg`. Each call is a new sidecar
    /// boot: a forced sidecar restart is dropping one and starting another
    /// with the [`LinkSim::last_numbers`] of the old one, which a real
    /// sidecar keeps in its journal, so no record is read twice.
    pub fn start(cfg: Config, last_numbers: HashMap<u64, u64>) -> LinkSim {
        let shared = Arc::new(Mutex::new(Shared {
            last: last_numbers,
            ..Shared::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut sink = LinkSink {
            shared: shared.clone(),
            map_path: cfg.map_path.clone(),
        };
        let s = stop.clone();
        let thread = std::thread::spawn(move || link::run(&cfg, &mut sink, &s));
        LinkSim {
            shared,
            stop,
            thread: Some(thread),
        }
    }

    /// The highest record number read, per agent boot.
    pub fn last_numbers(&self) -> HashMap<u64, u64> {
        lock(&self.shared).last.clone()
    }

    /// Writes that ended where the prediction did not.
    pub fn mispredicted(&self) -> u64 {
        lock(&self.shared).mispredicted
    }

    /// Whether a `HELLO` was read and commands may be sent.
    pub fn ready(&self) -> bool {
        lock(&self.shared).predict.is_some()
    }

    /// Commands queued and not yet written.
    pub fn queued(&self) -> usize {
        lock(&self.shared).queue.len()
    }

    /// The link's diagnostic lines since the last call.
    pub fn take_reports(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.shared).reports)
    }

    /// Stop the link thread and return why its loop ended.
    pub fn stop(mut self) -> Option<End> {
        self.halt()
    }

    fn halt(&mut self) -> Option<End> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().and_then(|t| t.join().ok())
    }
}

impl Drop for LinkSim {
    fn drop(&mut self) {
        self.halt();
    }
}

impl SimAdapter for LinkSim {
    fn position_after(&self, _opcode: u16, payload: &[u8]) -> u64 {
        let s = lock(&self.shared);
        match &s.predict {
            Some(p) => p.tail() + p.cost(payload.len()),
            None => 0,
        }
    }

    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached> {
        let mut s = lock(&self.shared);
        let Some(p) = s.predict.as_mut() else {
            return Err(Detached);
        };
        let end = p.tail() + p.cost(payload.len());
        *p = Producer::new(p.ring(), end);
        s.queue.push_back((
            Command {
                opcode,
                payload: payload.to_vec(),
            },
            end,
        ));
        Ok(end)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        std::mem::take(&mut lock(&self.shared).records)
    }
}

impl Sink for LinkSink {
    fn attached(&mut self, agent_boot: u64, _sidecar_boot: u64) {
        let ring = MappedFile::open_read_only(&self.map_path)
            .ok()
            .and_then(|m| status::check(&m, m.file_len()).ok())
            .map(|g| Ring::inbound(g.out_capacity, g.in_capacity));
        let mut s = lock(&self.shared);
        // A new session: what the last one queued never reached the ring.
        s.queue.clear();
        s.predict = None;
        s.ring = ring;
        s.agent_boot = agent_boot;
        s.hellos = 0;
        s.resync = None;
    }

    fn last_number(&self, agent_boot: u64) -> u64 {
        lock(&self.shared)
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
        kind_: u16,
        payload: &[u8],
    ) -> Result<(), String> {
        let mut s = lock(&self.shared);
        s.last.insert(agent_boot, number);
        let resync = s.resync.take_if(|n| *n == number).is_some();
        s.records.push(RawRecord {
            agent_boot,
            number,
            kind: kind_,
            payload: payload.to_vec(),
            resync,
        });
        Ok(())
    }

    fn durable(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn hello(&mut self, hello: &Hello, number: u64) {
        let mut s = lock(&self.shared);
        s.hellos += 1;
        if s.hellos > 1 {
            s.resync = Some(number);
        }
        let Some(ring) = s.ring else {
            s.reports
                .push("a HELLO before the ring's geometry was read".into());
            return;
        };
        // A second HELLO in an attach is a resync: the ring is untouched and
        // the queue goes on (spec/commands.md, "After CONTROL_LOST").
        if s.predict.is_none() {
            s.predict = Some(Producer::new(ring, hello.in_head));
        }
    }

    fn next_command(&mut self) -> Option<Command> {
        let s = lock(&self.shared);
        s.queue.front().map(|(c, _)| c.clone())
    }

    fn sent(&mut self, _command: &Command, end: u64, _wrapped: bool) {
        let mut s = lock(&self.shared);
        if let Some((_, want)) = s.queue.pop_front()
            && want != end
        {
            s.mispredicted += 1;
            s.reports
                .push(format!("a command ended at {end}, predicted {want}"));
        }
    }

    fn report(&mut self, line: &str) {
        lock(&self.shared).reports.push(line.to_owned());
    }
}
