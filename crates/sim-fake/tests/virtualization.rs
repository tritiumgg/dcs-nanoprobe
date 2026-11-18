//! PLAN S20's host exit test: a test controller keeps 2,000 of a
//! 10,000-group world materialized around a moving point, on the fake sim.
//! That size runs in release (`mise run virtual`, part of `mise run check`);
//! the normal test run keeps 200 of 1,000.
//!
//! The controller is the only one that changes authority. At each position
//! of the point it asks the store's grid what is near (the interest query),
//! takes the 2,000 groups nearest, and moves authority to DCS for those and
//! to the campaign for the rest. Then it runs frames until DCS holds
//! exactly those groups. Checked on every frame:
//!
//! - every group's authority is the one the controller last set;
//! - no unit entity is in DCS twice, and no group goes `lost`,
//!   `unspawnable` or `dead`;
//! - at most `materialize_per_frame` spawns reach DCS;
//! - a unit leaving DCS is captured with the position it had (within one
//!   frame's movement), its heading and its life;
//! - a unit entering DCS is spawned at the position and heading the store
//!   held, and after its `DAMAGE` has the life the store held.
//!
//! At the end every group and unit keeps its `EntityId`, every unit that
//! left or entered DCS was checked, and the counts of each match the
//! controller's commands. One group in ten has a route, so positions change
//! between handovers; one unit in five starts at 60 % life. After each move
//! one live unit in seven turns and is hit in DCS, so a capture that kept
//! the stored heading or life would fail.
//!
//! The owner's overhead targets are not measured here: the fake sim has no
//! agent. The server run (`cargo xtask persistence <target> --only virtual`)
//! measures them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::intents::{Intent, Waypoint};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::reconciler::Config as RecConfig;
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sidecar::store::{Authority, NewGroup, NewUnit, Point, Store, UnitState};
use nanoprobe_sim_fake::Config;
use nanoprobe_sim_fake::harness::{CAMPAIGN, THEATER, World};

const MEMBERS: usize = 2;
/// Groups on a square grid this many meters apart.
const SPACING: f64 = 1_000.0;
/// Meters a unit on a route moves per frame.
const STEP_M: f64 = 10.0;
/// The points the controller centers on, in order: across the world and
/// back, so groups leave and come back.
/// As fractions of the world's side.
const PATH: [(f64, f64); 6] = [
    (0.3, 0.3),
    (0.4, 0.35),
    (0.55, 0.5),
    (0.7, 0.7),
    (0.5, 0.45),
    (0.3, 0.3),
];

fn life_percent(unit_index: usize) -> f64 {
    if unit_index.is_multiple_of(5) {
        60.0
    } else {
        100.0
    }
}

fn world(groups: usize) -> (World, Vec<EntityId>, HashMap<EntityId, f64>) {
    let mut store = Store::new(CampaignId(CAMPAIGN), 10_000);
    let side = (groups as f64).sqrt().ceil() as usize;
    let mut ids = Vec::with_capacity(groups);
    let mut life = HashMap::new();
    for g in 0..groups {
        let (gx, gz) = ((g % side) as f64 * SPACING, (g / side) as f64 * SPACING);
        let units = (0..MEMBERS)
            .map(|u| NewUnit {
                label: format!("tank {g}.{u}"),
                skill: "Average".into(),
                ..NewUnit::new(
                    "M-113",
                    UnitState {
                        at: Point {
                            x: gx,
                            y: 0.0,
                            z: gz + u as f64 * 20.0,
                        },
                        // A distinct heading for each unit, below 2 pi.
                        heading: ((g * MEMBERS + u) % 628) as f64 / 100.0,
                        ..UnitState::default()
                    },
                )
            })
            .collect();
        let id = store
            .create_group(NewGroup {
                theater: THEATER.into(),
                country: 2,
                category: 2,
                coalition: 2,
                units,
                authority: Authority::Campaign,
                label: format!("platoon {g}"),
                extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
            })
            .expect("ids");
        let members = store.group(id).expect("group").members.clone();
        for (i, m) in members.iter().enumerate() {
            let p = life_percent(g * MEMBERS + i);
            if p < 100.0 {
                store.set_spawn_life(*m, p).expect("unit");
            }
            life.insert(*m, p);
        }
        if g.is_multiple_of(10) {
            store
                .set_order(
                    id,
                    Intent::Route {
                        waypoints: vec![
                            Waypoint {
                                x: gx + 2_000.0,
                                z: gz,
                            },
                            Waypoint {
                                x: gx + 2_000.0,
                                z: gz + 2_000.0,
                            },
                        ],
                        speed: 10.0,
                        on_road: false,
                    },
                )
                .expect("group");
        }
        ids.push(id);
    }
    let w = World::with_store(
        store,
        Config {
            step_m: STEP_M,
            ..Config::default()
        },
        RecConfig::default(),
    );
    (w, ids, life)
}

/// The interest query: the `KEEP` groups nearest the point, from the
/// store's grid, widening the radius until it finds enough.
fn nearest(store: &Store, x: f64, z: f64, keep: usize) -> BTreeSet<EntityId> {
    let mut radius = SPACING * 10.0;
    loop {
        let mut by_group: BTreeMap<EntityId, f64> = BTreeMap::new();
        for u in store.grid().within(THEATER, x, z, radius) {
            let Some(unit) = store.unit(u) else { continue };
            let d = (unit.state.at.x - x).hypot(unit.state.at.z - z);
            let e = by_group.entry(unit.group).or_insert(f64::MAX);
            *e = e.min(d);
        }
        if by_group.len() >= keep {
            let mut v: Vec<(f64, EntityId)> = by_group.into_iter().map(|(g, d)| (d, g)).collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            return v.into_iter().take(keep).map(|(_, g)| g).collect();
        }
        radius *= 1.5;
    }
}

#[derive(Debug, Default)]
struct Tally {
    moves: usize,
    materialized: usize,
    dematerialized: usize,
    units_in_checked: usize,
    units_out_checked: usize,
    most_spawns_in_a_frame: usize,
    frames: usize,
    largest_position_gap_in_m: f64,
    largest_position_gap_out_m: f64,
    /// The farthest a unit was captured from where it was created: units
    /// on a route move between handovers.
    largest_travel_m: f64,
    /// Units captured after DCS turned and hit them.
    changed_in_dcs_captured: usize,
}

#[test]
#[ignore = "runs in release: mise run virtual"]
fn a_controller_keeps_2000_of_10000_materialized_around_a_moving_point() {
    keep_around_a_moving_point(10_000, 2_000);
}

#[test]
fn a_controller_keeps_200_of_1000_materialized_around_a_moving_point() {
    keep_around_a_moving_point(1_000, 200);
}

fn keep_around_a_moving_point(groups: usize, keep: usize) {
    // Frames allowed for one handover of the whole set.
    let settle_frames = 3 * keep + 2_000;
    let extent = (groups as f64).sqrt().ceil() * SPACING;
    let (mut w, ids, mut life) = world(groups);
    let start_at: HashMap<EntityId, (f64, f64)> = ids
        .iter()
        .flat_map(|g| w.store.group(*g).expect("group").members.clone())
        .map(|m| {
            let u = w.store.unit(m).expect("unit");
            (m, (u.state.at.x, u.state.at.z))
        })
        .collect();
    let units_at_start: BTreeSet<EntityId> = ids
        .iter()
        .flat_map(|g| w.store.group(*g).expect("group").members.clone())
        .collect();
    let mut commanded: HashMap<EntityId, Authority> =
        ids.iter().map(|g| (*g, Authority::Campaign)).collect();
    // What the store held of each unit when the controller asked for its
    // group: checked when the unit appears in DCS.
    let mut expected_in: HashMap<u64, (f64, f64, f64)> = HashMap::new();
    let mut tally = Tally::default();
    let per_frame = RecConfig::default().materialize_per_frame;
    let mut truth_before: HashMap<u64, (f64, f64, f64)> = HashMap::new();
    // Units DCS turned and hit since their last spawn.
    let mut changed: BTreeSet<u64> = BTreeSet::new();

    for (step, &(fx, fz)) in PATH.iter().enumerate() {
        let (px, pz) = (fx * extent, fz * extent);
        let want = nearest(&w.store, px, pz, keep);
        assert_eq!(want.len(), keep);
        for g in &ids {
            let a = if want.contains(g) {
                Authority::Sim
            } else {
                Authority::Campaign
            };
            if commanded[g] != a {
                if a == Authority::Sim {
                    tally.materialized += 1;
                    for m in &w.store.group(*g).expect("group").members {
                        let u = w.store.unit(*m).expect("unit");
                        expected_in.insert(m.0, (u.state.at.x, u.state.at.z, u.state.heading));
                    }
                } else {
                    tally.dematerialized += 1;
                }
                w.store.set_authority(*g, a).expect("group");
                commanded.insert(*g, a);
            }
        }
        tally.moves += 1;
        let mut settled = false;
        for _ in 0..settle_frames {
            let spawns_before = w.sim.ledger().spawns.len();
            let leaving: Vec<EntityId> = ids
                .iter()
                .copied()
                .filter(|g| w.store.group(*g).expect("group").state == State::Dematerializing)
                .collect();
            w.tick().unwrap_or_else(|v| panic!("move {step}: {}", v.0));
            tally.frames += 1;
            let spawned = w.sim.ledger().spawns.len() - spawns_before;
            assert!(
                spawned <= per_frame,
                "move {step}: {spawned} spawns in one frame, budget {per_frame}"
            );
            tally.most_spawns_in_a_frame = tally.most_spawns_in_a_frame.max(spawned);

            let truth: HashMap<u64, (f64, f64, f64)> = w
                .sim
                .unit_states()
                .into_iter()
                .filter(|t| t.campaign == CAMPAIGN)
                .map(|t| (t.entity, (t.x, t.z, t.heading)))
                .collect();
            // No unit entity is in DCS twice.
            let held = w
                .sim
                .unit_states()
                .into_iter()
                .filter(|t| t.campaign == CAMPAIGN)
                .count();
            assert_eq!(held, truth.len(), "move {step}: a unit is in DCS twice");

            // Units that entered DCS this frame.
            for (e, &(x, z, h)) in &truth {
                if let Some((ex, ez, eh)) = expected_in.remove(e) {
                    let gap = (x - ex).hypot(z - ez);
                    assert!(
                        gap <= STEP_M + 1e-6,
                        "move {step}: unit {e} spawned {gap} m from its stored position"
                    );
                    assert!(
                        (h - eh).abs() < 1e-9,
                        "move {step}: unit {e} spawned at heading {h}, stored {eh}"
                    );
                    tally.largest_position_gap_in_m = tally.largest_position_gap_in_m.max(gap);
                    tally.units_in_checked += 1;
                }
            }
            // Units of groups that left DCS this frame: captured as DCS
            // last held them.
            for g in leaving {
                let grp = w.store.group(g).expect("group");
                if grp.state != State::Virtual {
                    continue;
                }
                for m in &grp.members {
                    let u = w.store.unit(*m).expect("unit");
                    assert!(
                        !truth.contains_key(&m.0),
                        "move {step}: {m:?} left but is in DCS"
                    );
                    let (x, z, h) = *truth_before
                        .get(&m.0)
                        .expect("the unit was in DCS before it left");
                    let gap = (u.state.at.x - x).hypot(u.state.at.z - z);
                    assert!(
                        gap <= STEP_M + 1e-6,
                        "move {step}: unit {m:?} captured {gap} m from where DCS held it"
                    );
                    assert!(
                        (u.state.heading - h).abs() < 1e-9,
                        "move {step}: unit {m:?} captured at heading {}, DCS held {h}",
                        u.state.heading
                    );
                    let want_life = life[m] / 100.0;
                    assert!(
                        (f64::from(u.state.life) - want_life).abs() < 1e-6,
                        "move {step}: unit {m:?} captured with life {}, held {want_life}",
                        u.state.life
                    );
                    tally.largest_position_gap_out_m = tally.largest_position_gap_out_m.max(gap);
                    let (sx, sz) = start_at[m];
                    tally.largest_travel_m = tally
                        .largest_travel_m
                        .max((u.state.at.x - sx).hypot(u.state.at.z - sz));
                    tally.units_out_checked += 1;
                    if changed.remove(&m.0) {
                        tally.changed_in_dcs_captured += 1;
                    }
                }
            }
            truth_before = truth;

            // Only the controller changes authority; nothing is lost.
            for g in &ids {
                let grp = w.store.group(*g).expect("group");
                assert_eq!(
                    grp.authority, commanded[g],
                    "move {step}: the sidecar changed the authority of {g:?}"
                );
                assert!(
                    !matches!(grp.state, State::Lost | State::Unspawnable | State::Dead),
                    "move {step}: group {g:?} is {:?}",
                    grp.state
                );
            }

            let live: BTreeSet<EntityId> = ids
                .iter()
                .copied()
                .filter(|g| w.store.group(*g).expect("group").state == State::Live)
                .collect();
            let virt = ids
                .iter()
                .filter(|g| w.store.group(**g).expect("group").state == State::Virtual)
                .count();
            if live == want && virt == groups - keep && expected_in.is_empty() {
                settled = true;
                break;
            }
        }
        assert!(
            settled,
            "move {step}: not settled in {settle_frames} frames"
        );

        // Every live unit has the life the store held, set by its DAMAGE.
        let lives: HashMap<u64, f64> = w.sim.lives().into_iter().collect();
        for g in &want {
            for m in &w.store.group(*g).expect("group").members {
                let p = life[m];
                let got = lives.get(&m.0).copied().unwrap_or(100.0);
                // The store keeps life as an f32.
                assert!(
                    (got - p).abs() < 1e-4,
                    "move {step}: unit {m:?} lives at {got} %, held {p} %"
                );
            }
        }
        // DCS holds exactly the wanted groups' units.
        let in_dcs: BTreeSet<u64> = truth_before.keys().copied().collect();
        let wanted_units: BTreeSet<u64> = want
            .iter()
            .flat_map(|g| w.store.group(*g).expect("group").members.clone())
            .map(|m| m.0)
            .collect();
        assert_eq!(in_dcs, wanted_units, "move {step}: DCS holds other units");

        // DCS changes one live unit in seven: it turns and is hit.
        for g in &want {
            for m in w.store.group(*g).expect("group").members.clone() {
                if !(m.0 + step as u64).is_multiple_of(7) {
                    continue;
                }
                let (_, _, h) = truth_before[&m.0];
                let turned = (h + 1.0).rem_euclid(std::f64::consts::TAU);
                let hit = if life[&m] == 35.0 { 80.0 } else { 35.0 };
                assert!(w.sim.set_heading(m.0, turned) && w.sim.set_life_percent(m.0, hit));
                truth_before.get_mut(&m.0).expect("in DCS").2 = turned;
                life.insert(m, hit);
                changed.insert(m.0);
            }
        }
    }

    // Every group and unit keeps its EntityId.
    let groups_now: BTreeSet<EntityId> = w.store.groups().map(|g| g.id).collect();
    assert_eq!(groups_now, ids.iter().copied().collect());
    let units_now: BTreeSet<EntityId> = ids
        .iter()
        .flat_map(|g| w.store.group(*g).expect("group").members.clone())
        .collect();
    assert_eq!(units_now, units_at_start);
    assert_eq!(tally.units_out_checked, MEMBERS * tally.dematerialized);
    assert_eq!(tally.units_in_checked, MEMBERS * tally.materialized);
    assert!(
        tally.changed_in_dcs_captured > 0,
        "no unit changed in DCS was captured"
    );
    assert!(
        tally.largest_travel_m > 100.0,
        "no unit moved between handovers"
    );
    println!("{tally:#?}");
}

/// A world of `groups` groups, the first `live` of them materialized and
/// settled, with a reconciler of budget `per_frame`.
fn settled_world(groups: usize, live: usize, per_frame: usize) -> (World, Vec<EntityId>) {
    let (w, ids, _) = world(groups);
    let mut w = World::with_store(
        w.store,
        Config {
            step_m: STEP_M,
            ..Config::default()
        },
        RecConfig {
            materialize_per_frame: per_frame,
            ..RecConfig::default()
        },
    );
    for g in &ids[..live] {
        w.store.set_authority(*g, Authority::Sim).expect("group");
    }
    for _ in 0..(live * 4 + 2_000) {
        w.tick().expect("tick");
        if ids[..live]
            .iter()
            .all(|g| w.store.group(*g).expect("group").state == State::Live)
        {
            return (w, ids);
        }
    }
    panic!("{live} groups not live");
}

/// Run frames until every group of `want` is live, counting the spawns
/// that reach DCS in each frame. The largest count.
fn most_spawns_per_frame(w: &mut World, want: &[EntityId]) -> usize {
    let mut most = 0;
    for _ in 0..20_000 {
        let before = w.sim.ledger().spawns.len();
        w.tick().expect("tick");
        most = most.max(w.sim.ledger().spawns.len() - before);
        if want
            .iter()
            .all(|g| w.store.group(*g).expect("group").state == State::Live)
        {
            return most;
        }
    }
    panic!("the materializations did not complete");
}

/// The materialization budget (spec/world.md, "Authority"): with 300
/// capture-and-destroys written ahead of the spawns, no frame outside the
/// restore burst takes more than `materialize_per_frame` spawns, a larger
/// budget pipelines up to it, and every materialization completes.
#[test]
fn spawns_behind_destroys_stay_within_the_budget() {
    for per_frame in [1, 3] {
        let (mut w, ids) = settled_world(400, 300, per_frame);
        for g in &ids[..300] {
            w.store
                .set_authority(*g, Authority::Campaign)
                .expect("group");
        }
        let entering = &ids[300..350];
        for g in entering {
            w.store.set_authority(*g, Authority::Sim).expect("group");
        }
        let most = most_spawns_per_frame(&mut w, entering);
        assert!(
            most <= per_frame,
            "{most} spawns in one frame, budget {per_frame}"
        );
        assert_eq!(most, per_frame, "budget {per_frame} never reached");
    }
}

/// The restore burst stays uncapped: after a mission load that starts
/// paused, the groups to restore spawn more than `materialize_per_frame`
/// in a frame (spec/persistence.md, "Restore burst").
#[test]
fn the_restore_burst_is_not_capped_by_the_budget() {
    let (mut w, ids) = settled_world(100, 50, 1);
    w.sim.load_mission();
    for _ in 0..10 {
        if w.sim.running() {
            break;
        }
        w.sim.frame();
    }
    w.sim.pause();
    let mut most = 0;
    for _ in 0..200 {
        let before = w.sim.ledger().spawns.len();
        w.tick().expect("tick");
        most = most.max(w.sim.ledger().spawns.len() - before);
    }
    assert!(most > 1, "the burst spawned at most {most} in a frame");
    assert!(w.sim.ledger().bursts.contains(&(true, true)));
    assert!(
        ids[..50]
            .iter()
            .all(|g| w.store.group(*g).expect("group").state == State::Live)
    );
}
