//! Marks, drawings and menu items, and the Lua extensions the controller
//! removed (spec/world.md, "Entities"; spec/commands.md, "Marks, drawings and
//! menu items in DCS").
//!
//! A marker is a set-state entity with an `EntityId` and a spec revision,
//! journalled as `Entry::Spec` and `Entry::Removed`. The store keeps it
//! across mission loads; the reconciler sets it again after each one. A
//! removed marker stays in the table, so its ID is never reused and DCS is
//! still told to drop it.
//!
//! A mark or a drawing may carry a label, unique among the campaign's live
//! marks and drawings, and a layer. The store chooses each one's DCS mark ID,
//! so `world.getMarkPanels` lists them in that order; whether a client draws
//! in it is UNVERIFIED (spec/world.md, "Labels and markup groups").

use std::collections::{BTreeMap, BTreeSet};

use nanoprobe_transport::codec::Lv;

use super::drawing::{self, DrawingShape};
use super::{Entry, Point, RemoveCause, Store};
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

/// The DCS mark IDs the sidecar gives (spec/world.md, "DCS IDs"). Marks and
/// drawings share one pool: every ID from 2^30 to 2^31 - 1, the highest ID
/// DCS draws. Every add takes a never-used slot of it, so the pool only
/// fills, until a new DCS process starts.
pub mod dcs_ids {
    /// The first ID of the pool: player marks and mission scripts are below it.
    pub const BASE: u64 = 1 << 30;
    /// The IDs in the pool, up to 2^31 - 1.
    pub const SLOTS: u64 = 1 << 30;

    /// Whether `id` is in the pool.
    pub fn in_pool(id: u64) -> bool {
        (BASE..BASE + SLOTS).contains(&id)
    }
}

/// The most bytes in a label.
pub const LABEL_MAX: usize = 128;
/// The most bytes in a layer's name.
pub const LAYER_NAME_MAX: usize = 64;

/// Whether a label or a layer name is well formed: 1 to `max` bytes of UTF-8
/// with no control character.
fn name_ok(name: &str, max: usize) -> bool {
    !name.is_empty() && name.len() <= max && !name.chars().any(char::is_control)
}

/// A layer: a named set of marks and drawings, shown or hidden together.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// Its name, unique in the campaign.
    pub name: String,
    /// Whether DCS shows its items.
    pub visible: bool,
    /// The model seconds an item put in it lives when its put gives none.
    pub expire_after_s: Option<f64>,
    /// The most live items it holds; a put over it removes the oldest.
    pub cap: Option<u32>,
}

impl Layer {
    fn new(name: &str) -> Self {
        Layer {
            name: name.to_owned(),
            visible: true,
            expire_after_s: None,
            cap: None,
        }
    }
}

/// A mark or drawing put with its label, layer and expiry.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerPut {
    /// The entity, 0 to create one or to find it by label.
    pub id: EntityId,
    /// A `scope` code.
    pub scope: u32,
    /// By scope.
    pub target: u64,
    /// The fields.
    pub spec: MarkerSpec,
    /// A drawing's typed form. When set, it replaces `spec` with its
    /// lowering.
    pub drawing: Option<DrawingShape>,
    /// The label; `None` keeps the item's own, empty clears it.
    pub label: Option<String>,
    /// The layer's name; `None` keeps the item's own, empty takes it out of
    /// any layer.
    pub layer: Option<String>,
    /// Model seconds from now until the item expires; `None` keeps its
    /// expiry, or for a new item takes its layer's.
    pub expire_after_s: Option<f64>,
}

impl MarkerPut {
    /// The put with `spec` set to its drawing's lowering, when it has one.
    fn lowered(mut self) -> MarkerPut {
        if let Some(d) = &self.drawing {
            let (shape, args) = d.lower();
            self.spec = MarkerSpec::Drawing { shape, args };
        }
        self
    }
}

/// What a put did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Put {
    /// The entity.
    pub id: EntityId,
    /// Its spec revision.
    pub rev: u64,
    /// The items a layer's cap removed.
    pub evicted: Vec<EntityId>,
}

/// What a layer replacement did: each label's entity, in the order given,
/// and the items removed.
pub type Replaced = (Vec<(String, EntityId)>, Vec<EntityId>);

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
    /// A map drawing, as the reconciler sends it: the lowering of the
    /// marker's [`DrawingShape`] when it has one.
    Drawing {
        /// The shape code: a `markupToAll` shape, or
        /// [`drawing::shape::CIRCLE`].
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
    /// When it was removed, wall-clock milliseconds since the Unix epoch.
    /// The reconciler purges it `removed_keep_h` later, once DCS holds it
    /// no more (spec/world.md, "Removal").
    pub removed_at_ms: Option<u64>,
    /// A `scope` code.
    pub scope: u32,
    /// By scope: 0, a coalition number or a group `EntityId`.
    pub target: u64,
    /// The fields.
    pub spec: MarkerSpec,
    /// A drawing's typed form; `spec` is its lowering. `None` for a mark, a
    /// menu item, and a drawing stored without one (store tests).
    pub drawing: Option<DrawingShape>,
    /// The DCS mark ID the reconciler last took for it, never one used before in
    /// the agent boot. 0 for a menu item, and for a mark or drawing DCS does
    /// not hold: new, hidden, or not yet added since the boot or mission load.
    pub dcs_id: u64,
    /// The draw rank: the value of the store's draw counter when an add of this
    /// item was last sent (a new item, a full set, a show, a re-add). A full
    /// redraw adds items ascending by it, so DCS IDs, which clients draw by,
    /// keep the order. 0 for an item never sent; those go last, by creation.
    pub rank: u64,
    /// The label, empty for none.
    pub label: String,
    /// The layer's name, empty for none.
    pub layer: String,
    /// The campaign model time it expires at, seconds.
    pub expires_at: Option<f64>,
    /// Why it was removed.
    pub removed_cause: Option<RemoveCause>,
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
    /// The pool has too few slots above its high-water mark for the adds the
    /// change needs, in this agent boot. Carries 0, the one pool's number.
    DcsIdsFull(u64),
    /// The label is held by another item, or by an item in another layer.
    LabelTaken(String),
    /// The label is not 1 to `LABEL_MAX` bytes without a control character,
    /// or is on a menu item.
    BadLabel,
    /// The layer's name is not 1 to `LAYER_NAME_MAX` bytes without a control
    /// character, or is on a menu item.
    BadLayer,
    /// No layer of the name.
    NoSuchLayer(String),
    /// An expiry that is not a finite number above 0.
    BadExpiry,
    /// A cap of 0, or a replacement with more items than the layer's cap.
    BadCap,
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
    /// The live mark or drawing of each label.
    labels: BTreeMap<String, EntityId>,
    /// The layers by name.
    layers: BTreeMap<String, Layer>,
    /// The slots issued per agent boot, the newest `ISSUE_BOOTS_KEEP` boots.
    issue: BTreeMap<u64, BootIssue>,
    /// The mark the server store holds for an agent boot, raised into that
    /// boot's mark when it begins or is current (spec/persistence.md, "The
    /// server store").
    server_mark: Option<(u64, u64)>,
    /// The agent boot the store issues slots in.
    boot: Option<u64>,
    /// Campaign model time: the model seconds the campaign's missions ran.
    played_s: f64,
    /// The agent boot, generation and model time of the last sample counted.
    played_at: Option<(u64, u32, f64)>,
    /// Slots a pool holds, below the full count only in tests.
    slot_limit: Option<u64>,
    /// The highest draw rank given. Rebuilt at open as the highest `rank` of
    /// any row, so a rank never repeats among live items.
    draw_rank: u64,
}

/// The agent boots whose issued slots the store keeps, for events a log still
/// holds.
pub const ISSUE_BOOTS_KEEP: usize = 16;

/// The slots the store gave in one agent boot: the high-water mark of the
/// shared pool, as the next slot to give (spec/persistence.md, "The store
/// file").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootIssue {
    /// The order boots began in, rising.
    pub ordinal: u64,
    /// The next slot of the pool; 0 when none was given.
    pub next: u64,
}

/// Whether a drawing's shape code and arguments are a layout the platform
/// sends (spec/commands.md, "Layout: DRAWING_SET"); see
/// [`drawing::layout_ok`].
pub fn drawing_layout_ok(shape: u32, args: &Lv) -> bool {
    drawing::layout_ok(shape, args)
}

/// Whether `args` hold the shape's lead (its points; a circle's center and
/// radius) and then the full style: color and fill (four numbers each), the
/// line type or font size, read only and the message.
fn has_style_tail(shape: u32, args: &[&Lv]) -> bool {
    let n = args.len();
    let lead = n.checked_sub(5);
    let lead_ok = match (shape, lead) {
        (drawing::shape::LINE | drawing::shape::ARROW, Some(2))
        | (drawing::shape::TEXT_BOX, Some(1))
        | (drawing::shape::QUAD, Some(4)) => true,
        (drawing::shape::POLYGON, Some(p)) => p >= 2,
        (drawing::shape::FREEFORM, Some(p)) => p >= 3,
        (drawing::shape::CIRCLE, Some(2)) => matches!(args[1], Lv::Number(_)),
        _ => false,
    };
    let color = |v: &Lv| {
        let items = crate::sim::wire::lv::items(v);
        items.len() == 4 && items.iter().all(|c| matches!(c, Lv::Number(_)))
    };
    lead_ok
        && color(args[n - 5])
        && color(args[n - 4])
        && matches!(args[n - 3], Lv::Number(_))
        && matches!(args[n - 2], Lv::Bool(_))
        && matches!(args[n - 1], Lv::String(_))
}

/// The `DRAWING_UPDATE` changes that take a drawing from `old` to `new`, if
/// only its colors, line type, a text box's font size and text, or a
/// circle's radius differ (spec/commands.md, "Marks, drawings and menu items
/// in DCS"). The arguments end with the style: color, fill, then the line
/// type (the font size for a text box), read only and the message (the
/// text). A circle's radius follows its center. Any other change, or
/// arguments that are not the shape's lead and full style tail, is a full
/// set. The values are checked at put time ([`DrawingShape::check`]) and by
/// the shim (spec/commands.md, "Layout: DRAWING_SET").
pub fn drawing_style_change(shape: u32, old: &Lv, new: &Lv) -> Option<Lv> {
    let (a, b) = (
        crate::sim::wire::lv::items(old),
        crate::sim::wire::lv::items(new),
    );
    let n = a.len();
    if b.len() != n || !has_style_tail(shape, &a) || !has_style_tail(shape, &b) {
        return None;
    }
    let text = shape == drawing::shape::TEXT_BOX;
    let circle = shape == drawing::shape::CIRCLE;
    // The points stay (a circle's center), read only, and a message that is
    // not a text box's.
    let lead = if circle { 1 } else { n - 5 };
    if a[..lead] != b[..lead] || a[n - 2] != b[n - 2] || (!text && a[n - 1] != b[n - 1]) {
        return None;
    }
    let mut changes = Vec::new();
    let mut put = |key: &str, i: usize| {
        if a[i] != b[i] {
            changes.push((Lv::String(key.as_bytes().to_vec()), b[i].clone()));
        }
    };
    if circle {
        put("radius", 1);
    }
    put("color", n - 5);
    put("fill", n - 4);
    put(if text { "font_size" } else { "line_type" }, n - 3);
    if text {
        put("text", n - 1);
    }
    (!changes.is_empty()).then_some(Lv::Table(changes))
}

/// Whether the marker `old`, with a change to `scope`, `target` and `spec`,
/// needs an add to DCS: it holds no ID, a mark changed, or a drawing changed
/// by more than the style [`drawing_style_change`] sends in place.
fn add_needed(
    old: &Marker,
    scope: u32,
    target: u64,
    spec: &MarkerSpec,
    drawing: &Option<DrawingShape>,
) -> bool {
    if old.dcs_id == 0 {
        return true;
    }
    if old.scope == scope && old.target == target && old.spec == *spec {
        // The same lowering under a new typed form (a theater, or a quad
        // given as a rectangle) is a new revision with no style change: the
        // reconciler adds it again.
        return old.drawing != *drawing;
    }
    match (&old.spec, spec) {
        (
            MarkerSpec::Drawing {
                shape: s0,
                args: a0,
            },
            MarkerSpec::Drawing { shape, args },
        ) => {
            !(old.scope == scope
                && old.target == target
                && s0 == shape
                && drawing_style_change(*shape, a0, args).is_some())
        }
        _ => true,
    }
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

    /// Take a removed marker out of the store (spec/world.md, "Removal").
    /// The caller has checked that DCS holds it no more. Returns whether it
    /// was a removed marker.
    pub fn purge_marker(&mut self, id: EntityId) -> bool {
        if !self.markers.items.get(&id).is_some_and(|m| m.removed) {
            return false;
        }
        self.journal(Entry::Purged { entity: id });
        self.touch(id);
        // A watch that held it is told it left the store. Menu items are
        // never served, so no watch holds one.
        if self.markers.items.get(&id).map(Marker::kind) != Some(MarkerKind::MenuItem) {
            self.purged_recent.insert((self.rev, id));
        }
        self.markers.items.remove(&id);
        true
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
    /// already has changes nothing. Keeps a mark's or drawing's label, layer
    /// and expiry. Returns the ID and the spec revision.
    pub fn put_marker(
        &mut self,
        id: EntityId,
        scope: u32,
        target: u64,
        spec: MarkerSpec,
    ) -> Result<(EntityId, u64), MarkerError> {
        let p = self.put_marker_with(MarkerPut {
            id,
            scope,
            target,
            spec,
            drawing: None,
            label: None,
            layer: None,
            expire_after_s: None,
        })?;
        Ok((p.id, p.rev))
    }

    /// Add a marker, or change one: by `id`, or for ID 0 by a label a live
    /// mark or drawing holds (an upsert). A put that changes nothing answers
    /// the current revision. A put that adds an item to a layer over its cap
    /// removes the layer's oldest items in the same write.
    pub fn put_marker_with(&mut self, p: MarkerPut) -> Result<Put, MarkerError> {
        let p = p.lowered();
        let kind = match p.spec {
            MarkerSpec::Mark { .. } => MarkerKind::Mark,
            MarkerSpec::Drawing { .. } => MarkerKind::Drawing,
            MarkerSpec::MenuItem { .. } => MarkerKind::MenuItem,
        };
        if !scope_ok(kind, p.scope) {
            return Err(MarkerError::BadScope(p.scope));
        }
        let pinned = kind != MarkerKind::MenuItem;
        if let Some(l) = p.label.as_deref().filter(|l| !l.is_empty())
            && (!pinned || !name_ok(l, LABEL_MAX))
        {
            return Err(MarkerError::BadLabel);
        }
        if let Some(l) = p.layer.as_deref().filter(|l| !l.is_empty())
            && (!pinned || !name_ok(l, LAYER_NAME_MAX))
        {
            return Err(MarkerError::BadLayer);
        }
        if p.expire_after_s.is_some_and(|s| !s.is_finite() || s <= 0.0) {
            return Err(MarkerError::BadExpiry);
        }
        // The item a label names, for an upsert or a clash.
        let mut id = p.id;
        if let Some(l) = p.label.as_deref().filter(|l| !l.is_empty())
            && let Some(&holder) = self.markers.labels.get(l)
        {
            if id.0 == 0 {
                id = holder;
            } else if id != holder {
                return Err(MarkerError::LabelTaken(l.to_owned()));
            }
            if self.markers.items[&holder].kind() != kind {
                return Err(MarkerError::LabelTaken(l.to_owned()));
            }
        }
        let (spec, scope, target, typed) = (p.spec, p.scope, p.target, p.drawing);
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
            Some(m.clone())
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
            if let Some(m) = &old
                && (m.scope != scope || m.target != target || !*submenu)
                && !self.menu_children(id).is_empty()
            {
                return Err(MarkerError::HasChildren(id));
            }
        }
        let label = p
            .label
            .unwrap_or_else(|| old.as_ref().map(|m| m.label.clone()).unwrap_or_default());
        let layer = p
            .layer
            .unwrap_or_else(|| old.as_ref().map(|m| m.layer.clone()).unwrap_or_default());
        let expires_at = match (p.expire_after_s, &old) {
            (Some(s), _) => Some(self.markers.played_s + s),
            (None, Some(m)) => m.expires_at,
            (None, None) => self
                .markers
                .layers
                .get(&layer)
                .and_then(|l| l.expire_after_s)
                .map(|s| self.markers.played_s + s),
        };
        let same_spec = old.as_ref().is_some_and(|m| {
            m.scope == scope && m.target == target && m.spec == spec && m.drawing == typed
        });
        let hidden = !self.markers.layers.get(&layer).is_none_or(|l| l.visible);
        if let Some(m) = &old
            && same_spec
            && m.label == label
            && m.layer == layer
            && m.expires_at == expires_at
        {
            return Ok(Put {
                id,
                rev: m.spec_rev,
                evicted: Vec::new(),
            });
        }
        // The reconciler takes the ID when it adds the item to DCS; a hidden
        // item holds none.
        let dcs_id = match kind {
            MarkerKind::MenuItem => 0,
            _ if hidden => 0,
            _ => old.as_ref().map_or(0, |m| m.dcs_id),
        };
        // Refuse before any write when no slot is left above the pool's
        // high-water mark for an add this put makes: a new item, a mark
        // changed, a drawing changed by more than its style, and an item DCS
        // holds no ID for.
        if kind != MarkerKind::MenuItem
            && !hidden
            && old
                .as_ref()
                .is_none_or(|m| add_needed(m, scope, target, &spec, &typed))
            && self.pool_free(|m| m.id == id) == 0
        {
            return Err(MarkerError::DcsIdsFull(0));
        }
        let (id, rev) = match &old {
            Some(m) if same_spec => (id, m.spec_rev),
            Some(m) => (id, m.spec_rev + 1),
            None => (self.allocate().map_err(|_| MarkerError::Exhausted)?, 1),
        };
        if !layer.is_empty() && !self.markers.layers.contains_key(&layer) {
            self.markers
                .layers
                .insert(layer.clone(), Layer::new(&layer));
            self.journal(Entry::Layer {
                name: layer.clone(),
                removed: false,
            });
        }
        if let Some(m) = &old
            && !m.label.is_empty()
        {
            self.markers.labels.remove(&m.label);
        }
        if !label.is_empty() {
            self.markers.labels.insert(label.clone(), id);
        }
        let joined = old.as_ref().is_none_or(|m| m.layer != layer);
        self.markers.items.insert(
            id,
            Marker {
                id,
                spec_rev: rev,
                removed: false,
                removed_at_ms: None,
                scope,
                target,
                spec,
                drawing: typed,
                dcs_id,
                rank: old.as_ref().map_or(0, |m| m.rank),
                label,
                layer: layer.clone(),
                expires_at,
                removed_cause: None,
            },
        );
        if old.as_ref().is_none_or(|m| m.spec_rev != rev) {
            self.journal(Entry::Spec { entity: id, rev });
        }
        self.touch(id);
        let evicted = if joined && !layer.is_empty() {
            self.evict_over_cap(&layer, Some(id))
        } else {
            Vec::new()
        };
        Ok(Put { id, rev, evicted })
    }

    /// Remove a marker. A submenu takes the items under it, at every depth.
    /// A marker already removed is left as it is. Returns the IDs removed
    /// now, the items under a submenu first.
    pub fn remove_marker(&mut self, id: EntityId) -> Result<Vec<EntityId>, MarkerError> {
        self.remove_marker_as(id, RemoveCause::Controller)
    }

    fn remove_marker_as(
        &mut self,
        id: EntityId,
        cause: RemoveCause,
    ) -> Result<Vec<EntityId>, MarkerError> {
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
            let now = self.clock_ms();
            let m = self.markers.items.get_mut(e).expect("marker");
            m.removed = true;
            m.removed_at_ms = Some(now);
            m.removed_cause = Some(cause);
            // The label is free again.
            if !m.label.is_empty() && self.markers.labels.get(&m.label) == Some(e) {
                let l = m.label.clone();
                self.markers.labels.remove(&l);
            }
            self.journal(Entry::Removed { entity: *e, cause });
            self.touch(*e);
        }
        Ok(order)
    }

    /// Remove every marker whose `GROUP` target is one of `groups`, as the
    /// end of an adoption does: the group has no binding to place them on.
    /// Each removal is journalled. Returns the IDs removed now.
    pub fn remove_markers_of_groups(&mut self, groups: &[EntityId]) -> Vec<EntityId> {
        let hit: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| {
                !m.removed && m.scope == scope::GROUP && groups.contains(&EntityId(m.target))
            })
            .map(|m| m.id)
            .collect();
        let mut out = Vec::new();
        for id in hit {
            if let Ok(v) = self.remove_marker(id) {
                out.extend(v);
            }
        }
        out
    }

    /// Remove every marker of `GROUP` scope whose target is neither a group
    /// of the store nor named by `adopted`: the group is not bound in this
    /// scope. Each removal is journalled. Returns the IDs removed now.
    pub fn remove_unbound_group_markers(&mut self, adopted: &[EntityId]) -> Vec<EntityId> {
        let hit: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| {
                let g = EntityId(m.target);
                !m.removed
                    && m.scope == scope::GROUP
                    && !adopted.contains(&g)
                    && !self.groups.contains_key(&g)
            })
            .map(|m| m.id)
            .collect();
        let mut out = Vec::new();
        for id in hit {
            if let Ok(v) = self.remove_marker(id) {
                out.extend(v);
            }
        }
        out
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

    /// The slots the pool holds, in full unless a test shrank it.
    fn slot_limit(&self) -> u64 {
        self.markers
            .slot_limit
            .map_or(dcs_ids::SLOTS, |l| l.min(dcs_ids::SLOTS))
    }

    /// The high-water mark in the current agent boot: the next slot.
    fn issued_next(&self) -> u64 {
        self.markers
            .boot
            .and_then(|b| self.markers.issue.get(&b))
            .map_or(0, |i| i.next)
    }

    /// The slots a new add can take: those above the high-water mark, less
    /// one for each live visible item DCS holds no ID for, which will take
    /// one. Items `skip` takes are not counted.
    fn pool_free(&self, skip: impl Fn(&Marker) -> bool) -> u64 {
        let waiting = self
            .markers
            .items
            .values()
            .filter(|m| {
                !m.removed
                    && m.kind() != MarkerKind::MenuItem
                    && m.dcs_id == 0
                    && self.marker_visible(m)
                    && !skip(m)
            })
            .count() as u64;
        self.dcs_slots_left().saturating_sub(waiting)
    }

    /// Give a live, visible mark or drawing a never-used DCS ID, for an add
    /// DCS must make: a new item, a full set of one already drawn, a show,
    /// and a re-add after a mission load or a restart. Moves the high-water
    /// mark and the item's `dcs_id`, both written with the commit that
    /// carries the command (spec/persistence.md, "Writes"). With `new_rev`
    /// the item is also at a new spec revision, journalled: the agent answers
    /// `AT_REVISION` to a set at a revision it holds, so an item it may hold
    /// needs one to be added under the new ID. Returns the ID and the
    /// revision. Fails with `DcsIdsFull` when the pool has no slot left in
    /// this agent boot, changing nothing.
    pub fn issue_dcs_id(&mut self, id: EntityId, new_rev: bool) -> Result<(u64, u64), MarkerError> {
        self.markers
            .items
            .get(&id)
            .filter(|m| !m.removed && m.kind() != MarkerKind::MenuItem)
            .ok_or(MarkerError::NoSuchMarker(id))?;
        if self.markers.boot.is_none() {
            self.begin_dcs_boot(0);
        }
        let boot = self.markers.boot.expect("boot");
        let next = self.issued_next();
        if next >= self.slot_limit() {
            return Err(MarkerError::DcsIdsFull(0));
        }
        self.markers.issue.get_mut(&boot).expect("issue").next = next + 1;
        let dcs_id = dcs_ids::BASE + next;
        let m = self.markers.items.get_mut(&id).expect("marker");
        m.dcs_id = dcs_id;
        self.markers.draw_rank += 1;
        m.rank = self.markers.draw_rank;
        if new_rev {
            m.spec_rev += 1;
        }
        let rev = m.spec_rev;
        if new_rev {
            self.journal(Entry::Spec { entity: id, rev });
        }
        self.touch(id);
        Ok((dcs_id, rev))
    }

    /// The agent boot the store issues DCS IDs in.
    pub fn dcs_boot(&self) -> Option<u64> {
        self.markers.boot
    }

    /// The current agent boot and its mark, the next slot: what a campaign's
    /// closing transaction writes to the server store's `dcs_issue` row
    /// (spec/persistence.md, "The server store"). None before any boot.
    pub fn dcs_mark(&self) -> Option<(u64, u64)> {
        self.markers.boot.map(|b| (b, self.issued_next()))
    }

    /// Raise the mark of agent boot `boot` to `next` at least: the server
    /// store's row, read when a campaign activates. It applies at once when
    /// `boot` is the current one, and when the store begins `boot` later.
    /// A mark never goes down.
    pub fn raise_dcs_mark(&mut self, boot: u64, next: u64) {
        let next = next.min(dcs_ids::SLOTS);
        let kept = match self.markers.server_mark {
            Some((b, n)) if b == boot => n.max(next),
            _ => next,
        };
        self.markers.server_mark = Some((boot, kept));
        if self.markers.boot == Some(boot) {
            self.raise_current(kept);
        }
    }

    /// The shim refused an ID at or below its process mark `mark`: raise the
    /// mark past it, so the next add takes a higher ID. Returns whether the
    /// mark moved.
    pub fn raise_dcs_mark_past(&mut self, mark: u64) -> bool {
        if !dcs_ids::in_pool(mark) {
            return false;
        }
        if self.markers.boot.is_none() {
            self.begin_dcs_boot(0);
        }
        let want = (mark - dcs_ids::BASE + 1).min(dcs_ids::SLOTS);
        self.raise_current(want)
    }

    fn raise_current(&mut self, next: u64) -> bool {
        let Some(i) = self
            .markers
            .boot
            .and_then(|b| self.markers.issue.get_mut(&b))
        else {
            return false;
        };
        if i.next >= next {
            return false;
        }
        i.next = next;
        true
    }

    /// Start issuing in agent boot `boot` when it is not the current one: its
    /// high-water mark begins at 0, or at the server store's for that boot,
    /// and DCS holds no item, so every `dcs_id` reads 0 until the item is
    /// added again. Marks of the boots before are kept for `ISSUE_BOOTS_KEEP`
    /// boots.
    pub(super) fn begin_dcs_boot(&mut self, boot: u64) {
        if self.markers.boot == Some(boot) {
            return;
        }
        let ordinal = self
            .markers
            .issue
            .values()
            .map(|i| i.ordinal)
            .max()
            .map_or(1, |o| o + 1);
        self.markers.issue.entry(boot).or_default().ordinal = ordinal;
        self.markers.boot = Some(boot);
        if let Some((b, n)) = self.markers.server_mark
            && b == boot
        {
            self.raise_current(n);
        }
        while self.markers.issue.len() > ISSUE_BOOTS_KEEP {
            let oldest = self
                .markers
                .issue
                .iter()
                .min_by_key(|(_, i)| i.ordinal)
                .map(|(b, _)| *b)
                .expect("boots");
            self.markers.issue.remove(&oldest);
        }
        self.forget_dcs_ids();
    }

    /// DCS holds no mark or drawing: a new agent boot or mission generation.
    /// Every live item reads `dcs_id` 0 until it is added again.
    pub(super) fn forget_dcs_ids(&mut self) {
        let held: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| m.dcs_id != 0 && !m.removed)
            .map(|m| m.id)
            .collect();
        for id in held {
            self.markers.items.get_mut(&id).expect("marker").dcs_id = 0;
            self.touch(id);
        }
    }

    /// Whether the platform issued `dcs_id` in agent boot `boot`: it is in
    /// the pool below that boot's high-water mark. False for a boot whose
    /// marks are no longer kept.
    pub fn dcs_issued_in_boot(&self, boot: u64, dcs_id: u64) -> bool {
        dcs_ids::in_pool(dcs_id)
            && self
                .markers
                .issue
                .get(&boot)
                .is_some_and(|i| dcs_id - dcs_ids::BASE < i.next)
    }

    /// Whether the platform issued `dcs_id` in the current agent boot.
    pub fn dcs_issued_now(&self, dcs_id: u64) -> bool {
        self.markers
            .boot
            .is_some_and(|b| self.dcs_issued_in_boot(b, dcs_id))
    }

    /// The slots left in the pool in the current agent boot: marks and
    /// drawings share it. Constant time.
    pub fn dcs_slots_left(&self) -> u64 {
        self.slot_limit().saturating_sub(self.issued_next())
    }

    /// The slots issued per agent boot, for the store file.
    #[cfg(feature = "disk")]
    pub(super) fn issue_rows(&self) -> &BTreeMap<u64, BootIssue> {
        &self.markers.issue
    }

    /// Remove the oldest live items of a layer over its cap, `keep` last of
    /// all. Returns those removed.
    fn evict_over_cap(&mut self, layer: &str, keep: Option<EntityId>) -> Vec<EntityId> {
        let Some(cap) = self.markers.layers.get(layer).and_then(|l| l.cap) else {
            return Vec::new();
        };
        let mut live: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| !m.removed && m.layer == layer)
            .map(|m| m.id)
            .collect();
        // Oldest first: IDs grow with creation.
        live.sort_by_key(|id| (Some(*id) == keep, *id));
        let over = live.len().saturating_sub(cap as usize);
        let mut out = Vec::new();
        for id in live.into_iter().take(over) {
            out.extend(
                self.remove_marker_as(id, RemoveCause::Evicted)
                    .unwrap_or_default(),
            );
        }
        out
    }

    /// A marker's state in DCS changed: what the API shows of it moves to a
    /// new store revision. Nothing is journalled.
    pub fn touch_marker(&mut self, id: EntityId) {
        if self.markers.items.contains_key(&id) {
            self.touch(id);
        }
    }

    /// Hold `slots` IDs in the pool, for a test of a full pool.
    #[doc(hidden)]
    pub fn set_slot_limit_for_test(&mut self, slots: u64) {
        self.markers.slot_limit = Some(slots);
    }

    /// A label's live mark or drawing.
    pub fn marker_by_label(&self, label: &str) -> Option<EntityId> {
        self.markers.labels.get(label).copied()
    }

    /// Every layer, by name.
    pub fn layers(&self) -> impl Iterator<Item = &Layer> {
        self.markers.layers.values()
    }

    /// A layer.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.markers.layers.get(name)
    }

    /// Whether DCS should show a live marker: it is in no layer or in a
    /// visible one.
    pub fn marker_visible(&self, m: &Marker) -> bool {
        m.layer.is_empty() || self.markers.layers.get(&m.layer).is_none_or(|l| l.visible)
    }

    /// Create a layer or change one. A lower cap removes its oldest items. Returns the items the cap removed.
    pub fn put_layer(&mut self, layer: Layer) -> Result<Vec<EntityId>, MarkerError> {
        if !name_ok(&layer.name, LAYER_NAME_MAX) {
            return Err(MarkerError::BadLayer);
        }
        if layer
            .expire_after_s
            .is_some_and(|s| !s.is_finite() || s <= 0.0)
        {
            return Err(MarkerError::BadExpiry);
        }
        if layer.cap == Some(0) {
            return Err(MarkerError::BadCap);
        }
        let old = self.markers.layers.get(&layer.name).cloned();
        if old.as_ref() == Some(&layer) {
            return Ok(Vec::new());
        }
        let name = layer.name.clone();
        let was_visible = old.as_ref().is_none_or(|l| l.visible);
        // Refuse before any write when the layer's items cannot all take
        // slots above the high-water mark: a shown layer adds them again.
        if layer.visible && !was_visible {
            let adds = self
                .markers
                .items
                .values()
                .filter(|m| !m.removed && m.layer == name && m.kind() != MarkerKind::MenuItem)
                .count() as u64;
            if adds > self.pool_free(|m| m.layer == name) {
                return Err(MarkerError::DcsIdsFull(0));
            }
        }
        self.markers.layers.insert(name.clone(), layer.clone());
        self.journal(Entry::Layer {
            name: name.clone(),
            removed: false,
        });
        self.touch(EntityId(0));
        // Hidden: each drawn item reads 0 and is added again, under a new ID,
        // when shown.
        if was_visible && !layer.visible {
            let drawn: Vec<EntityId> = self
                .markers
                .items
                .values()
                .filter(|m| !m.removed && m.layer == name && m.dcs_id != 0)
                .map(|m| m.id)
                .collect();
            for id in drawn {
                self.markers.items.get_mut(&id).expect("marker").dcs_id = 0;
                self.touch(id);
            }
        }
        Ok(self.evict_over_cap(&name, None))
    }

    /// Remove a layer and every item in it. Returns the items removed.
    pub fn remove_layer(&mut self, name: &str) -> Result<Vec<EntityId>, MarkerError> {
        if !self.markers.layers.contains_key(name) {
            return Err(MarkerError::NoSuchLayer(name.to_owned()));
        }
        let items: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| !m.removed && m.layer == name)
            .map(|m| m.id)
            .collect();
        let mut out = Vec::new();
        for id in items {
            out.extend(self.remove_marker(id)?);
        }
        self.markers.layers.remove(name);
        self.journal(Entry::Layer {
            name: name.to_owned(),
            removed: true,
        });
        self.touch(EntityId(0));
        Ok(out)
    }

    /// Make a layer hold exactly `items`, by label: change the items whose
    /// label it holds, add the others, and remove its items whose label is
    /// not among them, those with no label included. Every item needs a
    /// label, unique in the call and held by no item of another layer.
    /// Checked in full before anything is written. Returns each label's
    /// entity, in the order given, and the items removed.
    pub fn replace_layer(
        &mut self,
        name: &str,
        items: Vec<MarkerPut>,
    ) -> Result<Replaced, MarkerError> {
        if !name_ok(name, LAYER_NAME_MAX) {
            return Err(MarkerError::BadLayer);
        }
        let items: Vec<MarkerPut> = items.into_iter().map(MarkerPut::lowered).collect();
        if let Some(cap) = self.markers.layers.get(name).and_then(|l| l.cap)
            && items.len() > cap as usize
        {
            return Err(MarkerError::BadCap);
        }
        let mut seen = BTreeSet::new();
        for p in &items {
            let label = p.label.as_deref().unwrap_or("");
            let kind = match p.spec {
                MarkerSpec::Mark { .. } => MarkerKind::Mark,
                MarkerSpec::Drawing { .. } => MarkerKind::Drawing,
                MarkerSpec::MenuItem { .. } => return Err(MarkerError::BadScope(p.scope)),
            };
            if !name_ok(label, LABEL_MAX) || !seen.insert(label.to_owned()) {
                return Err(MarkerError::BadLabel);
            }
            if !scope_ok(kind, p.scope) {
                return Err(MarkerError::BadScope(p.scope));
            }
            if p.expire_after_s.is_some_and(|s| !s.is_finite() || s <= 0.0) {
                return Err(MarkerError::BadExpiry);
            }
            if let Some(holder) = self.markers.labels.get(label) {
                let m = &self.markers.items[holder];
                if m.layer != name || m.kind() != kind {
                    return Err(MarkerError::LabelTaken(label.to_owned()));
                }
            }
        }
        let keep: BTreeSet<String> = seen;
        let stale: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| !m.removed && m.layer == name && !keep.contains(&m.label))
            .map(|m| m.id)
            .collect();
        // Refuse before any write when a pool has too few slots above its
        // high-water mark for the adds: new items, changed marks, and items
        // DCS holds no ID for.
        let visible = self.markers.layers.get(name).is_none_or(|l| l.visible);
        // Each new item also takes an entity ID.
        let fresh = items
            .iter()
            .filter(|p| {
                !self
                    .markers
                    .labels
                    .contains_key(p.label.as_deref().unwrap_or(""))
            })
            .count() as u64;
        if fresh > self.ids_left() {
            return Err(MarkerError::Exhausted);
        }
        if visible {
            let mut adds = 0u64;
            for p in &items {
                let held = self
                    .markers
                    .labels
                    .get(p.label.as_deref().unwrap_or(""))
                    .map(|h| &self.markers.items[h]);
                if held.is_none_or(|m| add_needed(m, p.scope, p.target, &p.spec, &p.drawing)) {
                    adds += 1;
                }
            }
            if adds > self.pool_free(|m| m.layer == name) {
                return Err(MarkerError::DcsIdsFull(0));
            }
        }
        let mut removed = Vec::new();
        // Removals first, so the cap counts what stays.
        for id in stale {
            removed.extend(self.remove_marker(id)?);
        }
        let mut ids = Vec::new();
        for mut p in items {
            let label = p.label.clone().unwrap_or_default();
            p.id = EntityId(0);
            p.layer = Some(name.to_owned());
            let put = self.put_marker_with(p)?;
            removed.extend(put.evicted);
            ids.push((label, put.id));
        }
        Ok((ids, removed))
    }

    /// Every marker by ID, removed ones included, for the store file.
    #[cfg(feature = "disk")]
    pub(super) fn marker_rows(&self) -> &BTreeMap<EntityId, Marker> {
        &self.markers.items
    }

    /// Every layer by name, for the store file.
    #[cfg(feature = "disk")]
    pub(super) fn layer_rows(&self) -> &BTreeMap<String, Layer> {
        &self.markers.layers
    }

    /// Campaign model time and the sample it was last counted at, for the
    /// store file.
    #[cfg(feature = "disk")]
    pub(super) fn played(&self) -> (f64, Option<(u64, u32, f64)>) {
        (self.markers.played_s, self.markers.played_at)
    }

    /// Take the markers, layers and campaign model time a store file holds,
    /// after checking them (spec/persistence.md, "Opening a store"). The
    /// label index is derived. Each pool's next slot in the current boot (the
    /// newest in `issue`) is at least the stored mark and one past the
    /// highest DCS ID any row holds in it, removed rows included. Returns
    /// what is wrong, for a store the open refuses.
    #[cfg(feature = "disk")]
    pub(super) fn restore_markers(
        &mut self,
        items: BTreeMap<EntityId, Marker>,
        layers: BTreeMap<String, Layer>,
        issue: BTreeMap<u64, BootIssue>,
        played_s: f64,
        played_at: Option<(u64, u32, f64)>,
    ) -> Result<(), String> {
        for (name, l) in &layers {
            if *name != l.name || !name_ok(name, LAYER_NAME_MAX) {
                return Err(format!("layer {name:?}: bad name"));
            }
            if l.expire_after_s.is_some_and(|s| !s.is_finite() || s <= 0.0) {
                return Err(format!("layer {name:?}: expire_after_s"));
            }
            if l.cap == Some(0) {
                return Err(format!("layer {name:?}: cap 0"));
            }
        }
        let mut labels = BTreeMap::new();
        let mut live_ids = BTreeSet::new();
        let mut live_ranks = BTreeSet::new();
        let mut draw_rank = 0u64;
        let mut issue = issue;
        let mut held_next = 0u64;
        for (boot, i) in &issue {
            if i.next > dcs_ids::SLOTS {
                return Err(format!("boot {boot}: next slot {}", i.next));
            }
        }
        for (id, m) in &items {
            if *id != m.id || id.0 == 0 || id.0 >= self.next_entity {
                return Err(format!("marker {}: at or past the next EntityId", id.0));
            }
            if self.groups.contains_key(id) || self.units.contains_key(id) {
                return Err(format!("marker {} is also a group or unit", id.0));
            }
            if m.expires_at.is_some_and(|t| !t.is_finite()) {
                return Err(format!("marker {}: expires_at", id.0));
            }
            let kind = m.kind();
            if kind == MarkerKind::MenuItem {
                // A menu item has no DCS ID, label or layer.
                let parent = m.parent();
                if parent.0 != 0
                    && !items
                        .get(&parent)
                        .is_some_and(|p| p.kind() == MarkerKind::MenuItem)
                {
                    return Err(format!(
                        "menu item {}: parent {} is no menu item",
                        id.0, parent.0
                    ));
                }
                continue;
            }
            if !m.label.is_empty() && !name_ok(&m.label, LABEL_MAX) {
                return Err(format!("marker {}: bad label", id.0));
            }
            if !m.layer.is_empty() && !name_ok(&m.layer, LAYER_NAME_MAX) {
                return Err(format!("marker {}: bad layer name", id.0));
            }
            // 0 is an item DCS does not hold: new, hidden, or not added since
            // the boot or mission load.
            if m.dcs_id != 0 {
                if !dcs_ids::in_pool(m.dcs_id) {
                    return Err(format!(
                        "marker {}: DCS ID {} out of the pool",
                        id.0, m.dcs_id
                    ));
                }
                held_next = held_next.max(m.dcs_id - dcs_ids::BASE + 1);
            }
            draw_rank = draw_rank.max(m.rank);
            if m.removed {
                // A removed item keeps its layer's name only.
                continue;
            }
            if m.rank != 0 && !live_ranks.insert(m.rank) {
                return Err(format!("marker {}: draw rank {} held twice", id.0, m.rank));
            }
            if !m.layer.is_empty() && !layers.contains_key(&m.layer) {
                return Err(format!("marker {}: layer {:?} has no row", id.0, m.layer));
            }
            if m.dcs_id != 0 && !live_ids.insert(m.dcs_id) {
                return Err(format!("marker {}: DCS ID {} held twice", id.0, m.dcs_id));
            }
            if !m.label.is_empty() && labels.insert(m.label.clone(), *id).is_some() {
                return Err(format!("marker {}: label {:?} held twice", id.0, m.label));
            }
        }
        if !played_s.is_finite() || played_s < 0.0 {
            return Err("played_s".into());
        }
        if played_at.is_some_and(|(_, _, t)| !t.is_finite() || t < 0.0) {
            return Err("played_at".into());
        }
        self.markers.items = items;
        self.markers.draw_rank = draw_rank;
        self.markers.layers = layers;
        self.markers.labels = labels;
        // The current boot is the newest. A store with IDs and no boot (made
        // before the marks were kept) issues in boot 0.
        let current = issue
            .iter()
            .max_by_key(|(_, i)| i.ordinal)
            .map(|(b, _)| *b)
            .or_else(|| {
                (held_next != 0).then(|| {
                    issue.insert(
                        0,
                        BootIssue {
                            ordinal: 1,
                            ..BootIssue::default()
                        },
                    );
                    0
                })
            });
        if let Some(b) = current {
            let i = issue.get_mut(&b).expect("boot");
            i.next = i.next.max(held_next);
        }
        self.markers.issue = issue;
        self.markers.boot = current;
        self.markers.played_s = played_s;
        self.markers.played_at = played_at;
        Ok(())
    }

    /// Campaign model time, seconds: what the campaign's missions ran.
    pub fn played_s(&self) -> f64 {
        self.markers.played_s
    }

    /// Count the model time of a sample into the campaign model time. A new
    /// agent boot or generation starts counting from its first sample.
    pub(super) fn count_played(&mut self, boot: u64, gen_: u32, t: f64) {
        // A sample the agent sent with no usable time counts for nothing.
        if !t.is_finite() || t < 0.0 {
            return;
        }
        if let Some((b, g, last)) = self.markers.played_at
            && (b, g) == (boot, gen_)
            && t > last
        {
            self.markers.played_s += t - last;
        }
        self.markers.played_at = Some((boot, gen_, t));
    }

    /// Remove the live marks and drawings whose expiry has come, at most
    /// `limit`. Returns those removed.
    pub fn expire_markers(&mut self, limit: usize) -> Vec<EntityId> {
        let now = self.markers.played_s;
        let due: Vec<EntityId> = self
            .markers
            .items
            .values()
            .filter(|m| !m.removed && m.expires_at.is_some_and(|t| t <= now))
            .map(|m| m.id)
            .take(limit)
            .collect();
        let mut out = Vec::new();
        for id in due {
            out.extend(
                self.remove_marker_as(id, RemoveCause::Expired)
                    .unwrap_or_default(),
            );
        }
        out
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

    /// The revision floors of every extension name removed.
    #[cfg(feature = "disk")]
    pub(super) fn extension_floors(&self) -> &BTreeMap<String, u64> {
        &self.markers.ext_floor
    }

    /// Set the revision floors read back from a store file. Each is a tombstone,
    /// so the agent may still hold the extension: the removal is pending until
    /// the agent confirms it.
    #[cfg(feature = "disk")]
    pub(super) fn set_extension_floors(&mut self, floors: BTreeMap<String, u64>) {
        self.markers.ext_removed = floors.keys().cloned().collect();
        self.markers.ext_floor = floors;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CampaignId;
    use nanoprobe_transport::codec::Lv;

    fn drawing(n: f64) -> MarkerSpec {
        MarkerSpec::Drawing {
            shape: 7,
            args: Lv::Table(vec![(Lv::Number(1.0), Lv::Number(n))]),
        }
    }

    fn layout(points: usize) -> Vec<Lv> {
        let point = |i: usize| {
            Lv::Table(vec![
                (Lv::String(b"x".to_vec()), Lv::Number(i as f64)),
                (Lv::String(b"y".to_vec()), Lv::Number(0.0)),
                (Lv::String(b"z".to_vec()), Lv::Number(2.0)),
            ])
        };
        let rgba = || crate::sim::wire::lv::list(vec![Lv::Number(1.0); 4]);
        let mut v: Vec<Lv> = (0..points).map(point).collect();
        v.extend([
            rgba(),
            rgba(),
            Lv::Number(1.0),
            Lv::Bool(true),
            Lv::String(b"m".to_vec()),
        ]);
        v
    }

    #[test]
    fn only_known_shapes_with_their_layouts_are_ok() {
        let list = crate::sim::wire::lv::list;
        let ok = |shape, points| drawing_layout_ok(shape, &list(layout(points)));
        for (shape, points) in [
            (1, 2),
            (3, 2),
            (3, 9),
            (4, 2),
            (5, 1),
            (6, 4),
            (7, 3),
            (7, 8),
        ] {
            assert!(ok(shape, points), "shape {shape} with {points} points");
        }
        for (shape, points) in [
            (0, 2),
            (2, 1),
            (2, 2),
            (8, 4),
            (9, 2),
            (12, 4),
            (1, 1),
            (1, 3),
            (3, 1),
            (4, 3),
            (5, 2),
            (6, 3),
            (6, 5),
            (7, 2),
        ] {
            assert!(!ok(shape, points), "shape {shape} with {points} points");
        }
        // A point that is not a table of finite x, y and z.
        let mut v = layout(2);
        v[1] = Lv::Number(5.0);
        assert!(!drawing_layout_ok(1, &list(v)));
        let mut v = layout(2);
        v[0] = Lv::Table(vec![
            (Lv::String(b"x".to_vec()), Lv::Number(f64::NAN)),
            (Lv::String(b"y".to_vec()), Lv::Number(0.0)),
            (Lv::String(b"z".to_vec()), Lv::Number(0.0)),
        ]);
        assert!(!drawing_layout_ok(1, &list(v)));
        // A color or line type that is not finite, or a color that is not a
        // list of four.
        let nan = Lv::Number(f64::NAN);
        let mut v = layout(2);
        v[2] = crate::sim::wire::lv::list(vec![
            nan.clone(),
            Lv::Number(0.0),
            Lv::Number(0.0),
            Lv::Number(1.0),
        ]);
        assert!(!drawing_layout_ok(1, &list(v)));
        for bad in [nan, Lv::Number(f64::INFINITY)] {
            let mut v = layout(2);
            v[4] = bad;
            assert!(!drawing_layout_ok(1, &list(v)));
        }
        let sparse = Lv::Table(
            [1.0, 2.0, 3.0, 9.0]
                .into_iter()
                .map(|k| (Lv::Number(k), Lv::Number(1.0)))
                .collect(),
        );
        let mut extra = match crate::sim::wire::lv::list(vec![Lv::Number(1.0); 4]) {
            Lv::Table(p) => p,
            _ => unreachable!(),
        };
        extra.push((Lv::String(b"a".to_vec()), Lv::Number(1.0)));
        for color in [sparse, Lv::Table(extra)] {
            let mut v = layout(2);
            v[3] = color;
            assert!(!drawing_layout_ok(1, &list(v)));
        }
        // A style of the wrong types, or one short.
        let mut v = layout(2);
        v[5] = Lv::Number(1.0);
        assert!(!drawing_layout_ok(1, &list(v)));
        let mut v = layout(2);
        v.pop();
        assert!(!drawing_layout_ok(1, &list(v)));
        // Not a list: a string key, or a gap in the keys.
        let mut pairs = match list(layout(2)) {
            Lv::Table(p) => p,
            _ => unreachable!(),
        };
        pairs.push((Lv::String(b"extra".to_vec()), Lv::Number(1.0)));
        assert!(!drawing_layout_ok(1, &Lv::Table(pairs)));
        let mut pairs = match list(layout(2)) {
            Lv::Table(p) => p,
            _ => unreachable!(),
        };
        pairs[6].0 = Lv::Number(9.0);
        assert!(!drawing_layout_ok(1, &Lv::Table(pairs)));
        assert!(!drawing_layout_ok(1, &Lv::Number(1.0)));
    }

    fn circle(radius: f64, theater: &str) -> MarkerPut {
        use super::super::drawing::{LineType, MapPoint, ShapeKind};
        MarkerPut {
            drawing: Some(DrawingShape {
                theater: theater.into(),
                color: [1.0, 0.0, 0.0, 1.0],
                fill: [1.0, 0.0, 0.0, 0.5],
                line_type: LineType::Solid,
                text: String::new(),
                read_only: true,
                kind: ShapeKind::Circle {
                    center: MapPoint::new(1.0, 2.0),
                    radius,
                },
            }),
            ..put(drawing(0.0), "c", "")
        }
    }

    #[test]
    fn a_typed_drawing_is_stored_with_its_lowering() {
        let mut s = Store::new(CampaignId(1), 10);
        let a = s.put_marker_with(circle(100.0, "Caucasus")).unwrap();
        let m = s.marker(a.id).unwrap();
        let (code, args) = m.drawing.as_ref().unwrap().lower();
        assert_eq!(m.spec, MarkerSpec::Drawing { shape: code, args });
        // The same put changes nothing; a new theater alone is a new revision.
        assert_eq!(
            s.put_marker_with(circle(100.0, "Caucasus")).unwrap().rev,
            a.rev
        );
        assert_eq!(
            s.put_marker_with(circle(100.0, "Syria")).unwrap().rev,
            a.rev + 1
        );
    }

    #[test]
    fn a_circle_radius_alone_is_in_place() {
        let lowered = |r: f64| circle(r, "Caucasus").drawing.unwrap().lower();
        let ((code, a), (_, b)) = (lowered(100.0), lowered(250.0));
        let changes = drawing_style_change(code, &a, &b).expect("in place");
        assert_eq!(
            crate::sim::wire::lv::get(&changes, "radius"),
            Some(&Lv::Number(250.0))
        );
        // A radius is a circle's only: a polygon's second point is not one.
        let tri = |x: f64| {
            let mut v = layout(3);
            v[1] = crate::sim::wire::lv::obj(vec![
                ("x", Lv::Number(x)),
                ("y", Lv::Number(0.0)),
                ("z", Lv::Number(0.0)),
            ]);
            crate::sim::wire::lv::list(v)
        };
        assert!(drawing_style_change(3, &tri(1.0), &tri(2.0)).is_none());
    }

    fn mark(text: &str) -> MarkerSpec {
        MarkerSpec::Mark {
            theater: "Caucasus".into(),
            at: Point {
                x: 1.0,
                y: 0.0,
                z: 2.0,
            },
            text: text.into(),
            read_only: true,
        }
    }

    fn put(spec: MarkerSpec, label: &str, layer: &str) -> MarkerPut {
        MarkerPut {
            id: EntityId(0),
            scope: scope::ALL,
            target: 0,
            spec,
            drawing: None,
            label: Some(label.into()),
            layer: Some(layer.into()),
            expire_after_s: None,
        }
    }

    fn layer(name: &str) -> Layer {
        Layer::new(name)
    }

    fn dcs(s: &Store, e: EntityId) -> u64 {
        s.marker(e).unwrap().dcs_id
    }

    fn boot(s: &mut Store, agent_boot: u64) {
        s.set_loaded(crate::store::Loaded {
            agent_boot,
            mission_gen: 1,
            theater: "Caucasus".into(),
        });
    }

    #[test]
    fn an_item_reads_dcs_id_0_until_an_add_issues_one_from_the_shared_pool() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        s.put_layer(layer("top")).unwrap();
        let t = s.put_marker_with(put(drawing(1.0), "t", "top")).unwrap();
        let n = s.put_marker_with(put(drawing(1.0), "n", "")).unwrap();
        let m = s.put_marker_with(put(mark("m"), "m", "top")).unwrap();
        for e in [t.id, n.id, m.id] {
            assert_eq!(dcs(&s, e), 0);
        }
        for e in [n.id, t.id, m.id] {
            s.issue_dcs_id(e, false).unwrap();
        }
        // Marks and drawings take the next slot in the order they ask.
        assert_eq!(dcs(&s, n.id), dcs_ids::BASE);
        assert_eq!(dcs(&s, t.id), dcs_ids::BASE + 1);
        assert_eq!(dcs(&s, m.id), dcs_ids::BASE + 2);
        assert_eq!(s.dcs_slots_left(), dcs_ids::SLOTS - 3);
        // The pool ends at 2^31 - 1, the highest ID DCS draws.
        assert_eq!(dcs_ids::BASE + dcs_ids::SLOTS - 1, (1 << 31) - 1);
        // The draw rank rises with every add; an item never added reads 0.
        assert_eq!(
            [n.id, t.id, m.id].map(|e| s.marker(e).unwrap().rank),
            [1, 2, 3]
        );
        s.put_marker_with(put(drawing(9.0), "n", "")).unwrap();
        assert_eq!(s.marker(n.id).unwrap().rank, 1, "a put keeps the rank");
        s.issue_dcs_id(n.id, false).unwrap();
        assert_eq!(s.marker(n.id).unwrap().rank, 4);
        // An ID is never given twice.
        let again = s.issue_dcs_id(n.id, false).unwrap().0;
        assert_eq!(again, dcs_ids::BASE + 4);
    }

    #[test]
    fn a_removed_item_and_its_label_never_free_a_slot() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        let a = s.put_marker_with(put(mark("a"), "a", "")).unwrap();
        let first = s.issue_dcs_id(a.id, false).unwrap().0;
        s.remove_marker(a.id).unwrap();
        let b = s.put_marker_with(put(mark("a"), "a", "")).unwrap();
        assert!(s.issue_dcs_id(b.id, false).unwrap().0 > first);
    }

    #[test]
    fn hide_reads_0_and_a_show_issues_a_new_id_at_the_same_revision() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        s.put_layer(layer("p")).unwrap();
        let d = s.put_marker_with(put(drawing(1.0), "d", "p")).unwrap();
        let m = s.put_marker_with(put(mark("m"), "m", "p")).unwrap();
        let (d0, m0) = (
            s.issue_dcs_id(d.id, false).unwrap().0,
            s.issue_dcs_id(m.id, false).unwrap().0,
        );
        let rev = s.marker(d.id).unwrap().spec_rev;
        s.put_layer(Layer {
            visible: false,
            ..layer("p")
        })
        .unwrap();
        assert_eq!((dcs(&s, d.id), dcs(&s, m.id)), (0, 0));
        assert_eq!(s.marker(d.id).unwrap().spec_rev, rev);
        s.put_layer(layer("p")).unwrap();
        assert_eq!(dcs(&s, d.id), 0);
        let (d1, m1) = (
            s.issue_dcs_id(d.id, false).unwrap().0,
            s.issue_dcs_id(m.id, false).unwrap().0,
        );
        assert!(d1 > d0 && m1 > m0);
    }

    #[test]
    fn a_new_boot_reads_0_and_only_a_new_boot_resets_the_mark() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        s.put_layer(layer("p")).unwrap();
        let d = s.put_marker_with(put(drawing(1.0), "d", "p")).unwrap();
        let first = s.issue_dcs_id(d.id, false).unwrap().0;
        // A mission load in the boot reads 0 and keeps the mark.
        s.new_generation();
        assert_eq!(dcs(&s, d.id), 0);
        assert_eq!(s.issue_dcs_id(d.id, false).unwrap().0, first + 1);
        // The same boot again changes nothing.
        boot(&mut s, 7);
        assert_eq!(s.dcs_slots_left(), dcs_ids::SLOTS - 2);
        // A new boot starts at slot 0 and keeps the old boot's span.
        boot(&mut s, 8);
        assert_eq!(s.dcs_slots_left(), dcs_ids::SLOTS);
        assert_eq!(s.issue_dcs_id(d.id, false).unwrap().0, first);
    }

    #[test]
    fn the_server_mark_raises_a_boots_mark_and_a_stale_refusal_raises_it_past_the_shim() {
        let mut s = Store::new(CampaignId(1), 10);
        // A mark for a boot the store has not begun applies when it begins.
        s.raise_dcs_mark(7, 40);
        boot(&mut s, 7);
        assert_eq!(s.dcs_mark(), Some((7, 40)));
        // A mark never goes down, and another boot's mark is not this one's.
        s.raise_dcs_mark(7, 10);
        s.raise_dcs_mark(6, 90);
        assert_eq!(s.dcs_mark(), Some((7, 40)));
        s.raise_dcs_mark(7, 50);
        assert_eq!(s.dcs_mark(), Some((7, 50)));
        // The shim's process mark is an ID: the next slot is past it.
        assert!(s.raise_dcs_mark_past(dcs_ids::BASE + 99));
        assert_eq!(s.dcs_mark(), Some((7, 100)));
        assert!(!s.raise_dcs_mark_past(dcs_ids::BASE + 3));
        assert!(!s.raise_dcs_mark_past(5));
        let a = s.put_marker_with(put(mark("a"), "a", "")).unwrap();
        assert_eq!(s.issue_dcs_id(a.id, false).unwrap().0, dcs_ids::BASE + 100);
    }

    #[test]
    fn the_issued_spans_are_kept_per_boot() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        let a = s.put_marker_with(put(mark("a"), "a", "")).unwrap();
        let id1 = s.issue_dcs_id(a.id, false).unwrap().0;
        let id2 = s.issue_dcs_id(a.id, false).unwrap().0;
        assert!(s.dcs_issued_in_boot(7, id1) && s.dcs_issued_in_boot(7, id2));
        assert!(!s.dcs_issued_in_boot(7, id2 + 1));
        assert!(!s.dcs_issued_in_boot(7, dcs_ids::BASE - 1));
        assert!(!s.dcs_issued_in_boot(9, id1));
        boot(&mut s, 8);
        assert!(s.dcs_issued_in_boot(7, id2) && !s.dcs_issued_now(id1));
        let id3 = s.issue_dcs_id(a.id, false).unwrap().0;
        assert_eq!(id3, id1);
        assert!(s.dcs_issued_now(id1) && !s.dcs_issued_now(id2));
        assert_eq!(s.dcs_boot(), Some(8));
        // Only the newest boots are kept.
        for b in 100..100 + ISSUE_BOOTS_KEEP as u64 {
            boot(&mut s, b);
        }
        assert!(!s.dcs_issued_in_boot(7, id1) && !s.dcs_issued_in_boot(8, id1));
    }

    #[test]
    fn a_typed_change_with_the_same_lowering_needs_a_slot() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        s.set_slot_limit_for_test(1);
        let a = s.put_marker_with(circle(100.0, "Caucasus")).unwrap();
        s.issue_dcs_id(a.id, false).unwrap();
        // A new theater keeps the lowering but is added again: no slot is left.
        assert_eq!(
            s.put_marker_with(circle(100.0, "Syria")),
            Err(MarkerError::DcsIdsFull(0))
        );
        // A radius change is in place and needs none, and so does one that
        // comes with a new theater: the call differs only in its style.
        assert!(s.put_marker_with(circle(200.0, "Caucasus")).is_ok());
        assert!(s.put_marker_with(circle(300.0, "Syria")).is_ok());
    }

    #[test]
    fn a_full_pool_refuses_with_nothing_written() {
        let mut s = Store::new(CampaignId(1), 10);
        boot(&mut s, 7);
        s.set_slot_limit_for_test(2);
        let a = s.put_marker_with(put(drawing(1.0), "a", "")).unwrap();
        let b = s.put_marker_with(put(drawing(1.0), "b", "")).unwrap();
        // Waiting items count against the pool before they are issued.
        assert_eq!(
            s.put_marker_with(put(drawing(1.0), "c", "")),
            Err(MarkerError::DcsIdsFull(0))
        );
        s.issue_dcs_id(a.id, false).unwrap();
        s.issue_dcs_id(b.id, false).unwrap();
        // Removing frees no slot above the mark.
        s.remove_marker(a.id).unwrap();
        let state = |s: &Store| {
            (
                s.markers.items.clone(),
                s.markers.labels.clone(),
                s.entries().len(),
                s.revision(),
            )
        };
        let before = state(&s);
        assert_eq!(
            s.put_marker_with(put(drawing(1.0), "c", "")),
            Err(MarkerError::DcsIdsFull(0))
        );
        assert_eq!(state(&s), before);
        assert_eq!(s.issue_dcs_id(b.id, false), Err(MarkerError::DcsIdsFull(0)));
        assert_eq!(s.dcs_slots_left(), 0);
        // A new boot has its slots again.
        boot(&mut s, 8);
        assert!(s.put_marker_with(put(drawing(1.0), "c", "")).is_ok());
    }

    #[test]
    fn a_cap_removes_the_oldest_and_a_replacement_holds_exactly_its_items() {
        let mut s = Store::new(CampaignId(1), 10);
        s.put_layer(Layer {
            cap: Some(2),
            ..Layer::new("p")
        })
        .unwrap();
        let a = s.put_marker_with(put(drawing(1.0), "a", "p")).unwrap();
        s.put_marker_with(put(drawing(1.0), "b", "p")).unwrap();
        let c = s.put_marker_with(put(drawing(1.0), "c", "p")).unwrap();
        assert_eq!(c.evicted, vec![a.id]);
        assert_eq!(
            s.marker(a.id).unwrap().removed_cause,
            Some(RemoveCause::Evicted)
        );
        // A replacement: b changes, c goes, d comes; an unlabelled item goes.
        s.put_layer(Layer {
            cap: Some(3),
            ..Layer::new("p")
        })
        .unwrap();
        let mut free = put(drawing(1.0), "", "p");
        free.label = None;
        let f = s.put_marker_with(free).unwrap();
        let b_id = s.marker_by_label("b").unwrap();
        let (ids, removed) = s
            .replace_layer(
                "p",
                vec![put(drawing(5.0), "b", "x"), put(mark("d"), "d", "x")],
            )
            .unwrap();
        assert_eq!(ids[0], ("b".to_owned(), b_id));
        assert!(removed.contains(&c.id) && removed.contains(&f.id));
        assert_eq!(s.marker(b_id).unwrap().spec_rev, 2);
        assert_eq!(s.marker(ids[1].1).unwrap().layer, "p");
        // Over the cap, a label of another layer, or a label twice: nothing
        // is written.
        let rev = s.revision();
        let four = vec![
            put(drawing(1.0), "w", ""),
            put(drawing(1.0), "x", ""),
            put(drawing(1.0), "y", ""),
            put(drawing(1.0), "z", ""),
        ];
        assert_eq!(s.replace_layer("p", four), Err(MarkerError::BadCap));
        assert_eq!(s.revision(), rev);
        s.put_marker_with(put(drawing(1.0), "elsewhere", "q"))
            .unwrap();
        let rev2 = s.revision();
        assert_eq!(
            s.replace_layer("p", vec![put(drawing(1.0), "elsewhere", "")]),
            Err(MarkerError::LabelTaken("elsewhere".into()))
        );
        assert_eq!(
            s.replace_layer(
                "p",
                vec![put(drawing(1.0), "b", ""), put(drawing(1.0), "b", "")]
            ),
            Err(MarkerError::BadLabel)
        );
        assert_eq!(s.revision(), rev2);
        assert!(rev2 > rev);
        // Removing the layer removes its items.
        let gone = s.remove_layer("p").unwrap();
        assert_eq!(gone.len(), 2);
        assert!(s.layer("p").is_none());
        assert_eq!(
            s.remove_layer("p"),
            Err(MarkerError::NoSuchLayer("p".into()))
        );
    }

    /// The journal a front line of 50 segments makes when the controller
    /// replaces it often: nothing for a replacement that changes nothing,
    /// one `Spec` for each changed segment, a `Removed` for each dropped one.
    #[test]
    fn a_repeated_replacement_journals_only_what_changed() {
        let mut s = Store::new(CampaignId(1), 10);
        let segments = |n: usize, moved: f64| -> Vec<MarkerPut> {
            (0..n)
                .map(|i| {
                    put(
                        drawing(i as f64 + moved * f64::from(u8::from(i == 0))),
                        &format!("seg-{i}"),
                        "",
                    )
                })
                .collect()
        };
        s.replace_layer("front", segments(50, 0.0)).unwrap();
        let base = s.entries().len();
        for _ in 0..100 {
            s.replace_layer("front", segments(50, 0.0)).unwrap();
        }
        assert_eq!(
            s.entries().len(),
            base,
            "an equal replacement journals nothing"
        );
        s.replace_layer("front", segments(50, 0.5)).unwrap();
        assert_eq!(s.entries().len(), base + 1, "one segment moved");
        s.replace_layer("front", segments(40, 0.5)).unwrap();
        assert_eq!(s.entries().len(), base + 11, "ten segments dropped");
    }

    #[test]
    fn expiry_counts_campaign_model_time_across_generations() {
        let mut s = Store::new(CampaignId(1), 10);
        s.put_layer(Layer {
            expire_after_s: Some(100.0),
            ..Layer::new("short")
        })
        .unwrap();
        let a = s.put_marker_with(put(drawing(1.0), "a", "short")).unwrap();
        let mut long = put(drawing(1.0), "b", "short");
        long.expire_after_s = Some(500.0);
        let b = s.put_marker_with(long).unwrap();
        assert_eq!(s.marker(a.id).unwrap().expires_at, Some(100.0));
        s.count_played(1, 1, 10.0);
        s.count_played(1, 1, 70.0);
        // A new generation starts from its own first sample.
        s.count_played(1, 2, 5.0);
        s.count_played(1, 2, 44.0);
        assert_eq!(s.played_s(), 99.0);
        assert!(s.expire_markers(10).is_empty());
        s.count_played(1, 2, 45.0);
        assert_eq!(s.expire_markers(10), vec![a.id]);
        assert_eq!(
            s.marker(a.id).unwrap().removed_cause,
            Some(RemoveCause::Expired)
        );
        assert!(!s.marker(b.id).unwrap().removed);
        assert_eq!(
            s.put_marker_with(MarkerPut {
                expire_after_s: Some(f64::NAN),
                ..put(drawing(1.0), "c", "")
            }),
            Err(MarkerError::BadExpiry)
        );
    }

    #[test]
    fn a_sample_with_no_usable_time_counts_for_nothing() {
        let mut s = Store::new(CampaignId(1), 10);
        s.count_played(1, 1, 10.0);
        s.count_played(1, 1, 20.0);
        for bad in [f64::INFINITY, f64::NAN, f64::NEG_INFINITY, -5.0] {
            s.count_played(1, 1, bad);
        }
        assert_eq!(s.played_s(), 10.0);
        // The last good sample still anchors the next one.
        s.count_played(1, 1, 25.0);
        assert_eq!(s.played_s(), 15.0);
    }

    #[test]
    fn a_refused_replace_layer_or_put_layer_changes_nothing() {
        let mut s = Store::new(CampaignId(1), 10);
        s.put_layer(layer("a")).unwrap();
        s.put_layer(layer("b")).unwrap();
        for i in 0..2 {
            s.put_marker_with(put(drawing(1.0), &format!("a{i}"), "a"))
                .unwrap();
            s.put_marker_with(put(drawing(1.0), &format!("b{i}"), "b"))
                .unwrap();
        }
        s.put_marker_with(put(mark("m"), "m0", "a")).unwrap();
        s.set_slot_limit_for_test(2);
        let state = |s: &Store| {
            (
                s.markers.items.clone(),
                s.markers.layers.clone(),
                s.markers.labels.clone(),
                s.entries().len(),
                s.revision(),
            )
        };
        let before = state(&s);
        // Replacing "a" with three drawings of which two are new needs a
        // third slot in its band; removing its mark frees none of them.
        let items = ["a0", "n1", "n2"].map(|l| put(drawing(2.0), l, "")).into();
        assert!(matches!(
            s.replace_layer("a", items),
            Err(MarkerError::DcsIdsFull(_))
        ));
        assert_eq!(state(&s), before);
        // Three marks over a range of two slots.
        let items = ["m0", "m1", "m2"].map(|l| put(mark(l), l, "")).into();
        assert!(matches!(
            s.replace_layer("a", items),
            Err(MarkerError::DcsIdsFull(_))
        ));
        assert_eq!(state(&s), before);
        // Hidden, the layer's three items wait for no slot; shown again they
        // need three, and the pool holds two for the two items of "b".
        s.put_layer(Layer {
            visible: false,
            ..layer("a")
        })
        .unwrap();
        let before = state(&s);
        assert!(matches!(
            s.put_layer(layer("a")),
            Err(MarkerError::DcsIdsFull(_))
        ));
        assert_eq!(state(&s), before);
    }

    #[test]
    fn a_replace_layer_short_of_entity_ids_changes_nothing() {
        let mut s = Store::new(CampaignId(1), 10);
        s.put_layer(layer("a")).unwrap();
        s.put_marker_with(put(drawing(1.0), "old", "a")).unwrap();
        s.put_marker_with(put(drawing(1.0), "keep", "a")).unwrap();
        // Two entity IDs are left; the call needs three new ones.
        s.reserve_id(EntityId(crate::ids::MAX_ID - 2));
        assert_eq!(s.ids_left(), 2);
        let state = |s: &Store| {
            (
                s.markers.items.clone(),
                s.markers.labels.clone(),
                s.entries().len(),
                s.revision(),
                s.next_entity(),
            )
        };
        let before = state(&s);
        let items = ["keep", "n1", "n2", "n3"]
            .map(|l| put(drawing(2.0), l, ""))
            .into();
        assert_eq!(s.replace_layer("a", items), Err(MarkerError::Exhausted));
        assert_eq!(state(&s), before);
        // Two new items fit.
        let items = ["keep", "n1", "n2"]
            .map(|l| put(drawing(2.0), l, ""))
            .into();
        assert!(s.replace_layer("a", items).is_ok());
    }

    /// A cap of 0 has no row encoding (the file reads 0 as no cap), so the
    /// check is made on the layers a restore is given.
    #[cfg(feature = "disk")]
    #[test]
    fn a_restore_refuses_a_layer_with_cap_0() {
        let mut s = Store::new(CampaignId(1), 10);
        let layers = BTreeMap::from([(
            "a".to_owned(),
            Layer {
                cap: Some(0),
                ..Layer::new("a")
            },
        )]);
        let e = s
            .restore_markers(BTreeMap::new(), layers, BTreeMap::new(), 0.0, None)
            .unwrap_err();
        assert!(e.contains("cap 0"), "{e}");
    }
}
