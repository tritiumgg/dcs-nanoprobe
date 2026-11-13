//! The replay driver on recordings. A fake-sim run with interleaved faults
//! is recorded as the reconciler read it; replaying it into a fresh store,
//! with the sidecar restarting before any records, yields the same death
//! facts the live run journalled and decodes every record.
//!
//! Recordings from a running DCS are read from outside the repository; see
//! `recordings_from_dcs_replay`.

use std::collections::BTreeSet;

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::reconciler::Config;
use nanoprobe_sidecar::replay::{Recording, fold};
use nanoprobe_sidecar::store::{Entry, LostPolicy, Store};
use nanoprobe_sim_fake::harness::{Schedule, World, new_store};
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn a_replay_keeps_the_death_facts_wherever_the_sidecar_restarts(
        seed in any::<u64>(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let (w, want) = live_run(seed);
        let total = w.recording.records().count();
        let restarts: Vec<usize> = cuts.iter().map(|c| c.index(total.max(1))).collect();
        let mut store = new_store(5, 2);
        let counters = fold(Config::default(), &mut store, &w.recording, &restarts);
        prop_assert_eq!(counters.undecodable, 0);
        prop_assert_eq!(deaths(&store), want);
    }
}

#[test]
fn a_recording_survives_the_file_format() {
    let (w, want) = live_run(7);
    let mut buf = Vec::new();
    w.recording.write_jsonl(&mut buf).unwrap();
    let back = Recording::read_jsonl(buf.as_slice()).unwrap();
    assert!(back.records().eq(w.recording.records()));
    let mut store = new_store(5, 2);
    fold(Config::default(), &mut store, &back, &[]);
    assert_eq!(deaths(&store), want);
}

/// Recordings from a running DCS, made by `link-probe --record` in steps S9
/// and S10. They hold server data, so they stay outside the repository: set
/// `NANOPROBE_RECORDINGS` to a folder of `*.jsonl` files. Their units are not
/// the platform's, so the store is empty: the replay must decode every
/// record, wherever the sidecar restarts, and journal no death fact.
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
        for step in [0, 1, 7, 64] {
            let restarts: Vec<usize> = if step == 0 {
                vec![]
            } else {
                (0..total).step_by(step).collect()
            };
            let mut store = new_store(0, 0);
            let c = fold(Config::default(), &mut store, &rec, &restarts);
            assert_eq!(c.undecodable, 0, "{}", path.display());
            assert!(deaths(&store).is_empty(), "{}", path.display());
        }
        println!("{}: {total} records", path.display());
        seen += 1;
    }
    assert!(
        seen > 0,
        "no *.jsonl recording in {}",
        std::path::Path::new(&dir).display()
    );
}
