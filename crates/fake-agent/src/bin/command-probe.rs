//! `command-probe --map PATH --port N --token-file PATH [--seed N]
//! [--commands N] [--restart-every N] [--campaign N] [--seconds N]`
//!
//! The command path's exit test against a real agent (step S14): the
//! sidecar's store and reconciler, over the real link ([`LinkSim`]), write
//! `--commands` seeded mixed commands, set-state (`EXTENSION_SET`, from
//! changes to the store's extensions) and effects (`EXTENSION_INVOKE`), and
//! restart the sidecar after every `--restart-every` of them: a new link
//! session and a new reconciler from the kept store, as a sidecar restart
//! does. The agent needs raw Lua on.
//!
//! Each extension's chunk counts its own runs, by name and revision, and its
//! `invoke` counts each token, in one table in the `server` state's globals
//! (`__nanoprobe_s14`). At the end an `audit` extension returns that table
//! as an `invoke` value. The run passes when every revision run was run
//! once, each extension's last revision was run, every token was invoked at
//! most once, and the journalled outcomes match what ran: no token that
//! ran is journalled refused, rejected or not sent, no applied token or
//! revision is missing from the audit. It needs at least one sidecar
//! restart per `--restart-every` commands, and `--restart-every` below
//! `--commands`. At each restart it counts the commands in flight (the
//! reconciler's and the link's queue), and fails when no restart cut one: a
//! run whose restarts all fell between quiet points proves nothing. It also
//! reports the `AT_REVISION` results. It prints one JSON object per line: `progress`
//! lines, then `{"ev":"verdict","pass":B,...}`, and exits 0 on a pass.
//!
//! `--part world` runs the S15 exit test instead (100 groups spawned and
//! removed with every over-budget frame attributed to a command, and a group
//! replaced while paused); its options and rules are in
//! `nanoprobe_fake_agent::world_exit`.
//!
//! `--part players` runs the S16 live test (menus, marks, drawings, messages,
//! chat, extensions across a mission load, raw Lua gating, and moderation of a
//! scripted client); its steps and options are in
//! `nanoprobe_fake_agent::players_exit`.
//!
//! Test tooling only: built from source, never released.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::linksim::LinkSim;
use nanoprobe_fake_agent::{json_string, read_token};
use nanoprobe_sidecar::ids::CampaignId;
use nanoprobe_sidecar::link::Config;
use nanoprobe_sidecar::reconciler::{Config as RecConfig, Reconciler};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::store::{Entry, Outcome, Store};
use nanoprobe_transport::codec::{Lv, Reader};
use nanoprobe_transport::layout::{kind, opcode};
use nanoprobe_transport::records::{CommandHeader, CommandResult};

const NAMES: [&str; 4] = ["s14a", "s14b", "s14c", "s14d"];
const AUDIT: &str = "s14audit";

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

/// An extension's source: the chunk counts its run, and `invoke` counts
/// each token and returns the count.
fn source(name: &str, rev: u64) -> String {
    format!(
        "local c = rawget(_G, '__nanoprobe_s14') or {{}}\n\
         rawset(_G, '__nanoprobe_s14', c)\n\
         local k = 'set:{name}:{rev}'\n\
         c[k] = (c[k] or 0) + 1\n\
         return {{ invoke = function(token)\n\
           local t = 'inv:' .. tostring(token)\n\
           c[t] = (c[t] or 0) + 1\n\
           return c[t]\n\
         end }}\n"
    )
}

const AUDIT_SOURCE: &str =
    "return { invoke = function() return rawget(_G, '__nanoprobe_s14') or {} end }";

/// The adapter, with every `RESULT` copied out on the way to the reconciler.
struct Tap<'a> {
    sim: &'a mut LinkSim,
    results: &'a mut Vec<CommandResult>,
    /// The extension name of each `EXTENSION_SET` sent, by (epoch, seq): the
    /// journal's command entry holds the revision, not the name.
    names: &'a mut BTreeMap<(u64, u64), String>,
}

impl SimAdapter for Tap<'_> {
    fn position_after(&self, opcode: u16, payload: &[u8]) -> u64 {
        self.sim.position_after(opcode, payload)
    }

    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached> {
        if opcode == nanoprobe_transport::layout::opcode::EXTENSION_SET as u16 {
            let mut r = Reader::new(payload);
            if let Ok(h) = CommandHeader::read(&mut r)
                && let Ok(name) = r.str()
            {
                self.names.insert((h.epoch, h.seq), name.to_owned());
            }
        }
        self.sim.send(opcode, payload)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        let records = self.sim.read();
        for r in &records {
            if u64::from(r.kind) == kind::RESULT
                && let Ok(c) = CommandResult::decode(&r.payload)
            {
                self.results.push(c);
            }
        }
        records
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn num(args: &[String], name: &str, default: u64) -> Result<u64, String> {
    arg(args, name).map_or(Ok(default), |v| {
        v.parse().map_err(|_| format!("{name} takes a number"))
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            println!("{{\"ev\":\"error\",\"text\":{}}}", json_string(&e));
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<bool, String> {
    // The S15 exit test is its own module; see `nanoprobe_fake_agent::world_exit`.
    match arg(args, "--part").as_deref() {
        None | Some("commands") => {}
        Some("world") => return nanoprobe_fake_agent::world_exit::run(args),
        Some("players") => return nanoprobe_fake_agent::players_exit::run(args),
        Some(other) => {
            return Err(format!("--part is commands, world or players, not {other}"));
        }
    }
    let need = |name: &str| arg(args, name).ok_or(format!("{name} is required"));
    let token = read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
    let port: u16 = need("--port")?
        .parse()
        .map_err(|_| "--port takes a number")?;
    let seed = num(args, "--seed", 1)?;
    let commands = num(args, "--commands", 1_000)? as usize;
    let restart_every = num(args, "--restart-every", 100)? as usize;
    // The exit test needs forced sidecar restarts.
    if restart_every == 0 || restart_every >= commands {
        return Err("--restart-every must be above 0 and below --commands".into());
    }
    let campaign = num(args, "--campaign", 7)?;
    let seconds = num(args, "--seconds", 900)?;
    let cfg = Config {
        map_path: need("--map")?.into(),
        handshake: ([127, 0, 0, 1], port).into(),
        token,
        poll: Duration::from_millis(2),
        handshake_timeout: Duration::from_secs(15),
    };
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut store = Store::new(CampaignId(campaign), 1_000);
    let mut rec = Reconciler::new(RecConfig::default(), &store);
    let mut sim = LinkSim::start(cfg.clone(), Default::default());
    let mut results = Vec::new();
    let mut names: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut rng = Rng(seed);
    let mut version: BTreeMap<&str, u64> = BTreeMap::new();
    // Tokens of the invokes that were sent, with their (epoch, seq).
    let mut sent: Vec<(u64, (u64, u64))> = Vec::new();
    let mut tried = 0u64;
    let mut restarts = 0u32;
    let mut mispredicted = 0u64;
    // Commands in flight, summed over the restarts, and restarts that cut
    // at least one.
    let mut cut_commands = 0usize;
    let mut cutting_restarts = 0u32;
    let written = |store: &Store| {
        store
            .entries()
            .iter()
            .filter(|e| {
                matches!(e, Entry::Command(c)
                if c.opcode == opcode::EXTENSION_SET as u16
                    || c.opcode == opcode::EXTENSION_INVOKE as u16)
            })
            .count()
    };
    let mut next_restart = restart_every;
    let mut last_progress = Instant::now();
    loop {
        if Instant::now() > deadline {
            return Err(format!(
                "timed out with {} commands written",
                written(&store)
            ));
        }
        rec.step(
            &mut store,
            &mut Tap {
                sim: &mut sim,
                results: &mut results,
                names: &mut names,
            },
        );
        let n = written(&store);
        if n >= commands {
            break;
        }
        if n >= next_restart {
            next_restart += restart_every;
            let cut = rec.inflight().count() + sim.queued();
            cut_commands += cut;
            cutting_restarts += u32::from(cut > 0);
            mispredicted += sim.mispredicted();
            let last = sim.last_numbers();
            drop(sim);
            sim = LinkSim::start(cfg.clone(), last);
            rec = Reconciler::new(RecConfig::default(), &store);
            restarts += 1;
        }
        if rec.ready(&store) && sim.ready() && sim.queued() < 8 {
            let x = rng.next();
            let name = NAMES[(x >> 8) as usize % NAMES.len()];
            if x.is_multiple_of(2) {
                let v = version.entry(name).or_default();
                *v += 1;
                store.set_extension(name, &source(name, *v));
            } else {
                // A token is used once: one that was not sent is not reused.
                tried += 1;
                let token = tried;
                let args = Lv::Table(vec![(Lv::Number(1.0), Lv::Number(token as f64))]);
                let key = rec.invoke(
                    &mut store,
                    &mut Tap {
                        sim: &mut sim,
                        results: &mut results,
                        names: &mut names,
                    },
                    name,
                    args,
                );
                // `invoke` sends nothing when not attached: no key then.
                if let Some(key) = key {
                    sent.push((token, key));
                }
            }
        }
        if last_progress.elapsed() > Duration::from_secs(5) {
            last_progress = Instant::now();
            println!(
                "{{\"ev\":\"progress\",\"written\":{n},\"restarts\":{restarts},\"results\":{}}}",
                results.len()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Let the last revisions reach the agent: nothing in flight, every
    // extension held.
    store.set_extension(AUDIT, AUDIT_SOURCE);
    let settle_until = Instant::now() + Duration::from_secs(60);
    while Instant::now() < settle_until {
        rec.step(
            &mut store,
            &mut Tap {
                sim: &mut sim,
                results: &mut results,
                names: &mut names,
            },
        );
        if rec.inflight().next().is_none() && sim.queued() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let audit = rec
        .invoke(
            &mut store,
            &mut Tap {
                sim: &mut sim,
                results: &mut results,
                names: &mut names,
            },
            AUDIT,
            Lv::Table(vec![]),
        )
        .ok_or("the audit could not be sent")?;
    let mut counts = None;
    let audit_until = Instant::now() + Duration::from_secs(30);
    while counts.is_none() && Instant::now() < audit_until {
        rec.step(
            &mut store,
            &mut Tap {
                sim: &mut sim,
                results: &mut results,
                names: &mut names,
            },
        );
        counts = results
            .iter()
            .find(|r| (r.epoch, r.seq) == audit)
            .map(|r| r.value.clone());
        std::thread::sleep(Duration::from_millis(5));
    }
    mispredicted += sim.mispredicted();
    let counts = counts.ok_or("no audit result")?;
    let Lv::Table(pairs) = counts else {
        return Err(format!("the audit returned {counts:?}"));
    };
    let mut runs: BTreeMap<String, f64> = BTreeMap::new();
    for (k, v) in pairs {
        if let (Lv::String(k), Lv::Number(n)) = (k, v) {
            runs.insert(String::from_utf8_lossy(&k).into_owned(), n);
        }
    }
    let twice: Vec<&String> = runs
        .iter()
        .filter(|(_, n)| **n != 1.0)
        .map(|(k, _)| k)
        .collect();
    let missing: Vec<String> = version
        .iter()
        .map(|(name, v)| format!("set:{name}:{v}"))
        .filter(|k| !runs.contains_key(k))
        .collect();
    let outcomes = |o: Outcome| {
        store
            .entries()
            .iter()
            .filter(|e| matches!(e, Entry::Outcome { outcome, .. } if *outcome == o))
            .count()
    };
    // The journalled outcome of each command, the last one written.
    let mut outcome_of: BTreeMap<(u64, u64), Outcome> = BTreeMap::new();
    for e in store.entries() {
        if let Entry::Outcome {
            epoch,
            seq,
            outcome,
        } = e
        {
            outcome_of.insert((*epoch, *seq), *outcome);
        }
    }
    // Effects: a token that ran needs an Applied or Unknown outcome (a
    // result lost with the link is Unknown); an Applied one must have run;
    // one the reconciler called Refused, Rejected or NotSent never ran.
    let mut mismatched: Vec<String> = Vec::new();
    for (token, key) in &sent {
        let ran = runs.contains_key(&format!("inv:{token}"));
        let outcome = outcome_of.get(key).copied();
        let ok = match (ran, outcome) {
            (true, Some(Outcome::Applied | Outcome::Unknown)) => true,
            (false, Some(Outcome::Applied)) => false,
            (false, Some(_)) => true,
            (true, _) => false,
            (false, None) => true,
        };
        if !ok {
            mismatched.push(format!("inv:{token} ran={ran} outcome={outcome:?}"));
        }
    }
    // A token in the audit that was never sent ran from nowhere.
    let sent_tokens: std::collections::BTreeSet<String> =
        sent.iter().map(|(t, _)| format!("inv:{t}")).collect();
    for k in runs.keys().filter(|k| k.starts_with("inv:")) {
        if !sent_tokens.contains(k) {
            mismatched.push(format!("{k} ran and was not sent"));
        }
    }
    // Set-state, by extension name and revision: an Applied EXTENSION_SET
    // appears in the audit, and a revision in the audit was not journalled
    // only as NotSent, Rejected or Refused.
    let mut set_outcomes: BTreeMap<String, Vec<Outcome>> = BTreeMap::new();
    for e in store.entries() {
        if let Entry::Command(c) = e
            && c.opcode == opcode::EXTENSION_SET as u16
        {
            let Some(name) = names.get(&(c.epoch, c.seq)) else {
                // Never sent: it cannot have run, and must not be Applied.
                if outcome_of.get(&(c.epoch, c.seq)) == Some(&Outcome::Applied) {
                    mismatched.push(format!("set rev {} applied and never sent", c.rev));
                }
                continue;
            };
            // The audit extension's chunk records no run of its own.
            if name == AUDIT {
                continue;
            }
            let key = format!("set:{name}:{}", c.rev);
            let outcome = outcome_of.get(&(c.epoch, c.seq)).copied();
            if outcome == Some(Outcome::Applied) && !runs.contains_key(&key) {
                mismatched.push(format!("{key} applied and not run"));
            }
            if let Some(o) = outcome {
                set_outcomes.entry(key).or_default().push(o);
            }
        }
    }
    for (key, os) in &set_outcomes {
        let only_failed = os
            .iter()
            .all(|o| matches!(o, Outcome::NotSent | Outcome::Rejected | Outcome::Refused));
        if only_failed && runs.contains_key(key) {
            mismatched.push(format!("{key} ran and was journalled {os:?}"));
        }
    }
    // Every token journalled Applied must be in the audit: no fewer `inv:`
    // keys than Applied tokens (the agent's reply to a large audit is cut).
    let applied_tokens = sent
        .iter()
        .filter(|(_, k)| outcome_of.get(k) == Some(&Outcome::Applied))
        .count();
    let audited_tokens = runs.keys().filter(|k| k.starts_with("inv:")).count();
    if audited_tokens < applied_tokens {
        mismatched.push(format!(
            "the audit holds {audited_tokens} tokens, {applied_tokens} were applied"
        ));
    }
    // The exit test needs the restarts; see `xtask commands`.
    let expected_restarts = commands.div_ceil(restart_every).saturating_sub(1) as u32;
    let wrote = written(&store);
    let at_revision = results
        .iter()
        .filter(|r| u64::from(r.code) == nanoprobe_transport::layout::result::AT_REVISION)
        .count();
    let pass = twice.is_empty()
        && missing.is_empty()
        && mispredicted == 0
        && mismatched.is_empty()
        && restarts >= expected_restarts
        && cutting_restarts > 0
        && wrote >= commands;
    println!(
        "{{\"ev\":\"verdict\",\"pass\":{pass},\"written\":{wrote},\"set_runs\":{},\"invokes\":{},\
         \"tokens\":{},\"tokens_tried\":{tried},\"restarts\":{restarts},\
         \"expected_restarts\":{expected_restarts},\"mispredicted\":{mispredicted},\
         \"cut_commands\":{cut_commands},\"cutting_restarts\":{cutting_restarts},\
         \"at_revision\":{at_revision},\
         \"twice\":{},\"missing\":{},\"mismatched\":{},\"applied\":{},\"refused\":{},\
         \"rejected\":{},\"unknown\":{},\"not_sent\":{}}}",
        runs.keys().filter(|k| k.starts_with("set:")).count(),
        runs.keys().filter(|k| k.starts_with("inv:")).count(),
        sent.len(),
        json_string(&format!("{twice:?}")),
        json_string(&format!("{missing:?}")),
        json_string(&format!("{mismatched:?}")),
        outcomes(Outcome::Applied),
        outcomes(Outcome::Refused),
        outcomes(Outcome::Rejected),
        outcomes(Outcome::Unknown),
        outcomes(Outcome::NotSent),
    );
    Ok(pass)
}
