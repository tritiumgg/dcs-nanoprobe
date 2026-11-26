//! Ruins: a static in state `dead` stays in DCS as a ruin (spec/world.md,
//! "Ruins").
//!
//! `dead` is final, so the lifecycle table has no part in this. The plan
//! keeps DCS holding the static's dead object wherever it would hold a live
//! static: under DCS authority, on the loaded theater, not removed. In the
//! generation the static died in, that object is the static itself, and
//! nothing is sent. In any other generation the reconciler spawns it with
//! `dead = true` in the table (`STATIC_SPAWN`). Under campaign authority, on
//! another theater, or removed, it destroys the object (`STATIC_DESTROY`).
//!
//! The agent has no row for a dead static: its `isExist` reads false (LIVE,
//! docs/results/2026-11-17-person-fixes.md), so no `OBJECT` reports it and
//! no enumeration finds it. The store keeps what the reconciler knows
//! instead: the generation a ruin spawn was written in, and the generation
//! DCS holds the dead object in. Both are written with the step, before its
//! commands reach DCS, so a restarted sidecar neither spawns a ruin twice nor
//! leaves one behind.

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result as rc;
use nanoprobe_transport::records::CommandResult;

use super::{HOLD_SPAWN, Reconciler, rejected_for_now};
use crate::ids::{self, EntityId, NameKind};
use crate::lifecycle::State;
use crate::sim::SimAdapter;
use crate::sim::wire::{Body, lv, opcode};
use crate::store::{Authority, Group, Journalled, Kind, Loaded, Outcome, Store};

/// The key of a static's table that spawns it destroyed.
pub(super) const DEAD_KEY: &str = "dead";

/// Whether a `STATIC_SPAWN` table spawns the static destroyed.
pub(super) fn spawns_dead(table: &Lv) -> bool {
    matches!(lv::get(table, DEAD_KEY), Some(Lv::Bool(true)))
}

/// An author's further keys without `dead`: only the platform says whether a
/// static spawns destroyed, from its state in the store.
pub(super) fn without_dead(extra: &Lv) -> Lv {
    match extra {
        Lv::Table(pairs) => Lv::Table(
            pairs
                .iter()
                .filter(|(k, _)| !matches!(k, Lv::String(b) if b == DEAD_KEY.as_bytes()))
                .cloned()
                .collect(),
        ),
        other => other.clone(),
    }
}

impl Reconciler {
    /// Whether a group is a dead static whose ruin DCS should hold and does
    /// not yet, in the loaded generation.
    pub(super) fn ruin_owed(&self, g: &Group, loaded: &Loaded) -> bool {
        g.kind == Kind::Static
            && g.state == State::Dead
            && !g.removed
            && g.authority == Authority::Sim
            && g.theater == loaded.theater
            && g.ruin_in != Some((loaded.agent_boot, loaded.mission_gen))
            && self.spawn_rejected.get(&g.id) != Some(&(g.spec_rev, loaded.mission_gen))
    }

    /// The plan for one static in state `dead`. `has_objects` says the
    /// binding mirror holds an object of it; `budget` says a spawn may be
    /// written in this step. Returns whether it wrote a spawn.
    pub(super) fn ruin_plan(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        id: EntityId,
        loaded: &Loaded,
        has_objects: bool,
        budget: bool,
    ) -> bool {
        let g = store.group(id).expect("group");
        let now = (loaded.agent_boot, loaded.mission_gen);
        let want = !g.removed && g.authority == Authority::Sim && g.theater == loaded.theater;
        let held = g.ruin_in == Some(now);
        let may_hold = held || g.ruin_sent_in == Some(now);
        let spec_rev = g.spec_rev;
        if has_objects && self.live_object_bound(id) {
            // Drift: the mirror holds an object of a dead static, and the
            // agent reports only objects that exist. A script revived it, or
            // a spawn of the live static was answered after its death. It is
            // destroyed; the ruin follows once the destroy applies.
            if self.destroy(store, sim, id) {
                self.counters.ruins_revived += 1;
            }
            return false;
        }
        if !want {
            if may_hold {
                self.destroy(store, sim, id);
            }
            return false;
        }
        if held || !budget {
            return false;
        }
        let blocked = self.spawn_rejected.get(&id) == Some(&(spec_rev, loaded.mission_gen))
            || self.is_held(self.cmd_not_ready.get(&(id, HOLD_SPAWN)), spec_rev);
        if blocked || self.inflight_for(store, id, &[opcode::STATIC_SPAWN, opcode::STATIC_DESTROY])
        {
            return false;
        }
        self.spawn_ruin(store, sim, id, now)
    }

    /// Whether the mirror binds an object of the static other than the ruin
    /// the reconciler spawned. The ruin's own ID is exempt so that a DCS
    /// that reported a ruin as existing could not make the plan destroy and
    /// spawn it in a loop.
    fn live_object_bound(&self, id: EntityId) -> bool {
        let ruin = self.ruin_object.get(&id).copied();
        self.binding
            .iter()
            .any(|(object, row)| row.group == id && Some(*object) != ruin)
    }

    /// Write the `STATIC_SPAWN` of a ruin: the static's table with
    /// `dead = true`, at its spec revision. The agent answers `AT_REVISION`
    /// when it applied that revision in this generation, so the object of a
    /// static that died here is never replaced.
    fn spawn_ruin(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        id: EntityId,
        now: (u64, u32),
    ) -> bool {
        let g = store.group(id).expect("group").clone();
        let u = store.unit(g.members[0]).expect("unit");
        let name = ids::name(NameKind::Static, store.campaign(), id);
        let table = super::static_spawn_table(store.campaign(), &g, u, true);
        // Before the write: DCS may hold the ruin from here on, and a
        // sidecar that restarts must know to destroy it.
        store.ruin_sent(id, now.0, now.1).expect("group");
        self.send(
            store,
            sim,
            id,
            g.spec_rev,
            Body::StaticSpawn {
                country: g.country,
                table,
                name,
            },
        )
    }

    /// The spawn of a ruin resolved, in the loaded generation.
    pub(super) fn ruin_spawn_resolved(
        &mut self,
        store: &mut Store,
        c: &Journalled,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) {
        let id = c.entity;
        match outcome {
            Outcome::Applied => {
                store
                    .ruin_held(id, c.agent_boot, c.mission_gen)
                    .expect("group");
                // An error kept from a refused spawn no longer holds.
                store.clear_error(id).expect("group");
                // Only an `APPLIED` result names the ruin's object: the
                // agent answers `AT_REVISION` before a handler runs, and an
                // object it named there could be a revived live one, which
                // the drift destroy must not exempt.
                if let Some(r) = res
                    && u64::from(r.code) == rc::APPLIED
                {
                    if r.object != 0 {
                        self.ruin_object.insert(id, r.object);
                    }
                    self.counters.ruins_spawned += 1;
                }
            }
            Outcome::Refused => {
                // Not retried at this revision in this generation.
                self.spawn_rejected.insert(id, (c.rev, c.mission_gen));
                self.counters.ruins_refused += 1;
                let msg = res.map(|r| r.message.clone()).unwrap_or_default();
                store.set_error(id, msg).expect("group");
            }
            Outcome::Rejected => {
                if let Some(r) = res {
                    if u64::from(r.code) == rc::NOT_READY {
                        self.hold_not_ready(id, HOLD_SPAWN, c.rev);
                    } else if !rejected_for_now(r.code) {
                        // A rejection a resend cannot fix, such as a table
                        // too large: kept and shown like a refusal.
                        self.spawn_rejected.insert(id, (c.rev, c.mission_gen));
                        self.counters.ruins_refused += 1;
                        let msg = if r.message.is_empty() {
                            format!("the agent rejected the ruin's spawn with code {}", r.code)
                        } else {
                            r.message.clone()
                        };
                        store.set_error(id, msg).expect("group");
                    }
                }
            }
            // Sent again by the next plan: a spawn that ran is answered
            // `AT_REVISION`.
            Outcome::Unknown | Outcome::NotSent => {}
        }
    }

    /// In the loaded generation a static's last death fact arrived, or the
    /// object the store knows of a static already `dead` died: DCS holds its
    /// dead object there, and no ruin is spawned over it.
    pub(super) fn ruin_died(&mut self, store: &mut Store, id: EntityId) {
        let Some(l) = store.loaded().cloned() else {
            return;
        };
        if store
            .group(id)
            .is_some_and(|g| g.kind == Kind::Static && g.state == State::Dead)
        {
            store
                .ruin_held(id, l.agent_boot, l.mission_gen)
                .expect("group");
        }
    }

    /// A destroy of the entity applied: DCS holds no object of it.
    pub(super) fn ruin_destroyed(&mut self, store: &mut Store, id: EntityId) {
        self.ruin_object.remove(&id);
        store.ruin_gone(id).expect("group");
    }

    /// A destroy of the entity was taken and its result lost: it may have
    /// run. The store no longer says DCS holds the ruin, only that it may.
    /// So a static that should have its ruin is spawned again, answered
    /// `AT_REVISION` if the destroy did not run, and one that should not is
    /// destroyed again.
    pub(super) fn ruin_destroy_unknown(&mut self, store: &mut Store, id: EntityId) {
        self.ruin_object.remove(&id);
        store.ruin_unsure(id).expect("group");
    }
}

#[cfg(test)]
mod tests {
    use super::super::with_extra;
    use super::*;

    #[test]
    fn only_a_true_dead_key_spawns_dead_and_an_authors_key_is_dropped() {
        let table = |dead: Lv| lv::obj(vec![("name", lv::s("np#jqW1C")), (DEAD_KEY, dead)]);
        assert!(spawns_dead(&table(Lv::Bool(true))));
        assert!(!spawns_dead(&table(Lv::Bool(false))));
        assert!(!spawns_dead(&table(lv::s("true"))));
        assert!(!spawns_dead(&lv::obj(vec![("name", lv::s("np#jqW1C"))])));
        assert!(!spawns_dead(&Lv::Nil));

        let extra = lv::obj(vec![
            ("shape_name", lv::s("tent")),
            (DEAD_KEY, Lv::Bool(true)),
            ("hidden", Lv::Bool(false)),
        ]);
        assert_eq!(
            without_dead(&extra),
            lv::obj(vec![
                ("shape_name", lv::s("tent")),
                ("hidden", Lv::Bool(false))
            ])
        );
        assert_eq!(without_dead(&Lv::Nil), Lv::Nil);
        // The platform's key wins over an author's in a ruin's table.
        let ruin = with_extra(
            lv::obj(vec![(DEAD_KEY, Lv::Bool(true))]),
            &lv::obj(vec![(DEAD_KEY, Lv::Bool(false))]),
        );
        assert!(spawns_dead(&ruin));
    }
}
