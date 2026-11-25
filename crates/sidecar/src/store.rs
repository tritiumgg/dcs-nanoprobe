//! The world store (plan 4.4): typed tables keyed by `EntityId`, one revision
//! counter that stamps every change, the journal, and the spatial grid.
//!
//! This is the durable half of the sidecar. Whatever is here survives a
//! sidecar restart; the reconciler's own state does not. Step S18 puts the
//! tables and the journal in a per-campaign SQLite file; here they are in
//! memory, and a test keeps the [`Store`] across a simulated restart.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::records::Rates;

use crate::grid::Grid;
use crate::ids::{CampaignId, EntityId, MAX_ID};
use crate::intents::{self, Intent};
use crate::lifecycle::{self, State, Trigger};
use crate::retention;
use crate::stock::{Item, Stock};

#[cfg(feature = "disk")]
mod disk;

#[cfg(feature = "disk")]
pub(crate) use disk::sync_file;
#[cfg(feature = "disk")]
pub use disk::{
    CampaignRecord, CampaignState, Checkpoint, DiskError, JournalPrune, MIGRATIONS, Migration,
    REVISION_RESERVE, SCHEMA, TierPeriod, backup_file, checkpoint_of, idempotency_of, ident_of,
    journal_of, next_entity_of, next_mission_of, rebase_world, record_of, revision_of, schema_of,
    shell_slot_of, state_of, template_of, template_theaters_of,
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

        /// Nothing on disk to count.
        pub(super) fn disk_journal_extent(&self) -> (u64, Option<u64>) {
            (0, None)
        }

        /// Nothing on disk to count.
        pub(super) fn disk_journal_bytes(&self) -> u64 {
            0
        }
    }

    /// The size of an entry in a file this build never writes.
    pub(crate) fn entry_bytes(_: &super::Entry) -> u64 {
        0
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
    /// The last ammunition read of a player-occupied unit. Volatile: never
    /// written to disk, never part of a capture, and forgotten at a new
    /// mission generation.
    pub loadout: Option<Loadout>,
    /// Further keys of the DCS unit or static table: payload, livery,
    /// callsign and so on (spec/world.md, "Lua tables in the API"). Spec.
    pub extra: Lv,
}

/// The stamps of a record, as the API shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReadStamp {
    /// Wall-clock time, seconds since the epoch.
    pub wall_s: f64,
    /// Model time, seconds.
    pub model_time: f64,
    /// The mission generation.
    pub mission_gen: u32,
    /// The agent's frame sequence number.
    pub frame: u64,
}

/// A player-occupied unit's ammunition and the read it came from
/// (spec/records.md, "Layout: AMMO").
#[derive(Clone, Debug, PartialEq)]
pub struct Loadout {
    /// The event that asked for the read: 1 birth, 2 takeoff.
    pub cause: u32,
    /// Counts by `getAmmo` type name.
    pub ammo: BTreeMap<String, u32>,
    /// When the read was made.
    pub read: ReadStamp,
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

/// What a store holds, by [`Store::sizes`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sizes {
    /// Groups and statics neither dead nor removed.
    pub live: u64,
    /// Dead groups, not statics, not removed.
    pub dead: u64,
    /// Dead statics, not removed.
    pub ruins: u64,
    /// Removed groups and statics whose rows are kept.
    pub awaiting_purge: u64,
    /// Unit rows.
    pub units: u64,
    /// The removal time of the oldest of those awaiting purge,
    /// milliseconds since the Unix epoch.
    pub oldest_removal_ms: Option<u64>,
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
    /// When it was removed, by the controller or by the dead groups' expiry:
    /// wall-clock milliseconds since the Unix epoch. `None` in a body
    /// written without it: the purge counts from the first step that sees it
    /// (spec/world.md, "Removal").
    pub removed_at_ms: Option<u64>,
    /// The removal was the dead groups' expiry, not the controller's.
    pub expired: bool,
    /// When it became `dead`, wall-clock milliseconds since the Unix epoch.
    /// `None` in a body written without it: the expiry counts from the first
    /// step that sees it (spec/world.md, "Expiry").
    pub dead_at_ms: Option<u64>,
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
    /// For a static in state `dead`: the agent boot and generation in which
    /// a spawn of its ruin was written. DCS may hold the ruin
    /// (spec/world.md, "Ruins").
    pub ruin_sent_in: Option<(u64, u32)>,
    /// For a static in state `dead`: the agent boot and generation in which
    /// DCS holds its dead object, the static that died or the ruin spawned
    /// for it.
    pub ruin_in: Option<(u64, u32)>,
    /// A heliport group's further units, after the static itself; empty for
    /// anything else. Spec: a revision may add, drop or change them, unit i
    /// keeping its ID (spec/world.md, "Heliport groups").
    pub further: Vec<FurtherUnit>,
    /// The IDs of further units a revision dropped. Each still stands for
    /// this static until the next mission load: the replaced group's handle
    /// answers its name (spec/world.md, "Heliport groups").
    pub retired: Vec<EntityId>,
}

/// A further unit of a heliport group: part of its static's spec, with an
/// `EntityId` of its own that names it in DCS and that no other entity
/// takes. DCS reports the group as one object under the static's name, so
/// the unit has no object, state or binding of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct FurtherUnit {
    /// The ID its DCS name carries.
    pub id: EntityId,
    /// DCS type name.
    pub type_name: String,
    /// Its place.
    pub at: Point,
    /// True heading, radians.
    pub heading: f64,
    /// Further keys of its DCS unit table.
    pub extra: Lv,
}

/// A further unit of a heliport group, before the store gives it an ID.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFurther {
    /// DCS type name.
    pub type_name: String,
    /// Its place.
    pub at: Point,
    /// True heading, radians.
    pub heading: f64,
    /// Further keys of its DCS unit table.
    pub extra: Lv,
}

/// A static's spec as a revision gives it (`Store::revise_static`).
#[derive(Clone, Debug, PartialEq)]
pub struct StaticRevision {
    /// DCS type name.
    pub type_name: String,
    /// Its place.
    pub at: Point,
    /// True heading, radians.
    pub heading: f64,
    /// Further keys of its DCS static table.
    pub extra: Lv,
    /// A heliport group's further units, in order.
    pub further: Vec<NewFurther>,
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
    /// The date and start time written into the shell, milliseconds since
    /// the Unix epoch; `None` for a file the sidecar did not write.
    pub start_ms: Option<i64>,
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

/// Who removed an entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveCause {
    /// The controller, through the API.
    Controller,
    /// The sidecar: a dead group past `dead_keep_h` (spec/world.md, "Expiry").
    Expired,
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
    /// The controller removed a group, static or marker, or the sidecar
    /// expired a dead group.
    Removed {
        /// The entity.
        entity: EntityId,
        /// Who removed it.
        cause: RemoveCause,
    },
    /// A removed entity's rows left the store (spec/world.md, "Removal").
    Purged {
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
    /// The controller released a member held out of spawns: `dead` gave it
    /// a death fact, otherwise the next spawn includes it.
    Resolve {
        /// The unit.
        unit: EntityId,
        /// Whether it was resolved dead.
        dead: bool,
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
    /// The start written into the shell of the mission that runs now, ms
    /// since the Unix epoch: the campaign clock is it plus the model time.
    mission_start_ms: Option<i64>,
    /// The model time of the first sample of the mission that runs now,
    /// with its agent boot and generation.
    model0: Option<(u64, u32, f64)>,
    /// The shell slot (`a` or `b`) of the last load DCS applied.
    shell_slot: Option<u8>,
    /// What the next shell is written with, in the API's encoding; opaque
    /// to the store.
    next_mission: Option<Lv>,
    /// The campaign's own templates by theater (plan revision 35).
    templates: BTreeMap<String, Vec<u8>>,
    /// Theaters whose template changed since the last commit.
    templates_dirty: BTreeSet<String>,
    countries: Option<BTreeMap<u32, u32>>,
    sample_time: f64,
    route_events: Vec<RouteEvent>,
    /// Groups changed since the last write, a unit by its group: the step
    /// commits compare only these with what the file holds. A checkpoint
    /// compares every row (spec/persistence.md, "Writes").
    dirty: BTreeSet<EntityId>,
    /// Every row may have changed since the last write.
    dirty_all: bool,
    /// Groups changed since the last checkpoint, a unit by its group: the
    /// checkpoint compares only these, with their members, unless
    /// `dirty_all` (spec/persistence.md, "Writes").
    since_checkpoint: BTreeSet<EntityId>,
    /// Every row may have changed since the last checkpoint: a bulk change
    /// whose `dirty_all` a step commit then cleared. The next checkpoint
    /// compares every row.
    checkpoint_all: bool,
    /// Groups changed since the reconciler's plan last took them, a unit by
    /// its group, and whether every group may have changed. The plan keeps
    /// its work set from them.
    touched: BTreeSet<EntityId>,
    touched_all: bool,
    /// The wall-clock time of the step, milliseconds since the Unix epoch,
    /// as the driver sets it. Removal and death times are read from it.
    now_ms: u64,
    /// Removed groups and statics whose rows are still kept.
    removed_ids: BTreeSet<EntityId>,
    /// Groups and statics purged, by the revision of their purge, while the
    /// change ring holds that revision.
    purged_recent: BTreeSet<(u64, EntityId)>,
    /// Dead groups (not statics) that are not removed: the expiry's
    /// candidates.
    dead_ids: BTreeSet<EntityId>,
    /// The wall-clock time and the encoded size of each journal entry in
    /// `journal`, for the retention of a store in memory. The size is 0 in a
    /// store with a file.
    journal_at: Vec<(u64, u64)>,
    /// The sum of `journal_at`'s sizes: a running count, so no pass sums it.
    journal_mem_bytes: u64,
    /// Since the store was made or opened: removed entities purged, dead
    /// groups expired, late deaths dropped for purged units, and drift
    /// destroys of objects of entities the store does not hold.
    purged: u64,
    expired: u64,
    purged_deaths: u64,
    orphan_destroys: u64,
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
            mission_start_ms: None,
            model0: None,
            shell_slot: None,
            next_mission: None,
            templates: BTreeMap::new(),
            templates_dirty: BTreeSet::new(),
            countries: None,
            sample_time: 0.0,
            route_events: Vec::new(),
            dirty: BTreeSet::new(),
            dirty_all: true,
            since_checkpoint: BTreeSet::new(),
            checkpoint_all: true,
            touched: BTreeSet::new(),
            touched_all: true,
            now_ms: 0,
            removed_ids: BTreeSet::new(),
            purged_recent: BTreeSet::new(),
            dead_ids: BTreeSet::new(),
            journal_at: Vec::new(),
            journal_mem_bytes: 0,
            purged: 0,
            expired: 0,
            purged_deaths: 0,
            orphan_destroys: 0,
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
    /// reconciler takes it out of DCS, and it is hidden from the API. Its
    /// units leave the grid. The rows stay until the reconciler purges them
    /// ([`Store::purge`]); the ID is never given again either way.
    pub fn remove_entity(&mut self, id: EntityId) -> Result<(), StoreError> {
        self.remove_as(id, RemoveCause::Controller)
    }

    /// Expire a dead group (not a static): a removal by the sidecar, with no
    /// command to DCS (spec/world.md, "Expiry"). Returns whether it expired.
    pub fn expire(&mut self, id: EntityId) -> Result<bool, StoreError> {
        let g = self.group(id).ok_or(StoreError::NoSuchEntity(id))?;
        if g.kind != Kind::Group || g.state != State::Dead || g.removed {
            return Ok(false);
        }
        self.remove_as(id, RemoveCause::Expired)?;
        self.expired += 1;
        Ok(true)
    }

    fn remove_as(&mut self, id: EntityId, cause: RemoveCause) -> Result<(), StoreError> {
        if self.group_mut(id)?.removed {
            return Ok(());
        }
        self.set_authority(id, Authority::Campaign)?;
        let now = self.clock_ms();
        let g = self.group_mut(id)?;
        g.removed = true;
        g.removed_at_ms = Some(now);
        g.expired = cause == RemoveCause::Expired;
        self.removed_ids.insert(id);
        self.dead_ids.remove(&id);
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
            self.grid.remove(m);
        }
        self.journal(Entry::Removed { entity: id, cause });
        self.touch(id);
        Ok(())
    }

    /// Take a removed entity's rows out of the store: the group or static
    /// and its units, from memory and, at the next write, from the file.
    /// The caller has checked that DCS holds nothing of it. Returns its
    /// members, or `None` when it is not a removed entity.
    pub fn purge(&mut self, id: EntityId) -> Option<Vec<EntityId>> {
        if !self.groups.get(&id).is_some_and(|g| g.removed) {
            return None;
        }
        self.journal(Entry::Purged { entity: id });
        // Before the rows go: the revision moves and the write compares the
        // group's row, which is gone, so the file drops it and its members.
        self.touch(id);
        self.purged_recent.insert((self.rev, id));
        let g = self.groups.remove(&id).expect("removed group");
        for m in &g.members {
            self.units.remove(m);
            self.grid.remove(*m);
        }
        self.removed_ids.remove(&id);
        self.dead_ids.remove(&id);
        self.purged += 1;
        Some(g.members)
    }

    /// The removed groups and statics whose rows are kept, by ID.
    pub fn removed_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.removed_ids.iter().copied()
    }

    /// The dead groups (not statics) that are not removed, by ID.
    pub fn dead_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.dead_ids.iter().copied()
    }

    /// Give a removed or dead group the step's time as its removal or death
    /// time, when its body was written without one. Returns the time.
    pub fn stamp_removed(&mut self, id: EntityId) -> Option<u64> {
        let now = self.clock_ms();
        let g = self.groups.get(&id)?;
        if let Some(t) = g.removed_at_ms {
            return Some(t);
        }
        self.group_mut(id).ok()?.removed_at_ms = Some(now);
        Some(now)
    }

    /// Test accessor: clear every group's removal and death time, as a body
    /// written by an older build has none.
    #[doc(hidden)]
    pub fn forget_stamps(&mut self) {
        let ids: Vec<EntityId> = self.groups.keys().copied().collect();
        for id in ids {
            let g = self.group_mut(id).expect("group");
            g.removed_at_ms = None;
            g.dead_at_ms = None;
        }
    }

    /// As [`Store::stamp_removed`], for the time a group became `dead`.
    pub fn stamp_dead(&mut self, id: EntityId) -> Option<u64> {
        let now = self.clock_ms();
        let g = self.groups.get(&id)?;
        if let Some(t) = g.dead_at_ms {
            return Some(t);
        }
        self.group_mut(id).ok()?.dead_at_ms = Some(now);
        Some(now)
    }

    /// Removed entities purged and dead groups expired since the store was
    /// made or opened.
    pub fn purged_expired(&self) -> (u64, u64) {
        (self.purged, self.expired)
    }

    /// Late deaths dropped for purged units and drift destroys of objects of
    /// entities the store does not hold, since the store was made or opened.
    pub fn purge_drops(&self) -> (u64, u64) {
        (self.purged_deaths, self.orphan_destroys)
    }

    /// Count a late death dropped because its unit was purged.
    pub fn count_purged_death(&mut self) {
        self.purged_deaths += 1;
    }

    /// Count a drift destroy of an object of an entity the store does not
    /// hold.
    pub fn count_orphan_destroy(&mut self) {
        self.orphan_destroys += 1;
    }

    /// The bytes of the journal's entries kept, as the size limit counts
    /// them: those written to the file of a store on disk, or all those of
    /// a store in memory.
    pub fn journal_bytes(&self) -> u64 {
        if self.disk.0.is_some() {
            return self.disk_journal_bytes();
        }
        self.journal_mem_bytes
    }

    /// Counts of what the store holds, from one pass over the groups.
    pub fn sizes(&self) -> Sizes {
        let mut s = Sizes {
            units: self.units.len() as u64,
            ..Sizes::default()
        };
        for g in self.groups.values() {
            if g.removed {
                s.awaiting_purge += 1;
                if let Some(t) = g.removed_at_ms {
                    s.oldest_removal_ms = Some(s.oldest_removal_ms.map_or(t, |o| o.min(t)));
                }
            } else if g.state != State::Dead {
                s.live += 1;
            } else if g.kind == Kind::Group {
                s.dead += 1;
            } else {
                s.ruins += 1;
            }
        }
        s
    }

    /// The journal rows held and the time of the oldest, in milliseconds
    /// since the Unix epoch: the rows written to the file of a store on
    /// disk, or all those of a store in memory.
    pub fn journal_extent(&self) -> (u64, Option<u64>) {
        if self.disk.0.is_some() {
            return self.disk_journal_extent();
        }
        (
            self.journal.len() as u64,
            self.journal_at.first().map(|a| a.0),
        )
    }

    /// The groups the next checkpoint compares: those marked since the last
    /// one.
    pub fn checkpoint_marks(&self) -> usize {
        self.since_checkpoint.len()
    }

    /// Whether the next checkpoint compares every row (after a bulk change).
    pub fn checkpoint_compares_all(&self) -> bool {
        self.checkpoint_all || self.dirty_all
    }

    /// The next `EntityId` the store gives. Every ID below it was given
    /// once and is never given again.
    pub fn next_entity(&self) -> u64 {
        self.next_entity
    }

    /// The step's wall-clock time, or the system's when no step set one:
    /// the time journal entries, removals and deaths are stamped with.
    fn clock_ms(&self) -> u64 {
        if self.now_ms != 0 {
            return self.now_ms;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    /// Set the step's wall-clock time, milliseconds since the Unix epoch.
    pub fn set_now(&mut self, ms: u64) {
        self.now_ms = ms;
    }

    /// The step's wall-clock time, milliseconds since the Unix epoch.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// The groups changed since the last call, and whether every group may
    /// have changed (a new store, a load, a new mission generation). For the
    /// reconciler's work set.
    pub fn take_touched(&mut self) -> (BTreeSet<EntityId>, bool) {
        let all = std::mem::take(&mut self.touched_all);
        (std::mem::take(&mut self.touched), all)
    }

    /// Rebuild what the store derives from its rows: the removed and dead
    /// sets. After a load.
    #[cfg_attr(not(feature = "disk"), allow(dead_code))]
    pub(crate) fn reindex(&mut self) {
        self.removed_ids = self
            .groups
            .values()
            .filter(|g| g.removed)
            .map(|g| g.id)
            .collect();
        self.dead_ids = self
            .groups
            .values()
            .filter(|g| !g.removed && g.kind == Kind::Group && g.state == State::Dead)
            .map(|g| g.id)
            .collect();
        self.touched_all = true;
    }

    /// Delete journal entries from a store in memory (spec/persistence.md,
    /// "Journal retention"), by the rule of the journal on disk: those older
    /// than `retention_ms` before the earlier of `now_ms` and the newest
    /// entry's time (0 turns that off; skipped when the clock is behind the
    /// newest entry), and while the bytes of the entries kept, as the journal
    /// encodes them, are over `max_bytes` (0 turns that off). A store on disk
    /// holds only what it has not written, and its entries are not deleted
    /// here. The newest entry is kept. Returns how many were deleted.
    pub fn prune_memory_journal(
        &mut self,
        now_ms: u64,
        retention_ms: u64,
        max_bytes: u64,
    ) -> usize {
        if self.disk.0.is_some() || self.journal.len() < 2 {
            return 0;
        }
        let newest_at = self.journal_at.last().map_or(0, |a| a.0);
        let before_ms = if retention_ms == 0 || now_ms < newest_at {
            0
        } else {
            now_ms.min(newest_at).saturating_sub(retention_ms).max(1)
        };
        let total = self.journal_mem_bytes;
        let excess = if max_bytes == 0 {
            0
        } else {
            total.saturating_sub(max_bytes)
        };
        let batch: Vec<retention::Row> = self
            .journal_at
            .iter()
            .enumerate()
            .map(|(i, a)| (i as u64, a.0, a.1))
            .collect();
        let newest = batch.len() as u64 - 1;
        let cut = retention::cut(
            &batch,
            usize::MAX,
            newest,
            (before_ms != 0).then_some(before_ms),
            excess,
        );
        let Some(last) = cut.last else {
            return 0;
        };
        let n = last as usize + 1;
        self.journal.drain(..n);
        let gone: u64 = self.journal_at.drain(..n).map(|a| a.1).sum();
        self.journal_mem_bytes -= gone;
        n
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

    /// The start written into the running mission's shell, ms since the
    /// Unix epoch.
    pub fn mission_start_ms(&self) -> Option<i64> {
        self.mission_start_ms
    }

    /// Set the running mission's start. It goes to disk with the next
    /// commit, in `meta.world`.
    pub fn set_mission_start_ms(&mut self, start: Option<i64>) {
        self.mission_start_ms = start;
    }

    /// What the next shell is written with.
    pub fn next_mission(&self) -> Option<&Lv> {
        self.next_mission.as_ref()
    }

    /// Set what the next shell is written with. It goes to disk with the
    /// next commit, in `meta.world`.
    pub fn set_next_mission(&mut self, next: Option<Lv>) {
        self.next_mission = next;
    }

    /// The campaign's own template for `theater`.
    pub fn template(&self, theater: &str) -> Option<&[u8]> {
        self.templates.get(theater).map(Vec::as_slice)
    }

    /// The theaters the campaign has its own template for.
    pub fn template_theaters(&self) -> impl Iterator<Item = &str> {
        self.templates.keys().map(String::as_str)
    }

    /// Set or remove the campaign's own template for `theater`. It goes to
    /// disk with the next commit.
    pub fn set_template(&mut self, theater: &str, bytes: Option<Vec<u8>>) {
        match bytes {
            Some(b) => {
                self.templates.insert(theater.to_owned(), b);
            }
            None => {
                self.templates.remove(theater);
            }
        }
        self.templates_dirty.insert(theater.to_owned());
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
        self.mark(id);
        self.rev += 1;
        self.changes.push_back((self.rev, id));
        while self.changes.len() > self.changes_keep {
            self.changes.pop_front();
        }
        // The purges a watch may still read of: those whose change is kept.
        let oldest = self.changes.front().map_or(u64::MAX, |c| c.0);
        while self.purged_recent.first().is_some_and(|(r, _)| *r < oldest) {
            self.purged_recent.pop_first();
        }
    }

    /// Whether the group or static was purged at a revision the change ring
    /// still holds: a watch resuming from before it is told it left the
    /// store.
    pub fn was_purged(&self, id: EntityId) -> bool {
        self.purged_recent.iter().any(|(_, e)| *e == id)
    }

    /// Mark an entity's row as changed for the next write: a unit marks its
    /// group, whose members are compared with it.
    fn mark(&mut self, id: EntityId) {
        let g = self.units.get(&id).map_or(id, |u| u.group);
        self.dirty.insert(g);
        self.since_checkpoint.insert(g);
        self.touched.insert(g);
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
        let bytes = if self.disk.0.is_some() {
            0
        } else {
            disk::entry_bytes(&e)
        };
        self.journal.push(e);
        self.journal_at.push((self.clock_ms(), bytes));
        self.journal_mem_bytes += bytes;
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

    /// How many entity IDs are left to allocate. A call that makes several
    /// entities checks this before the first, so it makes all or none.
    pub fn ids_left(&self) -> u64 {
        (MAX_ID + 1).saturating_sub(self.next_entity)
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
                    loadout: None,
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
                removed_at_ms: None,
                expired: false,
                dead_at_ms: None,
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
                ruin_sent_in: None,
                ruin_in: None,
                further: Vec::new(),
                retired: Vec::new(),
            },
        );
        self.touch(id);
        Ok(id)
    }

    /// Create a heliport group: a static, in state `virtual`, and its
    /// further units, each with an `EntityId` of its own. Every ID is
    /// checked before the first is taken, so it makes all or nothing.
    pub fn create_static_group(
        &mut self,
        spec: NewGroup,
        further: Vec<NewFurther>,
    ) -> Result<EntityId, StoreError> {
        for f in &further {
            check_lv(&f.extra).map_err(StoreError::TooLarge)?;
        }
        if self.ids_left() < 1 + further.len() as u64 {
            return Err(StoreError::Exhausted);
        }
        let id = self.create_static(spec)?;
        let mut units = Vec::with_capacity(further.len());
        for f in further {
            units.push(FurtherUnit {
                id: self.allocate()?,
                type_name: f.type_name,
                at: f.at,
                heading: f.heading,
                extra: f.extra,
            });
        }
        self.groups.get_mut(&id).expect("created").further = units;
        Ok(id)
    }

    /// Revise a static's spec in place (spec/world.md, "Revising a
    /// static"): its type, place, heading, further keys and further units,
    /// at one new spec revision. Further unit i keeps its ID; units past
    /// the old count take new IDs, and the IDs of dropped units move to
    /// `retired`, still standing for the static until the next mission
    /// load. Every new ID is checked before the first is taken, so
    /// it changes all or nothing. Returns the new revision, or `None` when
    /// the revision equals the stored spec and nothing changed.
    pub fn revise_static(
        &mut self,
        id: EntityId,
        r: StaticRevision,
    ) -> Result<Option<u64>, StoreError> {
        let g = self
            .groups
            .get(&id)
            .filter(|g| g.kind == Kind::Static && !g.removed)
            .ok_or(StoreError::NoSuchEntity(id))?;
        check_lv(&r.extra).map_err(StoreError::TooLarge)?;
        for f in &r.further {
            check_lv(&f.extra).map_err(StoreError::TooLarge)?;
        }
        let u = &self.units[&id];
        let same_further = g.further.len() == r.further.len()
            && g.further.iter().zip(&r.further).all(|(a, b)| {
                a.type_name == b.type_name
                    && a.at == b.at
                    && a.heading == b.heading
                    && a.extra == b.extra
            });
        if same_further
            && u.type_name == r.type_name
            && u.state.at == r.at
            && u.state.heading == r.heading
            && u.extra == r.extra
        {
            return Ok(None);
        }
        let kept = g.further.len();
        let new = r.further.len().saturating_sub(kept) as u64;
        if self.ids_left() < new {
            return Err(StoreError::Exhausted);
        }
        let theater = g.theater.clone();
        let old: Vec<EntityId> = g.further.iter().map(|f| f.id).collect();
        let dropped = old.iter().skip(r.further.len()).copied();
        let mut retired = g.retired.clone();
        retired.extend(dropped);
        let mut further = Vec::with_capacity(r.further.len());
        for (i, f) in r.further.into_iter().enumerate() {
            let fid = match old.get(i) {
                Some(fid) => *fid,
                None => self.allocate()?,
            };
            further.push(FurtherUnit {
                id: fid,
                type_name: f.type_name,
                at: f.at,
                heading: f.heading,
                extra: f.extra,
            });
        }
        let u = self.units.get_mut(&id).expect("a static is its unit");
        u.type_name = r.type_name;
        u.state.at = r.at;
        u.state.heading = r.heading;
        u.extra = r.extra;
        self.grid.set(id, &theater, r.at.x, r.at.z);
        let g = self.group_mut(id)?;
        g.further = further;
        g.retired = retired;
        self.spec_changed(id).map(Some)
    }

    /// The static whose further unit the ID names, if any: the ID is held,
    /// by that static (spec/world.md, "Heliport groups").
    pub fn further_owner(&self, id: EntityId) -> Option<EntityId> {
        self.groups
            .values()
            .find(|g| g.further.iter().any(|f| f.id == id) || g.retired.contains(&id))
            .map(|g| g.id)
    }

    /// Create a static destroyed: its member has a death fact and it is
    /// `dead` from the start, so DCS only ever shows its ruin
    /// (spec/world.md, "Ruins").
    pub fn create_ruin(&mut self, spec: NewGroup) -> Result<EntityId, StoreError> {
        let id = self.create_static(spec)?;
        self.death(id)?;
        self.transition(id, Trigger::AllDead)?;
        Ok(id)
    }

    /// Whether the entity is a static in state `dead` that the controller
    /// has not removed: the reconciler keeps its ruin in DCS.
    pub fn is_ruin(&self, id: EntityId) -> bool {
        self.group(id)
            .is_some_and(|g| g.kind == Kind::Static && g.state == State::Dead && !g.removed)
    }

    /// Record that a spawn of a dead static's ruin is written in a
    /// generation: DCS may hold the ruin from now on.
    pub fn ruin_sent(
        &mut self,
        id: EntityId,
        agent_boot: u64,
        mission_gen: u32,
    ) -> Result<(), StoreError> {
        self.group_mut(id)?.ruin_sent_in = Some((agent_boot, mission_gen));
        Ok(())
    }

    /// Record that DCS holds a dead static's object in a generation: the
    /// static died there, or the spawn of its ruin applied.
    pub fn ruin_held(
        &mut self,
        id: EntityId,
        agent_boot: u64,
        mission_gen: u32,
    ) -> Result<(), StoreError> {
        if self
            .group(id)
            .is_some_and(|g| g.ruin_in != Some((agent_boot, mission_gen)))
        {
            self.group_mut(id)?.ruin_in = Some((agent_boot, mission_gen));
        }
        Ok(())
    }

    /// Record that DCS holds no object of a dead static: a destroy applied.
    pub fn ruin_gone(&mut self, id: EntityId) -> Result<(), StoreError> {
        if self
            .group(id)
            .is_some_and(|g| g.ruin_in.is_some() || g.ruin_sent_in.is_some())
        {
            let g = self.group_mut(id)?;
            g.ruin_in = None;
            g.ruin_sent_in = None;
        }
        Ok(())
    }

    /// Record that DCS may hold a dead static's object and may not: a
    /// destroy of it was taken and its result lost. The mark that DCS holds
    /// it becomes the mark that it may. A mark of an earlier generation says
    /// nothing of the loaded one: it is dropped, and a spawn written in the
    /// loaded generation keeps its own mark.
    pub fn ruin_unsure(&mut self, id: EntityId) -> Result<(), StoreError> {
        let now = self.loaded().map(|l| (l.agent_boot, l.mission_gen));
        if let Some(held) = self.group(id).and_then(|g| g.ruin_in) {
            let g = self.group_mut(id)?;
            g.ruin_in = None;
            if Some(held) == now {
                g.ruin_sent_in = Some(held);
            }
        }
        Ok(())
    }

    /// Forget a spawn's error: a later spawn applied.
    pub fn clear_error(&mut self, id: EntityId) -> Result<(), StoreError> {
        if self.group(id).is_some_and(|g| g.error.is_some()) {
            self.group_mut(id)?.error = None;
            self.touch(id);
        }
        Ok(())
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

    /// A group to change, marked for the next write.
    fn group_mut(&mut self, id: EntityId) -> Result<&mut Group, StoreError> {
        if self.groups.contains_key(&id) {
            self.dirty.insert(id);
            self.since_checkpoint.insert(id);
            self.touched.insert(id);
        }
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
        let now = self.clock_ms();
        let g = self.group_mut(id)?;
        g.state = to;
        if to != State::Dematerializing {
            g.captured = false;
            g.destroy_failures = 0;
        }
        if to == State::Dead {
            g.dead_at_ms = Some(now);
            if g.kind == Kind::Group && !g.removed {
                self.dead_ids.insert(id);
            }
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
        let mut ruin = false;
        if let Some(g) = self.groups.get_mut(&gid) {
            g.last_lead = None;
            ruin = g.kind == Kind::Static;
        }
        // A dead static is a ruin where it stood: an area query finds it
        // (spec/world.md, "Ruins").
        if !ruin {
            self.grid.remove(unit);
        }
        self.journal(Entry::Death { unit });
        self.touch(unit);
        Ok(true)
    }

    /// Release a member held out of spawns (spec/world.md, "Respawn
    /// gate"): `dead` journals a death fact, otherwise the mark is cleared
    /// so the group's next spawn includes it. Journalled as `resolve`.
    /// Returns whether the unit was held.
    pub fn resolve_member(&mut self, unit: EntityId, dead: bool) -> Result<bool, StoreError> {
        if self.unit(unit).is_none() {
            return Err(StoreError::NoSuchEntity(unit));
        }
        if !self.held(unit) {
            return Ok(false);
        }
        self.journal(Entry::Resolve { unit, dead });
        if dead {
            self.death(unit)?;
        } else {
            self.set_missing(unit, false);
        }
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
            self.mark(unit);
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
        // A removed entity's units stay out of the grid.
        if let Some(g) = self.groups.get(&gid).filter(|g| !g.removed) {
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
        self.dirty_all = true;
        self.checkpoint_all = true;
        self.touched_all = true;
        let mut cleared = Vec::new();
        for u in self.units.values_mut() {
            u.object = 0;
            u.loadout = None;
            if !u.left_logged && (u.missing || u.removed) {
                u.missing = false;
                u.removed = false;
                cleared.push(u.id);
            }
        }
        for id in cleared {
            self.touch(id);
        }
        // A dropped further unit's name finds nothing in a new mission.
        for g in self.groups.values_mut() {
            g.retired.clear();
        }
    }

    /// Whether a member is held out of spawns: it left DCS with no death
    /// fact (spec/world.md, "Respawn gate").
    pub fn held(&self, unit: EntityId) -> bool {
        self.units
            .get(&unit)
            .is_some_and(|u| !u.dead && (u.missing || u.removed))
    }

    /// The model time of the last samples.
    pub fn sample_time(&self) -> f64 {
        self.sample_time
    }

    /// Set the model time of the position samples that follow: the last
    /// `FRAME`'s.
    pub fn set_sample_time(&mut self, t: f64) {
        self.sample_time = t;
        // The first sample of a generation sets its model time zero for the
        // campaign clock.
        if let Some(l) = &self.loaded
            && self
                .model0
                .is_none_or(|(b, g, _)| (b, g) != (l.agent_boot, l.mission_gen))
        {
            self.model0 = Some((l.agent_boot, l.mission_gen, t));
        }
    }

    /// Count the model time from the next generation's first sample: until
    /// one arrives, [`Store::model_elapsed`] is 0. For a load just applied,
    /// whose mission has not started yet.
    pub fn await_generation(&mut self) {
        let (b, g) = self
            .loaded
            .as_ref()
            .map_or((0, 0), |l| (l.agent_boot, l.mission_gen));
        // The running generation's zero is past any of its samples.
        self.model0 = Some((b, g, f64::MAX));
    }

    /// The model time since the running generation's first sample, seconds.
    pub fn model_elapsed(&self) -> f64 {
        match (&self.loaded, self.model0) {
            (Some(l), Some((b, g, t0))) if (b, g) == (l.agent_boot, l.mission_gen) => {
                (self.sample_time - t0).max(0.0)
            }
            _ => 0.0,
        }
    }

    /// The shell slot of the last load DCS applied.
    pub fn shell_slot(&self) -> Option<u8> {
        self.shell_slot
    }

    /// Set the shell slot of the last load DCS applied.
    pub fn set_shell_slot(&mut self, slot: Option<u8>) {
        self.shell_slot = slot;
    }

    /// Forget the last samples of every group's lead unit: no segment is made
    /// across a new mission, a `HELLO` or another break in the samples.
    pub fn break_samples(&mut self) {
        let held: Vec<EntityId> = self
            .groups
            .values()
            .filter(|g| g.last_lead.is_some())
            .map(|g| g.id)
            .collect();
        // A change of the row: the revision moves, so the next checkpoint
        // writes it.
        for id in held {
            self.groups.get_mut(&id).expect("group").last_lead = None;
            self.touch(id);
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
        if !g.removed {
            self.grid.set(unit, &g.theater, at.x, at.z);
        }
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

    /// A player-occupied unit's ammunition read. It is volatile: nothing of
    /// it is written to disk.
    pub fn observe_loadout(&mut self, unit: EntityId, loadout: Loadout) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
            && u.loadout.as_ref() != Some(&loadout)
        {
            u.loadout = Some(loadout);
            self.touch(unit);
        }
    }

    /// The last player left the unit: it shows no ammunition read.
    pub fn forget_loadout(&mut self, unit: EntityId) {
        if let Some(u) = self.units.get_mut(&unit)
            && u.loadout.is_some()
        {
            u.loadout = None;
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
            let g = &self.groups[&u.group];
            if !g.removed {
                let theater = g.theater.clone();
                self.grid.set(unit, &theater, state.at.x, state.at.z);
            }
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
    fn a_removed_groups_units_leave_the_grid_and_do_not_come_back() {
        let mut s = Store::new(CampaignId(1), 10);
        let g = s.create_group(spec(2)).expect("group");
        let members = s.group(g).expect("g").members.clone();
        let near = |s: &Store| {
            s.grid()
                .within("Caucasus", 0.0, 0.0, 1_000.0)
                .into_iter()
                .filter(|u| members.contains(u))
                .count()
        };
        assert_eq!(near(&s), 2);
        s.remove_entity(g).expect("remove");
        assert_eq!(near(&s), 0, "removed");
        let at = Point {
            x: 5.0,
            y: 0.0,
            z: 5.0,
        };
        s.observe_position(members[0], at);
        s.observe_capture(
            members[1],
            UnitState {
                at,
                ..UnitState::default()
            },
        );
        s.death(members[0]).expect("death");
        s.revive(members[0], 77);
        assert_eq!(near(&s), 0, "no sample, capture or revival puts them back");
    }

    #[cfg(feature = "disk")]
    #[test]
    fn a_memory_journal_is_pruned_by_time_and_by_size_and_keeps_the_newest() {
        let spec_entry = |i: u64| Entry::Spec {
            entity: EntityId(i),
            rev: i,
        };
        let fill = || {
            let mut s = Store::new(CampaignId(1), 3);
            for i in 1..=9u64 {
                s.set_now(i * 1_000);
                s.journal(spec_entry(i));
            }
            s
        };
        let one = disk::entry_bytes(&spec_entry(1));
        assert!(one > 0);
        // No limit: nothing.
        assert_eq!(fill().prune_memory_journal(9_000, 0, 0), 0);
        // Time only: entries stamped before 4,000.
        let mut s = fill();
        assert_eq!(s.prune_memory_journal(9_000, 5_000, 0), 3);
        assert_eq!(s.entries().len(), 6);
        // Size only: keep about three entries.
        let mut s = fill();
        assert_eq!(s.prune_memory_journal(9_000, 0, 3 * one), 6);
        assert_eq!(s.entries().len(), 3);
        // Both: the time rule takes 3, the size rule takes more.
        let mut s = fill();
        assert_eq!(s.prune_memory_journal(9_000, 5_000, 3 * one), 6);
        // The newest stays however small the limit.
        let mut s = fill();
        assert_eq!(s.prune_memory_journal(9_000, 0, 1), 8);
        assert_eq!(s.entries().len(), 1);
        assert_eq!(s.prune_memory_journal(9_000, 0, 1), 0);
    }

    /// A clock behind the newest entry's time deletes nothing by time.
    #[cfg(feature = "disk")]
    #[test]
    fn a_memory_journal_is_not_pruned_by_time_while_the_clock_is_behind_the_newest_entry() {
        let mut s = Store::new(CampaignId(1), 3);
        for i in 1..=5u64 {
            s.set_now(i * 1_000);
            s.journal(Entry::Spec {
                entity: EntityId(i),
                rev: i,
            });
        }
        // The clock is behind the newest entry (5,000).
        assert_eq!(s.prune_memory_journal(2_500, 1_000, 0), 0);
        assert_eq!(s.entries().len(), 5);
        // With the clock at the newest entry, the old ones go.
        assert_eq!(s.prune_memory_journal(5_000, 1_000, 0), 3);
        assert_eq!(s.entries().len(), 2);
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

    fn further(n: usize) -> Vec<NewFurther> {
        (0..n)
            .map(|i| NewFurther {
                type_name: "FARP".into(),
                at: Point {
                    x: 100.0 + i as f64,
                    y: 0.0,
                    z: 5.0,
                },
                heading: 0.25,
                extra: Lv::Table(vec![(Lv::String(b"k".to_vec()), Lv::Number(i as f64))]),
            })
            .collect()
    }

    #[test]
    fn a_static_group_gives_every_further_unit_an_id_after_the_statics() {
        let mut s = Store::new(CampaignId(1), 3);
        let id = s.create_static_group(spec(1), further(2)).unwrap();
        assert_eq!(id, EntityId(1));
        let g = s.group(id).unwrap();
        let ids: Vec<EntityId> = g.further.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![EntityId(2), EntityId(3)]);
        assert_eq!(g.further[1].type_name, "FARP");
        assert_eq!(g.further[1].at.x, 101.0);
        assert_eq!(g.members, vec![id], "the static is the one member");
        let next = s.create_static(spec(1)).unwrap();
        assert_eq!(next, EntityId(4));
        assert!(s.group(next).unwrap().further.is_empty());
    }

    #[test]
    fn further_owner_finds_the_static_of_a_further_id_and_nothing_else() {
        let mut s = Store::new(CampaignId(1), 3);
        let a = s.create_static_group(spec(1), further(2)).unwrap();
        let plain = s.create_static(spec(1)).unwrap();
        assert_eq!(s.further_owner(EntityId(2)), Some(a));
        assert_eq!(s.further_owner(EntityId(3)), Some(a));
        assert_eq!(s.further_owner(a), None, "the static itself is not further");
        assert_eq!(s.further_owner(plain), None);
        assert_eq!(s.further_owner(EntityId(999)), None);
    }

    #[test]
    fn a_static_group_that_does_not_fit_in_the_ids_left_makes_nothing() {
        let mut s = Store::new(CampaignId(1), 3);
        s.reserve_id(EntityId(MAX_ID - 2));
        // MAX_ID - 1 and MAX_ID are left: two IDs, three needed.
        assert_eq!(s.ids_left(), 2);
        let next = s.next_entity();
        let changes = s.changes().count();
        let r = s.create_static_group(spec(1), further(2));
        assert!(matches!(r, Err(StoreError::Exhausted)));
        assert_eq!(s.next_entity(), next);
        assert_eq!(s.changes().count(), changes);
        assert_eq!(s.ids_left(), 2);
        // Exactly enough works.
        let id = s.create_static_group(spec(1), further(1)).unwrap();
        assert_eq!(s.group(id).unwrap().further.len(), 1);
        assert_eq!(s.ids_left(), 0);
    }

    /// The revision that gives a static of `spec(1)` its own spec back,
    /// with `further`.
    fn same(s: &Store, id: EntityId, further: Vec<NewFurther>) -> StaticRevision {
        let u = s.unit(id).unwrap();
        StaticRevision {
            type_name: u.type_name.clone(),
            at: u.state.at,
            heading: u.state.heading,
            extra: u.extra.clone(),
            further,
        }
    }

    #[test]
    fn a_revision_equal_to_the_spec_changes_nothing() {
        let mut s = Store::new(CampaignId(1), 10);
        let id = s.create_static_group(spec(1), further(2)).unwrap();
        let (rev, spec_rev) = (s.revision(), s.group(id).unwrap().spec_rev);
        assert_eq!(s.revise_static(id, same(&s, id, further(2))), Ok(None));
        assert_eq!(s.revision(), rev);
        assert_eq!(s.group(id).unwrap().spec_rev, spec_rev);
    }

    #[test]
    fn a_revision_moves_turns_and_retypes_a_static_at_one_new_revision() {
        let mut s = Store::new(CampaignId(1), 10);
        let id = s.create_static(spec(1)).unwrap();
        let mut r = same(&s, id, Vec::new());
        r.type_name = "Hangar A".into();
        r.at = Point {
            x: 500.0,
            y: 0.0,
            z: 50.0,
        };
        r.heading = 1.5;
        assert_eq!(s.revise_static(id, r), Ok(Some(2)));
        let u = s.unit(id).unwrap();
        assert_eq!(u.type_name, "Hangar A");
        assert_eq!((u.state.at.x, u.state.heading), (500.0, 1.5));
        assert_eq!(s.grid().within("Caucasus", 500.0, 50.0, 1.0), vec![id]);
        assert!(s.grid().within("Caucasus", 0.0, 0.0, 1.0).is_empty());
    }

    #[test]
    fn a_revision_keeps_further_ids_by_index_and_takes_new_ones_past_them() {
        let mut s = Store::new(CampaignId(1), 10);
        let id = s.create_static_group(spec(1), further(2)).unwrap();
        let other = s.create_static(spec(1)).unwrap();
        assert_eq!(other, EntityId(4));
        let ids = |s: &Store| -> Vec<u64> {
            s.group(id)
                .unwrap()
                .further
                .iter()
                .map(|f| f.id.0)
                .collect()
        };
        // Grow 2 to 4: 2 and 3 kept, 5 and 6 new.
        assert_eq!(s.revise_static(id, same(&s, id, further(4))), Ok(Some(2)));
        assert_eq!(ids(&s), vec![2, 3, 5, 6]);
        assert_eq!(s.further_owner(EntityId(6)), Some(id));
        // Shrink 4 to 1: 2 kept; the dropped IDs stand for the static until
        // the next mission load.
        assert_eq!(s.revise_static(id, same(&s, id, further(1))), Ok(Some(3)));
        assert_eq!(ids(&s), vec![2]);
        assert_eq!(
            s.group(id).unwrap().retired,
            vec![EntityId(3), EntityId(5), EntityId(6)]
        );
        for gone in [3, 5, 6] {
            assert_eq!(s.further_owner(EntityId(gone)), Some(id));
        }
        s.new_generation();
        for gone in [3, 5, 6] {
            assert_eq!(s.further_owner(EntityId(gone)), None);
        }
        assert_eq!(s.further_owner(EntityId(2)), Some(id));
        // Grow again: a dropped ID is never given again.
        assert_eq!(s.revise_static(id, same(&s, id, further(2))), Ok(Some(4)));
        assert_eq!(ids(&s), vec![2, 7]);
        // To none: a single heliport.
        assert_eq!(s.revise_static(id, same(&s, id, Vec::new())), Ok(Some(5)));
        assert!(ids(&s).is_empty());
    }

    #[test]
    fn a_revision_that_does_not_fit_in_the_ids_left_changes_nothing() {
        let mut s = Store::new(CampaignId(1), 10);
        let id = s.create_static_group(spec(1), further(1)).unwrap();
        s.reserve_id(EntityId(MAX_ID - 1));
        assert_eq!(s.ids_left(), 1);
        let rev = s.revision();
        let r = s.revise_static(id, same(&s, id, further(3)));
        assert_eq!(r, Err(StoreError::Exhausted));
        assert_eq!(s.revision(), rev);
        assert_eq!(s.group(id).unwrap().further.len(), 1);
        assert_eq!(s.ids_left(), 1);
        assert_eq!(s.revise_static(id, same(&s, id, further(2))), Ok(Some(2)));
    }

    #[test]
    fn only_a_held_static_is_revised() {
        let mut s = Store::new(CampaignId(1), 10);
        let g = s.create_group(spec(1)).unwrap();
        let r = StaticRevision {
            type_name: "FARP".into(),
            at: Point::default(),
            heading: 0.0,
            extra: Lv::Nil,
            further: Vec::new(),
        };
        assert_eq!(
            s.revise_static(g, r.clone()),
            Err(StoreError::NoSuchEntity(g))
        );
        let st = s.create_static(spec(1)).unwrap();
        s.remove_entity(st).unwrap();
        assert_eq!(s.revise_static(st, r), Err(StoreError::NoSuchEntity(st)));
    }

    #[test]
    fn a_moved_cargos_place_is_its_last_known_one_and_a_revision_replaces_it() {
        let mut s = Store::new(CampaignId(1), 10);
        let id = s.create_static(spec(1)).unwrap();
        let first = same(&s, id, Vec::new());
        let moved = Point {
            x: 40.0,
            y: 0.0,
            z: 5.0,
        };
        s.observe_position(id, moved);
        assert_eq!(s.unit(id).unwrap().state.at, moved, "play moved it");
        // The spec as created is now a change: the cargo goes back.
        assert_eq!(s.revise_static(id, first.clone()), Ok(Some(2)));
        assert_eq!(s.unit(id).unwrap().state.at, first.at);
        assert_eq!(s.revise_static(id, first), Ok(None));
    }

    #[test]
    fn a_revision_makes_an_unspawnable_static_virtual() {
        let mut s = Store::new(CampaignId(1), 10);
        let id = s.create_static(spec(1)).unwrap();
        s.transition(id, Trigger::Materialize).unwrap();
        s.transition(id, Trigger::SpawnRefused).unwrap();
        assert_eq!(s.group(id).unwrap().state, State::Unspawnable);
        let mut r = same(&s, id, Vec::new());
        r.heading = 1.0;
        s.revise_static(id, r).unwrap();
        assert_eq!(s.group(id).unwrap().state, State::Virtual);
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

    #[test]
    fn a_held_member_resolves_dead_or_restored_once() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.create_group(spec(2)).unwrap();
        let m = s.group(a).unwrap().members.clone();
        assert!(!s.resolve_member(m[0], true).unwrap(), "not held");
        assert!(matches!(
            s.resolve_member(EntityId(999), true),
            Err(StoreError::NoSuchEntity(_))
        ));
        assert!(s.left(m[0], 7, LeftAs::Removed));
        assert!(s.left(m[1], 8, LeftAs::Missing));
        assert!(s.held(m[0]) && s.held(m[1]));
        let rev = s.revision();
        assert!(s.resolve_member(m[0], true).unwrap());
        assert!(s.unit(m[0]).unwrap().dead && !s.held(m[0]));
        assert!(s.resolve_member(m[1], false).unwrap());
        assert!(!s.unit(m[1]).unwrap().dead && !s.held(m[1]));
        assert!(s.revision() > rev, "the change shows in a watch");
        assert!(!s.resolve_member(m[1], false).unwrap(), "once");
        let resolves: Vec<_> = s
            .entries()
            .iter()
            .filter_map(|e| match e {
                Entry::Resolve { unit, dead } => Some((*unit, *dead)),
                _ => None,
            })
            .collect();
        assert_eq!(resolves, [(m[0], true), (m[1], false)]);
    }
}
