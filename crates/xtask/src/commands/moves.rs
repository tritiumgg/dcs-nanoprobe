//! `commands <server> --part moves --client <windows-target>`: slot moves on
//! the deployed agent (spec/commands.md, "Slot moves"). A scripted client
//! joins and holds no slot, and `command-probe --part players --scenario
//! moves` in the server's game container sends each `MODERATE SLOT` over the
//! real link and reads the client's side and slot from `PLAYERS`. Test
//! tooling: built from source, never released.
//!
//! The server needs a running mission that lists a blue player slot: the
//! mission the server starts with, or `--miz FILE` loaded through the
//! development loader. Each move's step prints the handler's `cost_us`.
//! The client is a windows-ssh target running a `--dev` build at its main
//! menu, and the server's profile names `client_address` and
//! `client_password_file` (`client join`). In order:
//!
//! 1. deploy `--dev` and restart DCS on the server; start the probe;
//! 2. the client joins; the tool reads the first blue slot the client is
//!    offered and gives it to the probe;
//! 3. the probe's moves (`players_exit.rs`, "--scenario moves");
//! 4. at `race_armed` the tool tells the probe to go and runs
//!    `lua/experiments/player-moves/client-side.lua` on the client, which
//!    changes the client to red with `net.set_coalition`;
//! 5. the client leaves, whatever happened.
//!
//! The tool leaves the server running the build it deployed: the caller
//! restores it. Everything goes under `bundles/commands-moves-<name>-<time>/`,
//! with `moves.json`.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use super::players::{DIR, answer_file, drive, passed};
use crate::install::{Profile as _, RUNNING};
use crate::profile::Target;
use crate::sampling::live::{self, Opened, Run};
use crate::windows::Windows;
use crate::{bundle, eval, link};

/// Eval timeout for a client snippet, seconds.
const CLIENT_S: u64 = 120;

const LEAVE: &str = include_str!("../../../../lua/experiments/alone/e31-leave.lua");
const CLIENT_SIDE: &str = include_str!("../../../../lua/experiments/player-moves/client-side.lua");

/// The first blue slot the client is offered that is no crew seat and that no
/// connected player holds, as text; empty when there is none.
const FIRST_BLUE: &str = "-- states: hook\n\
local held = {}\n\
for _, q in ipairs(net.get_player_list() or {}) do\n\
  held[tostring(net.get_player_info(q, 'slot'))] = true\n\
end\n\
for _, s in ipairs(DCS.getAvailableSlots('blue') or {}) do\n\
  local id = tostring(s.unitId)\n\
  if not held[id] and not id:find('_%d+$') then\n\
    return id\n\
  end\n\
end\n\
return ''\n";

/// The steps a run must pass.
const REQUIRED: [&str; 13] = [
    "link",
    "players_query",
    "players_query_client",
    "connect",
    "blue_no_slot",
    "blue_slot",
    "keep_leaves_slot",
    "other_coalition_refused",
    "missing_slot_refused",
    "red_no_slot",
    "keep_takes_slot_coalition",
    "keep_race",
    "spectators",
];

/// The client-side snippet that changes the client to red.
fn to_red() -> String {
    CLIENT_SIDE.replacen("'<side>'", "'1'", 1)
}

/// A snippet's reply on the client; an error when it failed.
fn client_eval(w: &Windows, stem: &str, source: &str) -> Result<Value, String> {
    let r = eval::run(
        w,
        "hook",
        stem,
        source.as_bytes(),
        Duration::from_secs(CLIENT_S),
    )?;
    if r["ok"] != json!(true) || r["reply"]["ok"] != json!(true) {
        return Err(format!("{stem}: {r}"));
    }
    Ok(r["reply"].clone())
}

pub fn run(
    repository: &Path,
    name: &str,
    client: &str,
    miz: Option<&Path>,
) -> Result<ExitCode, String> {
    let w = match crate::target(client)? {
        Target::WindowsSsh { ssh, profile, .. } => Windows::connect(&ssh, &profile)?,
        _ => return Err(format!("{client} is not a windows-ssh target")),
    };
    if !matches!(crate::target(name)?, Target::DockerWine { .. }) {
        return Err(format!("{name} is not a docker-wine target"));
    }
    if w.read(RUNNING)?.is_none() {
        return Err(format!(
            "no hook runs on {client}: deploy with --dev and start DCS there first"
        ));
    }
    let mission = match miz {
        Some(p) => Some(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?),
        None => None,
    };
    let Opened { run, .. } = live::open(repository, name, "commands-moves")?;
    let started = bundle::utc_now();
    let mut client_steps = Vec::new();
    let result = attempt(
        &run,
        name,
        client,
        mission.as_deref(),
        &w,
        &mut client_steps,
    );
    let _ = client_eval(&w, "moves-leave", LEAVE);
    let (verdict, error) = match &result {
        Ok(v) => (v.clone(), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    let ok = result.as_ref().is_ok_and(meets);
    let out = json!({
        "target": name,
        "client": client,
        "started": started,
        "ended": bundle::utc_now(),
        "client_steps": client_steps,
        "verdict": verdict,
        "error": error,
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "client_agent_build": crate::soak::agent_build(client),
        "client_provenance": crate::soak::platform_provenance(client),
        "ok": ok,
    });
    let path = run.folder.join("moves.json");
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

/// Whether the probe's verdict passes every required step.
fn meets(verdict: &Value) -> bool {
    let done = passed(verdict);
    verdict["pass"] == json!(true) && REQUIRED.iter().all(|n| done.iter().any(|d| d == n))
}

fn attempt(
    run: &Run,
    name: &str,
    client: &str,
    miz: Option<&[u8]>,
    w: &Windows,
    client_steps: &mut Vec<Value>,
) -> Result<Value, String> {
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "command-probe")])?
        .pop()
        .ok_or("command-probe was not built")?;
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    println!("== restart");
    crate::restart(name)?;
    if let Some(bytes) = miz {
        println!("== load the mission");
        crate::live::load(&run.remote, "np-moves.miz", bytes)?;
    }
    run.sh(
        &format!("pkill -f '[c]ommand-probe --part players' || true; rm -rf {DIR}"),
        &[],
    )?;
    link::upload_container(&run.remote, DIR, &probe, "command-probe")?;
    run.sh(&format!("mkdir -p {DIR}/control"), &[])?;
    let [map, port, token] = run.link_args();
    println!("== command-probe --part players --scenario moves");
    run.sh(
        &format!(
            "nohup {DIR}/command-probe --part players --scenario moves \
             --map \"$1\" --port \"$2\" --token-file \"$3\" --control-dir {DIR}/control \
             --seconds 900 > {DIR}/moves.jsonl 2> {DIR}/moves.err < /dev/null &"
        ),
        &[map, port, token],
    )?;
    let mut notes = Vec::new();
    let result = drive(run, "moves", false, &mut notes, &mut |run, phase, _, _| {
        on_phase(run, phase, client, name, w, client_steps)
    });
    let _ = run.sh("pkill -f '[c]ommand-probe --part players' || true", &[]);
    for file in ["moves.jsonl", "moves.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    result
}

/// What the client does at each phase of the probe.
fn on_phase(
    run: &Run,
    phase: &str,
    client: &str,
    server: &str,
    w: &Windows,
    steps: &mut Vec<Value>,
) -> Result<(), String> {
    match phase {
        "ready" => {
            let code = crate::client::command(&["join", client, server])?;
            if format!("{code:?}") != format!("{:?}", ExitCode::SUCCESS) {
                return Err("the client did not join".into());
            }
            let reply = client_eval(w, "moves-first-blue", FIRST_BLUE)?;
            let slot = reply["values"][0].as_str().unwrap_or("").to_owned();
            steps.push(json!({"name": "client_joined", "verdict": "pass", "detail": format!("blue slot {slot}")}));
            answer_file(run, "joined", &slot)
        }
        "race_armed" => {
            answer_file(run, "race_go", "ok")?;
            let detail = match client_eval(w, "moves-to-red", &to_red()) {
                Ok(r) => r["observations"].to_string(),
                Err(e) => e,
            };
            steps.push(json!({"name": "client_to_red", "verdict": "note", "detail": detail}));
            Ok(())
        }
        "race_done" => answer_file(run, "race_checked", "ok"),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_snippet_changes_to_red() {
        let s = to_red();
        assert!(s.contains("tonumber('1')"));
        assert!(!s.contains("<side>"));
    }

    #[test]
    fn a_run_passes_only_with_every_required_step() {
        let steps: Vec<Value> = REQUIRED
            .iter()
            .map(|n| json!({"ev": "step", "name": n, "verdict": "pass"}))
            .collect();
        let all = json!({"pass": true, "steps": steps});
        assert!(meets(&all), "{all}");
        let mut fewer = all.clone();
        fewer["steps"].as_array_mut().unwrap().pop();
        assert!(!meets(&fewer));
        let mut failed = all;
        failed["pass"] = json!(false);
        assert!(!meets(&failed));
    }
}
