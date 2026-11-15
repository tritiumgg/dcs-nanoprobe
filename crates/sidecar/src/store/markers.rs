//! Marks, drawings and menu items, and the Lua extensions the controller
//! removed (spec/world.md, "Entities"; spec/commands.md, "Marks, drawings and
//! menu items in DCS").
//!
//! A marker is a set-state entity with an `EntityId` and a spec revision,
//! journalled as `Entry::Spec` and `Entry::Removed`. The store keeps it
//! across mission loads; the reconciler sets it again after each one. A
//! removed marker stays in the table, so its ID is never reused and DCS is
//! still told to drop it.

use std::collections::{BTreeMap, BTreeSet};

use nanoprobe_transport::codec::Lv;

use super::{Entry, Point, Store};
use crate::ids::EntityId;

/// `scope` codes a marker takes (spec/commands.md, "Codes: scope").
pub mod scope {
    /// Everyone.
    pub const ALL: u32 = 1;
    /// One coalition.
    pub const COALITION: u32 = 2;
    /// One group.
    pub const GROUP: u32 = 3;
}

/// What a marker is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    /// A map mark.
    Mark,
    /// A map drawing.
    Drawing,
    /// An F10 menu item.
    MenuItem,
}

/// A marker's own fields.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkerSpec {
    /// A map mark.
    Mark {
        /// The theater the point is on. The mark is set while it is loaded.
        theater: String,
        /// The point.
        at: Point,
        /// The text.
        text: String,
        /// Whether players cannot change it.
        read_only: bool,
    },
    /// A map drawing.
    Drawing {
        /// The DCS shape ID.
        shape: u32,
        /// The shape's arguments after the coalition and the ID, as an array.
        args: Lv,
    },
    /// An F10 menu item.
    MenuItem {
        /// The parent submenu, `EntityId(0)` for the root.
        parent: EntityId,
        /// Whether it is a submenu rather than a command.
        submenu: bool,
        /// The text.
        text: String,
    },
}

/// A mark, drawing or menu item the controller set.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    /// The entity.
    pub id: EntityId,
    /// 1 when created, one more at each change, and at each time the
    /// reconciler needs the agent to apply it again.
    pub spec_rev: u64,
    /// The controller removed it.
    pub removed: bool,
    /// A `scope` code.
    pub scope: u32,
    /// By scope: 0, a coalition number or a group `EntityId`.
    pub target: u64,
    /// The fields.
    pub spec: MarkerSpec,
}

impl Marker {
    /// What it is.
    pub fn kind(&self) -> MarkerKind {
        match self.spec {
            MarkerSpec::Mark { .. } => MarkerKind::Mark,
            MarkerSpec::Drawing { .. } => MarkerKind::Drawing,
            MarkerSpec::MenuItem { .. } => MarkerKind::MenuItem,
        }
    }

    /// The parent of a menu item, `EntityId(0)` for any other marker.
    pub fn parent(&self) -> EntityId {
        match self.spec {
            MarkerSpec::MenuItem { parent, .. } => parent,
            _ => EntityId(0),
        }
    }
}

/// A change to a marker the store refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerError {
    /// No such marker, or it was removed.
    NoSuchMarker(EntityId),
    /// The entity is a marker of another kind.
    WrongKind(EntityId),
    /// The scope is not one the kind takes.
    BadScope(u32),
    /// The parent is not a submenu of the same scope and target, or the
    /// item would be its own ancestor.
    BadParent(EntityId),
    /// A submenu with items in it cannot change its scope or target, or
    /// become a command.
    HasChildren(EntityId),
    /// No more `EntityId`s below 2^53.
    Exhausted,
}

/// The tables of this module.
#[derive(Clone, Debug, Default)]
pub struct Markers {
    items: BTreeMap<EntityId, Marker>,
    /// The highest revision of each extension the controller removed, so a
    /// registration of the same name again carries a higher one than the
    /// agent may still hold.
    ext_floor: BTreeMap<String, u64>,
    /// Extensions removed and not yet confirmed gone from the agent.
    ext_removed: BTreeSet<String>,
}

fn scope_ok(kind: MarkerKind, scope: u32) -> bool {
    match kind {
        MarkerKind::Drawing => matches!(scope, scope::ALL | scope::COALITION),
        _ => matches!(scope, scope::ALL | scope::COALITION | scope::GROUP),
    }
}

impl Store {
    /// A marker, removed ones included.
    pub fn marker(&self, id: EntityId) -> Option<&Marker> {
        self.markers.items.get(&id)
    }

    /// Every marker, removed ones included, by ID.
    pub fn markers(&self) -> impl Iterator<Item = &Marker> {
        self.markers.items.values()
    }

    /// The items directly under a submenu that were not removed.
    pub fn menu_children(&self, parent: EntityId) -> Vec<EntityId> {
        self.markers
            .items
            .values()
            .filter(|m| !m.removed && m.kind() == MarkerKind::MenuItem && m.parent() == parent)
            .map(|m| m.id)
            .collect()
    }

    /// The number of submenus above an item, 0 for an item at the root.
    pub fn menu_depth(&self, id: EntityId) -> usize {
        let mut depth = 0;
        let mut at = id;
        while let Some(m) = self.markers.items.get(&at) {
            let p = m.parent();
            if p.0 == 0 || depth > self.markers.items.len() {
                break;
            }
            depth += 1;
            at = p;
        }
        depth
    }

    /// Add a marker (`id` 0), or replace the spec of one. A spec the marker
    /// already has changes nothing. Returns the ID and the spec revision.
    pub fn put_marker(
        &mut self,
        id: EntityId,
        scope: u32,
        target: u64,
        spec: MarkerSpec,
    ) -> Result<(EntityId, u64), MarkerError> {
        let kind = match spec {
            MarkerSpec::Mark { .. } => MarkerKind::Mark,
            MarkerSpec::Drawing { .. } => MarkerKind::Drawing,
            MarkerSpec::MenuItem { .. } => MarkerKind::MenuItem,
        };
        if !scope_ok(kind, scope) {
            return Err(MarkerError::BadScope(scope));
        }
        let old = if id.0 == 0 {
            None
        } else {
            let m = self
                .markers
                .items
                .get(&id)
                .filter(|m| !m.removed)
                .ok_or(MarkerError::NoSuchMarker(id))?;
            if m.kind() != kind {
                return Err(MarkerError::WrongKind(id));
            }
            Some(m)
        };
        if let MarkerSpec::MenuItem {
            parent, submenu, ..
        } = &spec
        {
            if parent.0 != 0 {
                let ok = self.markers.items.get(parent).is_some_and(|p| {
                    !p.removed
                        && p.scope == scope
                        && p.target == target
                        && matches!(p.spec, MarkerSpec::MenuItem { submenu: true, .. })
                });
                // No item is its own ancestor.
                let mut up = *parent;
                let mut hops = 0;
                let mut cycle = false;
                while up.0 != 0 && hops <= self.markers.items.len() {
                    if up == id {
                        cycle = true;
                        break;
                    }
                    up = self
                        .markers
                        .items
                        .get(&up)
                        .map_or(EntityId(0), Marker::parent);
                    hops += 1;
                }
                if !ok || cycle {
                    return Err(MarkerError::BadParent(*parent));
                }
            }
            if let Some(m) = old
                && (m.scope != scope || m.target != target || !*submenu)
                && !self.menu_children(id).is_empty()
            {
                return Err(MarkerError::HasChildren(id));
            }
        }
        if let Some(m) = old
            && m.scope == scope
            && m.target == target
            && m.spec == spec
        {
            return Ok((id, m.spec_rev));
        }
        let (id, rev) = match old {
            Some(m) => (id, m.spec_rev + 1),
            None => (self.allocate().map_err(|_| MarkerError::Exhausted)?, 1),
        };
        self.markers.items.insert(
            id,
            Marker {
                id,
                spec_rev: rev,
                removed: false,
                scope,
                target,
                spec,
            },
        );
        self.journal(Entry::Spec { entity: id, rev });
        self.touch(id);
        Ok((id, rev))
    }

    /// Remove a marker. A submenu takes the items under it, at every depth.
    /// A marker already removed is left as it is. Returns the IDs removed
    /// now, the items under a submenu first.
    pub fn remove_marker(&mut self, id: EntityId) -> Result<Vec<EntityId>, MarkerError> {
        let m = self
            .markers
            .items
            .get(&id)
            .ok_or(MarkerError::NoSuchMarker(id))?;
        if m.removed {
            return Ok(Vec::new());
        }
        let mut order = Vec::new();
        fn below(s: &Store, id: EntityId, out: &mut Vec<EntityId>) {
            for c in s.menu_children(id) {
                below(s, c, out);
            }
            out.push(id);
        }
        below(self, id, &mut order);
        for e in &order {
            self.markers.items.get_mut(e).expect("marker").removed = true;
            self.journal(Entry::Removed { entity: *e });
            self.touch(*e);
        }
        Ok(order)
    }

    /// The agent must apply a marker's spec again, as after its group was
    /// spawned again: one more spec revision, journalled. Returns the new
    /// revision, or `None` for a marker that is gone.
    pub fn bump_marker(&mut self, id: EntityId) -> Option<u64> {
        let m = self.markers.items.get_mut(&id).filter(|m| !m.removed)?;
        m.spec_rev += 1;
        let rev = m.spec_rev;
        self.journal(Entry::Spec { entity: id, rev });
        self.touch(id);
        Some(rev)
    }

    /// The controller removes an extension. The agent is told to drop it
    /// until it confirms ([`Store::extension_gone`]). Returns whether the
    /// store held it.
    pub fn remove_extension(&mut self, name: &str) -> bool {
        let Some(e) = self.extensions.remove(name) else {
            return false;
        };
        let floor = self.markers.ext_floor.entry(name.to_owned()).or_default();
        *floor = (*floor).max(e.spec_rev);
        self.markers.ext_removed.insert(name.to_owned());
        self.journal(Entry::ExtensionRemoved {
            name: name.to_owned(),
        });
        self.touch(EntityId(0));
        true
    }

    /// The highest spec revision an extension of the name had before it was
    /// removed, 0 for none.
    pub(super) fn extension_floor(&self, name: &str) -> u64 {
        self.markers.ext_floor.get(name).copied().unwrap_or(0)
    }

    /// The extensions removed that the agent may still hold.
    pub fn extension_removals(&self) -> impl Iterator<Item = &str> {
        self.markers
            .ext_removed
            .iter()
            .filter(|n| !self.extensions.contains_key(*n))
            .map(String::as_str)
    }

    /// The agent confirmed an extension gone.
    pub fn extension_gone(&mut self, name: &str) {
        self.markers.ext_removed.remove(name);
    }
}
