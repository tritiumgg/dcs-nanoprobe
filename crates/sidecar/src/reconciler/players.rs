//! What the reconciler does for the player-facing commands (spec/commands.md,
//! "Marks, drawings and menu items in DCS", `MESSAGE`, `CHAT`, `EVAL`,
//! `MISSION_LOAD`, `EXTENSION_REMOVE`).
//!
//! Marks, drawings and menu items are set-state entities: each is sent once
//! per spec revision and mission generation, and removals are sent until the
//! agent confirms. Messages, chat, evals and mission loads are effects: sent
//! once, never derived again. A child menu item follows its submenu, and a
//! removal reaches the items under a submenu before the submenu.

use std::collections::{BTreeSet, HashMap, HashSet};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result as rc;
use nanoprobe_transport::records::CommandResult;

use super::{NotReady, Reconciler, rejected_for_now};
use crate::ids::EntityId;
use crate::sim::SimAdapter;
use crate::sim::wire::{Body, lv, opcode};
use crate::store::{
    Journalled, MarkerKind, MarkerSpec, Outcome, Store, drawing_style_change, scope,
};

const HOLD_PIN_SET: u8 = 7;
const HOLD_PIN_REMOVE: u8 = 8;

/// The most marker and extension commands one step sends, so a campaign with
/// many items does not fill the ring in one frame.
pub const PINS_PER_STEP: usize = 32;

const SETS: [u16; 4] = [
    opcode::MARK_SET,
    opcode::DRAWING_SET,
    opcode::DRAWING_UPDATE,
    opcode::MENU_SET,
];

/// What the reconciler knows about the marks, drawings and menu items in DCS
/// and about extension removals in flight.
#[derive(Debug, Default)]
pub(super) struct PinState {
    /// The spec revision the agent holds, in this agent boot and generation.
    held: HashMap<EntityId, u64>,
    /// The DCS ID, scope, target and spec of each drawing the agent holds at
    /// its held revision, for an update in place.
    held_spec: HashMap<EntityId, (u64, u32, u64, MarkerSpec)>,
    /// The DCS ID and spec revision of the last add sent or tried for each
    /// mark and drawing, until the agent applies it: an add that did not take
    /// is sent again under the same ID, one that may have is answered
    /// without touching DCS (spec/commands.md, "Marks, drawings and menu
    /// items in DCS"). Any other add takes a never-used ID.
    issued: HashMap<EntityId, (u64, u64)>,
    /// The adds the shim refused as at or below its process mark and that
    /// were tried again under a fresh ID, with the revision: a second such
    /// refusal at the same revision is final.
    retried: HashMap<EntityId, u64>,
    /// A revision the agent refused, with the generation: not sent again
    /// until either changes.
    refused: HashMap<EntityId, (u64, u32)>,
    /// Removed markers DCS holds no more in this generation, or that the
    /// agent refused to remove.
    gone: HashSet<EntityId>,
    /// Live marks and drawings the shim may hold from before this sidecar
    /// started: an item of a hidden layer among them is removed once.
    unsure: HashSet<EntityId>,
    /// An `EXTENSION_REMOVE` the agent answered `NOT_READY`.
    ext_not_ready: HashMap<String, NotReady>,
    /// The extension each `EXTENSION_REMOVE` in flight names.
    ext_removing: HashMap<(u64, u64), String>,
}

/// The mark in a shim refusal `id <ID> at or below the process mark <N>`.
fn process_mark(message: &str) -> Option<u64> {
    let tail = message.split("process mark ").nth(1)?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl PinState {
    /// A sidecar's start: the agent of the stored mission may still hold any
    /// live mark or drawing, and every removed one not purged
    /// (spec/persistence.md, "Restore").
    pub(super) fn starting(store: &Store) -> PinState {
        PinState {
            unsure: store
                .markers()
                .filter(|m| !m.removed && m.kind() != MarkerKind::MenuItem)
                .map(|m| m.id)
                .collect(),
            ..PinState::default()
        }
    }

    /// The agent holds none of them: a new agent boot, campaign or mission
    /// generation. A marker removed already is gone with the rest.
    pub(super) fn forget(&mut self, store: &Store) {
        self.issued.clear();
        self.retried.clear();
        self.held.clear();
        self.held_spec.clear();
        self.unsure.clear();
        self.refused.clear();
        self.gone = store
            .markers()
            .filter(|m| m.removed)
            .map(|m| m.id)
            .collect();
    }
}

impl Reconciler {
    /// A new agent boot, campaign or mission generation: DCS holds no mark,
    /// drawing or menu item. A new boot or campaign holds no extension
    /// either.
    pub(super) fn pins_forget(&mut self, store: &mut Store, extensions: bool) {
        self.pins.forget(store);
        if extensions {
            self.pins.ext_not_ready.clear();
            let names: Vec<String> = store.extension_removals().map(str::to_owned).collect();
            for n in names {
                store.extension_gone(&n);
            }
        }
    }

    /// Whether a removed marker is one DCS holds no more in this generation.
    pub(super) fn pins_gone(&self, id: EntityId) -> bool {
        self.pins.gone.contains(&id)
    }

    /// A removed marker was purged: nothing is kept of it.
    pub(super) fn pins_purged(&mut self, id: EntityId) {
        self.pins.gone.remove(&id);
        self.pins.held.remove(&id);
        self.pins.held_spec.remove(&id);
        self.pins.unsure.remove(&id);
        self.pins.refused.remove(&id);
        self.pins.issued.remove(&id);
        self.pins.retried.remove(&id);
    }

    /// The DCS ID and spec revision for an add of a mark or drawing: those of
    /// an add of this revision that has not taken, else a never-used ID from
    /// the store, committed with the command that carries it. An item the
    /// agent may hold at this revision (it holds it, or the sidecar just
    /// started) goes to a new revision, since the agent answers a set at a
    /// held revision `AT_REVISION` without adding. `None` when the item's
    /// pool has no slot left: the revision is refused.
    fn add_id(&mut self, store: &mut Store, id: EntityId, mission_gen: u32) -> Option<(u64, u64)> {
        let m = store.marker(id)?;
        let rev = m.spec_rev;
        if m.dcs_id != 0 && self.pins.issued.get(&id) == Some(&(m.dcs_id, rev)) {
            return Some((m.dcs_id, rev));
        }
        let maybe_held =
            self.pins.held.get(&id).is_some_and(|h| *h >= rev) || self.pins.unsure.contains(&id);
        match store.issue_dcs_id(id, maybe_held) {
            Ok((dcs_id, rev)) => {
                self.pins.issued.insert(id, (dcs_id, rev));
                Some((dcs_id, rev))
            }
            Err(_) => {
                self.pins.refused.insert(id, (rev, mission_gen));
                None
            }
        }
    }

    /// The spec revision of an extension the agent applied, as far as its
    /// answers show; 0 for none.
    pub fn extension_applied(&self, name: &str) -> u64 {
        self.ext_held.get(name).copied().unwrap_or(0)
    }

    /// The spec revision of a mark, drawing or menu item the agent applied
    /// in this mission generation, as far as its answers show; 0 for none.
    pub fn marker_applied(&self, id: EntityId) -> u64 {
        self.pins.held.get(&id).copied().unwrap_or(0)
    }

    /// The error the agent last gave for a marker's current revision, if it
    /// refused it.
    pub fn marker_refused(&self, id: EntityId, store: &Store) -> bool {
        let gen_ = store.loaded().map_or(0, |l| l.mission_gen);
        store
            .marker(id)
            .is_some_and(|m| self.pins.refused.get(&id) == Some(&(m.spec_rev, gen_)))
    }

    /// Send what the store's removed extensions, marks, drawings and menu
    /// items need. Returns false when a send failed.
    pub(super) fn pins_plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) -> bool {
        let mut budget = PINS_PER_STEP;
        let names: Vec<String> = store.extension_removals().map(str::to_owned).collect();
        for name in names {
            if budget == 0 {
                return true;
            }
            if self.pins.ext_removing.values().any(|n| *n == name)
                || self.is_held(self.pins.ext_not_ready.get(&name), 0)
            {
                continue;
            }
            let key = (self.epoch, self.next_seq);
            if !self.send(
                store,
                sim,
                EntityId(0),
                0,
                Body::ExtensionRemove { name: name.clone() },
            ) {
                return false;
            }
            self.pins.ext_removing.insert(key, name);
            budget -= 1;
        }
        let loaded = store.loaded().cloned().expect("ready");
        // Removals: the items under a submenu before the submenu. A live
        // item of a hidden layer that DCS holds is taken out too.
        let mut removals: Vec<(usize, EntityId)> = store
            .markers()
            .filter(|m| {
                (m.removed && !self.pins.gone.contains(&m.id))
                    || (!m.removed
                        && !store.marker_visible(m)
                        && (self.pins.held.contains_key(&m.id) || self.pins.unsure.contains(&m.id)))
            })
            .map(|m| (store.menu_depth(m.id), m.id))
            .collect();
        removals.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (_, id) in removals {
            if budget == 0 {
                return true;
            }
            let m = store.marker(id).expect("marker");
            let (op, body) = match m.kind() {
                MarkerKind::Mark => (opcode::MARK_REMOVE, Body::MarkRemove),
                MarkerKind::Drawing => (opcode::DRAWING_REMOVE, Body::DrawingRemove),
                MarkerKind::MenuItem => (opcode::MENU_REMOVE, Body::MenuRemove),
            };
            if self.inflight_for(store, id, &[op])
                || self.is_held(self.cmd_not_ready.get(&(id, HOLD_PIN_REMOVE)), 0)
            {
                continue;
            }
            if !self.send(store, sim, id, 0, body) {
                return false;
            }
            budget -= 1;
        }
        // Sets: marks and drawings together, ascending by draw rank (items
        // never sent last, by creation), then menu items, a parent before
        // its children. Each takes the next ID of the pool as it is sent, so
        // DCS IDs, which clients draw by, keep the rank order across a redraw.
        // Items of a hidden layer wait.
        let mut sets: Vec<(u8, usize, u64, EntityId)> = store
            .markers()
            .filter(|m| !m.removed && store.marker_visible(m))
            .map(|m| match m.kind() {
                MarkerKind::MenuItem => (1, store.menu_depth(m.id), 0, m.id),
                _ => (0, 0, if m.rank == 0 { u64::MAX } else { m.rank }, m.id),
            })
            .collect();
        sets.sort();
        for (_, _, _, id) in sets {
            if budget == 0 {
                return true;
            }
            let m = store.marker(id).expect("marker").clone();
            // The revision the set carries: a new one when the add takes one.
            let mut rev = m.spec_rev;
            // An item DCS holds at this revision but the store holds no ID for
            // was taken out of view (hidden) and is added again.
            if self.pins.held.get(&id).is_some_and(|h| {
                *h >= m.spec_rev && (m.dcs_id != 0 || m.kind() == MarkerKind::MenuItem)
            }) || self.pins.refused.get(&id) == Some(&(m.spec_rev, loaded.mission_gen))
                || self.is_held(self.cmd_not_ready.get(&(id, HOLD_PIN_SET)), m.spec_rev)
                || self.inflight_for(store, id, &SETS)
            {
                continue;
            }
            // An item that waits for its audience is added when it can be and
            // goes on top; the items above it are not held back, since its
            // group may never bind (spec/world.md, "Redraw order").
            if m.scope == scope::GROUP && !self.is_bound(EntityId(m.target)) {
                continue;
            }
            let body = match &m.spec {
                MarkerSpec::Mark {
                    theater,
                    at,
                    text,
                    read_only,
                } => {
                    if *theater != loaded.theater {
                        continue;
                    }
                    let Some((dcs_id, new_rev)) = self.add_id(store, id, loaded.mission_gen) else {
                        continue;
                    };
                    rev = new_rev;
                    Body::MarkSet {
                        scope: m.scope,
                        read_only: *read_only,
                        target: m.target,
                        x: at.x,
                        y: at.y,
                        z: at.z,
                        dcs_id,
                        text: text.clone(),
                    }
                }
                MarkerSpec::Drawing { shape, args } => {
                    // DCS holds the drawing under this ID with the same shape
                    // and audience: change its style in place.
                    let update = self.pins.held_spec.get(&id).and_then(|(d, sc, t, held)| {
                        let MarkerSpec::Drawing {
                            shape: s0,
                            args: a0,
                        } = held
                        else {
                            return None;
                        };
                        (*d == m.dcs_id && *sc == m.scope && *t == m.target && s0 == shape)
                            .then(|| drawing_style_change(*shape, a0, args))
                            .flatten()
                    });
                    match update {
                        Some(changes) => Body::DrawingUpdate {
                            dcs_id: m.dcs_id,
                            changes,
                        },
                        None => {
                            let Some((dcs_id, new_rev)) =
                                self.add_id(store, id, loaded.mission_gen)
                            else {
                                continue;
                            };
                            rev = new_rev;
                            Body::DrawingSet {
                                scope: m.scope,
                                shape: *shape,
                                target: m.target,
                                dcs_id,
                                args: args.clone(),
                            }
                        }
                    }
                }
                MarkerSpec::MenuItem {
                    parent,
                    submenu,
                    text,
                } => {
                    if parent.0 != 0 {
                        // The parent is applied, or its set is on the ring
                        // ahead of this one.
                        let p = store.marker(*parent).expect("parent");
                        let ahead = self.pins.held.get(parent).is_some_and(|h| *h >= p.spec_rev)
                            || self.inflight_for(store, *parent, &[opcode::MENU_SET]);
                        if !ahead {
                            continue;
                        }
                    }
                    Body::MenuSet {
                        parent: parent.0,
                        target: m.target,
                        scope: m.scope,
                        submenu: *submenu,
                        text: text.clone(),
                    }
                }
            };
            if !self.send(store, sim, id, rev, body) {
                return false;
            }
            budget -= 1;
        }
        true
    }

    /// A mark, drawing or menu item set or removed resolved.
    pub(super) fn pin_resolved(
        &mut self,
        store: &mut Store,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        let gen_now = c.agent_boot == self.agent_boot
            && store
                .loaded()
                .is_some_and(|l| l.agent_boot == c.agent_boot && l.mission_gen == c.mission_gen);
        if !gen_now {
            return;
        }
        let id = c.entity;
        let code = res.map(|r| u64::from(r.code));
        if SETS.contains(&c.opcode) {
            // An update in place DCS did not take: the drawing is set whole.
            if c.opcode == opcode::DRAWING_UPDATE
                && outcome != Outcome::Applied
                && !(outcome == Outcome::Rejected
                    && res.is_some_and(|r| {
                        rejected_for_now(r.code) || u64::from(r.code) == rc::NOT_READY
                    }))
            {
                self.pins.held.remove(&id);
                self.pins.held_spec.remove(&id);
                store.touch_marker(id);
                return;
            }
            match outcome {
                Outcome::Applied => {
                    let h = self.pins.held.entry(id).or_default();
                    *h = (*h).max(c.rev);
                    if self.pins.issued.get(&id).is_some_and(|(_, r)| *r <= c.rev) {
                        self.pins.issued.remove(&id);
                    }
                    self.pins.retried.remove(&id);
                    self.pins.refused.remove(&id);
                    // Shown again after a hide: a later removal is sent.
                    self.pins.gone.remove(&id);
                    self.pins.unsure.remove(&id);
                    match store.marker(id) {
                        Some(m) if m.spec_rev == c.rev && m.kind() == MarkerKind::Drawing => {
                            self.pins
                                .held_spec
                                .insert(id, (m.dcs_id, m.scope, m.target, m.spec.clone()));
                        }
                        _ => {
                            self.pins.held_spec.remove(&id);
                        }
                    }
                    // A watch shows the applied revision.
                    store.touch_marker(id);
                    // Items refused for want of this submenu are tried again.
                    let held = &self.pins.held;
                    self.pins.refused.retain(|k, _| {
                        store.marker(*k).is_none_or(|m| m.parent() != id) || held.contains_key(k)
                    });
                    // A submenu's replacement the shim could not rebuild in
                    // full names the items it dropped: they are sent again.
                    if c.opcode == opcode::MENU_SET {
                        for dropped in res.iter().flat_map(|r| lv::items(&r.value)) {
                            if let Lv::String(b) = dropped
                                && let Some(d) = std::str::from_utf8(b)
                                    .ok()
                                    .and_then(|t| t.parse::<u64>().ok())
                            {
                                self.pins.held.remove(&EntityId(d));
                            }
                        }
                    }
                }
                Outcome::Refused => {
                    // An add the shim refused as at or below its process
                    // mark: another campaign's adds moved it. Raise this
                    // campaign's mark past it and add again under a fresh ID,
                    // once.
                    let stale = matches!(c.opcode, opcode::MARK_SET | opcode::DRAWING_SET)
                        && code == Some(rc::DCS_REFUSED)
                        && res.is_some_and(|r| r.message.contains("at or below the process mark"));
                    if stale && self.pins.retried.insert(id, c.rev) != Some(c.rev) {
                        let refused_id = self.pins.issued.get(&id).map_or(0, |(d, _)| *d);
                        let mark = res
                            .and_then(|r| process_mark(&r.message))
                            .unwrap_or(refused_id);
                        store.raise_dcs_mark_past(mark.max(refused_id));
                        self.pins.issued.remove(&id);
                        store.touch_marker(id);
                    } else {
                        self.pins.refused.insert(id, (c.rev, c.mission_gen));
                    }
                }
                Outcome::Rejected if code == Some(rc::NOT_READY) => {
                    self.drop_issued(id, c);
                    self.hold_not_ready(id, HOLD_PIN_SET, c.rev);
                }
                Outcome::Rejected if !res.is_some_and(|r| rejected_for_now(r.code)) => {
                    self.pins.refused.insert(id, (c.rev, c.mission_gen));
                }
                _ => {
                    // Rejected for now: the shim never reached the add.
                    if outcome == Outcome::Rejected {
                        self.drop_issued(id, c);
                    }
                }
            }
            return;
        }
        // A removal: applied, or the shim held none, or refused for good.
        match outcome {
            Outcome::Applied | Outcome::Refused => {
                self.pins.gone.insert(id);
                self.pins.held.remove(&id);
                self.pins.held_spec.remove(&id);
                self.pins.unsure.remove(&id);
                store.touch_marker(id);
            }
            Outcome::Rejected if code == Some(rc::NOT_READY) => {
                self.hold_not_ready(id, HOLD_PIN_REMOVE, 0);
            }
            Outcome::Rejected if !res.is_some_and(|r| rejected_for_now(r.code)) => {
                self.pins.gone.insert(id);
            }
            _ => {}
        }
    }

    /// A `MARK_SET` or `DRAWING_SET` the agent rejected for now never reached
    /// the shim, and a later add may have moved its process mark past the ID:
    /// the next add takes a fresh ID. An add whose outcome is unknown keeps
    /// its ID and is sent again under it.
    fn drop_issued(&mut self, id: EntityId, c: &Journalled) {
        if matches!(c.opcode, opcode::MARK_SET | opcode::DRAWING_SET)
            && self.pins.issued.get(&id).is_some_and(|(_, r)| *r == c.rev)
        {
            self.pins.issued.remove(&id);
        }
    }

    /// An `EXTENSION_REMOVE` resolved. Applied, refused or rejected for
    /// good, the agent holds the extension no more or will not drop it;
    /// anything else derives the removal again.
    pub(super) fn extension_removed(
        &mut self,
        store: &mut Store,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        let Some(name) = self.pins.ext_removing.remove(&(c.epoch, c.seq)) else {
            return;
        };
        if c.agent_boot != self.agent_boot {
            return;
        }
        let code = res.map(|r| u64::from(r.code));
        match outcome {
            Outcome::Applied | Outcome::Refused => {
                self.ext_held.remove(&name);
                self.ext_refused.remove(&name);
                store.extension_gone(&name);
            }
            Outcome::Rejected if code == Some(rc::NOT_READY) => {
                let hold = self.not_ready_now(0);
                self.pins.ext_not_ready.insert(name, hold);
            }
            Outcome::Rejected if !res.is_some_and(|r| rejected_for_now(r.code)) => {
                store.extension_gone(&name);
            }
            _ => {}
        }
    }

    /// A group was spawned again in this generation: DCS dropped the marks
    /// and menu items of its old ID, so the agent applies the group's
    /// markers again, at a new revision.
    pub(super) fn pins_group_respawned(&mut self, store: &mut Store, group: EntityId) {
        let again: Vec<EntityId> = store
            .markers()
            .filter(|m| {
                !m.removed
                    && m.scope == scope::GROUP
                    && m.target == group.0
                    && self.pins.held.contains_key(&m.id)
            })
            .map(|m| m.id)
            .collect();
        for id in again {
            store.bump_marker(id);
        }
    }

    /// Text on screen: a `MESSAGE` body, an effect, never sent again. A
    /// `GROUP` or `UNIT` scope needs DCS to hold an object of the target
    /// (None otherwise), as does any call when nothing can be sent now.
    pub fn message(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        body: Body,
    ) -> Option<(u64, u64)> {
        let Body::Message { scope, target, .. } = &body else {
            return None;
        };
        if !self.effect_ready(store) || !self.scope_bound(*scope, *target) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Remove a mark or drawing the platform does not hold, by its DCS ID:
    /// a `MARK_CLEAR`, an effect, never sent again. None when nothing can be
    /// sent now.
    pub fn mark_clear(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        dcs_id: u64,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, Body::MarkClear { dcs_id })
            .then_some(key)
    }

    /// A chat message from the server: a `CHAT`, an effect. `player` is a DCS
    /// connection ID, 0 for everyone.
    pub fn chat(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        player: u32,
        text: &str,
    ) -> Option<(u64, u64)> {
        if !self.server_ready() {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let body = Body::Chat {
            player,
            text: text.to_owned(),
        };
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Run Lua source in the `server` state: an `EVAL`, an effect. The
    /// result's value is the first value the chunk returns.
    pub fn eval(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        source: &str,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let body = Body::Eval {
            source: Lv::String(source.as_bytes().to_vec()),
        };
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Load a mission file: a `MISSION_LOAD`, an effect, never sent again.
    /// It runs with no mission loaded, so it needs the agent attached and
    /// the campaign announced, not a running mission. No API calls it yet:
    /// step S19 owns the reload.
    pub fn mission_load(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        path: &str,
    ) -> Option<(u64, u64)> {
        if !self.server_ready() {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let body = Body::MissionLoad {
            path: path.to_owned(),
        };
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }
}

/// The target period of the positions of the units players occupy, seconds.
pub const SUBSCRIBE_PERIOD_S: f64 = 1.0;

impl Reconciler {
    /// The units the players occupy: the agent samples their positions at a
    /// higher rate (`SUBSCRIBE`), written when the set differs from the one
    /// the agent holds.
    pub fn set_subscribed(&mut self, units: BTreeSet<EntityId>) {
        self.subscribe_want = units;
    }

    /// Write `SUBSCRIBE` when the wanted set differs from the sent one.
    /// Returns false when a send failed.
    pub(super) fn subscribe_plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) -> bool {
        let sent = self.subscribe_sent.clone().unwrap_or_default();
        if self.subscribe_want == sent {
            return true;
        }
        let body = Body::Subscribe(nanoprobe_transport::records::Subscribe {
            period_s: SUBSCRIBE_PERIOD_S,
            attitude: 0,
            entity: self.subscribe_want.iter().map(|e| e.0).collect(),
        });
        if !self.send(store, sim, EntityId(0), 0, body) {
            return false;
        }
        self.subscribe_sent = Some(self.subscribe_want.clone());
        true
    }

    /// A moderation call: a `MODERATE`, an effect, never sent again. `ucid`
    /// goes to the agent and no further.
    pub fn moderate(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        body: Body,
    ) -> Option<(u64, u64)> {
        if !self.server_ready() {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, body).then_some(key)
    }

    /// Ask the hook for the connected players: a `PLAYERS` query.
    pub fn query_players(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
    ) -> Option<(u64, u64)> {
        if !self.server_ready() {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, Body::Players)
            .then_some(key)
    }

    /// Ask the hook for the ban list: a `BANS` query.
    pub fn query_bans(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
    ) -> Option<(u64, u64)> {
        if !self.server_ready() {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        self.send(store, sim, EntityId(0), 0, Body::Bans)
            .then_some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_mark_is_read_from_a_shim_refusal() {
        let m = "id 1073741830 at or below the process mark 1073741900";
        assert_eq!(process_mark(m), Some(1_073_741_900));
        assert_eq!(process_mark("id 5 is held"), None);
    }

    fn point(x: f64) -> Lv {
        lv::obj(vec![("x", lv::n(x)), ("y", lv::n(0.0)), ("z", lv::n(0.0))])
    }

    fn rgba(r: f64) -> Lv {
        lv::list(vec![lv::n(r), lv::n(0.0), lv::n(0.0), lv::n(1.0)])
    }

    /// A two-point shape's arguments: color, fill, line type, read only, message.
    fn two_point(x: f64, color: f64, fill: f64, read_only: bool, msg: &str) -> Lv {
        lv::list(vec![
            point(x),
            point(x + 1.0),
            rgba(color),
            rgba(fill),
            lv::n(1.0),
            Lv::Bool(read_only),
            Lv::String(msg.as_bytes().to_vec()),
        ])
    }

    fn text(color: f64, font: f64, msg: &str) -> Lv {
        lv::list(vec![
            point(0.0),
            rgba(color),
            rgba(0.0),
            lv::n(font),
            Lv::Bool(false),
            Lv::String(msg.as_bytes().to_vec()),
        ])
    }

    fn keys(changes: &Lv) -> Vec<String> {
        match changes {
            Lv::Table(p) => p
                .iter()
                .map(|(k, _)| match k {
                    Lv::String(b) => String::from_utf8(b.clone()).unwrap(),
                    _ => panic!("key"),
                })
                .collect(),
            _ => panic!("table"),
        }
    }

    #[test]
    fn a_color_change_is_in_place() {
        let c = drawing_style_change(
            3,
            &two_point(0.0, 1.0, 0.0, true, ""),
            &two_point(0.0, 0.5, 0.0, true, ""),
        );
        let c = c.expect("in place");
        assert_eq!(keys(&c), ["color"]);
        assert_eq!(lv::get(&c, "color"), Some(&rgba(0.5)));
    }

    #[test]
    fn a_fill_change_is_in_place() {
        let c = drawing_style_change(
            3,
            &two_point(0.0, 1.0, 0.0, true, ""),
            &two_point(0.0, 1.0, 0.5, true, ""),
        );
        let c = c.expect("in place");
        assert_eq!(keys(&c), ["fill"]);
        assert_eq!(lv::get(&c, "fill"), Some(&rgba(0.5)));
    }

    #[test]
    fn a_text_shapes_font_size_and_text_are_in_place() {
        let c = drawing_style_change(5, &text(1.0, 12.0, "a"), &text(1.0, 20.0, "b"))
            .expect("in place");
        assert_eq!(keys(&c), ["font_size", "text"]);
        assert_eq!(lv::get(&c, "font_size"), Some(&lv::n(20.0)));
    }

    #[test]
    fn a_changed_message_read_only_or_point_is_a_full_set() {
        let base = two_point(0.0, 1.0, 0.0, true, "m");
        assert!(drawing_style_change(3, &base, &two_point(0.0, 1.0, 0.0, true, "n")).is_none());
        assert!(drawing_style_change(3, &base, &two_point(0.0, 1.0, 0.0, false, "m")).is_none());
        assert!(drawing_style_change(3, &base, &two_point(5.0, 1.0, 0.0, true, "m")).is_none());
        assert!(drawing_style_change(3, &base, &base).is_none(), "no change");
    }

    #[test]
    fn a_polygon_of_any_point_count_is_in_place() {
        let tri = |c: f64| {
            let mut v = lv::items(&two_point(0.0, 1.0, c, true, ""))
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            v.insert(2, point(9.0));
            lv::list(v)
        };
        assert!(drawing_style_change(3, &tri(0.0), &tri(2.0)).is_some());
    }

    #[test]
    fn a_changed_point_count_is_a_full_set() {
        let mut more = lv::items(&two_point(0.0, 1.0, 0.0, true, ""))
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        more.insert(2, point(9.0));
        assert!(
            drawing_style_change(3, &two_point(0.0, 1.0, 0.0, true, ""), &lv::list(more)).is_none()
        );
    }

    #[test]
    fn arguments_without_the_full_style_tail_are_a_full_set() {
        // A line without its message: n is 6, so a fixed tail would take
        // the second point for the color.
        let short = |x: f64| {
            lv::list(vec![
                point(0.0),
                point(x),
                rgba(1.0),
                rgba(1.0),
                lv::n(1.0),
                Lv::Bool(true),
            ])
        };
        assert!(drawing_style_change(1, &short(1.0), &short(2.0)).is_none());
        // A circle without its message: the radius point sits at n - 5.
        let circle = |x: f64| {
            lv::list(vec![
                point(0.0),
                point(x),
                rgba(1.0),
                rgba(1.0),
                lv::n(1.0),
                Lv::Bool(true),
            ])
        };
        assert!(drawing_style_change(2, &circle(1.0), &circle(2.0)).is_none());
        // The right count, a wrong type in the tail.
        let mut bad = lv::items(&two_point(0.0, 1.0, 0.0, true, ""))
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        bad[5] = Lv::String(b"x".to_vec());
        assert!(
            drawing_style_change(3, &lv::list(bad), &two_point(0.0, 0.5, 0.0, true, "")).is_none()
        );
        // An unknown shape.
        let r = two_point(0.0, 1.0, 0.0, true, "");
        assert!(drawing_style_change(99, &r, &two_point(0.0, 0.5, 0.0, true, "")).is_none());
    }
}
