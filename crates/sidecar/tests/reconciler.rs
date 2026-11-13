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

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

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
        .sim
        .objects()
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
        .sim
        .objects()
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
    assert!(w.sim.platform_groups().is_empty());
}
