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
