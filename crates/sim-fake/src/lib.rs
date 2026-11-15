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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use nanoprobe_sidecar::ids;
use nanoprobe_sidecar::intents::{Waypoint, route_points};
use nanoprobe_sidecar::sim::wire::{
    Body, Command, Enumeration, Input, Object, ObjectGone, Position, Seen, gone_reason, lv,
    object_kind, opcode,
};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::stock::{Item, Stock};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{Class, gap_class, lifecycle as lc, record_class, result as rc};
use nanoprobe_transport::records::{
    Airbase, CommandResult, Frame, Gap, Hello, Lifecycle, Rates, Warehouse,
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

/// The campaign a platform name gives, 0 for any other name.
fn name_campaign(name: &str) -> u64 {
    ids::parse(name).map_or(0, |p| p.campaign.0)
}

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
            step_m: 100.0,
            first_boot: 0x1000,
            death_delay_max: 0.0,
            death_seed: 0,
            warehouse_every: 5,
            enum_end_hold: 0,
        }
    }
}

/// The coalition of each country the fake knows, answered to `COUNTRIES`:
/// country 0 is red, country 2 is blue (DCS numbering).
pub const COUNTRY_COALITIONS: [(u32, u32); 2] = [(0, 1), (2, 2)];

/// One airbase of the mission: its DCS object ID, owner and warehouse.
#[derive(Clone, Debug, PartialEq)]
struct FAirbase {
    object: u64,
    coalition: u32,
    stock: Stock,
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
    // The link.
    attached: bool,
    sidecar_boot: u64,
    inbound: VecDeque<(u64, u16, Vec<u8>)>,
    in_tail: u64,
    in_head: u64,
    out: Vec<RawRecord>,
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
    /// The airbases as DCS holds them now.
    airbases: BTreeMap<String, FAirbase>,
    /// Controller options applied, by group entity.
    options: BTreeMap<u64, BTreeMap<u32, Lv>>,
    /// Life of a unit object as a percentage of its initial life, when set.
    life: BTreeMap<u64, f64>,
    /// The sample rates last received.
    rates: Rates,
    rates_received: u32,
    /// The DCS object an `AttackUnit` task names, by DCS group.
    attack_unit: BTreeMap<u64, u64>,
    /// Objects whose destroy is pending until the resume, by object ID.
    pending: BTreeMap<u64, FObject>,
    /// The restore burst (spec/commands.md, `BURST`).
    burst: bool,
}

impl FakeSim {
    /// A DCS process that has started and is loading its first mission. No
    /// sidecar is attached.
    pub fn new(cfg: Config) -> FakeSim {
        let boot = cfg.first_boot;
        let cfg_seed = cfg.death_seed;
        let mut f = FakeSim {
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
            applied_order: HashMap::new(),
            attached: false,
            sidecar_boot: 0,
            inbound: VecDeque::new(),
            in_tail: 0,
            in_head: 0,
            out: Vec::new(),
            queue: VecDeque::new(),
            gap: None,
            groups: BTreeMap::new(),
            objects: BTreeMap::new(),
            table: BTreeMap::new(),
            run: None,
            late: Vec::new(),
            graves: BTreeMap::new(),
            rng: cfg_seed,
            theater: String::new(),
            lose_result: false,
            written_stamp: (0, 0),
            refuse_destroys: 0,
            orphans: BTreeSet::new(),
            losing: false,
            enumerations: 0,
            next_enum: 0,
            hidden: BTreeSet::new(),
            sent: BTreeSet::new(),
            full: true,
            airbase_seed: BTreeMap::new(),
            airbases: BTreeMap::new(),
            options: BTreeMap::new(),
            life: BTreeMap::new(),
            rates: Rates::default(),
            rates_received: 0,
            attack_unit: BTreeMap::new(),
            pending: BTreeMap::new(),
            burst: false,
        };
        f.load_mission();
        f
    }

    /// Add an airbase to the mission, as the mission file has it. It is in
    /// DCS now and after each mission load.
    pub fn add_airbase(&mut self, name: &str, coalition: u32, stock: Stock) {
        let object = 1_000_000 + self.airbase_seed.len() as u64;
        let a = FAirbase {
            object,
            coalition,
            stock,
        };
        self.airbase_seed.insert(name.to_owned(), a.clone());
        self.airbases.insert(name.to_owned(), a);
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

    /// The life, as a percentage of the initial life, of a unit entity's
    /// object, when a `DAMAGE` set it.
    pub fn life_percent_of(&self, entity: u64) -> Option<f64> {
        let (o, _) = self.objects.iter().find(|(_, x)| x.entity == entity)?;
        self.life.get(o).copied()
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

    /// The ground truth.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
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
                self.ledger.reported_deaths.insert((
                    self.agent_boot,
                    mission_gen,
                    campaign,
                    g.entity,
                ));
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
            life0: 1.0,
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
        self.applied_spec.clear();
        self.applied_order.clear();
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
        *self = FakeSim::new(Config {
            first_boot: boot,
            ..cfg
        });
        self.ledger = ledger;
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
        // The binding the kept rows were made under is gone: a death event
        // after the switch names no entity of either campaign.
        self.late.clear();
        // Groups in DCS whose applied revision the agent no longer holds: a
        // spawn under one of their names replaces it, as spec/commands.md
        // allows between a switch and the next load.
        self.orphans.extend(self.groups.keys().copied());
        // Every row's payload is encoded again under the new binding.
        self.sent.clear();
        for (object, b) in self.table.iter_mut() {
            *b = match self.objects.get(object) {
                Some(o) if o.campaign == c && c != 0 => (o.entity, o.group_entity),
                _ => (0, 0),
            };
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
            let bound = if campaign == self.campaign {
                (*u, group)
            } else {
                (0, 0)
            };
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

    /// The next `n` destroys of any entity raise in DCS and are answered
    /// `DCS_REFUSED`.
    pub fn refuse_destroys(&mut self, n: u32) {
        self.refuse_destroys = n;
    }

    /// The `OBJECT_GONE` `DEAD` records written and not yet read, as
    /// [`Ledger::reported_deaths`] keys them.
    pub fn unread_deaths(&self) -> BTreeSet<(u64, u32, u64, u64)> {
        let mut stamp = (0, 0);
        let mut out = BTreeSet::new();
        for r in &self.out {
            match Input::decode(r.kind, &r.payload) {
                Ok(Input::Frame(f)) => stamp = (f.mission_gen, f.campaign),
                Ok(Input::ObjectGone(g)) if g.reason == gone_reason::DEAD && g.entity != 0 => {
                    out.insert((r.agent_boot, stamp.0, stamp.1, g.entity));
                }
                _ => {}
            }
        }
        out
    }

    /// The link drops: the agent detaches and queues its records.
    pub fn detach(&mut self) {
        self.attached = false;
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
        self.kill_ledger(&o);
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
        self.kill_ledger(&o);
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

    fn kill_ledger(&mut self, o: &FObject) {
        if o.entity != 0 {
            self.ledger.deaths.insert((o.campaign, o.entity));
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
                    campaign: name_campaign(name),
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
                campaign: name_campaign(name),
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
                if let Ok(Some(c)) = Command::decode(op, &payload)
                    && !c.body.is_control()
                {
                    self.ledger.taken.insert(
                        (self.agent_boot, c.header.epoch, c.header.seq),
                        c.header.campaign,
                    );
                }
                self.handle(op, &payload);
            }
            self.ledger.most_per_frame = self.ledger.most_per_frame.max(taken);
        }
        if !paused {
            self.move_groups();
        }
        if self.attached {
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
        // The extension opcodes need a campaign (the real admission table's
        // NO_CAMPAIGN row, before the revision check) but not a generation.
        if matches!(
            cmd.body,
            Body::ExtensionSet { .. } | Body::ExtensionInvoke { .. }
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
            self.applied_extension.insert(name.clone(), h.rev);
            let key = (self.agent_boot, self.campaign, name, h.rev);
            *self.ledger.extension_runs.entry(key).or_default() += 1;
            return self.result(&cmd, rc::APPLIED, 0, "", Lv::Nil);
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
            opcode::GROUP_SPAWN | opcode::STATIC_SPAWN => Some(&self.applied_spec),
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
                let units: Vec<(String, String, Position)> = lv::get(group, "units")
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
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if units.is_empty() || units.iter().any(|(_, t, _)| t.starts_with("Unknown")) {
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
                self.spawn(&cmd, name, vec![(name.clone(), t, at)], true);
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
            Body::ExtensionSet { .. } | Body::ExtensionInvoke { .. } => {
                unreachable!("answered above")
            }
            Body::Campaign(_) | Body::Enumerate | Body::Burst(_) | Body::Rates(_) => {
                unreachable!("control")
            }
        }
    }

    /// Whether DCS holds what a `SOUND` or `PICTURE` scope names: a group
    /// or unit of the campaign for `GROUP` and `UNIT` (never with campaign
    /// 0), anything for the other scopes.
    fn scope_held(&self, campaign: u64, scope: u32, target: u64) -> bool {
        match scope {
            3 => campaign != 0 && self.group_in(campaign, target).is_some(),
            4 => {
                campaign != 0
                    && self
                        .objects
                        .values()
                        .any(|x| x.entity == target && x.campaign == campaign)
            }
            _ => true,
        }
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
        units: Vec<(String, String, Position)>,
        is_static: bool,
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
                if h.rev > self.applied_spec.get(&h.entity).copied().unwrap_or(0) {
                    self.ledger.in_place += 1;
                }
            }
            self.destroy_group(old);
        }
        let gid = self.next_object;
        let mut members = Vec::new();
        let mut entities = Vec::new();
        self.next_object += 1;
        for (i, (uname, t, at)) in units.into_iter().enumerate() {
            let id = if is_static { gid } else { self.next_object };
            if !is_static {
                self.next_object += 1;
            }
            let entity = ids::parse(&uname).map_or(0, |p| p.entity.0);
            entities.push(entity);
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
                campaign: name_campaign(name),
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
        self.result(cmd, rc::APPLIED, gid, "", Lv::Nil);
        for m in members {
            let o = &self.objects[&m];
            self.table.insert(m, (o.entity, o.group_entity));
            let r = self.object_record(m, 0);
            self.emit(r);
        }
    }

    fn destroy_group(&mut self, gid: u64) {
        let Some(g) = self.groups.remove(&gid) else {
            return;
        };
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
                    ("heading", lv::n(0.0)),
                    ("life", lv::n(self.life.get(m).map_or(1.0, |p| p / 100.0))),
                    ("fuel", lv::n(0.5)),
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
        self.inbound.push_back((self.in_tail, op, payload.to_vec()));
        Ok(self.in_tail)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        std::mem::take(&mut self.out)
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
