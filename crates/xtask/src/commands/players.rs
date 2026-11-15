//! `commands <name> --part players --raw-lua on|off`: the S16 live test on a
//! docker-wine target, needing no person.
//!
//! Deploys a build with the eval channel, checks that `agent.toml` holds the
//! `raw` setting `--raw-lua` names (it changes nothing in it: set the file,
//! then run), restarts DCS, loads a fresh shell mission and runs
//! `command-probe --part players` in the game container as the sidecar. The
//! probe sends the commands and judges the results and records
//! (`nanoprobe_fake_agent::players_exit` lists the steps). What only DCS can show
//! is this tool's part, answered through control files:
//!
//! - `menus_set`: the command item's callback in the server state, as ED's
//!   radio panel runs it, `missionCommands.doAction(index)`
//!   (`lua/experiments/s16/exit-menu-pick.lua`). DCS's numbering is
//!   UNVERIFIED, so it tries index 1, 2, ... up to 40 and stops when the probe
//!   prints a `MENU` record for the item. When the call is not there or
//!   raises, it answers `unreachable` and the probe reports the step as skipped.
//! - `marks_set` and `marks_removed`: `world.getMarkPanels()` in the server
//!   state (`lua/experiments/s16/exit-marks-read.lua`).
//! - the mission's path, learned from the hook state before the probe starts
//!   (`lua/experiments/s16/exit-mission-path.lua`), for `MISSION_LOAD`.
//!
//! The simulation is held running while the probe works, except while the
//! probe loads the mission. The target is left running the build it deployed,
//! with the shell mission: the caller restores the target. Everything goes
//! under `bundles/commands-players-<name>-<time>/`, with `players.json`, the
//! verdict. Run it twice, once for each mode.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::install::{AGENT_TOML, Profile as _};
use crate::sampling::live::{self, Opened, Run};
use crate::{bundle, link};

/// Where the probe goes in the container.
pub(super) const DIR: &str = "/tmp/nanoprobe-players";
/// The probe's own deadline, seconds.
const PROBE_S: u64 = 1_800;
/// Model seconds a hold runs while the probe works.
const HOLD_S: f64 = 3.0;
/// The highest action index tried for the menu command.
const MAX_INDEX: u32 = 40;

const MARKS_READ: &str = include_str!("../../../../lua/experiments/s16/exit-marks-read.lua");
const MISSION_PATH: &str = include_str!("../../../../lua/experiments/s16/exit-mission-path.lua");
const MENU_PICK: &str = include_str!("../../../../lua/experiments/s16/exit-menu-pick.lua");
/// The placeholder in [`MENU_PICK`].
const PLACEHOLDER: &str = "'<index>'";

/// The steps a run must pass in each mode. `menu_select` may also be skipped
/// when the tool could not reach the callback.
fn required(raw: bool) -> Vec<&'static str> {
    let mut steps = vec![
        "link",
        "menu_set",
        "menu_remove",
        "mark_set",
        "marks_present",
        "mark_remove",
        "marks_gone",
        "message_all",
        "message_coalition",
        "chat_all",
        "eval_gate",
    ];
    if raw {
        steps.extend([
            "extension_set",
            "extension_invoke",
            "extension_emit",
            "extension_record",
            "mission_load",
            "running_after_load",
            "extension_after_load",
            "extension_frames_after_load",
            "extension_record_after_load",
            "extension_emit_after_load",
            "extension_remove",
        ]);
    } else {
        steps.push("extension_gate");
    }
    steps
}

/// The `agent.toml` text's `[lua] raw`, off when absent.
pub(super) fn agent_raw(toml_text: &str) -> Result<bool, String> {
    let table: toml::Table =
        toml::from_str(toml_text).map_err(|e| format!("agent.toml on the target: {e}"))?;
    Ok(table
        .get("lua")
        .and_then(|l| l.get("raw"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false))
}

/// The menu snippet with `index` in place of its placeholder.
fn pick_source(index: u32) -> String {
    MENU_PICK.replacen(PLACEHOLDER, &index.to_string(), 1)
}

/// The names in `verdict`'s steps with verdict `pass`.
pub(super) fn passed(verdict: &Value) -> Vec<String> {
    steps_with(verdict, &["pass"])
}

fn steps_with(verdict: &Value, wanted: &[&str]) -> Vec<String> {
    verdict["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["verdict"].as_str().is_some_and(|v| wanted.contains(&v)))
        .filter_map(|s| s["name"].as_str().map(str::to_owned))
        .collect()
}

/// Whether the verdict meets the test in `raw` mode: the probe passed, it saw
/// the mode it was told to expect, and each required step passed.
pub(super) fn meets_exit_test(verdict: &Value, raw: bool) -> bool {
    missing(verdict, raw).is_empty()
        && verdict["pass"] == json!(true)
        && verdict["raw_lua_seen"] == json!(raw)
}

/// The required steps that did not pass. The menu selection must pass or be
/// skipped.
fn missing(verdict: &Value, raw: bool) -> Vec<String> {
    let done = passed(verdict);
    let skipped = steps_with(verdict, &["skip"]);
    let has = |list: &[String], name: &str| list.iter().any(|d| d == name);
    let mut out: Vec<String> = required(raw)
        .into_iter()
        .filter(|name| !has(&done, name))
        .map(str::to_owned)
        .collect();
    if !has(&done, "menu_select") && !has(&skipped, "menu_select") {
        out.push("menu_select".into());
    }
    out
}

pub fn run(repository: &Path, name: &str, raw: bool) -> Result<ExitCode, String> {
    let Opened { run, shell, .. } = live::open(repository, name, "commands-players")?;
    let started = bundle::utc_now();
    let mut notes = Vec::new();
    let result = attempt(&run, name, &shell, raw, &mut notes);
    let (verdict, error) = match &result {
        Ok(v) => (v.clone(), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    let missing = if verdict.is_null() {
        Vec::new()
    } else {
        missing(&verdict, raw)
    };
    let ok = result.as_ref().is_ok_and(|v| meets_exit_test(v, raw));
    let out = json!({
        "target": name,
        "started": started,
        "ended": bundle::utc_now(),
        "raw_lua_expected": raw,
        "controller": notes,
        "verdict": verdict,
        "missing_steps": missing,
        "error": error,
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("players.json");
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

fn attempt(
    run: &Run,
    name: &str,
    shell: &[u8],
    raw: bool,
    notes: &mut Vec<Value>,
) -> Result<Value, String> {
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "command-probe")])?
        .pop()
        .ok_or("command-probe was not built")?;
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let toml = run
        .remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let have = agent_raw(&String::from_utf8_lossy(&toml))?;
    if have != raw {
        return Err(format!(
            "agent.toml on the target has raw = {have}, and --raw-lua is {}: set `[lua] raw` \
             in the profile's nanoprobe/agent.toml, then run again",
            if raw { "on" } else { "off" }
        ));
    }
    println!("== restart, raw Lua {}", if raw { "on" } else { "off" });
    crate::restart(name)?;
    println!("== load a fresh shell mission");
    crate::live::load(&run.remote, "np-shell.miz", shell)?;
    let path = run.eval("hook", "s16-mission-path", MISSION_PATH, 60)?;
    let path = path.as_str().ok_or("the mission path is not a string")?;
    run.sh(
        &format!("pkill -f '[c]ommand-probe --part players' || true; rm -rf {DIR}"),
        &[],
    )?;
    link::upload_container(&run.remote, DIR, &probe, "command-probe")?;
    run.sh(&format!("mkdir -p {DIR}/control"), &[])?;
    let result = scenario(run, raw, path, notes);
    let _ = run.sh("pkill -f '[c]ommand-probe --part players' || true", &[]);
    for file in ["players.jsonl", "players.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    result
}

fn scenario(run: &Run, raw: bool, path: &str, notes: &mut Vec<Value>) -> Result<Value, String> {
    let [map, port, token] = run.link_args();
    println!(
        "== command-probe --part players, raw Lua {}",
        if raw { "on" } else { "off" }
    );
    run.sh(
        &format!(
            "nohup {DIR}/command-probe --part players --scenario functional --raw-lua {} \
             --map \"$1\" --port \"$2\" --token-file \"$3\" --mission-path \"$4\" \
             --control-dir {DIR}/control --seconds {PROBE_S} \
             > {DIR}/players.jsonl 2> {DIR}/players.err < /dev/null &",
            if raw { "on" } else { "off" }
        ),
        &[map, port, token, path],
    )?;
    drive(
        run,
        "players",
        true,
        notes,
        &mut |run, phase, line, notes| match phase {
            "menus_set" => {
                let command = line["command"].as_u64().ok_or("no command entity")?;
                let answer = pick_menu(run, command, notes)?;
                answer_file(run, "menus_picked", &answer)
            }
            "marks_set" => marks(run, "marks_read", notes),
            "marks_removed" => marks(run, "marks_gone", notes),
            _ => Ok(()),
        },
    )
}

/// Read the mark panels in the server state and answer with the read.
fn marks(run: &Run, file: &str, notes: &mut Vec<Value>) -> Result<(), String> {
    let read = run.eval("server", "s16-marks-read", MARKS_READ, 60)?;
    let read = read.as_str().ok_or("the mark read is not a string")?;
    notes.push(json!({"read": file, "value": read}));
    answer_file(run, file, read)
}

/// Write a control file for the probe: the text, put in place at once.
pub(super) fn answer_file(run: &Run, file: &str, text: &str) -> Result<(), String> {
    run.sh(
        &format!("printf '%s' \"$1\" > {DIR}/control/{file}.tmp && mv {DIR}/control/{file}.tmp {DIR}/control/{file}"),
        &[text],
    )?;
    Ok(())
}

/// Select the command item by action index in the server state, one index
/// after another, until the probe prints a `MENU` record for it.
fn pick_menu(run: &Run, command: u64, notes: &mut Vec<Value>) -> Result<String, String> {
    for index in 1..=MAX_INDEX {
        let value = match run.eval("server", "s16-menu-pick", &pick_source(index), 30) {
            Ok(v) => v,
            Err(e) => {
                notes.push(json!({"menu_pick_error": e}));
                return Ok(format!("unreachable: {e}"));
            }
        };
        if let Some(text) = value.as_str().filter(|t| t.starts_with("raised")) {
            notes.push(json!({"menu_pick": index, "value": text}));
            return Ok(format!("unreachable: doAction {text}"));
        }
        // A frame must pump for the callback's record to be written.
        run.hold(1.0)?;
        let lines = link::parse_lines(
            &run.sh(&format!("cat {DIR}/players.jsonl 2>/dev/null || true"), &[])?,
        );
        if lines
            .iter()
            .any(|l| l["ev"] == "menu" && l["entity"].as_u64() == Some(command))
        {
            notes.push(json!({"menu_pick": "selected", "index": index}));
            return Ok(format!("index={index}"));
        }
    }
    notes.push(json!({"menu_pick": "none", "tried": MAX_INDEX}));
    Ok(format!(
        "unreachable: no action index from 1 to {MAX_INDEX} selected the item"
    ))
}

/// What a phase handler gets: the run, the phase name, its line, the notes.
pub(super) type OnPhase<'a> =
    dyn FnMut(&Run, &str, &Value, &mut Vec<Value>) -> Result<(), String> + 'a;

/// Poll the probe's lines in `<DIR>/<scenario>.jsonl` until its verdict. Each
/// new `phase` line goes to `on_phase`. With `hold`, the simulation is held
/// running between polls, except while the probe loads the mission.
pub(super) fn drive(
    run: &Run,
    scenario: &str,
    hold: bool,
    notes: &mut Vec<Value>,
    on_phase: &mut OnPhase,
) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(PROBE_S + 120);
    let mut printed = 0usize;
    let mut phases = 0usize;
    loop {
        let lines = link::parse_lines(&run.sh(
            &format!("cat {DIR}/{scenario}.jsonl 2>/dev/null || true"),
            &[],
        )?);
        for l in lines
            .iter()
            .skip(printed)
            .filter(|l| matches!(l["ev"].as_str(), Some("phase" | "step" | "player" | "menu")))
        {
            println!("   {l}");
        }
        printed = lines.len();
        if let Some(v) = lines.iter().find(|l| l["ev"] == "verdict") {
            return Ok(v.clone());
        }
        if let Some(e) = lines.iter().find(|l| l["ev"] == "error") {
            return Err(format!("command-probe: {e}"));
        }
        let new: Vec<&Value> = lines
            .iter()
            .filter(|l| l["ev"] == "phase")
            .skip(phases)
            .collect();
        phases += new.len();
        for line in new {
            let name = line["name"].as_str().unwrap_or("");
            if let Err(e) = on_phase(run, name, line, notes) {
                abort(run);
                return Err(e);
            }
        }
        let alive = run.sh(
            "pgrep -f '[c]ommand-probe --part players' >/dev/null && echo yes || true",
            &[],
        )?;
        if !String::from_utf8_lossy(&alive).contains("yes") {
            let last = link::parse_lines(&run.sh(
                &format!("cat {DIR}/{scenario}.jsonl 2>/dev/null || true"),
                &[],
            )?);
            if let Some(v) = last.iter().find(|l| l["ev"] == "verdict") {
                return Ok(v.clone());
            }
            let err = run.sh(
                &format!("tail -c 2000 {DIR}/{scenario}.err 2>/dev/null || true"),
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
        if hold && !loading(&lines) {
            run.hold(HOLD_S)?;
        } else {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Tell the probe to stop (it removes what it added and unbans anyone it
/// banned), and wait for it to end.
fn abort(run: &Run) {
    let _ = answer_file(run, "abort", "");
    let by = Instant::now() + Duration::from_secs(45);
    while Instant::now() < by {
        let alive = run
            .sh(
                "pgrep -f '[c]ommand-probe --part players' >/dev/null && echo yes || true",
                &[],
            )
            .map(|o| String::from_utf8_lossy(&o).contains("yes"))
            .unwrap_or(false);
        if !alive {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The probe sent `MISSION_LOAD` and has not yet seen the mission again.
fn loading(lines: &[Value]) -> bool {
    let has = |name: &str| {
        lines
            .iter()
            .any(|l| l["ev"] == "phase" && l["name"] == name)
    };
    has("mission_load_sent") && !has("loaded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_raw_reads_the_lua_table() {
        assert!(!agent_raw("[transport]\nport = 1\n").unwrap());
        assert!(!agent_raw("[lua]\nraw = false\n").unwrap());
        assert!(agent_raw("[transport]\nport = 1\n[lua]\nraw = true\n").unwrap());
        assert!(agent_raw("this is not toml").is_err());
    }

    #[test]
    fn the_menu_snippet_gets_its_index() {
        let s = pick_source(7);
        assert!(s.contains("tonumber('7')") || s.contains("tonumber(7)"));
        assert!(!s.contains("<index>"));
        assert!(MENU_PICK.contains(PLACEHOLDER));
    }

    fn verdict(raw: bool, pass_steps: &[&str], skip: &[&str]) -> Value {
        let mut steps: Vec<Value> = pass_steps
            .iter()
            .map(|n| json!({"name": n, "verdict": "pass"}))
            .collect();
        steps.extend(skip.iter().map(|n| json!({"name": n, "verdict": "skip"})));
        json!({"pass": true, "raw_lua_seen": raw, "steps": steps})
    }

    #[test]
    fn the_exit_test_needs_every_required_step_in_the_mode() {
        for raw in [true, false] {
            let mut names = required(raw);
            let mut all = names.clone();
            all.push("menu_select");
            assert!(meets_exit_test(&verdict(raw, &all, &[]), raw));
            // The menu step may be skipped.
            assert!(meets_exit_test(
                &verdict(raw, &names, &["menu_select"]),
                raw
            ));
            // But not left out.
            assert!(!meets_exit_test(&verdict(raw, &names, &[]), raw));
            // Another step may not.
            let last = names.pop().unwrap();
            let mut fewer = names.clone();
            fewer.push("menu_select");
            assert!(!meets_exit_test(&verdict(raw, &fewer, &[]), raw), "{last}");
            // A probe that saw the other mode does not pass.
            assert!(!meets_exit_test(&verdict(!raw, &all, &[]), raw));
        }
        // A verdict that did not pass does not.
        let mut v = verdict(
            true,
            &{
                let mut a = required(true);
                a.push("menu_select");
                a
            },
            &[],
        );
        v["pass"] = json!(false);
        assert!(!meets_exit_test(&v, true));
    }

    #[test]
    fn the_raw_off_run_checks_the_gates_and_not_the_extension() {
        assert!(required(false).contains(&"extension_gate"));
        assert!(!required(false).contains(&"mission_load"));
        assert!(required(true).contains(&"mission_load"));
        assert!(!required(true).contains(&"extension_gate"));
    }

    #[test]
    fn the_mission_is_loading_between_the_two_phases() {
        let l = |name: &str| json!({"ev": "phase", "name": name});
        assert!(!loading(&[l("marks_set")]));
        assert!(loading(&[l("marks_set"), l("mission_load_sent")]));
        assert!(!loading(&[l("mission_load_sent"), l("loaded")]));
    }

    #[test]
    fn the_snippets_name_their_state() {
        assert!(MARKS_READ.contains("-- states: server"));
        assert!(MISSION_PATH.contains("-- states: hook"));
        assert!(MENU_PICK.contains("-- states: server"));
    }
}
