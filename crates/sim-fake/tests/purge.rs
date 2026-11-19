//! The purge and the expiry on the fake sim (spec/world.md, "Removal" and
//! "Expiry"; spec/persistence.md, "Restore a backup", "Export", "Size"): a
//! removed entity's rows leave the store once DCS holds nothing of it, a
//! dead group expires with no command to DCS, and an object of a purged
//! name is destroyed as drift. The full checks run in every test.

use std::collections::BTreeMap;
use std::path::Path;

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::players::Unmanaged;
use nanoprobe_sidecar::reconciler::Config as RecConfig;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sidecar::store::{
    Authority, CampaignRecord, CampaignState, Entry, Kind, LostPolicy, MarkerSpec, NewGroup,
    NewUnit, Point, RemoveCause, Store, UnitState, scope,
};
use nanoprobe_sim_fake::harness::{
    CAMPAIGN, FOREIGN_CAMPAIGN, Op, Schedule, THEATER, World, new_store,
};
use nanoprobe_sim_fake::{Config, Left};

const START_MS: u64 = 1_700_000_000_000;

fn rec_config(removed_keep_ms: u64, dead_keep_ms: u64, on_lost: LostPolicy) -> RecConfig {
    RecConfig {
        materialize_per_frame: 2,
        on_lost,
        destroy_retries: 3,
        removed_keep_ms,
        dead_keep_ms,
    }
}

/// A world over `store` with a simulated clock of 100 ms a frame.
fn world_over(store: Store, config: Config, rec: RecConfig) -> World {
    let mut w = World::with_store(store, config, rec);
    w.clock_ms = START_MS;
    w.store.set_now(START_MS);
    w.frame_ms = 100;
    w
}

fn config() -> Config {
    Config {
        event_queue: 10_000,
        ..Config::default()
    }
}

fn world(groups: usize, statics: usize, removed_keep_ms: u64, dead_keep_ms: u64) -> World {
    let mut w = world_over(
        new_store(groups, statics),
        config(),
        rec_config(removed_keep_ms, dead_keep_ms, LostPolicy::Report),
    );
    ticks(&mut w, 60);
    w
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().unwrap();
    }
}

fn spec(n: u64) -> NewGroup {
    let units = (0..3)
        .map(|u| NewUnit {
            label: format!("put {n}.{u}"),
            skill: "Average".into(),
            ..NewUnit::new(
                "M-113",
                UnitState {
                    at: Point {
                        x: (n % 50) as f64 * 1_000.0 + 500.0,
                        y: 0.0,
                        z: f64::from(u) * 20.0,
                    },
                    ..UnitState::default()
                },
            )
        })
        .collect();
    NewGroup {
        theater: THEATER.into(),
        country: 2,
        category: 2,
        coalition: 2,
        units,
        authority: Authority::Sim,
        label: format!("put {n}"),
        extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
    }
}

fn a_tent(x: f64) -> NewGroup {
    NewGroup {
        theater: THEATER.into(),
        country: 2,
        category: 0,
        coalition: 2,
        units: vec![NewUnit {
            extra: lv::obj(vec![("shape_name", lv::s("tent"))]),
            ..NewUnit::new(
                "Tent",
                UnitState {
                    at: Point { x, y: 0.0, z: 70.0 },
                    heading: 1.25,
                    life: 0.0,
                },
            )
        }],
        authority: Authority::Sim,
        label: "tent".into(),
        extra: lv::obj(vec![]),
    }
}

fn record() -> CampaignRecord {
    CampaignRecord {
        name: "purge".into(),
        state: CampaignState::Active,
        theaters: vec![THEATER.into()],
        theater: THEATER.into(),
        clock_ms: Some(START_MS as i64),
        created_ms: 1,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

fn groups(w: &World) -> Vec<EntityId> {
    w.store
        .groups()
        .filter(|g| g.kind == Kind::Group && !g.removed)
        .map(|g| g.id)
        .collect()
}

fn members(w: &World, id: EntityId) -> Vec<EntityId> {
    w.store.group(id).expect("group").members.clone()
}

/// The DCS objects of an entity (a group's units, or a static).
fn objects_of(w: &World, members: &[EntityId], id: EntityId) -> Vec<u64> {
    w.sim
        .objects()
        .into_iter()
        .filter(|(_, c, e)| {
            *c == w.store.campaign().0 && (members.contains(&EntityId(*e)) || *e == id.0)
        })
        .map(|(o, _, _)| o)
        .collect()
}

fn journalled(w: &World, f: impl Fn(&Entry) -> bool) -> usize {
    w.store.entries().iter().filter(|e| f(e)).count()
}

fn purged_entries(w: &World, id: EntityId) -> usize {
    journalled(
        w,
        |e| matches!(e, Entry::Purged { entity } if *entity == id),
    )
}

/// Run frames until `done` holds, at most `max`.
fn until(w: &mut World, max: usize, done: impl Fn(&World) -> bool) -> bool {
    for _ in 0..max {
        if done(w) {
            return true;
        }
        w.tick().unwrap();
    }
    done(w)
}

fn gone(w: &World, id: EntityId) -> bool {
    w.store.group(id).is_none()
}

/// A removed live group is destroyed in DCS, then purged once its keep has
/// passed and an enumeration ran.
#[test]
fn a_removed_live_group_is_destroyed_then_purged() {
    let mut w = world(2, 0, 1_000, 20_000);
    let id = groups(&w)[0];
    let other = groups(&w)[1];
    let m = members(&w, id);
    assert_eq!(objects_of(&w, &m, id).len(), 3, "spawned");
    w.store.remove_entity(id).unwrap();
    assert!(
        until(&mut w, 600, |w| gone(w, id)),
        "the group was not purged"
    );
    for u in &m {
        assert!(w.store.unit(*u).is_none(), "unit {} kept", u.0);
    }
    assert_eq!(purged_entries(&w, id), 1);
    assert!(
        objects_of(&w, &m, id).is_empty(),
        "DCS holds an object of it"
    );
    assert!(w.store.group(other).is_some(), "the other group stays");
    assert_eq!(w.store.purged_expired().0, 1);
    w.settle().unwrap();
}

/// Each guard holds the purge, and the purge follows once it clears.
#[test]
fn each_guard_holds_the_purge_until_it_clears() {
    // (a) DCS refuses the destroys. A removed live group leaves DCS by a
    // capture-destroy, which the fake never refuses, so the refused
    // destroy is a ruin's: its object stays, and the sidecar gives up after
    // `destroy_retries`.
    let mut w = world(0, 1, 0, 20_000);
    let id = w.store.groups().next().expect("static").id;
    let o = objects_of(&w, &[id], id)[0];
    w.sim.kill(o);
    ticks(&mut w, 60);
    assert_eq!(w.sim.ruins(CAMPAIGN).len(), 1);
    w.sim.refuse_destroys(1_000_000);
    w.store.remove_entity(id).unwrap();
    ticks(&mut w, 300);
    assert!(!gone(&w, id), "(a) purged while DCS holds its ruin");
    assert_eq!(w.sim.ruins(CAMPAIGN).len(), 1, "(a) the ruin stayed");
    w.sim.refuse_destroys(0);
    w.apply(&Op::SidecarRestart).unwrap();
    assert!(
        until(&mut w, 800, |w| gone(w, id)),
        "(a) not purged after DCS accepted"
    );
    assert!(w.sim.ruins(CAMPAIGN).is_empty());
    w.settle().unwrap();

    // (b) the link drops after the removal, before the destroy is written:
    // the objects stay bound. The unanswered destroy alone is
    // `an_unanswered_destroy_holds_the_purge_until_a_hello_settles_it`.
    let mut w = world(1, 0, 0, 20_000);
    let id = groups(&w)[0];
    w.store.remove_entity(id).unwrap();
    w.sim.detach();
    ticks(&mut w, 300);
    assert!(!gone(&w, id), "(b) purged while detached");
    w.apply(&Op::Reattach).unwrap();
    assert!(
        until(&mut w, 800, |w| gone(w, id)),
        "(b) not purged after reattach"
    );
    w.settle().unwrap();

    // (c) no enumeration since the removal.
    let mut w = world_over(
        new_store(1, 0),
        Config {
            enum_period: 1_000_000,
            ..config()
        },
        rec_config(0, 20_000, LostPolicy::Report),
    );
    ticks(&mut w, 60);
    let id = groups(&w)[0];
    let m = members(&w, id);
    w.store.remove_entity(id).unwrap();
    ticks(&mut w, 300);
    assert!(objects_of(&w, &m, id).is_empty(), "(c) destroyed");
    assert!(!gone(&w, id), "(c) purged with no enumeration since");
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    assert!(
        until(&mut w, 800, |w| gone(w, id)),
        "(c) not purged after an enumeration"
    );
    w.settle().unwrap();

    // (d) the keep has not passed.
    let mut w = world(1, 0, 10_000, 20_000);
    let id = groups(&w)[0];
    w.store.remove_entity(id).unwrap();
    ticks(&mut w, 60);
    assert!(!gone(&w, id), "(d) purged inside its keep");
    assert!(
        until(&mut w, 400, |w| gone(w, id)),
        "(d) not purged after its keep"
    );
    w.settle().unwrap();
}

/// A late death after the purge: the event is dropped and counted.
#[test]
fn a_late_death_after_the_purge_is_dropped_and_counted() {
    let mut w = world(1, 0, 0, 20_000);
    let id = groups(&w)[0];
    let m = members(&w, id);
    let o = objects_of(&w, &m, id)[0];
    w.sim.kill_late(o, 70.0, Left::Removed);
    ticks(&mut w, 5);
    w.store.remove_entity(id).unwrap();
    assert!(until(&mut w, 300, |w| gone(w, id)), "not purged");
    let before = w.rec.counters().purged_deaths;
    ticks(&mut w, 900);
    assert!(
        w.rec.counters().purged_deaths > before,
        "the late death was not counted: {}",
        w.rec.counters().purged_deaths
    );
    assert!(gone(&w, id));
    w.settle().unwrap();
}

/// A ruin is never expired or purged on its own; removed, its destroy
/// applies and it is purged.
#[test]
fn a_ruin_stays_until_removed_then_is_destroyed_and_purged() {
    let mut w = world(0, 1, 1_000, 2_000);
    let killed = w
        .store
        .groups()
        .find(|g| g.kind == Kind::Static)
        .expect("static")
        .id;
    let o = objects_of(&w, &[killed], killed)[0];
    w.sim.kill(o);
    let born = w.create_ruin(a_tent(-5_000.0));
    ticks(&mut w, 60);
    assert_eq!(w.store.group(killed).unwrap().state, State::Dead);
    // Many keep windows.
    ticks(&mut w, 800);
    for id in [killed, born] {
        let g = w.store.group(id).expect("the ruin stays");
        assert!(!g.removed && !g.expired, "ruin {} removed", id.0);
        assert_eq!(g.state, State::Dead);
    }
    assert_eq!(w.sim.ruins(CAMPAIGN).len(), 2);
    for id in [killed, born] {
        w.store.remove_entity(id).unwrap();
    }
    assert!(
        until(&mut w, 800, |w| gone(w, killed) && gone(w, born)),
        "the removed ruins were not purged"
    );
    assert!(w.sim.ruins(CAMPAIGN).is_empty(), "a ruin stays in DCS");
    w.settle().unwrap();
}

/// A dead group expires with no command to DCS, a lost group does not, and
/// with `dead_keep_ms` 0 nothing expires.
#[test]
fn a_dead_group_expires_with_no_command_and_is_purged_later() {
    let mut w = world(2, 0, 3_000, 5_000);
    let (dead, lost) = (groups(&w)[0], groups(&w)[1]);
    for o in objects_of(&w, &members(&w, dead), dead) {
        w.sim.kill(o);
    }
    let lm = members(&w, lost);
    let lo = objects_of(&w, &lm, lost);
    w.sim.remove_silently(lo[0]);
    for o in &lo[1..] {
        w.sim.kill(*o);
    }
    assert!(
        until(&mut w, 200, |w| w.store.group(lost).unwrap().state
            == State::Lost),
        "the group with a silent removal is not lost"
    );
    assert_eq!(w.store.group(dead).unwrap().state, State::Dead);
    let taken = w.sim.ledger().taken.len();
    let objects = w.sim.objects().len();
    assert!(
        until(&mut w, 200, |w| w
            .store
            .group(dead)
            .is_none_or(|g| g.removed)),
        "the dead group did not expire"
    );
    let g = w.store.group(dead).expect("expired, not yet purged");
    assert!(g.removed && g.expired);
    assert_eq!(
        journalled(
            &w,
            |e| matches!(e, Entry::Removed { entity, cause: RemoveCause::Expired } if *entity == dead)
        ),
        1
    );
    assert_eq!(
        w.sim.ledger().taken.len(),
        taken,
        "a command for the expiry"
    );
    assert_eq!(w.sim.objects().len(), objects);
    assert!(
        until(&mut w, 600, |w| gone(w, dead)),
        "an expired group was not purged"
    );
    assert_eq!(
        w.sim.ledger().taken.len(),
        taken,
        "a command for the dead group"
    );
    assert_eq!(w.sim.objects().len(), objects);
    let (_, expired) = w.store.purged_expired();
    assert_eq!(expired, 1);
    // The lost group, held, never expires.
    ticks(&mut w, 300);
    let g = w.store.group(lost).expect("lost group stays");
    assert!(!g.removed, "a held group expired");
    assert_eq!(g.state, State::Lost);

    // dead_keep_ms 0: nothing expires.
    let mut w = world(1, 0, 0, 0);
    let id = groups(&w)[0];
    for o in objects_of(&w, &members(&w, id), id) {
        w.sim.kill(o);
    }
    ticks(&mut w, 800);
    let g = w.store.group(id).expect("kept");
    assert!(!g.removed && g.state == State::Dead);
    assert_eq!(w.store.purged_expired(), (0, 0));
}

/// A removed live group whose destroy DCS took but whose `RESULT` was lost
/// is not purged while the command is unanswered; a `HELLO` settles it.
#[test]
fn an_unanswered_destroy_holds_the_purge_until_a_hello_settles_it() {
    let mut w = world(1, 0, 0, 20_000);
    let id = groups(&w)[0];
    let m = members(&w, id);
    assert_eq!(objects_of(&w, &m, id).len(), 3, "spawned");
    w.store.remove_entity(id).unwrap();
    // Lose the result of every frame until the destroy has been taken.
    let mut taken = false;
    for _ in 0..50 {
        w.sim.lose_next_result();
        w.tick().unwrap();
        if objects_of(&w, &m, id).is_empty() {
            taken = true;
            break;
        }
    }
    assert!(taken, "DCS did not take the destroy");
    for u in &m {
        assert!(
            !w.rec.binding().values().any(|b| b.unit == *u),
            "unit {} still bound",
            u.0
        );
    }
    ticks(&mut w, 300);
    assert!(!gone(&w, id), "purged while the destroy was unanswered");
    w.apply(&Op::Reattach).unwrap();
    assert!(
        until(&mut w, 800, |w| gone(w, id)),
        "not purged after the HELLO"
    );
    assert!(objects_of(&w, &m, id).is_empty());
    w.settle().unwrap();
}

fn a_mark(text: &str) -> MarkerSpec {
    MarkerSpec::Mark {
        theater: THEATER.into(),
        at: Point {
            x: 100.0,
            y: 5.0,
            z: 200.0,
        },
        text: text.into(),
        read_only: false,
    }
}

/// A removed mark is purged after `removed_keep_ms`, once DCS dropped it and
/// no command for it is in flight, and not before.
#[test]
fn a_removed_mark_is_purged_after_its_keep_and_not_before() {
    let mut w = world(0, 0, 5_000, 20_000);
    let (id, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, a_mark("rally"))
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.mark(id.0).is_some(), "set in DCS");
    w.store.remove_marker(id).unwrap();
    let removed_at = w.store.now_ms();
    ticks(&mut w, 10);
    assert!(w.sim.mark(id.0).is_none(), "DCS dropped it");
    // Inside the keep: kept.
    while w.store.now_ms() < removed_at + 4_000 {
        w.tick().unwrap();
        assert!(w.store.marker(id).is_some(), "purged inside its keep");
    }
    assert!(
        until(&mut w, 100, |w| w.store.marker(id).is_none()),
        "the mark was not purged"
    );
    assert!(
        w.store.now_ms() >= removed_at + 5_000,
        "purged {} ms after the removal",
        w.store.now_ms() - removed_at
    );
    assert_eq!(purged_entries(&w, id), 1);
    w.settle().unwrap();
}

/// Drift destroys of purged names spare an object of another campaign's
/// name, and an adopted unit whose ID has no row.
#[test]
fn drift_destroys_spare_foreign_and_adopted_objects() {
    let mut w = world(1, 0, 1_000, 20_000);
    // (a) another campaign's group, with IDs below the counter that have no
    // row here.
    let g = w.store.adopt_id().unwrap();
    let us: Vec<u64> = (0..3).map(|_| w.store.adopt_id().unwrap().0).collect();
    assert!(g.0 < w.store.next_entity());
    assert!(w.store.group(g).is_none());
    w.sim.add_campaign_group(FOREIGN_CAMPAIGN, g.0, &us);
    // (b) an adopted unit and its group.
    let object = w.sim.birth_client_unit_in("10", 77, "F-16C_50");
    ticks(&mut w, 5);
    let (unit, group) = w
        .rec
        .adopt(
            &mut w.store,
            &mut w.sim as &mut dyn SimAdapter,
            &Unmanaged {
                object,
                group: 77,
                entity: 0,
                group_entity: 0,
            },
        )
        .expect("adopted");
    assert!(w.store.unit(EntityId(unit)).is_none());
    assert!(w.store.group(EntityId(group)).is_none());
    let before = w.store.purge_drops().1;
    let counted = w.rec.counters().orphan_destroys;
    let objects = w.sim.objects().len();
    ticks(&mut w, 600);
    assert_eq!(w.store.purge_drops().1, before, "a drift destroy");
    assert_eq!(w.rec.counters().orphan_destroys, counted);
    assert_eq!(w.sim.objects().len(), objects, "an object left DCS");
    assert_eq!(w.sim.platform_groups(FOREIGN_CAMPAIGN).len(), 1);
    assert!(
        w.sim.objects().iter().any(|(o, _, _)| *o == object),
        "the adopted unit left DCS"
    );
}

/// A death event of an adopted unit, whose ID has no row, is not a purged
/// unit's late death: neither counter moves.
#[test]
fn an_adopted_units_death_is_not_counted_as_a_purged_death() {
    let mut w = world(1, 0, 1_000, 20_000);
    let object = w.sim.birth_client_unit_in("10", 77, "F-16C_50");
    ticks(&mut w, 5);
    let (unit, _group) = w
        .rec
        .adopt(
            &mut w.store,
            &mut w.sim as &mut dyn SimAdapter,
            &Unmanaged {
                object,
                group: 77,
                entity: 0,
                group_entity: 0,
            },
        )
        .expect("adopted");
    assert!(w.store.unit(EntityId(unit)).is_none());
    ticks(&mut w, 20);
    let deaths = w.rec.counters().purged_deaths;
    let drops = w.store.purge_drops().0;
    w.sim.kill(object);
    ticks(&mut w, 300);
    assert!(
        w.sim.objects().iter().all(|(o, _, _)| *o != object),
        "the unit is gone from the fake"
    );
    assert_eq!(w.rec.counters().purged_deaths, deaths, "counters");
    assert_eq!(w.store.purge_drops().0, drops, "store drops");
}

/// A clock that went an hour ahead for one step and came back does not stop
/// the purge: the pass re-arms at once, so a removed group held by a guard
/// is purged soon after the guard clears.
#[test]
fn a_clock_jump_undone_does_not_stop_the_purge() {
    let mut w = world_over(
        new_store(1, 0),
        Config {
            enum_period: 1_000_000,
            ..config()
        },
        rec_config(1_000, 20_000, LostPolicy::Report),
    );
    ticks(&mut w, 60);
    let id = groups(&w)[0];
    let m = members(&w, id);
    w.store.remove_entity(id).unwrap();
    // The keep passes and the objects are destroyed; no enumeration since.
    ticks(&mut w, 300);
    assert!(objects_of(&w, &m, id).is_empty(), "destroyed");
    assert!(!gone(&w, id), "purged with no enumeration since");
    let back = w.clock_ms;
    w.clock_ms = back + 3_600_000;
    w.tick().unwrap();
    w.clock_ms = back;
    ticks(&mut w, 5);
    assert!(!gone(&w, id), "purged with the guard held");
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    assert!(
        until(&mut w, 300, |w| gone(w, id)),
        "not purged after the guard cleared"
    );
    w.settle().unwrap();
}

/// A virtual group of one unit that the campaign simulates: DCS holds
/// nothing of it.
fn virtual_group(n: u64) -> NewGroup {
    NewGroup {
        authority: Authority::Campaign,
        units: vec![NewUnit::new("M-113", UnitState::default())],
        label: format!("v {n}"),
        ..spec(n)
    }
}

/// The values of `read` after each of `n` frames, with repeats dropped.
fn steps_of(w: &mut World, n: usize, read: impl Fn(&World) -> u64) -> Vec<u64> {
    let mut out = vec![read(w)];
    for _ in 0..n {
        w.tick().unwrap();
        let v = read(w);
        if out.last() != Some(&v) {
            out.push(v);
        }
    }
    out
}

/// One pass purges at most `PURGE_BATCH` removed groups; later passes finish
/// the backlog.
#[test]
fn a_purge_backlog_leaves_a_batch_a_pass() {
    let mut w = world(0, 0, 1_000, 20_000);
    let ids: Vec<EntityId> = (0..2_500)
        .map(|n| w.store.create_group(virtual_group(n)).unwrap())
        .collect();
    for id in &ids {
        w.store.remove_entity(*id).unwrap();
    }
    let seen = steps_of(&mut w, 600, |w| w.store.purged_expired().0);
    assert_eq!(
        seen,
        vec![0, 1_000, 2_000, 2_500],
        "purged after each frame"
    );
    assert!(ids.iter().all(|id| gone(&w, *id)));
    w.settle().unwrap();
}

/// One pass expires at most `PURGE_BATCH` dead groups; later passes finish
/// the backlog. The removal that follows is held by a long keep. The groups
/// are live units of the fake, killed there, so the checks hold.
#[test]
fn an_expiry_backlog_leaves_a_batch_a_pass() {
    let mut w = world_over(
        new_store(0, 0),
        config(),
        RecConfig {
            materialize_per_frame: 500,
            ..rec_config(3_600_000, 600_000, LostPolicy::Report)
        },
    );
    ticks(&mut w, 60);
    let ids: Vec<EntityId> = (0..2_500)
        .map(|n| {
            w.store
                .create_group(NewGroup {
                    units: spec(n).units.into_iter().take(1).collect(),
                    ..spec(n)
                })
                .unwrap()
        })
        .collect();
    assert!(
        until(&mut w, 1_500, |w| w.sim.objects().len() >= 2_500),
        "{} objects",
        w.sim.objects().len()
    );
    let campaign = w.store.campaign().0;
    for (o, c, _) in w.sim.objects() {
        if c == campaign {
            w.sim.kill(o);
        }
    }
    assert!(
        until(&mut w, 200, |w| ids.iter().all(|id| w
            .store
            .group(*id)
            .unwrap()
            .state
            == State::Dead)),
        "not all dead"
    );
    // Every group is older than its keep at the next pass.
    w.clock_ms += 3_600_000;
    let seen = steps_of(&mut w, 100, |w| w.store.purged_expired().1);
    assert_eq!(
        seen,
        vec![0, 1_000, 2_000, 2_500],
        "expired after each frame"
    );
    assert_eq!(w.store.purged_expired().0, 0, "nothing purged");
    w.settle().unwrap();
}

/// A store whose group bodies carry no removal or death time stamps them at
/// the open: nothing is purged or expired before its keep after the open.
#[test]
fn groups_without_stamps_wait_a_keep_after_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = new_store(2, 0);
    store.set_now(START_MS);
    store.save_as(&path, record()).unwrap();
    // The checks read the journal: keep what the file holds too.
    store.keep_journal(true);
    let mut w = world_over(
        store,
        config(),
        rec_config(10_000, 5_000, LostPolicy::Report),
    );
    ticks(&mut w, 60);
    let (removed, dead) = (groups(&w)[0], groups(&w)[1]);
    let rm = members(&w, removed);
    w.store.remove_entity(removed).unwrap();
    for o in objects_of(&w, &members(&w, dead), dead) {
        w.sim.kill(o);
    }
    ticks(&mut w, 30);
    assert_eq!(w.store.group(dead).unwrap().state, State::Dead);
    assert!(objects_of(&w, &rm, removed).is_empty(), "destroyed");
    w.store.forget_stamps();
    w.store.commit().unwrap();
    close_store(&mut w);
    let mut s = Store::open(&path, 10_000).unwrap();
    s.keep_journal(true);
    assert!(s.group(removed).unwrap().removed_at_ms.is_none());
    assert!(s.group(dead).unwrap().dead_at_ms.is_none());
    w.sidecar_of(s);
    w.store.set_now(w.clock_ms);
    let opened = w.clock_ms;
    let (mut expired_at, mut purged_at) = (None, None);
    for _ in 0..400 {
        w.tick().unwrap();
        let t = w.clock_ms - opened;
        if expired_at.is_none() && w.store.group(dead).is_none_or(|g| g.removed) {
            expired_at = Some(t);
        }
        if purged_at.is_none() && gone(&w, removed) {
            purged_at = Some(t);
        }
    }
    let (e, p) = (expired_at.expect("expired"), purged_at.expect("purged"));
    assert!(e >= 5_000, "the dead group expired {e} ms after the open");
    assert!(
        p >= 10_000,
        "the removed group was purged {p} ms after the open"
    );
    w.settle().unwrap();
}

/// Close the world's store and drop its connection, leaving a copy with no
/// file in its place.
fn close_store(w: &mut World) {
    w.store.close().unwrap();
    let copy = w.store.clone();
    drop(std::mem::replace(&mut w.store, copy));
}

/// Copy the store file over `path` the way the offline restore does.
fn restore_over(path: &Path, backup: &Path, aside: &Path) {
    let next = nanoprobe_sidecar::store::next_entity_of(path).unwrap();
    let rev = nanoprobe_sidecar::store::revision_of(path).unwrap();
    let slot = nanoprobe_sidecar::store::shell_slot_of(path).unwrap();
    nanoprobe_sidecar::store::backup_file(path, aside).unwrap();
    for wal in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(wal);
        let _ = std::fs::remove_file(p);
    }
    std::fs::copy(backup, path).unwrap();
    nanoprobe_sidecar::store::rebase_world(path, next, rev, slot).unwrap();
}

/// Objects of a name the restored store does not hold are destroyed as
/// drift, and no ID is given twice.
#[test]
fn objects_of_a_purged_name_are_destroyed_as_drift() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let backup = dir.path().join("backup.sqlite");
    let aside = dir.path().join("aside.sqlite");
    let mut store = new_store(1, 0);
    store.set_now(START_MS);
    store.save_as(&path, record()).unwrap();
    // The checks read the journal: keep what the file holds too.
    store.keep_journal(true);
    let mut w = world_over(
        store,
        config(),
        rec_config(1_000, 20_000, LostPolicy::Report),
    );
    ticks(&mut w, 60);
    let a = groups(&w)[0];
    let am = members(&w, a);
    w.store.commit().unwrap();
    nanoprobe_sidecar::store::backup_file(&path, &backup).unwrap();
    let c = w.store.create_group(spec(7)).unwrap();
    assert!(c.0 > a.0);
    ticks(&mut w, 60);
    let cm = members(&w, c);
    assert_eq!(objects_of(&w, &cm, c).len(), 3, "C is live");
    let next_with_c = w.store.next_entity();
    close_store(&mut w);
    restore_over(&path, &backup, &aside);
    let mut s = Store::open(&path, 10_000).unwrap();
    s.keep_journal(true);
    assert!(s.group(c).is_none(), "the backup has no C");
    assert!(s.next_entity() >= next_with_c, "the counter is the larger");
    w.sidecar_of(s);
    assert!(
        until(&mut w, 800, |w| objects_of(w, &cm, c).is_empty()),
        "C's objects were not destroyed"
    );
    assert!(w.rec.counters().orphan_destroys > 0);
    assert_eq!(objects_of(&w, &am, a).len(), 3, "A is untouched");
    assert_eq!(w.store.group(a).unwrap().state, State::Live);
    let d = w.store.create_group(spec(9)).unwrap();
    assert!(d.0 > c.0, "ID {} reused (C was {})", d.0, c.0);
    for u in &w.store.group(d).unwrap().members {
        assert!(u.0 > c.0);
    }
    w.settle().unwrap();
}

/// The export's lines as text. Keys are written in sorted order, so an
/// entity line ends with its ID and kind.
fn export_lines(store: &Path, events: &Path) -> Vec<String> {
    let mut out = Vec::new();
    nanoprobe_sidecar::export::export(CAMPAIGN, store, events, &mut |l| {
        out.push(String::from_utf8(l).expect("utf8 line"));
        true
    })
    .unwrap();
    out
}

fn has_entity_line(lines: &[String], id: EntityId) -> bool {
    let tail = format!("\"id\":{},\"kind\":\"entity\"}}", id.0);
    lines.iter().any(|l| l.trim_end().ends_with(&tail))
}

/// After a purge the export has no entity line for the ID and has a
/// `purged` journal line; a backup from before it, restored, brings the
/// entity back removed, and it is purged again.
#[test]
fn export_and_restore_after_a_purge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let backup = dir.path().join("backup.sqlite");
    let aside = dir.path().join("aside.sqlite");
    let mut store = new_store(2, 0);
    store.set_now(START_MS);
    store.save_as(&path, record()).unwrap();
    // The checks read the journal: keep what the file holds too.
    store.keep_journal(true);
    let mut w = world_over(
        store,
        config(),
        rec_config(1_000, 20_000, LostPolicy::Report),
    );
    ticks(&mut w, 60);
    let id = groups(&w)[0];
    let m = members(&w, id);
    w.store.remove_entity(id).unwrap();
    w.store.commit().unwrap();
    // Backed up after the removal, before the purge.
    nanoprobe_sidecar::store::backup_file(&path, &backup).unwrap();
    assert!(until(&mut w, 600, |w| gone(w, id)), "not purged");
    w.store.commit().unwrap();
    let events = dir.path().join("events.sqlite");
    let lines = export_lines(&path, &events);
    assert!(
        lines.iter().any(|l| l.contains("\"kind\":\"entity\"")),
        "the other group's lines"
    );
    assert!(!has_entity_line(&lines, id), "the group's line");
    for u in &m {
        assert!(!has_entity_line(&lines, *u), "unit {} line", u.0);
    }
    let purged = format!("\"entity\":{},\"entry\":\"purged\"", id.0);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&purged) && l.contains("\"kind\":\"journal\"")),
        "no purged journal line"
    );
    // Restore the backup from before the purge.
    close_store(&mut w);
    restore_over(&path, &backup, &aside);
    let mut s = Store::open(&path, 10_000).unwrap();
    s.keep_journal(true);
    let g = s.group(id).expect("the entity is back");
    assert!(g.removed, "back removed");
    w.sidecar_of(s);
    assert!(until(&mut w, 1_000, |w| gone(w, id)), "not purged again");
    assert_eq!(purged_entries(&w, id), 1);
    w.settle().unwrap();
}

/// Seeded fault schedules with the purge and the expiry active.
#[test]
fn seeded_faults_with_purge_and_expiry_pass_every_check() {
    let mut total = (0, 0);
    for seed in [0x51, 0x2a7, 0x9d3f] {
        let policy =
            [LostPolicy::Report, LostPolicy::Respawn, LostPolicy::Dead][(seed % 3) as usize];
        let mut w = world_over(
            new_store(6, 2),
            Config {
                event_queue: 6,
                ..Config::default()
            },
            rec_config(2_000, 5_000, policy),
        );
        let mut s = Schedule::new(seed);
        let mut puts = 0;
        for i in 0..400 {
            if i % 20 == 0 {
                let live = groups(&w);
                if let Some(id) = live.get(i / 20 % live.len().max(1)) {
                    w.store.remove_entity(*id).unwrap();
                }
                puts += 1;
                w.store.create_group(spec(puts)).unwrap();
            }
            // Every 50 ops one group dies whole, so the expiry runs too.
            if i % 50 == 25 {
                let c = w.store.campaign().0;
                let live = groups(&w);
                if let Some(id) = live.get(i / 50 % live.len().max(1)) {
                    let members = w.store.group(*id).expect("group").members.clone();
                    for (o, oc, e) in w.sim.objects() {
                        if oc == c && members.contains(&EntityId(e)) {
                            w.sim.kill(o);
                        }
                    }
                }
            }
            let op = s.op();
            if targets_removed(&w, &op) {
                continue;
            }
            if let Err(v) = w.apply(&op) {
                panic!("seed {seed:#x}, op {i} {op:?}: {}", v.0);
            }
        }
        if let Err(v) = w.settle() {
            panic!("seed {seed:#x}, settling: {}", v.0);
        }
        let (purged, expired) = w.store.purged_expired();
        eprintln!("seed {seed:#x}: purged {purged}, expired {expired}");
        assert!(purged > 0, "seed {seed:#x}: the faults left nothing purged");
        total = (total.0 + purged, total.1 + expired);
    }
    assert!(total.1 > 0, "no seed expired a dead group: {total:?}");
}

/// Whether the harness would apply `op` to a removed group, which the store
/// refuses (the harness picks from every group it holds).
fn targets_removed(w: &World, op: &Op) -> bool {
    let all: Vec<_> = w.store.groups().collect();
    let groups: Vec<_> = all.iter().filter(|g| g.kind == Kind::Group).collect();
    let removed = |v: &[&nanoprobe_sidecar::store::Group], i: u16| {
        !v.is_empty() && v[usize::from(i) % v.len()].removed
    };
    let all_refs: Vec<_> = all.iter().collect();
    let all_refs: Vec<&nanoprobe_sidecar::store::Group> = all_refs.into_iter().copied().collect();
    let groups: Vec<&nanoprobe_sidecar::store::Group> = groups.into_iter().copied().collect();
    match *op {
        Op::Authority(i, _) | Op::BreakSpec(i) | Op::FixSpec(i) => removed(&all_refs, i),
        Op::Order(i) => removed(&groups, i),
        Op::Attack(i, j) => removed(&groups, i) || removed(&groups, j),
        _ => false,
    }
}
