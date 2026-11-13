//! The fake sim passes the conformance suite that `RealSim` runs against
//! DCS.

use nanoprobe_sidecar::conformance;
use nanoprobe_sim_fake::{Config, FakeSim};

#[test]
fn the_fake_sim_conforms() {
    let mut sim = FakeSim::new(Config::default());
    sim.attach(1);
    let checks = conformance::run(&mut sim);
    let failed: Vec<String> = checks
        .iter()
        .filter_map(|c| c.result.as_ref().err().map(|e| format!("{}: {e}", c.name)))
        .collect();
    assert_eq!(checks.len(), 9);
    assert!(failed.is_empty(), "{failed:#?}");
}

/// The suite against a mission that was already running when the sidecar
/// attached: it learns the mission from the LIFECYCLE the agent writes
/// after the HELLO.
#[test]
fn the_fake_sim_conforms_when_attached_to_a_running_mission() {
    let mut sim = FakeSim::new(Config::default());
    for _ in 0..20 {
        sim.frame();
    }
    assert!(sim.running());
    sim.attach(1);
    let checks = conformance::run(&mut sim);
    let failed: Vec<String> = checks
        .iter()
        .filter_map(|c| c.result.as_ref().err().map(|e| format!("{}: {e}", c.name)))
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
}
