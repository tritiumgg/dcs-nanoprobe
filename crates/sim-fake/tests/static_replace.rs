//! A static revised in place, through the reconciler and the fake sim
//! (spec/world.md, "Revising a static"): a FARP's owner and stock come back
//! on its new airbase whatever order its records take, a revision's spawn
//! counts against the budget, a position of the old object does not replace
//! the revised place, and a death of the old object never marks the new one.

use nanoprobe_sidecar::ids::{self, CampaignId, EntityId, NameKind};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::sim::wire::{lv, opcode};
use nanoprobe_sidecar::stock::{Item, Stock};
use nanoprobe_sidecar::store::{
    Authority, Entry, LostPolicy, NewGroup, NewUnit, Point, StaticRevision, UnitState,
};
use nanoprobe_sim_fake::FarpSpawns;
use nanoprobe_sim_fake::harness::{CAMPAIGN, THEATER, World};
use nanoprobe_transport::codec::Lv;

fn world(statics: usize) -> World {
    let mut w = World::new(0, statics, LostPolicy::Report);
    w.settle().expect("settles");
    w
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().expect("tick");
    }
}

fn sent(w: &World, op: u16) -> usize {
    w.store
        .entries()
        .iter()
        .filter(|e| matches!(e, Entry::Command(c) if c.opcode == op))
        .count()
}

fn weapon(n: &str, c: f64) -> Stock {
    let mut s = Stock::default();
    s.set(Item::Weapon(n.into()), c);
    s
}

fn spec(type_name: &str, category: &str, x: f64) -> NewGroup {
    NewGroup {
        theater: THEATER.into(),
        country: 2,
        category: 0,
        coalition: 2,
        units: vec![NewUnit {
            extra: lv::obj(vec![("category", lv::s(category))]),
            ..NewUnit::new(
                type_name,
                UnitState {
                    at: Point { x, y: 0.0, z: 0.0 },
                    ..UnitState::default()
                },
            )
        }],
        authority: Authority::Sim,
        label: type_name.into(),
        extra: Lv::Nil,
    }
}

/// The revision that moves the static by `dx` and keeps the rest.
fn moved(w: &World, id: EntityId, dx: f64) -> StaticRevision {
    let u = w.store.unit(id).expect("unit");
    StaticRevision {
        type_name: u.type_name.clone(),
        at: Point {
            x: u.state.at.x + dx,
            ..u.state.at
        },
        heading: u.state.heading,
        extra: u.extra.clone(),
        further: Vec::new(),
    }
}

/// The DCS object the static's entity holds, if any.
fn object_of(w: &World, id: EntityId) -> Option<u64> {
    w.sim
        .objects()
        .into_iter()
        .find(|o| o.1 == CAMPAIGN && o.2 == id.0)
        .map(|o| o.0)
}

fn live_at(w: &World, id: EntityId, rev: u64) -> bool {
    let g = w.store.group(id).expect("held");
    g.state == State::Live
        && g.spec_rev == rev
        && w.sim
            .ledger()
            .spawns
            .iter()
            .any(|s| s.2 == id.0 && s.3 == rev)
}

/// A FARP spawned with an owner other than its country's coalition and a
/// stock, both applied in DCS. Returns its entity and airbase name.
fn farp_with_owner_and_stock(w: &mut World) -> (EntityId, String) {
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(w, 15);
    assert!(live_at(w, id, 1), "the FARP is live");
    assert_eq!(
        w.sim.airbase_owner(&name),
        Some(2),
        "its country's coalition"
    );
    w.store.set_airbase_owner(THEATER, &name, 1);
    w.store.set_stock(THEATER, &name, &weapon("AIM-9X", 4.0));
    ticks(w, 15);
    assert_eq!(w.sim.airbase_owner(&name), Some(1));
    assert_eq!(
        w.sim
            .airbase_stock(&name)
            .and_then(|s| s.get(&Item::Weapon("AIM-9X".into()))),
        Some(4.0)
    );
    (id, name)
}

/// Revise the FARP with DCS answering as `mode` says, and check that its
/// desired owner and stock come back on the new airbase and are never
/// replaced by what the new airbase showed first, and that an item the
/// controller never set is carried over from the old airbase.
fn farp_revision_keeps_owner_and_stock(mode: FarpSpawns) {
    let mut w = world(0);
    let (id, name) = farp_with_owner_and_stock(&mut w);
    // DCS has an item the controller never set.
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    ticks(&mut w, 15);
    let old = w.sim.airbase_object(&name).expect("airbase");
    let (owners, stocks) = (
        sent(&w, opcode::AIRBASE_OWNER),
        sent(&w, opcode::WAREHOUSE_SET),
    );
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..mode
    });
    let r = moved(&w, id, 50.0);
    assert_eq!(w.store.revise_static(id, r), Ok(Some(2)));
    ticks(&mut w, 40);
    assert!(live_at(&w, id, 2), "spawned again at the revision");
    let new = w.sim.airbase_object(&name).expect("a new airbase");
    if mode.same_object {
        assert_eq!(new, old);
    } else {
        assert_ne!(new, old);
    }
    assert_eq!(w.sim.airbase_owner(&name), Some(1), "{mode:?}");
    let stock = w.sim.airbase_stock(&name).expect("stock").clone();
    assert_eq!(
        stock.get(&Item::Weapon("AIM-9X".into())),
        Some(4.0),
        "{mode:?}"
    );
    assert_eq!(
        stock.get(&Item::Weapon("Mk-82".into())),
        Some(10.0),
        "an item never set is carried: {mode:?}"
    );
    assert!(
        w.store
            .desired_stock(THEATER, &name)
            .and_then(|d| d.stock.get(&Item::Weapon("Mk-82".into())))
            .is_none(),
        "a carried item does not become desired"
    );
    assert_eq!(
        w.store.desired_owner(THEATER, &name).map(|d| d.coalition),
        Some(1),
        "{mode:?}"
    );
    assert_eq!(
        w.store
            .desired_stock(THEATER, &name)
            .and_then(|d| d.stock.get(&Item::Weapon("AIM-9X".into()))),
        Some(4.0),
        "the new airbase's empty stock is not read as play: {mode:?}"
    );
    // The desired owner and stock once each; the carried item once. The
    // owner is desired, so nothing of it is carried.
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), owners + 1, "{mode:?}");
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), stocks + 2, "{mode:?}");
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), owners + 1, "sent once");
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), stocks + 2, "sent once");
}

#[test]
fn a_farp_revision_keeps_owner_and_stock_with_the_old_gone_first() {
    farp_revision_keeps_owner_and_stock(FarpSpawns::default());
}

#[test]
fn a_farp_revision_keeps_owner_and_stock_with_the_old_gone_lost() {
    farp_revision_keeps_owner_and_stock(FarpSpawns {
        lose_gone: true,
        ..FarpSpawns::default()
    });
}

#[test]
fn a_farp_revision_keeps_owner_and_stock_with_the_old_gone_after_the_result() {
    farp_revision_keeps_owner_and_stock(FarpSpawns {
        gone_after_result: true,
        ..FarpSpawns::default()
    });
}

#[test]
fn a_farp_revision_keeps_owner_and_stock_under_an_unchanged_object_id() {
    farp_revision_keeps_owner_and_stock(FarpSpawns {
        same_object: true,
        lose_gone: true,
        ..FarpSpawns::default()
    });
}

#[test]
fn a_captured_owner_with_no_desired_owner_is_carried() {
    let mut w = world(0);
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        lose_gone: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(&mut w, 15);
    w.sim.capture_airbase(&name, 1);
    ticks(&mut w, 15);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 40);
    assert!(live_at(&w, id, 2));
    assert_eq!(
        w.sim.airbase_owner(&name),
        Some(1),
        "the captured owner stays"
    );
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1, "carried once");
    assert!(
        w.store.desired_owner(THEATER, &name).is_none(),
        "and not made desired"
    );
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1);
    assert_eq!(w.sim.airbase_owner(&name), Some(1));
}

#[test]
fn a_desired_value_set_just_before_a_revision_wins_over_the_carry() {
    let mut w = world(0);
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        lose_gone: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(&mut w, 15);
    // DCS holds an owner and an item nobody set; the samples show them.
    w.sim.capture_airbase(&name, 1);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    ticks(&mut w, 15);
    // The controller sets both, and revises, before any sample shows it.
    w.store.set_airbase_owner(THEATER, &name, 2);
    w.store.set_stock(THEATER, &name, &weapon("Mk-82", 3.0));
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 40);
    assert!(live_at(&w, id, 2));
    assert_eq!(
        w.sim.airbase_owner(&name),
        Some(2),
        "the desired owner wins"
    );
    let stock = w.sim.airbase_stock(&name).expect("stock");
    assert_eq!(
        stock.get(&Item::Weapon("Mk-82".into())),
        Some(3.0),
        "the desired count wins"
    );
    assert_eq!(w.rec.counters().carried, 0, "nothing was carried");
}

#[test]
fn nothing_is_carried_to_a_farp_spawned_in_a_new_generation() {
    let mut w = world(0);
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(&mut w, 15);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    ticks(&mut w, 15);
    w.apply(&nanoprobe_sim_fake::harness::Op::LoadMission)
        .unwrap();
    w.settle().unwrap();
    ticks(&mut w, 30);
    // Spawned again in the new generation, at the same revision.
    let generation = w.sim.mission_gen();
    let again = w
        .sim
        .ledger()
        .spawns
        .iter()
        .filter(|s| s.1 == generation && s.2 == id.0)
        .count();
    assert_eq!(again, 1, "one spawn of the FARP in the new generation");
    assert_eq!(w.store.group(id).unwrap().state, State::Live);
    let stock = w.sim.airbase_stock(&name).expect("stock");
    assert_eq!(stock.get(&Item::Weapon("Mk-82".into())), None);
    assert_eq!(w.rec.counters().carried, 0);
}

#[test]
fn revisions_spawn_within_the_materialization_budget() {
    let mut w = world(4);
    let ids: Vec<EntityId> = w.store.groups().map(|g| g.id).collect();
    assert_eq!(ids.len(), 4);
    for id in &ids {
        let r = moved(&w, *id, 25.0);
        assert_eq!(w.store.revise_static(*id, r), Ok(Some(2)));
    }
    let mut before = sent(&w, opcode::STATIC_SPAWN);
    for _ in 0..20 {
        ticks(&mut w, 1);
        let now = sent(&w, opcode::STATIC_SPAWN);
        assert!(now - before <= 2, "{} spawns in one step", now - before);
        before = now;
    }
    for id in &ids {
        assert!(live_at(&w, *id, 2), "{id} spawned at its revision");
    }
}

#[test]
fn a_position_of_the_old_object_does_not_replace_the_revised_place() {
    let mut w = world(0);
    let id = w
        .store
        .create_static(spec("Container", "Cargos", -3_000.0))
        .expect("cargo");
    ticks(&mut w, 15);
    assert!(live_at(&w, id, 1));
    // A type DCS refuses: the spawn is owed at revision 2 and never
    // applies, while DCS keeps the old object and samples its place.
    let mut r = moved(&w, id, 75.0);
    r.type_name = "Unknown crate".into();
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 40);
    assert!(
        object_of(&w, id).is_some(),
        "the old object is still in DCS"
    );
    assert_eq!(w.store.unit(id).expect("unit").state.at.x, -2_925.0);
    // Fixed: the revised place is spawned.
    let mut r = moved(&w, id, 0.0);
    r.type_name = "Container".into();
    w.store.revise_static(id, r).expect("fix");
    ticks(&mut w, 20);
    assert!(live_at(&w, id, 3));
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    let table = w
        .sim
        .ledger()
        .static_tables
        .get(&name)
        .cloned()
        .expect("table");
    assert_eq!(lv::num_at(&table, "x"), Some(-2_925.0));
}

#[test]
fn a_death_before_the_revisions_spawn_applies_keeps_the_static_dead() {
    let mut w = world(1);
    let id = w.store.groups().next().expect("static").id;
    let old = object_of(&w, id).expect("in DCS");
    let r = moved(&w, id, 25.0);
    w.store.revise_static(id, r).expect("revise");
    // The spawn is written; the old static dies before DCS takes it.
    ticks(&mut w, 1);
    assert!(
        sent(&w, opcode::STATIC_SPAWN) >= 2,
        "the revision's spawn is in flight"
    );
    w.sim.kill(old);
    ticks(&mut w, 30);
    let g = w.store.group(id).expect("held");
    assert_eq!(
        g.state,
        State::Dead,
        "a dead static is not brought back alive"
    );
    assert!(w.store.unit(id).expect("unit").dead);
    // The revision's spawn reached DCS after the death; the ruin that
    // follows replaces what it made, so no live object of it stays.
    assert_eq!(object_of(&w, id), None, "no live object of a dead static");
    assert_eq!(w.sim.ruins(CAMPAIGN).len(), 1, "its ruin stands");
}

#[test]
fn a_late_death_of_the_old_object_does_not_mark_the_new_one() {
    let mut w = world(1);
    let id = w.store.groups().next().expect("static").id;
    let old = object_of(&w, id).expect("in DCS");
    let r = moved(&w, id, 25.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 1);
    // The old static died just before the replace; its death event comes
    // after the spawn's result and the new object's binding.
    w.sim.kill_late(old, 2.0, nanoprobe_sim_fake::Left::Missing);
    ticks(&mut w, 60);
    let g = w.store.group(id).expect("held");
    let new = object_of(&w, id);
    assert_eq!(w.sim.late_pending(), 0, "the late death was delivered");
    assert_eq!(g.state, State::Live);
    assert!(new.is_some_and(|n| n != old), "the new object stands");
    assert!(
        !w.store.unit(id).expect("unit").dead,
        "the new object is not dead"
    );
}

#[test]
fn a_heliport_groups_table_follows_its_revised_size() {
    let mut w = world(0);
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    ticks(&mut w, 15);
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    let units = |w: &World| -> usize {
        let t = w
            .sim
            .ledger()
            .static_tables
            .get(&name)
            .cloned()
            .expect("table");
        lv::items(lv::get(&t, "units").expect("units")).len()
    };
    assert_eq!(units(&w), 1);
    for (size, rev) in [(4usize, 2u64), (2, 3), (1, 4)] {
        let mut r = moved(&w, id, 0.0);
        r.heading += 0.1;
        r.further = (1..size)
            .map(|i| nanoprobe_sidecar::store::NewFurther {
                type_name: "FARP".into(),
                at: Point {
                    x: -2_000.0 + 200.0 * i as f64,
                    y: 0.0,
                    z: 0.0,
                },
                heading: 0.0,
                extra: lv::obj(vec![("category", lv::s("Heliports"))]),
            })
            .collect();
        assert_eq!(w.store.revise_static(id, r), Ok(Some(rev)));
        ticks(&mut w, 15);
        assert!(live_at(&w, id, rev));
        assert_eq!(units(&w), size);
        assert_eq!(w.sim.airbase_owner(&name), Some(2));
        // The fake makes the airbase under the first name only.
        for f in &w.store.group(id).unwrap().further {
            let n = ids::name(NameKind::Static, CampaignId(CAMPAIGN), f.id);
            assert_eq!(w.sim.airbase_object(&n), None, "no airbase under {n}");
        }
    }
}

#[test]
fn a_farp_revision_keeps_owner_and_stock_with_a_new_sample_before_the_result() {
    farp_revision_keeps_owner_and_stock(FarpSpawns {
        sample_before_result: true,
        ..FarpSpawns::default()
    });
}

/// A FARP with an item nobody set, revised, with its carried
/// `WAREHOUSE_SET` answered as `answer` says: the item still arrives.
fn carried_stock_survives(answer: impl FnOnce(&mut World)) {
    let mut w = world(0);
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(&mut w, 15);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    ticks(&mut w, 15);
    answer(&mut w);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    // Past a `NOT_READY` hold.
    ticks(&mut w, 400);
    assert!(live_at(&w, id, 2));
    let stock = w.sim.airbase_stock(&name).expect("stock");
    assert_eq!(stock.get(&Item::Weapon("Mk-82".into())), Some(10.0));
    assert!(
        w.store.desired_stock(THEATER, &name).is_none(),
        "nothing made desired"
    );
}

#[test]
fn a_carried_stock_answered_not_ready_is_sent_again() {
    carried_stock_survives(|w| w.sim.stock_not_ready(1));
}

#[test]
fn a_lost_carried_result_that_applied_is_not_written_again() {
    let mut w = world(0);
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(&mut w, 15);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    ticks(&mut w, 15);
    w.sim.lose_next_result_of(opcode::WAREHOUSE_SET);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 80);
    // The command applied and its result was lost: the carry is sent again
    // only where the new sample does not show it, and it does.
    ticks(&mut w, 1);
    w.apply(&nanoprobe_sim_fake::harness::Op::Reattach).unwrap();
    ticks(&mut w, 80);
    let stock = w.sim.airbase_stock(&name).expect("stock");
    assert_eq!(stock.get(&Item::Weapon("Mk-82".into())), Some(10.0));
    assert_eq!(w.rec.counters().carried, 1, "written once");
}

#[test]
fn a_revision_while_paused_holds_the_old_static_pending_and_binds_only_the_new() {
    let mut w = world(1);
    let id = w.store.groups().next().expect("static").id;
    let old = object_of(&w, id).expect("in DCS");
    let unmanaged = w.rec.counters().unmanaged;
    w.sim.pause();
    ticks(&mut w, 5);
    let r = moved(&w, id, 25.0);
    w.store.revise_static(id, r).expect("revise");
    // Past a full enumeration, still paused.
    ticks(&mut w, 120);
    assert!(w.sim.paused());
    assert!(
        w.sim.pending_destroys().iter().any(|p| p.0 == old),
        "the old static waits for the resume"
    );
    assert!(live_at(&w, id, 2), "live at the revision while paused");
    let new = object_of(&w, id).expect("the new static");
    assert_ne!(new, old);
    assert_eq!(w.sim.binding(old), (0, 0), "the old object is not bound");
    assert_eq!(sent(&w, opcode::STATIC_DESTROY), 0, "no orphan destroy");
    assert_eq!(
        w.rec.counters().unmanaged,
        unmanaged,
        "nothing read as unmanaged"
    );
    w.sim.resume();
    ticks(&mut w, 30);
    assert!(w.sim.pending_destroys().is_empty());
    assert!(live_at(&w, id, 2));
    assert_eq!(object_of(&w, id), Some(new));
}

/// A FARP whose owner DCS captured and which holds an item nobody set.
fn captured_farp(w: &mut World, mode: FarpSpawns) -> (EntityId, String) {
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..mode
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(w, 15);
    w.sim.capture_airbase(&name, 1);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    ticks(w, 15);
    (id, name)
}

fn shows_the_carry(w: &World, name: &str) {
    assert_eq!(w.sim.airbase_owner(name), Some(1), "the captured owner");
    let stock = w.sim.airbase_stock(name).expect("stock");
    assert_eq!(
        stock.get(&Item::Weapon("Mk-82".into())),
        Some(10.0),
        "the item"
    );
}

#[test]
fn a_second_revision_right_after_the_first_carries_the_first_airbases_state() {
    let mut w = world(0);
    let (id, name) = captured_farp(&mut w, FarpSpawns::default());
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    // The first carry is written, not yet seen in a sample.
    for _ in 0..40 {
        ticks(&mut w, 1);
        if w.rec.counters().carried > 0 {
            break;
        }
    }
    assert!(w.rec.counters().carried > 0, "the first carry was sent");
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise again");
    ticks(&mut w, 60);
    assert!(live_at(&w, id, 3));
    shows_the_carry(&w, &name);
}

#[test]
fn a_revision_whose_spawn_result_is_lost_carries_the_old_state() {
    let mut w = world(0);
    let (id, name) = captured_farp(&mut w, FarpSpawns::default());
    w.sim.lose_next_result_of(opcode::STATIC_SPAWN);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 10);
    // The link dropped with the result; the next attach's HELLO resolves
    // the spawn as unknown.
    w.apply(&nanoprobe_sim_fake::harness::Op::Reattach).unwrap();
    ticks(&mut w, 120);
    assert!(live_at(&w, id, 2));
    shows_the_carry(&w, &name);
}

#[test]
fn a_lost_gone_and_an_early_sample_keep_the_desired_owner_and_the_carry() {
    farp_revision_keeps_owner_and_stock(FarpSpawns {
        lose_gone: true,
        sample_before_result: true,
        ..FarpSpawns::default()
    });
}

#[test]
fn a_revision_spawn_dcs_did_not_take_carries_nothing_over_play() {
    let mut w = world(0);
    let (id, name) = captured_farp(&mut w, FarpSpawns::default());
    w.sim.static_not_ready(1);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    // The spawn is written; play draws from the airbase before its answer.
    ticks(&mut w, 1);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 7.0);
    ticks(&mut w, 400);
    assert!(live_at(&w, id, 2), "spawned once the hold ended");
    let stock = w.sim.airbase_stock(&name).expect("stock");
    assert_eq!(
        stock.get(&Item::Weapon("Mk-82".into())),
        Some(7.0),
        "the draw stands: the refused spawn carried nothing"
    );
}

#[test]
fn an_equal_revision_after_a_sampled_place_changes_nothing() {
    let mut w = world(0);
    // DCS reports the static on the ground; its spec gives 5 m.
    let mut s = spec("Hangar A", "Fortifications", -3_000.0);
    s.units[0].state.at.y = 5.0;
    let id = w.store.create_static(s).expect("static");
    ticks(&mut w, 30);
    assert!(live_at(&w, id, 1));
    let mut r = moved(&w, id, 0.0);
    r.at.y = 5.0;
    assert_eq!(w.store.revise_static(id, r), Ok(None), "nothing changed");
}

#[test]
fn a_spawn_retried_before_any_new_sample_still_carries_the_old_state() {
    // Warehouses sampled less often than a `NOT_READY` hold lasts: no sample
    // of the kept airbase lands between the refused spawn and its retry.
    let mut w = World::with_config(
        0,
        0,
        LostPolicy::Report,
        nanoprobe_sim_fake::Config {
            event_queue: 6,
            warehouse_every: 400,
            ..nanoprobe_sim_fake::Config::default()
        },
    );
    w.settle().expect("settles");
    w.sim.farp_spawns(FarpSpawns {
        make_airbase: true,
        ..FarpSpawns::default()
    });
    let id = w
        .store
        .create_static(spec("FARP", "Heliports", -2_000.0))
        .expect("static");
    let name = ids::name(NameKind::Static, CampaignId(CAMPAIGN), id);
    ticks(&mut w, 20);
    w.sim.capture_airbase(&name, 1);
    w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 10.0);
    // Until a sample shows both.
    for _ in 0..900 {
        ticks(&mut w, 1);
        if w.sim.frame_count() % 400 == 1 {
            break;
        }
    }
    w.sim.static_not_ready(1);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 1_000);
    assert!(live_at(&w, id, 2));
    shows_the_carry(&w, &name);
}

#[test]
fn a_cargo_moved_in_play_takes_its_sampled_place() {
    let mut w = world(0);
    let id = w
        .store
        .create_static(spec("Container", "Cargos", -3_000.0))
        .expect("cargo");
    ticks(&mut w, 15);
    assert!(live_at(&w, id, 1));
    let obj = object_of(&w, id).expect("in DCS");
    w.sim.move_object(obj, -2_800.0, 40.0);
    ticks(&mut w, 15);
    let at = w.store.unit(id).expect("unit").state.at;
    assert_eq!((at.x, at.z), (-2_800.0, 40.0), "its last known place");
    // The spec as created moves it back.
    let mut r = moved(&w, id, 0.0);
    r.at = Point {
        x: -3_000.0,
        y: 0.0,
        z: 0.0,
    };
    assert_eq!(w.store.revise_static(id, r), Ok(Some(2)));
    ticks(&mut w, 15);
    assert!(live_at(&w, id, 2));
}

#[test]
fn play_on_the_kept_airbase_after_a_refused_spawn_is_what_is_carried() {
    for mode in [
        FarpSpawns::default(),
        FarpSpawns {
            lose_gone: true,
            ..FarpSpawns::default()
        },
        FarpSpawns {
            gone_after_result: true,
            ..FarpSpawns::default()
        },
        FarpSpawns {
            same_object: true,
            lose_gone: true,
            ..FarpSpawns::default()
        },
    ] {
        let mut w = world(0);
        let (id, name) = captured_farp(&mut w, mode);
        w.sim.static_not_ready(1);
        let r = moved(&w, id, 50.0);
        w.store.revise_static(id, r).expect("revise");
        ticks(&mut w, 3);
        // Play on the airbase DCS kept, sampled before the retry.
        w.sim.use_stock(&name, Item::Weapon("Mk-82".into()), 7.0);
        ticks(&mut w, 400);
        assert!(live_at(&w, id, 2), "{mode:?}");
        let stock = w.sim.airbase_stock(&name).expect("stock");
        assert_eq!(
            stock.get(&Item::Weapon("Mk-82".into())),
            Some(7.0),
            "the draw stands: {mode:?}"
        );
        assert_eq!(w.sim.airbase_owner(&name), Some(1), "{mode:?}");
    }
}

#[test]
fn a_second_refusal_after_a_partial_sample_keeps_the_whole_seed() {
    let mut w = world(0);
    // The old object's `OBJECT_GONE` is lost: only the seed carries.
    let (id, name) = captured_farp(
        &mut w,
        FarpSpawns {
            lose_gone: true,
            ..FarpSpawns::default()
        },
    );
    // Only `WAREHOUSE` rows reach the sidecar while two spawns are refused.
    w.sim.drop_airbase_rows(true);
    w.sim.static_not_ready(2);
    let r = moved(&w, id, 50.0);
    w.store.revise_static(id, r).expect("revise");
    ticks(&mut w, 350);
    w.sim.drop_airbase_rows(false);
    ticks(&mut w, 400);
    assert!(live_at(&w, id, 2));
    shows_the_carry(&w, &name);
}
