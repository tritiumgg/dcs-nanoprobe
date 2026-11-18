//! A long campaign under churn levels off (spec/world.md, "Removal" and
//! "Expiry"; spec/persistence.md, "Size").
//!
//! The fake sim runs a campaign on a simulated clock for many keep windows.
//! Every few seconds a group is removed and a new one put, a group is
//! killed and replaced, and a group is moved between the campaign and DCS.
//! After the warm-up (the longest keep window and an enumeration), the
//! entities the store holds, the plan's work set, the rows the checkpoint
//! compares, the journal, the store file and the heap stop rising: their
//! largest value in the last third of the run is not above their largest in
//! the middle third, within a tolerance. A restart in the middle reopens the
//! store, and no `EntityId` given before it is given again.
//!
//! The heap is measured with a counting allocator, so this file holds one
//! test, which runs its two cases one after the other.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicIsize, Ordering};

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::reconciler::{Config as RecConfig, Reconciler};
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sidecar::store::{
    Authority, CampaignRecord, CampaignState, Kind, LostPolicy, NewGroup, NewUnit, Point, Store,
    UnitState,
};
use nanoprobe_sim_fake::Config;
use nanoprobe_sim_fake::harness::{THEATER, World, new_store};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every call forwards to the system allocator with the caller's
// layout and pointer, so its contract is the caller's; the counter only
// adds and subtracts sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from `alloc` with `layout`.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// One frame of the fake sim on the simulated clock.
const FRAME_MS: u64 = 100;
const REMOVED_KEEP_MS: u64 = 60_000;
const DEAD_KEEP_MS: u64 = 120_000;
const JOURNAL_KEEP_MS: u64 = 180_000;
/// The run: 18 simulated minutes, 10,800 frames, or the minutes in
/// `NANOPROBE_CHURN_MIN` for a longer run by hand.
fn run_ms() -> u64 {
    std::env::var("NANOPROBE_CHURN_MIN")
        .ok()
        .and_then(|m| m.parse::<u64>().ok())
        .unwrap_or(18)
        * 60_000
}
const SAMPLE_MS: u64 = 30_000;
/// The live load: groups of three under DCS authority, and some statics.
const GROUPS: usize = 30;
const STATICS: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    t_ms: u64,
    groups: usize,
    units: usize,
    awaiting_purge: usize,
    dead: usize,
    work: usize,
    checkpoint_marks: usize,
    journal_rows: u64,
    file_bytes: u64,
    /// The store's own heap: what a copy of it allocates (its rows, sets
    /// and journal held, without the file's shadow).
    store_heap: isize,
    /// The process's heap, the fake sim's included.
    heap: isize,
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
                        x: (n % 50) as f64 * 1_000.0,
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

fn record() -> CampaignRecord {
    CampaignRecord {
        name: "churn".into(),
        state: CampaignState::Active,
        theaters: vec![THEATER.into()],
        theater: THEATER.into(),
        clock_ms: Some(1_700_000_000_000),
        created_ms: 1,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

/// Groups (not statics) that are not removed, in a state `want` accepts.
fn groups_where(w: &World, want: impl Fn(State) -> bool) -> Vec<EntityId> {
    w.store
        .groups()
        .filter(|g| g.kind == Kind::Group && !g.removed && want(g.state))
        .map(|g| g.id)
        .collect()
}

fn file_bytes(path: Option<&std::path::Path>) -> u64 {
    let Some(p) = path else { return 0 };
    let mut wal = p.as_os_str().to_owned();
    wal.push("-wal");
    [p.to_path_buf(), std::path::PathBuf::from(wal)]
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum()
}

fn run(on_disk: bool) -> Vec<Sample> {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("store.sqlite");
    let mut w = World::with_store(
        new_store(GROUPS, STATICS),
        Config {
            event_queue: 6,
            ..Config::default()
        },
        RecConfig {
            materialize_per_frame: 4,
            on_lost: LostPolicy::Report,
            removed_keep_ms: REMOVED_KEEP_MS,
            dead_keep_ms: DEAD_KEEP_MS,
            ..RecConfig::default()
        },
    );
    // The clock starts at the wall clock, as the driver's does, and after
    // the entries the seed groups were journalled with.
    let start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
        + 1_000;
    w.clock_ms = start_ms;
    w.store.set_now(start_ms);
    if on_disk {
        w.store.save_as(&path, record()).expect("save");
    }
    w.lean();
    w.frame_ms = FRAME_MS;
    let file = on_disk.then_some(path.as_path());

    let mut puts = 0u64;
    let mut samples = Vec::new();
    let mut given_before_restart = 0u64;
    let mut created_after = Vec::new();
    let mut restarted = false;
    let frames = run_ms() / FRAME_MS;
    let mut marks_peak = 0;
    for f in 1..=frames {
        w.tick().expect("tick");
        let now = w.clock_ms;
        let t = now - start_ms;
        // Remove a group and put a new one, every 2 s.
        if f.is_multiple_of(20) {
            let live = groups_where(&w, |s| s != State::Dead);
            if let Some(id) = live.get((f / 20) as usize % live.len().max(1)) {
                w.store.remove_entity(*id).expect("remove");
            }
            puts += 1;
            let id = w.store.create_group(spec(puts)).expect("put");
            if restarted {
                created_after.push(id.0);
            }
        }
        // Kill a live group's units, every 4 s; a dead group is replaced.
        if f.is_multiple_of(40) {
            let live = groups_where(&w, |s| s == State::Live);
            if let Some(id) = live.get((f / 40) as usize % live.len().max(1)) {
                let members = w.store.group(*id).expect("group").members.clone();
                for (o, _, e) in w.sim.objects() {
                    if members.contains(&EntityId(e)) {
                        w.sim.kill(o);
                    }
                }
                puts += 1;
                let id = w.store.create_group(spec(puts)).expect("replace");
                if restarted {
                    created_after.push(id.0);
                }
            }
        }
        // Move a group to the campaign and another back to DCS, every 6 s.
        if f.is_multiple_of(60) {
            let all = groups_where(&w, |s| matches!(s, State::Live | State::Virtual));
            if !all.is_empty() {
                let id = all[(f / 60) as usize % all.len()];
                let to = match w.store.group(id).expect("group").authority {
                    Authority::Sim => Authority::Campaign,
                    Authority::Campaign => Authority::Sim,
                };
                w.store.set_authority(id, to).expect("authority");
            }
        }
        // The driver's timers: a checkpoint every 5 s of a store on disk,
        // the journal's retention every minute.
        if t.is_multiple_of(5_000) {
            marks_peak = marks_peak.max(w.store.checkpoint_marks());
            if on_disk {
                // This checkpoint compares the marked groups only: past the
                // first one after the store was made or reopened, which
                // compares every row by design.
                if t > 5_000 && !(restarted && t <= run_ms() / 2 + 5_000) {
                    assert!(!w.store.checkpoint_compares_all(), "t={t}");
                }
                w.store.checkpoint(now, Vec::new()).expect("checkpoint");
            }
        }
        if t.is_multiple_of(60_000) {
            if on_disk {
                while let nanoprobe_sidecar::store::JournalPrune::Deleted { more: true, .. } = w
                    .store
                    .prune_journal(JOURNAL_KEEP_MS, now, 1_000, 0)
                    .expect("prune")
                {
                    w.store.commit().expect("commit");
                }
            } else {
                w.store.prune_memory_journal(now, JOURNAL_KEEP_MS, 0);
            }
        }
        // A restart in the middle: the store reopened from its file, or,
        // in memory, a new reconciler over the same store.
        if !restarted && t >= run_ms() / 2 {
            restarted = true;
            // An ID given with no row (as an adopted unit's is), so the
            // counter is above every row: a reopen that rebuilt it from the
            // rows would give this ID again.
            w.store.adopt_id().expect("id");
            w.store.commit().expect("commit");
            given_before_restart = w.store.next_entity();
            if on_disk {
                w.store.close().expect("close");
                let s = Store::open(&path, 10_000).expect("reopen");
                w.sidecar_of(s);
                w.lean();
            } else {
                let cfg = w.rec_config();
                w.set_rec_config(cfg);
            }
            assert_eq!(w.store.next_entity(), given_before_restart);
        }
        if t.is_multiple_of(SAMPLE_MS) {
            let sizes = w.store.sizes();
            samples.push(Sample {
                t_ms: t,
                groups: w.store.groups().count(),
                units: sizes.units as usize,
                awaiting_purge: sizes.awaiting_purge as usize,
                dead: sizes.dead as usize,
                work: w.rec.work_len(),
                checkpoint_marks: std::mem::take(&mut marks_peak),
                journal_rows: w.store.journal_extent().0,
                file_bytes: file_bytes(file),
                store_heap: {
                    let h = LIVE.load(Ordering::Relaxed);
                    let c = w.store.clone();
                    let d = LIVE.load(Ordering::Relaxed) - h;
                    drop(c);
                    d
                },
                heap: LIVE.load(Ordering::Relaxed),
            });
        }
    }
    // What the reconciler holds beyond a fresh one over the same store: the
    // run's history must not be in it.
    let h0 = LIVE.load(Ordering::Relaxed);
    let fresh = Reconciler::new(w.rec_config(), &w.store);
    drop(std::mem::replace(&mut w.rec, fresh));
    let reconciler = h0 - LIVE.load(Ordering::Relaxed);
    eprintln!("reconciler beyond a fresh one: {reconciler} bytes");
    // About 16 KiB measured. A per-entity map the purge left behind holds
    // some 800 entries by the end, 16 KiB or more.
    assert!(
        reconciler < 24_576,
        "the reconciler holds {reconciler} bytes of history"
    );
    // No ID given before the restart was given again after it: the first
    // put after it took the counter, and none went below.
    assert_eq!(created_after.first().copied(), Some(given_before_restart));
    assert!(created_after.iter().all(|id| *id >= given_before_restart));
    let (purged, expired) = w.store.purged_expired();
    assert!(purged > 100, "purged {purged}");
    assert!(expired > 10, "expired {expired}");
    assert!(puts > 500, "puts {puts}");
    samples
}

/// The largest value of `f` over the samples in `[from, to)` of the run.
fn peak<T: Ord + Copy + Default>(s: &[Sample], from: u64, to: u64, f: impl Fn(&Sample) -> T) -> T {
    s.iter()
        .filter(|x| x.t_ms >= from && x.t_ms < to)
        .map(f)
        .max()
        .unwrap_or_default()
}

fn check_flat(on_disk: bool, s: &[Sample]) {
    for x in s {
        eprintln!(
            "{} t={:>4}s groups={:>4} units={:>4} awaiting={:>3} dead={:>3} work={:>3} marks={:>3} journal={:>6} file={:>8} store_heap={:>8} heap={:>9}",
            if on_disk { "disk" } else { "mem " },
            x.t_ms / 1000,
            x.groups,
            x.units,
            x.awaiting_purge,
            x.dead,
            x.work,
            x.checkpoint_marks,
            x.journal_rows,
            x.file_bytes,
            x.store_heap,
            x.heap
        );
    }
    let (a, b, c) = (run_ms() / 3, 2 * run_ms() / 3, run_ms() + 1);
    // Counts: the last third's peak within 15 % (and 5 rows) of the middle
    // third's. The run's churn is steady, so a store that kept what it
    // should drop would gain about a third again.
    let within = |name: &str, mid: u64, last: u64| {
        assert!(
            last <= mid + mid * 15 / 100 + 5,
            "{name} keeps rising: middle third {mid}, last third {last}"
        );
    };
    within(
        "groups",
        peak(s, a, b, |x| x.groups) as u64,
        peak(s, b, c, |x| x.groups) as u64,
    );
    within(
        "units",
        peak(s, a, b, |x| x.units) as u64,
        peak(s, b, c, |x| x.units) as u64,
    );
    within(
        "work set",
        peak(s, a, b, |x| x.work) as u64,
        peak(s, b, c, |x| x.work) as u64,
    );
    within(
        "checkpoint compare",
        peak(s, a, b, |x| x.checkpoint_marks) as u64,
        peak(s, b, c, |x| x.checkpoint_marks) as u64,
    );
    within(
        "journal",
        peak(s, a, b, |x| x.journal_rows),
        peak(s, b, c, |x| x.journal_rows),
    );
    if on_disk {
        // The file levels off: freed pages are reused. 25 % for the WAL,
        // which a checkpoint of SQLite's own resets at its own times.
        let (mid, last) = (
            peak(s, a, b, |x| x.file_bytes),
            peak(s, b, c, |x| x.file_bytes),
        );
        assert!(
            last <= mid + mid / 4 + 65_536,
            "the store file keeps growing: middle third {mid}, last third {last}"
        );
    }
    // The store's heap: 15 % and 64 KiB for the maps' and the change
    // ring's capacity, which a reopened store builds up again.
    let (mid, last) = (
        peak(s, a, b, |x| x.store_heap),
        peak(s, b, c, |x| x.store_heap),
    );
    assert!(
        last <= mid + mid * 15 / 100 + 65_536,
        "the store's heap keeps growing: middle third {mid}, last third {last}"
    );
    // The process's heap: 20 % and 256 KiB more. The fake sim, standing in
    // for the agent, keeps per-generation state of its own, which a
    // mission load clears.
    let (mid, last) = (peak(s, a, b, |x| x.heap), peak(s, b, c, |x| x.heap));
    assert!(
        last <= mid + mid / 5 + 262_144,
        "the heap keeps growing: middle third {mid}, last third {last}"
    );
    // Without the purge the store would hold every group ever put.
    let last = s.last().expect("samples");
    assert!(last.awaiting_purge < 100, "{last:?}");
    // The plan and the checkpoint visit fewer groups than the store holds:
    // the removed, the dead and the idle virtual ones are left out.
    for x in s.iter().filter(|x| x.t_ms >= a) {
        assert!(
            x.work * 2 < x.groups,
            "the work set is most of the store: {x:?}"
        );
        if on_disk {
            assert!(
                x.checkpoint_marks * 2 < x.groups,
                "the checkpoint compares most of the store: {x:?}"
            );
        }
    }
}

#[test]
fn a_long_churn_levels_off_on_disk_and_in_memory() {
    for on_disk in [true, false] {
        let s = run(on_disk);
        check_flat(on_disk, &s);
    }
}
