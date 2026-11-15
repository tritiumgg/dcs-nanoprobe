//! Late death events, `REMOVED`, revival and the respawn gate (spec/world.md,
//! "Respawn gate"), on the fake sim. A weapon kill leaves the agent's table
//! as `MISSING` at once, and DCS sends its death event up to about 83 s
//! later; the agent then writes a second `OBJECT_GONE` with `DEAD`.

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::lifecycle::Trigger;
use nanoprobe_sidecar::replay::{Persistence, fold};
use nanoprobe_sidecar::sim::RawRecord;
use nanoprobe_sidecar::sim::wire::{Input, ObjectGone, gone_reason, opcode};
use nanoprobe_sidecar::store::{Authority, Entry, LostPolicy};
use nanoprobe_sim_fake::harness::{Op, World, new_store};
use nanoprobe_sim_fake::{Config, Left};

/// A world whose event queue holds a detached stretch of kills.
fn world(groups: usize, statics: usize, policy: LostPolicy) -> World {
    World::with_config(
        groups,
        statics,
        policy,
        Config {
            event_queue: 10_000,
            ..Config::default()
        },
    )
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().unwrap();
    }
}

/// The group's units in DCS: object ID and unit entity.
fn units_of(w: &World, group: EntityId) -> Vec<(u64, u64)> {
    w.units_in_dcs()
        .into_iter()
        .filter(|(_, e)| w.store.unit(EntityId(*e)).is_some_and(|u| u.group == group))
        .collect()
}

fn state(w: &World, id: EntityId) -> State {
    w.store.group(id).expect("group").state
}

fn spawns(w: &World, entity: EntityId) -> usize {
    w.sim
        .ledger()
        .spawns
        .iter()
        .filter(|s| s.2 == entity.0)
        .count()
}

fn entries(w: &World, f: impl Fn(&Entry) -> bool) -> usize {
    w.store.entries().iter().filter(|e| f(e)).count()
}

fn first_group(w: &World) -> EntityId {
    w.store.groups().next().expect("a group").id
}

/// Kill every unit of the group, each event `delay` seconds late.
fn kill_all_late(w: &mut World, group: EntityId, delay: f64, left: Left) {
    for (o, _) in units_of(w, group) {
        w.sim.kill_late(o, delay, left);
    }
}

fn record(w: &World, input: Input, n: u64) -> RawRecord {
    let (kind, payload) = input.encode();
    RawRecord {
        agent_boot: w.sim.agent_boot(),
        number: u64::MAX - n,
        kind,
        payload,
        resync: false,
    }
}

/// One frame of DCS and one step of the reconciler, with no invariant check.
fn step(w: &mut World) {
    w.sim.frame();
    w.rec.step(&mut w.store, &mut w.sim);
}

/// A late death event after `MISSING` makes the group `dead`, and until it
/// comes the group is `lost`, held and never spawned again.
#[test]
fn a_late_death_after_missing_makes_the_group_dead() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    kill_all_late(&mut w, id, 60.0, Left::Missing);
    // Enumerations find nothing: the group is lost, and the gate holds it.
    ticks(&mut w, 300);
    assert_eq!(state(&w, id), State::Lost);
    assert!(w.rec.held().contains(&id));
    assert_eq!(w.rec.counters().respawns_held, 1);
    assert_eq!(spawns(&w, id), 1);
    // The death events come: lost -> dead, with no spawn in between.
    ticks(&mut w, 400);
    assert_eq!(state(&w, id), State::Dead);
    assert!(w.rec.held().is_empty());
    assert_eq!(spawns(&w, id), 1);
    let c = w.rec.counters();
    assert_eq!((c.late_deaths, c.classifier_faults), (3, 0));
    w.settle().unwrap();
}

/// A death that corrects a `REMOVED` counts a classifier fault, journalled
/// and counted, and makes the group `dead`.
#[test]
fn a_death_after_removed_counts_a_classifier_fault() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    kill_all_late(&mut w, id, 60.0, Left::Removed);
    ticks(&mut w, 300);
    assert_eq!(state(&w, id), State::Lost);
    assert_eq!(spawns(&w, id), 1);
    ticks(&mut w, 400);
    assert_eq!(state(&w, id), State::Dead);
    let c = w.rec.counters();
    assert_eq!((c.late_deaths, c.classifier_faults), (3, 3));
    assert_eq!(
        entries(&w, |e| matches!(e, Entry::ClassifierFault { .. })),
        3
    );
    assert_eq!(entries(&w, |e| matches!(e, Entry::Death { .. })), 3);
    w.settle().unwrap();
}

/// `REMOVED` and `MISSING` hold the gate for good: no timer and no
/// enumeration opens it.
#[test]
fn removed_and_missing_hold_the_gate_for_good() {
    for removed in [true, false] {
        let mut w = world(1, 0, LostPolicy::Respawn);
        w.settle().unwrap();
        let id = first_group(&w);
        for (o, _) in units_of(&w, id) {
            if removed {
                w.sim.remove_with_event(o);
            } else {
                w.sim.remove_silently(o);
            }
        }
        w.settle().unwrap();
        assert_eq!(state(&w, id), State::Lost, "removed {removed}");
        // Two thousand frames, forty enumerations.
        ticks(&mut w, 2_000);
        assert_eq!(state(&w, id), State::Lost, "removed {removed}");
        assert_eq!(spawns(&w, id), 1, "removed {removed}");
        assert!(w.rec.held().contains(&id));
        assert_eq!(w.rec.counters().respawns_held, 1, "once, not each step");
        assert!(
            w.store
                .group(id)
                .unwrap()
                .members
                .iter()
                .all(|m| w.store.unit(*m).unwrap().removed == removed)
        );
    }
}

/// A policy of `dead` still marks a held group dead: the gate stops only the
/// respawn.
#[test]
fn the_gate_does_not_stop_the_dead_policy() {
    let mut w = world(1, 0, LostPolicy::Dead);
    w.settle().unwrap();
    let id = first_group(&w);
    for (o, _) in units_of(&w, id) {
        w.sim.remove_with_event(o);
    }
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Dead);
}

/// A death fact that arrives after the respawn policy moved a group to
/// `materializing`, and before its spawn is written, makes it `dead` and the
/// spawn is never written.
#[test]
fn a_death_cancels_a_spawn_that_is_not_yet_issued() {
    let mut w = world(0, 1, LostPolicy::Respawn);
    let id = first_group(&w);
    w.settle().unwrap();
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
    // The static is lost with no member marked, and its binding is gone:
    // the shape of a group lost to destroys that failed.
    w.store.transition(id, Trigger::EnumMissing).unwrap();
    let boot_gone = record(
        &w,
        Input::ObjectGone(ObjectGone {
            object,
            reason: gone_reason::DESTROYED,
            entity: 0,
            group_entity: 0,
        }),
        1,
    );
    w.rec.record(&mut w.store, &boot_gone);
    assert!(w.rec.binding().get(&object).is_none());
    let before = entries(
        &w,
        |e| matches!(e, Entry::Command(c) if c.entity == id && c.opcode == opcode::STATIC_SPAWN),
    );
    // The policy moves it on: the gate is open.
    w.rec.step(&mut w.store, &mut w.sim);
    assert_eq!(state(&w, id), State::Materializing);
    // The death arrives before the spawn is written.
    let dead = record(
        &w,
        Input::ObjectGone(ObjectGone {
            object,
            reason: gone_reason::DEAD,
            entity: id.0,
            group_entity: 0,
        }),
        2,
    );
    w.rec.record(&mut w.store, &dead);
    assert_eq!(state(&w, id), State::Dead);
    for _ in 0..20 {
        step(&mut w);
    }
    let after = entries(
        &w,
        |e| matches!(e, Entry::Command(c) if c.entity == id && c.opcode == opcode::STATIC_SPAWN),
    );
    assert_eq!(after, before, "no spawn was issued");
    assert_eq!(state(&w, id), State::Dead);
}

/// A death for an object from before a respawn is journalled and counted,
/// and does not mark the respawned member.
#[test]
fn a_death_for_an_object_from_before_a_respawn_marks_nobody() {
    let mut w = world(0, 1, LostPolicy::Report);
    let id = first_group(&w);
    w.settle().unwrap();
    let (old, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
    // Out of DCS and back in the same generation: new object IDs.
    w.store.set_authority(id, Authority::Campaign).unwrap();
    w.settle().unwrap();
    w.store.set_authority(id, Authority::Sim).unwrap();
    w.settle().unwrap();
    let (new, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
    assert_ne!(old, new);
    let late = record(
        &w,
        Input::ObjectGone(ObjectGone {
            object: old,
            reason: gone_reason::DEAD,
            entity: id.0,
            group_entity: 0,
        }),
        1,
    );
    w.rec.record(&mut w.store, &late);
    assert!(!w.store.unit(id).unwrap().dead);
    assert_eq!(state(&w, id), State::Live);
    assert_eq!(w.rec.counters().stale_deaths, 1);
    assert_eq!(entries(&w, |e| matches!(e, Entry::StaleDeath { .. })), 1);
    assert_eq!(entries(&w, |e| matches!(e, Entry::Death { .. })), 0);
    // A death for the object it holds now is a death.
    let now = record(
        &w,
        Input::ObjectGone(ObjectGone {
            object: new,
            reason: gone_reason::DEAD,
            entity: id.0,
            group_entity: 0,
        }),
        2,
    );
    w.rec.record(&mut w.store, &now);
    assert_eq!(state(&w, id), State::Dead);
}

/// An object that had a death fact and is alive again: the death stays
/// journalled, the member is alive and bound, and it holds nothing.
#[test]
fn a_revived_unit_is_alive_bound_and_holds_nothing() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let (object, entity) = units_of(&w, id)[0];
    w.sim.kill(object);
    ticks(&mut w, 5);
    let unit = EntityId(entity);
    assert!(w.store.unit(unit).unwrap().dead);
    assert!(w.sim.revive(object));
    ticks(&mut w, 5);
    let u = w.store.unit(unit).unwrap();
    assert!(!u.dead && !u.missing);
    assert_eq!(w.rec.binding().get(&object).map(|b| b.unit), Some(unit));
    assert_eq!(w.rec.counters().revivals, 1);
    assert_eq!(entries(&w, |e| matches!(e, Entry::Death { .. })), 1);
    assert_eq!(entries(&w, |e| matches!(e, Entry::Revived { .. })), 1);
    assert_eq!(state(&w, id), State::Live);
    assert!(!w.store.gate_shut(id));
    assert_eq!(spawns(&w, id), 1, "a revival is not a spawn");
}

/// A member that left as `MISSING` with its death event pending holds the
/// gate until a death fact explains it; a member removed with no event
/// keeps holding it.
#[test]
fn a_member_missing_with_a_late_death_pending_holds_the_gate_until_it_dies() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let units = units_of(&w, id);
    w.sim.kill(units[0].0);
    w.sim.kill_late(units[1].0, 60.0, Left::Missing);
    w.sim.remove_silently(units[2].0);
    ticks(&mut w, 600);
    // One dead, one missing with its death pending, one missing: held.
    assert!(w.store.gate_shut(id));
    // The late death explains its member, which no longer holds the gate.
    ticks(&mut w, 400);
    assert!(w.store.unit(EntityId(units[1].1)).unwrap().dead);
    assert!(w.store.gate_shut(id), "the silent removal still holds it");
    assert_eq!(spawns(&w, id), 1);
}

/// The late death releases its member: it holds nothing, and the survivors
/// in DCS keep the group `live`.
#[test]
fn a_late_death_releases_its_member_from_the_gate() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let units = units_of(&w, id);
    w.sim.kill_late(units[0].0, 30.0, Left::Missing);
    ticks(&mut w, 100);
    assert!(w.store.gate_shut(id));
    ticks(&mut w, 300);
    assert!(w.store.unit(EntityId(units[0].1)).unwrap().dead);
    assert!(!w.store.gate_shut(id));
    // Survivors are alive in DCS: the group is live, and is not spawned.
    assert_eq!(state(&w, id), State::Live);
    assert_eq!(spawns(&w, id), 1);
}

/// The respawn leaves a dead member out and spawns the survivors: a group
/// lost through a failed destroy has a death fact for one member.
#[test]
fn a_respawn_leaves_the_dead_member_out() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let units = units_of(&w, id);
    w.sim.kill(units[0].0);
    ticks(&mut w, 5);
    assert!(w.store.unit(EntityId(units[0].1)).unwrap().dead);
    // Lost with no member marked, and no binding: the shape of a group lost
    // to destroys that failed.
    w.store.transition(id, Trigger::EnumMissing).unwrap();
    for (n, (o, _)) in units.iter().enumerate().skip(1) {
        let gone = record(
            &w,
            Input::ObjectGone(ObjectGone {
                object: *o,
                reason: gone_reason::DESTROYED,
                entity: 0,
                group_entity: 0,
            }),
            n as u64,
        );
        w.rec.record(&mut w.store, &gone);
    }
    // The survivors leave DCS with the destroy the sidecar did not see.
    for (o, _) in units.iter().skip(1) {
        w.sim.remove_silently(*o);
    }
    w.rec.step(&mut w.store, &mut w.sim);
    assert_eq!(state(&w, id), State::Materializing);
    for _ in 0..10 {
        step(&mut w);
    }
    let spawn = w
        .sim
        .ledger()
        .spawns
        .iter()
        .rfind(|s| s.2 == id.0)
        .expect("a spawn")
        .clone();
    assert_eq!(spawn.4.len(), units.len() - 1);
    assert!(
        !spawn.4.contains(&units[0].1),
        "the dead member is left out"
    );
}

/// A store with one group of ten units, under DCS authority.
fn ten_unit_store() -> nanoprobe_sidecar::store::Store {
    use nanoprobe_sidecar::store::{NewGroup, NewUnit, Point, UnitState};
    let mut store = nanoprobe_sidecar::store::Store::new(
        nanoprobe_sidecar::ids::CampaignId(nanoprobe_sim_fake::harness::CAMPAIGN),
        10_000,
    );
    let units = (0..10)
        .map(|u| {
            NewUnit::new(
                "M-113",
                UnitState {
                    at: Point {
                        x: 0.0,
                        y: 0.0,
                        z: u as f64 * 20.0,
                    },
                    ..UnitState::default()
                },
            )
        })
        .collect();
    store
        .create_group(NewGroup {
            theater: nanoprobe_sim_fake::harness::THEATER.into(),
            country: 2,
            category: 2,
            coalition: 2,
            units,
            authority: Authority::Sim,
            label: "ten".into(),
            extra: nanoprobe_sidecar::sim::wire::lv::obj(vec![]),
        })
        .expect("ids");
    store
}

/// One member killed, the other nine removed by script (`REMOVED` or
/// `MISSING`), the respawn policy: nothing is spawned and the group stays
/// `lost` and reported. A mission load then materializes the nine and never
/// the dead one.
#[test]
fn nine_removed_after_one_kill_are_held_then_materialize_after_a_reload_without_the_dead() {
    for removed in [true, false] {
        let mut w = world(0, 0, LostPolicy::Respawn);
        w.sidecar_of(ten_unit_store());
        w.settle().unwrap();
        let id = first_group(&w);
        let units = units_of(&w, id);
        assert_eq!(units.len(), 10);
        w.sim.kill(units[0].0);
        for (o, _) in units.iter().skip(1) {
            if removed {
                w.sim.remove_with_event(*o);
            } else {
                w.sim.remove_silently(*o);
            }
        }
        w.settle().unwrap();
        ticks(&mut w, 600);
        assert_eq!(state(&w, id), State::Lost, "removed {removed}");
        assert!(w.rec.held().contains(&id));
        assert_eq!(spawns(&w, id), 1, "nothing is spawned, removed {removed}");
        assert!(w.store.unit(EntityId(units[0].1)).unwrap().dead);

        w.apply(&Op::LoadMission).unwrap();
        w.settle().unwrap();
        assert_eq!(state(&w, id), State::Live);
        let spawn = w.sim.ledger().spawns.last().unwrap().clone();
        assert_eq!(spawn.4.len(), 9, "removed {removed}");
        assert!(!spawn.4.contains(&units[0].1), "the dead one never spawns");
    }
}

/// A revived member is bound again, the group is live and nothing holds it.
#[test]
fn a_revived_member_is_bound_again_and_the_group_is_live() {
    let mut w = world(0, 0, LostPolicy::Respawn);
    w.sidecar_of(ten_unit_store());
    w.settle().unwrap();
    let id = first_group(&w);
    let (object, entity) = units_of(&w, id)[0];
    w.sim.kill(object);
    ticks(&mut w, 5);
    assert!(w.sim.revive(object));
    ticks(&mut w, 5);
    assert_eq!(
        w.rec.binding().get(&object).map(|b| b.unit),
        Some(EntityId(entity))
    );
    assert_eq!(state(&w, id), State::Live);
    assert!(!w.store.gate_shut(id));
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Live);
}

/// A death after a revival is a new death fact.
#[test]
fn a_revived_unit_that_dies_again_has_a_second_death_fact() {
    let mut w = world(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let id = first_group(&w);
    let (object, entity) = units_of(&w, id)[0];
    w.sim.kill(object);
    ticks(&mut w, 5);
    assert!(w.sim.revive(object));
    ticks(&mut w, 5);
    assert!(!w.store.unit(EntityId(entity)).unwrap().dead);
    w.sim.kill(object);
    ticks(&mut w, 5);
    assert!(w.store.unit(EntityId(entity)).unwrap().dead);
    assert_eq!(entries(&w, |e| matches!(e, Entry::Death { .. })), 2);
    assert_eq!(w.rec.counters().revivals, 1);
}

/// A group that is already `dead` stays dead: its revived object is drift
/// and is destroyed.
#[test]
fn a_revival_in_a_dead_group_changes_nothing() {
    let mut w = world(0, 1, LostPolicy::Report);
    w.settle().unwrap();
    let id = first_group(&w);
    let (object, _) = w.units_in_dcs().into_iter().next().expect("in DCS");
    w.sim.kill(object);
    ticks(&mut w, 5);
    assert_eq!(state(&w, id), State::Dead);
    assert!(w.sim.revive(object));
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Dead);
    assert!(w.store.unit(id).unwrap().dead);
    assert_eq!(w.rec.counters().revivals, 0);
    assert!(w.rec.counters().after_dead >= 1);
    assert!(w.sim.platform_groups(w.store.campaign().0).is_empty());
}

/// A late death event written while the link is down is read after the
/// attach, after the `MISSING` that came first.
#[test]
fn a_late_death_written_while_detached_is_read_after_the_attach() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    w.apply(&Op::Detach).unwrap();
    kill_all_late(&mut w, id, 5.0, Left::Missing);
    ticks(&mut w, 100);
    w.apply(&Op::Reattach).unwrap();
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Dead);
    assert_eq!(spawns(&w, id), 1);
    assert_eq!(w.rec.counters().late_deaths, 3);
}

/// A sidecar that restarts with its store keeps the gate and reads the late
/// death.
#[test]
fn a_restart_with_the_store_keeps_the_gate_and_reads_the_late_death() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    kill_all_late(&mut w, id, 25.0, Left::Removed);
    ticks(&mut w, 150);
    assert_eq!(state(&w, id), State::Lost);
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 40);
    assert_eq!(state(&w, id), State::Lost, "the store holds the gate");
    assert_eq!(spawns(&w, id), 1);
    ticks(&mut w, 200);
    assert_eq!(state(&w, id), State::Dead);
    let c = w.rec.counters();
    assert_eq!((c.late_deaths, c.classifier_faults), (3, 3));
    w.settle().unwrap();
}

/// A sidecar that restarts without its store starts again from the specs:
/// the group is spawned, and the late death names the old objects.
#[test]
fn a_restart_without_the_store_ignores_the_late_death_of_the_old_objects() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    kill_all_late(&mut w, id, 30.0, Left::Missing);
    ticks(&mut w, 20);
    // The author writes the specs back; the agent still holds revision 1.
    let mut fresh = new_store(1, 0);
    fresh.bump_spec(id).unwrap();
    w.sidecar_of(fresh);
    // The new store spawns the group: new objects, bound.
    ticks(&mut w, 100);
    assert_eq!(state(&w, id), State::Live);
    assert_eq!(spawns(&w, id), 2);
    ticks(&mut w, 300);
    assert_eq!(
        state(&w, id),
        State::Live,
        "the old objects' deaths mark nobody"
    );
    assert_eq!(w.rec.counters().stale_deaths, 3);
    assert!(
        w.store
            .group(id)
            .unwrap()
            .members
            .iter()
            .all(|m| !w.store.unit(*m).unwrap().dead)
    );
    w.settle().unwrap();
}

/// A campaign switch discards the binding, and the agent's memory of the rows
/// that left with it: the death event that comes after names no entity. The
/// member stays marked and the gate stays shut.
#[test]
fn a_campaign_switch_drops_the_memory_of_the_late_death() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let (object, entity) = units_of(&w, id)[0];
    w.sim.kill_late(object, 20.0, Left::Missing);
    ticks(&mut w, 5);
    assert_eq!(w.sim.late_pending(), 1);
    w.apply(&Op::ForeignCampaign).unwrap();
    assert_eq!(w.sim.late_pending(), 0);
    ticks(&mut w, 250);
    w.apply(&Op::Reattach).unwrap();
    ticks(&mut w, 100);
    let u = w.store.unit(EntityId(entity)).unwrap();
    assert!(!u.dead, "no death fact: the agent wrote none");
    assert!(u.missing);
    assert!(w.store.gate_shut(id));
    w.settle().unwrap();
}

/// A record the agent stamped with another campaign is no death fact, even
/// when it names a unit of this campaign by entity.
#[test]
fn a_death_stamped_with_another_campaign_is_dropped() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let (object, entity) = units_of(&w, id)[0];
    w.sim.kill_late(object, 20.0, Left::Missing);
    ticks(&mut w, 5);
    // The agent is on another campaign when the record is written.
    w.sim
        .set_campaign(nanoprobe_sim_fake::harness::FOREIGN_CAMPAIGN);
    let frame = record(
        &w,
        Input::Frame(nanoprobe_transport::records::Frame {
            frame: 1,
            campaign: nanoprobe_sim_fake::harness::FOREIGN_CAMPAIGN,
            mission_gen: w.sim.mission_gen(),
            lifecycle: 2,
            ..Default::default()
        }),
        3,
    );
    let dead = record(
        &w,
        Input::ObjectGone(ObjectGone {
            object,
            reason: gone_reason::DEAD,
            entity,
            group_entity: id.0,
        }),
        2,
    );
    w.rec.record(&mut w.store, &frame);
    w.rec.record(&mut w.store, &dead);
    assert!(!w.store.unit(EntityId(entity)).unwrap().dead);
    assert_eq!(w.rec.counters().other_campaign, 1);
}

/// A mission load clears the agent's memory of rows that left: no late death
/// is written, and the group materializes again.
#[test]
fn a_reload_forgets_the_late_death() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    kill_all_late(&mut w, id, 30.0, Left::Missing);
    ticks(&mut w, 10);
    assert_eq!(w.sim.late_pending(), 3);
    w.apply(&Op::LoadMission).unwrap();
    assert_eq!(w.sim.late_pending(), 0);
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Live);
    assert_eq!(spawns(&w, id), 2);
    assert_eq!(entries(&w, |e| matches!(e, Entry::Death { .. })), 0);
    assert!(!w.store.gate_shut(id), "the marks are of one generation");
}

/// A group lost and held in one generation is spawned again by the mission
/// load that follows, as spec/world.md says.
#[test]
fn a_held_group_materializes_after_a_mission_load() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    for (o, _) in units_of(&w, id) {
        w.sim.remove_with_event(o);
    }
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Lost);
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Live);
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Kill,
    Enumerate,
    LateEvent,
    Detach,
    Reattach,
}

fn permutations(items: &mut Vec<Step>, k: usize, out: &mut Vec<Vec<Step>>) {
    if k == items.len() {
        out.push(items.clone());
        return;
    }
    for i in k..items.len() {
        items.swap(k, i);
        permutations(items, k + 1, out);
        items.swap(k, i);
    }
}

/// Every order of a kill, an enumeration, the late event, a detach and an
/// attach: the group ends `dead`, is never spawned again, and a replay of
/// the recording gives the same death facts, with the store kept.
#[test]
fn every_order_of_kill_enumeration_late_event_detach_and_replay_ends_dead() {
    let mut all = Vec::new();
    permutations(
        &mut vec![
            Step::Kill,
            Step::Enumerate,
            Step::LateEvent,
            Step::Detach,
            Step::Reattach,
        ],
        0,
        &mut all,
    );
    assert_eq!(all.len(), 120);
    for order in all {
        let mut w = world(1, 0, LostPolicy::Respawn);
        w.settle().unwrap();
        let id = first_group(&w);
        let mut killed = false;
        for s in &order {
            match s {
                Step::Kill => {
                    for (i, (o, _)) in units_of(&w, id).into_iter().enumerate() {
                        w.sim.kill_late(o, 2.0 * (i + 1) as f64, Left::Missing);
                    }
                    killed = true;
                }
                Step::Enumerate => ticks(&mut w, 60),
                Step::LateEvent => ticks(&mut w, 80),
                Step::Detach => w.apply(&Op::Detach).unwrap(),
                Step::Reattach => w.apply(&Op::Reattach).unwrap(),
            }
        }
        assert!(killed);
        w.settle()
            .unwrap_or_else(|v| panic!("order {order:?}: {}", v.0));
        assert_eq!(state(&w, id), State::Dead, "order {order:?}");
        assert_eq!(spawns(&w, id), 1, "order {order:?}");

        let want = entries(&w, |e| matches!(e, Entry::Death { .. }));
        assert_eq!(want, 3, "order {order:?}");
        // Replay with the store kept, restarting at the tail.
        let mut store = new_store(1, 0);
        let total = w.recording.records().count();
        fold(
            nanoprobe_sidecar::reconciler::Config::default(),
            &mut store,
            &w.recording,
            &[total / 2],
            Persistence::On,
        );
        let replayed = store
            .entries()
            .iter()
            .filter(|e| matches!(e, Entry::Death { .. }))
            .count();
        assert_eq!(replayed, want, "replay of order {order:?}");
    }
}

/// A kill whose death events come at once (an aircraft, a ship) makes the
/// member dead, and the member is never spawned again.
#[test]
fn a_kill_with_its_events_at_once_is_dead_and_never_spawned_again() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let units = units_of(&w, id);
    w.sim.kill(units[0].0);
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    assert!(w.store.unit(EntityId(units[0].1)).unwrap().dead);
    let spawn = w.sim.ledger().spawns.last().unwrap().clone();
    assert!(!spawn.4.contains(&units[0].1));
    for (o, _) in units_of(&w, id) {
        w.sim.kill(o);
    }
    w.settle().unwrap();
    assert_eq!(state(&w, id), State::Dead);
    assert_eq!(spawns(&w, id), 2);
}

/// An enumeration misses every unit of a group, as when the group's
/// `getUnits` fails. The agent's existence check finds the units alive and
/// writes each with that enumeration's number before `ENUM_END`, so the
/// group stays `live` and bound.
#[test]
fn a_miss_found_alive_before_the_end_keeps_the_group_live_and_bound() {
    let mut w = world(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let id = first_group(&w);
    let units = units_of(&w, id);
    for (o, _) in &units {
        w.sim.hide_from_enumeration(*o);
    }
    ticks(&mut w, 400);
    assert_eq!(state(&w, id), State::Live);
    assert!(!w.store.gate_shut(id));
    assert!(w.rec.held().is_empty());
    assert_eq!(spawns(&w, id), 1);
    for (o, e) in &units {
        let u = w.store.unit(EntityId(*e)).unwrap();
        assert!(!u.missing, "object {o}");
        assert_eq!(u.object, *o);
    }
    assert_eq!(entries(&w, |e| matches!(e, Entry::Left { .. })), 0);
    w.settle().unwrap();
}

/// An enumeration misses every unit of a group and holds its `ENUM_END`
/// while the misses are checked over frames. One missed unit dies, with its
/// death event, during the hold: it leaves as `DEAD`, its miss is dropped,
/// and the end that follows removes nothing else. The group stays `live` and
/// bound, the others alive (spec/records.md, "Layout: ENUM_END").
#[test]
fn a_death_while_enum_end_is_held_is_dead_and_the_rest_stay_live() {
    let mut w = World::with_config(
        1,
        0,
        LostPolicy::Respawn,
        Config {
            event_queue: 10_000,
            enum_end_hold: 20,
            ..Config::default()
        },
    );
    w.settle().unwrap();
    let id = first_group(&w);
    let units = units_of(&w, id);
    assert!(units.len() >= 2, "{units:?}");
    for (o, _) in &units {
        w.sim.hide_from_enumeration(*o);
    }
    let mut frames = 0;
    while !w.sim.enum_end_held() {
        w.tick().unwrap();
        frames += 1;
        assert!(frames < 400, "no enumeration held its end");
    }
    let before = w.rec.counters();
    w.sim.kill(units[0].0);
    // The first held slice took one of the hold's 20 frames. The other 19
    // follow, and the end is written in the frame after them.
    let mut frames = 0;
    while w.sim.enum_end_held() {
        w.tick().unwrap();
        frames += 1;
        assert!(frames <= 21, "the end stayed held");
    }
    assert_eq!(frames, 20, "the end was held {frames} frames");
    ticks(&mut w, 5);
    // The sidecar used the held enumeration; it did not discard it.
    let after = w.rec.counters();
    assert!(after.enumerations > before.enumerations);
    assert_eq!(after.enumerations_discarded, before.enumerations_discarded);
    let dead = w.store.unit(EntityId(units[0].1)).unwrap();
    assert!(dead.dead, "the unit that died during the hold");
    assert!(!dead.missing);
    for (o, e) in &units[1..] {
        let u = w.store.unit(EntityId(*e)).unwrap();
        assert!(!u.dead && !u.missing, "object {o}");
        assert_eq!(u.object, *o);
    }
    assert_eq!(state(&w, id), State::Live);
    assert!(!w.store.gate_shut(id));
    assert_eq!(spawns(&w, id), 1);
    assert_eq!(entries(&w, |e| matches!(e, Entry::Left { .. })), 0);
    for (o, _) in &units {
        w.sim.show_in_enumeration(*o);
    }
    w.settle().unwrap();
}
