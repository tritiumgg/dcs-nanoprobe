//! The S12 exit test on the fake sim: the reconciler holds its invariants
//! over interleaved deaths, removals, reconnects, sidecar restarts and
//! crashes, DCS crashes and restarts, mission loads, authority moves, order
//! and spec changes, and converges once the faults stop. The invariants are
//! listed in `nanoprobe_sim_fake::harness`.

use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::store::LostPolicy;
use nanoprobe_sim_fake::harness::{Op, Violation, World};
use proptest::prelude::*;

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (1..9u8).prop_map(Op::Frames),
        3 => any::<u16>().prop_map(Op::Kill),
        1 => any::<u16>().prop_map(Op::RemoveSilently),
        1 => any::<u16>().prop_map(Op::RemoveWithEvent),
        1 => Just(Op::Detach),
        1 => Just(Op::Reattach),
        1 => Just(Op::SidecarRestart),
        1 => (0..4u8).prop_map(Op::SidecarCrashInWrite),
        1 => Just(Op::Crash),
        1 => Just(Op::RestartDcs),
        1 => Just(Op::LoadMission),
        2 => (any::<u16>(), any::<bool>()).prop_map(|(i, b)| Op::Authority(i, b)),
        2 => any::<u16>().prop_map(Op::Order),
        1 => any::<u16>().prop_map(Op::BreakSpec),
        1 => any::<u16>().prop_map(Op::FixSpec),
        1 => Just(Op::LoseResult),
        1 => (any::<u16>(), any::<u16>()).prop_map(|(a, t)| Op::Attack(a, t)),
        1 => Just(Op::LoadOtherTheater),
        1 => (1..6u8).prop_map(Op::RefuseDestroys),
        1 => Just(Op::ForeignCampaign),
    ]
}

fn policy() -> impl Strategy<Value = LostPolicy> {
    prop_oneof![
        Just(LostPolicy::Report),
        Just(LostPolicy::Respawn),
        Just(LostPolicy::Dead)
    ]
}

fn run(groups: usize, statics: usize, on_lost: LostPolicy, ops: &[Op]) -> Result<(), String> {
    let mut w = World::new(groups, statics, on_lost);
    for (i, op) in ops.iter().enumerate() {
        w.apply(op)
            .map_err(|Violation(v)| format!("op {i} {op:?}: {v}"))?;
    }
    w.settle().map_err(|Violation(v)| format!("settling: {v}"))
}

/// The number of cases: `PROPTEST_CASES` when set, else the default here.
fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(96), ..ProptestConfig::default() })]

    #[test]
    fn the_reconciler_holds_under_interleaved_faults(
        groups in 1..7usize,
        statics in 0..3usize,
        on_lost in policy(),
        ops in prop::collection::vec(op(), 1..200),
    ) {
        if let Err(e) = run(groups, statics, on_lost, &ops) {
            prop_assert!(false, "{}", e);
        }
    }
}

#[test]
fn with_no_faults_every_group_goes_live_and_takes_its_order() {
    let mut w = World::new(5, 2, LostPolicy::Report);
    for i in 0..5 {
        w.apply(&Op::Order(i)).unwrap();
    }
    w.settle().unwrap();
    for g in w.store.groups() {
        assert_eq!(g.state, State::Live, "{}", g.id);
    }
}

#[test]
fn a_silently_removed_group_is_lost_then_spawned_again_after_a_mission_load() {
    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let units: Vec<u64> = w
        .units_in_dcs()
        .into_iter()
        .filter(|(_, e)| *e != 0)
        .map(|(o, _)| o)
        .collect();
    for o in units {
        w.sim.remove_silently(o);
    }
    w.settle().unwrap();
    assert_eq!(w.store.groups().next().unwrap().state, State::Lost);
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    assert_eq!(w.store.groups().next().unwrap().state, State::Live);
}

#[test]
fn a_group_whose_units_all_die_is_dead_and_never_spawned_again() {
    let mut w = World::new(1, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let units: Vec<u64> = w
        .units_in_dcs()
        .into_iter()
        .filter(|(_, e)| *e != 0)
        .map(|(o, _)| o)
        .collect();
    for o in units {
        w.sim.kill(o);
    }
    w.tick().unwrap();
    w.apply(&Op::RestartDcs).unwrap();
    w.settle().unwrap();
    assert_eq!(w.store.groups().next().unwrap().state, State::Dead);
    assert!(w.sim.platform_groups(w.store.campaign().0).is_empty());
}

/// `lost` -> `dead` by deaths is unreachable under the protocol's ordering:
/// a valid enumeration that finds no row of a group leaves no row to name in
/// a later `OBJECT_GONE`. This drives the reconciler with such records
/// directly, so the row is still checked.
#[test]
fn a_lost_group_whose_members_die_is_dead() {
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_sidecar::sim::wire::{Input, ObjectGone, gone_reason};

    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let units: Vec<(u64, u64)> = w
        .units_in_dcs()
        .into_iter()
        .filter(|(_, e)| *e != 0)
        .collect();
    for (o, _) in &units {
        w.sim.remove_silently(*o);
    }
    w.settle().unwrap();
    let g = w.store.groups().next().unwrap().id;
    assert_eq!(w.store.group(g).unwrap().state, State::Lost);
    for (n, (o, e)) in units.iter().enumerate() {
        let (kind, payload) = Input::ObjectGone(ObjectGone {
            object: *o,
            reason: gone_reason::DEAD,
            entity: *e,
            group_entity: g.0,
        })
        .encode();
        let r = RawRecord {
            agent_boot: w.sim.agent_boot(),
            number: u64::MAX - n as u64,
            kind,
            payload,
        };
        w.rec.record(&mut w.store, &r);
    }
    assert_eq!(w.store.group(g).unwrap().state, State::Dead);
}

/// A death the store cannot tie to the group the record names counts for
/// nothing: a stale or wrong binding never kills a group.
#[test]
fn a_death_with_a_binding_of_another_group_is_ignored() {
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_sidecar::sim::wire::{Input, ObjectGone, gone_reason};

    let mut w = World::new(2, 0, LostPolicy::Report);
    w.settle().unwrap();
    let ids: Vec<_> = w.store.groups().map(|g| g.id).collect();
    let unit = w.store.group(ids[0]).unwrap().members[0];
    let (kind, payload) = Input::ObjectGone(ObjectGone {
        object: 999_999,
        reason: gone_reason::DEAD,
        entity: unit.0,
        group_entity: ids[1].0,
    })
    .encode();
    let before = w.rec.counters().bad_binding;
    w.rec.record(
        &mut w.store,
        &RawRecord {
            agent_boot: w.sim.agent_boot(),
            number: u64::MAX,
            kind,
            payload,
        },
    );
    assert!(!w.store.unit(unit).unwrap().dead);
    assert_eq!(w.rec.counters().bad_binding, before + 1);
}

/// A spawn whose RESULT is lost, then a death of every member before the
/// next HELLO: the group goes `materializing` -> `dead`, and the death shows
/// the spawn ran, so it resolves `applied`.
#[test]
fn a_death_while_the_spawn_is_unanswered_is_dead_and_the_spawn_applied() {
    use nanoprobe_sidecar::store::{Entry, Outcome};

    let mut w = World::new(0, 1, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    for _ in 0..20 {
        if w.rec.inflight().any(|c| c.entity == id) {
            break;
        }
        w.tick().unwrap();
    }
    let spawn = w
        .rec
        .inflight()
        .find(|c| c.entity == id)
        .cloned()
        .expect("a spawn written");
    w.apply(&Op::LoseResult).unwrap();
    assert!(!w.sim.attached(), "the lost result dropped the link");
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("spawned");
    w.sim.kill(object);
    w.apply(&Op::Reattach).unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Dead);
    let journal = w.store.entries();
    assert!(journal.iter().any(|e| matches!(e,
        Entry::Transition { entity, from: State::Materializing, to: State::Dead, .. } if *entity == id)));
    assert!(journal.iter().any(|e| matches!(e,
        Entry::Outcome { epoch, seq, outcome: Outcome::Applied } if (*epoch, *seq) == (spawn.epoch, spawn.seq))));
    w.settle().unwrap();
}

fn commands(w: &World, entity: nanoprobe_sidecar::ids::EntityId, opcode: u16) -> usize {
    use nanoprobe_sidecar::store::Entry;
    w.store
        .entries()
        .iter()
        .filter(|e| matches!(e, Entry::Command(c) if c.entity == entity && c.opcode == opcode))
        .count()
}

/// An `unspawnable` group whose spec is fixed goes `virtual`, and from there
/// materializes because its authority is DCS and its theater is loaded.
#[test]
fn a_fixed_unspawnable_group_does_not_stick_in_virtual() {
    let mut w = World::new(1, 0, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.apply(&Op::BreakSpec(0)).unwrap();
    w.settle().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Unspawnable);
    assert!(w.store.group(id).unwrap().error.is_some());
    w.apply(&Op::FixSpec(0)).unwrap();
    assert!(
        w.store.group(id).unwrap().error.is_none(),
        "the spec change clears the error"
    );
    w.settle().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
}

/// A destroy DCS refuses is sent again up to the limit, then the entity goes
/// `lost` with the error; once DCS accepts destroys it leaves DCS.
#[test]
fn a_refused_destroy_stops_at_the_limit() {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(0, 1, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    w.sim.refuse_destroys(100);
    w.store.set_authority(id, Authority::Campaign).unwrap();
    for _ in 0..30 {
        w.tick().unwrap();
    }
    let g = w.store.group(id).unwrap();
    assert_eq!(g.state, State::Lost);
    assert!(
        g.error
            .as_deref()
            .unwrap()
            .starts_with("destroy failed 3 times"),
        "{:?}",
        g.error
    );
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 3);
    w.settle().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Virtual);
    assert!(w.sim.platform_groups(w.store.campaign().0).is_empty());
}

/// An attack order sent while its target is spawned again in the same
/// generation is compiled again at a new revision and aims at the new DCS
/// group.
#[test]
fn an_attack_order_follows_its_target_when_the_target_respawns() {
    use nanoprobe_sidecar::intents::Intent;

    let mut w = World::new(2, 0, LostPolicy::Respawn);
    w.settle().unwrap();
    let ids: Vec<_> = w.store.groups().map(|g| g.id).collect();
    let (attacker, target) = (ids[0], ids[1]);
    w.store
        .set_order(attacker, Intent::Attack { target })
        .unwrap();
    w.tick().unwrap();
    let rev = w.store.group(attacker).unwrap().order_rev;
    let (old, _) = w.sim.attack_of(w.store.campaign().0, target.0).unwrap();
    for (o, e) in w.units_in_dcs() {
        if w.store
            .unit(nanoprobe_sidecar::ids::EntityId(e))
            .is_some_and(|u| u.group == target)
        {
            w.sim.remove_silently(o);
        }
    }
    w.settle().unwrap();
    let (new, _) = w.sim.attack_of(w.store.campaign().0, target.0).unwrap();
    assert_ne!(old, new, "the target is a new DCS group");
    assert!(w.store.group(attacker).unwrap().order_rev > rev);
    assert_eq!(
        w.sim.attack_of(w.store.campaign().0, attacker.0).unwrap().1,
        Some(new)
    );
}

/// A route's progress moves with the group and never re-issues the order.
#[test]
fn a_route_advances_without_a_new_order() {
    use nanoprobe_sidecar::sim::wire::opcode;

    let mut w = World::new(1, 0, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.apply(&Op::Order(0)).unwrap();
    for _ in 0..10 {
        w.tick().unwrap();
    }
    let sent = commands(&w, id, opcode::GROUP_ORDER);
    assert_eq!(sent, 1);
    let before = w.store.group(id).unwrap().progress;
    for _ in 0..200 {
        w.tick().unwrap();
    }
    assert!(
        w.store.group(id).unwrap().progress > before,
        "the group reached a waypoint"
    );
    assert_eq!(commands(&w, id, opcode::GROUP_ORDER), sent);
}

/// A static that dies while it is dematerializing (its destroy refused, so
/// the state lasts) is `dead`.
#[test]
fn a_death_while_dematerializing_is_dead() {
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(0, 1, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    w.sim.refuse_destroys(1);
    w.store.set_authority(id, Authority::Campaign).unwrap();
    for _ in 0..3 {
        w.tick().unwrap();
    }
    assert_eq!(w.store.group(id).unwrap().state, State::Dematerializing);
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
    w.sim.kill(object);
    w.tick().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Dead);
    w.settle().unwrap();
}

/// A group made `materializing` by a mission load whose authority then moves
/// to the campaign goes `virtual` without a spawn.
#[test]
fn a_materializing_group_under_campaign_authority_is_withdrawn() {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(1, 0, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    let spawns = commands(&w, id, opcode::GROUP_SPAWN);
    w.sim.load_mission();
    w.tick().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Materializing);
    w.store.set_authority(id, Authority::Campaign).unwrap();
    for _ in 0..20 {
        w.tick().unwrap();
    }
    assert_eq!(w.store.group(id).unwrap().state, State::Virtual);
    assert_eq!(
        commands(&w, id, opcode::GROUP_SPAWN),
        spawns,
        "no spawn after the load"
    );
    w.settle().unwrap();
}

/// Round 3: a lost entity whose objects DCS still holds is not respawned by
/// a restarted sidecar before an enumeration has rebuilt the mirror. The
/// sidecar plans right after its HELLO, while the attach's enumeration has
/// only begun.
#[test]
fn a_restarted_sidecar_does_not_respawn_over_objects_dcs_holds() {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(0, 1, LostPolicy::Respawn);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    w.sim.refuse_destroys(5);
    w.store.set_authority(id, Authority::Campaign).unwrap();
    for _ in 0..30 {
        w.tick().unwrap();
        if w.store.group(id).unwrap().state == State::Lost {
            break;
        }
    }
    assert_eq!(w.store.group(id).unwrap().state, State::Lost);
    w.store.set_authority(id, Authority::Sim).unwrap();
    w.apply(&Op::Detach).unwrap();
    let spawns = commands(&w, id, opcode::STATIC_SPAWN);
    // A new sidecar attaches, reads the HELLO and the ENUM_BEGIN, and plans
    // before any frame writes the enumeration's rows.
    w.rec = nanoprobe_sidecar::reconciler::Reconciler::new(
        nanoprobe_sidecar::reconciler::Config {
            on_lost: LostPolicy::Respawn,
            ..Default::default()
        },
        &w.store,
    );
    w.sim.attach(99);
    w.rec.step(&mut w.store, &mut w.sim);
    assert_eq!(
        w.store.group(id).unwrap().state,
        State::Lost,
        "no respawn before an enumeration"
    );
    assert_eq!(commands(&w, id, opcode::STATIC_SPAWN), spawns);
    w.settle().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
}

/// Round 3: a dead entity whose objects DCS still holds and refuses to
/// destroy is sent a destroy only up to the limit until the next enumeration.
#[test]
fn a_dead_entity_s_refused_destroys_stop_at_the_limit() {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(0, 1, LostPolicy::Dead);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    w.sim.refuse_destroys(1_000);
    w.store.set_authority(id, Authority::Campaign).unwrap();
    for _ in 0..30 {
        w.tick().unwrap();
    }
    assert_eq!(w.store.group(id).unwrap().state, State::Dead);
    let sent = commands(&w, id, opcode::STATIC_DESTROY);
    for _ in 0..20 {
        w.tick().unwrap();
    }
    assert_eq!(
        commands(&w, id, opcode::STATIC_DESTROY),
        sent,
        "no destroy between enumerations"
    );
    assert!(sent <= 3 * 2, "{sent}");
}

/// Round 3: a `destroy_retries` of 0 counts as 1, so a destroy is still sent.
#[test]
fn a_destroy_limit_of_zero_counts_as_one() {
    use nanoprobe_sidecar::reconciler::{Config, Reconciler};
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(0, 1, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    w.rec = Reconciler::new(
        Config {
            destroy_retries: 0,
            ..Config::default()
        },
        &w.store,
    );
    w.apply(&Op::Reattach).unwrap();
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    w.sim.refuse_destroys(1);
    w.store.set_authority(id, Authority::Campaign).unwrap();
    for _ in 0..20 {
        w.tick().unwrap();
    }
    assert_eq!(commands(&w, id, opcode::STATIC_DESTROY), 1);
    assert_eq!(w.store.group(id).unwrap().state, State::Lost);
}

/// The coordinator's ruling A: the agent writes a LIFECYCLE of its state
/// right after every HELLO. A store with no mission learns it from that
/// (and is ready); a store that has it adds no journal entry, no
/// generation change and no store revision, and keeps the binding.
#[test]
fn the_lifecycle_after_a_hello_is_quiet_for_a_store_that_has_the_mission() {
    use nanoprobe_sidecar::store::Entry;

    let mut w = World::new(2, 0, LostPolicy::Report);
    w.settle().unwrap();
    let journal = w.store.entries().len();
    let revision = w.store.revision();
    let bound = w.rec.binding().len();
    assert!(bound > 0);
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    let added: Vec<&Entry> = w.store.entries()[journal..].iter().collect();
    assert!(added.is_empty(), "no journal entry: {added:?}");
    assert_eq!(w.store.revision(), revision, "no change for API clients");
    assert!(w.rec.binding().len() >= bound, "the binding is kept");
    assert!(w.rec.ready(&w.store));

    // A sidecar with a fresh store attaching to the running mission.
    let fresh = nanoprobe_sim_fake::harness::new_store(2, 0);
    w.sidecar_of(fresh);
    w.tick().unwrap();
    let loaded = w
        .store
        .loaded()
        .cloned()
        .expect("learned from the stamps and the LIFECYCLE");
    assert_eq!(
        (
            loaded.agent_boot,
            loaded.mission_gen,
            loaded.theater.as_str()
        ),
        (
            w.sim.agent_boot(),
            w.sim.mission_gen(),
            nanoprobe_sim_fake::harness::THEATER
        )
    );
    assert!(w.rec.ready(&w.store));
}

/// Round 4: the re-aim moves only an attack order written in this
/// generation. An attacker under campaign authority, whose order was never
/// sent, keeps its revision when the target respawns.
#[test]
fn an_attack_order_never_sent_keeps_its_revision_when_the_target_respawns() {
    use nanoprobe_sidecar::intents::Intent;
    use nanoprobe_sidecar::store::Authority;

    let mut w = World::new(2, 0, LostPolicy::Respawn);
    let ids: Vec<_> = w.store.groups().map(|g| g.id).collect();
    let (attacker, target) = (ids[0], ids[1]);
    w.store
        .set_authority(attacker, Authority::Campaign)
        .unwrap();
    w.store
        .set_order(attacker, Intent::Attack { target })
        .unwrap();
    w.settle().unwrap();
    let rev = w.store.group(attacker).unwrap().order_rev;
    let (old, _) = w.sim.attack_of(w.store.campaign().0, target.0).unwrap();
    for (o, e) in w.units_in_dcs() {
        if w.store
            .unit(nanoprobe_sidecar::ids::EntityId(e))
            .is_some_and(|u| u.group == target)
        {
            w.sim.remove_silently(o);
        }
    }
    w.settle().unwrap();
    let (new, _) = w.sim.attack_of(w.store.campaign().0, target.0).unwrap();
    assert_ne!(old, new, "the target respawned");
    assert_eq!(w.store.group(attacker).unwrap().order_rev, rev);
}
