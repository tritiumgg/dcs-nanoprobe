//! `commands <name>`: the S14 exit test on a docker-wine target.
//!
//! Deploys a build with the eval channel, turns raw Lua on in `agent.toml`
//! (extensions need it), restarts DCS and loads a fresh shell mission. Then
//! it runs `command-probe` in the game container as the sidecar: seeded
//! mixed commands (`EXTENSION_SET` and `EXTENSION_INVOKE`) with a sidecar
//! restart after every `--restart-every` of them, judged by the probe's
//! own audit of what the agent ran. While the probe runs, the simulation
//! is held running, so the server state's frames pump the commands.
//!
//! It puts `agent.toml` back as it found it, whatever the outcome; the
//! agent reads it at the next restart. It leaves the target running the
//! build it deployed, with the shell mission: the caller restores the
//! target. Everything goes under `bundles/commands-<name>-<time>/`, with
//! `commands.json`, the run's verdict.
//!
//! `--part world` runs the S15 exit test instead, and `--part names` the
//! naming check (`commands/world.rs`).
//! `--part players --raw-lua on|off` and `--part moderation --client <target>
//! [--miz FILE]` run the S16 live tests (`commands/players.rs`,
//! `commands/moderation.rs`).

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::install::{AGENT_TOML, Profile as _};
use crate::sampling::live::{self, Opened, Run};
use crate::{bundle, link};

mod moderation;
mod players;
mod world;

/// Where the probe goes in the container.
const DIR: &str = "/tmp/nanoprobe-commands";
/// The probe's own deadline, seconds.
const PROBE_S: u64 = 1_200;
/// Model seconds each hold runs while the probe works.
const HOLD_S: f64 = 15.0;

pub fn command(repository: &Path, args: &[&str]) -> Result<ExitCode, String> {
    let [name, rest @ ..] = args else {
        return Err(USAGE.into());
    };
    let (mut seed, mut commands, mut restart_every) = (1u64, 1_000u64, 100u64);
    let (mut groups, mut units) = (100u64, 4u64);
    let mut part = "commands";
    let (mut raw_lua, mut client, mut miz): (Option<&str>, Option<&str>, Option<&str>) =
        (None, None, None);
    let (mut template, mut min_ground): (Option<&str>, Option<f64>) = (None, None);
    let mut rest = rest;
    while let [flag, v, tail @ ..] = rest {
        if *flag == "--part" {
            part = v;
        } else if *flag == "--raw-lua" {
            raw_lua = Some(v);
        } else if *flag == "--client" {
            client = Some(v);
        } else if *flag == "--miz" {
            miz = Some(v);
        } else if *flag == "--template" {
            template = Some(v);
        } else if *flag == "--min-ground" {
            let m = v.parse::<f64>().map_err(|e| format!("{flag}: {e}"))?;
            if !m.is_finite() {
                return Err("--min-ground is a number of meters".into());
            }
            min_ground = Some(m);
        } else {
            let n = v.parse::<u64>().map_err(|e| format!("{flag}: {e}"))?;
            match *flag {
                "--seed" => seed = n,
                "--commands" => commands = n,
                "--restart-every" => restart_every = n,
                "--groups" => groups = n,
                "--units" => units = n,
                _ => return Err(USAGE.into()),
            }
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(USAGE.into());
    }
    match part {
        "world" => {
            if groups == 0 || units == 0 || units > 50 {
                return Err("--groups is at least 1 and --units between 1 and 50".into());
            }
            return world::run(repository, name, groups, units);
        }
        "names" => {
            if units == 0 || units > 50 {
                return Err("--units is between 1 and 50".into());
            }
            return world::run_names(repository, name, units);
        }
        "players" => {
            let raw = match raw_lua {
                Some("on") => true,
                Some("off") => false,
                _ => return Err("--part players needs --raw-lua on|off".into()),
            };
            return players::run(repository, name, raw);
        }
        "moderation" => {
            let client = client.ok_or("--part moderation needs --client <windows-target>")?;
            return moderation::run(repository, name, client, miz.map(Path::new));
        }
        "debris" => {
            if groups < 4 || units == 0 || units > 50 {
                return Err(
                    "--part debris needs --groups 4 or more and --units between 1 and 50".into(),
                );
            }
            return world::run_debris(
                repository,
                name,
                groups,
                units,
                template.map(Path::new),
                min_ground,
            );
        }
        "commands" => {}
        other => {
            return Err(format!(
                "--part is commands, world, names, players, moderation or debris, not {other}"
            ));
        }
    }
    // The exit test needs forced sidecar restarts.
    if restart_every == 0 || restart_every >= commands {
        return Err("--restart-every must be above 0 and below --commands".into());
    }
    run(repository, name, seed, commands, restart_every)
}

const USAGE: &str = "usage: cargo xtask commands <name> [--seed N] [--commands 1000] \
    [--restart-every 100] | cargo xtask commands <name> --part world [--groups 100] [--units 4] \
    | cargo xtask commands <name> --part names [--units 4] \
    | cargo xtask commands <name> --part debris [--groups 100] [--units 4] [--template FILE] \
    [--min-ground METERS] \
    | cargo xtask commands <name> --part players --raw-lua on|off \
    | cargo xtask commands <name> --part moderation --client <windows-target> [--miz FILE]";

/// `old` with `[lua] raw = true`, every other key kept.
fn raw_on(old: &str) -> Result<String, String> {
    let mut table: toml::Table =
        toml::from_str(old).map_err(|e| format!("agent.toml on the target: {e}"))?;
    let lua = table
        .entry("lua")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(lua) = lua.as_table_mut() else {
        return Err("agent.toml on the target: lua is not a table".into());
    };
    lua.insert("raw".into(), toml::Value::Boolean(true));
    toml::to_string(&table).map_err(|e| e.to_string())
}

/// The run's restarts the exit test requires: one after every
/// `restart_every` commands written, none after the last.
fn expected_restarts(commands: u64, restart_every: u64) -> u64 {
    commands.div_ceil(restart_every).saturating_sub(1)
}

/// Whether the verdict meets the exit test: the probe passed, it wrote every
/// command, and the sidecar restarted as often as the test requires. A probe
/// that ran with no restarts does not pass whatever its audit says.
fn meets_exit_test(verdict: &Value, commands: u64, restart_every: u64) -> bool {
    verdict["pass"] == json!(true)
        && verdict["written"].as_u64().is_some_and(|w| w >= commands)
        && verdict["restarts"]
            .as_u64()
            .is_some_and(|r| r >= expected_restarts(commands, restart_every))
}

fn run(
    repository: &Path,
    name: &str,
    seed: u64,
    commands: u64,
    restart_every: u64,
) -> Result<ExitCode, String> {
    let Opened { run, shell, .. } = live::open(repository, name, "commands")?;
    let started = bundle::utc_now();
    let result = attempt(&run, name, &shell, seed, commands, restart_every);
    // The record is written on every outcome, with the provenance of the run.
    let (verdict, error) = match &result {
        Ok(v) => (v.clone(), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    let ok = result
        .as_ref()
        .is_ok_and(|v| meets_exit_test(v, commands, restart_every));
    let out = json!({
        "target": name,
        "started": started,
        "ended": bundle::utc_now(),
        "seed": seed,
        "commands": commands,
        "restart_every": restart_every,
        "expected_restarts": expected_restarts(commands, restart_every),
        "verdict": verdict,
        "error": error,
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("commands.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "== {}: {}",
        if ok { "PASS" } else { "FAIL" },
        path.display()
    );
    result?;
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Build and deploy, run the probe, and put the target's files back: the
/// probe's verdict, or the first error.
fn attempt(
    run: &Run,
    name: &str,
    shell: &[u8],
    seed: u64,
    commands: u64,
    restart_every: u64,
) -> Result<Value, String> {
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "command-probe")])?
        .pop()
        .ok_or("command-probe was not built")?;
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let old = run
        .remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    run.remote.write(
        AGENT_TOML,
        raw_on(&String::from_utf8_lossy(&old))?.as_bytes(),
    )?;
    let result = with_raw(run, name, shell, &probe, seed, commands, restart_every);
    // Put agent.toml back before anything else can fail.
    let put_back = run.remote.write(AGENT_TOML, &old);
    let _ = run.sh("pkill -f '[c]ommand-probe --map' || true", &[]);
    for file in ["probe.jsonl", "probe.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    if let Err(e) = put_back {
        return Err(match result {
            Ok(_) => format!("agent.toml was not put back: {e}"),
            Err(r) => format!("{r}; agent.toml was not put back: {e}"),
        });
    }
    println!("== agent.toml put back as found");
    result
}

/// Restart with raw Lua on, load the shell, run the probe to its verdict.
fn with_raw(
    run: &Run,
    name: &str,
    shell: &[u8],
    probe: &Path,
    seed: u64,
    commands: u64,
    restart_every: u64,
) -> Result<Value, String> {
    println!("== restart, raw Lua on");
    crate::restart(name)?;
    println!("== load a fresh shell mission");
    crate::live::load(&run.remote, "np-shell.miz", shell)?;
    run.sh(
        &format!("pkill -f '[c]ommand-probe --map' || true; rm -rf {DIR}"),
        &[],
    )?;
    link::upload_container(&run.remote, DIR, probe, "command-probe")?;
    let [map, port, token] = run.link_args();
    let (seed, commands, restart_every) = (
        seed.to_string(),
        commands.to_string(),
        restart_every.to_string(),
    );
    println!("== command-probe: {commands} commands, a restart every {restart_every}");
    run.sh(
        &format!(
            "nohup {DIR}/command-probe --map \"$1\" --port \"$2\" --token-file \"$3\" \
             --seed \"$4\" --commands \"$5\" --restart-every \"$6\" --seconds {PROBE_S} \
             > {DIR}/probe.jsonl 2> {DIR}/probe.err < /dev/null &"
        ),
        &[map, port, token, &seed, &commands, &restart_every],
    )?;
    let deadline = Instant::now() + Duration::from_secs(PROBE_S + 120);
    loop {
        let lines =
            link::parse_lines(&run.sh(&format!("cat {DIR}/probe.jsonl 2>/dev/null || true"), &[])?);
        if let Some(v) = lines.iter().find(|l| l["ev"] == "verdict") {
            return Ok(v.clone());
        }
        if let Some(p) = lines.iter().rev().find(|l| l["ev"] == "progress") {
            println!("   {p}");
        }
        let alive = run.sh(
            "pgrep -f '[c]ommand-probe --map' >/dev/null && echo yes || true",
            &[],
        )?;
        if !String::from_utf8_lossy(&alive).contains("yes") {
            let err = run.sh(
                &format!("tail -c 2000 {DIR}/probe.err 2>/dev/null || true"),
                &[],
            )?;
            return Err(format!(
                "command-probe ended with no verdict: {}",
                String::from_utf8_lossy(&err).trim()
            ));
        }
        if Instant::now() > deadline {
            return Err("command-probe gave no verdict in time".into());
        }
        run.hold(HOLD_S)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_without_the_required_restarts_does_not_meet_the_exit_test() {
        assert_eq!(expected_restarts(1000, 100), 9);
        assert_eq!(expected_restarts(1000, 999), 1);
        let v = |pass, written, restarts| json!({"pass": pass, "written": written, "restarts": restarts});
        assert!(meets_exit_test(&v(true, 1000, 9), 1000, 100));
        assert!(!meets_exit_test(&v(true, 1000, 8), 1000, 100));
        assert!(!meets_exit_test(&v(true, 999, 9), 1000, 100));
        assert!(!meets_exit_test(&v(false, 1000, 9), 1000, 100));
        assert!(!meets_exit_test(&json!({"pass": true}), 1000, 100));
    }

    #[test]
    fn raw_on_keeps_every_other_key() {
        let t = raw_on("[transport]\nport = 5000\nmap_path = 'Z:\\m'\n").unwrap();
        let parsed = nanoprobe_agent_link::config::AgentToml::parse(&t).unwrap();
        assert!(parsed.lua.raw);
        assert_eq!(parsed.transport.port, 5000);
        assert_eq!(parsed.transport.map_path, "Z:\\m");
    }

    #[test]
    fn raw_on_turns_an_off_flag_on() {
        let t = raw_on("[transport]\nmap_path = 'm'\n[lua]\nraw = false\n").unwrap();
        assert!(
            nanoprobe_agent_link::config::AgentToml::parse(&t)
                .unwrap()
                .lua
                .raw
        );
    }
}
