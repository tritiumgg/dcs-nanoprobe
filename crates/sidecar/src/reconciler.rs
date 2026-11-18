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
use nanoprobe_transport::records::{
    Airbase, CommandHeader, CommandResult, Hello, Rates, Warehouse,
};

use crate::ids::{self, CampaignId, EntityId, NameKind};
use crate::intents::{self, Compiled, Intent, Target};
use crate::lifecycle::{State, Trigger};
use crate::sim::wire::{Body, Command, Input, Object, ObjectGone, Seen, gone_reason, lv, opcode};
use crate::sim::{RawRecord, SimAdapter};
use crate::stock::{Item, Stock};
mod adopt;
mod debris;
mod players;
mod ruins;

use crate::store::{
    Authority, CapturedMore, Entry, Journalled, Kind, LeftAs, Loaded, LostPolicy, Outcome, Point,
    Store, UnitState,
};

/// A `NOT_READY` hold: the revision, the lifecycle change count and the FRAME
/// count at the answer.
type NotReady = (u64, u64, u64);

/// A spawn, order, destroy, capture or extension answered `NOT_READY` is not
/// sent again until a lifecycle change, a `HELLO`, or this many FRAMEs are
/// read since the answer. A NOT_READY with no lifecycle cause (a pump that
/// never reached `command`, a shim reloaded without a LIFECYCLE record) would
/// otherwise hold until the next `HELLO`. One FRAME comes per pump, so 300 is
/// a few seconds of a running mission: slow enough that a shim that is still
/// starting is not asked every frame, quick enough that a lost cause is
/// retried.
pub const NOT_READY_HOLD_FRAMES: u64 = 300;

const HOLD_SPAWN: u8 = 0;
const HOLD_ORDER: u8 = 1;
const HOLD_DESTROY: u8 = 2;
const HOLD_CAPTURE: u8 = 3;
const HOLD_OPTION: u8 = 4;
const HOLD_BASES: u8 = 5;
const HOLD_COUNTRIES: u8 = 6;

/// Results of effects and queries kept for the API to read.
const EFFECT_RESULTS_KEPT: usize = 1024;

/// The reconciler's settings (spec/config.md, `[world]`).
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Spawns unanswered at a time outside the restore burst, so at most
    /// this many reach DCS in one frame (spec/world.md, "Authority").
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
    /// Records about an entity already `dead`. They change nothing, except
    /// that a death in DCS of the object the store knows of a dead static
    /// marks that DCS holds its ruin (spec/world.md, "Ruins").
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
    /// `OBJECT` records whose coalition differs from the one the country map
    /// gives for the object's country (spec/commands.md, `COUNTRIES`).
    pub coalition_disagreements: u64,
    /// `DAMAGE` commands sent to give a spawned unit its captured life.
    pub lives_restored: u64,
    /// Those that DCS did not apply, or that could not be sent: the unit
    /// runs at the life DCS gave it, and the next life sample says so.
    pub lives_lost: u64,
    /// Ruins DCS spawned: `STATIC_SPAWN`s with `dead = true` answered
    /// `APPLIED` (spec/world.md, "Ruins").
    pub ruins_spawned: u64,
    /// Ruin spawns DCS refused, or the agent rejected for good: not sent
    /// again in that generation.
    pub ruins_refused: u64,
    /// Destroys written for a live object bound to a dead static.
    pub ruins_revived: u64,
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
        self.coalition_disagreements += o.coalition_disagreements;
        self.lives_restored += o.lives_restored;
        self.lives_lost += o.lives_lost;
        self.ruins_spawned += o.ruins_spawned;
        self.ruins_refused += o.ruins_refused;
        self.ruins_revived += o.ruins_revived;
    }
}

/// What the latest samples say about one airbase and its warehouse.
#[derive(Clone, Debug, Default)]
struct BaseSeen {
    /// The airbase's object ID: a FARP's is its static's.
    object: u64,
    coalition: Option<u32>,
    stock: Option<Stock>,
}

/// How an effect or a query the API sent ended, kept for the API to read.
#[derive(Clone, Debug)]
pub struct EffectResult {
    /// The outcome (spec/errors.md).
    pub outcome: Outcome,
    /// The agent's result, absent when it was lost or the agent never took
    /// the command.
    pub result: Option<CommandResult>,
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

/// The bind-lag diagnostic's state ([`Reconciler::measure_binds`]).
#[derive(Debug, Default)]
struct BindLags {
    /// The units of applied spawns with no `OBJECT` yet, and when their
    /// spawn's result was read.
    waiting: HashMap<EntityId, std::time::Instant>,
    /// The lags measured and not yet taken, ms.
    ms: Vec<u64>,
    /// Of those, the units whose `OBJECT` came after their spawn's result.
    late: u64,
}

/// The most units and lags the diagnostic holds.
const BIND_LAGS_MAX: usize = 65_536;

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
    inflight: Inflight,
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
    /// A test diagnostic, off unless [`Reconciler::measure_binds`] turned it
    /// on: the time from a spawn's result to each unit's `OBJECT`.
    bind_lags: Option<BindLags>,
    /// The revision of each extension the agent holds, as far as answers
    /// show, in this agent boot and campaign.
    ext_held: HashMap<String, u64>,
    /// An extension revision the agent refused: not sent again until the
    /// revision changes.
    ext_refused: HashMap<String, u64>,
    /// An extension revision the agent answered `NOT_READY`, with the
    /// lifecycle change count and the FRAME count at the answer: not sent
    /// again until the revision, a lifecycle change or the agent's `HELLO`
    /// changes, or [`NOT_READY_HOLD_FRAMES`] FRAMEs are read.
    ext_not_ready: HashMap<String, NotReady>,
    /// A spawn, order, destroy or capture of one entity the agent answered
    /// `NOT_READY` (the key's `u8` is a `HOLD_` constant), held under the same
    /// rule as extensions. A destroy or capture has revision 0.
    cmd_not_ready: HashMap<(EntityId, u8), NotReady>,
    /// FRAMEs read: the second clock of the `NOT_READY` holds.
    frames_read: u64,
    /// LIFECYCLE records read, and FRAMEs that changed the state: the clock
    /// the `NOT_READY` holds run on. `plan` sees only RUNNING and PAUSED, so
    /// a state value cannot tell a restart from no change.
    lifecycle_changes: u64,
    /// The extension each `EXTENSION_SET` in flight names, by (epoch, seq).
    ext_sent: HashMap<(u64, u64), String>,
    /// Marks, drawings, menu items and extension removals (`players.rs`).
    pins: players::PinState,
    /// Drift destroys (of an entity DCS should not hold) refused or unknown,
    /// by entity, in the loaded generation. At the limit they stop.
    drift_destroy_failures: HashMap<EntityId, u32>,
    /// A spawn the agent rejected with a code that a resend cannot fix, by
    /// spec revision and generation; not sent again until either changes.
    spawn_rejected: HashMap<EntityId, (u64, u32)>,
    /// The spec revision the latest applied spawn of each entity carried, in
    /// the loaded generation. A live entity whose store `spec_rev` is ahead
    /// of it is spawned again, in place (spec/world.md, "Revisions").
    spawn_applied: HashMap<EntityId, u64>,
    /// A restored unit's life, as DCS reads it, held until the `DAMAGE` that
    /// sets it is applied: a sample or capture before then, at the life the
    /// spawn gave, does not replace it (spec/world.md, "Spawn state"). By
    /// unit, with the (epoch, seq) of that `DAMAGE`. The life is None for a
    /// unit whose life is the percentage a `PutGroup` asked for: the store
    /// keeps that percentage until the `DAMAGE` is applied.
    life_held: HashMap<EntityId, (Option<f32>, (u64, u64))>,
    /// The DCS object ID of the ruin spawned for each dead static in the
    /// loaded generation, from the spawn's `RESULT` (`ruins.rs`).
    ruin_object: HashMap<EntityId, u64>,
    /// The controller options the current spawn of each group applied.
    option_held: HashMap<EntityId, BTreeMap<u32, Lv>>,
    /// An option value DCS refused, by group and option; not sent again until
    /// the value changes or the group is spawned again.
    option_refused: HashMap<(EntityId, u32), Lv>,
    /// The group, option and value each `GROUP_OPTION` in flight names.
    opt_sent: HashMap<(u64, u64), (EntityId, u32, Lv)>,
    /// What the latest samples say, by airbase name, in the loaded
    /// generation.
    bases: BTreeMap<String, BaseSeen>,
    /// The desired-owner revision sent per airbase in this generation.
    owner_sent: HashMap<String, u64>,
    /// The desired-stock revision sent per airbase in this generation.
    stock_sent: HashMap<String, u64>,
    /// The airbase each `AIRBASE_OWNER` (true) or `WAREHOUSE_SET` (false) in
    /// flight names.
    base_sent: HashMap<(u64, u64), (bool, String, u32, Stock)>,
    /// Owners per airbase that `AIRBASE_OWNER` commands applied since the
    /// airbase's last sample. A sample showing one is our effect, not a fact.
    own_owner: HashMap<String, Vec<u32>>,
    /// The same for the stocks `WAREHOUSE_SET` commands applied.
    own_stock: HashMap<String, Vec<Stock>>,
    /// The airbase and delta of each `WAREHOUSE_ADJUST` in flight.
    adjust_sent: HashMap<(u64, u64), (String, Stock)>,
    /// The rates last written since the last `HELLO`.
    rates_sent: Option<Rates>,
    /// The units the players occupy, which `SUBSCRIBE` names (`players.rs`).
    subscribe_want: BTreeSet<EntityId>,
    /// The set last written since the last `HELLO`.
    subscribe_sent: Option<BTreeSet<EntityId>>,
    /// The unmanaged units players occupy, bound by the sidecar (`adopt.rs`).
    adopted: adopt::Adopted,
    /// The agent's `COUNTRIES` answer for this generation is in the store.
    countries_held: bool,
    /// Results of effects and queries, by (epoch, seq), for the API.
    effects: BTreeMap<(u64, u64), EffectResult>,
    /// Whole-map `CLEAR_DEBRIS` clears whose tiles are answering (`debris.rs`).
    debris_batches: Vec<debris::Batch>,
    /// Clears of one volume in flight, by (epoch, seq) (`debris.rs`).
    debris_single: BTreeSet<(u64, u64)>,
    /// Spawn nothing: an activation's mission load is owed or the mission it
    /// loads has not been recorded yet (spec/persistence.md, "Campaign
    /// operations").
    hold: bool,
    /// The restore burst is on (spec/persistence.md, "Restore burst").
    burst: bool,
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
        rc::STALE_EPOCH | rc::WRONG_CAMPAIGN | rc::WRONG_GENERATION | rc::NOT_READY
    )
}

/// The commands written and not yet answered, by epoch and sequence
/// number, with an index by entity.
#[derive(Debug, Default)]
struct Inflight {
    map: BTreeMap<(u64, u64), Journalled>,
    by_entity: HashMap<EntityId, BTreeSet<(u64, u64)>>,
}

impl Inflight {
    fn insert(&mut self, c: Journalled) {
        let key = (c.epoch, c.seq);
        if let Some(old) = self.remove(&key) {
            debug_assert_eq!(old.entity, c.entity, "a sequence number used twice");
        }
        self.by_entity.entry(c.entity).or_default().insert(key);
        self.map.insert(key, c);
    }

    fn remove(&mut self, key: &(u64, u64)) -> Option<Journalled> {
        let c = self.map.remove(key)?;
        self.unindex(c.entity, *key);
        Some(c)
    }

    fn unindex(&mut self, entity: EntityId, key: (u64, u64)) {
        if let Some(keys) = self.by_entity.get_mut(&entity) {
            keys.remove(&key);
            if keys.is_empty() {
                self.by_entity.remove(&entity);
            }
        }
    }

    fn get(&self, key: &(u64, u64)) -> Option<&Journalled> {
        self.map.get(key)
    }

    fn values(&self) -> impl Iterator<Item = &Journalled> {
        self.map.values()
    }

    /// The unanswered commands of one entity.
    fn of(&self, entity: EntityId) -> impl Iterator<Item = &Journalled> {
        self.by_entity
            .get(&entity)
            .into_iter()
            .flatten()
            .filter_map(|k| self.map.get(k))
    }
}

impl Reconciler {
    /// A reconciler for a store: at the sidecar's start, or after a restart.
    pub fn new(cfg: Config, store: &Store) -> Reconciler {
        let mut inflight = Inflight::default();
        for c in store.unanswered() {
            inflight.insert(c);
        }
        Reconciler {
            cfg,
            agent_boot: store.loaded().map_or(0, |l| l.agent_boot),
            attached: false,
            epoch: 0,
            next_seq: 1,
            lifecycle: 0,
            // Records delivered again after a restart come before the next
            // FRAME: they keep the stamp they were first read under.
            stamp: store.last_stamp().unwrap_or((0, 0, 0)),
            inflight,
            binding: HashMap::new(),
            run: None,
            unresolved: BTreeMap::new(),
            guard: false,
            enum_wanted: false,
            order_held: HashMap::new(),
            order_refused: HashMap::new(),
            bind_lags: None,
            ext_held: HashMap::new(),
            ext_refused: HashMap::new(),
            ext_not_ready: HashMap::new(),
            cmd_not_ready: HashMap::new(),
            lifecycle_changes: 0,
            frames_read: 0,
            ext_sent: HashMap::new(),
            pins: players::PinState::default(),
            drift_destroy_failures: HashMap::new(),
            spawn_rejected: HashMap::new(),
            spawn_applied: HashMap::new(),
            life_held: HashMap::new(),
            ruin_object: HashMap::new(),
            option_held: HashMap::new(),
            option_refused: HashMap::new(),
            opt_sent: HashMap::new(),
            bases: BTreeMap::new(),
            owner_sent: HashMap::new(),
            stock_sent: HashMap::new(),
            base_sent: HashMap::new(),
            own_owner: HashMap::new(),
            own_stock: HashMap::new(),
            adjust_sent: HashMap::new(),
            rates_sent: None,
            subscribe_want: BTreeSet::new(),
            subscribe_sent: None,
            adopted: adopt::Adopted::default(),
            countries_held: false,
            effects: BTreeMap::new(),
            debris_batches: Vec::new(),
            debris_single: BTreeSet::new(),
            hold: false,
            campaign_sent: false,
            burst: false,
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

    /// Turn on the bind-lag diagnostic: from here on the reconciler times
    /// each unit from its group's applied spawn to the `OBJECT` record that
    /// binds it. A test measurement, not part of the product's interface.
    pub fn measure_binds(&mut self) {
        self.bind_lags.get_or_insert_with(BindLags::default);
    }

    /// The bind lags measured since the last call, ms, in the order the
    /// units were bound; the units of applied spawns still unbound; and, of
    /// the lags, how many are of a unit whose `OBJECT` came after its
    /// spawn's result. Empty when the diagnostic is off. A unit that left
    /// DCS before its `OBJECT` came (its group destroyed, dematerialized or
    /// dead) stays in the unbound count: the count is an upper bound.
    pub fn take_bind_lags(&mut self) -> (Vec<u64>, usize, u64) {
        self.bind_lags.as_mut().map_or((Vec::new(), 0, 0), |b| {
            (
                std::mem::take(&mut b.ms),
                b.waiting.len(),
                std::mem::take(&mut b.late),
            )
        })
    }

    /// A spawn of `group` was applied: its members that are not bound yet
    /// wait for their `OBJECT` from now; one bound already has no lag.
    fn bind_lags_spawned(&mut self, store: &Store, group: EntityId) {
        let Some(b) = self.bind_lags.as_mut() else {
            return;
        };
        let Some(g) = store.group(group) else {
            return;
        };
        let now = std::time::Instant::now();
        for &m in &g.members {
            if self.binding.values().any(|r| r.unit == m) {
                b.ms.push(0);
            } else if b.waiting.len() < BIND_LAGS_MAX {
                b.waiting.insert(m, now);
            }
        }
    }

    /// An `OBJECT` bound `unit`.
    fn bind_lags_bound(&mut self, unit: EntityId) {
        if let Some(b) = self.bind_lags.as_mut()
            && let Some(since) = b.waiting.remove(&unit)
            && b.ms.len() < BIND_LAGS_MAX
        {
            b.ms.push(u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX));
            b.late += 1;
        }
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
    /// Then write the store in one transaction and let the step's commands
    /// through (spec/persistence.md, "Writes"). A failed write stops the
    /// sidecar through its panic path: no command of the step reaches DCS.
    pub fn step(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) {
        self.step_held(store, sim);
        if let Err(e) = store.commit() {
            panic!("the store could not be written, stopping: {e}");
        }
        sim.flush();
    }

    /// [`Reconciler::step`] without the write and the flush, for a driver
    /// that writes other files in the same step first. The commands it sent
    /// stay held until the driver's flush.
    pub fn step_held(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) {
        self.adopt_refresh(store);
        for r in sim.read() {
            if store.folded(&r) {
                continue;
            }
            self.record(store, &r);
            if r.agent_boot != 0 {
                store.set_last_record(r.agent_boot, r.number, self.stamp);
            }
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
                self.frames_read += 1;
                self.stamp = (r.agent_boot, f.mission_gen, f.campaign);
                store.set_sample_time(f.model_time);
                if f.replay == 0 {
                    if r.agent_boot == self.agent_boot || self.agent_boot == 0 {
                        if self.lifecycle != f.lifecycle {
                            self.lifecycle_changes += 1;
                        }
                        self.lifecycle = f.lifecycle;
                    }
                    self.seen_mission(store, r.agent_boot, f.mission_gen, None);
                }
            }
            Input::Hello(h) => {
                // The samples before a HELLO make no segment with those after.
                store.break_samples();
                self.hello(store, r.agent_boot, &h, r.resync)
            }
            Input::Lifecycle(l) => {
                self.lifecycle_changes += 1;
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
                    self.adopt_object(store, &o);
                }
            }
            Input::ObjectGone(g) => {
                if self.current(store, r.agent_boot) {
                    self.base_gone(g.object);
                    self.adopt_gone(g.object);
                    self.gone(store, g);
                }
            }
            Input::Seen(s) => {
                if self.current(store, r.agent_boot) {
                    self.seen(store, &s);
                    self.adopt_seen(&s);
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
                            match self.life_held.get(&b.unit) {
                                None if self.life_asked(store, b.unit) => {}
                                None => store.observe_vitals(b.unit, v.life),
                                Some((Some(held), _)) => store.observe_vitals(b.unit, *held),
                                Some((None, _)) => {}
                            }
                        }
                    }
                }
            }
            Input::Airbase(a) => {
                if self.current(store, r.agent_boot) {
                    self.airbase_sample(store, &a);
                }
            }
            Input::Warehouse(w) => {
                if self.current(store, r.agent_boot) {
                    self.warehouse_sample(store, &w);
                }
            }
            Input::Event(_) => self.counters.events += 1,
            // Read by the API's event log, not by the reconciler. The API
            // hub folds an `AMMO` into the store's unit.
            Input::Other(_)
            | Input::Ammo(_)
            | Input::AuthorEvent(_)
            | Input::Player(_)
            | Input::Menu(_)
            | Input::Extension(_) => self.counters.other += 1,
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
        self.forget_generation(store);
        self.pins_forget(store, false);
        self.spawn_rejected.clear();
        self.spawn_applied.clear();
        self.life_held.clear();
        self.ruin_object.clear();
        self.drift_destroy_failures.clear();
        self.held.clear();
        self.run = None;
        // A load leaves PAUSED, which ends the burst in the agent.
        self.burst = false;
        store.new_generation();
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

    fn hello(&mut self, store: &mut Store, agent_boot: u64, h: &Hello, resync: bool) {
        // Every command the HELLO does not answer: taken (at or before
        // in_head) and its result lost, or never taken. A command of another
        // agent boot cannot be answered any more, whatever its position
        // (spec/commands.md, "After DCS dies"). A resync HELLO leaves the
        // ring as it was, so a command after its in_head is still there and
        // stays pending (spec/commands.md, "After CONTROL_LOST").
        let open: Vec<Journalled> = self.inflight.values().cloned().collect();
        for c in open {
            let outcome = if c.agent_boot != h.agent_boot || c.position <= h.in_head {
                Outcome::Unknown
            } else if resync {
                continue;
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
            self.ext_held.clear();
            self.ext_refused.clear();
            self.forget_generation(store);
            self.pins_forget(store, true);
            self.run = None;
        }
        self.rates_sent = None;
        self.subscribe_sent = None;
        self.ext_not_ready.clear();
        self.cmd_not_ready.clear();
        self.agent_boot = agent_boot;
        self.attached = true;
        self.campaign_sent = false;
        self.enumerated = false;
    }

    /// What belongs to one mission generation and one agent: applied
    /// options, airbase and warehouse samples and what was sent for them,
    /// and the country map.
    fn forget_generation(&mut self, store: &mut Store) {
        self.option_held.clear();
        self.option_refused.clear();
        self.opt_sent.clear();
        self.bases.clear();
        self.owner_sent.clear();
        self.stock_sent.clear();
        self.base_sent.clear();
        self.own_owner.clear();
        self.own_stock.clear();
        self.adjust_sent.clear();
        self.countries_held = false;
        store.set_countries(None);
    }

    /// A group's spawn is new, or the group is gone: its options are
    /// applied no more.
    fn forget_options(&mut self, id: EntityId) {
        self.option_held.remove(&id);
        self.option_refused.retain(|(e, _), _| *e != id);
        self.opt_sent.retain(|_, (e, _, _)| *e != id);
    }

    fn resolve(
        &mut self,
        store: &mut Store,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        self.inflight.remove(&(c.epoch, c.seq));
        // A life-restore DAMAGE is the sidecar's own: nothing reads its
        // result, so it stays out of the effect results.
        let mut restore = false;
        if c.opcode == opcode::DAMAGE {
            restore = self.life_resolved((c.epoch, c.seq), outcome, res.map(|r| r.code));
        }
        store.journal(Entry::Outcome {
            epoch: c.epoch,
            seq: c.seq,
            outcome,
        });
        if c.opcode == opcode::CLEAR_DEBRIS && self.fold_debris(c.epoch, c.seq, outcome, res) {
            return;
        }
        if !restore
            && matches!(
                c.opcode,
                opcode::EXTENSION_INVOKE
                    | opcode::WAREHOUSE_ADJUST
                    | opcode::DAMAGE
                    | opcode::SOUND
                    | opcode::PICTURE
                    | opcode::PICTURE_STOP
                    | opcode::CAPTURE
                    | opcode::MESSAGE
                    | opcode::CHAT
                    | opcode::EVAL
                    | opcode::MISSION_LOAD
                    | opcode::MODERATE
                    | opcode::PLAYERS
                    | opcode::BANS
                    | opcode::CLEAR_DEBRIS
            )
        {
            self.effects.insert(
                (c.epoch, c.seq),
                EffectResult {
                    outcome,
                    result: res.cloned(),
                },
            );
            while self.effects.len() > EFFECT_RESULTS_KEPT {
                self.effects.pop_first();
            }
        }
        match c.opcode {
            opcode::EXTENSION_SET => {
                self.extension_resolved(c, outcome, res);
                return;
            }
            opcode::WAREHOUSE_ADJUST => {
                // Applied, the adjustment is a change of the desired counts:
                // it does not wait for a sample (LIVE, E6: clamped at 0).
                if let Some((airbase, delta)) = self.adjust_sent.remove(&(c.epoch, c.seq))
                    && outcome == Outcome::Applied
                    && let Some(theater) = store.loaded().map(|l| l.theater.clone())
                {
                    store.adjust_desired_stock(&theater, &airbase, &delta);
                }
                return;
            }
            // An effect: reported, never sent again.
            opcode::EXTENSION_INVOKE
            | opcode::DAMAGE
            | opcode::SOUND
            | opcode::PICTURE
            | opcode::PICTURE_STOP
            | opcode::MESSAGE
            | opcode::CHAT
            | opcode::EVAL
            | opcode::MISSION_LOAD
            | opcode::MODERATE
            | opcode::PLAYERS
            | opcode::BANS
            | opcode::CLEAR_DEBRIS => return,
            opcode::EXTENSION_REMOVE => {
                self.extension_removed(store, c, outcome, res);
                return;
            }
            opcode::MARK_SET
            | opcode::MARK_REMOVE
            | opcode::DRAWING_SET
            | opcode::DRAWING_REMOVE
            | opcode::MENU_SET
            | opcode::MENU_REMOVE => {
                self.pin_resolved(store, c, outcome, res);
                return;
            }
            opcode::AIRBASE_OWNER | opcode::WAREHOUSE_SET => {
                self.base_resolved(c, outcome, res);
                return;
            }
            opcode::COUNTRIES => {
                self.countries_resolved(store, c, outcome, res);
                return;
            }
            _ => {}
        }
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
        if c.opcode == opcode::STATIC_SPAWN && c.units.is_empty() {
            // The spawn of a ruin: it names no unit it brings to life.
            if state == State::Dead && gen_now {
                self.ruin_spawn_resolved(store, c, outcome, res);
            }
            return;
        }
        if state == State::Dead {
            self.counters.after_dead += 1;
            return;
        }
        match c.opcode {
            opcode::GROUP_SPAWN | opcode::STATIC_SPAWN => {
                if !gen_now {
                    return;
                }
                // A live entity whose spec changed is spawned again in place
                // and stays live: the spawn replaces the group in DCS.
                let again = match state {
                    State::Materializing if c.rev == spec_rev => false,
                    State::Live => true,
                    _ => return,
                };
                match outcome {
                    Outcome::Applied => {
                        // The group may be new, even when the answer is
                        // AT_REVISION to a resend of a spawn whose result was
                        // lost: a new group has no task, and an order refused
                        // for the old one may suit it. Sending the order again
                        // costs at most an AT_REVISION.
                        let at = self.spawn_applied.entry(id).or_default();
                        *at = (*at).max(c.rev);
                        self.order_held.remove(&id);
                        self.order_refused.remove(&id);
                        self.forget_options(id);
                        self.pins_group_respawned(store, id);
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
                                        && matches!(a.order, Some(Intent::Attack { target })
                                            if target == id
                                                || store.unit(target).is_some_and(|u| u.group == id))
                                })
                                .map(|a| a.id)
                                .collect();
                            for a in attackers {
                                store.bump_order(a).expect("group");
                            }
                        }
                        if !again {
                            store
                                .transition(id, Trigger::SpawnApplied)
                                .expect("materializing");
                            self.bind_lags_spawned(store, id);
                        }
                        if store.all_dead(id) {
                            store.transition(id, Trigger::AllDead).expect("live");
                        }
                    }
                    Outcome::Refused => {
                        let msg = res.map(|r| r.message.clone()).unwrap_or_default();
                        if again {
                            // Not retried at this revision.
                            self.spawn_rejected.insert(id, (c.rev, c.mission_gen));
                        } else {
                            store
                                .transition(id, Trigger::SpawnRefused)
                                .expect("materializing");
                        }
                        store.set_error(id, msg).expect("group");
                    }
                    Outcome::Unknown => {
                        store
                            .spawn_unknown(id, c.agent_boot, c.mission_gen)
                            .expect("group");
                        if !again {
                            store
                                .transition(id, Trigger::SpawnUnknown)
                                .expect("materializing");
                        }
                    }
                    Outcome::Rejected => {
                        if let Some(r) = res {
                            if u64::from(r.code) == rc::NOT_READY {
                                self.hold_not_ready(id, HOLD_SPAWN, c.rev);
                            } else if !rejected_for_now(r.code) {
                                self.spawn_rejected.insert(id, (c.rev, c.mission_gen));
                            }
                        }
                    }
                    Outcome::NotSent => {}
                }
            }
            opcode::GROUP_OPTION => self.option_resolved(c, gen_now, outcome, code),
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
                    Outcome::Rejected if code == Some(rc::NOT_READY) => {
                        self.hold_not_ready(id, HOLD_ORDER, c.rev);
                    }
                    _ => {}
                }
            }
            opcode::GROUP_CAPTURE_DESTROY | opcode::CAPTURE => {
                if gen_now
                    && state != State::Dematerializing
                    && c.opcode == opcode::CAPTURE
                    && code == Some(rc::APPLIED)
                    && let Some(r) = res
                {
                    // A client's capture: the store takes what DCS read.
                    self.take_capture(store, id, &r.value);
                }
                if !gen_now || state != State::Dematerializing {
                    return;
                }
                if outcome == Outcome::Rejected || outcome == Outcome::NotSent {
                    // Not tried: the next plan sends it again, but not for
                    // a while after NOT_READY.
                    if code == Some(rc::NOT_READY) {
                        self.hold_not_ready(id, HOLD_CAPTURE, 0);
                    }
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
                    self.forget_options(id);
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
            self.forget_options(id);
            unbind_group(&mut self.binding, id);
            self.drift_destroy_failures.remove(&id);
            self.ruin_destroyed(store, id);
            if state == State::Dematerializing && captured {
                store
                    .transition(id, Trigger::CaptureDone)
                    .expect("dematerializing");
            }
        } else if outcome == Outcome::Rejected && code == Some(rc::NOT_READY) {
            self.hold_not_ready(id, HOLD_DESTROY, 0);
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
                if outcome == Outcome::Unknown {
                    // The destroy may have run: DCS may hold the ruin no
                    // more (`ruins.rs`).
                    self.ruin_destroy_unknown(store, id);
                }
            }
        }
    }

    /// Read a captured state: a list of tables with the keys of
    /// spec/commands.md, `GROUP_CAPTURE_DESTROY`. A read that raised leaves
    /// its key out, and the unit keeps what the store had for it.
    fn take_capture(&mut self, store: &mut Store, group: EntityId, value: &Lv) {
        let campaign = store.campaign();
        for item in lv::items(value) {
            let Some(name) = lv::str_at(item, "name") else {
                continue;
            };
            let Some(p) = ids::parse(&name) else { continue };
            if !ids::of_campaign(&p, campaign)
                || store.unit(p.entity).is_none_or(|u| u.group != group)
            {
                continue;
            }
            let u = store.unit(p.entity).expect("unit");
            let (prev, prev_more) = (u.state, u.captured.clone());
            let asked = self.life_asked(store, p.entity);
            let num = |k: &str, d: f64| lv::num_at(item, k).unwrap_or(d);
            store.observe_capture(
                p.entity,
                UnitState {
                    at: Point {
                        x: num("x", prev.at.x),
                        y: num("y", prev.at.y),
                        z: num("z", prev.at.z),
                    },
                    heading: num("heading", prev.heading),
                    life: match self.life_held.get(&p.entity) {
                        Some((held, _)) => held.unwrap_or(prev.life),
                        None if asked => prev.life,
                        None => num("life", f64::from(prev.life)) as f32,
                    },
                },
            );
            store.observe_capture_more(
                p.entity,
                CapturedMore {
                    life0: num("life0", prev_more.life0),
                    pitch: num("pitch", prev_more.pitch),
                    roll: num("roll", prev_more.roll),
                    // observe_capture cleared what a PutGroup asked for;
                    // a held percentage stays until its DAMAGE is applied.
                    life_percent: match self.life_held.get(&p.entity) {
                        Some((None, _)) => prev_more.life_percent,
                        None if asked => prev_more.life_percent,
                        _ => None,
                    },
                },
                num("id", 0.0) as u64,
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
            && !ids::of_campaign(&n, campaign)
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
        // A member DCS lacked, MISSING or REMOVED and found again: the
        // group may take an order DCS refused while it held none of it, as
        // after a revival (spec/commands.md, "Layout: GROUP_ORDER").
        if store
            .unit(row.unit)
            .is_some_and(|u| !u.dead && (u.missing || u.removed))
        {
            self.order_refused.remove(&row.group);
        }
        store.set_missing(row.unit, false);
        store.set_object(row.unit, o.object);
        self.bind_lags_bound(row.unit);
        store.observe_life0(row.unit, f64::from(o.life0));
        store.observe_coalition(row.group, o.coalition);
        if let Some(c) = store.country_coalition(o.country)
            && c != o.coalition
        {
            self.counters.coalition_disagreements += 1;
        }
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
            gone_reason::DEAD if state == State::Dead => {
                self.counters.after_dead += 1;
                // A static the lost policy marked dead while DCS still held
                // its object (its destroys were refused): that object died
                // here, and DCS holds it dead. It is the static's ruin for
                // this generation, destroyed by the plan when the static
                // should have none, and no ruin is spawned over it when it
                // should. A death of an older object says nothing of the
                // one DCS holds now.
                if !stale {
                    self.ruin_died(store, group);
                }
            }
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
                    // A static that dies leaves its dead object in DCS:
                    // that is its ruin for this generation.
                    self.ruin_died(store, group);
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
        if seen == 0 {
            self.adopt_orphans(store);
        }
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
                    self.option_refused.retain(|(e, _), _| *e != id);
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
        let mission_gen = store.loaded().map_or(0, |l| l.mission_gen);
        let control = body.is_control();
        let header = CommandHeader {
            seq: if control { 0 } else { self.next_seq },
            epoch: if control { 0 } else { self.epoch },
            campaign: store.campaign().0,
            entity: entity.0,
            rev,
            mission_gen,
        };
        let units = match &body {
            Body::GroupSpawn { group, .. } => unit_names(group, store.campaign(), "units"),
            // A ruin's spawn brings no unit to life.
            Body::StaticSpawn { table, .. } if ruins::spawns_dead(table) => Vec::new(),
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
            mission_gen,
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
                    self.inflight.insert(j);
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

    /// The clocks of a `NOT_READY` answer read now.
    fn not_ready_now(&self, rev: u64) -> NotReady {
        (rev, self.lifecycle_changes, self.frames_read)
    }

    fn hold_not_ready(&mut self, id: EntityId, what: u8, rev: u64) {
        let hold = self.not_ready_now(rev);
        self.cmd_not_ready.insert((id, what), hold);
    }

    /// Whether a `NOT_READY` hold for `rev` still stands: the revision and
    /// the lifecycle are those of the answer, and fewer than
    /// [`NOT_READY_HOLD_FRAMES`] FRAMEs were read since.
    fn is_held(&self, hold: Option<&NotReady>, rev: u64) -> bool {
        hold.is_some_and(|h| {
            h.0 == rev
                && h.1 == self.lifecycle_changes
                && self.frames_read.saturating_sub(h.2) < NOT_READY_HOLD_FRAMES
        })
    }

    fn inflight_for(&self, store: &Store, entity: EntityId, ops: &[u16]) -> bool {
        let Some(l) = store.loaded() else {
            return false;
        };
        self.inflight.of(entity).any(|c| {
            ops.contains(&c.opcode)
                && c.agent_boot == l.agent_boot
                && c.mission_gen == l.mission_gen
        })
    }

    /// Spawns written for the loaded mission and not yet answered.
    fn spawns_inflight(&self, loaded: &Loaded) -> usize {
        self.inflight
            .values()
            .filter(|c| {
                matches!(c.opcode, opcode::GROUP_SPAWN | opcode::STATIC_SPAWN)
                    && c.agent_boot == loaded.agent_boot
                    && c.mission_gen == loaded.mission_gen
            })
            .count()
    }

    /// An `EXTENSION_SET` resolved. Applied, the agent holds the revision;
    /// refused, or rejected for good, it is not sent again until the
    /// revision changes; any other outcome derives it again (spec/errors.md).
    fn extension_resolved(
        &mut self,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        let Some(name) = self.ext_sent.remove(&(c.epoch, c.seq)) else {
            // Sent before a sidecar restart: derived again, and an
            // AT_REVISION answer shows it held.
            return;
        };
        if c.agent_boot != self.agent_boot {
            return;
        }
        match outcome {
            Outcome::Applied => {
                let held = self.ext_held.entry(name).or_default();
                *held = (*held).max(c.rev);
            }
            Outcome::Refused => {
                self.ext_refused.insert(name, c.rev);
            }
            Outcome::Rejected if !res.is_some_and(|r| rejected_for_now(r.code)) => {
                self.ext_refused.insert(name, c.rev);
            }
            Outcome::Rejected if res.is_some_and(|r| u64::from(r.code) == rc::NOT_READY) => {
                let hold = self.not_ready_now(c.rev);
                self.ext_not_ready.insert(name, hold);
            }
            _ => {}
        }
    }

    /// Send what the store's extensions need: every one whose revision the
    /// agent does not hold, once at a time. Returns false when a send failed.
    fn extensions(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) -> bool {
        let wanted: Vec<(String, u64, String)> = store
            .extensions()
            .map(|e| (e.name.clone(), e.spec_rev, e.source.clone()))
            .collect();
        for (name, rev, source) in wanted {
            if self.ext_held.get(&name).is_some_and(|h| *h >= rev)
                || self.ext_refused.get(&name) == Some(&rev)
                || self.is_held(self.ext_not_ready.get(&name), rev)
                || self.ext_sent.values().any(|n| *n == name)
            {
                continue;
            }
            let key = (self.epoch, self.next_seq);
            let body = Body::ExtensionSet {
                name: name.clone(),
                source: Lv::String(source.into_bytes()),
            };
            if !self.send(store, sim, EntityId(0), rev, body) {
                return false;
            }
            self.ext_sent.insert(key, name);
        }
        true
    }

    /// Run an extension's `invoke` in DCS with `args`, an array: an
    /// `EXTENSION_INVOKE`, an effect, never sent again. Returns the
    /// command's (epoch, seq), whose outcome the journal gets, or None when
    /// nothing can be sent now.
    pub fn invoke(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        name: &str,
        args: Lv,
    ) -> Option<(u64, u64)> {
        if !self.ready(store) || !self.attached || !self.campaign_sent {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let body = Body::ExtensionInvoke {
            name: name.to_owned(),
            args,
        };
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Turn the restore burst on while paused with more groups waiting to
    /// materialize than one frame's share, and off once none waits or the
    /// state leaves PAUSED (spec/persistence.md, "Restore burst"). False
    /// when the link dropped.
    fn restore_burst(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        loaded: &Loaded,
    ) -> bool {
        let paused = u64::from(self.lifecycle) == lc::PAUSED;
        let waiting = store
            .groups()
            .filter(|g| {
                (g.state == State::Materializing
                    && g.theater == loaded.theater
                    && g.authority == Authority::Sim)
                    // A ruin owed a spawn waits like a group to materialize.
                    || self.ruin_owed(g, loaded)
            })
            .count();
        let want =
            paused && (waiting > self.cfg.materialize_per_frame || (self.burst && waiting > 0));
        if want != self.burst {
            if !self.send(store, sim, EntityId(0), 0, Body::Burst(want)) {
                return false;
            }
            self.burst = want;
        }
        true
    }

    fn plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) {
        // The campaign is announced as soon as the agent is known, so the
        // server-scoped commands run with no mission loaded.
        if self.attached && self.epoch != 0 && !self.campaign_sent {
            let c = store.campaign().0;
            if !self.send(store, sim, EntityId(0), 0, Body::Campaign(c)) {
                return;
            }
            self.campaign_sent = true;
        }
        if !self.ready(store) {
            return;
        }
        if self.enum_wanted {
            if !self.send(store, sim, EntityId(0), 0, Body::Enumerate) {
                return;
            }
            self.enum_wanted = false;
            self.counters.enumerate_requests += 1;
        }
        if !self.extensions(store, sim) {
            return;
        }
        if let Some(r) = store.rates()
            && self.rates_sent != Some(r)
        {
            if !self.send(store, sim, EntityId(0), 0, Body::Rates(r)) {
                return;
            }
            self.rates_sent = Some(r);
        }
        if !self.adopt_plan(store, sim) || !self.subscribe_plan(store, sim) {
            return;
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
        // A hold stops spawning only: the rest of the plan goes on.
        if !self.hold && !self.restore_burst(store, sim, &loaded) {
            return;
        }
        // Outside the burst, unanswered spawns count against the budget:
        // spawns queued behind other commands in the ring would otherwise
        // reach DCS together, more than one frame's share.
        let mut spawns = if self.hold {
            0
        } else if self.burst {
            usize::MAX
        } else {
            self.cfg
                .materialize_per_frame
                .saturating_sub(self.spawns_inflight(&loaded))
        };
        let ids: Vec<EntityId> = store.groups().map(|g| g.id).collect();
        for id in ids {
            if !self.attached {
                return;
            }
            let g = store.group(id).expect("group");
            let here = g.theater == loaded.theater;
            let has_objects = bound.contains_key(&id);
            if g.kind == Kind::Static && g.state == State::Dead {
                // `dead` is final; its ruin is kept in DCS (`ruins.rs`).
                if self.ruin_plan(store, sim, id, &loaded, has_objects, spawns > 0) {
                    spawns -= 1;
                }
                continue;
            }
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
                    let rejected = self.spawn_rejected.get(&id)
                        == Some(&(g.spec_rev, loaded.mission_gen))
                        || self.is_held(self.cmd_not_ready.get(&(id, HOLD_SPAWN)), g.spec_rev);
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
                        // A live entity whose spec is ahead of its applied
                        // spawn is spawned again in place. Its order and
                        // options wait for that spawn: it replaces the group.
                        let applied = *self.spawn_applied.entry(id).or_insert(g.spec_rev);
                        let in_flight = self.inflight_for(
                            store,
                            id,
                            &[opcode::GROUP_SPAWN, opcode::STATIC_SPAWN],
                        );
                        let behind = applied < g.spec_rev;
                        let blocked = self.spawn_rejected.get(&id)
                            == Some(&(g.spec_rev, loaded.mission_gen))
                            || self.is_held(self.cmd_not_ready.get(&(id, HOLD_SPAWN)), g.spec_rev);
                        let mut sent = false;
                        if behind && !in_flight && !blocked && here && spawns > 0 {
                            sent = self.spawn(store, sim, id);
                            if sent {
                                spawns -= 1;
                            }
                        }
                        if !sent && !in_flight && !(behind && !blocked && here) {
                            self.order(store, sim, id, &bound);
                            self.options(store, sim, id);
                        }
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
        if !self.attached {
            return;
        }
        // After every spawn and order: the map only informs the API.
        if !self.countries_held
            && !self.inflight_for(store, EntityId(0), &[opcode::COUNTRIES])
            && !self.is_held(self.cmd_not_ready.get(&(EntityId(0), HOLD_COUNTRIES)), 0)
            && !self.send(store, sim, EntityId(0), 0, Body::Countries)
        {
            return;
        }
        if !self.pins_plan(store, sim) {
            return;
        }
        self.bases_plan(store, sim);
    }

    /// Write a destroy of the entity, unless one is unanswered, held, or
    /// given up. Returns whether one was written.
    fn destroy(&mut self, store: &mut Store, sim: &mut dyn SimAdapter, id: EntityId) -> bool {
        let ops = [opcode::GROUP_DESTROY, opcode::STATIC_DESTROY];
        let given_up = self.drift_destroy_failures.get(&id).copied().unwrap_or(0)
            >= self.cfg.destroy_retries.max(1);
        if given_up
            || self.inflight_for(store, id, &ops)
            || self.is_held(self.cmd_not_ready.get(&(id, HOLD_DESTROY)), 0)
        {
            return false;
        }
        let body = match store.group(id).expect("group").kind {
            Kind::Group => Body::GroupDestroy,
            Kind::Static => Body::StaticDestroy,
        };
        self.send(store, sim, id, 0, body)
    }

    fn dematerialize(&mut self, store: &mut Store, sim: &mut dyn SimAdapter, id: EntityId) {
        if self.is_held(self.cmd_not_ready.get(&(id, HOLD_CAPTURE)), 0) {
            return;
        }
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
        // A member that left DCS with no death fact is never spawned again:
        // it may be dead (spec/persistence.md, "Restore").
        let living: Vec<&crate::store::Unit> = living
            .into_iter()
            .filter(|u| !u.missing && !u.removed)
            .collect();
        if living.is_empty() {
            store
                .transition(id, Trigger::HeldOut)
                .expect("materializing");
            return false;
        }
        let mut owed = Vec::new();
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
                        if let Some(p) = u.spawn_life_percent()
                            && p < FULL_LIFE
                        {
                            owed.push((u.id, p, u.state.life));
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
                        // A live static never spawns destroyed, whatever
                        // its further keys say.
                        &ruins::without_dead(&u.extra),
                    ),
                    name,
                }
            }
        };
        let sent = self.send(store, sim, id, g.spec_rev, body);
        if sent {
            self.restore_lives(store, sim, owed);
        }
        sent
    }

    /// Queue a `DAMAGE` for each spawned unit owed a life, right after its
    /// spawn: ring order runs it after the spawn, which names the unit
    /// (spec/world.md, "Spawn state"). The life a sample would replace is
    /// held until DCS applies the `DAMAGE`.
    fn restore_lives(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        owed: Vec<(EntityId, f64, f32)>,
    ) {
        for (unit, p, life) in owed {
            let key = (self.epoch, self.next_seq);
            if self.effect_ready(store)
                && self.send(store, sim, unit, 0, Body::Damage { life_percent: p })
            {
                self.counters.lives_restored += 1;
                self.life_held
                    .insert(unit, ((life > 0.0).then_some(life), key));
            } else {
                self.life_lost(unit);
            }
        }
    }

    /// Whether a `PutGroup` life stands for the unit while its group's
    /// respawn is owed: what DCS reads of the old spawn does not replace it.
    fn life_asked(&self, store: &Store, unit: EntityId) -> bool {
        let Some(u) = store.unit(unit) else {
            return false;
        };
        u.captured.life_percent.is_some()
            && store.group(u.group).is_some_and(|g| {
                self.spawn_applied
                    .get(&g.id)
                    .is_some_and(|applied| *applied < g.spec_rev)
            })
    }

    /// A restored life DCS did not take: counted, and reported once.
    fn life_lost(&mut self, unit: EntityId) {
        self.counters.lives_lost += 1;
        eprintln!(
            "nanoprobe: the life restore of unit {unit} was not applied; it runs at the life DCS gave it"
        );
    }

    /// A `DAMAGE` that restored a life is answered: the life is no longer
    /// held. One that DCS refused, rejected or never took is counted. One
    /// rejected only for its moment is sent again with its spawn, and one
    /// whose result was lost may have been applied: neither is counted.
    /// Returns whether the key was a life restore.
    fn life_resolved(&mut self, key: (u64, u64), outcome: Outcome, code: Option<u16>) -> bool {
        let Some(unit) = self
            .life_held
            .iter()
            .find(|(_, (_, k))| *k == key)
            .map(|(u, _)| *u)
        else {
            return false;
        };
        self.life_held.remove(&unit);
        match outcome {
            Outcome::Applied => {}
            Outcome::Unknown => eprintln!(
                "nanoprobe: the life restore of unit {unit} may not have been applied; the next sample shows its life"
            ),
            Outcome::Rejected if code.is_some_and(rejected_for_now) => {}
            _ => self.life_lost(unit),
        }
        true
    }

    /// Send each controller option the group's current spawn has not
    /// applied, after its order. An option DCS refused at this value is not
    /// sent again until the value changes or the group is spawned again.
    fn options(&mut self, store: &mut Store, sim: &mut dyn SimAdapter, id: EntityId) {
        let g = store.group(id).expect("group");
        if g.kind != Kind::Group
            || g.options.is_empty()
            || self.inflight_for(store, id, &[opcode::GROUP_OPTION])
            || self.is_held(self.cmd_not_ready.get(&(id, HOLD_OPTION)), 0)
        {
            return;
        }
        let held = self.option_held.get(&id);
        let todo: Vec<(u32, Lv)> = g
            .options
            .iter()
            .filter(|(k, v)| {
                held.and_then(|h| h.get(*k)) != Some(*v)
                    && self.option_refused.get(&(id, **k)) != Some(*v)
            })
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (option, value) in todo {
            let key = (self.epoch, self.next_seq);
            let body = Body::GroupOption {
                option,
                value: value.clone(),
            };
            if !self.send(store, sim, id, 0, body) {
                return;
            }
            self.opt_sent.insert(key, (id, option, value));
        }
    }

    fn option_resolved(
        &mut self,
        c: &Journalled,
        gen_now: bool,
        outcome: Outcome,
        code: Option<u64>,
    ) {
        let Some((id, option, value)) = self.opt_sent.remove(&(c.epoch, c.seq)) else {
            return;
        };
        if !gen_now || c.agent_boot != self.agent_boot {
            return;
        }
        match outcome {
            Outcome::Applied => {
                self.option_held
                    .entry(id)
                    .or_default()
                    .insert(option, value);
            }
            Outcome::Refused => {
                self.option_refused.insert((id, option), value);
            }
            Outcome::Rejected if code == Some(rc::NOT_READY) => {
                self.hold_not_ready(id, HOLD_OPTION, 0);
            }
            _ => {}
        }
    }

    /// A base command of the kind (`owner`: `AIRBASE_OWNER`, else
    /// `WAREHOUSE_SET`) for the airbase is in flight. A sample then may show
    /// that command taking effect, so it is not a fact about the base.
    fn base_unresolved(&self, owner: bool, airbase: &str) -> bool {
        self.base_sent
            .values()
            .any(|(o, name, ..)| *o == owner && name == airbase)
    }

    /// An in-flight `WAREHOUSE_SET` or `WAREHOUSE_ADJUST` for the airbase
    /// names the item.
    fn item_unresolved(&self, airbase: &str, item: &Item) -> bool {
        self.base_sent
            .values()
            .any(|(o, name, _, diff)| !*o && name == airbase && diff.get(item).is_some())
            || self
                .adjust_sent
                .values()
                .any(|(name, delta)| name == airbase && delta.get(item).is_some())
    }

    /// `AIRBASE`: the owner of an airbase. An owner that changes from the
    /// one last seen is DCS's doing (a capture) or our own command taking
    /// effect. It replaces the desired owner only when the last sample
    /// matched the desired owner: the controller's value was then in effect
    /// and DCS moved away from it. A sample that changes while the desired
    /// owner is still unsent is not a fact about our value, so it is kept.
    fn airbase_sample(&mut self, store: &mut Store, a: &Airbase) {
        let theater = store
            .loaded()
            .map(|l| l.theater.clone())
            .unwrap_or_default();
        let unresolved = self.base_unresolved(true, &a.airbase);
        let seen = self.bases.entry(a.airbase.clone()).or_default();
        seen.object = a.object;
        let prev = seen.coalition.replace(a.coalition);
        let own = self.own_owner.remove(&a.airbase).unwrap_or_default();
        let desired = store
            .desired_owner(&theater, &a.airbase)
            .map(|d| d.coalition);
        if !unresolved
            && !own.contains(&a.coalition)
            && prev.is_some_and(|p| p != a.coalition)
            && prev == desired
        {
            store.observe_airbase_owner(&theater, &a.airbase, a.coalition);
        }
    }

    /// A spawned FARP's static is gone, and its airbase with it (spec/world.md,
    /// "Kinds"): what was seen and sent for it is forgotten. The desired
    /// owner and stock stay in the store by name, so a FARP spawned again
    /// under the name gets them sent again once its first samples come.
    fn base_gone(&mut self, object: u64) {
        if object == 0 {
            return;
        }
        let names: Vec<String> = self
            .bases
            .iter()
            .filter(|(_, b)| b.object == object)
            .map(|(n, _)| n.clone())
            .collect();
        for name in names {
            self.bases.remove(&name);
            self.owner_sent.remove(&name);
            self.stock_sent.remove(&name);
            self.own_owner.remove(&name);
            self.own_stock.remove(&name);
        }
    }

    /// `WAREHOUSE`: the stock of an airbase. A count that changes from the
    /// one last seen replaces the desired count of an item the controller
    /// set, only when the last sample matched that desired count: DCS used
    /// it, or an adjustment changed it, after our value took effect. A
    /// change while the desired count is still unsent is kept.
    fn warehouse_sample(&mut self, store: &mut Store, w: &Warehouse) {
        let theater = store
            .loaded()
            .map(|l| l.theater.clone())
            .unwrap_or_default();
        let stock = Stock::from_lv(&w.stock);
        let seen = self.bases.entry(w.airbase.clone()).or_default();
        let prev = seen.stock.replace(stock.clone());
        let own = self.own_stock.remove(&w.airbase).unwrap_or_default();
        if let Some(prev) = prev {
            for (item, count) in stock.items() {
                if self.item_unresolved(&w.airbase, &item) {
                    continue;
                }
                let desired = store
                    .desired_stock(&theater, &w.airbase)
                    .and_then(|d| d.stock.get(&item));
                let ours = own.iter().any(|o| o.get(&item) == Some(count));
                if !ours && prev.get(&item) != Some(count) && prev.get(&item) == desired {
                    store.observe_stock_item(&theater, &w.airbase, &item, count);
                }
            }
        }
    }

    /// Send an owner or a stock the latest sample does not show, once per
    /// desired revision and mission generation.
    fn bases_plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) {
        if self.is_held(self.cmd_not_ready.get(&(EntityId(0), HOLD_BASES)), 0) {
            return;
        }
        let theater = store.loaded().expect("ready").theater.clone();
        let owners: Vec<(String, u32, u64)> = store
            .desired_owners()
            .filter(|((t, _), _)| *t == theater)
            .map(|((_, a), d)| (a.clone(), d.coalition, d.rev))
            .collect();
        for (airbase, coalition, rev) in owners {
            let differs = self
                .bases
                .get(&airbase)
                .and_then(|b| b.coalition)
                .is_some_and(|c| c != coalition);
            if !differs || self.owner_sent.get(&airbase) == Some(&rev) {
                continue;
            }
            let key = (self.epoch, self.next_seq);
            let body = Body::AirbaseOwner {
                coalition,
                airbase: airbase.clone(),
            };
            if !self.send(store, sim, EntityId(0), 0, body) {
                return;
            }
            self.owner_sent.insert(airbase.clone(), rev);
            self.base_sent
                .insert(key, (true, airbase, coalition, Stock::default()));
        }
        let stocks: Vec<(String, Stock, u64)> = store
            .desired_stocks()
            .filter(|((t, _), _)| *t == theater)
            .map(|((_, a), d)| (a.clone(), d.stock.clone(), d.rev))
            .collect();
        for (airbase, desired, rev) in stocks {
            let Some(seen) = self.bases.get(&airbase).and_then(|b| b.stock.as_ref()) else {
                continue;
            };
            let mut diff = Stock::default();
            for (item, count) in desired.items() {
                if seen.get(&item) != Some(count) {
                    diff.set(item, count);
                }
            }
            if diff.is_empty() || self.stock_sent.get(&airbase) == Some(&rev) {
                continue;
            }
            let key = (self.epoch, self.next_seq);
            let body = Body::WarehouseSet {
                airbase: airbase.clone(),
                stock: diff.to_lv(),
            };
            if !self.send(store, sim, EntityId(0), 0, body) {
                return;
            }
            self.stock_sent.insert(airbase.clone(), rev);
            self.base_sent.insert(key, (false, airbase, 0, diff));
        }
    }

    /// An `AIRBASE_OWNER` or `WAREHOUSE_SET` resolved. Applied or refused,
    /// it is not sent again at this revision; a command the agent did not
    /// take, or whose answer was lost, is derived again.
    fn base_resolved(&mut self, c: &Journalled, outcome: Outcome, res: Option<&CommandResult>) {
        let Some((owner, name, coalition, stock)) = self.base_sent.remove(&(c.epoch, c.seq)) else {
            return;
        };
        if outcome == Outcome::Applied {
            if owner {
                self.own_owner
                    .entry(name.clone())
                    .or_default()
                    .push(coalition);
            } else {
                self.own_stock.entry(name.clone()).or_default().push(stock);
            }
        }
        let code = res.map(|r| r.code);
        let again = match outcome {
            Outcome::Unknown | Outcome::NotSent => true,
            Outcome::Rejected => code.is_some_and(rejected_for_now),
            _ => false,
        };
        if again {
            if owner {
                self.owner_sent.remove(&name);
            } else {
                self.stock_sent.remove(&name);
            }
        }
        if outcome == Outcome::Rejected && code.map(u64::from) == Some(rc::NOT_READY) {
            self.hold_not_ready(EntityId(0), HOLD_BASES, 0);
        }
    }

    /// A `COUNTRIES` resolved: an applied answer gives the map. Any other
    /// outcome leaves the reconciler without one, and the query is sent
    /// again after a hold.
    fn countries_resolved(
        &mut self,
        store: &mut Store,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        let gen_now = c.agent_boot == self.agent_boot
            && store
                .loaded()
                .is_some_and(|l| l.agent_boot == c.agent_boot && l.mission_gen == c.mission_gen);
        if !gen_now {
            return;
        }
        match res {
            Some(r) if outcome == Outcome::Applied && u64::from(r.code) == rc::APPLIED => {
                let mut map = BTreeMap::new();
                if let Lv::Table(pairs) = &r.value {
                    for (k, v) in pairs {
                        if let (Lv::Number(k), Lv::Number(v)) = (k, v)
                            && *k >= 0.0
                            && *v >= 0.0
                        {
                            map.insert(*k as u32, *v as u32);
                        }
                    }
                }
                store.set_countries(Some(map));
                self.countries_held = true;
            }
            _ => self.hold_not_ready(EntityId(0), HOLD_COUNTRIES, 0),
        }
    }

    fn effect_ready(&self, store: &Store) -> bool {
        self.ready(store) && self.attached && self.campaign_sent
    }

    /// Whether a server-scoped command can be sent: the agent is attached and
    /// the campaign announced. No mission needs to run.
    pub(crate) fn server_ready(&self) -> bool {
        self.attached && self.epoch != 0 && self.campaign_sent
    }

    /// Hold materializing, or let it go on: while an activation's mission
    /// load is owed, and until its generation runs, no group of the campaign
    /// materializes.
    pub fn hold_world(&mut self, on: bool) {
        self.hold = on;
    }

    /// Add to or take from a warehouse's stock: a `WAREHOUSE_ADJUST`, an
    /// effect, never sent again. Returns the command's (epoch, seq), whose
    /// result [`Reconciler::effect_result`] gives, or None when nothing can
    /// be sent now.
    pub fn adjust_warehouse(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        airbase: &str,
        delta: &Stock,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let body = Body::WarehouseAdjust {
            airbase: airbase.to_owned(),
            delta: delta.to_lv(),
        };
        let sent = self.send(store, sim, EntityId(0), 0, body);
        if sent {
            self.adjust_sent
                .insert(key, (airbase.to_owned(), delta.clone()));
        }
        sent.then_some(key)
    }

    /// Set a bound unit's life, as a percentage of its initial life: a
    /// `DAMAGE`, an effect. None when nothing can be sent now or DCS holds
    /// no object of the unit.
    pub fn damage(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        unit: EntityId,
        life_percent: f64,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) || !self.is_bound(unit) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, unit, 0, Body::Damage { life_percent })
            .then_some(key)
    }

    /// Play a sound file: a `SOUND`, an effect, never sent again. A `GROUP`
    /// or `UNIT` scope needs DCS to hold an object of the target (None
    /// otherwise), as does any call when nothing can be sent now.
    pub fn sound(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        scope: u32,
        target: u64,
        file: &str,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) || !self.scope_bound(scope, target) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let body = Body::Sound {
            scope,
            target,
            file: file.to_owned(),
        };
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Show a picture: a `PICTURE`, an effect, never sent again. None as for
    /// [`Reconciler::sound`].
    pub fn picture(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        body: Body,
    ) -> Option<(u64, u64)> {
        let Body::Picture { scope, target, .. } = &body else {
            return None;
        };
        if !self.effect_ready(store) || !self.scope_bound(*scope, *target) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Take every picture off screen: a `PICTURE_STOP`, an effect.
    pub fn picture_stop(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, Body::PictureStop)
            .then_some(key)
    }

    /// Whether a scope's target is something DCS can be asked about: any
    /// scope but `GROUP` and `UNIT`, or an entity DCS holds an object of.
    fn scope_bound(&self, scope: u32, target: u64) -> bool {
        !matches!(scope, 3 | 4) || self.is_bound(EntityId(target))
    }

    /// Read a live group's or static's state from DCS: a `CAPTURE`, a query.
    /// The store takes what DCS reads when the result arrives. None when
    /// nothing can be sent now.
    pub fn capture(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        entity: EntityId,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, entity, 0, Body::Capture)
            .then_some(key)
    }

    /// Whether DCS holds an object of the unit, group or static.
    pub fn is_bound(&self, entity: EntityId) -> bool {
        self.binding
            .values()
            .any(|r| r.unit == entity || r.group == entity)
            || self.is_adopted(entity)
    }

    /// How an effect or a query this reconciler sent ended, by the
    /// (epoch, seq) the call returned; None while it is unanswered.
    pub fn effect_result(&self, key: (u64, u64)) -> Option<&EffectResult> {
        self.effects.get(&key)
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
        let binding = &self.binding;
        let object_of = |e: EntityId| binding.iter().find(|(_, r)| r.unit == e).map(|(o, _)| *o);
        let target_now = |t: EntityId| match store.group(t) {
            Some(x) if x.kind == Kind::Group => bound
                .get(&t)
                .copied()
                .filter(|v| *v != 0)
                .map(Target::Group),
            Some(_) => object_of(t).map(Target::Static),
            None => object_of(t).map(Target::Unit),
        };
        let rev = g.order_rev;
        let held = self.order_held.get(&id).copied().unwrap_or(0);
        if held >= rev
            || self.order_refused.get(&id) == Some(&rev)
            || self.is_held(self.cmd_not_ready.get(&(id, HOLD_ORDER)), rev)
        {
            return;
        }
        let lead = g
            .members
            .iter()
            .map(|m| store.unit(*m).expect("unit"))
            .find(|u| !u.dead && !store.held(u.id));
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
/// A life at or above this percentage is spawned as is: no `DAMAGE` follows.
const FULL_LIFE: f64 = 99.95;

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
                .filter(|p| ids::of_campaign(p, campaign))
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
            (rc::NOT_READY, Outcome::Rejected),
        ] {
            assert_eq!(classify(code as u16, set_state), want, "code {code}");
            assert_eq!(classify(code as u16, query), want, "code {code}");
        }
        assert_eq!(classify(rc::TOO_LARGE as u16, set_state), Outcome::Applied);
        assert_eq!(classify(rc::TOO_LARGE as u16, query), Outcome::Refused);
        assert_eq!(
            nanoprobe_transport::layout::result::ALL.len(),
            15,
            "a code without a row here"
        );
    }

    #[test]
    fn further_keys_join_a_table_and_never_replace_the_platform_s() {
        let base = lv::obj(vec![("name", lv::s("np.blp2")), ("type", lv::s("M-113"))]);
        let extra = lv::obj(vec![
            ("name", lv::s("mine")),
            ("livery_id", lv::s("desert")),
        ]);
        let t = with_extra(base.clone(), &extra);
        assert_eq!(lv::str_at(&t, "name").as_deref(), Some("np.blp2"));
        assert_eq!(lv::str_at(&t, "livery_id").as_deref(), Some("desert"));
        assert_eq!(with_extra(base.clone(), &Lv::Nil), base);
        assert_eq!(with_extra(base.clone(), &Lv::Number(1.0)), base);
    }
}
