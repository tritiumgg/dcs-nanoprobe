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
