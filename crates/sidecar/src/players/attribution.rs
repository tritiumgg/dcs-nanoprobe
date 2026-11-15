//! What the roster remembers of objects, so an event that names an object
//! after its row left still resolves (spec/world.md, "Kill attribution" and
//! "Launch attribution").
//!
//! The memory belongs to one mission generation of one agent boot. The
//! roster clears it at either change.

use std::collections::{BTreeMap, VecDeque};

use nanoprobe_transport::records::{Event, Object};

use crate::ids;

/// `S_EVENT_SHOT`.
pub const SHOT: u32 = 1;
/// `S_EVENT_HIT`.
pub const HIT: u32 = 2;
/// `S_EVENT_DEAD`.
pub const DEAD: u32 = 8;
/// `S_EVENT_KILL`.
pub const KILL: u32 = 28;
/// `S_EVENT_BDA`.
pub const BDA: u32 = 37;

/// `event_field` codes (spec/records.md, "Codes: event_field").
pub mod field {
    /// `TIME`
    pub const TIME: u64 = 1;
    /// `INITIATOR`
    pub const INITIATOR: u64 = 2;
    /// `TARGET`
    pub const TARGET: u64 = 3;
    /// `WEAPON`
    pub const WEAPON: u64 = 4;
}

/// The most rows, weapons and BDAs each memory keeps.
pub const KEPT: usize = 20_000;

/// A row of the agent's table, as the last `OBJECT` record gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `object_kind`.
    pub kind: u32,
    /// The DCS group ID, 0 for a static.
    pub group: u64,
    /// The `EntityId` the agent's binding holds, 0 for none.
    pub entity: u64,
    /// The group's `EntityId` the agent's binding holds, 0 for none.
    pub group_entity: u64,
    /// DCS coalition number.
    pub coalition: u32,
    /// The DCS type name.
    pub type_name: String,
    /// Whether the name is a platform name: a managed entity, or one of
    /// another campaign.
    pub platform: bool,
}

impl Row {
    /// The row an `OBJECT` record gives.
    pub fn of(o: &Object) -> Row {
        Row {
            kind: o.kind,
            group: o.group,
            entity: o.entity,
            group_entity: o.group_entity,
            coalition: o.coalition,
            type_name: o.type_name.clone(),
            platform: ids::parse(&o.name).is_some(),
        }
    }
}

/// A row that left the table.
#[derive(Clone, Debug, PartialEq)]
pub struct Departed {
    /// The unit's `EntityId`, 0 for none.
    pub entity: u64,
    /// The DCS type name.
    pub type_name: String,
    /// DCS coalition number.
    pub coalition: u32,
    /// The keys of the players who occupied it when it left.
    pub keys: Vec<String>,
    /// Model time of the frame in which the row left.
    pub left: f64,
}

/// How a `HIT` or `KILL` was tied to the players who launched its weapon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchRule {
    /// The event names a weapon whose shooter was seen at `SHOT`.
    Weapon,
    /// The initiator's occupants before it left.
    Initiator,
}

/// What the roster adds to a world event.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attribution {
    /// The players in the initiator, now or when it left.
    pub initiator_players: Vec<String>,
    /// The players in the target, now or when it left.
    pub target_players: Vec<String>,
    /// The initiator's type name from the table or the departed row.
    pub initiator_type: Option<String>,
    /// The target's type name from the table or the departed row.
    pub target_type: Option<String>,
    /// The initiator's unit `EntityId` when its row has left.
    pub initiator_entity: Option<u64>,
    /// The target's unit `EntityId` when its row has left.
    pub target_entity: Option<u64>,
    /// The players who launched the weapon of a `HIT` or `KILL`.
    pub launch_players: Vec<String>,
    /// How `launch_players` was found.
    pub launch_rule: Option<LaunchRule>,
    /// Model time of the kill: the victim's `BDA`, else the time its row
    /// left.
    pub dated_time: Option<f64>,
}

/// The memory.
#[derive(Debug, Default)]
pub struct Memory {
    pub(super) rows: BTreeMap<u64, Row>,
    departed: BTreeMap<u64, Departed>,
    order: VecDeque<u64>,
    bda: BTreeMap<u64, f64>,
    weapons: BTreeMap<u64, Vec<String>>,
}

impl Memory {
    /// Forget everything: a new generation or agent boot.
    pub fn clear(&mut self) {
        *self = Memory::default();
    }

    /// The departed row of an object.
    pub fn departed(&self, object: u64) -> Option<&Departed> {
        self.departed.get(&object)
    }

    /// A row left the table. The first departure of an object is kept: a
    /// later `DEAD` for the same object fills in what the first lacked.
    pub fn leave(&mut self, object: u64, entity: u64, keys: Vec<String>, now: f64) {
        let row = self.rows.remove(&object);
        if let Some(d) = self.departed.get_mut(&object) {
            if d.entity == 0 {
                d.entity = entity;
            }
            return;
        }
        let (type_name, coalition, row_entity) = row
            .map(|r| (r.type_name, r.coalition, r.entity))
            .unwrap_or_default();
        self.departed.insert(
            object,
            Departed {
                entity: if entity != 0 { entity } else { row_entity },
                type_name,
                coalition,
                keys,
                left: now,
            },
        );
        self.order.push_back(object);
        while self.order.len() > KEPT {
            if let Some(o) = self.order.pop_front() {
                self.departed.remove(&o);
            }
        }
    }

    /// A `BDA` named a target.
    pub fn bda(&mut self, target: u64, time: f64) {
        self.bda.insert(target, time);
        while self.bda.len() > KEPT {
            self.bda.pop_first();
        }
    }

    /// A `SHOT`: the weapon and the players who were in its shooter.
    pub fn shot(&mut self, weapon: u64, keys: Vec<String>) {
        self.weapons.insert(weapon, keys);
        while self.weapons.len() > KEPT {
            self.weapons.pop_first();
        }
    }

    /// The players of a weapon seen at `SHOT`.
    pub fn weapon(&self, weapon: u64) -> Option<&Vec<String>> {
        self.weapons.get(&weapon)
    }

    /// When a victim died: its `BDA`, else the time its row left.
    pub fn dated(&self, victim: u64) -> Option<f64> {
        self.bda
            .get(&victim)
            .copied()
            .or_else(|| self.departed.get(&victim).map(|d| d.left))
    }
}

/// The object a numeric event field names.
pub fn object_of(e: &Event, code: u64) -> Option<u64> {
    e.number(code)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| n as u64)
}
