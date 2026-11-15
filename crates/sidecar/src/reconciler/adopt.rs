//! Adoption of unmanaged units (spec/world.md, "Managed and unmanaged"; spec/commands.md,
//! "Layout: BIND").
//!
//! The sidecar gives a unit a player occupies, and its DCS group, `EntityId`s
//! and writes them to the agent in `BIND` commands. The unit is then
//! subscribed by `EntityId` and a `GROUP` scope names its group. An adopted
//! entity has no spec in the store: it is observed and reported, never
//! reconciled, ordered or destroyed. The calls that act on a store entity
//! find none for its ID; an effect may name it, which
//! [`Reconciler::is_bound`] answers.
//!
//! The adoption belongs to the agent boot, mission generation and campaign
//! it was made in, as the binding does. The agent's `OBJECT` record carries
//! the `EntityId` its binding holds: a row the agent no longer binds is bound
//! again, which covers a `HELLO` that dropped the agent's binding. After a
//! sidecar restart the same record gives back the IDs, which the store then
//! never hands out again.

use std::collections::{BTreeSet, HashMap};

use nanoprobe_transport::records::Bind;

use super::{Binding, Reconciler};
use crate::ids::EntityId;
use crate::players::Unmanaged;
use crate::sim::SimAdapter;
use crate::sim::wire::{Body, Object, object_kind};
use crate::store::Store;

/// What the reconciler holds of the units it adopted.
#[derive(Debug, Default)]
pub(super) struct Adopted {
    /// The agent boot, mission generation and campaign of the rows.
    scope: Option<(u64, u32, u64)>,
    /// The adopted units, by DCS object ID.
    rows: HashMap<u64, Binding>,
    /// The adopted DCS groups.
    groups: HashMap<u64, EntityId>,
    /// Objects whose binding the agent lacks.
    rebind: BTreeSet<u64>,
    /// The enumeration that last carried an adopted object the agent did not
    /// bind. A record the agent wrote before it took the `BIND` says the
    /// same, so a second enumeration has to agree before the row is bound
    /// again.
    unbound_in: HashMap<u64, u64>,
}

fn scope_of(store: &Store) -> Option<(u64, u32, u64)> {
    store
        .loaded()
        .map(|l| (l.agent_boot, l.mission_gen, store.campaign().0))
}

impl Adopted {
    /// Drop every row when the agent boot, generation or campaign changed,
    /// and remove the marks, drawings and menu items pinned to an adopted
    /// group: the group is not bound again in the new scope.
    fn refresh(&mut self, store: &mut Store) {
        let now = scope_of(store);
        if self.scope != now {
            let mut gone: Vec<EntityId> = self.rows.values().map(|b| b.group).collect();
            gone.extend(self.groups.values().copied());
            gone.retain(|g| g.0 != 0);
            if !gone.is_empty() {
                store.remove_markers_of_groups(&gone);
            }
            self.rows.clear();
            self.groups.clear();
            self.rebind.clear();
            self.unbound_in.clear();
            self.scope = now;
        }
    }
}

impl Reconciler {
    /// Give an unmanaged unit and its group `EntityId`s, and bind them in
    /// the agent: a `BIND` for the unit and one for its group. The same
    /// unit gives the same IDs. `None` when no mission is
    /// loaded, the store has no more IDs or the write failed.
    pub fn adopt(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        u: &Unmanaged,
    ) -> Option<(u64, u64)> {
        self.adopted.refresh(store);
        scope_of(store)?;
        if let Some(b) = self.adopted.rows.get(&u.object) {
            return Some((b.unit.0, b.group.0));
        }
        let unit = if u.entity != 0 {
            store.reserve_id(EntityId(u.entity));
            EntityId(u.entity)
        } else {
            store.adopt_id().ok()?
        };
        let known_group = self.adopted.groups.get(&u.group).copied();
        let group = match (u.group, known_group) {
            (0, _) => EntityId(0),
            (_, Some(g)) => g,
            (_, None) if u.group_entity != 0 => {
                store.reserve_id(EntityId(u.group_entity));
                EntityId(u.group_entity)
            }
            _ => store.adopt_id().ok()?,
        };
        // An ID the agent's binding already holds needs no write.
        let units = Bind {
            kind: object_kind::UNIT,
            entity: vec![unit.0],
            object: vec![u.object],
        };
        if u.entity == 0 && !self.send(store, sim, EntityId(0), 0, Body::Bind(units)) {
            return None;
        }
        // The agent binds the rows its table holds for the group: a group
        // gets a write for each unit adopted in it.
        if group.0 != 0 {
            let groups = Bind {
                kind: object_kind::GROUP,
                entity: vec![group.0],
                object: vec![u.group],
            };
            if u.group_entity == 0 && !self.send(store, sim, EntityId(0), 0, Body::Bind(groups)) {
                return None;
            }
            self.adopted.groups.insert(u.group, group);
        }
        self.adopted.rows.insert(
            u.object,
            Binding {
                unit,
                group,
                dcs_group: u.group,
            },
        );
        Some((unit.0, group.0))
    }

    /// The unit and group `EntityId`s of a DCS object: the managed binding,
    /// else the adoption.
    pub fn entity_of(&self, store: &Store, object: u64) -> Option<(u64, u64)> {
        if let Some(b) = self.binding.get(&object) {
            return Some((b.unit.0, b.group.0));
        }
        if self.adopted.scope != scope_of(store) {
            return None;
        }
        self.adopted
            .rows
            .get(&object)
            .map(|b| (b.unit.0, b.group.0))
    }

    /// Whether the entity is an adopted unit or group of the loaded mission.
    pub fn is_adopted(&self, id: EntityId) -> bool {
        id.0 != 0
            && self
                .adopted
                .rows
                .values()
                .any(|b| b.unit == id || b.group == id)
    }

    /// The adopted units, by DCS object ID.
    pub fn adopted(&self) -> &HashMap<u64, Binding> {
        &self.adopted.rows
    }

    /// Leave the adoption of a step's old scope.
    pub(super) fn adopt_refresh(&mut self, store: &mut Store) {
        self.adopted.refresh(store);
    }

    /// An `OBJECT` record: an adopted row that two enumerations found
    /// unbound by the agent, or bound to another entity, is bound again. A unit the agent binds to an
    /// entity that the store does not hold and that has no platform name was
    /// adopted by an earlier sidecar: the adoption is read back.
    pub(super) fn adopt_object(&mut self, store: &mut Store, o: &Object) {
        self.adopted.refresh(store);
        if let Some(b) = self.adopted.rows.get(&o.object) {
            if o.entity == b.unit.0 {
                self.adopted.unbound_in.remove(&o.object);
            } else if o.enumeration != 0 {
                let first = self.adopted.unbound_in.insert(o.object, o.enumeration);
                if first.is_some_and(|e| e != o.enumeration) {
                    self.adopted.rebind.insert(o.object);
                    self.adopted.unbound_in.remove(&o.object);
                }
            }
            return;
        }
        if o.entity != 0
            && o.kind == object_kind::UNIT
            && crate::ids::parse(&o.name).is_none()
            && store.unit(EntityId(o.entity)).is_none()
            && scope_of(store).is_some()
        {
            store.reserve_id(EntityId(o.entity));
            if o.group_entity != 0 {
                store.reserve_id(EntityId(o.group_entity));
                self.adopted
                    .groups
                    .insert(o.group, EntityId(o.group_entity));
            }
            self.adopted.rows.insert(
                o.object,
                Binding {
                    unit: EntityId(o.entity),
                    group: EntityId(o.group_entity),
                    dcs_group: o.group,
                },
            );
        }
    }

    /// A `SEEN` record: the rows it names are unchanged since their last
    /// `OBJECT`, so one that record found unbound is still unbound.
    pub(super) fn adopt_seen(&mut self, s: &nanoprobe_transport::records::Seen) {
        for id in &s.objects {
            if let Some(e) = self.adopted.unbound_in.get(id).copied()
                && e != s.enumeration
                && self.adopted.rows.contains_key(id)
            {
                self.adopted.rebind.insert(*id);
                self.adopted.unbound_in.remove(id);
            }
        }
    }

    /// After an enumeration of whole `OBJECT` records: remove the markers of
    /// a group that is neither managed nor adopted. A rebuilt reconciler (a
    /// sidecar restart, a campaign made active again) holds no adoption that
    /// ended while it was away, and the records just read back every one
    /// that survived.
    pub(super) fn adopt_orphans(&mut self, store: &mut Store) {
        if self.adopted.scope != scope_of(store) || self.adopted.scope.is_none() {
            return;
        }
        let mut keep: Vec<EntityId> = self.adopted.groups.values().copied().collect();
        keep.extend(self.adopted.rows.values().map(|b| b.group));
        store.remove_unbound_group_markers(&keep);
    }

    /// An `OBJECT_GONE` record: the row is no longer adopted.
    pub(super) fn adopt_gone(&mut self, object: u64) {
        self.adopted.rows.remove(&object);
        self.adopted.rebind.remove(&object);
        self.adopted.unbound_in.remove(&object);
    }

    /// Write the `BIND`s of rows the agent lost. Returns false when a write
    /// failed.
    pub(super) fn adopt_plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) -> bool {
        if self.adopted.rebind.is_empty() {
            return true;
        }
        let wanted: Vec<u64> = std::mem::take(&mut self.adopted.rebind)
            .into_iter()
            .filter(|o| self.adopted.rows.contains_key(o))
            .collect();
        if wanted.is_empty() {
            return true;
        }
        let rows: Vec<(u64, Binding)> = wanted.iter().map(|o| (*o, self.adopted.rows[o])).collect();
        let units = Bind {
            kind: object_kind::UNIT,
            entity: rows.iter().map(|(_, b)| b.unit.0).collect(),
            object: rows.iter().map(|(o, _)| *o).collect(),
        };
        let mut seen = BTreeSet::new();
        let groups: Vec<(u64, u64)> = rows
            .iter()
            .filter(|(_, b)| b.dcs_group != 0 && seen.insert(b.dcs_group))
            .map(|(_, b)| (b.group.0, b.dcs_group))
            .collect();
        let ok = self.send(store, sim, EntityId(0), 0, Body::Bind(units))
            && (groups.is_empty()
                || self.send(
                    store,
                    sim,
                    EntityId(0),
                    0,
                    Body::Bind(Bind {
                        kind: object_kind::GROUP,
                        entity: groups.iter().map(|g| g.0).collect(),
                        object: groups.iter().map(|g| g.1).collect(),
                    }),
                ));
        if !ok {
            self.adopted.rebind.extend(wanted);
        }
        ok
    }
}
