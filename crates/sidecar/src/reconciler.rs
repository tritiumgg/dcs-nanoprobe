//! The reconciler (plan 4.4; spec/world.md, "Lifecycle"; spec/commands.md).
//!
//! Level-triggered: records are hints, the full enumeration is truth. Each
//! [`Reconciler::step`] reads what the agent wrote, folds it into the store,
//! then compares the store's desired state with what DCS holds and writes the
//! commands that close the difference. Facts (deaths) change the desired
//! state; drift (a group that should exist and does not, an order DCS has
//! not been given, an object that should be gone) is corrected.
//!
//! Every transition and every sequenced command is journalled before the
//! command that follows from it is written. The reconciler's own state,
//! the binding mirror included, is volatile: after a sidecar restart a new
//! one is built from the store ([`Reconciler::new`]), re-reads the binding
//! from the agent's enumeration, and resolves the commands it finds
//! unanswered at the agent's next `HELLO`.
//!
//! A death fact comes only from `OBJECT_GONE` with reason `DEAD`, which the
//! agent writes only for rows of its table: units and statics, never
//! scenery. `EVENT` records are hints and are counted, not read.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{gap_class, kind, lifecycle as lc, result as rc};
use nanoprobe_transport::records::{CommandHeader, CommandResult, Hello};

use crate::ids::{self, CampaignId, EntityId, NameKind};
use crate::intents::{self, Compiled, Intent};
use crate::lifecycle::{State, Trigger};
use crate::sim::wire::{Body, Command, Input, Object, ObjectGone, Seen, gone_reason, lv, opcode};
use crate::sim::{RawRecord, SimAdapter};
use crate::store::{
    Authority, Entry, Journalled, Kind, LeftAs, Loaded, LostPolicy, Outcome, Point, Store,
    UnitState,
};

/// The reconciler's settings (spec/config.md, `[world]`).
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Groups spawned per step; the sidecar steps once per frame it reads.
    pub materialize_per_frame: usize,
    /// The default policy for a lost group.
    pub on_lost: LostPolicy,
    /// Refused or unknown destroys of one entity before the reconciler stops
    /// sending them: a dematerializing entity goes `lost`. At least 1; a 0
    /// counts as 1.
    pub destroy_retries: u32,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            materialize_per_frame: 1,
            on_lost: LostPolicy::Report,
            destroy_retries: 3,
        }
    }
}

/// What the reconciler counted, for tests and metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Records that did not decode.
    pub undecodable: u64,
    /// Records of a kind the reconciler does not read.
    pub other: u64,
    /// Results for no command the reconciler holds.
    pub stray_results: u64,
    /// Objects with a platform name of another campaign.
    pub foreign: u64,
    /// Objects with no platform name.
    pub unmanaged: u64,
    /// Enumerations used.
    pub enumerations: u64,
    /// Enumerations thrown away: a count mismatch or a gap.
    pub enumerations_discarded: u64,
    /// `EVENT` records read.
    pub events: u64,
    /// Records about an entity already `dead`, which change nothing.
    pub after_dead: u64,
    /// `OBJECT` and `OBJECT_GONE` records stamped with another campaign than
    /// the store's (campaign 0 included): their binding names no entity of
    /// this campaign, and they are dropped.
    pub other_campaign: u64,
    /// `OBJECT_GONE` records whose binding names no unit of the group it
    /// gives.
    pub bad_binding: u64,
    /// Death facts for a member that had left as `MISSING` or `REMOVED`.
    pub late_deaths: u64,
    /// Death facts that corrected a `REMOVED`: the agent's classifier called
    /// a kill a removal.
    pub classifier_faults: u64,
    /// Death facts for an object from before a respawn: journalled, and they
    /// mark nothing.
    pub stale_deaths: u64,
    /// `MISSING` and `REMOVED` records for an object from before a respawn.
    pub stale_leaves: u64,
    /// Units a death fact named that an `OBJECT` showed alive again.
    pub revivals: u64,
    /// Lost groups the respawn policy held back: a member left as `MISSING`
    /// or `REMOVED` and nothing explains it. One count per group each time it
    /// begins to be held.
    pub respawns_held: u64,
    /// `ENUMERATE` commands sent because the reconciler asked for whole
    /// `OBJECT` records (a `SEEN` of an object it could not resolve).
    pub enumerate_requests: u64,
}

impl Counters {
    /// Add another reconciler's counts.
    pub fn add(&mut self, o: Counters) {
        self.undecodable += o.undecodable;
        self.other += o.other;
        self.stray_results += o.stray_results;
        self.foreign += o.foreign;
        self.unmanaged += o.unmanaged;
        self.enumerations += o.enumerations;
        self.enumerations_discarded += o.enumerations_discarded;
        self.events += o.events;
        self.after_dead += o.after_dead;
        self.bad_binding += o.bad_binding;
        self.other_campaign += o.other_campaign;
        self.late_deaths += o.late_deaths;
        self.classifier_faults += o.classifier_faults;
        self.stale_deaths += o.stale_deaths;
        self.stale_leaves += o.stale_leaves;
        self.revivals += o.revivals;
        self.respawns_held += o.respawns_held;
        self.enumerate_requests += o.enumerate_requests;
    }
}

/// One row of the binding mirror: a DCS object of the loaded mission and the
/// entities it is bound to (plan 4.4, "Binding").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The unit, or the static.
    pub unit: EntityId,
    /// Its group, or the static.
    pub group: EntityId,
    /// The DCS group ID, 0 for a static.
    pub dcs_group: u64,
}

#[derive(Clone, Debug)]
struct EnumRun {
    id: u64,
    before: BTreeSet<u64>,
    found: BTreeSet<u64>,
    /// `OBJECT` records of the enumeration.
    count: u32,
    /// `SEEN` IDs of the enumeration.
    seen: u32,
    gap: bool,
}

/// The reconciler.
#[derive(Debug)]
pub struct Reconciler {
    cfg: Config,
    /// The boot of the agent last attached, 0 for none since this start.
    agent_boot: u64,
    /// A `HELLO` was read and no write has failed since.
    attached: bool,
    /// The epoch in use, 0 until the first `HELLO` after a start.
    epoch: u64,
    next_seq: u64,
    lifecycle: u32,
    /// The agent boot, generation and active campaign of the last `FRAME`,
    /// replays included, which stamp the records after it.
    stamp: (u64, u32, u64),
    inflight: BTreeMap<(u64, u64), Journalled>,
    /// The binding mirror, by DCS object ID, for the loaded mission.
    binding: HashMap<u64, Binding>,
    run: Option<EnumRun>,
    /// Objects whose `OBJECT` named a unit the store does not hold, with the
    /// unit and whether an enumeration was asked for since. A `SEEN` for one
    /// cannot resolve it. Once the store holds the unit, the first `SEEN` of
    /// the object asks for an enumeration of whole `OBJECT` records, once. An
    /// object whose unit the store never gains asks for nothing: a repeated
    /// `OBJECT` cannot resolve it.
    unresolved: BTreeMap<u64, (EntityId, bool)>,
    /// A `GAP` of events was read and no enumeration without `SEEN` IDs was
    /// used since: an enumeration with `SEEN` IDs is discarded.
    guard: bool,
    enum_wanted: bool,
    /// The order revision the agent holds per group, as far as the reconciler
    /// knows, in the loaded generation.
    order_held: HashMap<EntityId, u64>,
    /// An order revision DCS refused; not sent again until it changes.
    order_refused: HashMap<EntityId, u64>,
    /// Drift destroys (of an entity DCS should not hold) refused or unknown,
    /// by entity, in the loaded generation. At the limit they stop.
    drift_destroy_failures: HashMap<EntityId, u32>,
    /// A spawn the agent rejected with a code that a resend cannot fix, by
    /// spec revision and generation; not sent again until either changes.
    spawn_rejected: HashMap<EntityId, (u64, u32)>,
    /// `CAMPAIGN` was written since the last `HELLO`.
    campaign_sent: bool,
    /// A full enumeration was used since this reconciler started, since the
    /// last `HELLO` and since the last `GAP` of events: until then the mirror
    /// may lack objects DCS holds.
    enumerated: bool,
    /// Lost groups the respawn policy is holding back now.
    held: BTreeSet<EntityId>,
    counters: Counters,
}

fn classify(code: u16, opcode: u16) -> Outcome {
    match u64::from(code) {
        rc::APPLIED | rc::AT_REVISION => Outcome::Applied,
        rc::TOO_LARGE if opcode == opcode::CAPTURE => Outcome::Refused,
        rc::TOO_LARGE => Outcome::Applied,
        rc::DCS_REFUSED | rc::NOT_BOUND | rc::LUA_ERROR => Outcome::Refused,
        rc::DUPLICATE => Outcome::Unknown,
        _ => Outcome::Rejected,
    }
}

/// Whether a rejection means the command is fine and only its moment was
/// wrong: derive it again under the current epoch (spec/errors.md).
fn rejected_for_now(code: u16) -> bool {
    matches!(
        u64::from(code),
        rc::STALE_EPOCH | rc::WRONG_CAMPAIGN | rc::WRONG_GENERATION
    )
}

impl Reconciler {
    /// A reconciler for a store: at the sidecar's start, or after a restart.
    pub fn new(cfg: Config, store: &Store) -> Reconciler {
        let inflight = store
            .unanswered()
            .into_iter()
            .map(|c| ((c.epoch, c.seq), c))
            .collect();
        Reconciler {
            cfg,
            agent_boot: store.loaded().map_or(0, |l| l.agent_boot),
            attached: false,
            epoch: 0,
            next_seq: 1,
            lifecycle: 0,
            stamp: (0, 0, 0),
            inflight,
            binding: HashMap::new(),
            run: None,
            unresolved: BTreeMap::new(),
            guard: false,
            enum_wanted: false,
            order_held: HashMap::new(),
            order_refused: HashMap::new(),
            drift_destroy_failures: HashMap::new(),
            spawn_rejected: HashMap::new(),
            campaign_sent: false,
            enumerated: false,
            held: BTreeSet::new(),
            counters: Counters::default(),
        }
    }

    /// What the reconciler counted.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// The lost groups the respawn policy is holding back: a member left as
    /// `MISSING` or `REMOVED` and nothing explains it (spec/world.md,
    /// "Respawn gate"). They stay `lost` and are reported by this set.
    pub fn held(&self) -> &BTreeSet<EntityId> {
        &self.held
    }

    /// The binding mirror, by DCS object ID.
    pub fn binding(&self) -> &HashMap<u64, Binding> {
        &self.binding
    }

    /// Whether the reconciler is attached and DCS runs a mission it may
    /// command.
    pub fn ready(&self, store: &Store) -> bool {
        self.attached
            && self.epoch != 0
            && matches!(u64::from(self.lifecycle), lc::RUNNING | lc::PAUSED)
            && store
                .loaded()
                .is_some_and(|l| l.agent_boot == self.agent_boot && !l.theater.is_empty())
    }

    /// Sequenced commands written and not yet answered.
    pub fn inflight(&self) -> impl Iterator<Item = &Journalled> {
        self.inflight.values()
    }

    /// Read and fold in every record, then write the commands that close the
    /// difference.
    pub fn step(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) {
        for r in sim.read() {
            self.record(store, &r);
        }
        self.plan(store, sim);
    }

    /// Fold one record into the store. The replay driver calls this directly.
    /// A record read twice is dropped by the link before it gets here
    /// (crates/sidecar/src/link.rs, `Sink::last_number`).
    pub fn record(&mut self, store: &mut Store, r: &RawRecord) {
        // A sidecar that started without its store knows no loaded
        // mission. It takes the one the stamps name, replayed stamps
        // included, and reads on in order: a record the agent queued while
        // detached belongs to the mission it was made in.
        let stamped = !matches!(
            u64::from(r.kind),
            kind::FRAME | kind::HELLO | kind::LIFECYCLE | kind::RESULT | kind::GAP
        );
        if stamped && store.loaded().is_none() && self.stamp.0 == r.agent_boot && r.agent_boot != 0
        {
            self.seen_mission(store, self.stamp.0, self.stamp.1, None);
        }
        self.fold(store, r);
    }

    fn fold(&mut self, store: &mut Store, r: &RawRecord) {
        let input = match Input::decode(r.kind, &r.payload) {
            Ok(i) => i,
            Err(_) => {
                self.counters.undecodable += 1;
                return;
            }
        };
        match input {
            Input::Frame(f) => {
                self.stamp = (r.agent_boot, f.mission_gen, f.campaign);
                if f.replay == 0 {
                    if r.agent_boot == self.agent_boot || self.agent_boot == 0 {
                        self.lifecycle = f.lifecycle;
                    }
                    self.seen_mission(store, r.agent_boot, f.mission_gen, None);
                }
            }
            Input::Hello(h) => self.hello(store, r.agent_boot, &h),
            Input::Lifecycle(l) => {
                if r.agent_boot == self.agent_boot || self.agent_boot == 0 {
                    self.lifecycle = l.state;
                }
                self.seen_mission(store, r.agent_boot, l.mission_gen, Some(&l.theater));
            }
            Input::Result(res) => self.result(store, &res),
            Input::Gap(g) => {
                if u64::from(g.class) == gap_class::EVENTS {
                    self.enum_wanted = true;
                    self.guard = true;
                    // The lost records may be OBJECT rows: the mirror may
                    // lack objects DCS holds until the next enumeration.
                    self.enumerated = false;
                    if let Some(run) = &mut self.run {
                        run.gap = true;
                    }
                }
            }
            Input::Object(o) => {
                if self.current(store, r.agent_boot) {
                    self.object(store, &o);
                }
            }
            Input::ObjectGone(g) => {
                if self.current(store, r.agent_boot) {
                    self.gone(store, g);
                }
            }
            Input::Seen(s) => {
                if self.current(store, r.agent_boot) {
                    self.seen(store, &s);
                }
            }
            Input::EnumBegin(e) => {
                if self.current(store, r.agent_boot) {
                    self.run = Some(EnumRun {
                        id: e.enumeration,
                        before: self.binding.keys().copied().collect(),
                        found: BTreeSet::new(),
                        count: 0,
                        seen: 0,
                        // An enumeration made under another campaign says
                        // nothing of this campaign's objects.
                        gap: self.stamp.2 != store.campaign().0,
                    });
                }
            }
            Input::EnumEnd(e) => {
                if self.current(store, r.agent_boot) {
                    self.enum_end(store, e.enumeration, e.objects, e.seen);
                }
            }
            Input::Positions(rows) => {
                if self.current(store, r.agent_boot) {
                    for p in rows {
                        if let Some(b) = self.binding.get(&p.object) {
                            let at = Point {
                                x: p.x,
                                y: p.y,
                                z: p.z,
                            };
                            store.observe_position(b.unit, at);
                        }
                    }
                }
            }
            Input::Vitals(rows) => {
                if self.current(store, r.agent_boot) {
                    for v in rows {
                        if let Some(b) = self.binding.get(&v.object) {
                            store.observe_vitals(b.unit, v.life, v.fuel);
                        }
                    }
                }
            }
            Input::Event(_) => self.counters.events += 1,
            Input::Other(_) => self.counters.other += 1,
        }
    }

    /// Whether a record stamped with the last `FRAME` belongs to the loaded
    /// mission. Records of an older generation are ignored (spec/records.md).
    fn current(&self, store: &Store, agent_boot: u64) -> bool {
        self.stamp.0 == agent_boot
            && store
                .loaded()
                .is_some_and(|l| l.agent_boot == agent_boot && l.mission_gen == self.stamp.1)
    }

    /// A `LIFECYCLE`, or a `FRAME` that is not a replay, names the loaded
    /// mission. A `FRAME` carries no theater; a sidecar that started without
    /// its store learns it from the `LIFECYCLE` the agent writes right after
    /// every `HELLO`. For a store that already holds the mission that
    /// `LIFECYCLE` changes nothing: no journal entry, no new generation.
    fn seen_mission(
        &mut self,
        store: &mut Store,
        agent_boot: u64,
        mission_gen: u32,
        theater: Option<&str>,
    ) {
        if let Some(l) = store.loaded()
            && l.agent_boot == agent_boot
            && l.mission_gen == mission_gen
        {
            if let Some(t) = theater
                && !t.is_empty()
                && t != l.theater
            {
                store.set_loaded(Loaded {
                    agent_boot,
                    mission_gen,
                    theater: t.to_owned(),
                });
            }
            return;
        }
        // A new mission generation: DCS holds none of the platform's objects
        // and the agent has forgotten every applied revision.
        let theater = theater.unwrap_or("").to_owned();
        store.set_loaded(Loaded {
            agent_boot,
            mission_gen,
            theater: theater.clone(),
        });
        self.binding.clear();
        self.unresolved.clear();
        self.order_held.clear();
        self.order_refused.clear();
        self.spawn_rejected.clear();
        self.drift_destroy_failures.clear();
        self.held.clear();
        self.run = None;
        store.clear_missing();
        let ids: Vec<EntityId> = store.groups().map(|g| g.id).collect();
        for id in ids {
            let g = store.group(id).expect("group");
            // Materializing again needs DCS authority as well as the theater.
            let same_theater =
                !theater.is_empty() && g.theater == theater && g.authority == Authority::Sim;
            if matches!(
                g.state,
                State::Live | State::Materializing | State::Lost | State::Dematerializing
            ) {
                store
                    .transition(id, Trigger::NewGeneration { same_theater })
                    .expect("the table holds every generation change");
            }
        }
    }

    fn hello(&mut self, store: &mut Store, agent_boot: u64, h: &Hello) {
        // Every command the HELLO does not answer: taken (at or before
        // in_head) and its result lost, or never taken. A command of another
        // agent boot cannot be answered any more.
        let open: Vec<Journalled> = self.inflight.values().cloned().collect();
        for c in open {
            let outcome = if c.agent_boot != h.agent_boot || c.position <= h.in_head {
                Outcome::Unknown
            } else {
                Outcome::NotSent
            };
            self.resolve(store, &c, outcome, None);
        }
        if h.agent_boot != self.agent_boot || self.epoch == 0 {
            let epoch = store.epoch().max(h.epoch) + 1;
            store.set_epoch(epoch);
            self.epoch = epoch;
            self.next_seq = 1;
        }
        if h.agent_boot != self.agent_boot || h.campaign != store.campaign().0 {
            // A new DCS process holds nothing this one knew. An agent on
            // another campaign discards the binding and every applied
            // revision when this sidecar's CAMPAIGN reaches it.
            self.binding.clear();
            self.unresolved.clear();
            self.order_held.clear();
            self.order_refused.clear();
            self.run = None;
        }
        self.agent_boot = agent_boot;
        self.attached = true;
        self.campaign_sent = false;
        self.enumerated = false;
    }

    fn resolve(
        &mut self,
        store: &mut Store,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        self.inflight.remove(&(c.epoch, c.seq));
        store.journal(Entry::Outcome {
            epoch: c.epoch,
            seq: c.seq,
            outcome,
        });
        let Some(g) = store.group(c.entity) else {
            return;
        };
        let gen_now = store
            .loaded()
            .is_some_and(|l| l.agent_boot == c.agent_boot && l.mission_gen == c.mission_gen);
        let (id, state, spec_rev, captured) = (g.id, g.state, g.spec_rev, g.captured);
        let code = res.map(|r| u64::from(r.code));
        if matches!(c.opcode, opcode::GROUP_DESTROY | opcode::STATIC_DESTROY) {
            // Before the check for `dead`: a dead entity's drift destroy is
            // counted to the limit like any other.
            if gen_now {
                self.destroy_resolved(store, id, state, captured, outcome, res);
            }
            return;
        }
        if state == State::Dead {
            self.counters.after_dead += 1;
            return;
        }
        match c.opcode {
            opcode::GROUP_SPAWN | opcode::STATIC_SPAWN => {
                if !gen_now || state != State::Materializing || c.rev != spec_rev {
                    return;
                }
                match outcome {
                    Outcome::Applied => {
                        // The group may be new, even when the answer is
                        // AT_REVISION to a resend of a spawn whose result was
                        // lost: a new group has no task, and an order refused
                        // for the old one may suit it. Sending the order again
                        // costs at most an AT_REVISION.
                        self.order_held.remove(&id);
                        self.order_refused.remove(&id);
                        // Spawned again in the same generation, the entity
                        // has new DCS object IDs: an attack order on it is
                        // compiled again at a new revision. After a
                        // generation change every order is sent again anyway.
                        if store
                            .spawned(id, c.agent_boot, c.mission_gen)
                            .expect("group")
                        {
                            let attackers: Vec<EntityId> = store
                                .groups()
                                .filter(|a| {
                                    a.order_sent_in == Some((c.agent_boot, c.mission_gen, a.order_rev))
                                        && matches!(a.order, Some(Intent::Attack { target }) if target == id)
                                })
                                .map(|a| a.id)
                                .collect();
                            for a in attackers {
                                store.bump_order(a).expect("group");
                            }
                        }
                        store
                            .transition(id, Trigger::SpawnApplied)
                            .expect("materializing");
                        if store.all_dead(id) {
                            store.transition(id, Trigger::AllDead).expect("live");
                        }
                    }
                    Outcome::Refused => {
                        let msg = res.map(|r| r.message.clone()).unwrap_or_default();
                        store
                            .transition(id, Trigger::SpawnRefused)
                            .expect("materializing");
                        store.set_error(id, msg).expect("group");
                    }
                    Outcome::Unknown => {
                        store
                            .spawn_unknown(id, c.agent_boot, c.mission_gen)
                            .expect("group");
                        store
                            .transition(id, Trigger::SpawnUnknown)
                            .expect("materializing");
                    }
                    Outcome::Rejected => {
                        if let Some(r) = res
                            && !rejected_for_now(r.code)
                        {
                            self.spawn_rejected.insert(id, (c.rev, c.mission_gen));
                        }
                    }
                    Outcome::NotSent => {}
                }
            }
            opcode::GROUP_ORDER => {
                if !gen_now {
                    return;
                }
                match outcome {
                    Outcome::Applied => {
                        let held = self.order_held.entry(id).or_default();
                        *held = (*held).max(c.rev);
                    }
                    Outcome::Refused => {
                        self.order_refused.insert(id, c.rev);
                    }
                    _ => {}
                }
            }
            opcode::GROUP_CAPTURE_DESTROY | opcode::CAPTURE => {
                if !gen_now || state != State::Dematerializing {
                    return;
                }
                if outcome == Outcome::Rejected || outcome == Outcome::NotSent {
                    // Not tried: the next plan sends it again.
                    return;
                }
                if code == Some(rc::APPLIED)
                    && let Some(r) = res
                {
                    self.take_capture(store, id, &r.value);
                }
                // The capture is settled: the captured state, or else the
                // last checkpointed one. A capture-and-destroy that ran in
                // full also removed the group; anything else leaves a
                // destroy to send (spec/errors.md).
                store.set_captured(id).expect("group");
                if c.opcode == opcode::GROUP_CAPTURE_DESTROY && code == Some(rc::APPLIED) {
                    self.order_held.remove(&id);
                    unbind_group(&mut self.binding, id);
                    store
                        .transition(id, Trigger::CaptureDone)
                        .expect("dematerializing");
                }
            }
            _ => {}
        }
    }

    fn destroy_resolved(
        &mut self,
        store: &mut Store,
        id: EntityId,
        state: State,
        captured: bool,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        let code = res.map(|r| u64::from(r.code));
        // APPLIED, or NOT_BOUND: DCS no longer holds the entity.
        let gone = outcome == Outcome::Applied || code == Some(rc::NOT_BOUND);
        if gone {
            // The agent sets the applied revisions of what it
            // destroyed to 0 (spec/commands.md).
            self.order_held.remove(&id);
            unbind_group(&mut self.binding, id);
            self.drift_destroy_failures.remove(&id);
            if state == State::Dematerializing && captured {
                store
                    .transition(id, Trigger::CaptureDone)
                    .expect("dematerializing");
            }
        } else if matches!(outcome, Outcome::Refused | Outcome::Unknown) {
            // Sent again, safely (destroying nothing is APPLIED),
            // up to the limit.
            let limit = self.cfg.destroy_retries.max(1);
            if state == State::Dematerializing {
                if store.destroy_failed(id).expect("group") >= limit {
                    let msg = res.map_or("the destroy's result was lost".to_owned(), |r| {
                        r.message.clone()
                    });
                    store
                        .set_error(id, format!("destroy failed {limit} times: {msg}"))
                        .expect("group");
                    store
                        .transition(id, Trigger::DestroyFailed)
                        .expect("dematerializing");
                }
            } else {
                *self.drift_destroy_failures.entry(id).or_default() += 1;
            }
        }
    }

    /// Read a captured state: a list of tables with `name`, `x`, `y`, `z`,
    /// `heading`, `life` and `fuel` (spec/commands.md,
    /// `GROUP_CAPTURE_DESTROY`; the shape is fixed in step S15).
    fn take_capture(&mut self, store: &mut Store, group: EntityId, value: &Lv) {
        let campaign = store.campaign();
        for item in lv::items(value) {
            let Some(name) = lv::str_at(item, "name") else {
                continue;
            };
            let Some(p) = ids::parse(&name) else { continue };
            if p.campaign != campaign || store.unit(p.entity).is_none_or(|u| u.group != group) {
                continue;
            }
            let num = |k| lv::num_at(item, k).unwrap_or(0.0);
            store.observe_capture(
                p.entity,
                UnitState {
                    at: Point {
                        x: num("x"),
                        y: num("y"),
                        z: num("z"),
                    },
                    heading: num("heading"),
                    life: num("life") as f32,
                    fuel: num("fuel") as f32,
                },
            );
        }
    }

    fn result(&mut self, store: &mut Store, r: &CommandResult) {
        if r.seq == 0 {
            // A failed control command: nothing is waiting for it.
            return;
        }
        let Some(c) = self.inflight.get(&(r.epoch, r.seq)).cloned() else {
            self.counters.stray_results += 1;
            return;
        };
        let outcome = classify(r.code, c.opcode);
        self.resolve(store, &c, outcome, Some(r));
        if c.opcode == opcode::GROUP_SPAWN && outcome == Outcome::Applied && r.object != 0 {
            for row in self
                .binding
                .values_mut()
                .filter(|row| row.group == c.entity)
            {
                row.dcs_group = r.object;
            }
        }
    }

    /// The entity an object names, from the binding the agent reported or
    /// from its platform name. `None` for an object the active campaign does
    /// not manage.
    fn resolve_object(&mut self, store: &Store, o: &Object) -> Option<Binding> {
        let campaign = store.campaign();
        let named = ids::parse(&o.name);
        if let Some(n) = named
            && n.campaign != campaign
        {
            self.counters.foreign += 1;
            return None;
        }
        let unit = if o.entity != 0 {
            EntityId(o.entity)
        } else if let Some(n) = named
            && matches!(n.kind, NameKind::Unit | NameKind::Static)
        {
            n.entity
        } else {
            self.counters.unmanaged += 1;
            return None;
        };
        let Some(u) = store.unit(unit) else {
            self.unresolved.entry(o.object).or_insert((unit, false)).0 = unit;
            return None;
        };
        Some(Binding {
            unit,
            group: u.group,
            dcs_group: o.group,
        })
    }

    fn object(&mut self, store: &mut Store, o: &Object) {
        let other = self.stamp.2 != store.campaign().0;
        if let Some(run) = &mut self.run
            && o.enumeration == run.id
        {
            run.found.insert(o.object);
            run.count += 1;
            run.gap |= other;
        }
        if other {
            // Made while another campaign was the agent's: its binding is
            // that campaign's (spec/world.md, death facts).
            self.counters.other_campaign += 1;
            return;
        }
        let Some(row) = self.resolve_object(store, o) else {
            return;
        };
        // An object ID that had a death fact, seen alive again: the same
        // entity under the same object ID. A different object ID is a new
        // object and leaves the death as it is.
        let revival = store
            .unit(row.unit)
            .is_some_and(|u| u.dead && o.object != 0 && u.object == o.object);
        if revival {
            if store
                .group(row.group)
                .is_some_and(|g| g.state == State::Dead)
            {
                // `dead` is final: the entity stays dead, and DCS holding
                // its object is drift the plan corrects.
                self.counters.after_dead += 1;
            } else if store.revive(row.unit, o.object) {
                self.counters.revivals += 1;
                // A revived member brings back the DCS group that refused the order.
                self.order_refused.remove(&row.group);
            }
        }
        store.set_missing(row.unit, false);
        store.set_object(row.unit, o.object);
        self.unresolved.remove(&o.object);
        self.binding.insert(o.object, row);
    }

    /// `SEEN`: the rows the agent found whose `OBJECT` is unchanged. The ones
    /// the reconciler binds are found in the run. An ID it does not bind is no
    /// refusal: the object is unmanaged, or another campaign's. A `SEEN`
    /// stamped under another campaign marks the run's gap, as an `OBJECT`
    /// does. An ID whose `OBJECT` the reconciler could not resolve, and whose
    /// unit the store now holds, asks once for an enumeration of whole
    /// `OBJECT` records.
    fn seen(&mut self, store: &Store, s: &Seen) {
        let other = self.stamp.2 != store.campaign().0;
        let Some(run) = &mut self.run else {
            return;
        };
        if s.enumeration != run.id {
            return;
        }
        run.seen = run.seen.saturating_add(s.objects.len() as u32);
        run.gap |= other;
        for id in &s.objects {
            if self.binding.contains_key(id) {
                run.found.insert(*id);
            } else if let Some((unit, asked)) = self.unresolved.get_mut(id)
                && !*asked
                && store.unit(*unit).is_some()
            {
                *asked = true;
                self.enum_wanted = true;
            }
        }
    }

    /// The unit and group an `OBJECT_GONE` is about: from the binding it
    /// carries, which must name a unit of that group (or a static, with no
    /// group), else from the mirror.
    fn gone_identity(
        &mut self,
        store: &Store,
        g: &ObjectGone,
        row: Option<Binding>,
    ) -> Option<(EntityId, EntityId)> {
        if g.entity == 0 {
            return row.map(|b| (b.unit, b.group));
        }
        let unit = EntityId(g.entity);
        let group = store.unit(unit).map(|u| u.group);
        let want = if g.group_entity == 0 {
            unit
        } else {
            EntityId(g.group_entity)
        };
        if group == Some(want) {
            Some((unit, want))
        } else {
            self.counters.bad_binding += 1;
            None
        }
    }

    fn gone(&mut self, store: &mut Store, g: ObjectGone) {
        let row = self.binding.remove(&g.object);
        self.unresolved.remove(&g.object);
        // The agent reported the row gone: ENUM_END does not count it
        // missing again (spec/records.md, "Layout: ENUM_END").
        if let Some(run) = &mut self.run {
            run.before.remove(&g.object);
        }
        if self.stamp.2 != store.campaign().0 {
            self.counters.other_campaign += 1;
            if let Some(run) = &mut self.run {
                run.gap = true;
            }
            return;
        }
        let Some((unit, group)) = self.gone_identity(store, &g, row) else {
            return;
        };
        let state = store.group(group).expect("group").state;
        // The member's object ID when it left: a record for an older object,
        // from before a respawn, is about no member that DCS holds now.
        let known = store.unit(unit).map_or(0, |u| u.object);
        let stale = known != 0 && g.object != 0 && known != g.object;
        match g.reason {
            gone_reason::DEAD if state == State::Dead => self.counters.after_dead += 1,
            gone_reason::DEAD if stale => {
                store.stale_death(unit, g.object);
                self.counters.stale_deaths += 1;
            }
            gone_reason::DEAD => {
                let (was_missing, was_removed) = store
                    .unit(unit)
                    .map_or((false, false), |u| (u.missing, u.removed));
                if !store.death(unit).unwrap_or(false) {
                    return;
                }
                store.set_object(unit, g.object);
                if was_missing || was_removed {
                    // A death that follows a `MISSING` or a `REMOVED`.
                    self.counters.late_deaths += 1;
                }
                if was_removed {
                    store.classifier_fault(unit, g.object);
                    self.counters.classifier_faults += 1;
                }
                if !store.all_dead(group) {
                    return;
                }
                // A death fact while the spawn is unanswered shows the spawn
                // ran: resolve it applied. A spawn not yet written is
                // cancelled by the move to `dead`.
                let pending: Vec<Journalled> = if state == State::Materializing {
                    let loaded = store.loaded().cloned();
                    self.inflight
                        .values()
                        .filter(|c| {
                            c.entity == group
                                && matches!(c.opcode, opcode::GROUP_SPAWN | opcode::STATIC_SPAWN)
                                && loaded.as_ref().is_some_and(|l| {
                                    l.agent_boot == c.agent_boot && l.mission_gen == c.mission_gen
                                })
                        })
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                if store.transition(group, Trigger::AllDead).is_ok() {
                    self.held.remove(&group);
                    for c in pending {
                        self.inflight.remove(&(c.epoch, c.seq));
                        store.journal(Entry::Outcome {
                            epoch: c.epoch,
                            seq: c.seq,
                            outcome: Outcome::Applied,
                        });
                    }
                }
            }
            gone_reason::MISSING | gone_reason::REMOVED if stale => {
                self.counters.stale_leaves += 1;
            }
            gone_reason::MISSING => {
                store.left(unit, g.object, LeftAs::Missing);
            }
            gone_reason::REMOVED => {
                store.left(unit, g.object, LeftAs::Removed);
            }
            _ => {}
        }
    }

    fn enum_end(&mut self, store: &mut Store, id: u64, objects: u32, seen: u32) {
        let Some(run) = self.run.take() else {
            self.enum_wanted = true;
            return;
        };
        if run.id != id || run.count != objects || run.seen != seen || run.gap {
            self.counters.enumerations_discarded += 1;
            self.enum_wanted = true;
            return;
        }
        if self.guard && seen > 0 {
            // After a lost record, only an enumeration of whole `OBJECT`
            // records can be trusted to repair the binding.
            self.counters.enumerations_discarded += 1;
            self.enum_wanted = true;
            return;
        }
        if seen == 0 {
            self.guard = false;
        }
        self.counters.enumerations += 1;
        self.enumerated = true;
        // A drift destroy that reached its limit is tried again after each
        // enumeration that still finds the objects.
        self.drift_destroy_failures.clear();
        // The agent's rule, applied to the mirror: a row bound at the start
        // and not found is missing.
        for o in run.before.difference(&run.found) {
            if let Some(row) = self.binding.remove(o) {
                store.set_missing(row.unit, true);
            }
        }
        let found: BTreeSet<EntityId> = self.binding.values().map(|r| r.group).collect();
        let bound_units: BTreeSet<EntityId> = self.binding.values().map(|r| r.unit).collect();
        let ids: Vec<(EntityId, State)> = store.groups().map(|g| (g.id, g.state)).collect();
        for (id, state) in ids {
            if state == State::Live {
                // A member the agent's table does not hold is missing, even
                // one the mirror never saw: its OBJECT may have been lost
                // to a GAP.
                for m in store.group(id).expect("group").members.clone() {
                    if !bound_units.contains(&m) {
                        store.set_missing(m, true);
                    }
                }
            }
            match state {
                State::Live if !found.contains(&id) && !store.all_dead(id) => {
                    for m in store.group(id).expect("group").members.clone() {
                        store.set_missing(m, true);
                    }
                    store.transition(id, Trigger::EnumMissing).expect("live");
                }
                State::Lost if found.contains(&id) => {
                    // An order refused while DCS held no group (a revived
                    // member brings one back) may be taken now.
                    self.order_refused.remove(&id);
                    store.transition(id, Trigger::EnumFound).expect("lost");
                }
                State::Dead if found.contains(&id) => self.counters.after_dead += 1,
                _ => {}
            }
        }
    }

    fn send(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        entity: EntityId,
        rev: u64,
        body: Body,
    ) -> bool {
        let loaded = store.loaded().cloned().expect("ready");
        let control = body.is_control();
        let header = CommandHeader {
            seq: if control { 0 } else { self.next_seq },
            epoch: if control { 0 } else { self.epoch },
            campaign: store.campaign().0,
            entity: entity.0,
            rev,
            mission_gen: loaded.mission_gen,
        };
        let units = match &body {
            Body::GroupSpawn { group, .. } => unit_names(group, store.campaign(), "units"),
            Body::StaticSpawn { .. } => vec![entity],
            _ => Vec::new(),
        };
        let cmd = Command { header, body };
        let (op, payload) = cmd.encode();
        let j = Journalled {
            agent_boot: self.agent_boot,
            epoch: self.epoch,
            seq: self.next_seq,
            opcode: op,
            entity,
            rev,
            mission_gen: loaded.mission_gen,
            position: sim.position_after(op, &payload),
            units,
        };
        if !control {
            self.next_seq += 1;
            store.journal(Entry::Command(j.clone()));
        }
        match sim.send(op, &payload) {
            Ok(pos) => {
                debug_assert!(
                    control || pos == j.position,
                    "the adapter predicted {}, wrote {pos}",
                    j.position
                );
                if !control {
                    self.inflight.insert((j.epoch, j.seq), j);
                }
                true
            }
            Err(_) => {
                self.attached = false;
                if !control {
                    store.journal(Entry::Outcome {
                        epoch: j.epoch,
                        seq: j.seq,
                        outcome: Outcome::NotSent,
                    });
                }
                false
            }
        }
    }

    fn inflight_for(&self, store: &Store, entity: EntityId, ops: &[u16]) -> bool {
        let Some(l) = store.loaded() else {
            return false;
        };
        self.inflight.values().any(|c| {
            c.entity == entity
                && ops.contains(&c.opcode)
                && c.agent_boot == l.agent_boot
                && c.mission_gen == l.mission_gen
        })
    }

    fn plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) {
        if !self.ready(store) {
            return;
        }
        if !self.campaign_sent {
            let c = store.campaign().0;
            if !self.send(store, sim, EntityId(0), 0, Body::Campaign(c)) {
                return;
            }
            self.campaign_sent = true;
        }
        if self.enum_wanted {
            if !self.send(store, sim, EntityId(0), 0, Body::Enumerate) {
                return;
            }
            self.enum_wanted = false;
            self.counters.enumerate_requests += 1;
        }
        let loaded = store.loaded().expect("ready").clone();
        self.held
            .retain(|id| store.group(*id).is_some_and(|g| g.state == State::Lost));
        let mut bound: HashMap<EntityId, u64> = HashMap::new();
        for row in self.binding.values() {
            let e = bound.entry(row.group).or_default();
            if row.dcs_group != 0 {
                *e = row.dcs_group;
            }
        }
        let mut spawns = self.cfg.materialize_per_frame;
        let ids: Vec<EntityId> = store.groups().map(|g| g.id).collect();
        for id in ids {
            if !self.attached {
                return;
            }
            let g = store.group(id).expect("group");
            let here = g.theater == loaded.theater;
            let has_objects = bound.contains_key(&id);
            match g.state {
                State::Virtual | State::Dead | State::Unspawnable if has_objects => {
                    // Drift: DCS holds objects of an entity it should not.
                    self.destroy(store, sim, id);
                }
                State::Virtual => {
                    if g.authority == Authority::Sim && here && spawns > 0 {
                        store.transition(id, Trigger::Materialize).expect("virtual");
                        if self.spawn(store, sim, id) {
                            spawns -= 1;
                        }
                    }
                }
                State::Materializing
                    if (g.authority == Authority::Campaign || !here)
                        && g.unknown_spawn_in != Some((loaded.agent_boot, loaded.mission_gen))
                        && !self.inflight_for(
                            store,
                            id,
                            &[opcode::GROUP_SPAWN, opcode::STATIC_SPAWN],
                        ) =>
                {
                    store
                        .transition(id, Trigger::Withdraw)
                        .expect("materializing");
                }
                State::Materializing => {
                    let rejected =
                        self.spawn_rejected.get(&id) == Some(&(g.spec_rev, loaded.mission_gen));
                    if here
                        && spawns > 0
                        && !rejected
                        && !self.inflight_for(
                            store,
                            id,
                            &[opcode::GROUP_SPAWN, opcode::STATIC_SPAWN],
                        )
                        && self.spawn(store, sim, id)
                    {
                        spawns -= 1;
                    }
                }
                State::Live => {
                    if g.authority == Authority::Campaign {
                        store.transition(id, Trigger::Dematerialize).expect("live");
                        self.dematerialize(store, sim, id);
                    } else {
                        self.order(store, sim, id, &bound);
                    }
                }
                State::Dematerializing => {
                    if g.captured {
                        self.destroy(store, sim, id);
                    } else if !self.inflight_for(
                        store,
                        id,
                        &[opcode::GROUP_CAPTURE_DESTROY, opcode::CAPTURE],
                    ) {
                        self.dematerialize(store, sim, id);
                    }
                }
                // A lost group with objects is settled by the next
                // enumeration, not destroyed.
                State::Lost => match g.on_lost.unwrap_or(self.cfg.on_lost) {
                    LostPolicy::Report => {}
                    LostPolicy::Respawn => {
                        // The gate: a group with a member that left as
                        // `MISSING` or `REMOVED`, and has no death fact, is
                        // never respawned in this generation. A member with
                        // a death fact is left out of the spawn. A weapon
                        // kill can come as `MISSING`
                        // with its death event up to a minute later, and a
                        // scripted removal can read as a kill or the other
                        // way round. No timer opens it (spec/world.md,
                        // "Respawn gate").
                        if store.gate_shut(id) {
                            if self.held.insert(id) {
                                self.counters.respawns_held += 1;
                            }
                            continue;
                        }
                        self.held.remove(&id);
                        // Objects DCS may still hold (a destroy that failed to
                        // its limit) are settled by an enumeration first: one
                        // used since the last HELLO, so the mirror is whole.
                        if g.authority == Authority::Sim && here && self.enumerated && !has_objects
                        {
                            store.respawn(id).expect("lost");
                        }
                    }
                    LostPolicy::Dead => {
                        store.transition(id, Trigger::PolicyDead).expect("lost");
                    }
                },
                State::Dead | State::Unspawnable => {}
            }
        }
    }

    fn destroy(&mut self, store: &mut Store, sim: &mut dyn SimAdapter, id: EntityId) {
        let ops = [opcode::GROUP_DESTROY, opcode::STATIC_DESTROY];
        let given_up = self.drift_destroy_failures.get(&id).copied().unwrap_or(0)
            >= self.cfg.destroy_retries.max(1);
        if given_up || self.inflight_for(store, id, &ops) {
            return;
        }
        let body = match store.group(id).expect("group").kind {
            Kind::Group => Body::GroupDestroy,
            Kind::Static => Body::StaticDestroy,
        };
        self.send(store, sim, id, 0, body);
    }

    fn dematerialize(&mut self, store: &mut Store, sim: &mut dyn SimAdapter, id: EntityId) {
        let body = match store.group(id).expect("group").kind {
            Kind::Group => Body::GroupCaptureDestroy,
            Kind::Static => Body::Capture,
        };
        self.send(store, sim, id, 0, body);
    }

    /// Compile and send a spawn. Returns whether one was sent.
    fn spawn(&mut self, store: &mut Store, sim: &mut dyn SimAdapter, id: EntityId) -> bool {
        let g = store.group(id).expect("group").clone();
        let campaign = store.campaign();
        let living: Vec<&crate::store::Unit> = g
            .members
            .iter()
            .map(|m| store.unit(*m).expect("unit"))
            .filter(|u| !u.dead)
            .collect();
        if living.is_empty() {
            store.transition(id, Trigger::AllDead).expect("not final");
            return false;
        }
        let body = match g.kind {
            Kind::Group => {
                let name = ids::name(NameKind::Group, campaign, id);
                let units = living
                    .iter()
                    .map(|u| {
                        let mut t = vec![
                            ("name", lv::s(&ids::name(NameKind::Unit, campaign, u.id))),
                            ("type", lv::s(&u.type_name)),
                            ("x", lv::n(u.state.at.x)),
                            ("y", lv::n(u.state.at.z)),
                            ("alt", lv::n(u.state.at.y)),
                            ("heading", lv::n(u.state.heading)),
                        ];
                        if !u.skill.is_empty() {
                            t.push(("skill", lv::s(&u.skill)));
                        }
                        with_extra(lv::obj(t), &u.extra)
                    })
                    .collect();
                Body::GroupSpawn {
                    country: g.country,
                    category: g.category,
                    group: with_extra(
                        lv::obj(vec![("name", lv::s(&name)), ("units", lv::list(units))]),
                        &g.extra,
                    ),
                    name,
                }
            }
            Kind::Static => {
                let u = living[0];
                let name = ids::name(NameKind::Static, campaign, id);
                Body::StaticSpawn {
                    country: g.country,
                    table: with_extra(
                        lv::obj(vec![
                            ("name", lv::s(&name)),
                            ("type", lv::s(&u.type_name)),
                            ("x", lv::n(u.state.at.x)),
                            ("y", lv::n(u.state.at.z)),
                            ("heading", lv::n(u.state.heading)),
                        ]),
                        &u.extra,
                    ),
                    name,
                }
            }
        };
        self.send(store, sim, id, g.spec_rev, body)
    }

    fn order(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        id: EntityId,
        bound: &HashMap<EntityId, u64>,
    ) {
        let g = store.group(id).expect("group");
        let Some(intent) = &g.order else { return };
        if g.kind != Kind::Group || self.inflight_for(store, id, &[opcode::GROUP_ORDER]) {
            return;
        }
        let target_now = |t: EntityId| bound.get(&t).copied().filter(|v| *v != 0);
        let rev = g.order_rev;
        let held = self.order_held.get(&id).copied().unwrap_or(0);
        if held >= rev || self.order_refused.get(&id) == Some(&rev) {
            return;
        }
        let lead = g
            .members
            .iter()
            .map(|m| store.unit(*m).expect("unit"))
            .find(|u| !u.dead);
        let status = intents::Status {
            at: lead.map(|u| (u.state.at.x, u.state.at.z)),
            progress: g.progress,
        };
        match intents::compile(intent, status, target_now) {
            Compiled::Task(task) => {
                let loaded = store.loaded().cloned().expect("ready");
                store
                    .order_sent(id, loaded.agent_boot, loaded.mission_gen, rev)
                    .expect("group");
                self.send(store, sim, id, rev, Body::GroupOrder(task));
            }
            Compiled::Done => {
                self.order_held.insert(id, rev);
            }
            Compiled::Waiting(_) => {}
        }
    }
}

/// A DCS table with the author's further keys added. A key the platform
/// sets (name, type, position, heading, units, skill) keeps the platform's
/// value: the platform's names are how it finds its objects again.
fn with_extra(table: Lv, extra: &Lv) -> Lv {
    let (Lv::Table(mut pairs), Lv::Table(more)) = (table.clone(), extra) else {
        // No further keys (nil), or a value that is not a table.
        return table;
    };
    for (k, v) in more {
        if !pairs.iter().any(|(pk, _)| pk == k) {
            pairs.push((k.clone(), v.clone()));
        }
    }
    Lv::Table(pairs)
}

fn unbind_group(binding: &mut HashMap<u64, Binding>, id: EntityId) {
    binding.retain(|_, r| r.group != id);
}

/// The units a spawn's group table names, by their platform names.
fn unit_names(group: &Lv, campaign: CampaignId, key: &str) -> Vec<EntityId> {
    lv::get(group, key)
        .map(|u| {
            lv::items(u)
                .into_iter()
                .filter_map(|t| lv::str_at(t, "name"))
                .filter_map(|n| ids::parse(&n))
                .filter(|p| p.campaign == campaign)
                .map(|p| p.entity)
                .collect()
        })
        .unwrap_or_default()
}

/// The outcome a result code gives a command of an opcode.
pub fn outcome_of(code: u16, opcode: u16) -> Outcome {
    classify(code, opcode)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every result code against the table of spec/errors.md, "Command
    /// outcomes".
    #[test]
    fn result_codes_map_to_the_outcomes_of_the_spec() {
        let set_state = opcode::GROUP_SPAWN;
        let query = opcode::CAPTURE;
        for (code, want) in [
            (rc::APPLIED, Outcome::Applied),
            (rc::AT_REVISION, Outcome::Applied),
            (rc::DUPLICATE, Outcome::Unknown),
            (rc::STALE_EPOCH, Outcome::Rejected),
            (rc::NO_CAMPAIGN, Outcome::Rejected),
            (rc::NOT_BOUND, Outcome::Refused),
            (rc::BAD_PAYLOAD, Outcome::Rejected),
            (rc::UNKNOWN_OPCODE, Outcome::Rejected),
            (rc::RAW_LUA_OFF, Outcome::Rejected),
            (rc::DCS_REFUSED, Outcome::Refused),
            (rc::LUA_ERROR, Outcome::Refused),
            (rc::WRONG_CAMPAIGN, Outcome::Rejected),
            (rc::WRONG_GENERATION, Outcome::Rejected),
        ] {
            assert_eq!(classify(code as u16, set_state), want, "code {code}");
            assert_eq!(classify(code as u16, query), want, "code {code}");
        }
        assert_eq!(classify(rc::TOO_LARGE as u16, set_state), Outcome::Applied);
        assert_eq!(classify(rc::TOO_LARGE as u16, query), Outcome::Refused);
        assert_eq!(
            nanoprobe_transport::layout::result::ALL.len(),
            14,
            "a code without a row here"
        );
    }

    #[test]
    fn further_keys_join_a_table_and_never_replace_the_platform_s() {
        let base = lv::obj(vec![("name", lv::s("np.u.1.2")), ("type", lv::s("M-113"))]);
        let extra = lv::obj(vec![
            ("name", lv::s("mine")),
            ("livery_id", lv::s("desert")),
        ]);
        let t = with_extra(base.clone(), &extra);
        assert_eq!(lv::str_at(&t, "name").as_deref(), Some("np.u.1.2"));
        assert_eq!(lv::str_at(&t, "livery_id").as_deref(), Some("desert"));
        assert_eq!(with_extra(base.clone(), &Lv::Nil), base);
        assert_eq!(with_extra(base.clone(), &Lv::Number(1.0)), base);
    }
}
