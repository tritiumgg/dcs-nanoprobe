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
//! command that follows from it is written. The reconciler's own state is
//! volatile: after a sidecar restart a new one is built from the store
//! ([`Reconciler::new`]) and resolves the commands it finds unanswered at the
//! agent's next `HELLO`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{gap_class, lifecycle as lc, result as rc};
use nanoprobe_transport::records::{CommandHeader, CommandResult, Hello, Lifecycle};

use crate::ids::{self, CampaignId, EntityId, NameKind};
use crate::intents::{self, Compiled};
use crate::lifecycle::{State, Trigger};
use crate::sim::wire::{Body, Command, Input, Object, ObjectGone, gone_reason, lv, opcode};
use crate::sim::{RawRecord, SimAdapter};
use crate::store::{
    Authority, Binding, Entry, Journalled, Kind, Loaded, LostPolicy, Outcome, Point, Store,
    UnitState,
};

/// The reconciler's settings (spec/config.md, `[world]`).
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Groups spawned per step.
    pub materialize_per_step: usize,
    /// The default policy for a lost group.
    pub on_lost: LostPolicy,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            materialize_per_step: 1,
            on_lost: LostPolicy::Report,
        }
    }
}

/// What the reconciler counted, for tests and metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Records that did not decode.
    pub undecodable: u64,
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
}

#[derive(Clone, Debug)]
struct EnumRun {
    id: u64,
    before: BTreeSet<u64>,
    found: BTreeSet<u64>,
    count: u32,
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
    inflight: BTreeMap<(u64, u64), Journalled>,
    run: Option<EnumRun>,
    enum_wanted: bool,
    /// The order revision the agent holds per group, as far as the reconciler
    /// knows, in the loaded generation.
    order_held: HashMap<EntityId, u64>,
    /// An order revision DCS refused; not sent again until it changes.
    order_refused: HashMap<EntityId, u64>,
    /// `CAMPAIGN` was written since the last `HELLO`.
    campaign_sent: bool,
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
            inflight,
            run: None,
            enum_wanted: false,
            order_held: HashMap::new(),
            order_refused: HashMap::new(),
            campaign_sent: false,
            counters: Counters::default(),
        }
    }

    /// What the reconciler counted.
    pub fn counters(&self) -> Counters {
        self.counters
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
    pub fn record(&mut self, store: &mut Store, r: &RawRecord) {
        let input = match Input::decode(r.kind, &r.payload) {
            Ok(i) => i,
            Err(_) => {
                self.counters.undecodable += 1;
                return;
            }
        };
        match input {
            Input::Frame(f) => {
                store.set_stamp(r.agent_boot, f.mission_gen);
                if f.replay == 0 && r.agent_boot == self.agent_boot {
                    self.lifecycle = f.lifecycle;
                }
            }
            Input::Hello(h) => self.hello(store, r.agent_boot, &h),
            Input::Lifecycle(l) => self.lifecycle(store, r.agent_boot, &l),
            Input::Result(res) => self.result(store, &res),
            Input::Gap(g) => {
                if u64::from(g.class) == gap_class::EVENTS {
                    self.enum_wanted = true;
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
            Input::EnumBegin(e) => {
                if self.current(store, r.agent_boot) {
                    self.run = Some(EnumRun {
                        id: e.enumeration,
                        before: store.binding().keys().copied().collect(),
                        found: BTreeSet::new(),
                        count: 0,
                        gap: false,
                    });
                }
            }
            Input::EnumEnd(e) => {
                if self.current(store, r.agent_boot) {
                    self.enum_end(store, e.enumeration, e.objects);
                }
            }
            Input::Positions(rows) => {
                if self.current(store, r.agent_boot) {
                    for p in rows {
                        if let Some(unit) = store.binding().get(&p.object).map(|b| b.unit) {
                            store.observe_position(
                                unit,
                                Point {
                                    x: p.x,
                                    y: p.y,
                                    z: p.z,
                                },
                            );
                        }
                    }
                }
            }
            Input::Vitals(rows) => {
                if self.current(store, r.agent_boot) {
                    for v in rows {
                        if let Some(unit) = store.binding().get(&v.object).map(|b| b.unit) {
                            store.observe_vitals(unit, v.life, v.fuel);
                        }
                    }
                }
            }
            Input::Event(_) => self.counters.events += 1,
            Input::Other(_) => {}
        }
    }

    /// Whether a record stamped with the last `FRAME` belongs to the loaded
    /// mission. Records of an older generation are ignored (spec/records.md).
    fn current(&self, store: &Store, agent_boot: u64) -> bool {
        store.loaded().is_some_and(|l| {
            l.agent_boot == agent_boot && (agent_boot, l.mission_gen) == store.stamp()
        })
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
        if h.agent_boot != self.agent_boot {
            // A new DCS process: it holds nothing of what this one knew. Its
            // LIFECYCLE records empty the binding mirror.
            self.order_held.clear();
            self.run = None;
        }
        self.agent_boot = agent_boot;
        self.attached = true;
        self.campaign_sent = false;
    }

    fn lifecycle(&mut self, store: &mut Store, agent_boot: u64, l: &Lifecycle) {
        if agent_boot == self.agent_boot || self.agent_boot == 0 {
            self.lifecycle = l.state;
        }
        let seen = Loaded {
            agent_boot,
            mission_gen: l.mission_gen,
            theater: l.theater.clone(),
        };
        let same = store
            .loaded()
            .is_some_and(|x| x.agent_boot == agent_boot && x.mission_gen == l.mission_gen);
        if same {
            if store.loaded().is_some_and(|x| x.theater != l.theater) && !l.theater.is_empty() {
                store.set_loaded(seen);
            }
            return;
        }
        // A new mission generation: DCS holds none of the platform's objects
        // and the agent has forgotten every applied revision.
        store.set_loaded(seen);
        self.order_held.clear();
        self.order_refused.clear();
        self.run = None;
        store.clear_missing();
        let ids: Vec<EntityId> = store.groups().map(|g| g.id).collect();
        for id in ids {
            let g = store.group(id).expect("group");
            let same_theater = g.theater == l.theater;
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
        let (id, state, spec_rev) = (g.id, g.state, g.spec_rev);
        match c.opcode {
            opcode::GROUP_SPAWN | opcode::STATIC_SPAWN => {
                if !gen_now || state != State::Materializing || c.rev != spec_rev {
                    return;
                }
                match outcome {
                    Outcome::Applied => {
                        if res.is_some_and(|r| u64::from(r.code) == rc::APPLIED) {
                            // A new group has no task, and an order refused
                            // for the old one may suit it.
                            self.order_held.remove(&id);
                            self.order_refused.remove(&id);
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
                            .transition(id, Trigger::SpawnUnknown)
                            .expect("materializing");
                    }
                    Outcome::Rejected | Outcome::NotSent => {}
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
                if outcome == Outcome::Applied
                    && let Some(r) = res
                    && u64::from(r.code) == rc::APPLIED
                {
                    self.take_capture(store, id, &r.value);
                }
                if c.opcode == opcode::GROUP_CAPTURE_DESTROY
                    && outcome == Outcome::Applied
                    && res.is_some_and(|r| u64::from(r.code) != rc::TOO_LARGE)
                {
                    unbind_group(store, id);
                }
                // Otherwise the last checkpointed state stands, and any object
                // still bound is destroyed by the plan.
                store
                    .transition(id, Trigger::CaptureDone)
                    .expect("dematerializing");
            }
            opcode::GROUP_DESTROY | opcode::STATIC_DESTROY
                if gen_now && outcome == Outcome::Applied =>
            {
                unbind_group(store, id);
            }
            _ => {}
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
        if c.opcode == opcode::GROUP_SPAWN
            && outcome == Outcome::Applied
            && r.object != 0
            && let Some(row) = store
                .binding_mut()
                .values_mut()
                .find(|row| row.group == c.entity)
        {
            row.dcs_group = r.object;
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
        let u = store.unit(unit)?;
        Some(Binding {
            unit,
            group: u.group,
            dcs_group: o.group,
        })
    }

    fn object(&mut self, store: &mut Store, o: &Object) {
        if let Some(run) = &mut self.run
            && o.enumeration == run.id
        {
            run.found.insert(o.object);
            run.count += 1;
        }
        let Some(row) = self.resolve_object(store, o) else {
            return;
        };
        store.set_missing(row.unit, false);
        store.binding_mut().insert(o.object, row);
    }

    fn gone(&mut self, store: &mut Store, g: ObjectGone) {
        let Some(row) = store.binding_mut().remove(&g.object) else {
            return;
        };
        match g.reason {
            gone_reason::DEAD => {
                if store.death(row.unit).unwrap_or(false) && store.all_dead(row.group) {
                    let _ = store.transition(row.group, Trigger::AllDead);
                }
            }
            gone_reason::MISSING | gone_reason::REMOVED => store.set_missing(row.unit, true),
            _ => {}
        }
    }

    fn enum_end(&mut self, store: &mut Store, id: u64, objects: u32) {
        let Some(run) = self.run.take() else {
            self.enum_wanted = true;
            return;
        };
        if run.id != id || run.count != objects || run.gap {
            self.counters.enumerations_discarded += 1;
            self.enum_wanted = true;
            return;
        }
        self.counters.enumerations += 1;
        // The agent's rule, applied to the binding: a row bound at the start
        // and not found is missing.
        for o in run.before.difference(&run.found) {
            if let Some(row) = store.binding_mut().remove(o) {
                store.set_missing(row.unit, true);
            }
        }
        let mut found: BTreeSet<EntityId> = BTreeSet::new();
        for row in store.binding().values() {
            found.insert(row.group);
        }
        let ids: Vec<(EntityId, State)> = store.groups().map(|g| (g.id, g.state)).collect();
        for (id, state) in ids {
            match state {
                State::Live if !found.contains(&id) && !store.all_dead(id) => {
                    for m in store.group(id).expect("group").members.clone() {
                        store.set_missing(m, true);
                    }
                    store.transition(id, Trigger::EnumMissing).expect("live");
                }
                State::Lost if found.contains(&id) => {
                    store.transition(id, Trigger::EnumFound).expect("lost");
                }
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
        }
        let theater = store.loaded().expect("ready").theater.clone();
        let mut bound: HashMap<EntityId, u64> = HashMap::new();
        for row in store.binding().values() {
            let e = bound.entry(row.group).or_default();
            if row.dcs_group != 0 {
                *e = row.dcs_group;
            }
        }
        let mut spawns = self.cfg.materialize_per_step;
        let ids: Vec<EntityId> = store.groups().map(|g| g.id).collect();
        for id in ids {
            if !self.attached {
                return;
            }
            let g = store.group(id).expect("group");
            let here = g.theater == theater;
            let has_objects = bound.contains_key(&id);
            match g.state {
                State::Virtual | State::Dead | State::Unspawnable | State::Lost if has_objects => {
                    // Drift: DCS holds objects of an entity it should not.
                    // A lost group with objects is settled by the next
                    // enumeration, not destroyed.
                    if g.state != State::Lost {
                        self.destroy(store, sim, id);
                    }
                }
                State::Virtual => {
                    if g.authority == Authority::Sim && here && spawns > 0 {
                        store.transition(id, Trigger::Materialize).expect("virtual");
                        if self.spawn(store, sim, id) {
                            spawns -= 1;
                        }
                    }
                }
                State::Materializing => {
                    if here
                        && spawns > 0
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
                    if !self.inflight_for(
                        store,
                        id,
                        &[opcode::GROUP_CAPTURE_DESTROY, opcode::CAPTURE],
                    ) {
                        self.dematerialize(store, sim, id);
                    }
                }
                State::Lost => match g.on_lost.unwrap_or(self.cfg.on_lost) {
                    LostPolicy::Report => {}
                    LostPolicy::Respawn => {
                        // The agent still holds the spawn's revision for this
                        // generation, so a respawn needs a new one.
                        store.bump_spec(id).expect("group");
                        store.transition(id, Trigger::PolicyRespawn).expect("lost");
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
        if self.inflight_for(store, id, &ops) {
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
                        lv::obj(vec![
                            ("name", lv::s(&ids::name(NameKind::Unit, campaign, u.id))),
                            ("type", lv::s(&u.type_name)),
                            ("x", lv::n(u.state.at.x)),
                            ("y", lv::n(u.state.at.z)),
                            ("alt", lv::n(u.state.at.y)),
                            ("heading", lv::n(u.state.heading)),
                        ])
                    })
                    .collect();
                Body::GroupSpawn {
                    country: g.country,
                    category: g.category,
                    group: lv::obj(vec![("name", lv::s(&name)), ("units", lv::list(units))]),
                    name,
                }
            }
            Kind::Static => {
                let u = living[0];
                let name = ids::name(NameKind::Static, campaign, id);
                Body::StaticSpawn {
                    country: g.country,
                    table: lv::obj(vec![
                        ("name", lv::s(&name)),
                        ("type", lv::s(&u.type_name)),
                        ("x", lv::n(u.state.at.x)),
                        ("y", lv::n(u.state.at.z)),
                        ("heading", lv::n(u.state.heading)),
                    ]),
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
        let rev = g.order_rev;
        if g.kind != Kind::Group
            || self.order_held.get(&id).copied().unwrap_or(0) >= rev
            || self.order_refused.get(&id) == Some(&rev)
            || self.inflight_for(store, id, &[opcode::GROUP_ORDER])
        {
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
        match intents::compile(intent, status, |t| {
            bound.get(&t).copied().filter(|v| *v != 0)
        }) {
            Compiled::Task(task) => {
                self.send(store, sim, id, rev, Body::GroupOrder(task));
            }
            Compiled::Done => {
                self.order_held.insert(id, rev);
            }
            Compiled::Waiting(_) => {}
        }
    }
}

fn unbind_group(store: &mut Store, id: EntityId) {
    store.binding_mut().retain(|_, r| r.group != id);
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
