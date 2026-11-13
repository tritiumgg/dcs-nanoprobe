//! A fake DCS behind the sidecar's simulation adapter, for development and CI.
//!
//! [`FakeSim`] models a DCS process with the agent in it, at the level of the
//! agent's protocol: it reads commands from an inbound queue and writes the
//! records spec/records.md describes. It follows spec/commands.md for epochs,
//! de-duplication, revisions and generations, and spec/records.md for the
//! object table, enumerations, the event queue and `GAP`. Time moves only
//! when the caller runs [`FakeSim::frame`], and nothing in it is random: a
//! test that drives it from a seed replays exactly.
//!
//! Faults are methods: kill a unit, remove an object with or without an
//! event, detach and attach the sidecar, load a mission, restart DCS. The
//! [`Ledger`] records the ground truth that the reconciler's tests check
//! against.
//!
//! What the fake does not model: pause and the pending-destroy set (a
//! destroy is immediate), `CONTROL_LOST`, the commanded-work budget in time
//! (it takes a fixed number of commands per frame), effects, and every
//! opcode the reconciler does not write, which it answers `UNKNOWN_OPCODE`.

pub mod harness;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use nanoprobe_sidecar::ids;
use nanoprobe_sidecar::intents::{Waypoint, route_points};
use nanoprobe_sidecar::sim::wire::{
    Body, Command, Enumeration, Input, Object, ObjectGone, Position, gone_reason, lv, object_kind,
    opcode,
};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{Class, gap_class, lifecycle as lc, record_class, result as rc};
use nanoprobe_transport::records::{CommandResult, Frame, Gap, Hello, Lifecycle};

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
        }
    }
}

/// Ground truth for the tests.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    /// Unit entities that died in DCS.
    pub deaths: BTreeSet<u64>,
    /// Spawns that replaced a group DCS still held under the name.
    pub replacements: u64,
    /// Spawns that reached the handler: agent boot, generation, group entity,
    /// spec revision, and the unit entities spawned.
    pub spawns: Vec<(u64, u32, u64, u64, Vec<u64>)>,
    /// Orders that reached the handler, by agent boot, DCS group ID, group
    /// entity and order revision. A respawned group is a new DCS group.
    pub orders: HashMap<(u64, u64, u64, u64), u32>,
    /// Sequenced commands the agent took: agent boot, epoch, sequence number.
    pub taken: BTreeSet<(u64, u64, u64)>,
    /// Unit entities that left DCS without a death, by agent boot and
    /// generation.
    pub removed: BTreeSet<(u64, u32, u64)>,
    /// Unit entities whose death or removal record the agent's event queue
    /// dropped, by agent boot and generation.
    pub unreported: BTreeSet<(u64, u32, u64)>,
}

#[derive(Clone, Debug)]
struct FObject {
    name: String,
    type_name: String,
    /// DCS group ID, 0 for a static.
    group: u64,
    entity: u64,
    group_entity: u64,
    kind: u32,
    at: Position,
}

#[derive(Clone, Debug)]
struct FGroup {
    name: String,
    entity: u64,
    members: Vec<u64>,
    route: Vec<Waypoint>,
    wp: usize,
}

#[derive(Clone, Debug)]
struct Stamp {
    frame: u64,
    model_time: f64,
    mission_gen_: u32,
    lifecycle: u32,
}

#[derive(Clone, Debug)]
struct Run {
    id: u64,
    rows: VecDeque<u64>,
    written: u32,
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
    table: BTreeSet<u64>,
    run: Option<Run>,
    /// The unit entity of each object that left DCS, for the ledger.
    gone_entities: HashMap<u64, u64>,
    enumerations: u64,
    next_enum: u64,
}

impl FakeSim {
    /// A DCS process that has started and is loading its first mission. No
    /// sidecar is attached.
    pub fn new(cfg: Config) -> FakeSim {
        let boot = cfg.first_boot;
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
            table: BTreeSet::new(),
            run: None,
            gone_entities: HashMap::new(),
            enumerations: 0,
            next_enum: 0,
        };
        f.load_mission();
        f
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

    /// The platform groups and statics DCS holds: entity, platform name and
    /// the unit entities alive in it.
    pub fn platform_groups(&self) -> Vec<(u64, String, Vec<u64>)> {
        self.groups
            .values()
            .filter(|g| g.entity != 0)
            .map(|g| {
                let units = g.members.iter().map(|o| self.objects[o].entity).collect();
                (g.entity, g.name.clone(), units)
            })
            .collect()
    }

    /// The order revision the agent holds for a group entity.
    pub fn applied_order(&self, entity: u64) -> u64 {
        self.applied_order.get(&entity).copied().unwrap_or(0)
    }

    /// The DCS object IDs alive in the world, with the unit entity each
    /// carries (0 for an unmanaged one).
    pub fn objects(&self) -> Vec<(u64, u64)> {
        self.objects.iter().map(|(o, x)| (*o, x.entity)).collect()
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            frame: self.frame,
            model_time: self.model_time,
            mission_gen_: self.mission_gen_,
            lifecycle: self.lifecycle,
        }
    }

    fn write(&mut self, input: &Input) {
        let (kind, payload) = input.encode();
        self.number += 1;
        self.out.push(RawRecord {
            agent_boot: self.agent_boot,
            number: self.number,
            kind,
            payload,
        });
    }

    fn frame_record(&mut self, s: &Stamp, replay: bool) {
        let f = Frame {
            frame: s.frame,
            model_time: s.model_time,
            real_time: s.model_time,
            campaign: self.campaign,
            number: self.number + 2,
            mission_gen: s.mission_gen_,
            lifecycle: s.lifecycle,
            replay: u32::from(replay),
        };
        self.write(&Input::Frame(f));
    }

    /// Emit a record: written when attached, else queued by its class.
    fn emit(&mut self, input: Input) {
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
                    if let Input::ObjectGone(g) = dropped
                        && let Some(e) = self.gone_entities.get(&g.object)
                    {
                        self.ledger
                            .unreported
                            .insert((self.agent_boot, s.mission_gen_, *e));
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
            self.cfg.theater.clone()
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
        if !self.alive {
            return;
        }
        self.mission_gen_ += 1;
        self.groups.clear();
        self.objects.clear();
        self.table.clear();
        self.run = None;
        self.applied_spec.clear();
        self.applied_order.clear();
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
        self.out.clear();
    }

    /// Whether the process is up.
    pub fn alive(&self) -> bool {
        self.alive
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
            if last_frame != Some(s.frame) {
                self.frame_record(&s, true);
                last_frame = Some(s.frame);
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
        if self.running() {
            self.start_enumeration();
        }
    }

    /// A unit or static dies: a death event, and the row leaves the table.
    pub fn kill(&mut self, object: u64) {
        let Some(o) = self.remove_object(object) else {
            return;
        };
        if o.entity != 0 {
            self.ledger.deaths.insert(o.entity);
        }
        if self.table.remove(&object) {
            self.emit(Input::Event(vec![8, 0, 0, 0, 0, 0, 0, 0]));
            self.emit(Input::ObjectGone(ObjectGone {
                object,
                reason: gone_reason::DEAD,
            }));
        }
    }

    /// An object leaves DCS with no event. Its row stays in the table until
    /// a full enumeration does not find it.
    pub fn remove_silently(&mut self, object: u64) {
        if let Some(o) = self.remove_object(object) {
            self.ledger
                .removed
                .insert((self.agent_boot, self.mission_gen_, o.entity));
        }
    }

    /// An object leaves DCS with a removal event.
    pub fn remove_with_event(&mut self, object: u64) {
        let Some(o) = self.remove_object(object) else {
            return;
        };
        self.ledger
            .removed
            .insert((self.agent_boot, self.mission_gen_, o.entity));
        if self.table.remove(&object) {
            self.emit(Input::ObjectGone(ObjectGone {
                object,
                reason: gone_reason::REMOVED,
            }));
        }
    }

    fn remove_object(&mut self, object: u64) -> Option<FObject> {
        let o = self.objects.remove(&object)?;
        self.gone_entities.insert(object, o.entity);
        let gid = if o.kind == object_kind::STATIC {
            object
        } else {
            o.group
        };
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
                        kind: object_kind::UNIT,
                        at: Position { object: id, ..at },
                    },
                );
                members.push(id);
            }
        }
        for m in &members {
            self.table.insert(*m);
        }
        self.groups.insert(
            gid,
            FGroup {
                name: name.to_owned(),
                entity,
                members,
                route: Vec::new(),
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
        }));
        self.run = Some(Run {
            id,
            rows: self.table.iter().copied().collect(),
            written: 0,
        });
        self.next_enum = self.frame + self.cfg.enum_period;
    }

    fn enumeration_slice(&mut self) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        for _ in 0..self.cfg.enum_slice {
            let Some(row) = run.rows.pop_front() else {
                break;
            };
            if !self.table.contains(&row) {
                continue;
            }
            if self.objects.contains_key(&row) {
                let r = self.object_record(row, run.id);
                self.emit(r);
                run.written += 1;
            } else {
                self.table.remove(&row);
                self.emit(Input::ObjectGone(ObjectGone {
                    object: row,
                    reason: gone_reason::MISSING,
                }));
            }
        }
        if run.rows.is_empty() {
            self.emit(Input::EnumEnd(Enumeration {
                enumeration: run.id,
                objects: run.written,
            }));
        } else {
            self.run = Some(run);
        }
    }

    // ---- the frame ------------------------------------------------------

    /// Run one frame.
    pub fn frame(&mut self) {
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
        self.model_time += 0.1;
        if self.attached {
            let s = self.stamp();
            self.frame_record(&s, false);
            for _ in 0..self.cfg.commands_per_frame {
                let Some((pos, op, payload)) = self.inbound.pop_front() else {
                    break;
                };
                self.in_head = pos;
                if let Ok(Some(c)) = Command::decode(op, &payload)
                    && !c.body.is_control()
                {
                    self.ledger
                        .taken
                        .insert((self.agent_boot, c.header.epoch, c.header.seq));
                }
                self.handle(op, &payload);
            }
        }
        self.move_groups();
        if self.attached {
            if self.run.is_none() && self.frame >= self.next_enum {
                self.start_enumeration();
            }
            self.enumeration_slice();
            if self.frame.is_multiple_of(self.cfg.positions_every) {
                let rows: Vec<Position> = self
                    .table
                    .iter()
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
                    if c != self.campaign {
                        self.campaign = c;
                        self.applied_spec.clear();
                        self.applied_order.clear();
                    }
                }
                Body::Enumerate => self.start_enumeration(),
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
                self.applied_order.insert(h.entity, h.rev);
                *self
                    .ledger
                    .orders
                    .entry((self.agent_boot, gid, h.entity, h.rev))
                    .or_default() += 1;
                self.result(&cmd, rc::APPLIED, gid, "", Lv::Nil);
            }
            Body::GroupDestroy | Body::StaticDestroy => {
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
            Body::Campaign(_) | Body::Enumerate | Body::Burst(_) => unreachable!("control"),
        }
    }

    fn group_of(&self, entity: u64) -> Option<u64> {
        self.groups
            .iter()
            .find(|(_, g)| g.entity == entity && entity != 0)
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
        if let Some(old) = self
            .groups
            .iter()
            .find(|(_, g)| g.name == name)
            .map(|(id, _)| *id)
        {
            self.ledger.replacements += 1;
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
                members: members.clone(),
                route: Vec::new(),
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
            self.table.insert(m);
            let r = self.object_record(m, 0);
            self.emit(r);
        }
    }

    fn destroy_group(&mut self, gid: u64) {
        let Some(g) = self.groups.remove(&gid) else {
            return;
        };
        for m in g.members {
            self.objects.remove(&m);
            if self.table.remove(&m) {
                self.emit(Input::ObjectGone(ObjectGone {
                    object: m,
                    reason: gone_reason::DESTROYED,
                }));
            }
        }
    }

    fn destroy(&mut self, entity: u64) {
        if let Some(gid) = self.group_of(entity) {
            self.destroy_group(gid);
        }
        self.applied_spec.remove(&entity);
        self.applied_order.remove(&entity);
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
                    ("life", lv::n(1.0)),
                    ("fuel", lv::n(0.5)),
                ])
            })
            .collect();
        lv::list(items)
    }

    /// The unit entity's DCS object, if it is alive.
    pub fn object_of(&self, entity: u64) -> Option<u64> {
        self.objects
            .iter()
            .find(|(_, o)| o.entity == entity && entity != 0)
            .map(|(id, _)| *id)
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

    fn unit_type(&self) -> &str {
        "M-113"
    }

    fn spawn_point(&self) -> (f64, f64) {
        (0.0, 0.0)
    }
}
