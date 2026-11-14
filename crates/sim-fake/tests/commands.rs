//! The command path's exit test on the fake sim (step S14): seeded runs of
//! mixed set-state commands (`EXTENSION_SET`, from changes to the store's
//! extensions) and effects (`EXTENSION_INVOKE`) through the reconciler, with
//! sidecar restarts, crashes in a write, detaches, lost results and DCS
//! restarts. Every extension revision DCS applies is applied once per agent
//! boot, each extension's last revision is applied, and no effect is applied
//! twice. Set `NANOPROBE_SEEDS` to run more seeds.

use std::collections::BTreeMap;

use nanoprobe_sidecar::sim::wire::opcode;
use nanoprobe_sidecar::store::{Entry, LostPolicy};
use nanoprobe_sim_fake::harness::{Op, World};
use nanoprobe_transport::codec::Lv;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

const NAMES: [&str; 4] = ["a", "b", "c", "d"];

/// Run one seed until at least `commands` extension commands were written.
fn run(seed: u64, commands: usize) -> Result<(), String> {
    let mut r = Rng(seed);
    let mut w = World::new(1, 0, LostPolicy::Report);
    w.settle().map_err(|v| format!("settle: {v:?}"))?;
    let mut version: BTreeMap<&str, u64> = BTreeMap::new();
    let written = |w: &World| {
        w.store
            .entries()
            .iter()
            .filter(|e| {
                matches!(e, Entry::Command(c)
                    if c.opcode == opcode::EXTENSION_SET || c.opcode == opcode::EXTENSION_INVOKE)
            })
            .count()
    };
    let mut steps = 0;
    let mut tokens = 0u64;
    let mut ops: BTreeMap<&str, u32> = BTreeMap::new();
    while written(&w) < commands {
        steps += 1;
        if steps > commands * 50 {
            return Err(format!("only {} commands after {steps} steps", written(&w)));
        }
        let x = r.next();
        let name = NAMES[(x >> 8) as usize % NAMES.len()];
        let op = match x % 20 {
            0..=5 => {
                let v = version.entry(name).or_default();
                *v += 1;
                w.store
                    .set_extension(name, &format!("-- {name} {v}\nreturn {{}}"));
                Op::Frames(1)
            }
            6..=11 => {
                // A unique token is the effect's identity in the ledger.
                tokens += 1;
                let args = Lv::Table(vec![(Lv::Number(1.0), Lv::Number(tokens as f64))]);
                w.rec.invoke(&mut w.store, &mut w.sim, name, args);
                Op::Frames(1)
            }
            12 => Op::SidecarRestart,
            13 => Op::SidecarCrashInWrite((x >> 40) as u8 % 4),
            14 => Op::Detach,
            15 => Op::Reattach,
            16 => Op::LoseResult,
            17 if (x >> 20).is_multiple_of(8) => Op::Crash,
            17 => Op::Frames(2),
            18 if !w.sim.alive() => Op::RestartDcs,
            _ => Op::Frames(1 + (x >> 50) as u8 % 3),
        };
        let kind = match op {
            Op::SidecarRestart => Some("restart"),
            Op::SidecarCrashInWrite(_) => Some("crash_in_write"),
            Op::LoseResult => Some("lose_result"),
            _ => None,
        };
        if let Some(k) = kind {
            *ops.entry(k).or_default() += 1;
        }
        w.apply(&op)
            .map_err(|v| format!("step {steps}, {op:?}: {v:?}"))?;
    }
    w.settle().map_err(|v| format!("final settle: {v:?}"))?;
    // Let every extension reach its last revision.
    for _ in 0..50 {
        w.apply(&Op::Frames(2))
            .map_err(|v| format!("tail: {v:?}"))?;
    }
    let l = w.sim.ledger();
    if let Some((k, n)) = l.extension_runs.iter().find(|(_, n)| **n != 1) {
        return Err(format!("extension {k:?} applied {n} times"));
    }
    if let Some((k, n)) = l.invokes.iter().find(|(_, n)| **n != 1) {
        return Err(format!("effect {k:?} applied {n} times"));
    }
    let boot = w.sim.agent_boot();
    let campaign = w.store.campaign().0;
    for e in w.store.extensions() {
        let key = (boot, campaign, e.name.clone(), e.spec_rev);
        if l.extension_runs.get(&key) != Some(&1) {
            return Err(format!(
                "extension {} at its last revision {} is not applied once in boot {boot}",
                e.name, e.spec_rev
            ));
        }
    }
    // Lower bounds on what the seed exercised, so the checks above are not
    // met on an empty set. The floors are far below what the four default
    // seeds reach (about 1,000 commands, a third of them invokes); they
    // fail a seed whose invokes or faults never happened.
    let count = |k| ops.get(k).copied().unwrap_or(0);
    if l.invokes.len() < 50 {
        return Err(format!("only {} effects ran", l.invokes.len()));
    }
    if l.extension_runs.len() < 50 {
        return Err(format!("only {} revisions ran", l.extension_runs.len()));
    }
    for k in ["restart", "crash_in_write", "lose_result"] {
        if count(k) == 0 {
            return Err(format!("no {k} in the seed"));
        }
    }
    if !l
        .results
        .values()
        .any(|c| u64::from(*c) == nanoprobe_transport::layout::result::AT_REVISION)
    {
        return Err("no command was answered AT_REVISION".into());
    }
    Ok(())
}

#[test]
fn seeded_mixed_commands_apply_set_state_once_and_effects_at_most_once() {
    let seeds: u64 = std::env::var("NANOPROBE_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    for seed in 0..seeds {
        if let Err(e) = run(seed, 1_000) {
            panic!("seed {seed:#x}: {e}");
        }
    }
}
