//! A deterministic simulation of the sidecar's reconciler against the fake
//! sim, with interleaved faults.
//!
//! A [`World`] holds a [`FakeSim`], the durable [`Store`] and the volatile
//! [`Reconciler`]. [`World::apply`] runs one [`Op`]: a fault or some frames.
//! After every frame it checks the safety invariants; [`World::settle`]
//! heals every fault, lets the reconciler converge, and checks that DCS
//! matches the store. [`Schedule`] draws operations from a seed, so a failure
//! names the seed that replays it.
//!
//! Safety, checked on every command and after every frame:
//!
//! - A sequenced command and the transition that led to it are journalled
//!   before the command is written.
//! - A spawn never includes a unit with a death fact.
//! - No spawn replaces a group DCS still holds (no duplicate group).
//! - No order revision reaches DCS twice in one mission generation.
//! - Every death fact is a death that happened.
//! - `dead` is final.
//!
//! Convergence, after [`World::settle`]: every entity under DCS authority is
//! `live`, `dead`, `unspawnable` or `lost`; a `live` group is in DCS with
//! exactly its members that have neither a death fact nor a missing mark,
//! and its order applied; nothing else of the campaign is in DCS.

use std::collections::BTreeSet;

use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::intents::{self, Compiled, Intent, Waypoint};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::reconciler::{Config as RecConfig, Reconciler};
use nanoprobe_sidecar::replay::{Batch, Recording};
use nanoprobe_sidecar::sim::wire::{Body, Command};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::store::{
    Authority, Entry, LostPolicy, NewGroup, Outcome, Point, Store, UnitState,
};

use crate::{Config, FakeSim};

/// One step of a schedule. Indexes pick a group, a unit or an object modulo
/// how many there are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Run some frames.
    Frames(u8),
    /// A unit or static dies.
    Kill(u16),
    /// An object leaves DCS with no event.
    RemoveSilently(u16),
    /// An object leaves DCS with a removal event.
    RemoveWithEvent(u16),
    /// The link drops.
    Detach,
    /// The sidecar attaches again (same sidecar).
    Reattach,
    /// The sidecar process restarts: its volatile state is lost, the store
    /// stays, and it attaches as a new sidecar boot.
    SidecarRestart,
    /// The sidecar dies in the middle of writing a command, after writing
    /// this many others in the next step, then starts again.
    SidecarCrashInWrite(u8),
    /// DCS runs a frame and dies before the sidecar reads it, and stays
    /// down until [`Op::RestartDcs`].
    Crash,
    /// The host starts DCS again, and the sidecar attaches.
    RestartDcs,
    /// A mission load.
    LoadMission,
    /// Move a group's authority: `true` to DCS, `false` to the campaign.
    Authority(u16, bool),
    /// Give a group a new route order.
    Order(u16),
    /// Change a unit's type to one DCS refuses.
    BreakSpec(u16),
    /// Change a unit's type back to one DCS accepts.
    FixSpec(u16),
}

/// A schedule drawn from a seed: a SplitMix64 stream, so the same seed draws
/// the same operations on every platform.
#[derive(Clone, Debug)]
pub struct Schedule {
    state: u64,
}

impl Schedule {
    /// The schedule for a seed.
    pub fn new(seed: u64) -> Schedule {
        Schedule { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// The next operation. Frames are drawn most often; faults less.
    pub fn op(&mut self) -> Op {
        let r = self.next_u64();
        let i = (r >> 16) as u16;
        match r % 100 {
            0..=44 => Op::Frames(1 + (i % 8) as u8),
            45..=54 => Op::Kill(i),
            55..=57 => Op::RemoveSilently(i),
            58..=59 => Op::RemoveWithEvent(i),
            60..=63 => Op::Detach,
            64..=68 => Op::Reattach,
            69..=70 => Op::SidecarRestart,
            71..=72 => Op::SidecarCrashInWrite((i % 4) as u8),
            73..=74 => Op::Crash,
            75..=78 => Op::RestartDcs,
            79..=80 => Op::LoadMission,
            81..=86 => Op::Authority(i, r & (1 << 40) != 0),
            87..=93 => Op::Order(i),
            94..=96 => Op::BreakSpec(i),
            _ => Op::FixSpec(i),
        }
    }
}

/// An invariant failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation(pub String);

/// The fake sim behind an adapter that logs every sequenced command.
struct Logged<'a> {
    sim: &'a mut FakeSim,
    recording: &'a mut Recording,
    sent: Vec<Command>,
    /// Sequenced commands to write before the sidecar dies in the middle of
    /// a write: that command reaches the ring, and nothing after it.
    crash_after: Option<usize>,
    crashed: bool,
}

impl SimAdapter for Logged<'_> {
    fn position_after(&self, op: u16, payload: &[u8]) -> u64 {
        self.sim.position_after(op, payload)
    }

    fn send(&mut self, op: u16, payload: &[u8]) -> Result<u64, Detached> {
        if self.crashed {
            return Err(Detached);
        }
        let r = self.sim.send(op, payload);
        if r.is_ok()
            && Command::decode(op, payload)
                .ok()
                .flatten()
                .is_some_and(|c| !c.body.is_control())
            && let Some(n) = &mut self.crash_after
        {
            if *n == 0 {
                self.crashed = true;
            } else {
                *n -= 1;
            }
        }
        if let Ok(Some(cmd)) = Command::decode(op, payload)
            && !cmd.body.is_control()
        {
            self.sent.push(cmd);
        }
        r
    }

    fn read(&mut self) -> Vec<RawRecord> {
        let rs = self.sim.read();
        if !rs.is_empty() {
            self.recording.batches.push(Batch::Records(rs.clone()));
        }
        rs
    }
}

/// Check each command written in a step against the journal as it stood
/// when the command was journalled: the command is there, journalled before
/// its inbound position was known, with its entity in the state the opcode
/// needs and no unit of a spawn with an earlier death fact.
fn check_journal(store: &Store, sent: &[Command]) -> Result<(), Violation> {
    if sent.is_empty() {
        return Ok(());
    }
    let mut state: std::collections::HashMap<EntityId, State> = std::collections::HashMap::new();
    let mut dead: BTreeSet<EntityId> = BTreeSet::new();
    let mut at: std::collections::HashMap<(u64, u64), (State, BTreeSet<EntityId>, u64)> =
        std::collections::HashMap::new();
    for e in store.entries() {
        match e {
            Entry::Transition { entity, to, .. } => {
                state.insert(*entity, *to);
            }
            Entry::Death { unit } => {
                dead.insert(*unit);
            }
            Entry::Command(c) => {
                let s = state.get(&c.entity).copied().unwrap_or(State::Virtual);
                let d: BTreeSet<EntityId> = c
                    .units
                    .iter()
                    .filter(|u| dead.contains(u))
                    .copied()
                    .collect();
                at.insert((c.epoch, c.seq), (s, d, c.position));
            }
            _ => {}
        }
    }
    for cmd in sent {
        let h = &cmd.header;
        let Some((s, dead_units, position)) = at.get(&(h.epoch, h.seq)) else {
            return Err(Violation(format!(
                "command {} seq {} written and not journalled",
                cmd.body.opcode(),
                h.seq
            )));
        };
        if *position == 0 {
            return Err(Violation(format!(
                "command seq {} journalled with no inbound position",
                h.seq
            )));
        }
        let want = match cmd.body {
            Body::GroupSpawn { .. } | Body::StaticSpawn { .. } => Some(State::Materializing),
            Body::GroupCaptureDestroy | Body::Capture => Some(State::Dematerializing),
            Body::GroupOrder(_) => Some(State::Live),
            _ => None,
        };
        if want.is_some_and(|w| w != *s) {
            return Err(Violation(format!(
                "opcode {} for entity {} journalled in state {}",
                cmd.body.opcode(),
                h.entity,
                s.name()
            )));
        }
        if let Some(u) = dead_units.iter().next() {
            return Err(Violation(format!(
                "spawn of {} includes unit {u} with a death fact",
                h.entity
            )));
        }
    }
    Ok(())
}

/// The store a [`World`] starts with: `groups` groups of three units and
/// `statics` statics, all under DCS authority, on [`THEATER`].
pub fn new_store(groups: usize, statics: usize) -> Store {
    let mut store = Store::new(CampaignId(1_763_000_000_000), 10_000);
    for g in 0..groups {
        let units = (0..3)
            .map(|u| {
                (
                    "M-113".to_owned(),
                    UnitState {
                        at: Point {
                            x: g as f64 * 1_000.0,
                            y: 0.0,
                            z: u as f64 * 20.0,
                        },
                        ..UnitState::default()
                    },
                )
            })
            .collect();
        store
            .create_group(NewGroup {
                theater: THEATER.into(),
                country: 2,
                category: 2,
                units,
                authority: Authority::Sim,
            })
            .expect("ids");
    }
    for s in 0..statics {
        store
            .create_static(NewGroup {
                theater: THEATER.into(),
                country: 2,
                category: 0,
                units: vec![(
                    "Tent".to_owned(),
                    UnitState {
                        at: Point {
                            x: -1_000.0 * (s + 1) as f64,
                            y: 0.0,
                            z: 0.0,
                        },
                        ..UnitState::default()
                    },
                )],
                authority: Authority::Sim,
            })
            .expect("ids");
    }
    store
}

/// What the safety check has read of the journal so far.
#[derive(Debug, Default)]
struct Scan {
    upto: usize,
    journalled: BTreeSet<(u64, u64, u64)>,
    commands: std::collections::HashMap<(u64, u64), u64>,
    boots: std::collections::HashMap<u64, u64>,
    dead: BTreeSet<EntityId>,
}

/// The sidecar and DCS, with faults.
pub struct World {
    scan: Scan,
    /// What the reconciler read, with a marker at each sidecar restart.
    pub recording: Recording,
    /// The fake DCS.
    pub sim: FakeSim,
    /// The durable store.
    pub store: Store,
    /// The volatile reconciler.
    pub rec: Reconciler,
    rec_cfg: RecConfig,
    sidecar_boot: u64,
    /// Operations applied, for a failure report.
    pub trace: Vec<Op>,
}

/// The theater every group is on.
pub const THEATER: &str = "Caucasus";

impl World {
    /// A world with `groups` groups of three units and `statics` statics,
    /// all under DCS authority, and the sidecar attached.
    pub fn new(groups: usize, statics: usize, on_lost: LostPolicy) -> World {
        let sim = FakeSim::new(Config {
            event_queue: 6,
            ..Config::default()
        });
        let store = new_store(groups, statics);
        let rec_cfg = RecConfig {
            materialize_per_step: 2,
            on_lost,
        };
        let rec = Reconciler::new(rec_cfg, &store);
        let mut w = World {
            scan: Scan::default(),
            recording: Recording::default(),
            sim,
            store,
            rec,
            rec_cfg,
            sidecar_boot: 1,
            trace: Vec::new(),
        };
        w.sim.attach(w.sidecar_boot);
        w
    }

    fn groups(&self) -> Vec<EntityId> {
        self.store
            .groups()
            .filter(|g| g.kind == nanoprobe_sidecar::store::Kind::Group)
            .map(|g| g.id)
            .collect()
    }

    fn all(&self) -> Vec<EntityId> {
        self.store.groups().map(|g| g.id).collect()
    }

    fn pick<T: Copy>(v: &[T], i: u16) -> Option<T> {
        (!v.is_empty()).then(|| v[usize::from(i) % v.len()])
    }

    /// Run one frame of DCS and one step of the reconciler, checking every
    /// command it writes and the safety invariants after.
    pub fn tick(&mut self) -> Result<(), Violation> {
        self.tick_crashing(None)
    }

    /// A tick in which the sidecar dies after writing `crash_after` more
    /// sequenced commands, in the middle of the next write, if it gets
    /// that far. It then starts again from the store.
    fn tick_crashing(&mut self, crash_after: Option<usize>) -> Result<(), Violation> {
        self.sim.frame();
        let mut logged = Logged {
            sim: &mut self.sim,
            recording: &mut self.recording,
            sent: Vec::new(),
            crash_after,
            crashed: false,
        };
        self.rec.step(&mut self.store, &mut logged);
        let (sent, crashed) = (logged.sent, logged.crashed);
        check_journal(&self.store, &sent)?;
        if crashed {
            self.restart_sidecar();
        }
        self.check_safety()
    }

    /// Apply one operation, then run a frame.
    pub fn apply(&mut self, op: &Op) -> Result<(), Violation> {
        self.trace.push(op.clone());
        let objects: Vec<u64> = self.sim.objects().into_iter().map(|(o, _)| o).collect();
        match *op {
            Op::Frames(n) => {
                for _ in 1..n {
                    self.tick()?;
                }
            }
            Op::Kill(i) => {
                if let Some(o) = Self::pick(&objects, i) {
                    self.sim.kill(o);
                }
            }
            Op::RemoveSilently(i) => {
                if let Some(o) = Self::pick(&objects, i) {
                    self.sim.remove_silently(o);
                }
            }
            Op::RemoveWithEvent(i) => {
                if let Some(o) = Self::pick(&objects, i) {
                    self.sim.remove_with_event(o);
                }
            }
            Op::Detach => self.sim.detach(),
            Op::Reattach => {
                if !self.sim.attached() {
                    self.sim.attach(self.sidecar_boot);
                }
            }
            Op::SidecarRestart => self.restart_sidecar(),
            Op::SidecarCrashInWrite(n) => return self.tick_crashing(Some(usize::from(n))),
            Op::Crash => {
                // DCS dies after a frame and before the sidecar reads it: the
                // commands it took in that frame lose their results.
                self.sim.frame();
                self.sim.crash();
            }
            Op::RestartDcs => {
                self.sim.restart_dcs();
                self.sim.attach(self.sidecar_boot);
            }
            Op::LoadMission => self.sim.load_mission(),
            Op::Authority(i, sim) => {
                if let Some(g) = Self::pick(&self.all(), i) {
                    let a = if sim {
                        Authority::Sim
                    } else {
                        Authority::Campaign
                    };
                    self.store.set_authority(g, a).expect("group");
                }
            }
            Op::Order(i) => {
                if let Some(g) = Self::pick(&self.groups(), i) {
                    let base = f64::from(i % 7) * 300.0;
                    let intent = Intent::Route {
                        waypoints: vec![
                            Waypoint { x: base, z: 500.0 },
                            Waypoint {
                                x: base + 400.0,
                                z: 900.0,
                            },
                        ],
                        speed: 10.0,
                        on_road: false,
                    };
                    self.store.set_order(g, intent).expect("group");
                }
            }
            Op::BreakSpec(i) | Op::FixSpec(i) => {
                if let Some(g) = Self::pick(&self.all(), i) {
                    let unit = self.store.group(g).expect("group").members[0];
                    let broken = matches!(op, Op::BreakSpec(_));
                    let (now, base) = {
                        let u = self.store.unit(unit).expect("unit");
                        let base = if g == u.group
                            && self.store.group(g).expect("g").kind
                                == nanoprobe_sidecar::store::Kind::Static
                        {
                            "Tent"
                        } else {
                            "M-113"
                        };
                        (u.type_name.clone(), base)
                    };
                    let want = if broken {
                        format!("Unknown-{base}")
                    } else {
                        base.to_owned()
                    };
                    // A spec change of a live group is not reconciled in this
                    // step (no transition re-spawns it); only virtual and
                    // unspawnable groups take one.
                    let state = self.store.group(g).expect("group").state;
                    if now != want && matches!(state, State::Virtual | State::Unspawnable) {
                        self.store.set_unit_type(unit, &want).expect("unit");
                    }
                }
            }
        }
        self.tick()
    }

    fn restart_sidecar(&mut self) {
        self.recording.batches.push(Batch::SidecarRestart);
        self.sim.detach();
        self.sidecar_boot += 1;
        self.rec = Reconciler::new(self.rec_cfg, &self.store);
        self.sim.attach(self.sidecar_boot);
    }

    fn check_safety(&mut self) -> Result<(), Violation> {
        let l = self.sim.ledger();
        if l.replacements != 0 {
            return Err(Violation(format!(
                "{} spawns replaced a group DCS held",
                l.replacements
            )));
        }
        if let Some((k, n)) = l.orders.iter().find(|(_, n)| **n > 1) {
            return Err(Violation(format!("order {k:?} reached DCS {n} times")));
        }
        for g in self.store.groups() {
            for m in &g.members {
                let u = self.store.unit(*m).expect("unit");
                if u.dead && !l.deaths.contains(&m.0) {
                    return Err(Violation(format!(
                        "unit {m} has a death fact and did not die"
                    )));
                }
            }
        }
        // The journal entries added since the last check.
        let entries = &self.store.entries()[self.scan.upto..];
        let scan = &mut self.scan;
        for e in entries {
            match e {
                Entry::Command(c) => {
                    scan.journalled.insert((c.agent_boot, c.epoch, c.seq));
                    scan.commands.insert((c.epoch, c.seq), c.agent_boot);
                    if *scan.boots.entry(c.epoch).or_insert(c.agent_boot) != c.agent_boot {
                        return Err(Violation(format!(
                            "epoch {} used for two agent boots",
                            c.epoch
                        )));
                    }
                }
                Entry::Outcome {
                    epoch,
                    seq,
                    outcome,
                } => {
                    let boot = scan.commands.get(&(*epoch, *seq)).copied().unwrap_or(0);
                    let taken = l.taken.contains(&(boot, *epoch, *seq));
                    let wrong = match outcome {
                        Outcome::NotSent => taken,
                        // An Unknown for a command of an agent boot that is
                        // gone may be one it never took (spec/commands.md).
                        Outcome::Unknown => !taken && boot == self.sim.agent_boot(),
                        _ => !taken,
                    };
                    if wrong {
                        return Err(Violation(format!(
                            "command epoch {epoch} seq {seq} resolved {outcome:?}, taken: {taken}"
                        )));
                    }
                }
                Entry::Transition { entity, from, to } => {
                    if *from == State::Dead && scan.dead.contains(entity) {
                        return Err(Violation(format!("entity {entity} left dead")));
                    }
                    if *to == State::Dead {
                        scan.dead.insert(*entity);
                    }
                }
                _ => {}
            }
        }
        scan.upto = self.store.entries().len();
        if let Some(t) = l.taken.difference(&scan.journalled).next() {
            return Err(Violation(format!(
                "DCS took command {t:?}, which is not journalled"
            )));
        }
        Ok(())
    }

    /// Heal every fault, run until the reconciler has converged, and check
    /// that DCS matches the store.
    pub fn settle(&mut self) -> Result<(), Violation> {
        if !self.sim.alive() {
            self.sim.restart_dcs();
        }
        if !self.sim.attached() {
            self.sim.attach(self.sidecar_boot);
        }
        // Long enough for a load, several enumerations and every spawn at
        // two per step.
        let frames = 400 + 4 * self.store.groups().count();
        for _ in 0..frames {
            self.tick()?;
        }
        self.check_converged()
    }

    fn check_converged(&self) -> Result<(), Violation> {
        let held = self.sim.platform_groups();
        let bad = |m: String| Err(Violation(m));
        let ledger = self.sim.ledger();
        let (boot, mission_gen) = (self.sim.agent_boot(), self.sim.mission_gen());
        if self
            .store
            .loaded()
            .is_none_or(|l| l.agent_boot != boot || l.mission_gen != mission_gen)
        {
            return bad(format!(
                "the store has {:?} loaded, DCS {boot:#x} gen {mission_gen}",
                self.store.loaded()
            ));
        }
        for g in self.store.groups() {
            let in_dcs = held.iter().find(|(e, _, _)| *e == g.id.0);
            let want_dcs = g.authority == Authority::Sim;
            match g.state {
                State::Live => {
                    if !want_dcs {
                        return bad(format!("{} live under campaign authority", g.id));
                    }
                    let Some((_, _, units)) = in_dcs else {
                        return bad(format!("{} live and not in DCS", g.id));
                    };
                    let want: BTreeSet<u64> = g
                        .members
                        .iter()
                        .filter(|m| {
                            let u = self.store.unit(**m).expect("unit");
                            !u.dead && !u.missing
                        })
                        .map(|m| m.0)
                        .collect();
                    let have: BTreeSet<u64> = units.iter().copied().collect();
                    if want != have {
                        return bad(format!(
                            "{} live with units {have:?}, store has {want:?}",
                            g.id
                        ));
                    }
                    if let Some(order) = &g.order {
                        let lead = g
                            .members
                            .iter()
                            .map(|m| self.store.unit(*m).expect("unit"))
                            .find(|u| !u.dead);
                        let status = intents::Status {
                            at: lead.map(|u| (u.state.at.x, u.state.at.z)),
                            progress: g.progress,
                        };
                        let done =
                            matches!(intents::compile(order, status, |_| None), Compiled::Done);
                        if !done && self.sim.applied_order(g.id.0) != g.order_rev {
                            return bad(format!(
                                "{} holds order {} in DCS, store has {}",
                                g.id,
                                self.sim.applied_order(g.id.0),
                                g.order_rev
                            ));
                        }
                    }
                }
                State::Dead | State::Unspawnable | State::Lost | State::Virtual => {
                    let why = match g.state {
                        State::Dead => {
                            g.members.iter().all(|m| ledger.deaths.contains(&m.0))
                                || g.on_lost.unwrap_or(self.rec_cfg.on_lost) == LostPolicy::Dead
                        }
                        State::Unspawnable => g.members.iter().any(|m| {
                            self.store
                                .unit(*m)
                                .expect("unit")
                                .type_name
                                .starts_with("Unknown")
                        }),
                        State::Lost => g.members.iter().any(|m| {
                            let k = (boot, mission_gen, m.0);
                            ledger.removed.contains(&k) || ledger.unreported.contains(&k)
                        }),
                        _ => true,
                    };
                    if !why {
                        return bad(format!(
                            "{} is {} with no cause in DCS",
                            g.id,
                            g.state.name()
                        ));
                    }
                    if in_dcs.is_some() {
                        return bad(format!("{} is {} and in DCS", g.id, g.state.name()));
                    }
                    if g.state == State::Virtual && want_dcs {
                        return bad(format!("{} virtual under DCS authority", g.id));
                    }
                }
                State::Materializing | State::Dematerializing => {
                    return bad(format!("{} still {}", g.id, g.state.name()));
                }
            }
        }
        if let Some((e, n, _)) = held
            .iter()
            .find(|(e, _, _)| self.store.group(EntityId(*e)).is_none())
        {
            return bad(format!("DCS holds {n} ({e}), which the store does not"));
        }
        if self.rec.inflight().count() != 0 {
            return bad(format!(
                "{} commands unanswered",
                self.rec.inflight().count()
            ));
        }
        Ok(())
    }
}

/// Run a seed's schedule for `ops` operations, then settle. On failure the
/// error names the seed and the operation that broke an invariant.
pub fn run_seed(seed: u64, ops: usize, groups: usize, statics: usize) -> Result<(), String> {
    let policy = [LostPolicy::Report, LostPolicy::Respawn, LostPolicy::Dead][(seed % 3) as usize];
    let mut w = World::new(groups, statics, policy);
    let mut s = Schedule::new(seed);
    for i in 0..ops {
        let op = s.op();
        if let Err(Violation(v)) = w.apply(&op) {
            return Err(format!("seed {seed:#x}, op {i} {op:?}: {v}"));
        }
    }
    w.settle()
        .map_err(|Violation(v)| format!("seed {seed:#x}, settling after {ops} ops: {v}"))
}
