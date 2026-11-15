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
use crate::store::{Journalled, MarkerKind, MarkerSpec, Outcome, Store, scope};

const HOLD_PIN_SET: u8 = 7;
const HOLD_PIN_REMOVE: u8 = 8;

/// The most marker and extension commands one step sends, so a campaign with
/// many items does not fill the ring in one frame.
pub const PINS_PER_STEP: usize = 32;

const SETS: [u16; 3] = [opcode::MARK_SET, opcode::DRAWING_SET, opcode::MENU_SET];

/// What the reconciler knows about the marks, drawings and menu items in DCS
/// and about extension removals in flight.
#[derive(Debug, Default)]
pub(super) struct PinState {
    /// The spec revision the agent holds, in this agent boot and generation.
    held: HashMap<EntityId, u64>,
    /// A revision the agent refused, with the generation: not sent again
    /// until either changes.
    refused: HashMap<EntityId, (u64, u32)>,
    /// Removed markers DCS holds no more in this generation, or that the
    /// agent refused to remove.
    gone: HashSet<EntityId>,
    /// An `EXTENSION_REMOVE` the agent answered `NOT_READY`.
    ext_not_ready: HashMap<String, NotReady>,
    /// The extension each `EXTENSION_REMOVE` in flight names.
    ext_removing: HashMap<(u64, u64), String>,
}

impl PinState {
    /// The agent holds none of them: a new agent boot, campaign or mission
    /// generation. A marker removed already is gone with the rest.
    pub(super) fn forget(&mut self, store: &Store) {
        self.held.clear();
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
        // Removals: the items under a submenu before the submenu.
        let mut removals: Vec<(usize, EntityId)> = store
            .markers()
            .filter(|m| m.removed && !self.pins.gone.contains(&m.id))
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
        // Sets: marks, then drawings, then menu items, a parent before its
        // children.
        let mut sets: Vec<(u8, usize, EntityId)> = store
            .markers()
            .filter(|m| !m.removed)
            .map(|m| {
                let k = match m.kind() {
                    MarkerKind::Mark => 0,
                    MarkerKind::Drawing => 1,
                    MarkerKind::MenuItem => 2,
                };
                (k, store.menu_depth(m.id), m.id)
            })
            .collect();
        sets.sort();
        for (_, _, id) in sets {
            if budget == 0 {
                return true;
            }
            let m = store.marker(id).expect("marker").clone();
            if self.pins.held.get(&id).is_some_and(|h| *h >= m.spec_rev)
                || self.pins.refused.get(&id) == Some(&(m.spec_rev, loaded.mission_gen))
                || self.is_held(self.cmd_not_ready.get(&(id, HOLD_PIN_SET)), m.spec_rev)
                || self.inflight_for(store, id, &SETS)
            {
                continue;
            }
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
                    Body::MarkSet {
                        scope: m.scope,
                        read_only: *read_only,
                        target: m.target,
                        x: at.x,
                        y: at.y,
                        z: at.z,
                        text: text.clone(),
                    }
                }
                MarkerSpec::Drawing { shape, args } => Body::DrawingSet {
                    scope: m.scope,
                    shape: *shape,
                    target: m.target,
                    args: args.clone(),
                },
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
            if !self.send(store, sim, id, m.spec_rev, body) {
                return false;
            }
            budget -= 1;
        }
        true
    }

    /// A mark, drawing or menu item set or removed resolved.
    pub(super) fn pin_resolved(
        &mut self,
        store: &Store,
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
            match outcome {
                Outcome::Applied => {
                    let h = self.pins.held.entry(id).or_default();
                    *h = (*h).max(c.rev);
                    self.pins.refused.remove(&id);
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
                    self.pins.refused.insert(id, (c.rev, c.mission_gen));
                }
                Outcome::Rejected if code == Some(rc::NOT_READY) => {
                    self.hold_not_ready(id, HOLD_PIN_SET, c.rev);
                }
                Outcome::Rejected if !res.is_some_and(|r| rejected_for_now(r.code)) => {
                    self.pins.refused.insert(id, (c.rev, c.mission_gen));
                }
                _ => {}
            }
            return;
        }
        // A removal: applied, or the shim held none, or refused for good.
        match outcome {
            Outcome::Applied | Outcome::Refused => {
                self.pins.gone.insert(id);
                self.pins.held.remove(&id);
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
