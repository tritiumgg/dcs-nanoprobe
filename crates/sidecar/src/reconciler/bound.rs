//! What keeps the sidecar from growing with a campaign's history
//! (spec/world.md, "Removal" and "Expiry").
//!
//! - The plan's work set: the groups a plan may have to act on. A group
//!   whose state needs nothing (removed, dead, unspawnable, a virtual group
//!   the campaign simulates) leaves it, and comes back with any object of it
//!   the binding holds. The plan walks the work set, not every group kept.
//! - The purge: a removed entity's rows leave the store `removed_keep_h`
//!   after the removal, once DCS holds nothing of it.
//! - The expiry: a dead group (not a static) is removed `dead_keep_h` after
//!   it died, with no command to DCS.
//! - Purged names: an object in DCS whose name is this campaign's and names
//!   an entity the store no longer has is destroyed as drift.

use std::collections::{BTreeSet, HashSet};

use super::{Reconciler, unbind_group};
use crate::ids::{self, EntityId, NameKind};
use crate::lifecycle::State;
use crate::sim::SimAdapter;
use crate::sim::wire::{Body, Object, opcode};
use crate::store::{Authority, Group, Journalled, Kind, Loaded, LostPolicy, Outcome, Store};

/// Whether DCS may hold a dead static's object in the loaded generation:
/// the static died there, or a ruin was spawned there and no destroy of it
/// applied since (`ruins.rs`).
fn ruin_may_be_held(g: &Group, l: &Loaded) -> bool {
    let now = Some((l.agent_boot, l.mission_gen));
    g.ruin_in == now || g.ruin_sent_in == now
}

/// How often the purge and the expiry look at the store, in the step's
/// wall-clock milliseconds.
const PASS_EVERY_MS: u64 = 1_000;

/// The most removed entities one pass purges, so a backlog (a store opened
/// after months) leaves over several steps, each with its own commit.
pub const PURGE_BATCH: usize = 1_000;

/// An object in DCS of an entity the store no longer has: what destroys it.
#[derive(Clone, Debug, Default)]
pub(super) struct Orphan {
    /// Whether the entity is a static: `STATIC_DESTROY`, else `GROUP_DESTROY`.
    pub(super) is_static: bool,
    /// The DCS object IDs seen.
    pub(super) objects: BTreeSet<u64>,
    /// Destroys refused or unknown, up to `destroy_retries`.
    pub(super) failures: u32,
}

impl Reconciler {
    /// Whether the plan has anything to do for a group with no object bound,
    /// in the loaded mission: the work set's rule. A dead static not removed
    /// keeps its ruin in DCS in every generation, so it stays; a removed one
    /// stays while DCS may hold its ruin in this generation.
    pub(super) fn needs_plan(&self, g: &Group, l: &Loaded) -> bool {
        match g.state {
            State::Dead => g.kind == Kind::Static && (!g.removed || ruin_may_be_held(g, l)),
            State::Unspawnable => false,
            State::Virtual => g.authority == Authority::Sim && g.theater == l.theater,
            State::Lost => g.on_lost.unwrap_or(self.cfg.on_lost) != LostPolicy::Report,
            State::Materializing | State::Live | State::Dematerializing => true,
        }
    }

    /// Bring the work set up to date from the groups the store changed, and
    /// return the groups this plan walks: the work set and every group with
    /// an object bound, in `EntityId` order.
    pub(super) fn work_walk(&mut self, store: &mut Store, bound: &[EntityId]) -> Vec<EntityId> {
        let loaded = store.loaded().cloned().expect("ready");
        let key = (
            store.campaign().0,
            loaded.agent_boot,
            loaded.mission_gen,
            loaded.theater.clone(),
        );
        let (touched, all) = store.take_touched();
        if all || self.work_key.as_ref() != Some(&key) {
            self.work = store
                .groups()
                .filter(|g| self.needs_plan(g, &loaded))
                .map(|g| g.id)
                .collect();
            self.work_key = Some(key);
        } else {
            for id in touched {
                match store.group(id) {
                    Some(g) if self.needs_plan(g, &loaded) => {
                        self.work.insert(id);
                    }
                    _ => {
                        self.work.remove(&id);
                    }
                }
            }
        }
        let mut walk: BTreeSet<EntityId> = self.work.clone();
        walk.extend(
            bound
                .iter()
                .copied()
                .filter(|id| store.group(*id).is_some()),
        );
        if cfg!(debug_assertions) {
            // The full pass: the work set is exactly the groups that need the
            // plan.
            let full: BTreeSet<EntityId> = store
                .groups()
                .filter(|g| self.needs_plan(g, &loaded))
                .map(|g| g.id)
                .collect();
            assert_eq!(
                self.work, full,
                "the plan's work set differs from the full pass"
            );
        }
        walk.into_iter().collect()
    }

    /// The groups the work set holds now, for tests and the status.
    pub fn work_len(&self) -> usize {
        self.work.len()
    }

    /// Expire dead groups and purge removed entities whose time has come,
    /// once a second of the step's clock. Bounded per pass.
    pub(super) fn bound_pass(&mut self, store: &mut Store) {
        let now = store.now_ms();
        // A clock set back more than a pass re-arms the gate at once, so a
        // forward jump undone does not stop the purge until it catches up.
        if now < self.next_bound_ms && self.next_bound_ms - now <= PASS_EVERY_MS {
            return;
        }
        self.next_bound_ms = now.saturating_add(PASS_EVERY_MS);
        if self.cfg.dead_keep_ms > 0 {
            // At most a batch a pass, as the purge: a suspended campaign
            // activated after days expires its backlog over several steps.
            let dead: Vec<EntityId> = store.dead_ids().collect();
            let mut expired = 0;
            for id in dead {
                if expired >= PURGE_BATCH {
                    break;
                }
                let at = store.stamp_dead(id).unwrap_or(now);
                // A group with a held member is `lost`, not `dead`, so it is
                // not here.
                if now.saturating_sub(at) >= self.cfg.dead_keep_ms
                    && store.expire(id).expect("group")
                {
                    expired += 1;
                }
            }
        }
        // What DCS may still hold: bound objects, commands in flight.
        let bound: HashSet<EntityId> = self
            .binding
            .values()
            .flat_map(|r| [r.group, r.unit])
            .collect();
        let removed: Vec<EntityId> = store.removed_ids().collect();
        let mut purged = 0;
        for id in removed {
            if purged >= PURGE_BATCH {
                break;
            }
            let at = store.stamp_removed(id).unwrap_or(now);
            if now.saturating_sub(at) < self.cfg.removed_keep_ms {
                continue;
            }
            if self.dcs_may_hold(store, id, at, &bound) {
                continue;
            }
            self.purge(store, id);
            purged += 1;
        }
        // Removed marks, drawings and menu items DCS holds no more.
        let markers: Vec<EntityId> = store
            .markers()
            .filter(|m| {
                m.removed
                    && m.removed_at_ms
                        .is_some_and(|t| now.saturating_sub(t) >= self.cfg.removed_keep_ms)
            })
            .map(|m| m.id)
            .collect();
        for id in markers {
            if self.pins_gone(id) && self.inflight.of(id).next().is_none() {
                // A marker's commands are of the loaded generation: a new
                // one forgets every marker in DCS.
                store.purge_marker(id);
                self.pins_purged(id);
            }
        }
    }

    /// Whether DCS may still hold something of a removed entity: an object
    /// bound, a command naming it unanswered, its ruin, or no full
    /// enumeration used since the removal (`at`). A dead static's ruin is
    /// never in an enumeration, so its purge rests on the destroy the plan
    /// sent for it having applied, which clears the ruin.
    fn dcs_may_hold(
        &self,
        store: &Store,
        id: EntityId,
        at: u64,
        bound: &HashSet<EntityId>,
    ) -> bool {
        let Some(g) = store.group(id) else {
            return false;
        };
        if bound.contains(&id) || g.members.iter().any(|m| bound.contains(m)) {
            return true;
        }
        // A command of an earlier mission generation reached no object DCS
        // holds now.
        let Some(l) = store.loaded() else {
            return true;
        };
        let here = |c: &Journalled| c.agent_boot == l.agent_boot && c.mission_gen == l.mission_gen;
        if self.inflight.of(id).any(here)
            || g.members.iter().any(|m| self.inflight.of(*m).any(here))
        {
            return true;
        }
        if self.orphans.contains_key(&id) {
            return true;
        }
        if g.kind == Kind::Static && g.state == State::Dead {
            return ruin_may_be_held(g, l);
        }
        !self.enum_used_from_ms.is_some_and(|began| began > at)
    }

    /// Purge one removed entity: its rows and what the reconciler keeps of
    /// it.
    fn purge(&mut self, store: &mut Store, id: EntityId) {
        let Some(members) = store.purge(id) else {
            return;
        };
        self.work.remove(&id);
        self.held.remove(&id);
        self.spawn_applied.remove(&id);
        self.spawn_rejected.remove(&id);
        self.order_held.remove(&id);
        self.order_refused.remove(&id);
        self.ruin_object.remove(&id);
        self.drift_destroy_failures.remove(&id);
        self.forget_options(id);
        unbind_group(&mut self.binding, id);
        self.cmd_not_ready
            .retain(|(e, _), _| *e != id && !members.contains(e));
        for m in &members {
            self.life_held.remove(m);
        }
    }

    /// An object whose name is this campaign's and names an entity the
    /// store does not hold, below the next `EntityId`: an entity that was
    /// purged, or made after the backup a restored store came from. It is
    /// kept for a drift destroy. Only a platform name counts, so an adopted
    /// unit's ID (which no name carries) never makes one. Returns whether it
    /// was one.
    pub(super) fn orphan_seen(&mut self, store: &Store, o: &Object) -> bool {
        let campaign = store.campaign();
        let Some(n) = ids::parse(&o.name).filter(|n| ids::of_campaign(n, campaign)) else {
            return false;
        };
        if n.entity.0 >= store.next_entity() || store.unit(n.entity).is_some() {
            return false;
        }
        let target = match n.kind {
            NameKind::Static => Some((n.entity, true)),
            NameKind::Unit => ids::parse(&o.group_name)
                .filter(|g| ids::of_campaign(g, campaign) && g.kind == NameKind::Group)
                .filter(|g| g.entity.0 < store.next_entity() && store.group(g.entity).is_none())
                .map(|g| (g.entity, false)),
            _ => None,
        };
        let Some((entity, is_static)) = target else {
            return false;
        };
        let e = self.orphans.entry(entity).or_insert_with(|| Orphan {
            is_static,
            ..Orphan::default()
        });
        e.objects.insert(o.object);
        true
    }

    /// An object left DCS: an orphan's object is gone.
    pub(super) fn orphan_gone(&mut self, object: u64) {
        self.orphans.retain(|_, o| {
            o.objects.remove(&object);
            !o.objects.is_empty()
        });
    }

    /// Send the drift destroys the orphans need. Returns false when a send
    /// failed.
    pub(super) fn orphans_plan(&mut self, store: &mut Store, sim: &mut dyn SimAdapter) -> bool {
        let limit = self.cfg.destroy_retries.max(1);
        let due: Vec<(EntityId, bool)> = self
            .orphans
            .iter()
            .filter(|(_, o)| o.failures < limit)
            .map(|(id, o)| (*id, o.is_static))
            .collect();
        for (id, is_static) in due {
            let ops = [opcode::GROUP_DESTROY, opcode::STATIC_DESTROY];
            if self.inflight_for(store, id, &ops) {
                continue;
            }
            let body = if is_static {
                Body::StaticDestroy
            } else {
                Body::GroupDestroy
            };
            if !self.send(store, sim, id, 0, body) {
                return false;
            }
            self.counters.orphan_destroys += 1;
            store.count_orphan_destroy();
        }
        true
    }

    /// A destroy for an entity the store does not hold was answered.
    pub(super) fn orphan_resolved(&mut self, id: EntityId, outcome: Outcome, not_bound: bool) {
        if outcome == Outcome::Applied || not_bound {
            self.orphans.remove(&id);
        } else if matches!(outcome, Outcome::Refused | Outcome::Unknown)
            && let Some(o) = self.orphans.get_mut(&id)
        {
            o.failures += 1;
        }
    }

    /// The entities whose objects are kept for a drift destroy now.
    pub fn orphans(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.orphans.keys().copied()
    }
}
