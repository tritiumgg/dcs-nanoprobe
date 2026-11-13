//! The world store (plan 4.4): typed tables keyed by `EntityId`, one revision
//! counter that stamps every change, the journal, and the spatial grid.
//!
//! This is the durable half of the sidecar. Whatever is here survives a
//! sidecar restart; the reconciler's own state does not. Step S18 puts the
//! tables and the journal in a per-campaign SQLite file; here they are in
//! memory, and a test keeps the [`Store`] across a simulated restart.

use std::collections::{BTreeMap, VecDeque};

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
}

/// One row of the binding mirror: a DCS object of the loaded mission and the
/// entities it is bound to (plan 4.4, "Binding").
///
/// The mirror is durable: a death the agent queued while no sidecar was
/// attached arrives as an `OBJECT_GONE` before the enumeration that would
/// rebuild a lost mirror, and only the mirror names the unit it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The unit, or the static.
    pub unit: EntityId,
    /// Its group, or the static.
    pub group: EntityId,
    /// The DCS group ID, 0 for a static or when not known.
    pub dcs_group: u64,
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

/// A new group's spec.
#[derive(Clone, Debug, PartialEq)]
pub struct NewGroup {
    /// The theater.
    pub theater: String,
    /// DCS country number.
    pub country: u32,
    /// DCS group category number.
    pub category: u32,
    /// Each unit's type and starting state.
    pub units: Vec<(String, UnitState)>,
    /// Who simulates it.
    pub authority: Authority,
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
    binding: BTreeMap<u64, Binding>,
    stamp: (u64, u32),
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
            binding: BTreeMap::new(),
            stamp: (0, 0),
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
        for (type_name, state) in spec.units {
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
                    type_name,
                    state,
                    dead: false,
                    missing: false,
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
                spec_rev: 1,
                order: None,
                order_rev: 0,
                progress: 0,
                state: State::Virtual,
                authority: spec.authority,
                on_lost: None,
                error: None,
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

    /// Change a unit's type: a spec change at a new revision. An
    /// `unspawnable` group becomes `materializing`.
    pub fn set_unit_type(&mut self, unit: EntityId, type_name: &str) -> Result<u64, StoreError> {
        let u = self
            .units
            .get_mut(&unit)
            .ok_or(StoreError::NoSuchEntity(unit))?;
        u.type_name = type_name.to_owned();
        let gid = u.group;
        let rev = self.bump_spec(gid)?;
        if self.groups[&gid].state == State::Unspawnable {
            self.transition(gid, Trigger::SpecChanged)?;
        }
        Ok(rev)
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
        });
        self.group_mut(id)?.state = to;
        self.touch(id);
        Ok(to)
    }

    /// Record why DCS refused a spawn.
    pub fn set_error(&mut self, id: EntityId, error: String) -> Result<(), StoreError> {
        self.group_mut(id)?.error = Some(error);
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
        for u in self.units.values_mut() {
            u.missing = false;
        }
    }

    /// A sampled position. Updates the grid and the group's order progress
    /// when the unit is its lead.
    pub fn observe_position(&mut self, unit: EntityId, at: Point) {
        let Some(u) = self.units.get_mut(&unit) else {
            return;
        };
        if u.dead {
            return;
        }
        u.state.at = at;
        let gid = u.group;
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
            }
        }
    }

    /// Sampled life and fuel.
    pub fn observe_vitals(&mut self, unit: EntityId, life: f32, fuel: f32) {
        if let Some(u) = self.units.get_mut(&unit)
            && !u.dead
        {
            u.state.life = life;
            u.state.fuel = fuel;
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

    /// The binding mirror, by DCS object ID.
    pub fn binding(&self) -> &BTreeMap<u64, Binding> {
        &self.binding
    }

    /// The binding mirror, to change.
    pub fn binding_mut(&mut self) -> &mut BTreeMap<u64, Binding> {
        &mut self.binding
    }

    /// The agent boot and mission generation of the last `FRAME` read, which
    /// stamp the records after it. Kept with the read position: a sidecar
    /// that restarts reads on from where the last one stopped, which may be
    /// after a `FRAME` and before the records it stamps.
    pub fn stamp(&self) -> (u64, u32) {
        self.stamp
    }

    /// Set the stamp from a `FRAME`.
    pub fn set_stamp(&mut self, agent_boot: u64, mission_gen: u32) {
        self.stamp = (agent_boot, mission_gen);
    }

    /// The mission last seen loaded.
    pub fn loaded(&self) -> Option<&Loaded> {
        self.loaded.as_ref()
    }

    /// Journal a newly seen loaded mission. A new agent boot or generation
    /// empties the binding mirror: DCS holds none of the earlier objects.
    pub fn set_loaded(&mut self, loaded: Loaded) {
        if self.loaded.as_ref().is_none_or(|l| {
            l.agent_boot != loaded.agent_boot || l.mission_gen != loaded.mission_gen
        }) {
            self.binding.clear();
        }
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
            units: (0..n)
                .map(|i| {
                    (
                        "M-113".to_owned(),
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
