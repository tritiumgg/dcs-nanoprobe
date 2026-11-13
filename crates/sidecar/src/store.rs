//! The world store (plan 4.4): typed tables keyed by `EntityId`, one revision
//! counter that stamps every change, the journal, and the spatial grid.
//!
//! This is the durable half of the sidecar. Whatever is here survives a
//! sidecar restart; the reconciler's own state does not. Step S18 puts the
//! tables and the journal in a per-campaign SQLite file; here they are in
//! memory, and a test keeps the [`Store`] across a simulated restart.

use std::collections::{BTreeMap, VecDeque};

use nanoprobe_transport::codec::Lv;

use crate::grid::Grid;
use crate::ids::{CampaignId, EntityId, MAX_ID};
use crate::intents::{self, Intent};
use crate::lifecycle::{self, State, Trigger};

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
    /// `getFuel()`, or 0 when never sampled.
    pub fuel: f32,
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
    /// Missing from DCS without a death fact, in this mission generation.
    pub missing: bool,
    /// The author's label. Never sent to DCS, where names hold only numbers
    /// (spec/world.md, "Names").
    pub label: String,
    /// DCS skill, empty for the default. Spec.
    pub skill: String,
    /// Further keys of the DCS unit or static table: payload, livery,
    /// callsign and so on (spec/world.md, "Lua tables in the API"). Spec.
    pub extra: Lv,
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
    /// The order, if any.
    pub order: Option<Intent>,
    /// Revision of the order, 0 for none.
    pub order_rev: u64,
    /// Progress of the order (intents::progress).
    pub progress: usize,
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
    journal: Vec<Entry>,
    changes: VecDeque<(u64, EntityId)>,
    changes_keep: usize,
    grid: Grid,
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
    /// An attack order whose target is not a group of this campaign: a unit
    /// or a static as the target is step S15's. The API answers
    /// `INVALID_FIELD`.
    UnsupportedTarget(EntityId),
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
            changes: VecDeque::new(),
            changes_keep,
            grid: Grid::new(GRID_CELL_M),
        }
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

    /// Append an entry to the journal.
    pub fn journal(&mut self, e: Entry) {
        self.journal.push(e);
    }

    /// The journal, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.journal
    }

    fn allocate(&mut self) -> Result<EntityId, StoreError> {
        if self.next_entity > MAX_ID {
            return Err(StoreError::Exhausted);
        }
        let id = EntityId(self.next_entity);
        self.next_entity += 1;
        Ok(id)
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
                    label: u.label,
                    skill: u.skill,
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
                order: None,
                order_rev: 0,
                progress: 0,
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

    /// The grid of unit positions.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    fn group_mut(&mut self, id: EntityId) -> Result<&mut Group, StoreError> {
        self.groups.get_mut(&id).ok_or(StoreError::NoSuchEntity(id))
    }

    /// Give a group an order, at a new revision. Its progress starts over.
    pub fn set_order(&mut self, id: EntityId, intent: Intent) -> Result<u64, StoreError> {
        if let Intent::Attack { target } = &intent
            && self
                .groups
                .get(target)
                .is_none_or(|t| t.kind != Kind::Group)
        {
            return Err(StoreError::UnsupportedTarget(*target));
        }
        let g = self.group_mut(id)?;
        g.order_rev += 1;
        g.order = Some(intent);
        g.progress = 0;
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

    /// Mark a unit missing or found. A dead unit stays as it is.
    pub fn set_missing(&mut self, unit: EntityId, missing: bool) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
            && u.missing != missing
        {
            u.missing = missing;
            self.touch(unit);
        }
    }

    /// Clear every unit's missing mark: a new mission generation.
    pub fn clear_missing(&mut self) {
        let marked: Vec<EntityId> = self
            .units
            .values()
            .filter(|u| u.missing)
            .map(|u| u.id)
            .collect();
        for id in marked {
            self.units.get_mut(&id).expect("unit").missing = false;
            self.touch(id);
        }
    }

    /// A sampled position. Updates the grid and the group's order progress
    /// when the unit is its lead.
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
        let lead = g.members.iter().find(|m| !self.units[*m].dead).copied();
        if lead == Some(unit)
            && let Some(order) = &g.order
        {
            let p = intents::progress(order, g.progress, at.x, at.z);
            if p != g.progress {
                self.groups.get_mut(&gid).expect("group").progress = p;
                self.touch(gid);
            }
        }
    }

    /// Sampled life and fuel.
    pub fn observe_vitals(&mut self, unit: EntityId, life: f32, fuel: f32) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
            && (u.state.life, u.state.fuel) != (life, fuel)
        {
            u.state.life = life;
            u.state.fuel = fuel;
            self.touch(unit);
        }
    }

    /// A captured state replaces the unit's.
    pub fn observe_capture(&mut self, unit: EntityId, state: UnitState) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
        {
            u.state = state;
            let theater = self.groups[&u.group].theater.clone();
            self.grid.set(unit, &theater, state.at.x, state.at.z);
            self.touch(unit);
        }
    }

    /// The command epoch, kept in the server-scoped store (spec/commands.md).
    pub fn epoch(&self) -> u64 {
        self.epoch
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

    /// Commands journalled with no outcome, in the order they were written.
    pub fn unanswered(&self) -> Vec<Journalled> {
        let mut open: BTreeMap<(u64, u64), Journalled> = BTreeMap::new();
        for e in &self.journal {
            match e {
                Entry::Command(c) => {
                    open.insert((c.epoch, c.seq), c.clone());
                }
                Entry::Outcome { epoch, seq, .. } => {
                    open.remove(&(*epoch, *seq));
                }
                _ => {}
            }
        }
        open.into_values().collect()
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
    fn an_attack_order_on_a_unit_or_a_static_is_refused() {
        let mut s = Store::new(CampaignId(1), 100);
        let a = s.create_group(spec(2)).unwrap();
        let b = s.create_group(spec(1)).unwrap();
        let unit = s.group(b).unwrap().members[0];
        let st = s.create_static(spec(1)).unwrap();
        let rev = s.group(a).unwrap().order_rev;
        for target in [unit, st, EntityId(999)] {
            assert_eq!(
                s.set_order(a, Intent::Attack { target }),
                Err(StoreError::UnsupportedTarget(target))
            );
        }
        assert_eq!(s.group(a).unwrap().order_rev, rev, "nothing changed");
        assert!(s.group(a).unwrap().order.is_none());
        assert!(s.set_order(a, Intent::Attack { target: b }).is_ok());
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
}
