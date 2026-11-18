//! Ruins (spec/world.md, "Ruins"), on the fake sim: a managed static that
//! dies stays `dead` in the store and stays in DCS as a ruin. It is spawned
//! dead again after a mission load, a DCS restart and a materialize, never
//! alive, and only a destroy the platform sends removes it.

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::sim::wire::{lv, opcode};
use nanoprobe_sidecar::store::{
    Authority, Entry, Kind, LostPolicy, NewGroup, NewUnit, Point, UnitState,
};
use nanoprobe_sim_fake::harness::{CAMPAIGN, Op, THEATER, World, new_store};
use nanoprobe_sim_fake::{Config, Left, Ruin};

fn world(statics: usize) -> World {
    let mut w = World::with_config(
        1,
        statics,
        LostPolicy::Report,
        Config {
            event_queue: 10_000,
            ..Config::default()
        },
    );
    ticks(&mut w, 40);
    w
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().unwrap();
    }
}

fn statics(w: &World) -> Vec<EntityId> {
    w.store
        .groups()
        .filter(|g| g.kind == Kind::Static)
        .map(|g| g.id)
        .collect()
}

fn state(w: &World, id: EntityId) -> State {
    w.store.group(id).expect("static").state
}

/// The static's live object in DCS.
fn object_of(w: &World, id: EntityId) -> Option<u64> {
    w.units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .map(|(o, _)| o)
}

/// The static's dead object in DCS.
fn ruin_of(w: &World, id: EntityId) -> Option<Ruin> {
    w.sim
        .ruins(CAMPAIGN)
        .into_iter()
        .find(|r| r.entity == id.0)
        .cloned()
}

/// Spawns of the live static that reached DCS.
fn live_spawns(w: &World, id: EntityId) -> usize {
    w.sim.ledger().spawns.iter().filter(|s| s.2 == id.0).count()
}

/// Spawns of the ruin that reached DCS.
fn ruin_spawns(w: &World, id: EntityId) -> usize {
    w.sim
        .ledger()
        .ruin_spawns
        .iter()
        .filter(|s| s.2 == id.0)
        .count()
}

/// Commands of an opcode journalled for the entity.
fn commands(w: &World, id: EntityId, op: u16) -> usize {
    w.store
        .entries()
        .iter()
        .filter(|e| matches!(e, Entry::Command(c) if c.entity == id && c.opcode == op))
        .count()
}

fn kill(w: &mut World, id: EntityId) {
    let o = object_of(w, id).expect("the static is in DCS");
    w.sim.kill(o);
    ticks(w, 5);
    assert_eq!(state(w, id), State::Dead);
}

fn a_tent(x: f64, type_name: &str) -> NewGroup {
    NewGroup {
        theater: THEATER.into(),
        country: 2,
        category: 0,
        coalition: 2,
        units: vec![NewUnit {
            extra: lv::obj(vec![("shape_name", lv::s("tent"))]),
            ..NewUnit::new(
                type_name,
                UnitState {
                    at: Point { x, y: 0.0, z: 70.0 },
                    heading: 1.25,
                    life: 0.0,
                },
            )
        }],
        authority: Authority::Sim,
        label: "ruin".into(),
        extra: lv::obj(vec![]),
    }
}

/// A static that dies is `dead`, is not marked as having left, and its
/// object stays in DCS with no command sent. After a mission load it is
/// spawned dead, at its place, and never alive.
#[test]
fn a_killed_static_stays_a_ruin_and_a_mission_load_restores_it_dead() {
    let mut w = world(2);
    let id = statics(&w)[0];
    let other = statics(&w)[1];
    kill(&mut w, id);
    let u = w.store.unit(id).expect("unit").clone();
    assert!(u.dead && !u.missing && !u.removed, "a death is no removal");
    assert!(
        !w.store
            .entries()
            .iter()
            .any(|e| matches!(e, Entry::Left { unit, .. } if *unit == id)),
        "a dead static was journalled as having left"
    );
    let corpse = ruin_of(&w, id).expect("the dead static stays in DCS");
    assert!(!corpse.spawned);
    ticks(&mut w, 20);
    // Counted as written: the agent would answer a spawn at the revision it
    // holds `AT_REVISION`, so the handler's count alone would hide one.
    assert_eq!(
        commands(&w, id, opcode::STATIC_SPAWN),
        1,
        "no spawn is written over it"
    );
    assert_eq!(ruin_spawns(&w, id), 0);
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 0);
    assert_eq!(live_spawns(&w, id), 1);

    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(state(&w, id), State::Dead);
    let ruin = ruin_of(&w, id).expect("the ruin is restored");
    assert!(ruin.spawned);
    assert_eq!(
        (ruin.type_name.as_str(), ruin.x, ruin.z, ruin.heading),
        ("Tent", u.state.at.x, u.state.at.z, u.state.heading)
    );
    assert_eq!(object_of(&w, id), None, "no live static of it");
    assert_eq!(live_spawns(&w, id), 1, "it is never spawned alive again");
    assert_eq!(ruin_spawns(&w, id), 1);
    // The other static was restored alive.
    assert_eq!(state(&w, other), State::Live);
    assert_eq!(live_spawns(&w, other), 2);
    // A second load restores it again; no step spawns it twice in one.
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(ruin_spawns(&w, id), 2);
    assert_eq!(live_spawns(&w, id), 1);
    w.settle().unwrap();
}

/// A sidecar restart sends nothing for a ruin DCS holds: not in the
/// generation the static died in, and not after a restore.
#[test]
fn a_sidecar_restart_neither_spawns_nor_destroys_a_ruin_dcs_holds() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 40);
    assert_eq!(
        commands(&w, id, opcode::STATIC_SPAWN),
        1,
        "only the live spawn"
    );
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 0);
    assert!(ruin_of(&w, id).is_some());

    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(ruin_spawns(&w, id), 1);
    let sent = commands(&w, id, opcode::STATIC_SPAWN);
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 40);
    assert_eq!(commands(&w, id, opcode::STATIC_SPAWN), sent);
    assert_eq!(ruin_spawns(&w, id), 1);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    w.settle().unwrap();
}

/// After DCS dies and starts again, the ruin is spawned dead in the new
/// process.
#[test]
fn a_dcs_restart_restores_the_ruin_dead() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.apply(&Op::Crash).unwrap();
    w.apply(&Op::RestartDcs).unwrap();
    ticks(&mut w, 80);
    assert_eq!(state(&w, id), State::Dead);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(object_of(&w, id), None);
    assert_eq!(live_spawns(&w, id), 1, "the one spawn before it died");
    // A sidecar restart on top changes nothing.
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 40);
    assert_eq!(ruin_spawns(&w, id), 1);
    w.settle().unwrap();
}

/// Under campaign authority DCS holds no object of a dead static; back
/// under DCS authority it is spawned dead. In the generation it died in,
/// the dematerialize destroys the static's own dead object.
#[test]
fn a_dematerialize_destroys_the_ruin_and_a_materialize_spawns_it_dead() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.store
        .set_authority(id, Authority::Campaign)
        .expect("static");
    ticks(&mut w, 20);
    assert_eq!(ruin_of(&w, id), None, "the dead static left DCS");
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1);
    assert_eq!(state(&w, id), State::Dead);
    ticks(&mut w, 20);
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1, "sent once");

    w.store.set_authority(id, Authority::Sim).expect("static");
    ticks(&mut w, 20);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(ruin_spawns(&w, id), 1);
    assert_eq!(live_spawns(&w, id), 1);

    // The same across a mission load made while it is virtual.
    w.store
        .set_authority(id, Authority::Campaign)
        .expect("static");
    ticks(&mut w, 20);
    assert_eq!(ruin_of(&w, id), None);
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(ruin_of(&w, id), None, "virtual: nothing is spawned");
    assert_eq!(ruin_spawns(&w, id), 1);
    w.store.set_authority(id, Authority::Sim).expect("static");
    ticks(&mut w, 20);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(ruin_spawns(&w, id), 2);
    w.settle().unwrap();
}

/// A mission on another theater holds no ruin; the next load on the
/// static's theater spawns it dead.
#[test]
fn a_ruin_follows_its_theater() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.apply(&Op::LoadOtherTheater).unwrap();
    ticks(&mut w, 60);
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!(ruin_spawns(&w, id), 0);
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(live_spawns(&w, id), 1);
    w.settle().unwrap();
}

/// Only the controller's removal takes a ruin out of DCS, with a
/// `STATIC_DESTROY`; a whole-map clear of debris sends none and leaves it.
#[test]
fn only_a_removal_destroys_a_ruin() {
    let mut w = world(2);
    let (id, restored) = (statics(&w)[0], statics(&w)[1]);
    kill(&mut w, id);
    kill(&mut w, restored);
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    w.rec
        .clear_debris_everywhere(&mut w.store, &mut w.sim)
        .expect("sent");
    ticks(&mut w, 400);
    // The reconciler's half of the rule: a clear makes it send no destroy.
    // That the agent's handler leaves a managed dead static is checked on
    // the mock (lua/tests/dispatch.lua, `clear-debris` and `world`); the
    // fake's clear touches no static.
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 0);
    assert_eq!(commands(&w, restored, opcode::STATIC_DESTROY), 0);
    assert!(ruin_of(&w, id).is_some() && ruin_of(&w, restored).is_some());

    w.store.remove_entity(id).expect("static");
    ticks(&mut w, 20);
    assert_eq!(ruin_of(&w, id), None, "the removal destroyed the ruin");
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1);
    assert!(ruin_of(&w, restored).is_some(), "the other ruin stays");
    // A removed ruin does not come back.
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(ruin_of(&w, id), None);
    assert!(ruin_of(&w, restored).is_some_and(|r| r.spawned));
    w.settle().unwrap();
}

/// A removal in the generation the static died in destroys its own dead
/// object, which no row in the agent's table reports.
#[test]
fn a_removal_destroys_the_object_of_a_static_that_just_died() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    assert!(w.rec.binding().values().all(|r| r.group != id));
    w.store.remove_entity(id).expect("static");
    ticks(&mut w, 20);
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1);
    w.settle().unwrap();
}

/// A death that arrives while a mission load is pending: the kill and the
/// load happen in the same frame, so the death's record comes before the
/// new generation's. The new mission gets the ruin, never the live static.
#[test]
fn a_death_just_before_a_mission_load_restores_a_ruin() {
    let mut w = world(1);
    let id = statics(&w)[0];
    let o = object_of(&w, id).expect("the static is in DCS");
    w.sim.kill(o);
    w.sim.load_mission();
    ticks(&mut w, 60);
    assert_eq!(state(&w, id), State::Dead);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(object_of(&w, id), None);
    assert_eq!(live_spawns(&w, id), 1, "the one spawn before the load");
    w.settle().unwrap();
}

/// A death that arrives while the restore of the static is unanswered: the
/// static's spawn after a load ran, and it died before the sidecar read the
/// result. It is `dead`, and the next load spawns the ruin.
#[test]
fn a_death_while_the_restore_is_unanswered_makes_a_ruin() {
    let mut w = world(1);
    let id = statics(&w)[0];
    w.apply(&Op::LoadMission).unwrap();
    // Until the spawn is written and unanswered.
    let mut sent = false;
    for _ in 0..80 {
        w.tick().unwrap();
        if state(&w, id) == State::Materializing
            && w.rec
                .inflight()
                .any(|c| c.entity == id && c.opcode == opcode::STATIC_SPAWN)
        {
            sent = true;
            break;
        }
    }
    assert!(sent, "the restore spawn was never in flight");
    // DCS runs the spawn and the static dies before the sidecar reads.
    w.sim.frame();
    let o = object_of(&w, id).expect("the spawn ran");
    w.sim.kill(o);
    let written = commands(&w, id, opcode::STATIC_SPAWN);
    ticks(&mut w, 20);
    assert_eq!(state(&w, id), State::Dead);
    assert!(ruin_of(&w, id).is_some_and(|r| !r.spawned));
    assert_eq!(
        commands(&w, id, opcode::STATIC_SPAWN),
        written,
        "its own dead object is the ruin: no spawn is written"
    );
    assert_eq!(ruin_spawns(&w, id), 0);
    assert_eq!(w.rec.inflight().count(), 0);
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(live_spawns(&w, id), 2, "the first spawn and the restore");
    w.settle().unwrap();
}

/// A static that leaves the agent's table first, as `REMOVED` or `MISSING`,
/// and whose death event comes later: the death fact clears the mark, so it
/// is a ruin and never reads as a removal. Its own dead object is the ruin in
/// that generation, with nothing written, and the next load spawns it dead.
#[test]
fn a_late_death_after_a_removed_or_missing_report_makes_a_ruin() {
    for left in [Left::Removed, Left::Missing] {
        let mut w = world(1);
        let id = statics(&w)[0];
        let o = object_of(&w, id).expect("the static is in DCS");
        w.sim.kill_late(o, 2.0, left);
        w.tick().unwrap();
        w.tick().unwrap();
        let u = w.store.unit(id).expect("unit").clone();
        assert!(!u.dead && u.missing, "{left:?}: it left before its death");
        assert_eq!(u.removed, left == Left::Removed);
        // Past the delay: the death event comes.
        ticks(&mut w, 80);
        let u = w.store.unit(id).expect("unit").clone();
        assert!(
            u.dead && !u.missing && !u.removed,
            "{left:?}: the mark stays"
        );
        assert_eq!(state(&w, id), State::Dead, "{left:?}");
        assert!(ruin_of(&w, id).is_some_and(|r| !r.spawned), "{left:?}");
        assert_eq!(
            (
                commands(&w, id, opcode::STATIC_SPAWN),
                commands(&w, id, opcode::STATIC_DESTROY)
            ),
            (1, 0),
            "{left:?}: nothing is written in the generation it died in"
        );
        w.apply(&Op::LoadMission).unwrap();
        ticks(&mut w, 60);
        assert!(ruin_of(&w, id).is_some_and(|r| r.spawned), "{left:?}");
        assert_eq!(live_spawns(&w, id), 1, "{left:?}: never alive again");
        w.settle().unwrap();
    }
}

/// A dead static that a script brings back alive is destroyed, and its ruin
/// is spawned: the static stays `dead`.
#[test]
fn a_revived_dead_static_is_destroyed_and_its_ruin_spawned() {
    let mut w = world(1);
    let id = statics(&w)[0];
    let o = object_of(&w, id).expect("the static is in DCS");
    kill(&mut w, id);
    assert!(w.sim.revive(o), "the fake revived it");
    ticks(&mut w, 30);
    assert_eq!(state(&w, id), State::Dead);
    assert_eq!(object_of(&w, id), None, "the live object was destroyed");
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(w.rec.counters().ruins_revived, 1);
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1);
    assert_eq!(live_spawns(&w, id), 1);
    w.settle().unwrap();
}

/// A static created destroyed is `dead` from the start: DCS gets its table
/// with `dead = true` and never a live static.
#[test]
fn a_static_created_destroyed_is_spawned_dead_only() {
    let mut w = world(0);
    let id = w.create_ruin(a_tent(-5_000.0, "Tent"));
    assert_eq!(state(&w, id), State::Dead);
    assert!(w.store.unit(id).expect("unit").dead);
    ticks(&mut w, 20);
    let ruin = ruin_of(&w, id).expect("the ruin is in DCS");
    assert!(ruin.spawned);
    assert_eq!(
        (ruin.type_name.as_str(), ruin.x, ruin.z, ruin.heading),
        ("Tent", -5_000.0, 70.0, 1.25)
    );
    assert_eq!(live_spawns(&w, id), 0);
    assert_eq!(ruin_spawns(&w, id), 1);
    assert_eq!(w.rec.counters().ruins_spawned, 1);
    // The store's grid still finds it, for an area query.
    assert!(
        w.store
            .grid()
            .within(THEATER, -5_000.0, 70.0, 100.0)
            .contains(&id)
    );
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(ruin_spawns(&w, id), 2);
    assert_eq!(live_spawns(&w, id), 0);
    w.settle().unwrap();
}

/// A live static whose further keys hold `dead` is still spawned alive:
/// only its state in the store says whether it spawns destroyed.
#[test]
fn a_live_static_is_never_spawned_dead_by_its_further_keys() {
    let mut w = world(0);
    let mut spec = a_tent(-7_000.0, "Tent");
    spec.units[0].extra = lv::obj(vec![("dead", nanoprobe_transport::codec::Lv::Bool(true))]);
    let id = w.store.create_static(spec).expect("ids");
    ticks(&mut w, 20);
    assert_eq!(state(&w, id), State::Live);
    assert!(object_of(&w, id).is_some());
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!((live_spawns(&w, id), ruin_spawns(&w, id)), (1, 0));
    w.settle().unwrap();
}

/// A ruin DCS refuses is not sent again in that generation, and its error
/// shows; the next generation tries once more.
#[test]
fn a_refused_ruin_is_not_retried_in_its_generation() {
    let mut w = world(0);
    let id = w.create_ruin(a_tent(-6_000.0, "Unknown-Tent"));
    ticks(&mut w, 60);
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!(commands(&w, id, opcode::STATIC_SPAWN), 1, "sent once");
    assert_eq!(w.rec.counters().ruins_refused, 1);
    assert_eq!(
        w.store.group(id).expect("static").error.as_deref(),
        Some("unknown static type")
    );
    assert_eq!(state(&w, id), State::Dead);
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert_eq!(commands(&w, id, opcode::STATIC_SPAWN), 2);
    w.settle().unwrap();
}

/// A ruin's spawn whose result is lost is sent again, and the agent answers
/// it without spawning a second ruin.
#[test]
fn a_ruin_spawn_with_a_lost_result_is_not_spawned_twice() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.apply(&Op::LoadMission).unwrap();
    // Lose the result of the ruin's spawn, whichever step sends it.
    let mut lost = false;
    for _ in 0..80 {
        w.tick().unwrap();
        if !lost
            && w.rec
                .inflight()
                .any(|c| c.entity == id && c.opcode == opcode::STATIC_SPAWN && c.units.is_empty())
        {
            w.sim.lose_next_result();
            lost = true;
        }
    }
    assert!(lost, "the ruin's spawn was never in flight");
    w.settle().unwrap();
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(ruin_spawns(&w, id), 1, "the handler ran once");
    assert!(w.sim.ledger().ruins_replaced.is_empty());
}

/// Many ruins restored while paused use the restore burst, like groups.
#[test]
fn ruins_restored_while_paused_use_the_burst() {
    let mut store = new_store(0, 0);
    let mut ids = Vec::new();
    for i in 0..8 {
        ids.push(
            store
                .create_ruin(a_tent(-1_000.0 * f64::from(i + 1), "Tent"))
                .expect("ids"),
        );
    }
    let mut w = World::with_store(
        store,
        Config {
            event_queue: 10_000,
            ..Config::default()
        },
        nanoprobe_sidecar::reconciler::Config {
            materialize_per_frame: 2,
            on_lost: LostPolicy::Report,
            destroy_retries: 3,
        },
    );
    for id in &ids {
        w.born_dead(*id);
    }
    // The mission starts paused, before the sidecar reads anything.
    for _ in 0..10 {
        if w.sim.running() {
            break;
        }
        w.sim.frame();
    }
    w.sim.pause();
    assert!(w.sim.paused());
    // All 8 spawns reach DCS in one frame: the burst lifts the budget of 2.
    let mut most = 0;
    for _ in 0..40 {
        let before = w.sim.ledger().ruin_spawns.len();
        w.tick().unwrap();
        most = most.max(w.sim.ledger().ruin_spawns.len() - before);
    }
    assert_eq!(most, 8);
    for id in &ids {
        assert!(ruin_of(&w, *id).is_some_and(|r| r.spawned), "ruin {id}");
    }
    assert!(
        w.sim.ledger().bursts.contains(&(true, true)),
        "no burst while paused: {:?}",
        w.sim.ledger().bursts
    );
    assert!(!w.sim.burst(), "the burst ends once every ruin is spawned");
}

/// Without the burst, ruins take their turn within the spawn budget.
#[test]
fn ruins_are_spawned_within_the_materialization_budget() {
    let mut w = world(0);
    let ids: Vec<EntityId> = (0..7)
        .map(|i| w.create_ruin(a_tent(-1_000.0 * f64::from(i + 1), "Tent")))
        .collect();
    let mut most = 0;
    for _ in 0..40 {
        w.tick().unwrap();
        let unanswered = w
            .rec
            .inflight()
            .filter(|c| c.opcode == opcode::STATIC_SPAWN)
            .count();
        most = most.max(unanswered);
    }
    assert!(most <= 2, "{most} ruin spawns unanswered at once, budget 2");
    for id in &ids {
        assert!(ruin_of(&w, *id).is_some(), "ruin {id}");
    }
    w.settle().unwrap();
}

/// Tick until a command of the opcode is unanswered for the entity, then
/// lose its result: the link drops with it.
fn lose_result_of(w: &mut World, id: EntityId, op: u16) {
    for _ in 0..80 {
        w.tick().unwrap();
        if w.rec.inflight().any(|c| c.entity == id && c.opcode == op) {
            w.sim.lose_next_result();
            w.tick().unwrap();
            return;
        }
    }
    panic!("opcode {op} was never unanswered for {id}");
}

/// A ruin's destroy whose result is lost may have run. A materialize in the
/// same generation still gets the ruin: the store no longer says DCS holds
/// it, so the spawn is written again.
#[test]
fn a_destroy_with_a_lost_result_does_not_strand_the_ruin() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.store
        .set_authority(id, Authority::Campaign)
        .expect("static");
    lose_result_of(&mut w, id, opcode::STATIC_DESTROY);
    assert_eq!(ruin_of(&w, id), None, "the destroy ran in DCS");
    w.store.set_authority(id, Authority::Sim).expect("static");
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Dead);
    assert!(
        ruin_of(&w, id).is_some_and(|r| r.spawned),
        "the ruin is back in the generation it was destroyed in"
    );
    assert_eq!(live_spawns(&w, id), 1);
}

/// The same lost result with the static left under campaign authority: no
/// ruin comes back, and the destroy is not left half done.
#[test]
fn a_destroy_with_a_lost_result_stays_destroyed_under_campaign_authority() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.store
        .set_authority(id, Authority::Campaign)
        .expect("static");
    lose_result_of(&mut w, id, opcode::STATIC_DESTROY);
    w.settle().unwrap();
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!(ruin_spawns(&w, id), 0);
    let g = w.store.group(id).expect("static");
    assert_eq!((g.ruin_in, g.ruin_sent_in), (None, None));
}

/// A sidecar that restarts with a ruin's spawn written and unanswered still
/// knows DCS may hold the ruin: a dematerialize destroys it.
#[test]
fn a_restarted_sidecar_destroys_a_ruin_whose_spawn_it_had_written() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.apply(&Op::LoadMission).unwrap();
    lose_result_of(&mut w, id, opcode::STATIC_SPAWN);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned), "the spawn ran");
    let g = w.store.group(id).expect("static").clone();
    assert!(g.ruin_sent_in.is_some() && g.ruin_in != g.ruin_sent_in);
    w.apply(&Op::SidecarRestart).unwrap();
    w.store
        .set_authority(id, Authority::Campaign)
        .expect("static");
    w.settle().unwrap();
    assert_eq!(ruin_of(&w, id), None, "the ruin was left in DCS");
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1);
}

/// The error of a refused ruin goes when a later spawn of it applies.
#[test]
fn a_refused_ruins_error_clears_when_a_later_spawn_applies() {
    let mut w = world(0);
    w.sim.refuse_ruins(1);
    let id = w.create_ruin(a_tent(-8_000.0, "Tent"));
    ticks(&mut w, 40);
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!(commands(&w, id, opcode::STATIC_SPAWN), 1);
    assert_eq!(
        w.store.group(id).expect("static").error.as_deref(),
        Some("injected: the dead static was refused")
    );
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(w.store.group(id).expect("static").error, None);
    w.settle().unwrap();
}

/// A ruin's spawn the agent rejects for good, as it would a table too
/// large, is kept and shown like a refusal: an error, and no resend in the
/// generation.
#[test]
fn a_rejected_ruin_shows_its_error_and_is_not_retried() {
    let mut w = world(0);
    w.sim.reject_ruins(1);
    let id = w.create_ruin(a_tent(-9_000.0, "Tent"));
    ticks(&mut w, 60);
    assert_eq!(ruin_of(&w, id), None);
    assert_eq!(commands(&w, id, opcode::STATIC_SPAWN), 1, "sent once");
    assert_eq!(w.rec.counters().ruins_refused, 1);
    assert!(
        w.store
            .group(id)
            .expect("static")
            .error
            .as_deref()
            .is_some_and(|e| e.contains("rejected")),
        "no error for a rejected ruin"
    );
    assert_eq!(state(&w, id), State::Dead);
    w.apply(&Op::LoadMission).unwrap();
    ticks(&mut w, 60);
    assert!(ruin_of(&w, id).is_some_and(|r| r.spawned));
    assert_eq!(w.store.group(id).expect("static").error, None);
    w.settle().unwrap();
}

/// A ruin's destroy that DCS refused and whose result is lost did not run:
/// the static's dead object is still in DCS. The spawn written after a
/// materialize is answered `AT_REVISION`, nothing is spawned over the
/// object, and the store marks it held again.
#[test]
fn a_lost_result_of_a_destroy_that_did_not_run_keeps_the_dead_object() {
    let mut w = world(1);
    let id = statics(&w)[0];
    kill(&mut w, id);
    w.store
        .set_authority(id, Authority::Campaign)
        .expect("static");
    w.sim.refuse_destroys(1);
    lose_result_of(&mut w, id, opcode::STATIC_DESTROY);
    assert!(
        ruin_of(&w, id).is_some_and(|r| !r.spawned),
        "the refused destroy left the dead object"
    );
    w.store.set_authority(id, Authority::Sim).expect("static");
    w.settle().unwrap();
    assert!(ruin_of(&w, id).is_some_and(|r| !r.spawned));
    assert_eq!(ruin_spawns(&w, id), 0, "no handler ran: AT_REVISION");
    assert!(w.sim.ledger().ruins_replaced.is_empty());
    let l = w.store.loaded().expect("loaded").clone();
    assert_eq!(
        w.store.group(id).expect("static").ruin_in,
        Some((l.agent_boot, l.mission_gen))
    );
}

/// A lost destroy result moves only a mark of the loaded generation: a
/// stale one is dropped, and the mark of a spawn written in the loaded
/// generation stays, so the ruin is still destroyed when it should be.
#[test]
fn a_lost_destroy_result_keeps_the_mark_of_a_spawn_written_since() {
    let mut store = new_store(0, 1);
    let id = store.groups().next().expect("static").id;
    store.set_loaded(nanoprobe_sidecar::store::Loaded {
        agent_boot: 7,
        mission_gen: 3,
        theater: THEATER.into(),
    });
    // Held in generation 2; a spawn written in generation 3.
    store.ruin_held(id, 7, 2).expect("held");
    store.ruin_sent(id, 7, 3).expect("sent");
    store.ruin_unsure(id).expect("unsure");
    let g = store.group(id).expect("static");
    assert_eq!((g.ruin_in, g.ruin_sent_in), (None, Some((7, 3))));
    // Held in the loaded generation: the mark moves.
    store.ruin_held(id, 7, 3).expect("held");
    store.ruin_gone(id).expect("gone");
    store.ruin_held(id, 7, 3).expect("held");
    store.ruin_unsure(id).expect("unsure");
    let g = store.group(id).expect("static");
    assert_eq!((g.ruin_in, g.ruin_sent_in), (None, Some((7, 3))));
}
