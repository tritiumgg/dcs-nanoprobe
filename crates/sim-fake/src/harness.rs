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
//! - A sequenced command is journalled, with its inbound position, before it
//!   is written, and the transition that led to it before that: a sidecar
//!   that dies in the middle of a write ([`Op::SidecarCrashInWrite`]) keeps
//!   only what it journalled first, and every command DCS took must be in
//!   the journal.
//! - A spawn never includes a unit with a death fact.
//! - No spawn replaces a group DCS still holds (no duplicate group).
//! - Every death fact is a death that happened; `dead` is final.
//! - Every outcome matches what the agent did: `not_sent` was never taken,
//!   `unknown` was taken unless its agent boot is gone, and `applied` was
//!   taken, or is a spawn of a revision DCS applied in that generation.
//! - One epoch never spans two agent boots.
//!
//! Convergence, after [`World::settle`]: every entity is `live`, `dead`,
//! `unspawnable`, `lost` or `virtual`, each with a cause; a `live` group is
//! in DCS with exactly its members that have neither a death fact nor a
//! missing mark, holding its order, and an attack order names its target's
//! current DCS group; no entity outside `dead` has every member dead; and
//! nothing else of the campaign is in DCS.

use std::collections::BTreeSet;

use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::intents::{self, Compiled, Intent, Waypoint};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::reconciler::{Config as RecConfig, Reconciler};
use nanoprobe_sidecar::replay::{Batch, Recording};
use nanoprobe_sidecar::sim::wire::{Body, Command, lv, opcode};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::store::{
    Authority, Entry, Journalled, LostPolicy, NewGroup, NewUnit, Outcome, Point, Store, UnitState,
};
use nanoprobe_transport::layout::result as rc;

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
    /// The next `RESULT` is lost and the link drops with it; its command
    /// stays unanswered until the next attach's `HELLO`.
    LoseResult,
    /// Give a group an attack order on another group.
    Attack(u16, u16),
    /// A mission load on a theater none of the groups is on.
    LoadOtherTheater,
    /// The next destroys, this many, raise in DCS.
    RefuseDestroys(u8),
    /// The link drops, and a sidecar of another campaign has the agent switch
    /// to it (`CAMPAIGN`); that campaign's group, whose `EntityId`s are this
    /// campaign's too, is in DCS. This campaign's sidecar switches back when
    /// it next attaches.
    ForeignCampaign,
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
        let j = (r >> 32) as u16;
        match r % 100 {
            0..=39 => Op::Frames(1 + (i % 8) as u8),
            40..=41 => Op::LoseResult,
            42..=43 => Op::Attack(i, j),
            44 => Op::LoadOtherTheater,
            45 => Op::RefuseDestroys(1 + (i % 5) as u8),
            46 => Op::ForeignCampaign,
            47..=54 => Op::Kill(i),
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

/// The outcome a result code gives, from the table in spec/errors.md,
/// "Command outcomes", written here apart from the reconciler's own mapping.
fn spec_outcome(code: u16, op: u16) -> Outcome {
    match u64::from(code) {
        rc::APPLIED | rc::AT_REVISION => Outcome::Applied,
        rc::TOO_LARGE if op == opcode::CAPTURE => Outcome::Refused,
        rc::TOO_LARGE => Outcome::Applied,
        rc::DCS_REFUSED | rc::NOT_BOUND | rc::LUA_ERROR => Outcome::Refused,
        rc::STALE_EPOCH
        | rc::WRONG_CAMPAIGN
        | rc::WRONG_GENERATION
        | rc::NO_CAMPAIGN
        | rc::BAD_PAYLOAD
        | rc::UNKNOWN_OPCODE
        | rc::RAW_LUA_OFF => Outcome::Rejected,
        // DUPLICATE says only that the number was handled before; it never
        // becomes `applied`.
        _ => Outcome::Unknown,
    }
}

/// The fake sim behind an adapter that logs every sequenced command.
struct Logged<'a> {
    sim: &'a mut FakeSim,
    recording: &'a mut Recording,
    sent: Vec<Command>,
    /// Sequenced commands to write before the sidecar dies in the middle of
    /// a write: that command reaches the ring, and the sidecar stops there.
    crash_after: Option<usize>,
}

/// The panic that stops the sidecar in the middle of a write.
struct Crash;

/// Keep the [`Crash`] panic out of the test output; any other panic is
/// reported as before.
fn quiet_crash_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<Crash>().is_none() {
                earlier(info);
            }
        }));
    });
}

impl SimAdapter for Logged<'_> {
    fn position_after(&self, op: u16, payload: &[u8]) -> u64 {
        self.sim.position_after(op, payload)
    }

    fn send(&mut self, op: u16, payload: &[u8]) -> Result<u64, Detached> {
        let r = self.sim.send(op, payload);
        if let Ok(Some(cmd)) = Command::decode(op, payload)
            && !cmd.body.is_control()
            && r.is_ok()
        {
            self.sent.push(cmd);
            if let Some(n) = &mut self.crash_after {
                if *n == 0 {
                    // The command is in the ring; the sidecar dies before
                    // the write returns.
                    std::panic::panic_any(Crash);
                }
                *n -= 1;
            }
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

/// Check each command written in a step against the journal: it is there,
/// with a nonzero inbound position, its entity in the state the opcode needs
/// as of that entry, and no unit of a spawn with an earlier death fact. That
/// it was journalled before the write, and not after, is what
/// [`Op::SidecarCrashInWrite`] checks.
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

/// The campaign a [`World`] runs.
pub const CAMPAIGN: u64 = 1_763_000_000_000;

/// The store a [`World`] starts with: `groups` groups of three units and
/// `statics` statics, all under DCS authority, on [`THEATER`].
pub fn new_store(groups: usize, statics: usize) -> Store {
    new_store_for(CAMPAIGN, groups, statics)
}

/// [`new_store`] for another campaign: the same `EntityId`s.
pub fn new_store_for(campaign: u64, groups: usize, statics: usize) -> Store {
    let mut store = Store::new(CampaignId(campaign), 10_000);
    for g in 0..groups {
        let units = (0..3)
            .map(|u| NewUnit {
                label: format!("tank {g}.{u}"),
                skill: "Average".into(),
                extra: lv::obj(vec![("livery_id", lv::s("desert"))]),
                ..NewUnit::new(
                    "M-113",
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
                coalition: 2,
                units,
                authority: Authority::Sim,
                label: format!("platoon {g}"),
                extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
            })
            .expect("ids");
    }
    for s in 0..statics {
        store
            .create_static(NewGroup {
                theater: THEATER.into(),
                country: 2,
                category: 0,
                coalition: 2,
                units: vec![NewUnit::new(
                    "Tent",
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
                label: format!("tent {s}"),
                extra: lv::obj(vec![]),
            })
            .expect("ids");
    }
    store
}

/// What the safety check has read of the journal so far.
#[derive(Debug, Default)]
struct Scan {
    upto: usize,
    /// Commands taken before this store took over, which its journal does
    /// not hold.
    before: BTreeSet<(u64, u64, u64)>,
    journalled: BTreeSet<(u64, u64, u64)>,
    commands: std::collections::HashMap<(u64, u64), Journalled>,
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
/// The campaign of [`Op::ForeignCampaign`].
pub const FOREIGN_CAMPAIGN: u64 = 1_700_000_000_000;

/// A theater no group is on.
pub const OTHER_THEATER: &str = "Syria";

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
            materialize_per_frame: 2,
            on_lost,
            destroy_retries: 3,
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

    /// The units of this campaign alive in DCS: object ID and unit entity.
    pub fn units_in_dcs(&self) -> Vec<(u64, u64)> {
        let c = self.store.campaign().0;
        self.sim
            .objects()
            .into_iter()
            .filter(|(_, oc, e)| *oc == c && *e != 0)
            .map(|(o, _, e)| (o, e))
            .collect()
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
        quiet_crash_panics();
        self.sim.frame();
        let mut logged = Logged {
            sim: &mut self.sim,
            recording: &mut self.recording,
            sent: Vec::new(),
            crash_after,
        };
        let (rec, store) = (&mut self.rec, &mut self.store);
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rec.step(store, &mut logged)
        }));
        let crashed = match step {
            Ok(()) => false,
            Err(p) if p.downcast_ref::<Crash>().is_some() => true,
            Err(p) => std::panic::resume_unwind(p),
        };
        let sent = logged.sent;
        check_journal(&self.store, &sent)?;
        if crashed {
            self.restart_sidecar();
        }
        self.check_safety()
    }

    /// Apply one operation, then run a frame.
    pub fn apply(&mut self, op: &Op) -> Result<(), Violation> {
        self.trace.push(op.clone());
        let objects: Vec<u64> = self.sim.objects().into_iter().map(|(o, _, _)| o).collect();
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
            Op::LoadOtherTheater => self.sim.load_mission_on(OTHER_THEATER),
            Op::LoseResult => self.sim.lose_next_result(),
            Op::RefuseDestroys(n) => self.sim.refuse_destroys(u32::from(n)),
            Op::ForeignCampaign => {
                self.sim.detach();
                self.sim.set_campaign(FOREIGN_CAMPAIGN);
                if self.sim.platform_groups(FOREIGN_CAMPAIGN).is_empty() {
                    self.sim.add_campaign_group(FOREIGN_CAMPAIGN, 1, &[2, 3, 4]);
                }
            }
            Op::Attack(i, j) => {
                let groups = self.groups();
                if let (Some(a), Some(t)) = (Self::pick(&groups, i), Self::pick(&groups, j))
                    && a != t
                {
                    self.store
                        .set_order(a, Intent::Attack { target: t })
                        .expect("group");
                }
            }
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

    /// A sidecar with another store takes over: the link drops, and the new
    /// sidecar attaches as a new sidecar boot. With a store of another
    /// campaign it switches the agent to that campaign.
    pub fn sidecar_of(&mut self, store: Store) {
        self.recording.batches.push(Batch::SidecarRestart);
        self.sim.detach();
        self.sidecar_boot += 1;
        self.store = store;
        self.scan = Scan {
            before: self.sim.ledger().taken.keys().copied().collect(),
            ..Scan::default()
        };
        self.rec = Reconciler::new(self.rec_cfg, &self.store);
        self.sim.attach(self.sidecar_boot);
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
        for g in self.store.groups() {
            for m in &g.members {
                let u = self.store.unit(*m).expect("unit");
                if u.dead && !l.deaths.contains(&(self.store.campaign().0, m.0)) {
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
                    scan.commands.insert((c.epoch, c.seq), c.clone());
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
                    let cmd = scan.commands.get(&(*epoch, *seq));
                    let boot = cmd.map_or(0, |c| c.agent_boot);
                    // A spawn still unanswered when its group's last member
                    // died resolves `applied` (spec/world.md) without a
                    // result: DCS applied that revision in that generation,
                    // and the agent would answer the spawn `AT_REVISION`.
                    let spawned = cmd.is_some_and(|c| {
                        matches!(c.opcode, opcode::GROUP_SPAWN | opcode::STATIC_SPAWN)
                            && l.spawns.iter().any(|s| {
                                (s.0, s.1, s.2, s.3)
                                    == (c.agent_boot, c.mission_gen, c.entity.0, c.rev)
                            })
                    });
                    let taken = l.taken.contains_key(&(boot, *epoch, *seq));
                    // The outcome the agent's own result gives, where it gave
                    // one (read or not).
                    let by_result = cmd.and_then(|c| {
                        l.results
                            .get(&(c.agent_boot, *epoch, *seq))
                            .map(|code| spec_outcome(*code, c.opcode))
                    });
                    if let Some(want) = by_result
                        && matches!(
                            outcome,
                            Outcome::Applied | Outcome::Refused | Outcome::Rejected
                        )
                        && *outcome != want
                        && !(spawned && *outcome == Outcome::Applied)
                    {
                        return Err(Violation(format!(
                            "command epoch {epoch} seq {seq} resolved {outcome:?}, the agent's result gives {want:?}"
                        )));
                    }
                    let wrong = match outcome {
                        Outcome::NotSent => taken,
                        // An Unknown for a command of an agent boot that is
                        // gone may be one it never took (spec/commands.md).
                        // A result written to the ring is read: `unknown` is
                        // for a result that was lost.
                        Outcome::Unknown => {
                            (!taken && boot == self.sim.agent_boot())
                                || (l.delivered.contains(&(boot, *epoch, *seq))
                                    && by_result != Some(Outcome::Unknown))
                        }
                        Outcome::Applied => !taken && !spawned,
                        _ => !taken,
                    };
                    if wrong {
                        return Err(Violation(format!(
                            "command epoch {epoch} seq {seq} resolved {outcome:?}, taken: {taken}"
                        )));
                    }
                }
                Entry::Transition {
                    entity, from, to, ..
                } => {
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
        let campaign = self.store.campaign().0;
        if let Some((t, _)) = l.taken.iter().find(|(t, c)| {
            **c == campaign && !scan.journalled.contains(t) && !scan.before.contains(t)
        }) {
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
        self.sim.refuse_destroys(0);
        // Attach again, as after a resync: a `HELLO` settles every command
        // whose result was lost.
        self.sim.detach();
        self.sim.attach(self.sidecar_boot);
        // Long enough for a load, several enumerations and every spawn at
        // two per step.
        let frames = 400 + 4 * self.store.groups().count();
        for _ in 0..frames {
            self.tick()?;
        }
        self.check_converged()
    }

    fn check_converged(&self) -> Result<(), Violation> {
        let campaign = self.store.campaign().0;
        let held = self.sim.platform_groups(campaign);
        let bad = |m: String| Err(Violation(m));
        let ledger = self.sim.ledger();
        let policy =
            |g: &nanoprobe_sidecar::store::Group| g.on_lost.unwrap_or(self.rec_cfg.on_lost);
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
            let want_dcs = g.authority == Authority::Sim && g.theater == self.sim.theater();
            if g.state != State::Dead && self.store.all_dead(g.id) {
                return bad(format!(
                    "{} is {} with every member dead",
                    g.id,
                    g.state.name()
                ));
            }
            match g.state {
                State::Live => {
                    if !want_dcs {
                        return bad(format!("{} live and not under DCS authority", g.id));
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
                    if let Some(Intent::Attack { target }) = &g.order
                        && let Some((_, aim)) = self.sim.attack_of(campaign, g.id.0)
                        && let Some((target_gid, _)) = self.sim.attack_of(campaign, target.0)
                        && aim != Some(target_gid)
                    {
                        return bad(format!(
                            "{} attacks DCS group {aim:?}, its target {target} is DCS group {target_gid}",
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
                        let compiled = intents::compile(order, status, |t| {
                            self.sim.attack_of(campaign, t.0).map(|(gid, _)| gid)
                        });
                        let needed = matches!(compiled, Compiled::Task(_));
                        if needed && self.sim.applied_order(g.id.0) != g.order_rev {
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
                        // Every member died, or the group was lost and the
                        // policy marked it dead.
                        // Every member died, or it was lost and the policy is
                        // to mark it dead.
                        State::Dead => {
                            g.members.iter().all(|m| ledger.deaths.contains(&(campaign, m.0)))
                                || (policy(g) == LostPolicy::Dead
                                    && self.store.entries().iter().any(|e| {
                                        matches!(e, Entry::Transition { entity, from: State::Lost, to: State::Dead, .. }
                                            if *entity == g.id)
                                    }))
                        }
                        State::Unspawnable => g.members.iter().any(|m| {
                            self.store
                                .unit(*m)
                                .expect("unit")
                                .type_name
                                .starts_with("Unknown")
                        }),
                        State::Lost => {
                            g.members.iter().any(|m| {
                                let k = (boot, mission_gen, campaign, m.0);
                                ledger.removed.contains(&k) || ledger.unreported.contains(&k)
                            }) || (g.error.as_deref().is_some_and(|e| e.starts_with("destroy failed"))
                                && ledger.failed_destroys.get(&(campaign, g.id.0)).copied().unwrap_or(0)
                                    >= self.rec_cfg.destroy_retries)
                        }
                        _ => true,
                    };
                    if g.state == State::Lost
                        && policy(g) == LostPolicy::Respawn
                        && want_dcs
                        && in_dcs.is_none()
                    {
                        return bad(format!(
                            "{} is lost under the respawn policy and was not respawned",
                            g.id
                        ));
                    }
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
                        return bad(format!(
                            "{} virtual under DCS authority on the loaded theater",
                            g.id
                        ));
                    }
                }
                State::Materializing | State::Dematerializing => {
                    return bad(format!("{} still {}", g.id, g.state.name()));
                }
            }
        }
        // Every death a record reported is a fact: a reported death of a
        // unit of this campaign, unless its group was marked dead whole.
        for (_, _, c, e) in &ledger.reported_deaths {
            if *c != campaign {
                continue;
            }
            let Some(u) = self.store.unit(EntityId(*e)) else {
                continue;
            };
            let marked = self
                .store
                .group(u.group)
                .is_some_and(|g| g.state == State::Dead);
            if !u.dead && !marked {
                return bad(format!("unit {e}'s death was reported and is no fact"));
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
