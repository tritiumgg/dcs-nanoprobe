//! Growth measures for the sidecar: heap bytes and disk bytes per kept item,
//! and the cost of a driver step and of a store open as removed groups pile
//! up. Ignored in the normal run; run by hand in release:
//!
//! ```text
//! cargo test --release -p nanoprobe-sidecar --test growth_measure -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each measure prints one JSON line.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use nanoprobe_sidecar::eventfile::{EventFile, EventRow};
use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::reconciler::Config as RecConfig;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sidecar::store::{
    Authority, CampaignRecord, CampaignState, IdemRow, NewGroup, NewUnit, Point, Store, UnitState,
};
use nanoprobe_sim_fake::Config as SimConfig;
use nanoprobe_sim_fake::harness::{CAMPAIGN, THEATER, World};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every call forwards to `System`; the counter only tracks sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` came from `alloc` with this layout.
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: `p` came from `alloc` with layout `l`.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            LIVE.fetch_add(new as isize - l.size() as isize, Ordering::Relaxed);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn live() -> isize {
    LIVE.load(Ordering::Relaxed)
}

fn record() -> CampaignRecord {
    CampaignRecord {
        name: "growth".into(),
        state: CampaignState::Active,
        theaters: vec!["Caucasus".into()],
        theater: THEATER.into(),
        clock_ms: None,
        created_ms: 0,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

/// store_scale's group, with `authority`.
fn group(g: usize, members: usize, authority: Authority) -> NewGroup {
    NewGroup {
        theater: THEATER.into(),
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
        authority,
        label: format!("platoon {g}"),
        extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn timed_ms(n: usize, mut f: impl FnMut(usize)) -> f64 {
    median(
        (0..n)
            .map(|i| {
                let t = Instant::now();
                f(i);
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect(),
    )
}

fn sql(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open sql")
}

fn file_bytes(path: &Path) -> i64 {
    let c = sql(path);
    let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    pages * size
}

/// `(kind, rows, body bytes)` of a table with a `kind` column.
fn by_kind(path: &Path, table: &str) -> Vec<(String, i64, i64)> {
    let c = sql(path);
    let mut s = c
        .prepare(&format!(
            "SELECT kind, count(*), sum(length(body)) FROM {table} GROUP BY kind"
        ))
        .unwrap();
    s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn kinds_json(v: &[(String, i64, i64)]) -> serde_json::Value {
    serde_json::Value::Object(
        v.iter()
            .map(|(k, n, b)| {
                (
                    k.clone(),
                    serde_json::json!({"rows": n, "body_bytes": b, "avg_body": *b as f64 / *n as f64}),
                )
            })
            .collect(),
    )
}

fn say(v: serde_json::Value) {
    println!("{v}");
}

fn mem_store() -> Store {
    Store::new(CampaignId(1), 1_000)
}

// ---------------------------------------------------------------- 1. heap

/// Heap bytes per group in a store on disk, created and committed (so the
/// journal is written and dropped from memory), then removed and committed.
fn heap_per_group(members: usize) {
    const N: usize = 5_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("campaign.sqlite");
    let mut store = Store::create_on_disk(&path, CampaignId(1), record(), 1_000).unwrap();
    store.commit().unwrap();
    let base = live();
    let mut ids = Vec::with_capacity(N);
    for g in 0..N {
        ids.push(
            store
                .create_group(group(g, members, Authority::Campaign))
                .unwrap(),
        );
    }
    let before_commit = live();
    store.commit().unwrap();
    let live_group = live();
    for id in &ids {
        store.remove_entity(*id).unwrap();
    }
    let removed_uncommitted = live();
    store.commit().unwrap();
    let removed = live();
    say(serde_json::json!({
        "measure": "heap_per_group", "units": members, "n": N,
        "live_group_bytes_after_commit": (live_group - base) as f64 / N as f64,
        "live_group_bytes_before_commit_incl_journal_and_dirty": (before_commit - base) as f64 / N as f64,
        "removed_group_bytes_after_commit": (removed - base) as f64 / N as f64,
        "removed_group_bytes_before_commit": (removed_uncommitted - base) as f64 / N as f64,
    }));
    // The same for a store in memory, which keeps every journal entry.
    let mut m = mem_store();
    let base = live();
    let mut ids = Vec::new();
    for g in 0..N {
        ids.push(
            m.create_group(group(g, members, Authority::Campaign))
                .unwrap(),
        );
    }
    for id in &ids {
        m.remove_entity(*id).unwrap();
    }
    say(serde_json::json!({
        "measure": "heap_per_removed_group_in_memory_store_with_journal", "units": members, "n": N,
        "bytes": (live() - base) as f64 / N as f64,
        "journal_entries": m.entries().len(),
    }));
}

#[test]
#[ignore = "a measurement"]
fn m1_heap_per_group() {
    heap_per_group(1);
    heap_per_group(4);
}

// ---------------------------------------------------------------- 2. disk

#[test]
#[ignore = "a measurement"]
fn m2_disk_per_row() {
    const N: usize = 5_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("campaign.sqlite");
    let mut store = Store::create_on_disk(&path, CampaignId(1), record(), 1_000).unwrap();
    store.commit().unwrap();
    let f0 = file_bytes(&path);
    for members in [1usize, 4] {
        let f_before = file_bytes(&path);
        let ids: Vec<EntityId> = (0..N)
            .map(|g| {
                store
                    .create_group(group(g, members, Authority::Campaign))
                    .unwrap()
            })
            .collect();
        store.commit().unwrap();
        let f_created = file_bytes(&path);
        for id in &ids {
            store.remove_entity(*id).unwrap();
        }
        store.commit().unwrap();
        let f_removed = file_bytes(&path);
        say(serde_json::json!({
            "measure": "disk_group_rows", "units": members, "n": N,
            "file_growth_per_group_created": (f_created - f_before) as f64 / N as f64,
            "file_growth_per_group_removal_step": (f_removed - f_created) as f64 / N as f64,
        }));
    }
    let _ = f0;
    say(serde_json::json!({
        "measure": "disk_entity_rows_by_kind_total (N groups of 1 unit + N groups of 4 units)",
        "entity": kinds_json(&by_kind_entity(&path)),
        "journal": kinds_json(&by_kind(&path, "journal")),
        "journal_total_rows": sql(&path).query_row("SELECT count(*) FROM journal", [], |r| r.get::<_, i64>(0)).unwrap(),
    }));
    // The groups of one unit only, for the per-group and per-unit size.
    let dir1 = tempfile::tempdir().unwrap();
    let p1 = dir1.path().join("c.sqlite");
    let mut s1 = Store::create_on_disk(&p1, CampaignId(1), record(), 1_000).unwrap();
    s1.commit().unwrap();
    for g in 0..N {
        s1.create_group(group(g, 4, Authority::Campaign)).unwrap();
    }
    s1.commit().unwrap();
    say(serde_json::json!({
        "measure": "disk_entity_rows_4_unit_groups_only", "n": N,
        "entity": kinds_json(&by_kind_entity(&p1)),
        "journal": kinds_json(&by_kind(&p1, "journal")),
    }));

    // Idempotency rows: a 36 byte key, a 32 byte hash, a 120 byte response.
    let f_i0 = file_bytes(&path);
    for i in 0..N {
        store.put_idempotency(IdemRow {
            key: format!("{:08x}-1111-4222-8333-{:012x}", i, i * 7),
            at_ms: 1_700_000_000_000 + i as u64,
            request_hash: vec![0xab; 32],
            response: vec![0x5a; 120],
        });
        if i % 100 == 99 {
            store.commit().unwrap();
        }
    }
    store.commit().unwrap();
    let f_i1 = file_bytes(&path);
    let c = sql(&path);
    let (n, kb, hb, rb): (i64, i64, i64, i64) = c
        .query_row(
            "SELECT count(*), sum(length(key)), sum(length(request_hash)), sum(length(response)) FROM idempotency",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    say(serde_json::json!({
        "measure": "disk_idempotency_row", "rows": n,
        "payload_bytes_per_row": (kb + hb + rb + 8) as f64 / n as f64,
        "file_growth_per_row": (f_i1 - f_i0) as f64 / N as f64,
    }));

    // Event rows: a 120 byte body (an assumed size; the file overhead per row
    // is the figure to carry) and a source `boot:number`.
    let epath = dir.path().join("events.sqlite");
    let mut ef = EventFile::open(&epath, 1).unwrap();
    let e0 = file_bytes(&epath);
    for body_len in [120usize, 400] {
        let a = file_bytes(&epath);
        for i in 0..N {
            ef.push(EventRow {
                seq: ef.next_seq(),
                wall_ms: 1_700_000_000_000 + i as u64,
                source: Some(format!("{body_len}:{i}")),
                body: vec![0x33; body_len],
            });
            if i % 100 == 99 {
                ef.commit().unwrap();
            }
        }
        ef.commit().unwrap();
        let b = file_bytes(&epath);
        say(serde_json::json!({
            "measure": "disk_event_row", "body_len": body_len, "rows": N,
            "file_growth_per_row": (b - a) as f64 / N as f64,
            "overhead_beyond_body_and_source": (b - a) as f64 / N as f64 - body_len as f64,
        }));
    }
    let _ = e0;

    // The journal a running reconciler writes, by kind: 250 groups of 4
    // units settled on the fake sim, then a kill and removal of half.
    let wdir = tempfile::tempdir().unwrap();
    let wpath = wdir.path().join("campaign.sqlite");
    let mut ws = Store::create_on_disk(&wpath, CampaignId(CAMPAIGN), record(), 10_000).unwrap();
    for g in 0..250 {
        ws.create_group(group(g, 4, Authority::Sim)).unwrap();
    }
    ws.commit().unwrap();
    let mut w = World::with_store(
        ws,
        SimConfig {
            event_queue: 6,
            ..SimConfig::default()
        },
        RecConfig::default(),
    );
    w.settle().expect("settle");
    for _ in 0..50 {
        w.tick().expect("tick");
    }
    let ids: Vec<EntityId> = w.store.groups().map(|g| g.id).collect();
    for id in ids.iter().take(125) {
        w.store.remove_entity(*id).unwrap();
    }
    for _ in 0..800 {
        w.tick().expect("tick");
    }
    let j = by_kind(&wpath, "journal");
    let total_rows: i64 = j.iter().map(|x| x.1).sum();
    let total_body: i64 = j.iter().map(|x| x.2).sum();
    say(serde_json::json!({
        "measure": "disk_journal_from_running_reconciler (250 groups x4 units, 125 removed)",
        "by_kind": kinds_json(&j),
        "rows": total_rows,
        "avg_body": total_body as f64 / total_rows as f64,
        "file_bytes": file_bytes(&wpath),
    }));
}

fn by_kind_entity(path: &Path) -> Vec<(String, i64, i64)> {
    by_kind(path, "entity")
}

// ---------------------------------------------------------------- 3. step

fn add_removed(store: &mut Store, from: usize, to: usize, dead: bool) {
    for g in from..to {
        let id = store
            .create_group(group(10_000 + g, 4, Authority::Campaign))
            .unwrap();
        if dead {
            let members = store.group(id).unwrap().members.clone();
            for m in members {
                store.death(m).unwrap();
            }
            // Deaths alone leave a virtual group virtual: it is dead once
            // the lifecycle says so, and only then does the expiry see it.
            store
                .transition(id, nanoprobe_sidecar::lifecycle::Trigger::AllDead)
                .unwrap();
        } else {
            store.remove_entity(id).unwrap();
        }
    }
}

fn step_cost(dead: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("campaign.sqlite");
    let mut s = Store::create_on_disk(&path, CampaignId(CAMPAIGN), record(), 10_000).unwrap();
    for g in 0..250 {
        s.create_group(group(g, 4, Authority::Sim)).unwrap();
    }
    s.commit().unwrap();
    let mut w = World::with_store(
        s,
        SimConfig {
            event_queue: 6,
            ..SimConfig::default()
        },
        // Nothing is purged or expired: the bound pass only looks.
        RecConfig {
            removed_keep_ms: 1 << 50,
            dead_keep_ms: 1 << 50,
            ..RecConfig::default()
        },
    );
    // The harness's checks read the journal, which a store on disk does not
    // keep: run the frames unchecked.
    for _ in 0..50 {
        w.sim.frame();
        w.rec.step(&mut w.store, &mut w.sim);
        w.store.commit().unwrap();
        w.sim.flush();
    }
    let mut now_ms = 1_700_000_000_000u64;
    w.store.set_now(now_ms);
    let live_ids: Vec<EntityId> = w.store.groups().map(|g| g.id).collect();
    assert_eq!(live_ids.len(), 250);
    let mut have = 0usize;
    for r in [0usize, 2_000, 10_000, 50_000] {
        add_removed(&mut w.store, have, r, dead);
        have = r;
        // Let the reconciler see the new groups, and write them.
        for _ in 0..5 {
            w.sim.frame();
            w.rec.step(&mut w.store, &mut w.sim);
        }
        let states: BTreeMap<&str, usize> = w.store.groups().fold(BTreeMap::new(), |mut m, g| {
            *m.entry(g.state.name()).or_default() += 1;
            m
        });
        // 700 steps of 2 ms: the once-a-second bound pass runs in some.
        let mut pass_ms = Vec::new();
        for _ in 0..700 {
            now_ms += 2;
            w.store.set_now(now_ms);
            w.sim.frame();
            let t = Instant::now();
            w.rec.step_held(&mut w.store, &mut w.sim);
            pass_ms.push(t.elapsed().as_secs_f64() * 1e3);
            w.store.commit().unwrap();
            w.sim.flush();
        }
        let step_max_ms = pass_ms.iter().copied().fold(0.0_f64, f64::max);
        let step_median_ms = median(pass_ms);
        let mut plan_ms = Vec::new();
        let mut commit_idle_ms = Vec::new();
        let mut commit_dirty_ms = Vec::new();
        let mut walk_ms = Vec::new();
        for i in 0..50 {
            w.sim.frame();
            let t = Instant::now();
            w.rec.step_held(&mut w.store, &mut w.sim);
            plan_ms.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            w.store.commit().unwrap();
            commit_idle_ms.push(t.elapsed().as_secs_f64() * 1e3);
            w.sim.flush();
            // A step that changed one live group.
            w.store
                .set_label(live_ids[i % 250], &format!("platoon x{i}"))
                .unwrap();
            let t = Instant::now();
            w.store.commit().unwrap();
            commit_dirty_ms.push(t.elapsed().as_secs_f64() * 1e3);
            // A walk of every group, as the plan's first pass does.
            let t = Instant::now();
            let ids: Vec<EntityId> = w.store.groups().map(|g| g.id).collect();
            let n: usize = w
                .store
                .groups()
                .map(|g| g.members.len())
                .sum::<usize>()
                .wrapping_add(ids.len());
            std::hint::black_box(n);
            walk_ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let mut at = 100_000u64;
        let cp = timed_ms(5, |i| {
            w.store
                .set_label(live_ids[0], &format!("cp {r} {i}"))
                .unwrap();
            at += 5_000;
            w.store.checkpoint(at, Vec::new()).unwrap();
        });
        say(serde_json::json!({
            "measure": "step_cost", "kind": if dead { "dead" } else { "removed" },
            "removed_or_dead_groups": r, "live_groups": 250,
            "states": states,
            "step_2ms_clock_ms_median": step_median_ms,
            "step_2ms_clock_ms_max": step_max_ms,
            "plan_step_held_ms_p50": median(plan_ms),
            "commit_idle_ms_p50": median(commit_idle_ms),
            "commit_one_group_dirty_ms_p50": median(commit_dirty_ms),
            "walk_all_groups_ms_p50": median(walk_ms),
            "checkpoint_ms_p50": cp,
        }));
    }
}

#[test]
#[ignore = "a measurement"]
fn m3_step_cost_removed() {
    step_cost(false);
}

#[test]
#[ignore = "a measurement"]
fn m3_step_cost_dead() {
    step_cost(true);
}

// ---------------------------------------------------------------- 4. open

#[test]
#[ignore = "a measurement"]
fn m4_open_time() {
    for r in [0usize, 10_000, 50_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campaign.sqlite");
        let mut s = Store::create_on_disk(&path, CampaignId(1), record(), 10_000).unwrap();
        for g in 0..250 {
            s.create_group(group(g, 4, Authority::Sim)).unwrap();
        }
        add_removed(&mut s, 0, r, false);
        s.commit().unwrap();
        s.close().unwrap();
        drop(s);
        let size = file_bytes(&path);
        let mut times = Vec::new();
        let mut heap = 0isize;
        for _ in 0..3 {
            let before = live();
            let t = Instant::now();
            let opened = Store::open(&path, 10_000).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1e3);
            heap = live() - before;
            assert_eq!(opened.groups().count(), 250 + r);
        }
        say(serde_json::json!({
            "measure": "open_time", "removed_groups": r, "live_groups": 250,
            "open_ms_p50_of_3": median(times),
            "heap_bytes_after_open": heap,
            "file_bytes": size,
        }));
    }
}
