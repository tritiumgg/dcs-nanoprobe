//! The replay driver on recordings. A fake-sim run with interleaved faults
//! is recorded as the reconciler read it. Replayed into a fresh store, it
//! yields the death facts the live run journalled, wherever the sidecar
//! restarts, with persistence on; with persistence off it yields every death
//! read after the last restart.
//!
//! Recordings from a running DCS are read from outside the repository; see
//! `recordings_from_dcs_replay`.

use std::collections::BTreeSet;

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::reconciler::Config;
use nanoprobe_sidecar::replay::{Batch, Persistence, Recording, fold};
use nanoprobe_sidecar::sim::wire::{Input, gone_reason};
use nanoprobe_sidecar::store::{Entry, LostPolicy, Store};
use nanoprobe_sim_fake::harness::{CAMPAIGN, Op, Schedule, World, new_store, new_store_for};
use nanoprobe_transport::layout::kind;
use proptest::prelude::*;

fn deaths(store: &Store) -> BTreeSet<EntityId> {
    store
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Death { unit } => Some(*unit),
            _ => None,
        })
        .collect()
}

fn live_run(seed: u64) -> (World, BTreeSet<EntityId>) {
    let mut w = World::new(5, 2, LostPolicy::Report);
    let mut s = Schedule::new(seed);
    for _ in 0..250 {
        w.apply(&s.op()).unwrap();
    }
    w.settle().unwrap();
    let d = deaths(&w.store);
    (w, d)
}

/// The index of the first `FRAME` at or after `i`, where [`fold`] restarts.
fn frame_at_or_after(rec: &Recording, i: usize) -> Option<usize> {
    rec.records()
        .enumerate()
        .skip(i)
        .find(|(_, r)| u64::from(r.kind) == kind::FRAME)
        .map(|(n, _)| n)
}

/// The record index at each [`Batch::SidecarRestart`] of a recording.
fn restart_markers(rec: &Recording) -> Vec<usize> {
    let mut i = 0;
    let mut out = Vec::new();
    for b in &rec.batches {
        match b {
            Batch::Records(rs) => i += rs.len(),
            Batch::SidecarRestart => out.push(i),
        }
    }
    out
}

/// The units whose `OBJECT_GONE` `DEAD`, stamped with this campaign, comes
/// at or after record `from`.
fn dead_records_from(rec: &Recording, from: usize) -> BTreeSet<EntityId> {
    let mut campaign = 0;
    let mut out = BTreeSet::new();
    for (i, r) in rec.records().enumerate() {
        match Input::decode(r.kind, &r.payload) {
            Ok(Input::Frame(f)) => campaign = f.campaign,
            Ok(Input::ObjectGone(g))
                if i >= from
                    && g.reason == gone_reason::DEAD
                    && g.entity != 0
                    && campaign == CAMPAIGN =>
            {
                out.insert(EntityId(g.entity));
            }
            _ => {}
        }
    }
    out
}

/// The number of cases: `PROPTEST_CASES` when set, else the default here.
fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(48), ..ProptestConfig::default() })]

    #[test]
    fn a_replay_keeps_the_death_facts_wherever_the_sidecar_restarts(
        seed in any::<u64>(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let (w, want) = live_run(seed);
        let total = w.recording.records().count();
        let restarts: Vec<usize> = cuts.iter().map(|c| c.index(total.max(1))).collect();
        let mut store = new_store(5, 2);
        let counters = fold(Config::default(), &mut store, &w.recording, &restarts, Persistence::On);
        prop_assert_eq!(counters.undecodable, 0);
        prop_assert_eq!(deaths(&store), want);
    }

    /// With persistence off a restart loses the store: the author writes the
    /// specs back, and every death read after the last restart is a fact
    /// again, from the binding `OBJECT_GONE` carries.
    #[test]
    fn with_persistence_off_the_deaths_after_the_last_restart_are_facts(
        seed in any::<u64>(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 1..4),
    ) {
        let (w, live) = live_run(seed);
        let total = w.recording.records().count();
        let restarts: Vec<usize> = cuts.iter().map(|c| c.index(total.max(1))).collect();
        let last = last_restart(&w.recording, &restarts);
        let mut store = new_store(5, 2);
        let fresh = || new_store(5, 2);
        fold(Config::default(), &mut store, &w.recording, &restarts, Persistence::Off(&fresh));
        let want: BTreeSet<EntityId> = dead_records_from(&w.recording, last)
            .intersection(&live)
            .copied()
            .collect();
        prop_assert_eq!(deaths(&store), want);
    }
}

/// Where the last restart of a fold falls: the first `FRAME` at or after a
/// restart index, the tail (the record count) for an index with no `FRAME`
/// after it, or a restart marker of the recording.
fn last_restart(rec: &Recording, restarts: &[usize]) -> usize {
    let total = rec.records().count();
    restarts
        .iter()
        .map(|i| frame_at_or_after(rec, *i).unwrap_or(total))
        .chain(restart_markers(rec))
        .max()
        .unwrap_or(0)
}

/// A restart after the last `FRAME` restarts at the tail: with persistence
/// off nothing is left. Seeds whose recording ends after its last `FRAME`
/// exercise that branch of `fold` on every run.
#[test]
fn with_persistence_off_a_restart_at_the_tail_leaves_no_fact() {
    let mut checked = 0;
    for seed in 0..64 {
        let (w, _) = live_run(seed);
        let total = w.recording.records().count();
        if frame_at_or_after(&w.recording, total - 1).is_some() {
            continue;
        }
        let mut store = new_store(5, 2);
        let fresh = || new_store(5, 2);
        let cut = [total - 1];
        fold(
            Config::default(),
            &mut store,
            &w.recording,
            &cut,
            Persistence::Off(&fresh),
        );
        assert_eq!(last_restart(&w.recording, &cut), total);
        assert!(deaths(&store).is_empty(), "seed {seed}");
        checked += 1;
    }
    assert!(checked > 0, "no recording ends after its last FRAME");
}

/// The required case of the coordinator's ruling (b): a death the agent
/// queued while detached, read by a sidecar that restarted without its store
/// and has no binding yet.
#[test]
fn a_death_queued_while_detached_is_a_fact_after_a_restart_without_the_store() {
    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let (object, unit) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e != 0)
        .expect("a unit in DCS");
    w.apply(&Op::Detach).unwrap();
    w.sim.kill(object);
    let attach_at = w.recording.records().count();
    w.apply(&Op::Reattach).unwrap();
    w.settle().unwrap();
    assert!(deaths(&w.store).contains(&EntityId(unit)));

    let mut store = new_store(1, 0);
    let fresh = || new_store(1, 0);
    fold(
        Config::default(),
        &mut store,
        &w.recording,
        &[attach_at],
        Persistence::Off(&fresh),
    );
    assert!(
        deaths(&store).contains(&EntityId(unit)),
        "the queued death is a fact"
    );
}

#[test]
fn a_recording_survives_the_file_format() {
    let (w, want) = live_run(7);
    let mut buf = Vec::new();
    w.recording.write_jsonl(&mut buf).unwrap();
    let back = Recording::read_jsonl(buf.as_slice()).unwrap();
    assert!(back.records().eq(w.recording.records()));
    let mut store = new_store(5, 2);
    fold(Config::default(), &mut store, &back, &[], Persistence::On);
    assert_eq!(deaths(&store), want);
}

/// A small deterministic generator for restart points: 1 to 7 of them.
fn points(seed: u64, total: usize) -> Vec<usize> {
    let n = 1 + (seed % 7) as usize;
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % total.max(1) as u64) as usize
        })
        .collect()
}

/// Recordings from a running DCS, made by `link-probe --record` in steps S9
/// and S10. They hold server data, so they stay outside the repository: set
/// `NANOPROBE_RECORDINGS` to a folder of `*.jsonl` files. Their units are not
/// the platform's, so the store holds no group. Wherever the sidecar
/// restarts, with persistence on and off, the replay must decode every record, read every `EVENT`, meet
/// no record kind it does not read but `LOG`, end on the generation and theater of the
/// last `LIFECYCLE`, and journal no death fact.
#[test]
fn recordings_from_dcs_replay() {
    let Some(dir) = std::env::var_os("NANOPROBE_RECORDINGS") else {
        return;
    };
    let mut seen = 0;
    for e in std::fs::read_dir(&dir).expect("NANOPROBE_RECORDINGS is a folder") {
        let path = e.unwrap().path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let file = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
        let rec = Recording::read_jsonl(file).unwrap();
        let total = rec.records().count();
        let events = rec
            .records()
            .filter(|r| u64::from(r.kind) == kind::EVENT)
            .count() as u64;
        let logs = rec
            .records()
            .filter(|r| u64::from(r.kind) == kind::LOG)
            .count() as u64;
        let last = rec
            .records()
            .filter_map(|r| match Input::decode(r.kind, &r.payload) {
                Ok(Input::Lifecycle(l)) => Some((r.agent_boot, l.mission_gen, l.theater)),
                _ => None,
            })
            .last();
        let fresh = || new_store(0, 0);
        let runs = (1..=32u64)
            .map(|s| (s, Persistence::On))
            .chain((1..=32u64).map(|s| (s, Persistence::Off(&fresh))));
        for (seed, persistence) in runs {
            let restarts = points(seed, total);
            let mut store = new_store(0, 0);
            let c = fold(Config::default(), &mut store, &rec, &restarts, persistence);
            let at = path.display();
            assert_eq!(c.undecodable, 0, "{at}");
            assert_eq!(
                c.other, logs,
                "{at}: a record kind the reconciler does not read"
            );
            assert_eq!(c.events, events, "{at}");
            assert!(deaths(&store).is_empty(), "{at}");
            // Without the store, the sidecar knows the mission only from
            // what it read after its last restart: the stamps of a FRAME,
            // and the theater from a LIFECYCLE.
            let from = match persistence {
                Persistence::On => 0,
                Persistence::Off(_) => last_restart(&rec, &restarts),
            };
            let lifecycle_after = rec
                .records()
                .skip(from)
                .any(|r| u64::from(r.kind) == kind::LIFECYCLE);
            if let Some((boot, mission_gen, theater)) = &last {
                if from == total {
                    assert!(store.loaded().is_none(), "{at}");
                    continue;
                }
                let l = store.loaded().expect("a loaded mission");
                assert_eq!((l.agent_boot, l.mission_gen), (*boot, *mission_gen), "{at}");
                if !theater.is_empty() && lifecycle_after {
                    assert_eq!(&l.theater, theater, "{at}");
                }
            }
        }
        println!("{}: {total} records, {events} EVENT", path.display());
        seen += 1;
    }
    assert!(
        seen > 0,
        "no *.jsonl recording in {}",
        std::path::Path::new(&dir).display()
    );
}

/// Another campaign whose `EntityId`s are this campaign's.
const B: u64 = CAMPAIGN + 1;

/// A world whose campaign-A unit 2 died while the agent was detached, then
/// read by a sidecar of campaign B, without a store (persistence off). Units
/// 2 to 4 of both campaigns form group 1.
fn a_death_queued_for_another_campaign() -> World {
    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == 2)
        .expect("unit 2 of A");
    w.apply(&Op::Detach).unwrap();
    w.sim.kill(object);
    w.sidecar_of(new_store_for(B, 1, 0));
    for _ in 0..5 {
        w.tick().unwrap();
    }
    w
}

/// The coordinator's campaign ruling: a death the agent queued under
/// campaign A, read by a sidecar of campaign B where entity 2 exists, is no
/// death fact of B, and is counted as dropped.
#[test]
fn a_death_queued_under_another_campaign_is_dropped() {
    let w = a_death_queued_for_another_campaign();
    assert_eq!(w.store.campaign().0, B);
    assert!(!w.store.unit(EntityId(2)).unwrap().dead);
    assert!(deaths(&w.store).is_empty());
    assert!(w.rec.counters().other_campaign > 0);
}

/// The same, replayed by a sidecar of B with no store and no loaded mission
/// that restarts in the middle: before the agent switched to B, at its
/// attach, and after.
#[test]
fn a_replay_by_another_campaign_drops_the_death() {
    let w = a_death_queued_for_another_campaign();
    let total = w.recording.records().count();
    let fresh = || new_store_for(B, 1, 0);
    for restarts in [
        vec![],
        vec![0],
        vec![total / 2],
        vec![total - 1],
        vec![1, total / 3, total / 2],
    ] {
        let mut store = fresh();
        let c = fold(
            Config::default(),
            &mut store,
            &w.recording,
            &restarts,
            Persistence::Off(&fresh),
        );
        assert!(deaths(&store).is_empty(), "{restarts:?}");
        assert!(c.other_campaign > 0, "{restarts:?}");
    }
}

/// Between a switch to B and the next load, campaign A's objects stay in
/// DCS with no binding: B binds none of them, and A's unit 2 dying is no
/// death of B's unit 2. B, started without a store, learns the theater from
/// the load and only then spawns.
#[test]
fn between_a_switch_and_the_next_load_the_old_campaign_is_ignored() {
    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().unwrap();
    let (object, _) = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == 2)
        .expect("unit 2 of A");
    w.sidecar_of(new_store_for(B, 1, 0));
    for _ in 0..120 {
        w.tick().unwrap();
    }
    assert!(
        w.sim
            .objects()
            .iter()
            .any(|(o, c, _)| *o == object && *c == CAMPAIGN),
        "A's unit is still in DCS"
    );
    assert!(!w.rec.binding().contains_key(&object));
    w.sim.kill(object);
    for _ in 0..60 {
        w.tick().unwrap();
    }
    assert!(!w.store.unit(EntityId(2)).unwrap().dead);
    assert!(deaths(&w.store).is_empty());
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    assert_eq!(
        w.store.group(EntityId(1)).unwrap().state,
        nanoprobe_sidecar::lifecycle::State::Live
    );
}
