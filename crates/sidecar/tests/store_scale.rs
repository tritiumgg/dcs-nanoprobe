//! The store's write times at 10,000 groups (PLAN S20, the persistence
//! review's dirty-set and `unit_state` items). Ignored in the normal run;
//! run it by hand in release:
//!
//! ```text
//! cargo test --release -p nanoprobe-sidecar --test store_scale -- --ignored --nocapture
//! ```
//!
//! It prints one JSON line per measure. The numbers and the reading of
//! them are in docs/results/2026-11-17-s20-virtualization.md.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sidecar::store::{
    Authority, CampaignRecord, CampaignState, NewGroup, NewUnit, Point, Store, UnitState,
};

const GROUPS: usize = 10_000;

fn record() -> CampaignRecord {
    CampaignRecord {
        name: "scale".into(),
        state: CampaignState::Active,
        theaters: vec!["Caucasus".into()],
        theater: "Caucasus".into(),
        clock_ms: None,
        created_ms: 0,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

fn group(g: usize, members: usize) -> NewGroup {
    NewGroup {
        theater: "Caucasus".into(),
        country: 2,
        category: 2,
        coalition: 2,
        units: (0..members)
            .map(|u| NewUnit {
                label: format!("tank {g}.{u}"),
                skill: "Average".into(),
                ..NewUnit::new(
                    "M-113",
                    UnitState {
                        at: Point {
                            x: (g % 100) as f64 * 500.0,
                            y: 0.0,
                            z: (g / 100) as f64 * 500.0 + u as f64 * 20.0,
                        },
                        ..UnitState::default()
                    },
                )
            })
            .collect(),
        authority: Authority::Campaign,
        label: format!("platoon {g}"),
        extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The median and the largest of `n` timed runs of `f`, in milliseconds.
fn timed(n: usize, mut f: impl FnMut(usize)) -> (f64, f64) {
    let mut v: Vec<f64> = (0..n)
        .map(|i| {
            let t = Instant::now();
            f(i);
            ms(t.elapsed())
        })
        .collect();
    v.sort_by(f64::total_cmp);
    (v[n / 2], v[n - 1])
}

fn measure(members: usize) {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("campaign.sqlite");
    let mut store = Store::create_on_disk(&path, CampaignId(1), record(), 100_000).expect("create");
    let t = Instant::now();
    let mut ids = Vec::with_capacity(GROUPS);
    for g in 0..GROUPS {
        ids.push(store.create_group(group(g, members)).expect("group"));
    }
    store.commit().expect("commit");
    let create = ms(t.elapsed());
    let units: Vec<EntityId> = ids
        .iter()
        .flat_map(|id| store.group(*id).expect("group").members.clone())
        .collect();
    let (idle, idle_max) = timed(50, |_| store.commit().expect("commit"));
    let (one, one_max) = timed(50, |i| {
        let a = if i % 2 == 0 {
            Authority::Sim
        } else {
            Authority::Campaign
        };
        store.set_authority(ids[i], a).expect("authority");
        store.commit().expect("commit");
    });
    // A step's commit while 2,000 groups' positions move between
    // checkpoints: sampled state is left to the checkpoint.
    let moving = 2_000 * members;
    let mut tick = 0.0;
    let mut nudge = |store: &mut Store, n: usize| {
        tick += 1.0;
        for u in &units[..n] {
            store.observe_position(
                *u,
                Point {
                    x: tick,
                    y: 0.0,
                    z: u.0 as f64,
                },
            );
        }
    };
    let (sampled, sampled_max) = timed(20, |_| {
        nudge(&mut store, moving);
        store.commit().expect("commit");
    });
    let mut at = 0;
    let (cp_part, cp_part_max) = timed(10, |_| {
        nudge(&mut store, moving);
        at += 5_000;
        store.checkpoint(at, Vec::new()).expect("checkpoint");
    });
    let all = units.len();
    let (cp_all, cp_all_max) = timed(10, |_| {
        nudge(&mut store, all);
        at += 5_000;
        store.checkpoint(at, Vec::new()).expect("checkpoint");
    });
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    println!(
        "{}",
        serde_json::json!({
            "groups": GROUPS,
            "units": all,
            "create_and_first_commit_ms": create,
            "commit_nothing_changed_ms": [idle, idle_max],
            "commit_one_authority_change_ms": [one, one_max],
            "commit_with_positions_moving_ms": {"units": moving, "p50_max": [sampled, sampled_max]},
            "checkpoint_ms": {"units": moving, "p50_max": [cp_part, cp_part_max]},
            "checkpoint_all_moved_ms": {"units": all, "p50_max": [cp_all, cp_all_max]},
            "file_bytes": size,
        })
    );
}

#[test]
#[ignore = "a measurement; run by hand in release"]
fn store_writes_at_10_000_groups() {
    measure(1);
    measure(3);
}
