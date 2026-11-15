//! The S12 exit test on the fake sim: the reconciler holds its invariants
//! over interleaved deaths, removals, reconnects, sidecar restarts and
//! crashes, DCS crashes and restarts, mission loads, authority moves, order
//! and spec changes, and converges once the faults stop. The invariants are
//! listed in `nanoprobe_sim_fake::harness`.

use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::store::LostPolicy;
use nanoprobe_sim_fake::harness::{CAMPAIGN, Op, Violation, World};
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
        2 => any::<u16>().prop_map(Op::KillRemoved),
        2 => any::<u16>().prop_map(Op::Revive),
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

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(48), ..ProptestConfig::default() })]

    /// Kills draw death delays of up to 83 s. No respawn names a group with a
    /// member that left DCS unexplained in its generation, no spawn names a
    /// member with a death fact, and every reported death is a fact.
    #[test]
    fn the_gate_holds_with_late_deaths_removals_and_revivals(
        seed in any::<u64>(),
        groups in 1..7usize,
        statics in 0..3usize,
        on_lost in policy(),
        ops in prop::collection::vec(op(), 1..200),
    ) {
        let mut w = World::new_late(groups, statics, on_lost, seed);
        for (i, op) in ops.iter().enumerate() {
            if let Err(Violation(v)) = w.apply(op) {
                prop_assert!(false, "op {} {:?}: {}", i, op, v);
            }
        }
        if let Err(Violation(v)) = w.settle() {
            prop_assert!(false, "settling: {}", v);
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
fn a_silently_removed_group_stays_lost_after_a_mission_load() {
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
    // DCS gave no death and no proof of a removal: each member may be dead,
    // so a restore does not re-create them (spec/persistence.md, "Restore").
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    assert_eq!(w.store.groups().next().unwrap().state, State::Lost);
    assert!(w.units_in_dcs().iter().all(|(_, e)| *e == 0));
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

/// `lost` -> `dead` by deaths: the members left as `MISSING`, and their death
/// events come after. This drives the reconciler with the records directly.
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
            resync: false,
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
            resync: false,
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

/// An order that DCS refused because it held none of the group's members is
/// sent again once a member is revived under its object ID, although the
/// group stayed `live`: the revived member brings the DCS group back.
#[test]
fn a_refused_order_is_sent_again_when_a_member_is_revived() {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sidecar::store::{Entry, Outcome};
    use nanoprobe_sim_fake::harness::Op;

    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let id = w.store.groups().next().unwrap().id;
    // All members but one die and the last is removed with an event: the
    // group stays `live` and DCS holds none of its members.
    let objects: Vec<u64> = w
        .sim
        .objects()
        .into_iter()
        .filter(|(_, _, entity)| *entity != 0)
        .map(|(o, _, _)| o)
        .collect();
    let (last, dead) = objects.split_last().unwrap();
    w.sim.remove_with_event(*last);
    w.apply(&Op::Frames(2)).unwrap();
    for o in dead {
        w.sim.kill(*o);
        w.apply(&Op::Frames(2)).unwrap();
    }
    w.apply(&Op::Order(0)).unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
    w.apply(&Op::Frames(4)).unwrap();
    assert_eq!(w.sim.applied_order(id.0), 0);
    assert!(
        w.store.entries().iter().any(|e| matches!(
            e,
            Entry::Outcome {
                outcome: Outcome::Refused,
                ..
            }
        )),
        "DCS held no group, so it refused the order"
    );
    let rev = w.store.group(id).unwrap().order_rev;
    let sent = commands(&w, id, opcode::GROUP_ORDER);
    assert!(w.sim.revive(dead[0]));
    w.apply(&Op::Frames(4)).unwrap();
    assert!(commands(&w, id, opcode::GROUP_ORDER) > sent);
    assert_eq!(w.sim.applied_order(id.0), rev);
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
}

/// A group whose last present member left DCS with no death, its other
/// members dead, then that member found again. `silent` removes it with no
/// event (MISSING after the enumeration), else with an event (REMOVED).
fn refused_then_found(silent: bool) {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sim_fake::harness::Op;

    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let id = w.store.groups().next().unwrap().id;
    let members: Vec<(u64, u64)> = w
        .sim
        .objects()
        .into_iter()
        .filter(|(_, _, entity)| *entity != 0)
        .map(|(o, _, e)| (o, e))
        .collect();
    let (&(last, last_entity), dead) = members.split_last().unwrap();
    for (o, _) in dead {
        w.sim.kill(*o);
        w.apply(&Op::Frames(2)).unwrap();
    }
    if silent {
        w.sim.remove_silently(last);
    } else {
        w.sim.remove_with_event(last);
    }
    let unit = nanoprobe_sidecar::ids::EntityId(last_entity);
    for _ in 0..200 {
        let u = w.store.unit(unit).unwrap();
        if u.missing || u.removed {
            break;
        }
        w.apply(&Op::Frames(4)).unwrap();
    }
    let u = w.store.unit(unit).unwrap();
    assert!(u.missing || u.removed, "the member left unexplained");
    assert!(!u.dead);
    w.apply(&Op::Order(0)).unwrap();
    w.apply(&Op::Frames(4)).unwrap();
    assert_eq!(w.sim.applied_order(id.0), 0, "DCS held no group");
    let rev = w.store.group(id).unwrap().order_rev;
    let sent = commands(&w, id, opcode::GROUP_ORDER);
    w.apply(&Op::Frames(8)).unwrap();
    assert_eq!(
        commands(&w, id, opcode::GROUP_ORDER),
        sent,
        "held while refused"
    );
    assert!(w.sim.revive(last));
    // A MISSING last member leaves the group `lost`; the enumeration that
    // finds it makes it `live` again.
    for _ in 0..200 {
        if commands(&w, id, opcode::GROUP_ORDER) > sent {
            break;
        }
        w.apply(&Op::Frames(4)).unwrap();
    }
    assert!(commands(&w, id, opcode::GROUP_ORDER) > sent);
    w.settle().unwrap();
    assert_eq!(w.sim.applied_order(id.0), rev);
}

/// A refused order is sent again when a MISSING member is found.
#[test]
fn a_refused_order_is_sent_again_when_a_missing_member_is_found() {
    refused_then_found(true);
}

/// A refused order is sent again when a REMOVED member is found.
#[test]
fn a_refused_order_is_sent_again_when_a_removed_member_is_found() {
    refused_then_found(false);
}

/// A member that never left does not send an applied order again across
/// enumerations.
#[test]
fn a_member_never_gone_sends_no_order_again() {
    use nanoprobe_sidecar::sim::wire::opcode;
    use nanoprobe_sim_fake::harness::Op;

    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let id = w.store.groups().next().unwrap().id;
    w.apply(&Op::Order(0)).unwrap();
    w.settle().unwrap();
    let sent = commands(&w, id, opcode::GROUP_ORDER);
    for _ in 0..50 {
        w.apply(&Op::Frames(8)).unwrap();
    }
    assert_eq!(commands(&w, id, opcode::GROUP_ORDER), sent);
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
    // The target leaves DCS and comes back under DCS authority in the same
    // generation: new DCS object IDs. A removal the respawn policy would
    // answer is held by the respawn gate, so the policy is not the way back.
    w.store
        .set_authority(target, nanoprobe_sidecar::store::Authority::Campaign)
        .unwrap();
    w.settle().unwrap();
    w.store
        .set_authority(target, nanoprobe_sidecar::store::Authority::Sim)
        .unwrap();
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

/// The agent writes a LIFECYCLE of its state right after every HELLO (step
/// S10 specifies it). A store with no mission learns it from that
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
    // The target leaves DCS and comes back under DCS authority in the same
    // generation: new DCS object IDs. A removal the respawn policy would
    // answer is held by the respawn gate, so the policy is not the way back.
    w.store
        .set_authority(target, nanoprobe_sidecar::store::Authority::Campaign)
        .unwrap();
    w.settle().unwrap();
    w.store
        .set_authority(target, nanoprobe_sidecar::store::Authority::Sim)
        .unwrap();
    w.settle().unwrap();
    let (new, _) = w.sim.attack_of(w.store.campaign().0, target.0).unwrap();
    assert_ne!(old, new, "the target respawned");
    assert_eq!(w.store.group(attacker).unwrap().order_rev, rev);
}

/// Round 5: a GAP of events may have dropped OBJECT rows, so the mirror may
/// lack objects DCS holds: a lost entity is not respawned until the next
/// enumeration. Here the mirror loses the static's row (as an OBJECT lost
/// to the GAP would leave it), then the GAP arrives, and the sidecar plans.
#[test]
fn a_gap_of_events_holds_a_respawn_until_the_next_enumeration() {
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_sidecar::sim::wire::{Input, ObjectGone, gone_reason, opcode};
    use nanoprobe_sidecar::store::Authority;
    use nanoprobe_transport::layout::gap_class;
    use nanoprobe_transport::records::Gap;

    let mut w = World::new(0, 1, LostPolicy::Respawn);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
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
    let boot = w.sim.agent_boot();
    let record = |input: Input, n: u64| {
        let (kind, payload) = input.encode();
        RawRecord {
            agent_boot: boot,
            number: u64::MAX - n,
            kind,
            payload,
            resync: false,
        }
    };
    let gone = record(
        Input::ObjectGone(ObjectGone {
            object,
            reason: gone_reason::MISSING,
            entity: 0,
            group_entity: 0,
        }),
        1,
    );
    let gap = record(
        Input::Gap(Gap {
            class: gap_class::EVENTS as u32,
            count: 1,
            ..Gap::default()
        }),
        0,
    );
    w.rec.record(&mut w.store, &gone);
    w.rec.record(&mut w.store, &gap);
    let spawns = commands(&w, id, opcode::STATIC_SPAWN);
    w.rec.step(&mut w.store, &mut w.sim);
    assert_eq!(
        commands(&w, id, opcode::STATIC_SPAWN),
        spawns,
        "no respawn before an enumeration"
    );
    w.settle().unwrap();
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
}

/// A row the agent reports gone inside an enumeration, and that an event
/// binds again before ENUM_END, is not missing at ENUM_END (spec/records.md,
/// "Layout: ENUM_END"): the mirror agrees with the agent's table.
#[test]
fn a_row_gone_and_bound_again_inside_an_enumeration_is_not_missing() {
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_sidecar::sim::wire::{Enumeration, Input, ObjectGone, gone_reason};
    use nanoprobe_transport::records::Object;

    let mut w = World::new(0, 1, LostPolicy::Respawn);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    let (object, entity) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
    let boot = w.sim.agent_boot();
    let record = |input: Input, n: u64| {
        let (kind, payload) = input.encode();
        RawRecord {
            agent_boot: boot,
            number: u64::MAX - n,
            kind,
            payload,
            resync: false,
        }
    };
    let begin = record(
        Input::EnumBegin(Enumeration {
            enumeration: 900,
            objects: 0,
            seen: 0,
        }),
        4,
    );
    let gone = record(
        Input::ObjectGone(ObjectGone {
            object,
            reason: gone_reason::MISSING,
            entity: 0,
            group_entity: 0,
        }),
        3,
    );
    let again = record(
        Input::Object(Object {
            object,
            entity,
            enumeration: 0,
            ..Object::default()
        }),
        2,
    );
    let end = record(
        Input::EnumEnd(Enumeration {
            enumeration: 900,
            objects: 0,
            seen: 0,
        }),
        1,
    );
    for r in [&begin, &gone, &again, &end] {
        w.rec.record(&mut w.store, r);
    }
    let unit = w.store.group(id).unwrap().members[0];
    assert!(!w.store.unit(unit).unwrap().missing);
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
}

/// What the `SEEN` tests feed the reconciler: records of the agent boot the
/// fake sim runs, applied in order.
struct Feed {
    boot: u64,
    next: u64,
}

impl Feed {
    fn new(w: &World) -> Feed {
        Feed {
            boot: w.sim.agent_boot(),
            next: u64::MAX - 1_000,
        }
    }

    fn apply(&mut self, w: &mut World, input: nanoprobe_sidecar::sim::wire::Input) {
        let (kind, payload) = input.encode();
        self.next += 1;
        let r = nanoprobe_sidecar::sim::RawRecord {
            agent_boot: self.boot,
            number: self.next,
            kind,
            payload,
            resync: false,
        };
        w.rec.record(&mut w.store, &r);
    }

    fn begin(&mut self, w: &mut World, n: u64) {
        use nanoprobe_sidecar::sim::wire::{Enumeration, Input};
        self.apply(
            w,
            Input::EnumBegin(Enumeration {
                enumeration: n,
                ..Enumeration::default()
            }),
        );
    }

    fn seen(&mut self, w: &mut World, n: u64, ids: &[u64]) {
        use nanoprobe_sidecar::sim::wire::{Input, Seen};
        self.apply(
            w,
            Input::Seen(Seen {
                enumeration: n,
                objects: ids.to_vec(),
            }),
        );
    }

    fn end(&mut self, w: &mut World, n: u64, objects: u32, seen: u32) {
        use nanoprobe_sidecar::sim::wire::{Enumeration, Input};
        self.apply(
            w,
            Input::EnumEnd(Enumeration {
                enumeration: n,
                objects,
                seen,
            }),
        );
    }

    fn gap(&mut self, w: &mut World) {
        use nanoprobe_sidecar::sim::wire::Input;
        self.apply(
            w,
            Input::Gap(nanoprobe_transport::records::Gap {
                class: nanoprobe_transport::layout::gap_class::EVENTS as u32,
                count: 1,
                ..Default::default()
            }),
        );
    }
}

/// A settled world with one static (an entity with one member), and that
/// member's DCS object.
fn one_group() -> (World, nanoprobe_sidecar::ids::EntityId, u64) {
    let mut w = World::new(0, 1, LostPolicy::Report);
    let id = w.store.groups().next().unwrap().id;
    w.settle().unwrap();
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .expect("in DCS");
    (w, id, object)
}

/// A `SEEN` ID the reconciler binds counts as found: the member is not
/// missing at `ENUM_END`. With no ID, the enumeration finds nothing.
#[test]
fn a_seen_id_counts_as_found_at_enum_end() {
    let (mut w, id, object) = one_group();
    let unit = w.store.group(id).unwrap().members[0];
    let used = w.rec.counters().enumerations;
    let mut f = Feed::new(&w);
    f.begin(&mut w, 900);
    f.seen(&mut w, 900, &[object, object + 1_000_000]);
    f.end(&mut w, 900, 0, 2);
    assert_eq!(w.rec.counters().enumerations, used + 1);
    assert!(!w.store.unit(unit).unwrap().missing);
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
    // An enumeration that names no ID does not find the member.
    f.begin(&mut w, 901);
    f.end(&mut w, 901, 0, 0);
    assert!(w.store.unit(unit).unwrap().missing);
}

/// `objects` and `seen` are compared with `ENUM_END` separately; a mismatch
/// discards the enumeration and asks for another.
#[test]
fn a_seen_count_that_differs_discards_the_enumeration() {
    let (mut w, _, object) = one_group();
    let before = (
        w.rec.counters().enumerations,
        w.rec.counters().enumerations_discarded,
    );
    let mut f = Feed::new(&w);
    f.begin(&mut w, 900);
    f.seen(&mut w, 900, &[object]);
    f.end(&mut w, 900, 1, 0);
    f.begin(&mut w, 901);
    f.seen(&mut w, 901, &[object]);
    f.end(&mut w, 901, 0, 2);
    // A `SEEN` of another enumeration counts for neither.
    f.begin(&mut w, 902);
    f.seen(&mut w, 899, &[object]);
    f.end(&mut w, 902, 0, 1);
    assert_eq!(
        (
            w.rec.counters().enumerations,
            w.rec.counters().enumerations_discarded
        ),
        (before.0, before.1 + 3)
    );
}

/// After a `GAP` of events an enumeration with `SEEN` IDs is discarded, until
/// one with none is used.
#[test]
fn after_a_gap_of_events_an_enumeration_with_seen_ids_is_discarded() {
    let (mut w, _, object) = one_group();
    let mut f = Feed::new(&w);
    f.gap(&mut w);
    let (used, discarded) = (
        w.rec.counters().enumerations,
        w.rec.counters().enumerations_discarded,
    );
    f.begin(&mut w, 900);
    f.seen(&mut w, 900, &[object]);
    f.end(&mut w, 900, 0, 1);
    assert_eq!(w.rec.counters().enumerations_discarded, discarded + 1);
    assert_eq!(w.rec.counters().enumerations, used);
    // A whole enumeration is used, and lifts the guard.
    f.begin(&mut w, 901);
    f.end(&mut w, 901, 0, 0);
    assert_eq!(w.rec.counters().enumerations, used + 1);
    // Its `OBJECT` records are the agent's; the next one may use `SEEN`.
    f.begin(&mut w, 902);
    f.seen(&mut w, 902, &[object]);
    f.end(&mut w, 902, 0, 1);
    assert_eq!(w.rec.counters().enumerations, used + 2);
}

/// A `SEEN` stamped under another campaign marks the run's gap, as an
/// `OBJECT` does.
#[test]
fn a_seen_under_another_campaign_discards_the_enumeration() {
    use nanoprobe_sidecar::sim::wire::Input;
    use nanoprobe_transport::records::Frame;
    let (mut w, _, object) = one_group();
    let mut f = Feed::new(&w);
    let loaded = w.store.loaded().cloned().expect("loaded");
    let campaign = w.store.campaign().0;
    let used = w.rec.counters().enumerations;
    f.begin(&mut w, 900);
    f.apply(
        &mut w,
        Input::Frame(Frame {
            mission_gen: loaded.mission_gen,
            campaign: campaign + 1,
            ..Frame::default()
        }),
    );
    f.seen(&mut w, 900, &[object]);
    f.end(&mut w, 900, 0, 1);
    assert_eq!(w.rec.counters().enumerations, used);
}

/// An `OBJECT` for `object` that names `entity`, in enumeration `n`.
fn object_for(f: &mut Feed, w: &mut World, n: u64, object: u64, entity: u64) {
    use nanoprobe_sidecar::sim::wire::{Input, Object};
    f.apply(
        w,
        Input::Object(Object {
            object,
            entity,
            enumeration: n,
            ..Object::default()
        }),
    );
}

/// An object whose `OBJECT` names a unit the store never gains stays in DCS
/// over many periodic enumerations: none of its `SEEN` records asks for an
/// `ENUMERATE`, because a repeated `OBJECT` cannot resolve it. An ID the
/// reconciler does not manage asks for nothing either, and no enumeration is
/// discarded.
#[test]
fn an_object_that_cannot_resolve_does_not_ask_for_enumerations() {
    let (mut w, _, object) = one_group();
    let mut f = Feed::new(&w);
    let (ghost, stranger) = (777_000u64, 888_000u64);
    let used = w.rec.counters().enumerations;
    let asked = w.rec.counters().enumerate_requests;
    f.begin(&mut w, 900);
    object_for(&mut f, &mut w, 900, ghost, 9_999_999);
    f.seen(&mut w, 900, &[object]);
    f.end(&mut w, 900, 1, 1);
    for n in 901..904 {
        f.begin(&mut w, n);
        f.seen(&mut w, n, &[object, ghost, stranger]);
        f.end(&mut w, n, 0, 3);
        w.tick().unwrap();
    }
    let c = w.rec.counters();
    assert!(c.enumerations >= used + 4, "every enumeration is used");
    assert_eq!(c.enumerate_requests, asked);
}

/// Once the store holds the unit an unresolved object named, its `SEEN` asks
/// for one `ENUMERATE`, not one for every periodic enumeration after it.
#[test]
fn an_unresolved_object_asks_once_when_its_unit_appears() {
    use nanoprobe_sidecar::ids::EntityId;
    use nanoprobe_sidecar::store::{Authority, NewGroup, NewUnit, UnitState};
    let (mut w, id, object) = one_group();
    let top = w
        .store
        .group(id)
        .unwrap()
        .members
        .iter()
        .map(|u| u.0)
        .max()
        .unwrap()
        .max(id.0);
    let mut f = Feed::new(&w);
    let ghost = 777_000u64;
    // A static's unit is its entity: the next static takes the next ID.
    let future = top + 1;
    f.begin(&mut w, 900);
    object_for(&mut f, &mut w, 900, ghost, future);
    f.seen(&mut w, 900, &[object]);
    f.end(&mut w, 900, 1, 1);
    // The unit appears in the store.
    let spec = NewGroup {
        theater: w.store.loaded().unwrap().theater.clone(),
        country: 2,
        category: 0,
        coalition: 2,
        units: vec![NewUnit::new("Tent", UnitState::default())],
        authority: Authority::Sim,
        label: "late".into(),
        extra: nanoprobe_sidecar::sim::wire::lv::obj(vec![]),
    };
    let late = w.store.create_static(spec).unwrap();
    assert_eq!(w.store.group(late).unwrap().members[0], EntityId(future));
    let asked = w.rec.counters().enumerate_requests;
    for n in 901..904 {
        f.begin(&mut w, n);
        f.seen(&mut w, n, &[object, ghost]);
        f.end(&mut w, n, 0, 2);
        w.tick().unwrap();
    }
    assert_eq!(w.rec.counters().enumerate_requests, asked + 1);
}

/// A GROUP_ORDER written but not yet taken, and a HELLO of `agent_boot`
/// whose `in_head` lies before it; the outcomes journalled for it.
fn order_then_hello(
    agent_boot_of: impl Fn(&World) -> u64,
    resync: bool,
) -> (World, Vec<nanoprobe_sidecar::store::Outcome>) {
    use nanoprobe_sidecar::intents::{Intent, Waypoint};
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_sidecar::sim::wire::{Input, opcode};
    use nanoprobe_sidecar::store::Entry;
    use nanoprobe_transport::records::Hello;

    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let g = w.store.groups().next().unwrap().id;
    w.store
        .set_order(
            g,
            Intent::Route {
                waypoints: vec![Waypoint { x: 1.0, z: 2.0 }],
                speed: 10.0,
                on_road: false,
            },
        )
        .unwrap();
    // The reconciler writes the order; the fake DCS runs no frame, so the
    // agent does not take it.
    w.rec.step(&mut w.store, &mut w.sim);
    let c = w
        .rec
        .inflight()
        .find(|c| c.opcode == opcode::GROUP_ORDER)
        .cloned()
        .expect("the order is in flight");
    let hello = Hello {
        agent_boot: agent_boot_of(&w),
        sidecar_boot: 1,
        campaign: w.store.campaign().0,
        epoch: c.epoch,
        last_applied_seq: c.seq - 1,
        in_head: c.position - 1,
        ..Hello::default()
    };
    let (kind, payload) = Input::Hello(hello).encode();
    let r = RawRecord {
        agent_boot: agent_boot_of(&w),
        number: u64::MAX,
        kind,
        payload,
        resync,
    };
    w.rec.record(&mut w.store, &r);
    let outcomes = w
        .store
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Outcome {
                epoch,
                seq,
                outcome,
                ..
            } if (*epoch, *seq) == (c.epoch, c.seq) => Some(*outcome),
            _ => None,
        })
        .collect();
    (w, outcomes)
}

/// A resync HELLO after CONTROL_LOST leaves the ring untouched: a command
/// after its `in_head` stays pending, and is not derived again.
#[test]
fn a_resync_hello_leaves_a_command_after_in_head_pending() {
    use nanoprobe_sidecar::store::Outcome;

    let (w, outcomes) = order_then_hello(|w| w.sim.agent_boot(), true);
    assert!(outcomes.is_empty(), "{outcomes:?}");
    assert!(w.rec.inflight().any(|c| c.opcode == 17));
    let (_, outcomes) = order_then_hello(|w| w.sim.agent_boot(), false);
    assert_eq!(
        outcomes,
        vec![Outcome::NotSent],
        "a new attach: never taken"
    );
}

/// A command of an agent boot that ended resolves `unknown` at the first
/// HELLO of a new boot, though it lies past that boot's last `in_head`
/// (spec/commands.md, "After DCS dies").
#[test]
fn a_dead_boot_s_command_past_its_in_head_is_unknown() {
    use nanoprobe_sidecar::store::Outcome;

    let (_, outcomes) = order_then_hello(|w| w.sim.agent_boot() + 1, false);
    assert_eq!(outcomes, vec![Outcome::Unknown]);
}

mod extension_answers {
    use super::*;
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_sidecar::sim::wire::{Input, opcode};
    use nanoprobe_sidecar::store::{Entry, Outcome};
    use nanoprobe_transport::codec::Lv;
    use nanoprobe_transport::layout::{lifecycle as lc, result as rc};
    use nanoprobe_transport::records::{CommandResult, Hello, Lifecycle};

    fn feed(w: &mut World, input: Input) {
        let (kind, payload) = input.encode();
        let r = RawRecord {
            agent_boot: w.sim.agent_boot(),
            number: u64::MAX,
            kind,
            payload,
            resync: false,
        };
        w.rec.record(&mut w.store, &r);
    }

    fn sets(w: &World) -> usize {
        w.store
            .entries()
            .iter()
            .filter(|e| matches!(e, Entry::Command(c) if c.opcode == opcode::EXTENSION_SET))
            .count()
    }

    fn outcomes(w: &World) -> Vec<Outcome> {
        w.store
            .entries()
            .iter()
            .filter_map(|e| match e {
                Entry::Outcome { outcome, .. } => Some(*outcome),
                _ => None,
            })
            .collect()
    }

    /// A settled world, and its extension `x` written once. The fake DCS
    /// runs no frame, so the agent never takes the command.
    fn world() -> World {
        let mut w = World::new(1, 0, LostPolicy::Report);
        w.settle().unwrap();
        w.store.set_extension("x", "return 1");
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 1);
        w
    }

    /// Answer the `EXTENSION_SET` in flight with `code`.
    fn answer(w: &mut World, code: u64) {
        let c = w
            .rec
            .inflight()
            .find(|c| c.opcode == opcode::EXTENSION_SET)
            .cloned()
            .expect("an EXTENSION_SET is in flight");
        feed(
            w,
            Input::Result(CommandResult {
                seq: c.seq,
                epoch: c.epoch,
                campaign: w.store.campaign().0,
                entity: 0,
                rev: c.rev,
                object: 0,
                cost_us: 0,
                opcode: opcode::EXTENSION_SET,
                code: code as u16,
                message: String::new(),
                value: Lv::Nil,
            }),
        );
    }

    fn lifecycle(w: &mut World, state: u64) {
        let l = w.store.loaded().cloned().unwrap();
        feed(
            w,
            Input::Lifecycle(Lifecycle {
                state: state as u32,
                mission_gen: l.mission_gen,
                theater: l.theater,
                ..Lifecycle::default()
            }),
        );
    }

    #[test]
    fn a_refused_revision_is_not_sent_again_until_the_source_changes() {
        let mut w = world();
        answer(&mut w, rc::LUA_ERROR);
        assert_eq!(outcomes(&w).last(), Some(&Outcome::Refused));
        for _ in 0..3 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 1);
        assert_eq!(w.store.set_extension("x", "return 2"), 2);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 2, "the next revision is sent");
    }

    #[test]
    fn a_rejected_revision_is_not_sent_again() {
        let mut w = world();
        answer(&mut w, rc::BAD_PAYLOAD);
        assert_eq!(outcomes(&w).last(), Some(&Outcome::Rejected));
        for _ in 0..3 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 1);
    }

    #[test]
    fn a_stale_epoch_answer_derives_the_revision_again() {
        let mut w = world();
        answer(&mut w, rc::STALE_EPOCH);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 2);
        let c = w.rec.inflight().last().cloned().unwrap();
        assert_eq!(c.rev, 1);
    }

    #[test]
    fn a_not_ready_answer_is_sent_again_once_per_change() {
        let mut w = world();
        answer(&mut w, rc::NOT_READY);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 1, "the lifecycle did not change");
        // A new lifecycle state, then back to RUNNING: one resend each.
        lifecycle(&mut w, lc::PAUSED);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 2);
        answer(&mut w, rc::NOT_READY);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 2);
        lifecycle(&mut w, lc::RUNNING);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 3);
        // A new revision is sent at once.
        answer(&mut w, rc::NOT_READY);
        w.store.set_extension("x", "return 2");
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 4);
    }

    #[test]
    fn a_not_ready_revision_is_sent_again_after_a_restart_to_the_same_state() {
        let mut w = world();
        answer(&mut w, rc::NOT_READY);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 1);
        // The plan sees no state between: the mission restarts and runs.
        for state in [lc::STOPPING, lc::LOADING, lc::RUNNING] {
            lifecycle(&mut w, state);
        }
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(sets(&w), 2, "one resend");
    }

    /// Answer the command in flight for `opcode` with `NOT_READY`, and
    /// count the commands written for it.
    fn answer_not_ready(w: &mut World, op: u16) {
        let c = w
            .rec
            .inflight()
            .find(|c| c.opcode == op)
            .cloned()
            .expect("the command is in flight");
        feed(
            w,
            Input::Result(CommandResult {
                seq: c.seq,
                epoch: c.epoch,
                campaign: w.store.campaign().0,
                entity: c.entity.0,
                rev: c.rev,
                object: 0,
                cost_us: 0,
                opcode: op,
                code: rc::NOT_READY as u16,
                message: String::new(),
                value: Lv::Nil,
            }),
        );
    }

    fn count(w: &World, op: u16) -> usize {
        w.store
            .entries()
            .iter()
            .filter(|e| matches!(e, Entry::Command(c) if c.opcode == op))
            .count()
    }

    #[test]
    fn a_not_ready_spawn_is_sent_once_per_lifecycle_change() {
        let mut w = World::new(1, 0, LostPolicy::Report);
        w.settle().unwrap();
        w.sim.load_mission();
        for _ in 0..60 {
            w.tick().unwrap();
            if w.rec.inflight().any(|c| c.opcode == opcode::GROUP_SPAWN) {
                break;
            }
        }
        let spawns = count(&w, opcode::GROUP_SPAWN);
        answer_not_ready(&mut w, opcode::GROUP_SPAWN);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(count(&w, opcode::GROUP_SPAWN), spawns, "held");
        for state in [lc::STOPPING, lc::LOADING, lc::RUNNING] {
            lifecycle(&mut w, state);
        }
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(count(&w, opcode::GROUP_SPAWN), spawns + 1, "one resend");
    }

    #[test]
    fn a_not_ready_order_is_sent_once_per_lifecycle_change() {
        use nanoprobe_sidecar::intents::{Intent, Waypoint};
        let mut w = World::new(1, 0, LostPolicy::Report);
        w.settle().unwrap();
        let g = w.store.groups().next().unwrap().id;
        let route = Intent::Route {
            waypoints: vec![Waypoint { x: 1.0, z: 2.0 }],
            speed: 10.0,
            on_road: false,
        };
        w.store.set_order(g, route).unwrap();
        w.rec.step(&mut w.store, &mut w.sim);
        let orders = count(&w, opcode::GROUP_ORDER);
        assert!(orders > 0);
        answer_not_ready(&mut w, opcode::GROUP_ORDER);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(count(&w, opcode::GROUP_ORDER), orders, "held");
        for state in [lc::STOPPING, lc::LOADING, lc::RUNNING] {
            lifecycle(&mut w, state);
        }
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(count(&w, opcode::GROUP_ORDER), orders + 1, "one resend");
    }

    #[test]
    fn a_hello_lets_a_not_ready_revision_go_again() {
        let mut w = world();
        answer(&mut w, rc::NOT_READY);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 1);
        let hello = Hello {
            agent_boot: w.sim.agent_boot(),
            sidecar_boot: 1,
            campaign: w.store.campaign().0,
            epoch: 1,
            in_head: u64::MAX,
            ..Hello::default()
        };
        feed(&mut w, Input::Hello(hello));
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 2);
    }

    /// Read `n` FRAMEs of the running mission: no lifecycle change.
    fn frames(w: &mut World, n: u64) {
        let l = w.store.loaded().cloned().unwrap();
        for _ in 0..n {
            feed(
                w,
                Input::Frame(nanoprobe_transport::records::Frame {
                    mission_gen: l.mission_gen,
                    campaign: w.store.campaign().0,
                    lifecycle: lc::RUNNING as _,
                    ..Default::default()
                }),
            );
        }
    }

    /// A NOT_READY in RUNNING that no LIFECYCLE follows is held for
    /// `NOT_READY_HOLD_FRAMES` FRAMEs, then sent again.
    #[test]
    fn a_not_ready_without_a_lifecycle_change_is_sent_again_after_the_bound() {
        use nanoprobe_sidecar::reconciler::NOT_READY_HOLD_FRAMES;
        let mut w = world();
        answer(&mut w, rc::NOT_READY);
        frames(&mut w, NOT_READY_HOLD_FRAMES - 1);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 1, "held inside the bound");
        frames(&mut w, 1);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 2, "sent again once the bound passes");
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(sets(&w), 2, "once, not every pass");
    }

    /// A capture and a destroy answered NOT_READY are each sent once until
    /// the bound passes, a lifecycle change or a HELLO.
    #[test]
    fn a_not_ready_capture_and_destroy_are_held() {
        use nanoprobe_sidecar::reconciler::NOT_READY_HOLD_FRAMES;
        use nanoprobe_sidecar::store::Authority;
        let mut w = World::new(0, 1, LostPolicy::Report);
        let id = w.store.groups().next().unwrap().id;
        w.settle().unwrap();
        w.store.set_authority(id, Authority::Campaign).unwrap();
        w.rec.step(&mut w.store, &mut w.sim);
        let cap = count(&w, opcode::CAPTURE);
        assert_eq!(cap, 1);
        answer_not_ready(&mut w, opcode::CAPTURE);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(count(&w, opcode::CAPTURE), cap, "the capture is held");
        frames(&mut w, NOT_READY_HOLD_FRAMES);
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(count(&w, opcode::CAPTURE), cap + 1, "sent after the bound");

        // The capture settled: the destroy follows.
        w.store.set_captured(id).unwrap();
        answer_not_ready(&mut w, opcode::CAPTURE);
        w.rec.step(&mut w.store, &mut w.sim);
        let d = count(&w, opcode::STATIC_DESTROY);
        assert_eq!(d, 1);
        answer_not_ready(&mut w, opcode::STATIC_DESTROY);
        for _ in 0..5 {
            w.rec.step(&mut w.store, &mut w.sim);
        }
        assert_eq!(count(&w, opcode::STATIC_DESTROY), d, "the destroy is held");
        for state in [lc::STOPPING, lc::LOADING, lc::RUNNING] {
            lifecycle(&mut w, state);
        }
        w.rec.step(&mut w.store, &mut w.sim);
        assert_eq!(count(&w, opcode::STATIC_DESTROY), d + 1, "one resend");
    }
}

/// Regression: kills while another campaign held the agent, then a DCS
/// restart. The members' OBJECT_GONE records were lost with the restart, so
/// their marks are inferred at an enumeration's end, not journalled, and
/// clear at the new generation: the restart spawns them again. That is the
/// cost the S18 ruling accepts (an inferred mark can name a member DCS never
/// bound). The schedule must still settle with every invariant held.
#[test]
fn kills_lost_with_a_dcs_restart_settle_under_the_inferred_mark_rule() {
    let ops = [
        Op::Order(0),
        Op::KillRemoved(0),
        Op::Frames(1),
        Op::Kill(0),
        Op::ForeignCampaign,
        Op::Kill(423),
        Op::Reattach,
        Op::Kill(21352),
        Op::Kill(1272),
        Op::FixSpec(37),
        Op::RestartDcs,
        Op::Authority(37099, false),
        Op::RefuseDestroys(5),
        Op::Kill(29656),
        Op::Kill(38785),
        Op::Kill(56337),
    ];
    let mut w = World::new(2, 2, LostPolicy::Report);
    let first_boot = w.sim.agent_boot();
    for op in &ops {
        w.apply(op).unwrap();
    }
    w.settle().unwrap();
    // Members 2 and 3 of group 1 died in DCS before the restart, and the
    // restart spawned them again: their marks were inferred, not reported.
    let deaths = &w.sim.ledger().deaths;
    assert!(deaths.contains(&(CAMPAIGN, 2)) && deaths.contains(&(CAMPAIGN, 3)));
    assert!(
        w.sim
            .ledger()
            .spawns
            .iter()
            .any(|s| s.0 > first_boot && s.2 == 1 && s.4.contains(&2) && s.4.contains(&3))
    );
    assert!(!w.store.held(nanoprobe_sidecar::ids::EntityId(3)));
}

/// Naming step (spec/world.md, "Names"): after a sidecar restart, with the
/// agent's binding gone, the rows bind again by the campaign tag in their
/// platform names alone, every group stays live and nothing is spawned again.
/// A row of another tag stays unbound and is `foreign_name`; a row of a
/// never-seen campaign with the same tag binds, which is the known limit.
#[test]
fn a_restarted_sidecar_rebinds_from_names_alone() {
    use nanoprobe_sidecar::sim::wire::opcode;

    use nanoprobe_sidecar::ids::{CampaignId, tag};

    let mut w = World::new(2, 1, LostPolicy::Respawn);
    w.settle().unwrap();
    let ids: Vec<_> = w.store.groups().map(|g| g.id).collect();
    let campaign = w.store.campaign().0;
    // In DCS too: a group of another campaign, whose tag differs, and one of
    // a campaign never seen that shares the active campaign's tag (the known
    // limit). Entities far above the store's keep them apart from its own.
    let other = campaign + 1;
    assert_ne!(tag(CampaignId(other)), tag(CampaignId(campaign)));
    let mut twin = campaign + 2;
    while tag(CampaignId(twin)) != tag(CampaignId(campaign)) {
        twin += 1;
    }
    let before_objects: Vec<u64> = w.sim.objects().iter().map(|o| o.0).collect();
    w.sim.add_campaign_group(other, 9_001, &[9_002]);
    let with_other: Vec<u64> = w.sim.objects().iter().map(|o| o.0).collect();
    w.sim.add_campaign_group(twin, 9_101, &[9_102]);
    let new_of = |from: &[u64], to: &[u64]| -> Vec<u64> {
        to.iter().filter(|o| !from.contains(o)).copied().collect()
    };
    let other_rows = new_of(&before_objects, &with_other);
    let twin_rows = new_of(
        &with_other,
        &w.sim.objects().iter().map(|o| o.0).collect::<Vec<_>>(),
    );
    let spawned = |w: &World, id| {
        commands(w, id, opcode::GROUP_SPAWN) + commands(w, id, opcode::STATIC_SPAWN)
    };
    let before: Vec<_> = ids.iter().map(|&id| spawned(&w, id)).collect();
    w.apply(&Op::Detach).unwrap();
    // A switch away and back discards the agent's binding: a row binds again
    // only when its name carries the campaign's tag.
    w.sim.set_campaign(campaign + 1);
    w.sim.set_campaign(campaign);
    w.rec = nanoprobe_sidecar::reconciler::Reconciler::new(
        nanoprobe_sidecar::reconciler::Config {
            on_lost: LostPolicy::Respawn,
            ..Default::default()
        },
        &w.store,
    );
    w.sim.attach(99);
    w.settle().unwrap();
    for (i, &id) in ids.iter().enumerate() {
        assert_eq!(
            w.store.group(id).unwrap().state,
            State::Live,
            "group {id:?}"
        );
        assert_eq!(spawned(&w, id), before[i], "group {id:?} was spawned again");
    }
    // Every row of the store's entities is bound again, by its name.
    for g in w.store.groups() {
        for m in &g.members {
            let row = w
                .sim
                .objects()
                .into_iter()
                .find(|&(_, c, e)| c == campaign && e == m.0)
                .expect("a row of the unit");
            assert_eq!(w.sim.binding(row.0).0, m.0, "unit {m:?}");
        }
    }
    // The other campaign's row stays unbound and is reported foreign.
    for o in &other_rows {
        assert_eq!(w.sim.binding(*o), (0, 0));
    }
    assert!(w.rec.counters().foreign > 0);
    // The never-seen campaign with the same tag binds as the active one's.
    for o in &twin_rows {
        assert_eq!(w.sim.binding(*o), (9_102, 9_101));
    }
}

/// A menu pinned to an adopted group is removed when a mission load ends the
/// adoption, with its children, and the removal is journalled.
#[test]
fn markers_of_an_adopted_group_are_removed_when_a_mission_load_ends_the_adoption() {
    use nanoprobe_sidecar::ids::EntityId;
    use nanoprobe_sidecar::players::Unmanaged;
    use nanoprobe_sidecar::store::{MarkerSpec, scope};

    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let (_unit, group) = w
        .rec
        .adopt(
            &mut w.store,
            &mut w.sim,
            &Unmanaged {
                object: 90_001,
                group: 90_002,
                entity: 0,
                group_entity: 0,
            },
        )
        .expect("adopted");
    let menu = |parent: EntityId, submenu: bool| MarkerSpec::MenuItem {
        parent,
        submenu,
        text: "m".into(),
    };
    let (sub, _) = w
        .store
        .put_marker(EntityId(0), scope::GROUP, group, menu(EntityId(0), true))
        .unwrap();
    let (child, _) = w
        .store
        .put_marker(EntityId(0), scope::GROUP, group, menu(sub, false))
        .unwrap();
    let (other, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(EntityId(0), false))
        .unwrap();
    w.tick().unwrap();
    assert!(!w.store.marker(sub).unwrap().removed);

    w.sim.load_mission();
    w.tick().unwrap();
    w.tick().unwrap();
    assert!(!w.rec.is_adopted(EntityId(group)));
    assert!(w.store.marker(sub).unwrap().removed);
    assert!(w.store.marker(child).unwrap().removed);
    assert!(!w.store.marker(other).unwrap().removed, "other scopes stay");
}

/// A rebuilt reconciler (a sidecar restart) removes the markers of an adopted
/// group whose adoption did not survive, and keeps those of one the agent
/// still binds.
#[test]
fn a_rebuilt_reconciler_removes_the_markers_of_an_adoption_that_ended() {
    use nanoprobe_sidecar::ids::EntityId;
    use nanoprobe_sidecar::players::Unmanaged;
    use nanoprobe_sidecar::store::{MarkerSpec, scope};

    for survives in [false, true] {
        let mut w = World::new(1, 0, LostPolicy::Report);
        w.settle().unwrap();
        let object = w.sim.birth_client_unit_in("m1", 90_002, "F-16C_50");
        w.settle().unwrap();
        let (_unit, group) = w
            .rec
            .adopt(
                &mut w.store,
                &mut w.sim,
                &Unmanaged {
                    object,
                    group: 90_002,
                    entity: 0,
                    group_entity: 0,
                },
            )
            .expect("adopted");
        let (menu, _) = w
            .store
            .put_marker(
                EntityId(0),
                scope::GROUP,
                group,
                MarkerSpec::MenuItem {
                    parent: EntityId(0),
                    submenu: true,
                    text: "m".into(),
                },
            )
            .unwrap();
        // The first step's commit lets the `BIND`s through; the agent
        // applies them at the next frame.
        w.tick().unwrap();
        w.tick().unwrap();
        assert!(!w.store.marker(menu).unwrap().removed);

        w.rec = nanoprobe_sidecar::reconciler::Reconciler::new(
            nanoprobe_sidecar::reconciler::Config {
                on_lost: LostPolicy::Report,
                ..Default::default()
            },
            &w.store,
        );
        if !survives {
            w.sim.load_mission();
        }
        w.settle().unwrap();
        assert_eq!(
            w.store.marker(menu).unwrap().removed,
            !survives,
            "survives {survives}"
        );
    }
}
