//! The deterministic simulation: seeded schedules of faults against the
//! reconciler. Set `NANOPROBE_SEED` (hex) to run one seed, and
//! `NANOPROBE_SEEDS` to run more than the default 64.

use nanoprobe_sim_fake::harness::run_seed;

#[test]
fn seeded_schedules_hold_every_invariant() {
    if let Ok(s) = std::env::var("NANOPROBE_SEED") {
        let seed = u64::from_str_radix(s.trim_start_matches("0x"), 16).expect("a hex seed");
        run_seed(seed, 300, 6, 2).unwrap();
        return;
    }
    let seeds: u64 = std::env::var("NANOPROBE_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);
    let mut failures = Vec::new();
    for seed in 0..seeds {
        if let Err(e) = run_seed(seed, 300, 6, 2) {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The seeds reach `live` -> `dead`, and never `virtual` -> `dead`.
/// `materializing` -> `dead` and `dematerializing` -> `dead` need a death
/// in a short window, too rare for random schedules to be sure of; scripted
/// tests cover them (crates/sidecar/tests/reconciler.rs), as they do
/// `lost` -> `dead`, which the protocol's ordering rules out.
#[test]
fn the_seeds_reach_the_new_arrows_to_dead() {
    use std::collections::BTreeMap;

    use nanoprobe_sidecar::lifecycle::State;
    use nanoprobe_sidecar::store::{Entry, LostPolicy};
    use nanoprobe_sim_fake::harness::{Schedule, World};

    let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
    for seed in 0..200u64 {
        let mut w = World::new(6, 2, LostPolicy::Report);
        let mut s = Schedule::new(seed);
        for _ in 0..300 {
            w.apply(&s.op()).unwrap();
        }
        for e in w.store.entries() {
            if let Entry::Transition {
                from,
                to: State::Dead,
                ..
            } = e
            {
                *seen.entry(from.name()).or_default() += 1;
            }
        }
    }
    // spec/world.md: `virtual` -> `dead` cannot happen in order.
    assert_eq!(seen.get("virtual"), None, "{seen:?}");
    assert!(
        seen.get("live").copied().unwrap_or(0) > 0,
        "no live -> dead in 200 seeds: {seen:?}"
    );
}
