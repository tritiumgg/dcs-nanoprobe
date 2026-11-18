//! A fake DCS behind the sidecar's simulation adapter, for development and CI.
//!
//! [`FakeSim`] models a DCS process with the agent in it, at the level of the
//! agent's protocol: it reads commands from an inbound queue and writes the
//! records spec/records.md describes. It follows spec/commands.md for epochs,
//! de-duplication, revisions and generations, and spec/records.md for the
//! object table, enumerations, the event queue and `GAP`. Time moves only
//! when the caller runs [`FakeSim::frame`], and nothing in it is random but
//! the death delay, which a seed draws: a test that drives it from a seed
//! replays exactly.
//!
//! Faults are methods: kill a unit, remove an object with or without an
//! event, detach and attach the sidecar, load a mission, restart DCS. A kill
//! can send its death event late (`Config::death_delay_max`, up to about 83 s
//! of model time, LIVE): the row leaves the agent's table as `MISSING` or
//! `REMOVED` at once, and the agent, which keeps the object ID, entity and
//! group entity of every row that left so, writes a second `OBJECT_GONE`
//! `DEAD` when the event arrives. A killed object can come alive again
//! ([`FakeSim::revive`]) under the same object ID. The
//! [`Ledger`] records the ground truth that the reconciler's tests check
//! against.
//!
//! Pause ([`FakeSim::pause`]) stops model time and movement; the agent still
//! takes commands. A destroy while paused is pending (spec/commands.md): the
//! rows leave as `DESTROYED` at once, and the objects stay in DCS until the
//! resume. `BURST` lifts the per-frame command limit while paused.
//!
//! What the fake does not model: `CONTROL_LOST`, the commanded-work budget in time
//! (it takes a fixed number of commands per frame), effects, and every
//! opcode the reconciler does not write, which it answers `UNKNOWN_OPCODE`.

pub mod harness;

/// `player_event` codes (spec/records.md).
mod pe {
    pub const CONNECT: u32 = 1;
    pub const DISCONNECT: u32 = 2;
    pub const SLOT: u32 = 3;
    pub const CHAT: u32 = 4;
}

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use nanoprobe_sidecar::ids;
use nanoprobe_sidecar::intents::{Waypoint, route_points};
use nanoprobe_sidecar::sim::wire::{
    Body, Command, Debris, Enumeration, Input, Object, ObjectGone, Position, Seen, gone_reason, lv,
    moderation, object_kind, opcode,
};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::stock::{Item, Stock};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{Class, gap_class, lifecycle as lc, record_class, result as rc};
use nanoprobe_transport::records::{
    Airbase, AuthorEvent, CommandResult, Frame, Gap, Hello, Lifecycle, Menu, Player, Rates,
    Warehouse,
};

/// Scenery objects that die with each unit or static the fake kills.
pub const SCENERY_PER_DEATH: u64 = 50;
/// Scenery object IDs start here, above any ID the fake gives a unit.
const SCENERY_BASE: u64 = 1 << 40;

/// An `EVENT` for `S_EVENT_DEAD` (8) with `INITIATOR` and
/// `INITIATOR_CATEGORY`: 1 a unit, 3 a static, 5 scenery (S9, LIVE).
fn dead_event(object: u64, category: f64) -> nanoprobe_transport::records::Event {
    nanoprobe_transport::records::Event {
        id: 8,
        fields: vec![(2, Lv::Number(object as f64)), (17, Lv::Number(category))],
    }
}

/// `campaign` if a platform name carries its tag, 0 for any other name.
fn name_campaign(name: &str, campaign: u64) -> u64 {
    match ids::parse(name) {
        Some(p) if campaign != 0 && ids::of_campaign(&p, ids::CampaignId(campaign)) => campaign,
        _ => 0,
    }
}

/// The entity a platform name gives under `campaign`, by its tag, as the
/// agent binds a row (spec/world.md, "Names"); 0 for any other name.
fn name_entity(name: &str, campaign: u64) -> u64 {
    match ids::parse(name) {
        Some(p) if name_campaign(name, campaign) != 0 => p.entity.0,
        _ => 0,
    }
}

pub use nanoprobe_transport::records::ExtensionStats;

/// The fake's settings.
#[derive(Clone, Debug)]
pub struct Config {
    /// The theater every mission loads.
    pub theater: String,
    /// Commands the agent takes per frame.
    pub commands_per_frame: usize,
    /// Rows a full enumeration visits per frame.
    pub enum_slice: usize,
    /// Frames between full enumerations.
    pub enum_period: u64,
    /// Frames between position batches.
    pub positions_every: u64,
    /// Event-class records the agent's queue holds while detached.
    pub event_queue: usize,
    /// Frames a mission load takes.
    pub load_frames: u32,
    /// Frames between a `MISSION_LOAD`'s `APPLIED` and the start of its
    /// load, in which the mission before runs on: `net.load_mission` loads
    /// at a later frame, not inside the call.
    pub load_delay_frames: u32,
    /// Meters a unit on a route moves per frame.
    pub step_m: f64,
    /// The first agent boot; each restart adds one.
    pub first_boot: u64,
    /// The longest a death event comes after a kill, in model seconds. 0
    /// sends it at once, with the row's `DEAD`. Otherwise each kill draws a
    /// delay from 0 to this, from `death_seed`.
    pub death_delay_max: f64,
    /// The seed of the death delays.
    pub death_seed: u64,
    /// Frames between `AIRBASE` and `WAREHOUSE` samples.
    pub warehouse_every: u64,
    /// Frames an enumeration with misses holds its `ENUM_END` after its last
    /// slice, as when the agent's checks of the misses span frames
    /// (spec/records.md, "Layout: ENUM_END"). 0 checks them in that slice.
    pub enum_end_hold: u64,
    /// Whether `agent.toml` allows raw Lua: `EXTENSION_SET` and `EVAL` are
    /// answered `RAW_LUA_OFF` when it does not.
    pub raw_lua: bool,
    /// The junk objects each `CLEAR_DEBRIS` reports removed.
    pub debris_per_clear: f64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theater: "Caucasus".into(),
            commands_per_frame: 4,
            enum_slice: 8,
            enum_period: 50,
            positions_every: 5,
            event_queue: 64,
            load_frames: 3,
            load_delay_frames: 0,
            step_m: 100.0,
            first_boot: 0x1000,
            death_delay_max: 0.0,
            death_seed: 0,
            warehouse_every: 5,
            enum_end_hold: 0,
            raw_lua: true,
            debris_per_clear: 3.0,
        }
    }
}

/// The coalition of each country the fake knows, answered to `COUNTRIES`:
/// country 0 is red, country 2 is blue (DCS numbering).
pub const COUNTRY_COALITIONS: [(u32, u32); 2] = [(0, 1), (2, 2)];

/// A map mark the agent holds.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeMark {
    /// A `scope` code.
    pub scope: u32,
    /// By scope: a coalition number or a group `EntityId`.
    pub target: u64,
    /// Whether players cannot change it.
    pub read_only: bool,
    /// DCS `x`, `y`, `z`.
    pub at: (f64, f64, f64),
    /// The text.
    pub text: String,
    /// The spec revision applied.
    pub rev: u64,
    /// The DCS group a `GROUP` scope named, 0 otherwise.
    pub dcs_group: u64,
}

/// A map drawing the agent holds.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeDrawing {
    /// A `scope` code.
    pub scope: u32,
    /// The coalition number, by scope.
    pub target: u64,
    /// The DCS shape ID.
    pub shape: u32,
    /// The shape's arguments.
    pub args: Lv,
    /// The spec revision applied.
    pub rev: u64,
}

/// An F10 menu item the agent holds.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeMenu {
    /// The parent's `EntityId`, 0 for the root.
    pub parent: u64,
    /// A `scope` code.
    pub scope: u32,
    /// By scope: a coalition number or a group `EntityId`.
    pub target: u64,
    /// Whether it is a submenu.
    pub submenu: bool,
    /// The text.
    pub text: String,
    /// The spec revision applied.
    pub rev: u64,
    /// The DCS group a `GROUP` scope named, 0 otherwise.
    pub dcs_group: u64,
    /// Where the item was first set: the order the shim adds items again.
    pub order: u64,
}

/// One airbase of the mission: its DCS object ID, owner and warehouse.
#[derive(Clone, Debug, PartialEq)]
struct FAirbase {
    object: u64,
    coalition: u32,
    stock: Stock,
}

/// A wreck or a dead static a test placed ([`FakeSim::place_wreck`]).
#[derive(Clone, Debug)]
struct Junk {
    name: String,
    /// DCS `x`, `y`, `z`.
    at: [f64; 3],
    dead_static: bool,
}

impl Junk {
    /// Whether it lies inside a `CLEAR_DEBRIS` volume, edges included.
    fn inside(&self, volume: &Debris) -> bool {
        match volume {
            Debris::Sphere { center, radius } => {
                let d2: f64 = (0..3).map(|i| (self.at[i] - center[i]).powi(2)).sum();
                d2 <= radius * radius
            }
            Debris::Box { min, max } => {
                (0..3).all(|i| min[i] <= self.at[i] && self.at[i] <= max[i])
            }
        }
    }
}

/// What DCS holds of one unit object ([`FakeSim::unit_states`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitTruth {
    /// The unit entity its name gives.
    pub entity: u64,
    /// The campaign its name gives.
    pub campaign: u64,
    /// DCS `x` (north), meters.
    pub x: f64,
    /// DCS `y` (altitude), meters.
    pub y: f64,
    /// DCS `z` (east), meters.
    pub z: f64,
    /// The heading it was spawned with, radians.
    pub heading: f64,
}

/// Ground truth for the tests.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    /// Unit entities that died in DCS, by the campaign their name gives.
    pub deaths: BTreeSet<(u64, u64)>,
    /// Spawns that replaced a group DCS still held under the name.
    pub replacements: u64,
    /// Of those, the spawns at a spec revision above the one the replaced
    /// group was spawned at: a changed spec spawned in place.
    pub in_place: u64,
    /// Spawns that reached the handler: agent boot, generation, group entity,
    /// spec revision, and the unit entities spawned.
    pub spawns: Vec<(u64, u32, u64, u64, Vec<u64>)>,
    /// Sequenced commands the agent took: agent boot, epoch, sequence number.
    /// The campaign each was written for.
    pub taken: BTreeMap<(u64, u64, u64), u64>,
    /// The result code the agent gave each command it answered, by agent
    /// boot, epoch and sequence number, whether or not the result was read.
    pub results: HashMap<(u64, u64, u64), u16>,
    /// Results written to the ring, which a sidecar reads: agent boot, epoch,
    /// sequence number. A result lost with the link, or cleared by a crash
    /// before it was read, is not here.
    pub delivered: BTreeSet<(u64, u64, u64)>,
    /// `OBJECT_GONE` `DEAD` records with a binding written to the ring: agent
    /// boot, and the generation, campaign and unit entity of their stamps.
    pub reported_deaths: BTreeSet<(u64, u32, u64, u64)>,
    /// Of those, the ones a sidecar read and acknowledged with a flush, keyed
    /// as [`Ledger::reported_deaths`], each with the link clock at its first
    /// such read. The link clock counts the sidecar's reads and command
    /// writes, so a command written later has a larger clock.
    pub deaths_read: BTreeMap<(u64, u32, u64, u64), u64>,
    /// Destroys of an entity that DCS refused or whose result was lost, by
    /// agent boot, generation, campaign and entity: one count per destroy,
    /// cleared whenever DCS removes the entity's group.
    pub failed_destroys: HashMap<(u64, u32, u64, u64), u32>,
    /// Unit entities that left DCS without a death: agent boot, generation,
    /// campaign, entity. A kill whose death event is late is here too: it
    /// left as `MISSING` or `REMOVED` first.
    pub removed: BTreeSet<(u64, u32, u64, u64)>,
    /// `BURST` commands taken: whether on, and whether DCS was paused.
    pub bursts: Vec<(bool, bool)>,
    /// The most commands the agent took in one frame.
    pub most_per_frame: usize,
    /// Unit entities that left DCS without a death event yet, in a
    /// generation: removed, or killed with the event late, and not revived.
    /// Agent boot, generation, campaign, entity. A group with one that the
    /// sidecar has no death fact for is never respawned in that generation.
    pub left_in: BTreeSet<(u64, u32, u64, u64)>,
    /// Unit entities whose death or removal no record names: the agent's
    /// event queue dropped it, or the agent held no binding for the row (it
    /// was made under another campaign). Agent boot, generation, campaign,
    /// entity.
    pub unreported: BTreeSet<(u64, u32, u64, u64)>,
    /// Extension chunks the agent ran: agent boot, campaign, name, revision,
    /// and how many times.
    pub extension_runs: BTreeMap<(u64, u64, String, u64), u32>,
    /// `EXTENSION_INVOKE`s the agent ran: extension name, the invoke's token
    /// (`args[1]`), and how many times. The agent boot is not in the key: an
    /// effect sent again to a new boot counts a second time.
    pub invokes: BTreeMap<(String, u64), u32>,
    /// `GROUP_OPTION`s the agent applied.
    pub options_applied: u32,
    /// `WAREHOUSE_ADJUST`s the agent applied, by airbase.
    pub adjustments: BTreeMap<String, u32>,
    /// `COUNTRIES` queries the agent answered.
    pub countries_asked: u32,
    /// `SOUND`s the agent applied: scope, target, file.
    pub sounds: Vec<(u32, u64, String)>,
    /// `PICTURE`s the agent applied, each as the `Picture` body it was sent.
    pub pictures: Vec<Body>,
    /// `PICTURE_STOP`s the agent applied.
    pub picture_stops: u32,
    /// `CLEAR_DEBRIS` volumes the agent applied, in order. Each removes
    /// `Config::debris_per_clear` junk objects, and the placed wrecks and
    /// unmanaged dead statics inside it.
    pub debris: Vec<Debris>,
    /// `MESSAGE`s the agent applied: scope, target, clear, seconds, text.
    pub messages: Vec<(u32, u64, bool, f64, String)>,
    /// `CHAT`s the agent applied: player, text.
    pub chats: Vec<(u32, String)>,
    /// `EVAL` sources the agent ran.
    pub evals: Vec<String>,
    /// `MISSION_LOAD` paths the agent took.
    pub mission_loads: Vec<String>,
    /// Extensions the agent removed, by name.
    pub extension_removals: Vec<String>,
    /// `MARK_SET`, `DRAWING_SET` and `MENU_SET` commands the agent applied
    /// (not `AT_REVISION`), by entity.
    pub pin_sets: BTreeMap<u64, u32>,
    /// `MODERATE` commands the agent applied, as sent.
    pub moderations: Vec<Body>,
    /// `PLAYERS` queries the agent answered.
    pub players_asked: u32,
    /// `BANS` queries the agent answered.
    pub bans_asked: u32,
    /// `STATIC_SPAWN`s with `dead = true` that reached the handler: agent
    /// boot, generation, entity and spec revision.
    pub ruin_spawns: Vec<(u64, u32, u64, u64)>,
    /// Of those, the ones that found a dead static under the name already:
    /// agent boot, generation, campaign, entity, whether the dead static was
    /// itself spawned dead (false for a static that died there), and whether
    /// a sidecar had read its death ([`Ledger::deaths_read`]) before the
    /// spawn was written.
    pub ruins_replaced: Vec<(u64, u32, u64, u64, bool, bool)>,
    /// Spawns of a live static under the name of a dead one.
    pub ruins_replaced_alive: u64,
}

/// A death's key in [`Ledger::reported_deaths`]: agent boot, generation,
/// campaign and unit entity.
type DeathKey = (u64, u32, u64, u64);

/// A dead static DCS holds ([`FakeSim::ruins`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Ruin {
    /// Its DCS object ID.
    pub object: u64,
    /// The entity its name gives.
    pub entity: u64,
    /// The campaign its name gives.
    pub campaign: u64,
    /// Its DCS type.
    pub type_name: String,
    /// DCS `x` (north), meters.
    pub x: f64,
    /// DCS `z` (east), meters.
    pub z: f64,
    /// Heading, radians.
    pub heading: f64,
    /// Spawned with `dead = true`; false for a static that was killed.
    pub spawned: bool,
}

#[derive(Clone, Debug)]
struct FObject {
    name: String,
    type_name: String,
    /// DCS group ID, 0 for a static.
    group: u64,
    entity: u64,
    group_entity: u64,
    /// The campaign its platform name gives, 0 for none.
    campaign: u64,
    kind: u32,
    at: Position,
}

/// How a killed row leaves the agent's table before its death event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Left {
    /// `MISSING`: the cause is unknown.
    Missing,
    /// `REMOVED`: the classifier read a removal.
    Removed,
}

/// A row that left the table as `MISSING` or `REMOVED`, which the agent
/// keeps until a death event names it.
#[derive(Clone, Debug)]
struct Late {
    due: f64,
    object: u64,
    entity: u64,
    group_entity: u64,
    category: f64,
}

#[derive(Clone, Debug)]
struct FGroup {
    name: String,
    entity: u64,
    /// The campaign its platform name gives, 0 for none.
    campaign: u64,
    members: Vec<u64>,
    route: Vec<Waypoint>,
    wp: usize,
    /// The DCS group an attack task names.
    attack: Option<u64>,
}

#[derive(Clone, Debug)]
struct Stamp {
    frame: u64,
    model_time: f64,
    mission_gen_: u32,
    lifecycle: u32,
    campaign: u64,
}

#[derive(Clone, Debug)]
struct Run {
    id: u64,
    rows: VecDeque<u64>,
    /// Rows the enumeration did not find, checked before it ends.
    misses: Vec<u64>,
    written: u32,
    /// `SEEN` IDs written.
    seen: u32,
    /// The enumeration took the `full` flag: every row is an `OBJECT`.
    full: bool,
    /// Frames its `ENUM_END` is still held for its misses.
    hold: u64,
}

/// One DCS process with the agent in it.
#[derive(Debug)]
pub struct FakeSim {
    /// Frames until a `MISSION_LOAD` already answered starts its load.
    load_after: Option<u32>,
    /// The theater a `MISSION_LOAD` loads; `None` loads the configured one.
    load_theater: Option<String>,
    cfg: Config,
    ledger: Ledger,
    // The process.
    alive: bool,
    agent_boot: u64,
    next_object: u64,
    number: u64,
    frame: u64,
    model_time: f64,
    lifecycle: u32,
    mission_gen_: u32,
    loading: u32,
    campaign: u64,
    // The agent's command state.
    epoch: u64,
    last_applied_seq: u64,
    applied_spec: HashMap<u64, u64>,
    applied_order: HashMap<u64, u64>,
    applied_extension: HashMap<String, u64>,
    // What the agent holds in DCS for the players.
    marks: BTreeMap<u64, FakeMark>,
    drawings: BTreeMap<u64, FakeDrawing>,
    menus: BTreeMap<u64, FakeMenu>,
    menu_order: u64,
    /// Records the agent writes in its next frame.
    queued_out: Vec<Input>,
    // The link.
    attached: bool,
    sidecar_boot: u64,
    inbound: VecDeque<(u64, u16, Vec<u8>)>,
    /// Commands written and held back until the sidecar's flush: the agent
    /// does not see them yet.
    held: VecDeque<(u64, u16, Vec<u8>)>,
    in_tail: u64,
    in_head: u64,
    out: Vec<RawRecord>,
    /// Records read and not yet acknowledged by the sidecar's flush: a
    /// sidecar that attaches again to this agent boot reads them again.
    unacked: Vec<RawRecord>,
    /// The link clock ([`Ledger::deaths_read`]).
    link_clock: u64,
    /// `OBJECT_GONE` `DEAD` records with a binding, by agent boot and record
    /// number: the key of [`Ledger::reported_deaths`] each reports.
    death_records: HashMap<(u64, u64), DeathKey>,
    /// Deaths read and not yet acknowledged: the record, its key, and the
    /// link clock of the read.
    deaths_unacked: Vec<((u64, u64), DeathKey, u64)>,
    /// The link clock at which each command in the ring was written, by its
    /// position.
    written_at: HashMap<u64, u64>,
    /// The link clock at which the command being handled was written.
    handling_written_at: Option<u64>,
    queue: VecDeque<(Stamp, Input)>,
    gap: Option<Gap>,
    // The world.
    groups: BTreeMap<u64, FGroup>,
    objects: BTreeMap<u64, FObject>,
    /// The agent's object table: each row's unit entity and group entity.
    table: BTreeMap<u64, (u64, u64)>,
    run: Option<Run>,
    /// Rows that left as `MISSING` or `REMOVED` and await a death event.
    late: Vec<Late>,
    /// Objects that left DCS, with their group as it was, for a revival.
    graves: BTreeMap<u64, (FObject, FGroup)>,
    /// The dead statics DCS holds, by name: a static that was killed, or one
    /// spawned with `dead = true`. The agent's table has no row for one: its
    /// `isExist` reads false (LIVE, docs/results/2026-11-17-person-fixes.md).
    ruins: BTreeMap<String, Ruin>,
    /// The state of the death delays' generator.
    rng: u64,
    /// The loaded mission's theater.
    theater: String,
    /// Drop the next `RESULT` of the next frame.
    lose_result: bool,
    /// The generation and campaign of the last `FRAME` written.
    written_stamp: (u32, u64),
    /// Destroys to answer `DCS_REFUSED`.
    refuse_destroys: u32,
    /// `DAMAGE` commands to answer `DCS_REFUSED`.
    refuse_damages: u32,
    refuse_ruins: u32,
    reject_ruins: u32,
    /// The initial life an `OBJECT` record carries, by unit entity; 1 when
    /// absent.
    life0_of: HashMap<u64, f32>,
    /// `MISSION_LOAD`s still to refuse.
    refuse_loads: u32,
    /// DCS groups that were in DCS when the active campaign changed.
    orphans: BTreeSet<u64>,
    /// Drop the next `RESULT` of this frame.
    losing: bool,
    enumerations: u64,
    next_enum: u64,
    /// Objects the enumeration does not find although they exist.
    hidden: BTreeSet<u64>,
    /// Rows whose `OBJECT` the agent queued since the last `HELLO` and has
    /// not changed: an enumeration may report them in `SEEN` records.
    sent: BTreeSet<u64>,
    /// The agent's `full` flag (spec/records.md, "Layout: SEEN").
    full: bool,
    /// The mission's airbases as the mission file has them, by name.
    airbase_seed: BTreeMap<String, FAirbase>,
    /// The next airbase object ID. It never goes down, so an airbase added
    /// after a destroy never takes the destroyed one's ID.
    next_airbase_object: u64,
    /// The airbases as DCS holds them now.
    airbases: BTreeMap<String, FAirbase>,
    /// Controller options applied, by group entity.
    options: BTreeMap<u64, BTreeMap<u32, Lv>>,
    /// Life of a unit object as a percentage of its initial life, when set.
    life: BTreeMap<u64, f64>,
    /// Wrecks and dead statics a test placed, which a `CLEAR_DEBRIS` removes.
    junk: Vec<Junk>,
    /// The heading each unit object was spawned with, radians, which a
    /// capture reads back.
    heading: BTreeMap<u64, f64>,
    /// The sample rates last received.
    rates: Rates,
    rates_received: u32,
    binds_received: u32,
    /// The DCS object an `AttackUnit` task names, by DCS group.
    attack_unit: BTreeMap<u64, u64>,
    /// Objects whose destroy is pending until the resume, by object ID.
    pending: BTreeMap<u64, FObject>,
    /// The restore burst (spec/commands.md, `BURST`).
    burst: bool,
    /// The clients connected to the server, by DCS connection ID.
    clients: BTreeMap<u32, FakeClient>,
    /// The server's ban list. It outlives the DCS process, as DCS's does.
    bans: Vec<FakeBan>,
    /// The objects the `SUBSCRIBE` last received named.
    subscribed: BTreeSet<u64>,
}

/// The wall-clock second the fake's ban times count from.
pub const FAKE_WALL_NOW: f64 = 1_800_000_000.0;

/// A client connected to the server.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeClient {
    /// The ucid DCS gives the client.
    pub ucid: String,
    /// The name.
    pub name: String,
    /// The coalition number, 0 for none.
    pub side: u32,
    /// The slot ID, empty for spectators.
    pub slot: String,
}

/// A ban list entry.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeBan {
    /// The banned ucid.
    pub ucid: String,
    /// The name the player had, empty for a ucid never seen.
    pub name: String,
    /// When the ban ends, seconds since the Unix epoch; 0 for none given.
    pub banned_until: f64,
    /// The reason.
    pub reason: String,
}

impl FakeSim {
    /// A DCS process that has started and is loading its first mission. No
    /// sidecar is attached.
    pub fn new(cfg: Config) -> FakeSim {
        let boot = cfg.first_boot;
        let cfg_seed = cfg.death_seed;
        let mut f = FakeSim {
            load_after: None,
            load_theater: None,
            cfg,
            ledger: Ledger::default(),
            alive: true,
            agent_boot: boot,
            next_object: 1,
            number: 0,
            frame: 0,
            model_time: 0.0,
            lifecycle: lc::STARTING as u32,
            mission_gen_: 0,
            loading: 0,
            campaign: 0,
            epoch: 0,
            last_applied_seq: 0,
            applied_spec: HashMap::new(),
            applied_extension: HashMap::new(),
            marks: BTreeMap::new(),
            drawings: BTreeMap::new(),
            menus: BTreeMap::new(),
            menu_order: 0,
            queued_out: Vec::new(),
            applied_order: HashMap::new(),
            attached: false,
            sidecar_boot: 0,
            inbound: VecDeque::new(),
            held: VecDeque::new(),
            in_tail: 0,
            in_head: 0,
            out: Vec::new(),
            unacked: Vec::new(),
            link_clock: 0,
            death_records: HashMap::new(),
            deaths_unacked: Vec::new(),
            written_at: HashMap::new(),
            handling_written_at: None,
            queue: VecDeque::new(),
            gap: None,
            groups: BTreeMap::new(),
            objects: BTreeMap::new(),
            table: BTreeMap::new(),
            run: None,
            late: Vec::new(),
            graves: BTreeMap::new(),
            ruins: BTreeMap::new(),
            rng: cfg_seed,
            theater: String::new(),
            lose_result: false,
            written_stamp: (0, 0),
            refuse_destroys: 0,
            refuse_damages: 0,
            refuse_ruins: 0,
            reject_ruins: 0,
            life0_of: HashMap::new(),
            refuse_loads: 0,
            orphans: BTreeSet::new(),
            losing: false,
            enumerations: 0,
            next_enum: 0,
            hidden: BTreeSet::new(),
            sent: BTreeSet::new(),
            full: true,
            airbase_seed: BTreeMap::new(),
            next_airbase_object: 1_000_000,
            airbases: BTreeMap::new(),
            options: BTreeMap::new(),
            life: BTreeMap::new(),
            junk: Vec::new(),
            heading: BTreeMap::new(),
            rates: Rates::default(),
            rates_received: 0,
            binds_received: 0,
            attack_unit: BTreeMap::new(),
            pending: BTreeMap::new(),
            burst: false,
            clients: BTreeMap::new(),
            bans: Vec::new(),
            subscribed: BTreeSet::new(),
        };
        f.load_mission();
        f
    }

    /// Add an airbase to the mission, as the mission file has it. It is in
    /// DCS now and after each mission load.
    pub fn add_airbase(&mut self, name: &str, coalition: u32, stock: Stock) {
        let object = self.next_airbase_object;
        self.next_airbase_object += 1;
        let a = FAirbase {
            object,
            coalition,
            stock,
        };
        self.airbase_seed.insert(name.to_owned(), a.clone());
        self.airbases.insert(name.to_owned(), a);
    }

    /// A FARP static that made an airbase is destroyed: the airbase goes
    /// with it, and the agent reports the static's object, which is the
    /// airbase's, gone. A later `add_airbase` of the name is the FARP
    /// spawned again, with a new object.
    pub fn destroy_farp(&mut self, name: &str) {
        if let Some(a) = self.airbases.remove(name) {
            self.airbase_seed.remove(name);
            self.queued_out.push(Input::ObjectGone(ObjectGone {
                object: a.object,
                reason: gone_reason::DESTROYED,
                entity: 0,
                group_entity: 0,
            }));
        }
    }

    /// DCS captures an airbase: its owner changes without a command.
    pub fn capture_airbase(&mut self, name: &str, coalition: u32) {
        if let Some(a) = self.airbases.get_mut(name) {
            a.coalition = coalition;
        }
    }

    /// DCS uses stock: an item's count changes without a command.
    pub fn use_stock(&mut self, name: &str, item: Item, count: f64) {
        if let Some(a) = self.airbases.get_mut(name) {
            a.stock.set(item, count);
        }
    }

    /// The object ID of an airbase in DCS.
    pub fn airbase_object(&self, name: &str) -> Option<u64> {
        self.airbases.get(name).map(|a| a.object)
    }

    /// The owner of an airbase in DCS.
    pub fn airbase_owner(&self, name: &str) -> Option<u32> {
        self.airbases.get(name).map(|a| a.coalition)
    }

    /// The stock of an airbase in DCS.
    pub fn airbase_stock(&self, name: &str) -> Option<&Stock> {
        self.airbases.get(name).map(|a| &a.stock)
    }

    /// The value of a controller option DCS holds for a group entity.
    pub fn option_of(&self, entity: u64, option: u32) -> Option<&Lv> {
        self.options.get(&entity)?.get(&option)
    }

    /// Leave a wreck at a DCS point (`x`, `y`, `z`). The fake has no terrain:
    /// a test puts the wreck at the height its ground has.
    pub fn place_wreck(&mut self, name: &str, at: [f64; 3]) {
        self.junk.push(Junk {
            name: name.into(),
            at,
            dead_static: false,
        });
    }

    /// Leave a dead static at a DCS point. One whose name starts with `np#`
    /// is the platform's and stays through every clear (spec/commands.md,
    /// "Layout: CLEAR_DEBRIS"). The fake copies that rule from the agent's
    /// handler, so a test on the fake shows nothing of the handler's own
    /// rule; `lua/tests/dispatch.lua clear-debris` tests that.
    pub fn place_dead_static(&mut self, name: &str, at: [f64; 3]) {
        self.junk.push(Junk {
            name: name.into(),
            at,
            dead_static: true,
        });
    }

    /// The names of the placed wrecks and dead statics no clear has removed.
    pub fn junk_left(&self) -> Vec<&str> {
        self.junk.iter().map(|j| j.name.as_str()).collect()
    }

    /// The life, as a percentage of the initial life, of a unit entity's
    /// object, when a `DAMAGE` set it.
    pub fn life_percent_of(&self, entity: u64) -> Option<f64> {
        let (o, _) = self.objects.iter().find(|(_, x)| x.entity == entity)?;
        self.life.get(o).copied()
    }

    /// Turn a unit entity's object to `heading`, radians, as DCS would when
    /// it drives. False when DCS holds no object of it.
    pub fn set_heading(&mut self, entity: u64, heading: f64) -> bool {
        let Some(o) = self.object_of_entity(entity) else {
            return false;
        };
        self.heading.insert(o, heading);
        true
    }

    /// Set a unit entity's life, as a percentage of its initial life, as a
    /// hit in DCS would. False when DCS holds no object of it.
    pub fn set_life_percent(&mut self, entity: u64, percent: f64) -> bool {
        let Some(o) = self.object_of_entity(entity) else {
            return false;
        };
        self.life.insert(o, percent);
        true
    }

    fn object_of_entity(&self, entity: u64) -> Option<u64> {
        self.objects
            .iter()
            .find(|(_, x)| x.entity == entity && entity != 0)
            .map(|(o, _)| *o)
    }

    /// The unit entities in DCS that a `DAMAGE` set, with the percentage of
    /// their initial life.
    pub fn lives(&self) -> Vec<(u64, f64)> {
        self.objects
            .iter()
            .filter_map(|(o, x)| Some((x.entity, *self.life.get(o)?)))
            .collect()
    }

    /// The DCS object the group of an entity was told to attack with an
    /// `AttackUnit` task.
    pub fn attack_unit_of(&self, campaign: u64, entity: u64) -> Option<u64> {
        let gid = self.group_in(campaign, entity)?;
        self.attack_unit.get(&gid).copied()
    }

    /// The sample rates the agent last received, and how many `RATES`
    /// commands it took.
    pub fn rates(&self) -> (Rates, u32) {
        (self.rates, self.rates_received)
    }

    /// `BIND` commands the agent took.
    pub fn binds(&self) -> u32 {
        self.binds_received
    }

    /// The mark the agent holds for an entity.
    pub fn mark(&self, entity: u64) -> Option<&FakeMark> {
        self.marks.get(&entity)
    }

    /// Every mark the agent holds, by entity.
    pub fn marks(&self) -> &BTreeMap<u64, FakeMark> {
        &self.marks
    }

    /// The drawing the agent holds for an entity.
    pub fn drawing(&self, entity: u64) -> Option<&FakeDrawing> {
        self.drawings.get(&entity)
    }

    /// Every drawing the agent holds, by entity.
    pub fn drawings(&self) -> &BTreeMap<u64, FakeDrawing> {
        &self.drawings
    }

    /// The menu item the agent holds for an entity.
    pub fn menu_item(&self, entity: u64) -> Option<&FakeMenu> {
        self.menus.get(&entity)
    }

    /// Every menu item the agent holds, in the order they were first set.
    pub fn menu_items(&self) -> Vec<(u64, &FakeMenu)> {
        let mut v: Vec<_> = self.menus.iter().map(|(e, m)| (*e, m)).collect();
        v.sort_by_key(|(_, m)| m.order);
        v
    }

    /// The revision of an extension the agent holds, 0 for none.
    pub fn extension_rev(&self, name: &str) -> u64 {
        self.applied_extension.get(name).copied().unwrap_or(0)
    }

    /// A player selects a command menu item. The agent writes a `MENU`
    /// record in its next frame. False for an item that is not a command
    /// the agent holds.
    pub fn select_menu(&mut self, entity: u64, player: u32) -> bool {
        let Some(m) = self.menus.get(&entity).filter(|m| !m.submenu) else {
            return false;
        };
        let (target, scope) = (m.target, m.scope);
        self.queued_out.push(Input::Menu(Menu {
            entity,
            target,
            scope,
            player,
        }));
        true
    }

    /// An extension calls `emit`: the agent writes an `AUTHOR_EVENT` in its
    /// next frame, with DCS object IDs.
    pub fn emit_author_event(&mut self, objects: &[u64], name: &str, payload: Lv) {
        self.queued_out.push(Input::AuthorEvent(AuthorEvent {
            objects: objects.to_vec(),
            name: name.to_owned(),
            payload,
        }));
    }

    /// The agent writes a `PLAYER` record in its next frame: a chat line.
    pub fn emit_player_record(&mut self) {
        self.queued_out.push(Input::Player(Player {
            event: 4,
            player: 2,
            side: 2,
            seat: 0,
            client: "client".into(),
            name: "pilot".into(),
            slot: "slot".into(),
            text: "hello".into(),
        }));
    }

    /// The agent writes an `EXTENSION` record in its next frame.
    pub fn emit_extension_stats(&mut self, stats: ExtensionStats) {
        self.queued_out.push(Input::Extension(stats));
    }

    /// A client connects: the hook's `onPlayerConnect` writes a `PLAYER`
    /// record in the next frame. False for a banned ucid, which DCS
    /// refuses.
    pub fn connect_client(&mut self, id: u32, ucid: &str, name: &str, side: u32) -> bool {
        if self.bans.iter().any(|b| b.ucid == ucid) {
            return false;
        }
        self.clients.insert(
            id,
            FakeClient {
                ucid: ucid.to_owned(),
                name: name.to_owned(),
                side,
                slot: String::new(),
            },
        );
        self.player_record(pe::CONNECT, id, "");
        true
    }

    /// A client DCS already holds when the sidecar attaches: no `PLAYER`
    /// record is written, so only a `PLAYERS` query finds it.
    pub fn seed_client(&mut self, id: u32, ucid: &str, name: &str, side: u32, slot: &str) {
        self.clients.insert(
            id,
            FakeClient {
                ucid: ucid.to_owned(),
                name: name.to_owned(),
                side,
                slot: slot.to_owned(),
            },
        );
    }

    /// A client takes a slot (`onPlayerChangeSlot`); an empty `slot` is
    /// spectators.
    pub fn client_slot(&mut self, id: u32, side: u32, slot: &str) {
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        c.side = side;
        c.slot = slot.to_owned();
        self.player_record(pe::SLOT, id, "");
    }

    /// A client changes its name. DCS has no callback for it: the sidecar
    /// sees the new name in the client's next `CONNECT` or `SLOT` record.
    pub fn rename_client(&mut self, id: u32, name: &str) {
        if let Some(c) = self.clients.get_mut(&id) {
            c.name = name.to_owned();
        }
    }

    /// A client sends a chat line (`onPlayerTrySendChat`).
    pub fn client_chat(&mut self, id: u32, text: &str) {
        if self.clients.contains_key(&id) {
            self.player_record(pe::CHAT, id, text);
        }
    }

    /// A client leaves (`onPlayerDisconnect`).
    pub fn disconnect_client(&mut self, id: u32) {
        if self.clients.contains_key(&id) {
            self.player_record(pe::DISCONNECT, id, "");
            self.clients.remove(&id);
        }
    }

    /// The clients connected now.
    pub fn clients(&self) -> &BTreeMap<u32, FakeClient> {
        &self.clients
    }

    /// The ban list.
    pub fn bans(&self) -> &[FakeBan] {
        &self.bans
    }

    /// The objects the last `SUBSCRIBE` named that the agent holds.
    pub fn subscribed(&self) -> &BTreeSet<u64> {
        &self.subscribed
    }

    /// A client unit the mission file holds appears: a new object in no
    /// campaign, its `OBJECT` record and an `S_EVENT_BIRTH` with the
    /// mission ID (`INITIATOR_MISSION_ID`) in the next frame. Returns the
    /// DCS object ID.
    pub fn birth_client_unit(&mut self, mission_id: &str) -> u64 {
        self.birth_client_unit_in(mission_id, 0, "F-16C_50")
    }

    /// As [`FakeSim::birth_client_unit`], in the DCS group `group` (0 for
    /// none), of the type `type_name`.
    pub fn birth_client_unit_in(&mut self, mission_id: &str, group: u64, type_name: &str) -> u64 {
        let id = self.place_client_unit_in(mission_id, group, type_name);
        self.birth_event(id, Some(mission_id));
        id
    }

    /// As [`FakeSim::birth_client_unit_in`] without the `S_EVENT_BIRTH`: the
    /// unit's `OBJECT` record is queued and the birth is sent later with
    /// [`FakeSim::birth_event`]. A record queued between the two reaches the
    /// sidecar before the birth that seats the player.
    pub fn place_client_unit_in(&mut self, mission_id: &str, group: u64, type_name: &str) -> u64 {
        let id = self.next_object;
        self.next_object += 1;
        self.objects.insert(
            id,
            FObject {
                name: format!("client-{mission_id}"),
                type_name: type_name.to_owned(),
                group,
                entity: 0,
                group_entity: 0,
                campaign: 0,
                kind: object_kind::UNIT,
                at: Position {
                    object: id,
                    ..Position::default()
                },
            },
        );
        self.table.insert(id, (0, 0));
        let o = self.object_record(id, 0);
        self.sent.insert(id);
        self.queued_out.push(o);
        id
    }

    /// An `S_EVENT_BIRTH` for an object the fake holds. With no mission ID
    /// the event carries none, as DCS may send it.
    pub fn birth_event(&mut self, object: u64, mission_id: Option<&str>) {
        let mut fields = vec![
            (1, Lv::Number(self.model_time)),
            (2, Lv::Number(object as f64)),
        ];
        if let Some(m) = mission_id {
            fields.push((21, lv::s(m)));
        }
        self.queued_out
            .push(Input::Event(nanoprobe_transport::records::Event {
                id: 15,
                fields,
            }));
    }

    /// A world event with the fields given, each an `event_field` code.
    pub fn emit_event(&mut self, id: u32, fields: Vec<(u16, Lv)>) {
        self.queued_out
            .push(Input::Event(nanoprobe_transport::records::Event {
                id,
                fields,
            }));
    }

    /// The DCS entity binding of an object: its unit and group `EntityId`s
    /// the agent holds, 0 for none.
    pub fn binding_of(&self, object: u64) -> Option<(u64, u64)> {
        self.table.get(&object).copied()
    }

    /// `BIND`: an object or group whose row is in the table takes the
    /// entity; any other pair is ignored. A bound row is written whole by
    /// the next enumeration.
    fn bind(&mut self, b: &nanoprobe_transport::records::Bind) {
        for (entity, object) in b.entity.iter().zip(&b.object) {
            if b.kind == object_kind::GROUP {
                let rows: Vec<u64> = self
                    .objects
                    .iter()
                    .filter(|(o, x)| x.group == *object && self.table.contains_key(*o))
                    .map(|(o, _)| *o)
                    .collect();
                for o in rows {
                    self.objects.get_mut(&o).expect("row").group_entity = *entity;
                    if let Some(t) = self.table.get_mut(&o) {
                        t.1 = *entity;
                    }
                    self.sent.remove(&o);
                }
            } else if self.table.contains_key(object)
                && let Some(x) = self.objects.get_mut(object)
            {
                x.entity = *entity;
                if let Some(t) = self.table.get_mut(object) {
                    t.0 = *entity;
                }
                self.sent.remove(object);
            }
        }
    }

    /// A world event with an initiator and a target, by DCS object ID.
    pub fn event_between(&mut self, id: u32, initiator: Option<u64>, target: Option<u64>) {
        let mut fields = Vec::new();
        if let Some(i) = initiator {
            fields.push((2, Lv::Number(i as f64)));
        }
        if let Some(t) = target {
            fields.push((3, Lv::Number(t as f64)));
        }
        self.queued_out
            .push(Input::Event(nanoprobe_transport::records::Event {
                id,
                fields,
            }));
    }

    /// A vitals row for an object, in the next frame.
    pub fn emit_vitals(&mut self, object: u64, life: f32) {
        self.queued_out
            .push(Input::Vitals(vec![nanoprobe_sidecar::sim::wire::Vitals {
                object,
                life,
            }]));
    }

    /// An `AMMO` record for an object, in the next frame: what the agent
    /// writes for a player-occupied unit at its birth (`cause` 1) or its
    /// takeoff (2). `rows` are `getAmmo` type names and counts.
    pub fn emit_ammo(&mut self, object: u64, cause: u32, rows: &[(&str, f64)]) {
        let ammo = Lv::Table(
            rows.iter()
                .enumerate()
                .map(|(i, (t, c))| {
                    (
                        Lv::Number((i + 1) as f64),
                        Lv::Table(vec![
                            (lv::s("type"), lv::s(t)),
                            (lv::s("count"), Lv::Number(*c)),
                        ]),
                    )
                })
                .collect(),
        );
        self.queued_out
            .push(Input::Ammo(nanoprobe_transport::records::Ammo {
                object,
                cause,
                ammo,
            }));
    }

    fn player_record(&mut self, event: u32, id: u32, text: &str) {
        let Some(c) = self.clients.get(&id) else {
            return;
        };
        let seat = nanoprobe_sidecar::players::seat_of(&c.slot);
        self.queued_out.push(Input::Player(Player {
            event,
            player: id,
            side: c.side,
            seat,
            client: c.ucid.clone(),
            name: c.name.clone(),
            slot: c.slot.clone(),
            text: text.to_owned(),
        }));
    }

    /// The agent's `MODERATE` handler (spec/commands.md, "Moderation").
    fn moderate(&mut self, cmd: &Command) {
        let Body::Moderate {
            action,
            player,
            side,
            seconds,
            ucid,
            slot,
            reason,
        } = &cmd.body
        else {
            return;
        };
        if !(moderation::KICK..=moderation::SLOT).contains(action)
            || ucid.is_empty()
            || !seconds.is_finite()
            || *seconds < 0.0
            || *player == 1
        {
            return self.result(cmd, rc::BAD_PAYLOAD, 0, "", Lv::Nil);
        }
        let names_player = *player != 0 && *action != moderation::UNBAN;
        if names_player && self.clients.get(player).is_none_or(|c| c.ucid != *ucid) {
            return self.result(cmd, rc::NOT_BOUND, 0, "no such player", Lv::Nil);
        }
        match *action {
            moderation::KICK => self.disconnect_client(*player),
            moderation::BAN => {
                let name = self
                    .clients
                    .get(player)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                self.bans.retain(|b| b.ucid != *ucid);
                self.bans.push(FakeBan {
                    ucid: ucid.clone(),
                    name,
                    banned_until: if *seconds > 0.0 {
                        FAKE_WALL_NOW + *seconds
                    } else {
                        0.0
                    },
                    reason: reason.clone(),
                });
                self.disconnect_client(*player);
            }
            moderation::UNBAN => {
                let before = self.bans.len();
                self.bans.retain(|b| b.ucid != *ucid);
                if self.bans.len() == before {
                    return self.result(cmd, rc::DCS_REFUSED, 0, "DCS returned false", Lv::Nil);
                }
            }
            _ => {
                if let Some(c) = self.clients.get_mut(player) {
                    c.side = *side;
                    c.slot = slot.clone();
                }
                self.player_record(pe::SLOT, *player, "");
            }
        }
        self.ledger.moderations.push(cmd.body.clone());
        self.result(cmd, rc::APPLIED, 0, "", Lv::Nil);
    }

    /// The ground truth.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Forget what the ledger recorded and the dead objects kept for a
    /// revival, for a long run that checks no invariant from them and must
    /// not grow with them.
    pub fn clear_ledger(&mut self) {
        self.ledger = Ledger::default();
        self.graves.clear();
    }

    /// The agent's boot.
    pub fn agent_boot(&self) -> u64 {
        self.agent_boot
    }

    /// The mission generation.
    pub fn mission_gen(&self) -> u32 {
        self.mission_gen_
    }

    /// Whether a sidecar is attached.
    pub fn attached(&self) -> bool {
        self.attached
    }

    /// Whether the mission runs (not loading).
    pub fn running(&self) -> bool {
        self.lifecycle == lc::RUNNING as u32
    }

    /// The groups and statics of a campaign's platform names that DCS holds:
    /// entity, platform name and the unit entities alive in it.
    pub fn platform_groups(&self, campaign: u64) -> Vec<(u64, String, Vec<u64>)> {
        self.groups
            .values()
            .filter(|g| g.entity != 0 && g.campaign == campaign)
            .map(|g| {
                let units = g.members.iter().map(|o| self.objects[o].entity).collect();
                (g.entity, g.name.clone(), units)
            })
            .collect()
    }

    /// The dead statics of a campaign's platform names that DCS holds.
    pub fn ruins(&self, campaign: u64) -> Vec<&Ruin> {
        self.ruins
            .values()
            .filter(|r| r.entity != 0 && r.campaign == campaign)
            .collect()
    }

    /// The loaded mission's theater.
    pub fn theater(&self) -> &str {
        &self.theater
    }

    /// The DCS group of a group entity, and the DCS group its attack task
    /// names, if it has one.
    pub fn attack_of(&self, campaign: u64, entity: u64) -> Option<(u64, Option<u64>)> {
        let gid = self.group_in(campaign, entity)?;
        Some((gid, self.groups[&gid].attack))
    }

    /// The order revision the agent holds for a group entity.
    pub fn applied_order(&self, entity: u64) -> u64 {
        self.applied_order.get(&entity).copied().unwrap_or(0)
    }

    /// The binding the agent holds for a row: `(entity, group entity)`, 0s
    /// for a row it does not bind or does not hold.
    pub fn binding(&self, object: u64) -> (u64, u64) {
        self.table.get(&object).copied().unwrap_or((0, 0))
    }

    /// Each unit object alive in the world, keyed by its unit entity (0
    /// for unmanaged ones left out): its campaign, position and spawn
    /// heading.
    pub fn unit_states(&self) -> Vec<UnitTruth> {
        self.objects
            .iter()
            .filter(|(_, x)| x.entity != 0)
            .map(|(o, x)| UnitTruth {
                entity: x.entity,
                campaign: x.campaign,
                x: x.at.x,
                y: x.at.y,
                z: x.at.z,
                heading: self.heading.get(o).copied().unwrap_or(0.0),
            })
            .collect()
    }

    /// The DCS objects alive in the world: object ID, the campaign its name
    /// gives and the unit entity it carries (0 and 0 for an unmanaged one).
    pub fn objects(&self) -> Vec<(u64, u64, u64)> {
        self.objects
            .iter()
            .map(|(o, x)| (*o, x.campaign, x.entity))
            .collect()
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            frame: self.frame,
            model_time: self.model_time,
            mission_gen_: self.mission_gen_,
            lifecycle: self.lifecycle,
            campaign: self.campaign,
        }
    }

    fn write(&mut self, input: &Input) {
        match input {
            Input::Frame(f) => self.written_stamp = (f.mission_gen, f.campaign),
            Input::Result(r) if r.seq != 0 => {
                self.ledger
                    .delivered
                    .insert((self.agent_boot, r.epoch, r.seq));
            }
            Input::ObjectGone(g) if g.reason == gone_reason::DEAD && g.entity != 0 => {
                let (mission_gen, campaign) = self.written_stamp;
                let key = (self.agent_boot, mission_gen, campaign, g.entity);
                self.ledger.reported_deaths.insert(key);
                self.death_records
                    .insert((self.agent_boot, self.number + 1), key);
            }
            _ => {}
        }
        let (kind, payload) = input.encode();
        self.number += 1;
        self.out.push(RawRecord {
            agent_boot: self.agent_boot,
            number: self.number,
            kind,
            payload,
            resync: false,
        });
    }

    fn frame_record(&mut self, s: &Stamp, replay: bool) {
        let f = Frame {
            frame: s.frame,
            model_time: s.model_time,
            real_time: s.model_time,
            campaign: s.campaign,
            number: self.number + 2,
            mission_gen: s.mission_gen_,
            lifecycle: s.lifecycle,
            replay: u32::from(replay),
        };
        self.write(&Input::Frame(f));
    }

    /// Emit a record: written when attached, else queued by its class.
    fn emit(&mut self, input: Input) {
        if let Input::Object(o) = &input {
            self.sent.insert(o.object);
        }
        if self.losing
            && let Input::Result(r) = &input
        {
            // The result is lost and the link drops with it; the next
            // attach's HELLO settles the command.
            self.destroy_result_lost(r);
            self.losing = false;
            self.attached = false;
            return;
        }
        if self.attached {
            self.write(&input);
            return;
        }
        let (kind, _) = input.encode();
        match record_class(u64::from(kind)) {
            Some(Class::Sample) | None => {}
            Some(Class::Control) => self.queue.push_back((self.stamp(), input)),
            Some(Class::Event) => {
                self.queue.push_back((self.stamp(), input));
                let events = self
                    .queue
                    .iter()
                    .filter(|(_, i)| record_class(u64::from(i.encode().0)) == Some(Class::Event))
                    .count();
                if events > self.cfg.event_queue {
                    let at = self
                        .queue
                        .iter()
                        .position(|(_, i)| {
                            record_class(u64::from(i.encode().0)) == Some(Class::Event)
                        })
                        .expect("an event");
                    let (s, dropped) = self.queue.remove(at).expect("queued");
                    // A dropped record: the next enumeration is whole.
                    self.full = true;
                    if let Input::ObjectGone(g) = dropped {
                        self.ledger.unreported.insert((
                            self.agent_boot,
                            s.mission_gen_,
                            s.campaign,
                            g.entity,
                        ));
                    }
                    let g = self.gap.get_or_insert(Gap {
                        class: gap_class::EVENTS as u32,
                        count: 0,
                        from_real: s.model_time,
                        to_real: s.model_time,
                        from_model: s.model_time,
                        to_model: s.model_time,
                        from_gen: s.mission_gen_,
                        to_gen: s.mission_gen_,
                    });
                    g.count += 1;
                    g.to_real = s.model_time;
                    g.to_model = s.model_time;
                    g.to_gen = s.mission_gen_;
                }
            }
        }
    }

    fn object_record(&self, id: u64, enumeration: u64) -> Input {
        let o = &self.objects[&id];
        let group_name = self
            .groups
            .get(&o.group)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        Input::Object(Object {
            object: id,
            group: o.group,
            entity: o.entity,
            group_entity: o.group_entity,
            enumeration,
            kind: o.kind,
            category: 2,
            coalition: 2,
            country: 2,
            life0: self.life0_of.get(&o.entity).copied().unwrap_or(1.0),
            name: o.name.clone(),
            type_name: o.type_name.clone(),
            group_name: if o.kind == object_kind::STATIC {
                String::new()
            } else {
                group_name
            },
        })
    }

    fn lifecycle_record(&mut self) {
        let theater = if self.mission_gen_ == 0 {
            String::new()
        } else {
            self.theater.clone()
        };
        self.emit(Input::Lifecycle(Lifecycle {
            state: self.lifecycle,
            mission_gen: self.mission_gen_,
            theater,
            mission: if self.mission_gen_ == 0 {
                String::new()
            } else {
                "shell.miz".into()
            },
        }));
    }

    // ---- faults and world control -------------------------------------

    /// Start a mission load: every object leaves DCS and the agent's table
    /// without a record, the agent forgets the binding and applied
    /// revisions, and the mission runs after `load_frames` frames.
    pub fn load_mission(&mut self) {
        let t = self.cfg.theater.clone();
        self.load_mission_on(&t);
    }

    /// Pause the simulation: model time and movement stop, the agent still
    /// runs its pump. Only a running mission pauses.
    pub fn pause(&mut self) {
        if self.alive && self.lifecycle == lc::RUNNING as u32 {
            self.lifecycle = lc::PAUSED as u32;
            self.lifecycle_record();
        }
    }

    /// Resume a paused simulation: the objects whose destroy is pending go,
    /// with no event, and the burst turns off.
    pub fn resume(&mut self) {
        if self.alive && self.paused() {
            self.lifecycle = lc::RUNNING as u32;
            self.pending.clear();
            self.burst = false;
            self.lifecycle_record();
        }
    }

    /// Whether the simulation is paused.
    pub fn paused(&self) -> bool {
        self.lifecycle == lc::PAUSED as u32
    }

    /// Objects whose destroy waits for the resume: object ID and unit
    /// entity.
    pub fn pending_destroys(&self) -> Vec<(u64, u64)> {
        self.pending.iter().map(|(o, x)| (*o, x.entity)).collect()
    }

    /// Whether the restore burst is on.
    pub fn burst(&self) -> bool {
        self.burst
    }

    /// Make a `MISSION_LOAD` load `theater`, as a shell for it would; `None`
    /// loads the configured theater.
    pub fn set_load_theater(&mut self, theater: Option<String>) {
        self.load_theater = theater;
    }

    /// The load a `MISSION_LOAD` starts.
    fn load_commanded(&mut self) {
        match self.load_theater.clone() {
            Some(t) => self.load_mission_on(&t),
            None => self.load_mission(),
        }
    }

    /// Start a mission load on another theater.
    pub fn load_mission_on(&mut self, theater: &str) {
        if !self.alive {
            return;
        }
        self.pending.clear();
        self.burst = false;
        self.theater = theater.to_owned();
        self.mission_gen_ += 1;
        self.groups.clear();
        self.orphans.clear();
        self.objects.clear();
        self.table.clear();
        self.sent.clear();
        self.run = None;
        // A generation change clears the agent's memory of rows that left.
        self.late.clear();
        self.graves.clear();
        self.ruins.clear();
        self.applied_spec.clear();
        self.applied_order.clear();
        // Marks, drawings and menu items are gone with the mission.
        self.marks.clear();
        self.drawings.clear();
        self.menus.clear();
        // The mission file's airbases, owners and stock; no option or life
        // survives its group.
        self.airbases = self.airbase_seed.clone();
        self.options.clear();
        self.life.clear();
        self.lifecycle = lc::LOADING as u32;
        self.loading = self.cfg.load_frames.max(1);
        self.lifecycle_record();
        // A Mission Editor group the platform did not create.
        self.add_group(
            "Ground-1",
            0,
            &["M-113", "M-113"],
            0,
            false,
            Position::default(),
        );
    }

    /// DCS dies and the host starts it again: a new agent boot with empty
    /// memory. Records not yet read and commands not yet taken are lost. No
    /// sidecar is attached afterwards.
    pub fn restart_dcs(&mut self) {
        let boot = self.agent_boot + 1;
        let cfg = self.cfg.clone();
        let ledger = std::mem::take(&mut self.ledger);
        let bans = std::mem::take(&mut self.bans);
        // The mission's units keep their initial life.
        let life0_of = std::mem::take(&mut self.life0_of);
        *self = FakeSim::new(Config {
            first_boot: boot,
            ..cfg
        });
        self.ledger = ledger;
        self.bans = bans;
        self.life0_of = life0_of;
    }

    /// DCS dies and stays down: nothing runs, nothing is read, and records
    /// not yet read are lost. [`FakeSim::restart_dcs`] starts it again.
    pub fn crash(&mut self) {
        self.alive = false;
        self.attached = false;
        // What was written and not read is lost: take it out of the ledger.
        let mut stamp = (0, 0);
        for r in std::mem::take(&mut self.out) {
            match Input::decode(r.kind, &r.payload) {
                Ok(Input::Frame(f)) => stamp = (f.mission_gen, f.campaign),
                Ok(Input::Result(res)) => {
                    self.ledger
                        .delivered
                        .remove(&(r.agent_boot, res.epoch, res.seq));
                    self.destroy_result_lost(&res);
                }
                Ok(Input::ObjectGone(g)) if g.reason == gone_reason::DEAD => {
                    self.ledger
                        .reported_deaths
                        .remove(&(r.agent_boot, stamp.0, stamp.1, g.entity));
                }
                _ => {}
            }
        }
    }

    /// Count a destroy whose result will not be read.
    fn destroy_result_lost(&mut self, r: &CommandResult) {
        // A refused destroy was counted when DCS refused it.
        if matches!(r.opcode, opcode::GROUP_DESTROY | opcode::STATIC_DESTROY)
            && u64::from(r.code) != rc::DCS_REFUSED
        {
            let key = (self.agent_boot, self.mission_gen_, r.campaign, r.entity);
            *self.ledger.failed_destroys.entry(key).or_default() += 1;
        }
    }

    /// Whether the process is up.
    pub fn alive(&self) -> bool {
        self.alive
    }

    /// The next `RESULT` written in the next frame is lost, and the link
    /// drops with it. Its command
    /// stays unanswered until the next attach's `HELLO`. (A `RESULT` lost to
    /// `CONTROL_LOST` and its resync `HELLO` are not modeled.)
    pub fn lose_next_result(&mut self) {
        self.lose_result = true;
    }

    /// Take at most `n` commands a frame from now on; 0 stalls the ring.
    pub fn set_commands_per_frame(&mut self, n: usize) {
        self.cfg.commands_per_frame = n;
    }

    /// The agent's active campaign changes, as a `CAMPAIGN` command from a
    /// sidecar of another campaign does: the agent discards the binding and
    /// the applied revisions, and binds again only the rows whose platform
    /// names give the new campaign (spec/commands.md, `CAMPAIGN`).
    pub fn set_campaign(&mut self, c: u64) {
        if c == self.campaign {
            return;
        }
        self.campaign = c;
        self.applied_spec.clear();
        self.applied_order.clear();
        self.applied_extension.clear();
        self.marks.clear();
        self.drawings.clear();
        self.menus.clear();
        // The binding the kept rows were made under is gone: a death event
        // after the switch names no entity of either campaign.
        self.late.clear();
        // Groups in DCS whose applied revision the agent no longer holds: a
        // spawn under one of their names replaces it, as spec/commands.md
        // allows between a switch and the next load.
        self.orphans.extend(self.groups.keys().copied());
        // Every row's payload is encoded again under the new binding.
        self.sent.clear();
        // The agent binds a row again from its names, by the campaign's tag.
        for (object, b) in self.table.iter_mut() {
            *b = match self.objects.get(object) {
                Some(o) => (
                    name_entity(&o.name, c),
                    self.groups
                        .get(&o.group)
                        .map_or(0, |g| name_entity(&g.name, c)),
                ),
                None => (0, 0),
            };
        }
        // A row bound by `BIND`, which no platform name binds, loses it too.
        for o in self.objects.values_mut().filter(|o| o.campaign == 0) {
            o.entity = 0;
            o.group_entity = 0;
        }
    }

    /// A group of another campaign's platform names, in DCS from a mission
    /// that campaign ran: group `group`, units `units`, all `M-113`. The
    /// agent binds its rows only while that campaign is active.
    pub fn add_campaign_group(&mut self, campaign: u64, group: u64, units: &[u64]) {
        use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind};
        let gname = ids::name(NameKind::Group, CampaignId(campaign), EntityId(group));
        let gid = self.next_object;
        self.next_object += 1;
        let mut members = Vec::new();
        for (i, u) in units.iter().enumerate() {
            let id = self.next_object;
            self.next_object += 1;
            self.objects.insert(
                id,
                FObject {
                    name: ids::name(NameKind::Unit, CampaignId(campaign), EntityId(*u)),
                    type_name: "M-113".into(),
                    group: gid,
                    entity: *u,
                    group_entity: group,
                    campaign,
                    kind: object_kind::UNIT,
                    at: Position {
                        object: id,
                        x: 5_000.0 + 20.0 * i as f64,
                        y: 0.0,
                        z: 5_000.0,
                    },
                },
            );
            let bound = (
                name_entity(&self.objects[&id].name, self.campaign),
                name_entity(&gname, self.campaign),
            );
            self.table.insert(id, bound);
            members.push(id);
        }
        self.groups.insert(
            gid,
            FGroup {
                name: gname,
                entity: group,
                campaign,
                members,
                route: Vec::new(),
                attack: None,
                wp: 0,
            },
        );
    }

    /// The next `n` mission loads fail in DCS: answered `DCS_REFUSED`, and
    /// the mission runs on.
    pub fn refuse_loads(&mut self, n: u32) {
        self.refuse_loads = n;
    }

    /// The next `n` destroys of any entity raise in DCS and are answered
    /// `DCS_REFUSED`.
    pub fn refuse_destroys(&mut self, n: u32) {
        self.refuse_destroys = n;
    }

    /// The next `n` `DAMAGE` commands raise in DCS and are answered
    /// `DCS_REFUSED`.
    pub fn refuse_damages(&mut self, n: u32) {
        self.refuse_damages = n;
    }

    /// The next `n` `STATIC_SPAWN` commands with `dead = true` are answered
    /// `DCS_REFUSED`, and DCS adds nothing.
    pub fn refuse_ruins(&mut self, n: u32) {
        self.refuse_ruins = n;
    }

    /// The next `n` `STATIC_SPAWN` commands with `dead = true` are answered
    /// `BAD_PAYLOAD`, a rejection a resend cannot fix.
    pub fn reject_ruins(&mut self, n: u32) {
        self.reject_ruins = n;
    }

    /// The initial life the `OBJECT` records of a unit entity carry.
    pub fn set_life0(&mut self, entity: u64, life0: f32) {
        self.life0_of.insert(entity, life0);
    }

    /// The link drops: the agent detaches and queues its records. Commands
    /// the sidecar held back never reach it.
    pub fn detach(&mut self) {
        self.attached = false;
        self.held.clear();
    }

    /// A sidecar attaches (spec/transport.md, "Attaching"): commands written
    /// and not taken are discarded, the queued records and any `GAP` are
    /// written, then a `HELLO`, and a full enumeration starts.
    pub fn attach(&mut self, sidecar_boot: u64) {
        if !self.alive {
            return;
        }
        self.attached = true;
        self.sidecar_boot = sidecar_boot;
        self.inbound.clear();
        self.held.clear();
        self.written_at.clear();
        // A death read and never acknowledged is read again.
        self.deaths_unacked.clear();
        // Records the last sidecar read and never acknowledged come first:
        // they are older than anything still in the ring. Those of an agent
        // boot that is gone went with it.
        let boot = self.agent_boot;
        let mut reread: Vec<RawRecord> = std::mem::take(&mut self.unacked)
            .into_iter()
            .filter(|r| r.agent_boot == boot)
            .collect();
        reread.append(&mut self.out);
        self.out = reread;
        self.in_tail = self.in_head;
        let queued: Vec<(Stamp, Input)> = self.queue.drain(..).collect();
        let mut last_frame = None;
        if let Some(g) = self.gap.take() {
            let s = self.stamp();
            self.frame_record(&s, true);
            self.write(&Input::Gap(g));
        }
        for (s, input) in queued {
            // A FRAME whenever the stamps change, the campaign included.
            let key = (s.frame, s.mission_gen_, s.campaign, s.lifecycle);
            if last_frame != Some(key) {
                self.frame_record(&s, true);
                last_frame = Some(key);
            }
            self.write(&input);
        }
        let s = self.stamp();
        self.frame_record(&s, false);
        let hello = Hello {
            agent_boot: self.agent_boot,
            sidecar_boot,
            build: 1,
            campaign: self.campaign,
            epoch: self.epoch,
            last_applied_seq: self.last_applied_seq,
            in_head: self.in_head,
            interface: 1,
            flags: 1,
            dcs_version: "fake".into(),
        };
        self.write(&Input::Hello(hello));
        // Rows queued before the HELLO are of the last attach: every row is
        // written whole in the first enumeration, and again until sent.
        self.sent.clear();
        self.full = true;
        // A LIFECYCLE of the current state right after every HELLO, so a
        // sidecar that attaches to a running mission learns its theater
        // (step S10 specifies it and builds it in the agent).
        self.lifecycle_record();
        if self.running() {
            self.start_enumeration();
        }
    }

    /// A unit or static dies: a death event, and the row leaves the table.
    /// Scenery dies with it, [`SCENERY_PER_DEATH`] objects, each its own
    /// death event and never in the table (S9's catalogue: 300 of 306
    /// deaths were scenery, LIVE). With `Config::death_delay_max` above 0 the
    /// event is late, as [`FakeSim::kill_late`] with a drawn delay.
    pub fn kill(&mut self, object: u64) {
        if self.cfg.death_delay_max > 0.0 {
            let delay = self.draw_delay();
            return self.kill_late(object, delay, Left::Missing);
        }
        let Some(o) = self.remove_object(object) else {
            return;
        };
        self.kill_ledger(object, &o);
        self.unattributed(object, &o);
        let category = Self::category(&o);
        self.scenery_deaths(object);
        if self.table.contains_key(&object) {
            self.emit(Input::Event(dead_event(object, category)));
            self.gone(object, gone_reason::DEAD);
        }
    }

    /// A unit or static dies and its death event comes `delay` model seconds
    /// later. The object leaves DCS now, and its row leaves the agent's table
    /// at once as `MISSING` or as `REMOVED` (the classifier reading a kill as
    /// a removal). The agent keeps the object ID, entity and group entity;
    /// when the event is due it writes the death event and a second
    /// `OBJECT_GONE` with `DEAD` and the kept fields. A generation change or
    /// a DCS restart in the gap writes nothing.
    pub fn kill_late(&mut self, object: u64, delay: f64, left: Left) {
        let Some(o) = self.remove_object(object) else {
            return;
        };
        self.kill_ledger(object, &o);
        // The row left DCS without a death fact, so far.
        self.ledger
            .removed
            .insert((self.agent_boot, self.mission_gen_, o.campaign, o.entity));
        self.ledger
            .left_in
            .insert((self.agent_boot, self.mission_gen_, o.campaign, o.entity));
        self.unattributed(object, &o);
        self.scenery_deaths(object);
        if let Some((entity, group_entity)) = self.table.get(&object).copied() {
            let reason = match left {
                Left::Missing => gone_reason::MISSING,
                Left::Removed => gone_reason::REMOVED,
            };
            self.gone(object, reason);
            self.late.push(Late {
                due: self.model_time + delay.max(0.0),
                object,
                entity,
                group_entity,
                category: Self::category(&o),
            });
        }
    }

    /// A killed object comes alive again, as a script setting its life does:
    /// the same object ID, entity and group, and the agent writes an `OBJECT`
    /// for the row. A late death event that has not come is forgotten.
    /// Returns whether the object was one that left DCS.
    pub fn revive(&mut self, object: u64) -> bool {
        if self.objects.contains_key(&object) {
            return false;
        }
        let Some((o, group)) = self.graves.remove(&object) else {
            return false;
        };
        // DCS holds a group of that name now: the object has nothing to
        // come back to.
        if self
            .groups
            .iter()
            .any(|(id, g)| g.name == group.name && *id != o.group)
        {
            self.graves.insert(object, (o, group));
            return false;
        }
        self.late.retain(|l| l.object != object);
        self.ledger
            .left_in
            .remove(&(self.agent_boot, self.mission_gen_, o.campaign, o.entity));
        // A revived static is no ruin.
        self.ruins.retain(|_, r| r.object != object);
        let gid = if o.kind == object_kind::STATIC {
            object
        } else {
            o.group
        };
        match self.groups.get_mut(&gid) {
            Some(g) => g.members.push(object),
            None => {
                let mut g = group;
                g.members = vec![object];
                self.groups.insert(gid, g);
            }
        }
        let bound = if o.campaign == self.campaign && self.campaign != 0 {
            (o.entity, o.group_entity)
        } else {
            (0, 0)
        };
        self.objects.insert(object, o);
        self.table.insert(object, bound);
        let r = self.object_record(object, 0);
        self.emit(r);
        true
    }

    /// The objects that left DCS and can be revived.
    pub fn graves(&self) -> Vec<u64> {
        self.graves.keys().copied().collect()
    }

    /// How many model seconds the latest pending death event is away, 0 for
    /// none.
    pub fn late_horizon(&self) -> f64 {
        self.late
            .iter()
            .map(|l| l.due - self.model_time)
            .fold(0.0, f64::max)
    }

    /// Death events the agent has yet to write.
    pub fn late_pending(&self) -> usize {
        self.late.len()
    }

    fn draw_delay(&mut self) -> f64 {
        // SplitMix64, as the harness's schedules use.
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64 * self.cfg.death_delay_max
    }

    fn category(o: &FObject) -> f64 {
        if o.kind == object_kind::STATIC {
            3.0
        } else {
            1.0
        }
    }

    fn kill_ledger(&mut self, object: u64, o: &FObject) {
        if o.entity != 0 {
            self.ledger.deaths.insert((o.campaign, o.entity));
        }
        // A killed static stays in DCS as a dead object under its name.
        if o.kind == object_kind::STATIC {
            self.ruins.insert(
                o.name.clone(),
                Ruin {
                    object,
                    entity: o.entity,
                    campaign: o.campaign,
                    type_name: o.type_name.clone(),
                    x: o.at.x,
                    z: o.at.z,
                    heading: self.heading.get(&object).copied().unwrap_or(0.0),
                    spawned: false,
                },
            );
        }
    }

    fn scenery_deaths(&mut self, object: u64) {
        for i in 0..SCENERY_PER_DEATH {
            self.emit(Input::Event(dead_event(
                SCENERY_BASE + object * 100 + i,
                5.0,
            )));
        }
    }

    /// Write the death events that are due: the event itself, and the
    /// second `OBJECT_GONE` with `DEAD` for the row that left earlier.
    fn fire_late(&mut self) {
        if self.late.is_empty() {
            return;
        }
        let now = self.model_time;
        let (due, rest): (Vec<Late>, Vec<Late>) = std::mem::take(&mut self.late)
            .into_iter()
            .partition(|l| l.due <= now);
        self.late = rest;
        for l in due {
            self.emit(Input::Event(dead_event(l.object, l.category)));
            self.emit(Input::ObjectGone(ObjectGone {
                object: l.object,
                reason: gone_reason::DEAD,
                entity: l.entity,
                group_entity: l.group_entity,
            }));
        }
    }

    /// A platform object whose row holds no binding: its death or removal
    /// names no unit.
    fn unattributed(&mut self, object: u64, o: &FObject) {
        if o.entity != 0 && self.table.get(&object).is_some_and(|b| b.0 == 0) {
            self.ledger.unreported.insert((
                self.agent_boot,
                self.mission_gen_,
                o.campaign,
                o.entity,
            ));
        }
    }

    /// The row leaves the table with an `OBJECT_GONE` that carries its
    /// binding.
    fn gone(&mut self, object: u64, reason: u32) {
        self.sent.remove(&object);
        if let Some((entity, group_entity)) = self.table.remove(&object) {
            self.emit(Input::ObjectGone(ObjectGone {
                object,
                reason,
                entity,
                group_entity,
            }));
        }
    }

    /// An object leaves DCS with no event. Its row stays in the table until
    /// a full enumeration does not find it.
    pub fn remove_silently(&mut self, object: u64) {
        if let Some(o) = self.remove_object(object) {
            self.ledger
                .removed
                .insert((self.agent_boot, self.mission_gen_, o.campaign, o.entity));
            self.ledger
                .left_in
                .insert((self.agent_boot, self.mission_gen_, o.campaign, o.entity));
        }
    }

    /// Full enumerations do not find this object while it exists, as when a
    /// group's `getUnits` fails. The agent checks each miss before it ends
    /// the enumeration: an object that still exists is written with that
    /// enumeration's number, one that is gone leaves as `MISSING`.
    pub fn hide_from_enumeration(&mut self, object: u64) {
        self.hidden.insert(object);
    }

    /// Whether an enumeration has visited every row and holds its
    /// `ENUM_END` for its misses (`Config::enum_end_hold`).
    pub fn enum_end_held(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| r.rows.is_empty() && !r.misses.is_empty())
    }

    /// Full enumerations find this object again.
    pub fn show_in_enumeration(&mut self, object: u64) {
        self.hidden.remove(&object);
    }

    /// An object leaves DCS with a removal event.
    pub fn remove_with_event(&mut self, object: u64) {
        let Some(o) = self.remove_object(object) else {
            return;
        };
        self.ledger
            .removed
            .insert((self.agent_boot, self.mission_gen_, o.campaign, o.entity));
        self.ledger
            .left_in
            .insert((self.agent_boot, self.mission_gen_, o.campaign, o.entity));
        self.unattributed(object, &o);
        self.gone(object, gone_reason::REMOVED);
    }

    fn remove_object(&mut self, object: u64) -> Option<FObject> {
        let o = self.objects.remove(&object)?;
        let gid = if o.kind == object_kind::STATIC {
            object
        } else {
            o.group
        };
        if let Some(g) = self.groups.get(&gid) {
            self.graves.insert(object, (o.clone(), g.clone()));
        }
        if let Some(g) = self.groups.get_mut(&gid) {
            g.members.retain(|m| *m != object);
            if g.members.is_empty() {
                self.groups.remove(&gid);
            }
        }
        Some(o)
    }

    fn add_group(
        &mut self,
        name: &str,
        entity: u64,
        types: &[&str],
        _rev: u64,
        is_static: bool,
        at: Position,
    ) -> u64 {
        let gid = self.next_object;
        let mut members = Vec::new();
        if is_static {
            self.objects.insert(
                gid,
                FObject {
                    name: name.to_owned(),
                    type_name: types[0].to_owned(),
                    group: 0,
                    entity,
                    group_entity: 0,
                    campaign: name_campaign(name, self.campaign),
                    kind: object_kind::STATIC,
                    at: Position { object: gid, ..at },
                },
            );
            self.next_object += 1;
            members.push(gid);
        } else {
            self.next_object += 1;
            for (i, t) in types.iter().enumerate() {
                let id = self.next_object;
                self.next_object += 1;
                self.objects.insert(
                    id,
                    FObject {
                        name: format!("{name}-{}", i + 1),
                        type_name: (*t).to_owned(),
                        group: gid,
                        entity: 0,
                        group_entity: entity,
                        campaign: 0,
                        kind: object_kind::UNIT,
                        at: Position { object: id, ..at },
                    },
                );
                members.push(id);
            }
        }
        for m in &members {
            let o = &self.objects[m];
            self.table.insert(*m, (o.entity, o.group_entity));
        }
        self.groups.insert(
            gid,
            FGroup {
                name: name.to_owned(),
                entity,
                campaign: name_campaign(name, self.campaign),
                members,
                route: Vec::new(),
                attack: None,
                wp: 0,
            },
        );
        gid
    }

    fn start_enumeration(&mut self) {
        self.enumerations += 1;
        let id = self.enumerations;
        self.emit(Input::EnumBegin(Enumeration {
            enumeration: id,
            objects: 0,
            seen: 0,
        }));
        self.run = Some(Run {
            id,
            rows: self.table.keys().copied().collect(),
            misses: Vec::new(),
            written: 0,
            seen: 0,
            full: std::mem::take(&mut self.full),
            hold: self.cfg.enum_end_hold,
        });
        self.next_enum = self.frame + self.cfg.enum_period;
    }

    /// Write the open batch of `SEEN` IDs of the enumeration `run`.
    fn flush_seen(&mut self, run: &mut Run, seen: &mut Vec<u64>) {
        if !seen.is_empty() {
            self.emit(Input::Seen(Seen {
                enumeration: run.id,
                objects: std::mem::take(seen),
            }));
        }
    }

    fn enumeration_slice(&mut self) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        let mut seen = Vec::new();
        for _ in 0..self.cfg.enum_slice {
            let Some(row) = run.rows.pop_front() else {
                break;
            };
            if !self.table.contains_key(&row) {
                continue;
            }
            if self.objects.contains_key(&row) && !self.hidden.contains(&row) {
                if run.full || !self.sent.contains(&row) {
                    // Another record goes after the open batch of `SEEN` IDs,
                    // never before it.
                    self.flush_seen(&mut run, &mut seen);
                    let r = self.object_record(row, run.id);
                    self.emit(r);
                    run.written += 1;
                } else {
                    seen.push(row);
                    run.seen += 1;
                }
            } else {
                run.misses.push(row);
            }
        }
        self.flush_seen(&mut run, &mut seen);
        if run.rows.is_empty() && run.hold > 0 && !run.misses.is_empty() {
            run.hold -= 1;
            self.run = Some(run);
        } else if run.rows.is_empty() {
            // Each miss is checked before the end: alive, it is written
            // with this enumeration's number; gone, it leaves.
            for row in std::mem::take(&mut run.misses) {
                if self.objects.contains_key(&row) {
                    let r = self.object_record(row, run.id);
                    self.emit(r);
                    run.written += 1;
                } else {
                    self.gone(row, gone_reason::MISSING);
                }
            }
            self.emit(Input::EnumEnd(Enumeration {
                enumeration: run.id,
                objects: run.written,
                seen: run.seen,
            }));
        } else {
            self.run = Some(run);
        }
    }

    // ---- the frame ------------------------------------------------------

    /// Run one frame.
    pub fn frame(&mut self) {
        // A lost result is lost in this frame or not at all.
        self.losing = std::mem::take(&mut self.lose_result);
        if !self.alive {
            return;
        }
        if let Some(n) = self.load_after {
            if n == 0 {
                self.load_after = None;
                self.load_commanded();
            } else {
                self.load_after = Some(n - 1);
            }
        }
        if self.lifecycle == lc::LOADING as u32 {
            self.loading -= 1;
            if self.loading > 0 {
                return;
            }
            self.lifecycle = lc::RUNNING as u32;
            self.frame += 1;
            if self.attached {
                let s = self.stamp();
                self.frame_record(&s, false);
            }
            self.lifecycle_record();
            if self.attached {
                self.start_enumeration();
            }
            return;
        }
        self.frame += 1;
        let paused = self.paused();
        if !paused {
            self.model_time += 0.1;
        }
        if !self.attached {
            // The events queue for the next attach.
            self.fire_late();
        }
        if self.attached {
            let s = self.stamp();
            self.frame_record(&s, false);
            self.fire_late();
            // The burst lifts the limit from the command after it on.
            let mut taken = 0;
            while (self.burst && paused) || taken < self.cfg.commands_per_frame {
                let Some((pos, op, payload)) = self.inbound.pop_front() else {
                    break;
                };
                taken += 1;
                self.in_head = pos;
                self.handling_written_at = self.written_at.remove(&pos);
                if let Ok(Some(c)) = Command::decode(op, &payload)
                    && !c.body.is_control()
                {
                    self.ledger.taken.insert(
                        (self.agent_boot, c.header.epoch, c.header.seq),
                        c.header.campaign,
                    );
                }
                self.handle(op, &payload);
                self.handling_written_at = None;
            }
            self.ledger.most_per_frame = self.ledger.most_per_frame.max(taken);
        }
        if !paused {
            self.move_groups();
        }
        if self.attached {
            for r in std::mem::take(&mut self.queued_out) {
                self.emit(r);
            }
            if self.run.is_none() && self.frame >= self.next_enum {
                self.start_enumeration();
            }
            self.enumeration_slice();
            if self.frame.is_multiple_of(self.cfg.warehouse_every.max(1)) {
                let bases: Vec<(String, FAirbase)> = self
                    .airbases
                    .iter()
                    .map(|(n, a)| (n.clone(), a.clone()))
                    .collect();
                for (name, a) in bases {
                    self.emit(Input::Airbase(Airbase {
                        object: a.object,
                        coalition: a.coalition,
                        airbase: name.clone(),
                    }));
                    self.emit(Input::Warehouse(Warehouse {
                        airbase: name,
                        stock: a.stock.to_lv(),
                    }));
                }
            }
            if self.frame.is_multiple_of(self.cfg.positions_every) {
                let rows: Vec<Position> = self
                    .table
                    .keys()
                    .filter_map(|o| self.objects.get(o).map(|x| x.at))
                    .collect();
                if !rows.is_empty() {
                    self.emit(Input::Positions(rows));
                }
            }
        }
    }

    fn move_groups(&mut self) {
        let step = self.cfg.step_m;
        for g in self.groups.values_mut() {
            let Some(target) = g.route.get(g.wp).copied() else {
                continue;
            };
            let mut arrived = true;
            for m in &g.members {
                let o = self.objects.get_mut(m).expect("member");
                let (dx, dz) = (target.x - o.at.x, target.z - o.at.z);
                let d = (dx * dx + dz * dz).sqrt();
                if d <= step {
                    o.at.x = target.x;
                    o.at.z = target.z;
                } else {
                    o.at.x += dx / d * step;
                    o.at.z += dz / d * step;
                    arrived = false;
                }
            }
            if arrived {
                g.wp += 1;
            }
        }
    }

    // ---- commands -------------------------------------------------------

    fn result(&mut self, cmd: &Command, code: u64, object: u64, message: &str, value: Lv) {
        let h = &cmd.header;
        if h.seq != 0 {
            self.ledger
                .results
                .insert((self.agent_boot, h.epoch, h.seq), code as u16);
        }
        self.emit(Input::Result(CommandResult {
            seq: h.seq,
            epoch: h.epoch,
            campaign: h.campaign,
            entity: h.entity,
            rev: h.rev,
            object,
            cost_us: 10,
            opcode: cmd.body.opcode(),
            code: code as u16,
            message: message.to_owned(),
            value,
        }));
    }

    fn handle(&mut self, op: u16, payload: &[u8]) {
        let cmd = match Command::decode(op, payload) {
            Ok(Some(c)) => c,
            Ok(None) => {
                // An opcode the fake has no handler for.
                let mut r = nanoprobe_transport::codec::Reader::new(payload);
                let Ok(h) = nanoprobe_transport::records::CommandHeader::read(&mut r) else {
                    return;
                };
                if h.seq != 0 && h.epoch >= self.epoch && h.seq > self.last_applied_seq {
                    self.epoch = h.epoch;
                    self.last_applied_seq = h.seq;
                }
                self.emit(Input::Result(CommandResult {
                    seq: h.seq,
                    epoch: h.epoch,
                    campaign: h.campaign,
                    entity: h.entity,
                    rev: h.rev,
                    object: 0,
                    cost_us: 0,
                    opcode: op,
                    code: rc::UNKNOWN_OPCODE as u16,
                    message: String::new(),
                    value: Lv::Nil,
                }));
                return;
            }
            Err(_) => {
                // A failed consuming check: no result; the agent detaches.
                self.attached = false;
                return;
            }
        };
        if cmd.body.is_control() {
            match cmd.body {
                Body::Campaign(c) => {
                    let changed = c != self.campaign;
                    self.set_campaign(c);
                    // The records after it carry the new campaign: a FRAME
                    // stamps them (the agent does the same, S10).
                    if changed && self.attached {
                        let s = self.stamp();
                        self.frame_record(&s, false);
                    }
                }
                Body::Enumerate => {
                    self.full = true;
                    self.start_enumeration();
                }
                Body::Rates(r) => {
                    self.rates = r;
                    self.rates_received += 1;
                }
                Body::Burst(on) => {
                    let paused = self.paused();
                    self.ledger.bursts.push((on, paused));
                    self.burst = on && paused;
                }
                Body::Bind(b) => {
                    self.binds_received += 1;
                    self.bind(&b);
                }
                Body::Subscribe(sub) => {
                    // An entity the binding does not hold is ignored.
                    self.subscribed = self
                        .table
                        .iter()
                        .filter(|(_, (unit, _))| *unit != 0 && sub.entity.contains(unit))
                        .map(|(o, _)| *o)
                        .collect();
                }
                _ => {}
            }
            return;
        }
        let h = cmd.header;
        if h.epoch < self.epoch {
            return self.result(&cmd, rc::STALE_EPOCH, 0, "", Lv::Nil);
        }
        if h.epoch > self.epoch {
            self.epoch = h.epoch;
            self.last_applied_seq = 0;
        }
        if h.seq <= self.last_applied_seq {
            return self.result(&cmd, rc::DUPLICATE, 0, "", Lv::Nil);
        }
        self.last_applied_seq = h.seq;
        if h.campaign != self.campaign {
            return self.result(&cmd, rc::WRONG_CAMPAIGN, 0, "", Lv::Nil);
        }
        // The opcodes whose Campaign column says `active` need a campaign
        // (the real admission table's NO_CAMPAIGN row, before the revision
        // check). The extension opcodes and `EVAL` need no generation.
        if matches!(
            cmd.body,
            Body::ExtensionSet { .. }
                | Body::ExtensionRemove { .. }
                | Body::ExtensionInvoke { .. }
                | Body::Eval { .. }
                | Body::MarkSet { .. }
                | Body::MarkRemove
                | Body::DrawingSet { .. }
                | Body::DrawingRemove
                | Body::MenuSet { .. }
                | Body::MenuRemove
        ) && self.campaign == 0
        {
            return self.result(&cmd, rc::NO_CAMPAIGN, 0, "", Lv::Nil);
        }
        if let Body::ExtensionSet { name, .. } = &cmd.body {
            let name = name.clone();
            if h.rev == 0 {
                return self.result(&cmd, rc::BAD_PAYLOAD, 0, "no revision", Lv::Nil);
            }
            if h.rev <= self.applied_extension.get(&name).copied().unwrap_or(0) {
                return self.result(&cmd, rc::AT_REVISION, 0, "", Lv::Nil);
            }
            if !self.cfg.raw_lua {
                return self.result(&cmd, rc::RAW_LUA_OFF, 0, "", Lv::Nil);
            }
            self.applied_extension.insert(name.clone(), h.rev);
            let key = (self.agent_boot, self.campaign, name, h.rev);
            *self.ledger.extension_runs.entry(key).or_default() += 1;
            return self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
        }
        match &cmd.body {
            Body::ExtensionRemove { name } => {
                self.applied_extension.remove(name);
                self.ledger.extension_removals.push(name.clone());
                return self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::Eval { source } => {
                if !self.cfg.raw_lua {
                    return self.result(&cmd, rc::RAW_LUA_OFF, 0, "", Lv::Nil);
                }
                let Lv::String(b) = source else {
                    return self.result(&cmd, rc::BAD_PAYLOAD, 0, "", Lv::Nil);
                };
                let src = String::from_utf8_lossy(b).into_owned();
                self.ledger.evals.push(src.clone());
                // The fake runs no Lua: `return <number>` and `return "text"`
                // answer that value, `error(` raises, anything else returns
                // nothing.
                let t = src.trim();
                if t.contains("error(") {
                    return self.result(&cmd, rc::LUA_ERROR, 0, "raised", Lv::Nil);
                }
                let v = match t.strip_prefix("return ").map(str::trim) {
                    Some(x) => match x.parse::<f64>() {
                        Ok(n) => Lv::Number(n),
                        Err(_) => Lv::String(x.trim_matches(['"', '\'']).as_bytes().to_vec()),
                    },
                    None => Lv::Nil,
                };
                return self.result(&cmd, rc::APPLIED, 0, "", v);
            }
            Body::Message {
                scope,
                clear,
                target,
                seconds,
                text,
            } => {
                if !(1..=5).contains(scope)
                    || !(seconds.is_finite() && (0.0..=86_400.0).contains(seconds))
                {
                    return self.result(&cmd, rc::BAD_PAYLOAD, 0, "", Lv::Nil);
                }
                if !self.scope_held(h.campaign, *scope, *target) {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                }
                self.ledger
                    .messages
                    .push((*scope, *target, *clear, *seconds, text.clone()));
                return self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::Chat { player, text } => {
                if text.is_empty() || text.len() > 1024 {
                    return self.result(&cmd, rc::BAD_PAYLOAD, 0, "", Lv::Nil);
                }
                self.ledger.chats.push((*player, text.clone()));
                return self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::Moderate { .. } => return self.moderate(&cmd),
            Body::Players => {
                self.ledger.players_asked += 1;
                let rows: Vec<Lv> = self
                    .clients
                    .iter()
                    .map(|(id, c)| {
                        lv::obj(vec![
                            ("id", lv::n(f64::from(*id))),
                            ("ucid", lv::s(&c.ucid)),
                            ("name", lv::s(&c.name)),
                            ("side", lv::n(f64::from(c.side))),
                            ("slot", lv::s(&c.slot)),
                        ])
                    })
                    .collect();
                return self.result(&cmd, rc::APPLIED, 0, "", lv::list(rows));
            }
            Body::Bans => {
                self.ledger.bans_asked += 1;
                let rows: Vec<Lv> = self
                    .bans
                    .iter()
                    .map(|b| {
                        lv::obj(vec![
                            ("ucid", lv::s(&b.ucid)),
                            ("name", lv::s(&b.name)),
                            ("banned_until", lv::n(b.banned_until)),
                            ("reason", lv::s(&b.reason)),
                        ])
                    })
                    .collect();
                return self.result(&cmd, rc::APPLIED, 0, "", lv::list(rows));
            }
            Body::MissionLoad { path } => {
                if path.is_empty() || path.len() > 1024 {
                    return self.result(&cmd, rc::BAD_PAYLOAD, 0, "", Lv::Nil);
                }
                if self.refuse_loads > 0 {
                    self.refuse_loads -= 1;
                    return self.result(
                        &cmd,
                        rc::DCS_REFUSED,
                        0,
                        "injected: the load failed",
                        Lv::Nil,
                    );
                }
                self.ledger.mission_loads.push(path.clone());
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
                // The result is queued, then DCS loads: the fake loads its
                // one mission, now or after `load_delay_frames`.
                if self.cfg.load_delay_frames == 0 {
                    self.load_commanded();
                } else {
                    self.load_after = Some(self.cfg.load_delay_frames);
                }
                return;
            }
            _ => {}
        }
        if let Body::ExtensionInvoke { name, args } = &cmd.body {
            if !self.applied_extension.contains_key(name) {
                return self.result(&cmd, rc::LUA_ERROR, 0, "no extension", Lv::Nil);
            }
            // An effect is identified by its token, the number at args[1],
            // so a copy sent under a new sequence number counts twice. An
            // invoke with no token falls back to its wire position.
            let token = match args {
                Lv::Table(p) => p.iter().find_map(|(k, v)| match (k, v) {
                    (Lv::Number(k), Lv::Number(t)) if *k == 1.0 => Some(*t as u64),
                    _ => None,
                }),
                _ => None,
            };
            let key = (name.clone(), token.unwrap_or(h.epoch << 40 ^ h.seq));
            let n = self.ledger.invokes.entry(key).or_default();
            *n += 1;
            let v = Lv::Number(f64::from(*n));
            return self.result(&cmd, rc::APPLIED, 0, "", v);
        }
        if h.mission_gen != self.mission_gen_ {
            return self.result(&cmd, rc::WRONG_GENERATION, 0, "", Lv::Nil);
        }
        let rev_map = match op {
            opcode::GROUP_SPAWN
            | opcode::STATIC_SPAWN
            | opcode::MARK_SET
            | opcode::DRAWING_SET
            | opcode::MENU_SET => Some(&self.applied_spec),
            opcode::GROUP_ORDER => Some(&self.applied_order),
            _ => None,
        };
        if let Some(map) = rev_map {
            if h.rev == 0 {
                return self.result(&cmd, rc::BAD_PAYLOAD, 0, "no revision", Lv::Nil);
            }
            if h.rev <= map.get(&h.entity).copied().unwrap_or(0) {
                let object = self.group_of(h.entity).unwrap_or(0);
                return self.result(&cmd, rc::AT_REVISION, object, "", Lv::Nil);
            }
        }
        match &cmd.body {
            Body::GroupSpawn { name, group, .. } => {
                let units: Vec<(String, String, Position, f64)> = lv::get(group, "units")
                    .map(|u| {
                        lv::items(u)
                            .into_iter()
                            .map(|t| {
                                (
                                    lv::str_at(t, "name").unwrap_or_default(),
                                    lv::str_at(t, "type").unwrap_or_default(),
                                    Position {
                                        object: 0,
                                        x: lv::num_at(t, "x").unwrap_or(0.0),
                                        y: lv::num_at(t, "alt").unwrap_or(0.0),
                                        z: lv::num_at(t, "y").unwrap_or(0.0),
                                    },
                                    lv::num_at(t, "heading").unwrap_or(0.0),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if units.is_empty() || units.iter().any(|(_, t, _, _)| t.starts_with("Unknown")) {
                    return self.result(&cmd, rc::DCS_REFUSED, 0, "unknown unit type", Lv::Nil);
                }
                self.spawn(&cmd, name, units, false);
            }
            Body::StaticSpawn { name, table, .. } => {
                let t = lv::str_at(table, "type").unwrap_or_default();
                if t.starts_with("Unknown") {
                    return self.result(&cmd, rc::DCS_REFUSED, 0, "unknown static type", Lv::Nil);
                }
                let at = Position {
                    object: 0,
                    x: lv::num_at(table, "x").unwrap_or(0.0),
                    y: 0.0,
                    z: lv::num_at(table, "y").unwrap_or(0.0),
                };
                let heading = lv::num_at(table, "heading").unwrap_or(0.0);
                if matches!(lv::get(table, "dead"), Some(Lv::Bool(true))) {
                    if self.reject_ruins > 0 {
                        self.reject_ruins -= 1;
                        return self.result(&cmd, rc::BAD_PAYLOAD, 0, "", Lv::Nil);
                    }
                    if self.refuse_ruins > 0 {
                        self.refuse_ruins -= 1;
                        return self.result(
                            &cmd,
                            rc::DCS_REFUSED,
                            0,
                            "injected: the dead static was refused",
                            Lv::Nil,
                        );
                    }
                    return self.spawn_ruin(&cmd, name, t, at, heading);
                }
                self.spawn(&cmd, name, vec![(name.clone(), t, at, heading)], true);
            }
            Body::GroupOrder(task) => {
                let Some(gid) = self.group_of(h.entity) else {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                };
                let g = self.groups.get_mut(&gid).expect("group");
                g.route = route_points(task);
                g.wp = 0;
                g.attack = lv::get(task, "params")
                    .filter(|_| lv::str_at(task, "id").as_deref() == Some("AttackGroup"))
                    .and_then(|p| lv::num_at(p, "groupId"))
                    .map(|v| v as u64);
                match lv::get(task, "params")
                    .filter(|_| lv::str_at(task, "id").as_deref() == Some("AttackUnit"))
                    .and_then(|p| lv::num_at(p, "unitId"))
                {
                    Some(u) => self.attack_unit.insert(gid, u as u64),
                    None => self.attack_unit.remove(&gid),
                };
                self.applied_order.insert(h.entity, h.rev);
                self.result(&cmd, rc::APPLIED, gid, "", Lv::Nil);
            }
            Body::GroupDestroy | Body::StaticDestroy => {
                if self.refuse_destroys > 0 {
                    self.refuse_destroys -= 1;
                    let key = (self.agent_boot, self.mission_gen_, h.campaign, h.entity);
                    *self.ledger.failed_destroys.entry(key).or_default() += 1;
                    return self.result(
                        &cmd,
                        rc::DCS_REFUSED,
                        0,
                        "injected: the destroy raised",
                        Lv::Nil,
                    );
                }
                self.destroy(h.entity);
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::GroupCaptureDestroy | Body::Capture => {
                let Some(gid) = self.group_of(h.entity) else {
                    let code = if op == opcode::CAPTURE {
                        rc::NOT_BOUND
                    } else {
                        rc::APPLIED
                    };
                    return self.result(&cmd, code, 0, "", Lv::Nil);
                };
                let value = self.capture(gid);
                if op == opcode::GROUP_CAPTURE_DESTROY {
                    self.destroy(h.entity);
                    self.result(&cmd, rc::APPLIED, 0, "", value);
                } else {
                    self.result(&cmd, rc::APPLIED, gid, "", value);
                }
            }
            Body::GroupOption { option, value } => {
                if self.group_of(h.entity).is_none() {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                }
                self.options
                    .entry(h.entity)
                    .or_default()
                    .insert(*option, value.clone());
                self.ledger.options_applied += 1;
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::AirbaseOwner { coalition, airbase } => {
                let Some(a) = self.airbases.get_mut(airbase) else {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                };
                a.coalition = *coalition;
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::WarehouseSet { airbase, stock } => {
                let Some(a) = self.airbases.get_mut(airbase) else {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                };
                let set = Stock::from_lv(stock);
                if set.items().iter().any(|(_, c)| *c < 0.0) {
                    return self.result(&cmd, rc::DCS_REFUSED, 0, "a negative count", Lv::Nil);
                }
                a.stock.merge(&set);
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::WarehouseAdjust { airbase, delta } => {
                let Some(a) = self.airbases.get_mut(airbase) else {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                };
                let delta = Stock::from_lv(delta);
                // DCS clamps a count at zero and raises no error (LIVE, E6,
                // step S15): each item stops at zero.
                for (i, d) in delta.items() {
                    let now = a.stock.get(&i).unwrap_or(0.0);
                    a.stock.set(i, (now + d).max(0.0));
                }
                *self.ledger.adjustments.entry(airbase.clone()).or_default() += 1;
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::Damage { life_percent } => {
                if self.refuse_damages > 0 {
                    self.refuse_damages -= 1;
                    return self.result(
                        &cmd,
                        rc::DCS_REFUSED,
                        0,
                        "injected: the damage raised",
                        Lv::Nil,
                    );
                }
                let campaign = self.campaign;
                let Some((o, _)) = self
                    .objects
                    .iter()
                    .find(|(_, x)| x.entity == h.entity && x.campaign == campaign)
                else {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                };
                let o = *o;
                self.life.insert(o, *life_percent);
                self.result(&cmd, rc::APPLIED, o, "", Lv::Nil);
            }
            Body::ClearDebris(d) => {
                self.ledger.debris.push(*d);
                // Every placed wreck and unmanaged dead static inside the
                // volume goes and counts once.
                let before = self.junk.len();
                self.junk
                    .retain(|j| !j.inside(d) || (j.dead_static && j.name.starts_with("np#")));
                let n = self.cfg.debris_per_clear + (before - self.junk.len()) as f64;
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Number(n));
            }
            Body::Sound {
                scope,
                target,
                file,
            } => {
                if !self.scope_held(h.campaign, *scope, *target) {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                }
                self.ledger.sounds.push((*scope, *target, file.clone()));
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::Picture { scope, target, .. } => {
                if !self.scope_held(h.campaign, *scope, *target) {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "", Lv::Nil);
                }
                self.ledger.pictures.push(cmd.body.clone());
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::PictureStop => {
                self.ledger.picture_stops += 1;
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::MarkSet {
                scope,
                read_only,
                target,
                x,
                y,
                z,
                text,
            } => {
                let Some(dcs_group) = self.pin_group(h.campaign, *scope, *target, true) else {
                    return self.pin_refused(&cmd, *scope, true);
                };
                self.marks.insert(
                    h.entity,
                    FakeMark {
                        scope: *scope,
                        target: *target,
                        read_only: *read_only,
                        at: (*x, *y, *z),
                        text: text.clone(),
                        rev: h.rev,
                        dcs_group,
                    },
                );
                self.pin_applied(&cmd);
            }
            Body::DrawingSet {
                scope,
                shape,
                target,
                args,
            } => {
                if self.pin_group(h.campaign, *scope, *target, false).is_none() {
                    return self.pin_refused(&cmd, *scope, false);
                }
                self.drawings.insert(
                    h.entity,
                    FakeDrawing {
                        scope: *scope,
                        target: *target,
                        shape: *shape,
                        args: args.clone(),
                        rev: h.rev,
                    },
                );
                self.pin_applied(&cmd);
            }
            Body::MenuSet {
                parent,
                target,
                scope,
                submenu,
                text,
            } => {
                let Some(dcs_group) = self.pin_group(h.campaign, *scope, *target, true) else {
                    return self.pin_refused(&cmd, *scope, true);
                };
                if *parent != 0
                    && !self
                        .menus
                        .get(parent)
                        .is_some_and(|p| p.submenu && p.scope == *scope && p.target == *target)
                {
                    return self.result(&cmd, rc::NOT_BOUND, 0, "no such submenu", Lv::Nil);
                }
                let order = match self.menus.get(&h.entity) {
                    Some(old) => old.order,
                    None => {
                        self.menu_order += 1;
                        self.menu_order
                    }
                };
                self.menus.insert(
                    h.entity,
                    FakeMenu {
                        parent: *parent,
                        scope: *scope,
                        target: *target,
                        submenu: *submenu,
                        text: text.clone(),
                        rev: h.rev,
                        dcs_group,
                        order,
                    },
                );
                self.pin_applied(&cmd);
            }
            Body::MarkRemove | Body::DrawingRemove => {
                self.marks.remove(&h.entity);
                self.drawings.remove(&h.entity);
                self.applied_spec.remove(&h.entity);
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::MenuRemove => {
                // A submenu takes the items under it.
                let mut gone = vec![h.entity];
                while let Some(e) = gone.pop() {
                    self.menus.remove(&e);
                    self.applied_spec.remove(&e);
                    gone.extend(
                        self.menus
                            .iter()
                            .filter(|(_, m)| m.parent == e)
                            .map(|(c, _)| *c),
                    );
                }
                self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
            }
            Body::Countries => {
                let v = Lv::Table(
                    COUNTRY_COALITIONS
                        .iter()
                        .map(|(c, k)| (lv::n(f64::from(*c)), lv::n(f64::from(*k))))
                        .collect(),
                );
                self.ledger.countries_asked += 1;
                self.result(&cmd, rc::APPLIED, 0, "", v);
            }
            Body::ExtensionSet { .. }
            | Body::ExtensionRemove { .. }
            | Body::ExtensionInvoke { .. }
            | Body::Eval { .. }
            | Body::Message { .. }
            | Body::Chat { .. }
            | Body::Moderate { .. }
            | Body::Players
            | Body::Bans
            | Body::MissionLoad { .. } => {
                unreachable!("answered above")
            }
            Body::Campaign(_)
            | Body::Enumerate
            | Body::Burst(_)
            | Body::Rates(_)
            | Body::Subscribe(_)
            | Body::Bind(_) => {
                unreachable!("control")
            }
        }
    }

    /// The DCS group a marker's scope names: `Some(0)` for a scope with no
    /// group, `None` for a scope the opcode does not take or a group DCS
    /// does not have. `group_ok` says whether `GROUP` is a scope it takes.
    fn pin_group(&self, campaign: u64, scope: u32, target: u64, group_ok: bool) -> Option<u64> {
        match scope {
            1 | 2 => Some(0),
            3 if group_ok => self.bound_group(campaign, target),
            _ => None,
        }
    }

    /// The answer for a marker whose scope DCS cannot use: a scope the
    /// opcode does not take is `BAD_PAYLOAD`, a group DCS lacks `NOT_BOUND`.
    fn pin_refused(&mut self, cmd: &Command, scope: u32, group_ok: bool) {
        let code = if scope == 3 && group_ok {
            rc::NOT_BOUND
        } else {
            rc::BAD_PAYLOAD
        };
        self.result(cmd, code, 0, "", Lv::Nil);
    }

    fn pin_applied(&mut self, cmd: &Command) {
        let h = cmd.header;
        self.applied_spec.insert(h.entity, h.rev);
        *self.ledger.pin_sets.entry(h.entity).or_default() += 1;
        self.result(cmd, rc::APPLIED, 0, "", Lv::Nil);
    }

    /// Whether DCS holds what a `SOUND` or `PICTURE` scope names: for `GROUP`
    /// and `UNIT`, what the agent's binding table holds for the entity, else a
    /// platform object of the campaign (never with campaign 0); anything for
    /// the other scopes.
    fn scope_held(&self, campaign: u64, scope: u32, target: u64) -> bool {
        match scope {
            3 => self.bound_group(campaign, target).is_some(),
            4 => {
                self.table.values().any(|t| target != 0 && t.0 == target)
                    || campaign != 0
                        && self
                            .objects
                            .values()
                            .any(|x| x.entity == target && x.campaign == campaign)
            }
            _ => true,
        }
    }

    /// The DCS group an entity names, as the agent resolves a `GROUP` scope:
    /// the group its binding table holds for the entity, whatever its name,
    /// else the campaign's platform group of that entity.
    fn bound_group(&self, campaign: u64, entity: u64) -> Option<u64> {
        if entity != 0 {
            let bound = self
                .objects
                .iter()
                .find(|(o, x)| x.group != 0 && self.table.get(*o).is_some_and(|t| t.1 == entity));
            if let Some((_, x)) = bound {
                return Some(x.group);
            }
        }
        (campaign != 0)
            .then(|| self.group_in(campaign, entity))
            .flatten()
    }

    /// The DCS group of an entity of the active campaign.
    fn group_of(&self, entity: u64) -> Option<u64> {
        self.group_in(self.campaign, entity)
    }

    fn group_in(&self, campaign: u64, entity: u64) -> Option<u64> {
        self.groups
            .iter()
            .find(|(_, g)| g.entity == entity && entity != 0 && g.campaign == campaign)
            .map(|(id, _)| *id)
    }

    fn spawn(
        &mut self,
        cmd: &Command,
        name: &str,
        units: Vec<(String, String, Position, f64)>,
        is_static: bool,
    ) {
        let h = &cmd.header;
        self.graves.retain(|_, (_, g)| g.name != name);
        // A static under the name of a dead one replaces it.
        if is_static && self.ruins.remove(name).is_some() {
            self.ledger.ruins_replaced_alive += 1;
        }
        let old = self
            .groups
            .iter()
            .find(|(_, g)| g.name == name)
            .map(|(id, _)| *id);
        if let Some(old) = old {
            if !self.orphans.contains(&old) {
                self.ledger.replacements += 1;
                if h.rev > self.applied_spec.get(&h.entity).copied().unwrap_or(0) {
                    self.ledger.in_place += 1;
                }
            }
            // A static's old object leaves first, as the agent writes it. A
            // group's leaves after the new units' `OBJECT` records (below).
            if is_static {
                self.destroy_group(old);
            }
        }
        let gid = self.next_object;
        let mut members = Vec::new();
        let mut entities = Vec::new();
        self.next_object += 1;
        for (i, (uname, t, at, heading)) in units.into_iter().enumerate() {
            let id = if is_static { gid } else { self.next_object };
            if !is_static {
                self.next_object += 1;
            }
            let entity = ids::parse(&uname).map_or(0, |p| p.entity.0);
            entities.push(entity);
            self.heading.insert(id, heading);
            self.objects.insert(
                id,
                FObject {
                    name: if uname.is_empty() {
                        format!("{name}-{}", i + 1)
                    } else {
                        uname
                    },
                    type_name: t,
                    group: if is_static { 0 } else { gid },
                    entity,
                    group_entity: if is_static { 0 } else { h.entity },
                    campaign: h.campaign,
                    kind: if is_static {
                        object_kind::STATIC
                    } else {
                        object_kind::UNIT
                    },
                    at: Position { object: id, ..at },
                },
            );
            members.push(id);
        }
        self.groups.insert(
            gid,
            FGroup {
                name: name.to_owned(),
                entity: h.entity,
                campaign: name_campaign(name, self.campaign),
                members: members.clone(),
                route: Vec::new(),
                attack: None,
                wp: 0,
            },
        );
        self.applied_spec.insert(h.entity, h.rev);
        self.applied_order.insert(h.entity, 0);
        self.ledger.spawns.push((
            self.agent_boot,
            self.mission_gen_,
            h.entity,
            h.rev,
            entities,
        ));
        // A group's handler reads the new units, so the agent writes their
        // `OBJECT` records before the replaced group's `OBJECT_GONE` and the
        // `RESULT` (spec/commands.md, "Layout: GROUP_SPAWN"). A static's
        // `OBJECT` follows its result.
        if !is_static {
            for m in &members {
                let o = &self.objects[m];
                self.table.insert(*m, (o.entity, o.group_entity));
                let r = self.object_record(*m, 0);
                self.emit(r);
            }
            if let Some(old) = old {
                self.destroy_group(old);
            }
        }
        self.result(cmd, rc::APPLIED, gid, "", Lv::Nil);
        if is_static {
            for m in members {
                let o = &self.objects[&m];
                self.table.insert(m, (o.entity, o.group_entity));
                let r = self.object_record(m, 0);
                self.emit(r);
            }
        }
    }

    /// `STATIC_SPAWN` with `dead = true`: DCS holds a dead static under the
    /// name. Its `isExist` reads false, so the agent adds no row and writes
    /// no `OBJECT`; the `RESULT` carries the new object ID. A live static
    /// under the name is replaced, as by any spawn.
    fn spawn_ruin(
        &mut self,
        cmd: &Command,
        name: &str,
        type_name: String,
        at: Position,
        heading: f64,
    ) {
        let h = &cmd.header;
        self.graves.retain(|_, (_, g)| g.name != name);
        if let Some(old) = self
            .groups
            .iter()
            .find(|(_, g)| g.name == name)
            .map(|(id, _)| *id)
        {
            if !self.orphans.contains(&old) {
                self.ledger.replacements += 1;
            }
            self.destroy_group(old);
        }
        if let Some(old) = self.ruins.remove(name) {
            let key = (self.agent_boot, self.mission_gen_, old.campaign, old.entity);
            // A command handled outside a frame counts as written now.
            let written = self.handling_written_at.unwrap_or(self.link_clock);
            let known = self
                .ledger
                .deaths_read
                .get(&key)
                .is_some_and(|read| *read < written);
            self.ledger.ruins_replaced.push((
                self.agent_boot,
                self.mission_gen_,
                old.campaign,
                old.entity,
                old.spawned,
                known,
            ));
        }
        let object = self.next_object;
        self.next_object += 1;
        self.ruins.insert(
            name.to_owned(),
            Ruin {
                object,
                entity: h.entity,
                campaign: name_campaign(name, self.campaign),
                type_name,
                x: at.x,
                z: at.z,
                heading,
                spawned: true,
            },
        );
        self.applied_spec.insert(h.entity, h.rev);
        self.ledger
            .ruin_spawns
            .push((self.agent_boot, self.mission_gen_, h.entity, h.rev));
        self.result(cmd, rc::APPLIED, object, "", Lv::Nil);
    }

    fn destroy_group(&mut self, gid: u64) {
        let Some(g) = self.groups.remove(&gid) else {
            return;
        };
        // The marks and menu items of the group's ID go with it.
        self.marks.retain(|_, m| m.dcs_group != gid);
        self.menus.retain(|_, m| m.dcs_group != gid);
        // A group DCS destroyed leaves nothing to revive.
        self.graves
            .retain(|id, (o, _)| o.group != gid && *id != gid);
        self.ledger.failed_destroys.remove(&(
            self.agent_boot,
            self.mission_gen_,
            g.campaign,
            g.entity,
        ));
        let paused = self.paused();
        for m in g.members {
            if let Some(o) = self.objects.remove(&m)
                && paused
            {
                self.pending.insert(m, o);
            }
            self.gone(m, gone_reason::DESTROYED);
        }
    }

    fn destroy(&mut self, entity: u64) {
        if let Some(gid) = self.group_of(entity) {
            self.destroy_group(gid);
        }
        // `STATIC_DESTROY` finds a dead static by its name and destroys it
        // (LIVE, docs/results/2026-11-18-ruins.md). Nothing is left to
        // revive.
        let campaign = self.campaign;
        let gone: Vec<u64> = self
            .ruins
            .values()
            .filter(|r| r.entity == entity && entity != 0 && r.campaign == campaign)
            .map(|r| r.object)
            .collect();
        self.ruins.retain(|_, r| !gone.contains(&r.object));
        self.graves.retain(|id, _| !gone.contains(id));
        self.applied_spec.remove(&entity);
        self.applied_order.remove(&entity);
        self.options.remove(&entity);
    }

    fn capture(&self, gid: u64) -> Lv {
        let g = &self.groups[&gid];
        let items = g
            .members
            .iter()
            .map(|m| {
                let o = &self.objects[m];
                lv::obj(vec![
                    ("name", lv::s(&o.name)),
                    ("x", lv::n(o.at.x)),
                    ("y", lv::n(o.at.y)),
                    ("z", lv::n(o.at.z)),
                    (
                        "heading",
                        lv::n(self.heading.get(m).copied().unwrap_or(0.0)),
                    ),
                    ("life", lv::n(self.life.get(m).map_or(1.0, |p| p / 100.0))),
                    ("life0", lv::n(1.0)),
                ])
            })
            .collect();
        lv::list(items)
    }
}

impl SimAdapter for FakeSim {
    fn position_after(&self, _opcode: u16, _payload: &[u8]) -> u64 {
        self.in_tail + 1
    }

    fn send(&mut self, op: u16, payload: &[u8]) -> Result<u64, Detached> {
        if !self.alive || !self.attached {
            return Err(Detached);
        }
        self.in_tail += 1;
        self.link_clock += 1;
        self.written_at.insert(self.in_tail, self.link_clock);
        self.held.push_back((self.in_tail, op, payload.to_vec()));
        Ok(self.in_tail)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        let v = std::mem::take(&mut self.out);
        self.link_clock += 1;
        for r in &v {
            let record = (r.agent_boot, r.number);
            if let Some(key) = self.death_records.get(&record) {
                self.deaths_unacked.push((record, *key, self.link_clock));
            }
        }
        self.unacked.extend(v.iter().cloned());
        v
    }

    fn flush(&mut self) {
        self.inbound.extend(self.held.drain(..));
        self.unacked.clear();
        // An acknowledged record is never read again.
        for (record, key, clock) in std::mem::take(&mut self.deaths_unacked) {
            self.death_records.remove(&record);
            self.ledger.deaths_read.entry(key).or_insert(clock);
        }
    }
}

impl nanoprobe_sidecar::conformance::Target for FakeSim {
    fn advance(&mut self) {
        self.frame();
    }

    fn reload(&mut self) {
        self.load_mission();
    }

    fn reattach(&mut self) {
        self.detach();
        self.attach(self.sidecar_boot);
    }

    fn unit_type(&self) -> &str {
        "M-113"
    }

    fn spawn_point(&self) -> (f64, f64) {
        (0.0, 0.0)
    }
}
