//! The world store (plan 4.4): typed tables keyed by `EntityId`, one revision
//! counter that stamps every change, the journal, and the spatial grid.
//!
//! This is the durable half of the sidecar. Whatever is here survives a
//! sidecar restart; the reconciler's own state does not. Step S18 puts the
//! tables and the journal in a per-campaign SQLite file; here they are in
//! memory, and a test keeps the [`Store`] across a simulated restart.

use std::collections::{BTreeMap, VecDeque};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::records::Rates;

use crate::grid::Grid;
use crate::ids::{CampaignId, EntityId, MAX_ID};
use crate::intents::{self, Intent};
use crate::lifecycle::{self, State, Trigger};
use crate::stock::{Item, Stock};

#[cfg(feature = "disk")]
mod disk;

#[cfg(feature = "disk")]
pub use disk::{
    CampaignRecord, CampaignState, Checkpoint, DiskError, MIGRATIONS, Migration, REVISION_RESERVE,
    SCHEMA, TierPeriod, backup_file, checkpoint_of, idempotency_of, ident_of, journal_of,
    record_of, revision_of, schema_of, state_of,
};

/// Without the `disk` feature a store lives in memory only: its slot is
/// always empty, and nothing commits.
#[cfg(not(feature = "disk"))]
mod disk {
    /// The store's link to a file it never has.
    #[derive(Clone, Debug, Default)]
    pub struct Slot(pub(super) Option<std::convert::Infallible>);

    impl super::Store {
        /// Nothing to write: the store is in memory.
        pub fn commit(&mut self) -> Result<(), std::convert::Infallible> {
            Ok(())
        }
    }
}

/// Whether `v` encodes whole: no table nested deeper than the codec's
/// [`MAX_DEPTH`](nanoprobe_transport::codec::MAX_DEPTH) and none with more
/// pairs than [`MAX_PAIRS`](nanoprobe_transport::codec::MAX_PAIRS). The
/// transport writer writes nil past either; the store refuses instead
/// (spec/persistence.md, "Encodings").
pub fn check_lv(v: &Lv) -> Result<(), String> {
    fn at(v: &Lv, depth: usize) -> Result<(), String> {
        let Lv::Table(pairs) = v else {
            return Ok(());
        };
        if depth >= nanoprobe_transport::codec::MAX_DEPTH {
            return Err(format!(
                "a table nested deeper than {} levels",
                nanoprobe_transport::codec::MAX_DEPTH
            ));
        }
        if pairs.len() > nanoprobe_transport::codec::MAX_PAIRS as usize {
            return Err(format!(
                "a table of more than {} pairs",
                nanoprobe_transport::codec::MAX_PAIRS
            ));
        }
        for (k, v) in pairs {
            at(k, depth + 1)?;
            at(v, depth + 1)?;
        }
        Ok(())
    }
    at(v, 0)
}

mod markers;
pub use markers::{Marker, MarkerError, MarkerKind, MarkerSpec, Markers, scope};

/// Who simulates a managed entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Authority {
    /// The campaign: the entity is virtual.
    Campaign,
    /// DCS, while the entity's theater is loaded.
    Sim,
}

/// What happens to a `lost` group (spec/config.md, `on_lost`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LostPolicy {
    /// Report and wait for the next mission generation or enumeration.
    Report,
    /// Spawn it again.
    Respawn,
    /// Mark it dead.
    Dead,
}

/// A group or a static.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A group of units.
    Group,
    /// A static, which is its own single member.
    Static,
}

/// A point in DCS map coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// North, meters.
    pub x: f64,
    /// Altitude, meters.
    pub y: f64,
    /// East, meters.
    pub z: f64,
}

/// What the store knows of a unit's state: from the spec, then from samples
/// and captures.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UnitState {
    /// Position.
    pub at: Point,
    /// Heading, radians.
    pub heading: f64,
    /// `getLife()`, or 0 when never sampled.
    pub life: f32,
}

/// The keys of a capture that `UnitState` does not hold
/// (spec/commands.md, `GROUP_CAPTURE_DESTROY`). A read that raised leaves its
/// key out, which here is 0 or nil.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedMore {
    /// `getLife0()`, or 0 when not read.
    pub life0: f64,
    /// Pitch, radians.
    pub pitch: f64,
    /// Roll, radians.
    pub roll: f64,
    /// The life a `PutGroup` asked the unit to spawn with, as a percentage
    /// of its initial life, until DCS reads the unit's life.
    pub life_percent: Option<f64>,
}

impl Default for CapturedMore {
    fn default() -> CapturedMore {
        CapturedMore {
            life0: 0.0,
            pitch: 0.0,
            roll: 0.0,
            life_percent: None,
        }
    }
}

/// A unit, or the one member of a static.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// The unit.
    pub id: EntityId,
    /// Its group, or itself for a static.
    pub group: EntityId,
    /// DCS type name.
    pub type_name: String,
    /// The last known state.
    pub state: UnitState,
    /// A death fact names it. Final.
    pub dead: bool,
    /// Missing from DCS without a death fact, in this mission generation:
    /// it left as `MISSING` or `REMOVED` and has not been found since.
    pub missing: bool,
    /// It left as `REMOVED` (DCS no longer has it and the read shows a
    /// removal, not a kill). Implies `missing`.
    pub removed: bool,
    /// A `Left` fact is journalled for the present `missing` mark. Not set
    /// by `set_missing`, which journals nothing.
    left_logged: bool,
    /// The DCS object ID it was last bound to, 0 for none known. A member
    /// keeps the ID it left with, so a death event for an object from before
    /// a respawn is told apart from one for the respawned member.
    pub object: u64,
    /// The author's label. Never sent to DCS, where names hold only numbers
    /// (spec/world.md, "Names").
    pub label: String,
    /// DCS skill, empty for the default. Spec.
    pub skill: String,
    /// What the last capture read besides the state: initial life and attitude.
    /// Absent keys stay 0 and nil.
    pub captured: CapturedMore,
    /// Further keys of the DCS unit or static table: payload, livery,
    /// callsign and so on (spec/world.md, "Lua tables in the API"). Spec.
    pub extra: Lv,
}

impl Unit {
    /// Whether DCS has read the unit's life: the store keeps 0 until the
    /// first read.
    pub fn vitals_read(&self) -> bool {
        self.state.life != 0.0
    }

    /// The life to spawn the unit with, as a percentage of its initial life
    /// (spec/world.md, "Spawn state"): what DCS last read, else what a
    /// `PutGroup` asked for. None when neither is known.
    pub fn spawn_life_percent(&self) -> Option<f64> {
        if self.dead {
            return None;
        }
        if self.vitals_read() && self.captured.life0 > 0.0 && self.state.life > 0.0 {
            return Some(
                (100.0 * f64::from(self.state.life) / self.captured.life0).clamp(0.0, 100.0),
            );
        }
        self.captured.life_percent
    }
}

/// A managed group or static.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The entity.
    pub id: EntityId,
    /// Group or static.
    pub kind: Kind,
    /// The theater it is on.
    pub theater: String,
    /// DCS country number.
    pub country: u32,
    /// DCS group category number; 0 for a static.
    pub category: u32,
    /// Its units in spec order; for a static, itself.
    pub members: Vec<EntityId>,
    /// The author's label. Never sent to DCS.
    pub label: String,
    /// DCS coalition number. Spec.
    pub coalition: u32,
    /// Further keys of the DCS group table; nil for a static, whose table is
    /// its member's. Spec.
    pub extra: Lv,
    /// Author components by registered name: opaque bytes the sidecar stores
    /// and serves. Never sent to DCS.
    pub components: BTreeMap<String, Vec<u8>>,
    /// Revision of the spec.
    pub spec_rev: u64,
    /// The coalition its `OBJECT` records last gave, DCS numbering: 0 neutral,
    /// 1 red, 2 blue. `None` until an object of it is seen.
    pub observed_coalition: Option<u32>,
    /// The controller removed it: it is under campaign authority, so DCS
    /// holds none of it, and the API no longer lists it. Its ID is never
    /// reused.
    pub removed: bool,
    /// Controller options by DCS option ID. Not spec: a change moves only
    /// the store revision (spec/world.md, "Revisions").
    pub options: BTreeMap<u32, Lv>,
    /// The order, if any.
    pub order: Option<Intent>,
    /// Revision of the order, 0 for none.
    pub order_rev: u64,
    /// Progress of the order (intents::progress).
    pub progress: usize,
    /// The last sample of the lead unit made under this order: the unit, its
    /// `x` and `z`, and the model time. Makes the segment to the next sample.
    pub last_lead: Option<(EntityId, f64, f64, f64)>,
    /// Lifecycle state.
    pub state: State,
    /// Who simulates it.
    pub authority: Authority,
    /// The author's override of the lost policy.
    pub on_lost: Option<LostPolicy>,
    /// Why DCS refused its spawn, when `unspawnable`.
    pub error: Option<String>,
    /// While `dematerializing`: the capture is settled and only the destroy
    /// remains.
    pub captured: bool,
    /// The agent boot and mission generation of its last applied spawn.
    pub spawned_in: Option<(u64, u32)>,
    /// Destroys refused or unknown while `dematerializing`.
    pub destroy_failures: u32,
    /// The agent boot and generation in which a spawn of it resolved
    /// `unknown`: DCS may hold it, so it is not withdrawn to `virtual`.
    pub unknown_spawn_in: Option<(u64, u32)>,
    /// The agent boot, generation and order revision of the last order
    /// written for it.
    pub order_sent_in: Option<(u64, u32, u64)>,
}

/// The mission DCS has loaded, as last seen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    /// The agent's boot.
    pub agent_boot: u64,
    /// Mission generation.
    pub mission_gen: u32,
    /// The theater, empty when none is loaded.
    pub theater: String,
}

/// An activation's mission load, owed until the agent answers it
/// (spec/persistence.md, "Campaign operations").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwedLoad {
    /// The mission file as DCS opens it.
    pub path: String,
    /// The `MISSION_LOAD`'s epoch and sequence number once sent.
    pub cmd: Option<(u64, u64)>,
}

/// How a unit left DCS without a death fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LeftAs {
    /// DCS no longer has it and the cause is unknown: a death may follow.
    Missing,
    /// DCS no longer has it and the read shows a removal, not a kill.
    Removed,
}

/// A command's outcome (spec/errors.md, "Command outcomes").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// DCS is in the commanded state.
    Applied,
    /// The command reached DCS and failed.
    Refused,
    /// The agent did not try it.
    Rejected,
    /// The agent took it and its result was lost.
    Unknown,
    /// The agent never took it.
    NotSent,
}

/// One journal entry. Every entry is written before what it describes is
/// acted on.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// An entity was created or its spec changed.
    Spec {
        /// The entity.
        entity: EntityId,
        /// The new revision.
        rev: u64,
    },
    /// An extension was registered or its source changed.
    ExtensionSpec {
        /// The extension's name.
        name: String,
        /// The new revision.
        rev: u64,
    },
    /// The controller removed an extension.
    ExtensionRemoved {
        /// The extension's name.
        name: String,
    },
    /// The controller removed a group, static or marker.
    Removed {
        /// The entity.
        entity: EntityId,
    },
    /// A controller option of a group was set.
    Option {
        /// The group.
        entity: EntityId,
        /// The DCS option ID.
        option: u32,
    },
    /// The desired owner of an airbase was set by the controller, or
    /// replaced by a capture DCS made.
    AirbaseOwner {
        /// The theater.
        theater: String,
        /// The airbase's DCS name.
        airbase: String,
        /// The desired owner.
        coalition: u32,
        /// The desired state's revision: moves when the controller sets it.
        rev: u64,
    },
    /// The desired counts of a warehouse changed: the controller set items,
    /// or DCS changed an item the controller had set.
    Warehouse {
        /// The theater.
        theater: String,
        /// The airbase's DCS name.
        airbase: String,
        /// The desired state's revision: moves when the controller sets it.
        rev: u64,
    },
    /// The desired sample rates were set.
    SampleRates(Rates),
    /// An order was set.
    Order {
        /// The group.
        entity: EntityId,
        /// The new revision.
        rev: u64,
    },
    /// The authority changed.
    Authority {
        /// The entity.
        entity: EntityId,
        /// The new authority.
        authority: Authority,
    },
    /// A lifecycle transition.
    Transition {
        /// The entity.
        entity: EntityId,
        /// From.
        from: State,
        /// To.
        to: State,
        /// A new spec revision taken with the transition: a respawn of a
        /// `lost` group, journalled as one entry with it.
        spec_rev: Option<u64>,
    },
    /// A dematerializing entity's capture is settled: its captured state, or
    /// its last checkpointed one, is the state to keep. What remains is the
    /// destroy.
    Captured {
        /// The entity.
        entity: EntityId,
    },
    /// A death fact.
    Death {
        /// The unit.
        unit: EntityId,
    },
    /// A unit left DCS as `MISSING` or `REMOVED`: it holds its group's
    /// respawn gate and stays out of spawns, across mission generations too,
    /// until a death fact or the group's removal releases it.
    Left {
        /// The unit.
        unit: EntityId,
        /// The DCS object ID it left with.
        object: u64,
        /// How it left.
        how: LeftAs,
    },
    /// A death fact for an object from before a respawn: it names a member
    /// that is bound to another object now, so it marks nothing.
    StaleDeath {
        /// The unit.
        unit: EntityId,
        /// The DCS object ID the record named.
        object: u64,
    },
    /// A death fact corrected a `REMOVED`: the agent's classifier called a
    /// kill a removal.
    ClassifierFault {
        /// The unit.
        unit: EntityId,
        /// The DCS object ID it left with.
        object: u64,
    },
    /// An `OBJECT` for the object ID of a unit with a death fact showed it
    /// alive again. The death stays a fact in the journal.
    Revived {
        /// The unit.
        unit: EntityId,
        /// The DCS object ID.
        object: u64,
    },
    /// A loaded mission was seen.
    Loaded(Loaded),
    /// A new command epoch.
    Epoch(u64),
    /// A sequenced command, journalled before it is written.
    Command(Journalled),
    /// A command's outcome.
    Outcome {
        /// The command's epoch.
        epoch: u64,
        /// The command's sequence number.
        seq: u64,
        /// The outcome.
        outcome: Outcome,
    },
}

/// A sequenced command as journalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Journalled {
    /// The agent boot it was written for.
    pub agent_boot: u64,
    /// Epoch.
    pub epoch: u64,
    /// Sequence number.
    pub seq: u64,
    /// Opcode.
    pub opcode: u16,
    /// The entity.
    pub entity: EntityId,
    /// The revision it carries.
    pub rev: u64,
    /// The mission generation it was written for.
    pub mission_gen: u32,
    /// The inbound position just after it, 0 until it is written.
    pub position: u64,
    /// For a spawn, the units it spawns.
    pub units: Vec<EntityId>,
}

/// A new unit's spec.
#[derive(Clone, Debug, PartialEq)]
pub struct NewUnit {
    /// DCS type name.
    pub type_name: String,
    /// Starting state.
    pub state: UnitState,
    /// The author's label.
    pub label: String,
    /// DCS skill, empty for the default.
    pub skill: String,
    /// Further keys of the DCS unit table, or nil.
    pub extra: Lv,
}

impl NewUnit {
    /// A unit of a type at a state, with no label, skill or extra keys.
    pub fn new(type_name: &str, state: UnitState) -> NewUnit {
        NewUnit {
            type_name: type_name.to_owned(),
            state,
            label: String::new(),
            skill: String::new(),
            extra: Lv::Nil,
        }
    }
}

/// A new group's spec.
#[derive(Clone, Debug, PartialEq)]
pub struct NewGroup {
    /// The theater.
    pub theater: String,
    /// DCS country number.
    pub country: u32,
    /// DCS group category number.
    pub category: u32,
    /// DCS coalition number.
    pub coalition: u32,
    /// Its units; a static has exactly one.
    pub units: Vec<NewUnit>,
    /// Who simulates it.
    pub authority: Authority,
    /// The author's label.
    pub label: String,
    /// Further keys of the DCS group table, or nil.
    pub extra: Lv,
}

/// The store's tables.
#[derive(Clone, Debug)]
pub struct Store {
    campaign: CampaignId,
    next_entity: u64,
    rev: u64,
    epoch: u64,
    loaded: Option<Loaded>,
    groups: BTreeMap<EntityId, Group>,
    units: BTreeMap<EntityId, Unit>,
    /// Journal entries not yet written, or, for a store in memory or one
    /// that keeps its journal, every entry.
    journal: Vec<Entry>,
    /// Commands journalled with no outcome yet.
    inflight: BTreeMap<(u64, u64), Journalled>,
    /// The last agent record folded in: agent boot and record number, and
    /// the stamp it was read under (agent boot, generation, campaign).
    last_record: Option<(u64, u64)>,
    last_stamp: Option<(u64, u32, u64)>,
    /// Keep written journal entries in memory, for tests that read them.
    keep_journal: bool,
    /// Idempotency rows to write with the next commit.
    idempotency: Vec<IdemRow>,
    changes: VecDeque<(u64, EntityId)>,
    changes_keep: usize,
    grid: Grid,
    extensions: BTreeMap<String, Extension>,
    markers: Markers,
    owners: BTreeMap<(String, String), DesiredOwner>,
    stocks: BTreeMap<(String, String), DesiredStock>,
    rates: Option<Rates>,
    owed_load: Option<OwedLoad>,
    countries: Option<BTreeMap<u32, u32>>,
    sample_time: f64,
    route_events: Vec<RouteEvent>,
    disk: disk::Slot,
}

/// A route event: a waypoint of a group's order reached, or the route done.
/// The store queues them and the API puts them in the campaign's event log,
/// so each is written once: progress is kept in the store, never goes back,
/// and an event is made only when it advances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteEvent {
    /// The group.
    pub group: EntityId,
    /// The order revision.
    pub order_rev: u64,
    /// The waypoint's index in the order, from 0; `None` for the route being
    /// complete.
    pub index: Option<usize>,
    /// Model time of the sample that showed it.
    pub model_time: f64,
    /// Whether the position was observed. Always true: only samples move
    /// progress, and the controller cannot write a position that does.
    pub observed: bool,
}

/// The owner the controller wants an airbase to have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredOwner {
    /// DCS coalition number.
    pub coalition: u32,
    /// One more at each controller set. A DCS capture that replaces the
    /// owner leaves it, so the reconciler sends a set once per revision and
    /// mission generation.
    pub rev: u64,
}

/// The counts the controller wants in a warehouse: only items it set.
#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStock {
    /// The counts.
    pub stock: Stock,
    /// One more at each controller set.
    pub rev: u64,
}

/// A registered Lua extension the campaign wants in DCS (spec/world.md,
/// spec/commands.md "Extensions"). Its identity is its name.
#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    /// The name.
    pub name: String,
    /// The spec revision: 1 when set first, one more on each change.
    pub spec_rev: u64,
    /// The Lua source.
    pub source: String,
}

/// A change the store refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// No such entity.
    NoSuchEntity(EntityId),
    /// The trigger does not apply in the entity's state.
    NoTransition(EntityId, State, Trigger),
    /// No more `EntityId`s below 2^53.
    Exhausted,
    /// An attack order whose target is neither a group, a static nor a unit
    /// of this campaign. The API answers `INVALID_FIELD`.
    UnsupportedTarget(EntityId),
    /// A Lua value the store cannot encode whole (spec/persistence.md,
    /// "Encodings"). The API answers `INVALID_FIELD`.
    TooLarge(String),
}

/// A changing call's answer, kept with its idempotency key in the store of
/// the campaign it acted on (spec/persistence.md, "Idempotency keys").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdemRow {
    /// The key.
    pub key: String,
    /// When the call was answered, milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The digest of the method and request.
    pub request_hash: Vec<u8>,
    /// The encoded response.
    pub response: Vec<u8>,
}

/// The default grid cell, in meters.
pub const GRID_CELL_M: f64 = 2_000.0;

impl Store {
    /// An empty store for a campaign. `changes_keep` is how many changes a
    /// watch may resume from (spec/config.md, `world_changes_keep`).
    pub fn new(campaign: CampaignId, changes_keep: usize) -> Store {
        Store {
            campaign,
            next_entity: 1,
            rev: 0,
            epoch: 0,
            loaded: None,
            groups: BTreeMap::new(),
            units: BTreeMap::new(),
            journal: Vec::new(),
            inflight: BTreeMap::new(),
            last_record: None,
            last_stamp: None,
            keep_journal: true,
            idempotency: Vec::new(),
            changes: VecDeque::new(),
            changes_keep,
            grid: Grid::new(GRID_CELL_M),
            extensions: BTreeMap::new(),
            markers: Markers::default(),
            owners: BTreeMap::new(),
            stocks: BTreeMap::new(),
            rates: None,
            owed_load: None,
            countries: None,
            sample_time: 0.0,
            route_events: Vec::new(),
            disk: disk::Slot::default(),
        }
    }

    /// The coalition each DCS country has in the loaded mission, once the
    /// agent answered `COUNTRIES`: country number to DCS coalition number.
    pub fn countries(&self) -> Option<&BTreeMap<u32, u32>> {
        self.countries.as_ref()
    }

    /// The DCS coalition of a country, if the map holds it.
    pub fn country_coalition(&self, country: u32) -> Option<u32> {
        self.countries.as_ref()?.get(&country).copied()
    }

    /// Keep the agent's country map, or forget it with `None`. Observed, not
    /// desired state: it adds no journal entry. A group that is not in DCS
    /// takes its coalition from this map, so a changed map touches every
    /// visible group: the store revision moves and a world watch sends the
    /// new coalition.
    pub fn set_countries(&mut self, map: Option<BTreeMap<u32, u32>>) {
        if self.countries == map {
            return;
        }
        self.countries = map;
        let ids: Vec<EntityId> = self
            .groups
            .values()
            .filter(|g| !g.removed)
            .map(|g| g.id)
            .collect();
        for id in ids {
            self.touch(id);
        }
    }

    /// An `OBJECT` of the group or static gave its coalition. Returns whether
    /// it changed.
    pub fn observe_coalition(&mut self, id: EntityId, coalition: u32) -> bool {
        match self.groups.get_mut(&id) {
            Some(g) if g.observed_coalition != Some(coalition) => {
                g.observed_coalition = Some(coalition);
                self.touch(id);
                true
            }
            _ => false,
        }
    }

    /// Set one controller option of a group or static. A value the group
    /// already has changes nothing. Not spec: only the store revision moves.
    pub fn set_option(&mut self, id: EntityId, option: u32, value: Lv) -> Result<(), StoreError> {
        check_lv(&value).map_err(StoreError::TooLarge)?;
        let g = self.group_mut(id)?;
        if g.options.get(&option) == Some(&value) {
            return Ok(());
        }
        g.options.insert(option, value);
        self.journal(Entry::Option { entity: id, option });
        self.touch(id);
        Ok(())
    }

    /// Remove a group or static: it goes to campaign authority, so the
    /// reconciler takes it out of DCS, and it is hidden from the API. A
    /// removed entity stays in the store, so its ID is never reused and its
    /// objects in DCS are still destroyed as drift.
    pub fn remove_entity(&mut self, id: EntityId) -> Result<(), StoreError> {
        if self.group_mut(id)?.removed {
            return Ok(());
        }
        self.set_authority(id, Authority::Campaign)?;
        self.group_mut(id)?.removed = true;
        // The controller's removal releases every member's mark: it is the
        // one way out of a mark kept across a generation (spec/world.md,
        // "Respawn gate").
        let members = self.groups[&id].members.clone();
        for m in members {
            if let Some(u) = self.units.get_mut(&m) {
                u.missing = false;
                u.removed = false;
                u.left_logged = false;
            }
        }
        self.journal(Entry::Removed { entity: id });
        self.touch(id);
        Ok(())
    }

    /// The owner the controller wants for an airbase, if it set one.
    pub fn desired_owner(&self, theater: &str, airbase: &str) -> Option<&DesiredOwner> {
        self.owners.get(&(theater.to_owned(), airbase.to_owned()))
    }

    /// Every desired owner, by theater and airbase name.
    pub fn desired_owners(&self) -> impl Iterator<Item = (&(String, String), &DesiredOwner)> {
        self.owners.iter()
    }

    /// The controller wants an airbase to belong to a coalition. Returns the
    /// desired state's revision.
    pub fn set_airbase_owner(&mut self, theater: &str, airbase: &str, coalition: u32) -> u64 {
        let key = (theater.to_owned(), airbase.to_owned());
        let rev = self.owners.get(&key).map_or(1, |o| o.rev + 1);
        self.owners.insert(key, DesiredOwner { coalition, rev });
        self.journal(Entry::AirbaseOwner {
            theater: theater.to_owned(),
            airbase: airbase.to_owned(),
            coalition,
            rev,
        });
        self.touch(EntityId(0));
        rev
    }

    /// DCS changed the owner of an airbase the controller set: a capture is a
    /// fact and replaces the desired owner, at the same revision. Returns
    /// whether anything changed.
    pub fn observe_airbase_owner(&mut self, theater: &str, airbase: &str, coalition: u32) -> bool {
        let key = (theater.to_owned(), airbase.to_owned());
        let Some(o) = self.owners.get_mut(&key) else {
            return false;
        };
        if o.coalition == coalition {
            return false;
        }
        o.coalition = coalition;
        let rev = o.rev;
        self.journal(Entry::AirbaseOwner {
            theater: theater.to_owned(),
            airbase: airbase.to_owned(),
            coalition,
            rev,
        });
        self.touch(EntityId(0));
        true
    }

    /// The counts the controller wants in a warehouse, if it set any.
    pub fn desired_stock(&self, theater: &str, airbase: &str) -> Option<&DesiredStock> {
        self.stocks.get(&(theater.to_owned(), airbase.to_owned()))
    }

    /// Every desired warehouse, by theater and airbase name.
    pub fn desired_stocks(&self) -> impl Iterator<Item = (&(String, String), &DesiredStock)> {
        self.stocks.iter()
    }

    /// The controller wants these counts in a warehouse; items it does not
    /// name keep what was desired. Returns the desired state's revision.
    pub fn set_stock(&mut self, theater: &str, airbase: &str, stock: &Stock) -> u64 {
        let key = (theater.to_owned(), airbase.to_owned());
        let d = self.stocks.entry(key).or_insert(DesiredStock {
            stock: Stock::default(),
            rev: 0,
        });
        d.stock.merge(stock);
        d.rev += 1;
        let rev = d.rev;
        self.journal(Entry::Warehouse {
            theater: theater.to_owned(),
            airbase: airbase.to_owned(),
            rev,
        });
        self.touch(EntityId(0));
        rev
    }

    /// DCS changed the count of an item the controller set: the count it has
    /// replaces the desired one, at the same revision. An item the controller
    /// did not set is left alone. Returns whether anything changed.
    pub fn observe_stock_item(
        &mut self,
        theater: &str,
        airbase: &str,
        item: &Item,
        count: f64,
    ) -> bool {
        let key = (theater.to_owned(), airbase.to_owned());
        let Some(d) = self.stocks.get_mut(&key) else {
            return false;
        };
        match d.stock.get(item) {
            Some(c) if c != count => {}
            _ => return false,
        }
        d.stock.set(item.clone(), count);
        let rev = d.rev;
        self.journal(Entry::Warehouse {
            theater: theater.to_owned(),
            airbase: airbase.to_owned(),
            rev,
        });
        self.touch(EntityId(0));
        true
    }

    /// An applied `WAREHOUSE_ADJUST` changed the stock: add its delta to the
    /// desired count of each item that has one, clamped at 0 (LIVE, E6).
    /// Returns whether anything changed.
    pub fn adjust_desired_stock(&mut self, theater: &str, airbase: &str, delta: &Stock) -> bool {
        let key = (theater.to_owned(), airbase.to_owned());
        let Some(d) = self.stocks.get_mut(&key) else {
            return false;
        };
        let mut changed = false;
        for (item, by) in delta.items() {
            if let Some(c) = d.stock.get(&item) {
                let n = (c + by).max(0.0);
                if n != c {
                    d.stock.set(item, n);
                    changed = true;
                }
            }
        }
        if !changed {
            return false;
        }
        let rev = d.rev;
        self.journal(Entry::Warehouse {
            theater: theater.to_owned(),
            airbase: airbase.to_owned(),
            rev,
        });
        self.touch(EntityId(0));
        true
    }

    /// The sample rates the controller set, if it did.
    pub fn rates(&self) -> Option<Rates> {
        self.rates
    }

    /// The activation's mission load not yet answered (spec/persistence.md,
    /// "Campaign operations").
    pub fn owed_load(&self) -> Option<&OwedLoad> {
        self.owed_load.as_ref()
    }

    /// Set or clear the owed mission load. It goes to disk with the next
    /// commit, in `meta.world`.
    pub fn set_owed_load(&mut self, owed: Option<OwedLoad>) {
        self.owed_load = owed;
    }

    /// Set the sample rates the agent is told.
    pub fn set_rates(&mut self, rates: Rates) {
        if self.rates == Some(rates) {
            return;
        }
        self.rates = Some(rates);
        self.journal(Entry::SampleRates(rates));
        self.touch(EntityId(0));
    }

    /// Register an extension, or change its source: a new spec revision,
    /// journalled. The same source again changes nothing. Returns the
    /// revision.
    pub fn set_extension(&mut self, name: &str, source: &str) -> u64 {
        if let Some(e) = self.extensions.get(name)
            && e.source == source
        {
            return e.spec_rev;
        }
        let rev = self
            .extensions
            .get(name)
            .map_or(self.extension_floor(name) + 1, |e| e.spec_rev + 1);
        self.extensions.insert(
            name.to_owned(),
            Extension {
                name: name.to_owned(),
                spec_rev: rev,
                source: source.to_owned(),
            },
        );
        self.journal(Entry::ExtensionSpec {
            name: name.to_owned(),
            rev,
        });
        rev
    }

    /// An extension by name.
    pub fn extension(&self, name: &str) -> Option<&Extension> {
        self.extensions.get(name)
    }

    /// Every extension, by name.
    pub fn extensions(&self) -> impl Iterator<Item = &Extension> {
        self.extensions.values()
    }

    /// The campaign.
    pub fn campaign(&self) -> CampaignId {
        self.campaign
    }

    /// The store revision: the number of changes so far.
    pub fn revision(&self) -> u64 {
        self.rev
    }

    /// The changes kept, oldest first: each revision and the entity it
    /// changed.
    pub fn changes(&self) -> impl Iterator<Item = &(u64, EntityId)> {
        self.changes.iter()
    }

    fn touch(&mut self, id: EntityId) {
        self.rev += 1;
        self.changes.push_back((self.rev, id));
        while self.changes.len() > self.changes_keep {
            self.changes.pop_front();
        }
    }

    /// Append an entry to the journal. The driver writes it with the rest
    /// of its step, before the step's commands reach DCS (spec/persistence.md,
    /// "Writes").
    pub fn journal(&mut self, e: Entry) {
        match &e {
            Entry::Command(c) => {
                self.inflight.insert((c.epoch, c.seq), c.clone());
            }
            Entry::Outcome { epoch, seq, .. } => {
                self.inflight.remove(&(*epoch, *seq));
            }
            _ => {}
        }
        self.journal.push(e);
    }

    /// The journal entries held in memory, oldest first: every entry of a
    /// store in memory or one that keeps its journal, otherwise those not
    /// yet written.
    pub fn entries(&self) -> &[Entry] {
        &self.journal
    }

    /// Keep written journal entries in memory, for a test that reads them.
    pub fn keep_journal(&mut self, keep: bool) {
        self.keep_journal = keep;
    }

    /// The last agent record folded in, agent boot and record number.
    pub fn last_record(&self) -> Option<(u64, u64)> {
        self.last_record
    }

    /// Whether `r` is at or below the last agent record folded in: read
    /// before a restart that came before the agent learned it was read
    /// (spec/persistence.md, "A crash between the two commits").
    pub fn folded(&self, r: &crate::sim::RawRecord) -> bool {
        self.last_record
            .is_some_and(|(boot, n)| r.agent_boot == boot && r.number <= n)
    }

    /// Note the agent record just folded in and the stamp it was read
    /// under; written with the next commit.
    pub fn set_last_record(&mut self, agent_boot: u64, number: u64, stamp: (u64, u32, u64)) {
        self.last_record = Some((agent_boot, number));
        self.last_stamp = Some(stamp);
    }

    /// The stamp the last record folded in was read under: the records the
    /// agent delivers again after a restart come before their next `FRAME`.
    pub fn last_stamp(&self) -> Option<(u64, u32, u64)> {
        self.last_stamp
    }

    /// Keep a changing call's answer with the next commit. Nothing for a
    /// store in memory, whose keys the API keeps.
    pub fn put_idempotency(&mut self, row: IdemRow) {
        if self.disk.0.is_some() {
            self.idempotency.push(row);
        }
    }

    fn allocate(&mut self) -> Result<EntityId, StoreError> {
        if self.next_entity > MAX_ID {
            return Err(StoreError::Exhausted);
        }
        let id = EntityId(self.next_entity);
        self.next_entity += 1;
        Ok(id)
    }

    /// A new `EntityId` for an unmanaged entity the sidecar adopts. The
    /// store keeps no row for it: the ID is only never given again.
    pub fn adopt_id(&mut self) -> Result<EntityId, StoreError> {
        self.allocate()
    }

    /// An `EntityId` the agent's binding holds for an adopted entity, read
    /// back after a restart: it is never given again.
    pub fn reserve_id(&mut self, id: EntityId) {
        if id.0 >= self.next_entity && id.0 <= MAX_ID {
            self.next_entity = id.0 + 1;
        }
    }

    /// Create a group from its spec, in state `virtual`.
    pub fn create_group(&mut self, spec: NewGroup) -> Result<EntityId, StoreError> {
        self.create(Kind::Group, spec)
    }

    /// Create a static, in state `virtual`. `spec.units` holds exactly its
    /// type and state.
    pub fn create_static(&mut self, spec: NewGroup) -> Result<EntityId, StoreError> {
        assert_eq!(spec.units.len(), 1, "a static is one object");
        self.create(Kind::Static, spec)
    }

    fn create(&mut self, kind: Kind, spec: NewGroup) -> Result<EntityId, StoreError> {
        check_lv(&spec.extra).map_err(StoreError::TooLarge)?;
        for u in &spec.units {
            check_lv(&u.extra).map_err(StoreError::TooLarge)?;
        }
        let id = self.allocate()?;
        let mut members = Vec::with_capacity(spec.units.len());
        for u in spec.units {
            let state = u.state;
            let uid = if kind == Kind::Static {
                id
            } else {
                self.allocate()?
            };
            members.push(uid);
            self.units.insert(
                uid,
                Unit {
                    id: uid,
                    group: id,
                    type_name: u.type_name,
                    state,
                    dead: false,
                    missing: false,
                    removed: false,
                    left_logged: false,
                    object: 0,
                    label: u.label,
                    skill: u.skill,
                    captured: CapturedMore::default(),
                    extra: u.extra,
                },
            );
            self.grid.set(uid, &spec.theater, state.at.x, state.at.z);
        }
        self.journal(Entry::Spec { entity: id, rev: 1 });
        self.groups.insert(
            id,
            Group {
                id,
                kind,
                theater: spec.theater,
                country: spec.country,
                category: spec.category,
                members,
                label: spec.label,
                coalition: spec.coalition,
                extra: spec.extra,
                components: BTreeMap::new(),
                spec_rev: 1,
                observed_coalition: None,
                removed: false,
                options: BTreeMap::new(),
                order: None,
                order_rev: 0,
                progress: 0,
                last_lead: None,
                state: State::Virtual,
                authority: spec.authority,
                on_lost: None,
                error: None,
                captured: false,
                spawned_in: None,
                destroy_failures: 0,
                unknown_spawn_in: None,
                order_sent_in: None,
            },
        );
        self.touch(id);
        Ok(id)
    }

    /// A group or static.
    pub fn group(&self, id: EntityId) -> Option<&Group> {
        self.groups.get(&id)
    }

    /// Every group and static, in `EntityId` order.
    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.values()
    }

    /// A unit, or a static's member.
    pub fn unit(&self, id: EntityId) -> Option<&Unit> {
        self.units.get(&id)
    }

    /// Whether an attack order may name the entity: a group or static that
    /// is not removed, or a unit of one.
    pub fn attackable(&self, id: EntityId) -> bool {
        let owner = self.units.get(&id).map_or(id, |u| u.group);
        self.groups.get(&owner).is_some_and(|g| !g.removed)
    }

    /// The grid of unit positions.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    fn group_mut(&mut self, id: EntityId) -> Result<&mut Group, StoreError> {
        self.groups.get_mut(&id).ok_or(StoreError::NoSuchEntity(id))
    }

    /// Give a group an order, at a new revision. Its progress starts over.
    pub fn set_order(&mut self, id: EntityId, intent: Intent) -> Result<u64, StoreError> {
        // A group, a static or a unit of a group may be attacked, unless
        // its group was removed.
        if let Intent::Attack { target } = &intent
            && !self.attackable(*target)
        {
            return Err(StoreError::UnsupportedTarget(*target));
        }
        let g = self.group_mut(id)?;
        g.order_rev += 1;
        g.order = Some(intent);
        g.progress = 0;
        g.last_lead = None;
        let rev = g.order_rev;
        self.journal(Entry::Order { entity: id, rev });
        self.touch(id);
        Ok(rev)
    }

    /// Change who simulates the entity.
    pub fn set_authority(&mut self, id: EntityId, authority: Authority) -> Result<(), StoreError> {
        let g = self.group_mut(id)?;
        if g.authority != authority {
            g.authority = authority;
            self.journal(Entry::Authority {
                entity: id,
                authority,
            });
            self.touch(id);
        }
        Ok(())
    }

    /// Set the author's lost policy for one group.
    pub fn set_on_lost(
        &mut self,
        id: EntityId,
        policy: Option<LostPolicy>,
    ) -> Result<(), StoreError> {
        self.group_mut(id)?.on_lost = policy;
        self.touch(id);
        Ok(())
    }

    /// Set the author's label of a group, unit or static. Not spec.
    pub fn set_label(&mut self, id: EntityId, label: &str) -> Result<(), StoreError> {
        if let Some(g) = self.groups.get_mut(&id) {
            g.label = label.to_owned();
        } else {
            self.units
                .get_mut(&id)
                .ok_or(StoreError::NoSuchEntity(id))?
                .label = label.to_owned();
        }
        self.touch(id);
        Ok(())
    }

    /// Set an author component of a group or static by its registered name;
    /// `None` removes it. Not spec.
    pub fn set_component(
        &mut self,
        id: EntityId,
        name: &str,
        value: Option<Vec<u8>>,
    ) -> Result<(), StoreError> {
        let g = self.group_mut(id)?;
        match value {
            Some(v) => {
                g.components.insert(name.to_owned(), v);
            }
            None => {
                g.components.remove(name);
            }
        }
        self.touch(id);
        Ok(())
    }

    /// Set a group's or static's coalition: a spec change at a new revision.
    pub fn set_coalition(&mut self, id: EntityId, coalition: u32) -> Result<u64, StoreError> {
        self.group_mut(id)?.coalition = coalition;
        self.spec_changed(id)
    }

    /// Set the further keys of a group's table, or of a unit's or static's:
    /// a spec change at a new revision.
    pub fn set_extra(&mut self, id: EntityId, extra: Lv) -> Result<u64, StoreError> {
        check_lv(&extra).map_err(StoreError::TooLarge)?;
        let group = match self.groups.get_mut(&id) {
            Some(g) if g.kind == Kind::Group => {
                g.extra = extra;
                id
            }
            _ => {
                let u = self
                    .units
                    .get_mut(&id)
                    .ok_or(StoreError::NoSuchEntity(id))?;
                u.extra = extra;
                u.group
            }
        };
        self.spec_changed(group)
    }

    /// Set a unit's skill: a spec change at a new revision.
    pub fn set_skill(&mut self, unit: EntityId, skill: &str) -> Result<u64, StoreError> {
        let u = self
            .units
            .get_mut(&unit)
            .ok_or(StoreError::NoSuchEntity(unit))?;
        u.skill = skill.to_owned();
        let gid = u.group;
        self.spec_changed(gid)
    }

    /// A spec change of a group or static: a new revision, and an
    /// `unspawnable` entity becomes `virtual`.
    fn spec_changed(&mut self, id: EntityId) -> Result<u64, StoreError> {
        let rev = self.bump_spec(id)?;
        if self.groups[&id].state == State::Unspawnable {
            self.transition(id, Trigger::SpecChanged)?;
        }
        Ok(rev)
    }

    /// Change a unit's type: a spec change at a new revision. An
    /// `unspawnable` group becomes `virtual`, and materializes from there.
    pub fn set_unit_type(&mut self, unit: EntityId, type_name: &str) -> Result<u64, StoreError> {
        let u = self
            .units
            .get_mut(&unit)
            .ok_or(StoreError::NoSuchEntity(unit))?;
        u.type_name = type_name.to_owned();
        let gid = u.group;
        self.spec_changed(gid)
    }

    /// A new spec revision with nothing else changed. A spawn at an older
    /// revision is answered `AT_REVISION`; this one is not.
    pub fn bump_spec(&mut self, id: EntityId) -> Result<u64, StoreError> {
        let g = self.group_mut(id)?;
        g.spec_rev += 1;
        g.error = None;
        let rev = g.spec_rev;
        self.journal(Entry::Spec { entity: id, rev });
        self.touch(id);
        Ok(rev)
    }

    /// Apply a lifecycle trigger, journalling the transition first. Returns
    /// the new state.
    pub fn transition(&mut self, id: EntityId, trigger: Trigger) -> Result<State, StoreError> {
        let from = self.group(id).ok_or(StoreError::NoSuchEntity(id))?.state;
        let to =
            lifecycle::next(from, trigger).ok_or(StoreError::NoTransition(id, from, trigger))?;
        self.journal(Entry::Transition {
            entity: id,
            from,
            to,
            spec_rev: None,
        });
        let g = self.group_mut(id)?;
        g.state = to;
        if to != State::Dematerializing {
            g.captured = false;
            g.destroy_failures = 0;
        }
        self.touch(id);
        Ok(to)
    }

    /// Respawn a `lost` group: a new spec revision and the move to
    /// `materializing`, journalled as one entry. The agent holds the spawn's
    /// old revision for the whole generation and would answer a spawn at it
    /// `AT_REVISION` (spec/world.md).
    pub fn respawn(&mut self, id: EntityId) -> Result<u64, StoreError> {
        let from = self.group(id).ok_or(StoreError::NoSuchEntity(id))?.state;
        let trigger = Trigger::PolicyRespawn;
        let to =
            lifecycle::next(from, trigger).ok_or(StoreError::NoTransition(id, from, trigger))?;
        let g = self.group_mut(id)?;
        g.spec_rev += 1;
        g.state = to;
        let rev = g.spec_rev;
        self.journal(Entry::Transition {
            entity: id,
            from,
            to,
            spec_rev: Some(rev),
        });
        self.touch(id);
        Ok(rev)
    }

    /// Settle a dematerializing entity's capture (see [`Entry::Captured`]).
    pub fn set_captured(&mut self, id: EntityId) -> Result<(), StoreError> {
        let g = self.group_mut(id)?;
        if g.state == State::Dematerializing && !g.captured {
            g.captured = true;
            self.journal(Entry::Captured { entity: id });
            self.touch(id);
        }
        Ok(())
    }

    /// A new order revision with the same intent: the target of an attack
    /// order got new DCS object IDs (spec/world.md, "Revisions").
    pub fn bump_order(&mut self, id: EntityId) -> Result<u64, StoreError> {
        let g = self.group_mut(id)?;
        g.order_rev += 1;
        let rev = g.order_rev;
        self.journal(Entry::Order { entity: id, rev });
        self.touch(id);
        Ok(rev)
    }

    /// Record an applied spawn. Returns whether the entity was already
    /// spawned in the same agent boot and generation: a respawn that gives
    /// it new DCS object IDs, as opposed to the spawn after a generation
    /// change.
    pub fn spawned(
        &mut self,
        id: EntityId,
        agent_boot: u64,
        mission_gen: u32,
    ) -> Result<bool, StoreError> {
        let g = self.group_mut(id)?;
        let again = g.spawned_in == Some((agent_boot, mission_gen));
        g.spawned_in = Some((agent_boot, mission_gen));
        Ok(again)
    }

    /// Record an order written for a group at a revision in a generation.
    pub fn order_sent(
        &mut self,
        id: EntityId,
        agent_boot: u64,
        mission_gen: u32,
        rev: u64,
    ) -> Result<(), StoreError> {
        self.group_mut(id)?.order_sent_in = Some((agent_boot, mission_gen, rev));
        Ok(())
    }

    /// Record a spawn that resolved `unknown` in a generation.
    pub fn spawn_unknown(
        &mut self,
        id: EntityId,
        agent_boot: u64,
        mission_gen: u32,
    ) -> Result<(), StoreError> {
        self.group_mut(id)?.unknown_spawn_in = Some((agent_boot, mission_gen));
        Ok(())
    }

    /// Count a refused or unknown destroy of a dematerializing entity.
    /// Returns the count.
    pub fn destroy_failed(&mut self, id: EntityId) -> Result<u32, StoreError> {
        let g = self.group_mut(id)?;
        g.destroy_failures += 1;
        Ok(g.destroy_failures)
    }

    /// Record why DCS refused a spawn.
    pub fn set_error(&mut self, id: EntityId, error: String) -> Result<(), StoreError> {
        self.group_mut(id)?.error = Some(error);
        self.touch(id);
        Ok(())
    }

    /// Journal a unit's death fact. Returns whether it was new.
    pub fn death(&mut self, unit: EntityId) -> Result<bool, StoreError> {
        let u = self
            .units
            .get_mut(&unit)
            .ok_or(StoreError::NoSuchEntity(unit))?;
        if u.dead {
            return Ok(false);
        }
        u.dead = true;
        u.missing = false;
        u.removed = false;
        u.left_logged = false;
        let gid = u.group;
        if let Some(g) = self.groups.get_mut(&gid) {
            g.last_lead = None;
        }
        self.grid.remove(unit);
        self.journal(Entry::Death { unit });
        self.touch(unit);
        Ok(true)
    }

    /// Whether every member of the group has a death fact.
    pub fn all_dead(&self, id: EntityId) -> bool {
        self.group(id)
            .is_some_and(|g| g.members.iter().all(|m| self.units[m].dead))
    }

    /// Mark a unit missing or found. A dead unit stays as it is. A unit
    /// that is found is no longer `removed` either. The mark is durable state:
    /// the step's commit writes it before any command of the step reaches
    /// DCS, since a mark holds the member out of spawns.
    pub fn set_missing(&mut self, unit: EntityId, missing: bool) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
            && (u.missing != missing || (!missing && u.removed))
        {
            u.missing = missing;
            if !missing {
                u.removed = false;
                u.left_logged = false;
            }
            self.touch(unit);
        }
    }

    /// A unit left DCS as `MISSING` or `REMOVED`, with the object ID it left
    /// with. A dead unit stays as it is. `REMOVED` is kept once set. Returns
    /// whether anything changed; a change is journalled, including the first
    /// `Left` for a unit that `set_missing` already marked.
    pub fn left(&mut self, unit: EntityId, object: u64, how: LeftAs) -> bool {
        let Some(u) = self.units.get_mut(&unit) else {
            return false;
        };
        let removed = u.removed || how == LeftAs::Removed;
        if u.dead || (u.left_logged && u.removed == removed && u.object == object) {
            return false;
        }
        u.missing = true;
        u.left_logged = true;
        u.removed = removed;
        if object != 0 {
            u.object = object;
        }
        self.journal(Entry::Left { unit, object, how });
        self.touch(unit);
        true
    }

    /// Record the DCS object ID a unit is bound to.
    pub fn set_object(&mut self, unit: EntityId, object: u64) {
        if let Some(u) = self.units.get_mut(&unit)
            && u.object != object
        {
            u.object = object;
        }
    }

    /// Journal a death fact for an object from before a respawn.
    pub fn stale_death(&mut self, unit: EntityId, object: u64) {
        self.journal(Entry::StaleDeath { unit, object });
    }

    /// Journal that a death fact corrected a `REMOVED`.
    pub fn classifier_fault(&mut self, unit: EntityId, object: u64) {
        self.journal(Entry::ClassifierFault { unit, object });
    }

    /// A unit with a death fact is alive again, bound to `object`: it is no
    /// longer dead and holds nothing.
    /// The death stays in the journal. Returns whether the unit was dead.
    pub fn revive(&mut self, unit: EntityId, object: u64) -> bool {
        let Some(u) = self.units.get_mut(&unit) else {
            return false;
        };
        if !u.dead {
            return false;
        }
        u.dead = false;
        u.missing = false;
        u.removed = false;
        u.left_logged = false;
        u.object = object;
        let (gid, at) = (u.group, u.state.at);
        self.journal(Entry::Revived { unit, object });
        self.touch(unit);
        if let Some(g) = self.groups.get(&gid) {
            self.grid.set(unit, &g.theater, at.x, at.z);
        }
        true
    }

    /// Whether a member of the group left DCS as `MISSING` or `REMOVED` in
    /// this mission generation and has not been found or named by a death
    /// fact since: its fate is unexplained. A group with such a member is
    /// never respawned (spec/world.md, "Respawn gate"). A member with a death
    /// fact does not hold it: the spawn leaves that member out.
    pub fn gate_shut(&self, id: EntityId) -> bool {
        self.group(id).is_some_and(|g| {
            g.members.iter().any(|m| {
                let u = &self.units[m];
                !u.dead && (u.missing || u.removed)
            })
        })
    }

    /// A new mission generation: every unit's DCS object ID is forgotten.
    /// A member the agent reported gone as `MISSING` or `REMOVED`, with no
    /// death fact, keeps its mark, so no restore re-creates it
    /// (spec/persistence.md, "Restore"). A mark inferred at an
    /// enumeration's end clears: it can name a member DCS never bound.
    pub fn new_generation(&mut self) {
        self.break_samples();
        let mut cleared = Vec::new();
        for u in self.units.values_mut() {
            u.object = 0;
            if !u.left_logged && (u.missing || u.removed) {
                u.missing = false;
                u.removed = false;
                cleared.push(u.id);
            }
        }
        for id in cleared {
            self.touch(id);
        }
    }

    /// Whether a member is held out of spawns: it left DCS with no death
    /// fact (spec/world.md, "Respawn gate").
    pub fn held(&self, unit: EntityId) -> bool {
        self.units
            .get(&unit)
            .is_some_and(|u| !u.dead && (u.missing || u.removed))
    }

    /// Set the model time of the position samples that follow: the last
    /// `FRAME`'s.
    pub fn set_sample_time(&mut self, t: f64) {
        self.sample_time = t;
    }

    /// Forget the last samples of every group's lead unit: no segment is made
    /// across a new mission, a `HELLO` or another break in the samples.
    pub fn break_samples(&mut self) {
        for g in self.groups.values_mut() {
            g.last_lead = None;
        }
    }

    /// Route events made since the last call, oldest first.
    pub fn take_route_events(&mut self) -> Vec<RouteEvent> {
        std::mem::take(&mut self.route_events)
    }

    /// A sampled position. Updates the grid and the group's order progress
    /// when the unit is its lead. The lead is the first member that is not
    /// dead or held.
    pub fn observe_position(&mut self, unit: EntityId, at: Point) {
        let Some(u) = self.units.get_mut(&unit) else {
            return;
        };
        if u.dead || u.state.at == at {
            return;
        }
        u.state.at = at;
        let gid = u.group;
        self.touch(unit);
        let Some(g) = self.groups.get(&gid) else {
            return;
        };
        self.grid.set(unit, &g.theater, at.x, at.z);
        let lead = g
            .members
            .iter()
            .find(|m| {
                let u = &self.units[*m];
                !u.dead && !u.missing && !u.removed
            })
            .copied();
        if lead == Some(unit)
            && let Some(order) = &g.order
        {
            let t = self.sample_time;
            // The previous sample makes a segment only if it is the same
            // unit's, earlier and not too long ago.
            let from = g
                .last_lead
                .filter(|(who, _, _, pt)| {
                    *who == unit && *pt <= t && t - *pt <= intents::SEGMENT_MAX_S
                })
                .map(|(_, x, z, _)| (x, z));
            let earlier = g.progress;
            let p = intents::progress(order, earlier, from, at.x, at.z);
            let len = match order {
                Intent::Route { waypoints, .. } => waypoints.len(),
                _ => 0,
            };
            let rev = g.order_rev;
            let g = self.groups.get_mut(&gid).expect("group");
            g.last_lead = (len > 0).then_some((unit, at.x, at.z, t));
            if p != earlier {
                g.progress = p;
                for index in earlier..p {
                    self.route_events.push(RouteEvent {
                        group: gid,
                        order_rev: rev,
                        index: Some(index),
                        model_time: t,
                        observed: true,
                    });
                }
                if p == len {
                    self.route_events.push(RouteEvent {
                        group: gid,
                        order_rev: rev,
                        index: None,
                        model_time: t,
                        observed: true,
                    });
                }
                self.touch(gid);
            }
        }
    }

    /// Sampled life. It replaces the life a `PutGroup` asked for.
    pub fn observe_vitals(&mut self, unit: EntityId, life: f32) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
            && (u.state.life != life || u.captured.life_percent.is_some())
        {
            u.state.life = life;
            u.captured.life_percent = None;
            self.touch(unit);
        }
    }

    /// The life the unit started with, from its `OBJECT` record, when DCS
    /// gave one. A capture's reading replaces it.
    pub fn observe_life0(&mut self, unit: EntityId, life0: f64) {
        if let Some(u) = self.units.get_mut(&unit)
            && life0.is_finite()
            && life0 > 0.0
            && u.captured.life0 != life0
        {
            u.captured.life0 = life0;
            self.touch(unit);
        }
    }

    /// The life a `PutGroup` asks a unit to spawn with, as a percentage of
    /// its initial life (spec/world.md, "Spawn state"). It replaces what DCS
    /// read until DCS reads the unit's life again, and is a spec change, so
    /// the group spawns again with it.
    pub fn set_spawn_life(&mut self, unit: EntityId, life_percent: f64) -> Result<u64, StoreError> {
        let u = self
            .units
            .get_mut(&unit)
            .ok_or(StoreError::NoSuchEntity(unit))?;
        // What DCS read no longer stands for the unit.
        u.state.life = 0.0;
        u.captured.life_percent = Some(life_percent);
        let gid = u.group;
        self.spec_changed(gid)
    }

    /// A captured state replaces the unit's, and what a `PutGroup` asked for.
    pub fn observe_capture(&mut self, unit: EntityId, state: UnitState) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
        {
            u.state = state;
            u.captured.life_percent = None;
            let theater = self.groups[&u.group].theater.clone();
            self.grid.set(unit, &theater, state.at.x, state.at.z);
            self.touch(unit);
        }
    }

    /// The rest of a capture: initial life, attitude, and the DCS
    /// object ID the unit has now (0 leaves it as it was).
    pub fn observe_capture_more(&mut self, unit: EntityId, more: CapturedMore, object: u64) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
        {
            u.captured = more;
            if object != 0 {
                u.object = object;
            }
            self.touch(unit);
        }
    }

    /// The command epoch: kept in the server store, not in this one
    /// (spec/commands.md); the driver gives it to the store at each start.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Start from the epoch the server store holds: the next one is above
    /// it. Not journalled.
    pub fn seed_epoch(&mut self, epoch: u64) {
        self.epoch = self.epoch.max(epoch);
    }

    /// Journal a new epoch.
    pub fn set_epoch(&mut self, epoch: u64) {
        debug_assert!(epoch > self.epoch);
        self.epoch = epoch;
        self.journal(Entry::Epoch(epoch));
    }

    /// The mission last seen loaded.
    pub fn loaded(&self) -> Option<&Loaded> {
        self.loaded.as_ref()
    }

    /// Journal a newly seen loaded mission.
    pub fn set_loaded(&mut self, loaded: Loaded) {
        // A campaign-level change: stamped under no entity.
        self.touch(EntityId(0));
        self.journal(Entry::Loaded(loaded.clone()));
        self.loaded = Some(loaded);
    }

    /// Commands journalled with no outcome, by epoch and sequence number
    /// (spec/persistence.md, `inflight`).
    pub fn unanswered(&self) -> Vec<Journalled> {
        self.inflight.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(n: usize) -> NewGroup {
        NewGroup {
            theater: "Caucasus".into(),
            country: 2,
            category: 2,
            coalition: 2,
            units: (0..n)
                .map(|i| {
                    NewUnit::new(
                        "M-113",
                        UnitState {
                            at: Point {
                                x: i as f64 * 10.0,
                                y: 0.0,
                                z: 0.0,
                            },
                            ..UnitState::default()
                        },
                    )
                })
                .collect(),
            authority: Authority::Sim,
            label: String::new(),
            extra: Lv::Nil,
        }
    }

    #[test]
    fn ids_are_never_reused_and_every_change_bumps_the_revision() {
        let mut s = Store::new(CampaignId(1), 3);
        let a = s.create_group(spec(2)).unwrap();
        let b = s.create_static(spec(1)).unwrap();
        assert_eq!(a, EntityId(1));
        assert_eq!(s.group(a).unwrap().members, vec![EntityId(2), EntityId(3)]);
        assert_eq!(b, EntityId(4));
        assert_eq!(s.group(b).unwrap().members, vec![b]);
        let r = s.revision();
        s.set_order(a, Intent::Hold).unwrap();
        s.transition(a, Trigger::Materialize).unwrap();
        assert_eq!(s.revision(), r + 2);
        assert_eq!(s.changes().count(), 3);
        assert_eq!(s.grid().len(), 3);
    }

    #[test]
    fn a_transition_that_does_not_apply_is_refused_and_not_journalled() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(1)).unwrap();
        let n = s.entries().len();
        assert!(matches!(
            s.transition(a, Trigger::SpawnApplied),
            Err(StoreError::NoTransition(..))
        ));
        assert_eq!(s.entries().len(), n);
    }

    #[test]
    fn the_gate_is_shut_by_an_unexplained_member_only() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(3)).unwrap();
        let m = s.group(a).unwrap().members.clone();
        assert!(!s.gate_shut(a));
        assert!(s.left(m[0], 7, LeftAs::Removed));
        assert!(s.gate_shut(a));
        // A death fact explains the member and releases its hold.
        s.death(m[0]).unwrap();
        assert!(!s.gate_shut(a));
        // A revived member holds nothing; a dead one never does.
        assert!(s.revive(m[0], 7));
        assert!(!s.gate_shut(a));
        assert!(s.left(m[1], 8, LeftAs::Missing));
        assert!(s.gate_shut(a));
        s.set_missing(m[1], false);
        assert!(!s.gate_shut(a));
        assert!(s.left(m[2], 9, LeftAs::Missing));
        // The mark outlives the generation: the member may be dead.
        s.new_generation();
        assert!(s.gate_shut(a));
        assert_eq!(s.unit(m[2]).unwrap().object, 0);
    }

    #[test]
    fn only_journalled_marks_outlive_a_generation_and_removal_releases_them() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(3)).unwrap();
        let m = s.group(a).unwrap().members.clone();
        // An enumeration's end inferred this mark; no OBJECT_GONE named it.
        s.set_missing(m[0], true);
        assert!(s.left(m[1], 8, LeftAs::Removed));
        s.new_generation();
        assert!(!s.held(m[0]), "an inferred mark clears");
        assert!(s.held(m[1]), "a journalled mark is kept");
        // Only the controller releases it.
        s.remove_entity(a).unwrap();
        assert!(!s.held(m[1]));
        assert!(!s.gate_shut(a));
    }

    #[test]
    fn a_left_after_an_enumeration_miss_is_journalled_once() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(2)).unwrap();
        let u = s.group(a).unwrap().members[0];
        let lefts = |s: &Store| {
            s.entries()
                .iter()
                .filter(|e| matches!(e, Entry::Left { .. }))
                .count()
        };
        // The enumeration's end marks the member; no journal entry.
        s.set_missing(u, true);
        assert_eq!(lefts(&s), 0);
        // The agent's OBJECT_GONE for the same object is journalled.
        assert!(s.left(u, 7, LeftAs::Missing));
        assert_eq!(lefts(&s), 1);
        assert!(s.gate_shut(a));
        // An exact repeat is not.
        assert!(!s.left(u, 7, LeftAs::Missing));
        assert_eq!(lefts(&s), 1);
        // Found again, it can leave again.
        s.set_missing(u, false);
        assert!(s.left(u, 7, LeftAs::Missing));
        assert_eq!(lefts(&s), 2);
    }

    #[test]
    fn deaths_are_journalled_once_and_final() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(2)).unwrap();
        let u = s.group(a).unwrap().members[0];
        assert!(s.death(u).unwrap());
        assert!(!s.death(u).unwrap());
        s.set_missing(u, true);
        assert!(!s.unit(u).unwrap().missing);
        assert!(!s.all_dead(a));
        assert!(s.death(s.group(a).unwrap().members[1]).unwrap());
        assert!(s.all_dead(a));
    }

    #[test]
    fn unanswered_commands_come_from_the_journal() {
        let mut s = Store::new(CampaignId(1), 10);
        let c = |seq| Journalled {
            agent_boot: 1,
            epoch: 1,
            seq,
            opcode: 16,
            entity: EntityId(1),
            rev: 1,
            mission_gen: 1,
            position: seq,
            units: vec![],
        };
        s.journal(Entry::Command(c(1)));
        s.journal(Entry::Command(c(2)));
        s.journal(Entry::Outcome {
            epoch: 1,
            seq: 1,
            outcome: Outcome::Applied,
        });
        assert_eq!(s.unanswered(), vec![c(2)]);
    }

    #[test]
    fn labels_and_components_stamp_the_revision_and_spec_fields_bump_spec_rev() {
        let mut s = Store::new(CampaignId(1), 100);
        let a = s.create_group(spec(2)).unwrap();
        let u = s.group(a).unwrap().members[0];
        let (r, spec_rev) = (s.revision(), s.group(a).unwrap().spec_rev);
        s.set_label(a, "platoon").unwrap();
        s.set_label(u, "lead").unwrap();
        s.set_component(a, "supply", Some(vec![1, 2])).unwrap();
        assert_eq!(s.revision(), r + 3);
        assert_eq!(s.group(a).unwrap().spec_rev, spec_rev, "not spec");
        assert_eq!(s.group(a).unwrap().components["supply"], vec![1, 2]);
        s.set_component(a, "supply", None).unwrap();
        assert!(s.group(a).unwrap().components.is_empty());
        s.set_skill(u, "High").unwrap();
        s.set_extra(a, Lv::Nil).unwrap();
        s.set_extra(u, Lv::Nil).unwrap();
        s.set_coalition(a, 1).unwrap();
        assert_eq!(s.group(a).unwrap().spec_rev, spec_rev + 4, "spec");
        assert_eq!(s.unit(u).unwrap().label, "lead");
        let last: Vec<EntityId> = s.changes().map(|c| c.1).collect();
        assert_eq!(last[last.len() - 4..], [a, a, a, a]);
        assert!(s.set_label(EntityId(99), "x").is_err());
    }

    #[test]
    fn an_attack_order_names_a_group_a_unit_or_a_static() {
        let mut s = Store::new(CampaignId(1), 100);
        let a = s.create_group(spec(2)).unwrap();
        let b = s.create_group(spec(1)).unwrap();
        let unit = s.group(b).unwrap().members[0];
        let st = s.create_static(spec(1)).unwrap();
        let rev = s.group(a).unwrap().order_rev;
        let target = EntityId(999);
        assert_eq!(
            s.set_order(a, Intent::Attack { target }),
            Err(StoreError::UnsupportedTarget(target))
        );
        assert_eq!(s.group(a).unwrap().order_rev, rev, "nothing changed");
        assert!(s.group(a).unwrap().order.is_none());
        for target in [b, unit, st] {
            assert!(s.set_order(a, Intent::Attack { target }).is_ok());
        }
        // A removed group, a removed static and a unit of a removed group.
        s.remove_entity(b).unwrap();
        s.remove_entity(st).unwrap();
        for target in [b, unit, st] {
            assert_eq!(
                s.set_order(a, Intent::Attack { target }),
                Err(StoreError::UnsupportedTarget(target))
            );
        }
    }

    #[test]
    fn the_lead_unit_moves_the_route_progress() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(2)).unwrap();
        s.set_order(
            a,
            Intent::Route {
                waypoints: vec![crate::intents::Waypoint { x: 1_000.0, z: 0.0 }],
                speed: 5.0,
                on_road: false,
            },
        )
        .unwrap();
        let [lead, second] = s.group(a).unwrap().members[..] else {
            panic!()
        };
        s.observe_position(
            second,
            Point {
                x: 1_000.0,
                y: 0.0,
                z: 0.0,
            },
        );
        assert_eq!(s.group(a).unwrap().progress, 0);
        s.observe_position(
            lead,
            Point {
                x: 1_000.0,
                y: 0.0,
                z: 0.0,
            },
        );
        assert_eq!(s.group(a).unwrap().progress, 1);
    }

    #[test]
    fn route_events_are_made_once_per_waypoint_and_a_segment_counts() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(1)).unwrap();
        let wps = vec![
            crate::intents::Waypoint { x: 1_000.0, z: 0.0 },
            crate::intents::Waypoint { x: 2_000.0, z: 0.0 },
        ];
        let rev = s
            .set_order(
                a,
                Intent::Route {
                    waypoints: wps,
                    speed: 5.0,
                    on_road: false,
                },
            )
            .unwrap();
        let lead = s.group(a).unwrap().members[0];
        let at = |x: f64| Point { x, y: 0.0, z: 0.0 };
        s.set_sample_time(10.0);
        s.observe_position(lead, at(500.0));
        assert!(s.take_route_events().is_empty());
        // 500 m to 2,500 m in 20 s: both waypoints lie on the segment.
        s.set_sample_time(30.0);
        s.observe_position(lead, at(2_500.0));
        let ev = s.take_route_events();
        let idx: Vec<_> = ev.iter().map(|e| e.index).collect();
        assert_eq!(idx, vec![Some(0), Some(1), None]);
        assert!(
            ev.iter()
                .all(|e| e.order_rev == rev && e.model_time == 30.0)
        );
        // Nothing again, and no segment across a long gap.
        s.set_sample_time(40.0);
        s.observe_position(lead, at(3_000.0));
        assert!(s.take_route_events().is_empty());
        let rev2 = s
            .set_order(
                a,
                Intent::Route {
                    waypoints: vec![crate::intents::Waypoint { x: 4_000.0, z: 0.0 }],
                    speed: 5.0,
                    on_road: false,
                },
            )
            .unwrap();
        s.set_sample_time(50.0);
        s.observe_position(lead, at(3_500.0));
        s.set_sample_time(200.0);
        s.observe_position(lead, at(4_500.0));
        assert!(
            s.take_route_events().is_empty(),
            "a long gap makes no segment"
        );
        s.set_sample_time(210.0);
        s.observe_position(lead, at(4_020.0));
        let ev = s.take_route_events();
        assert_eq!(ev.len(), 2);
        assert!(ev.iter().all(|e| e.order_rev == rev2));
    }

    /// A route whose waypoint lies on the segment across a break in the
    /// samples: the break makes no segment, so no event.
    #[test]
    fn no_route_event_comes_from_a_segment_across_a_break() {
        let route = |x: f64| Intent::Route {
            waypoints: vec![crate::intents::Waypoint { x, z: 0.0 }],
            speed: 5.0,
            on_road: false,
        };
        let at = |x: f64| Point { x, y: 0.0, z: 0.0 };
        let setup = |far: f64| {
            let mut s = Store::new(CampaignId(1), 10);
            let a = s.create_group(spec(2)).unwrap();
            s.set_order(a, route(far)).unwrap();
            let [lead, second] = s.group(a).unwrap().members[..] else {
                panic!()
            };
            s.set_sample_time(10.0);
            s.observe_position(lead, at(500.0));
            s.observe_position(second, at(500.0));
            (s, a, lead, second)
        };

        // set_order between two samples.
        let (mut s, a, lead, _) = setup(9_000.0);
        s.set_order(a, route(1_000.0)).unwrap();
        s.set_sample_time(20.0);
        s.observe_position(lead, at(1_500.0));
        assert!(s.take_route_events().is_empty(), "set_order");
        assert_eq!(s.group(a).unwrap().progress, 0);

        // break_samples (a HELLO) between two samples.
        let (mut s, a, lead, _) = setup(1_000.0);
        s.break_samples();
        s.set_sample_time(20.0);
        s.observe_position(lead, at(1_500.0));
        assert!(s.take_route_events().is_empty(), "break_samples");
        assert_eq!(s.group(a).unwrap().progress, 0);

        // The lead dies between a sample of the old lead and one of the new.
        let (mut s, a, lead, second) = setup(1_000.0);
        s.death(lead).unwrap();
        s.set_sample_time(20.0);
        s.observe_position(second, at(1_500.0));
        assert!(s.take_route_events().is_empty(), "the lead's death");
        assert_eq!(s.group(a).unwrap().progress, 0);
    }

    #[test]
    fn options_move_only_the_store_revision() {
        let mut s = Store::new(CampaignId(1), 100);
        let g = s.create_group(spec(1)).unwrap();
        let (spec_rev, rev) = (s.group(g).unwrap().spec_rev, s.revision());
        s.set_option(g, 3, Lv::Number(2.0)).unwrap();
        assert_eq!(s.group(g).unwrap().spec_rev, spec_rev);
        assert_eq!(s.revision(), rev + 1);
        // The same value again changes nothing.
        s.set_option(g, 3, Lv::Number(2.0)).unwrap();
        assert_eq!(s.revision(), rev + 1);
        s.set_option(g, 3, Lv::Number(1.0)).unwrap();
        assert_eq!(s.group(g).unwrap().options[&3], Lv::Number(1.0));
        assert_eq!(
            s.entries()
                .iter()
                .filter(|e| matches!(e, Entry::Option { .. }))
                .count(),
            2
        );
        assert!(s.set_option(EntityId(99), 1, Lv::Nil).is_err());
    }

    #[test]
    fn desired_owners_and_stock_hold_only_what_the_controller_set() {
        let mut s = Store::new(CampaignId(1), 100);
        assert!(s.desired_owner("Caucasus", "Batumi").is_none());
        // A fact about an airbase nobody set is not kept.
        assert!(!s.observe_airbase_owner("Caucasus", "Batumi", 1));
        assert_eq!(s.set_airbase_owner("Caucasus", "Batumi", 2), 1);
        assert_eq!(s.set_airbase_owner("Caucasus", "Batumi", 2), 2);
        assert!(s.observe_airbase_owner("Caucasus", "Batumi", 1));
        let o = s.desired_owner("Caucasus", "Batumi").unwrap();
        assert_eq!((o.coalition, o.rev), (1, 2), "a capture keeps the revision");
        assert!(s.desired_owner("Afghanistan", "Batumi").is_none());

        let mut st = Stock::default();
        st.set(Item::Weapon("AIM-9X".into()), 4.0);
        assert_eq!(s.set_stock("Caucasus", "Batumi", &st), 1);
        let mut more = Stock::default();
        more.set(Item::Liquid(0), 9.0);
        assert_eq!(s.set_stock("Caucasus", "Batumi", &more), 2);
        let d = s.desired_stock("Caucasus", "Batumi").unwrap();
        assert_eq!(
            d.stock.items().len(),
            2,
            "the second set kept the first's item"
        );
        // DCS used two missiles; an item nobody set is not kept.
        assert!(s.observe_stock_item("Caucasus", "Batumi", &Item::Weapon("AIM-9X".into()), 2.0));
        assert!(!s.observe_stock_item("Caucasus", "Batumi", &Item::Weapon("AIM-9X".into()), 2.0));
        assert!(!s.observe_stock_item("Caucasus", "Batumi", &Item::Weapon("X".into()), 1.0));
        let d = s.desired_stock("Caucasus", "Batumi").unwrap();
        assert_eq!(d.stock.get(&Item::Weapon("AIM-9X".into())), Some(2.0));
        assert_eq!(d.rev, 2);
    }

    #[test]
    fn rates_are_kept_and_journalled_once_per_change() {
        let mut s = Store::new(CampaignId(1), 100);
        assert!(s.rates().is_none());
        let r = Rates {
            position_s: 2.0,
            ..Rates::default()
        };
        s.set_rates(r);
        s.set_rates(r);
        assert_eq!(s.rates(), Some(r));
        assert_eq!(
            s.entries()
                .iter()
                .filter(|e| matches!(e, Entry::SampleRates(_)))
                .count(),
            1
        );
    }
}
