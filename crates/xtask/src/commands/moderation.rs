//! `commands <server> --part moderation --client <windows-target>`: the
//! E34 and E35 driver. A scripted client joins the server, takes a slot and
//! spawns, and `command-probe --part players --scenario moderation` in the
//! server's game container moderates it. Test tooling: built from source,
//! never released.
//!
//! The server needs a mission with player slots, running. By default it is
//! the mission the server starts with (the tool restarts DCS to load the
//! build and loads nothing); `--miz <file>` loads that mission through the
//! development loader instead. The client is a windows-ssh target, running a
//! `--dev` build at its main menu, and the server's profile names
//! `client_address` and `client_password_file` (`client join`, which this
//! tool runs for each join, documents both).
//!
//! The probe moderates and reports what the server and the link show; this
//! tool does what only the client can: join, take a slot, spawn, read the
//! client's own view with `lua/experiments/alone/s16-observe.lua`, and leave.
//! In order, each step printing a verdict:
//!
//! 1. the client joins (`connect`); it takes a slot and spawns;
//! 2. `MODERATE SLOT` to spectators: a `SLOT` record shows it and the
//!    client's own slot changed (`client_slot_after_move`);
//! 3. E35: the client takes a slot and asks to spawn in one step
//!    (`lua/experiments/alone/s16-slot-race.lua`), and on its `SLOT` record the
//!    probe sends `MODERATE SLOT` to spectators; the probe prints the
//!    time from the record to the command being queued, written, answered and
//!    to the next `SLOT` record, and whether a `BIRTH` came;
//! 4. `KICK` with a reason: the client's own disconnect and the reason text
//!    it saw, which must carry the kick reason and a code
//!    (`client_kick_disconnect`); it joins again;
//! 5. `BAN`: the same (`client_ban_disconnect`), a join that is refused, with
//!    the ban code 101 seen by the client (`client_rejoin_refused`);
//!    `BANS` lists the client; `UNBAN`; `BANS` no longer lists it; a join
//!    that works; the client leaves.
//!
//! No ucid is printed or written: the probe names a player by a hash prefix.
//! The first failure ends the run: the probe unbans a client it banned, and
//! this tool makes the client leave. A ban the run cannot lift ends by itself
//! after 180 s. The tool restarts DCS on the server, so the target is left
//! running the build it deployed. Everything goes under
//! `bundles/commands-moderation-<name>-<time>/`, with `moderation.json`.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use super::players::{DIR, answer_file, drive, passed};
use crate::install::{AGENT_TOML, Profile as _, RUNNING};
use crate::profile::Target;
use crate::sampling::live::{self, Opened, Run};
use crate::windows::Windows;
use crate::{bundle, eval, link};

/// Eval timeout for a client snippet, seconds.
const CLIENT_S: u64 = 120;

const TAKE_SLOT: &str = include_str!("../../../../lua/experiments/alone/e30-take-slot.lua");
const SPAWN: &str = include_str!("../../../../lua/experiments/alone/e31-spawn.lua");
const LEAVE: &str = include_str!("../../../../lua/experiments/alone/e31-leave.lua");
const OBSERVE: &str = include_str!("../../../../lua/experiments/alone/s16-observe.lua");
const SLOT_RACE: &str = include_str!("../../../../lua/experiments/alone/s16-slot-race.lua");
/// The placeholder in [`OBSERVE`].
const WAIT: &str = "'<wait>'";

/// The steps a run must pass.
const REQUIRED: [&str; 19] = [
    "link",
    "players_query",
    "connect",
    "players_query_client",
    "slot_record",
    "slot_to_spectators",
    "e35_slot_race",
    "kick",
    "reconnect_after_kick",
    "ban",
    "ban_refused",
    "bans_lists_client",
    "unban",
    "bans_forgot_client",
    "rejoin_after_unban",
    "client_slot_after_move",
    "client_kick_disconnect",
    "client_ban_disconnect",
    "client_rejoin_refused",
];

/// The observer snippet that arms and returns at once.
fn observe_source() -> String {
    OBSERVE.replacen(WAIT, "0", 1)
}

/// Whether a `client join` ended in a join.
fn joined(code: &ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

/// The client, over SSH, and what the run learned of it.
struct Client {
    w: Windows,
    name: String,
    server: String,
    /// The client-side verdicts, as the probe's steps are.
    steps: Vec<Value>,
}

impl Client {
    /// Run a snippet in the client's hook state; its reply.
    fn eval(&self, stem: &str, source: &str) -> Result<Value, String> {
        let r = eval::run(
            &self.w,
            "hook",
            stem,
            source.as_bytes(),
            Duration::from_secs(CLIENT_S),
        )?;
        if r["ok"] != json!(true) || r["reply"]["ok"] != json!(true) {
            return Err(format!("{stem} on {}: {r}", self.name));
        }
        Ok(r["reply"].clone())
    }

    /// One observation of a reply.
    fn observation(reply: &Value, name: &str) -> Value {
        reply["observations"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|o| o["name"] == name)
            .map_or(Value::Null, |o| o["value"].clone())
    }

    fn join(&self) -> Result<bool, String> {
        let code = crate::client::command(&["join", &self.name, &self.server])?;
        Ok(joined(&code))
    }

    fn arm(&self) -> Result<(), String> {
        let r = self.eval("s16-observe", &observe_source())?;
        let v = &r["values"][0];
        if v != &json!("armed") {
            return Err(format!("the observer answered {v}, not armed"));
        }
        Ok(())
    }

    /// What the armed observer saw; it disarms.
    fn report(&self) -> Result<Value, String> {
        let r = self.eval("s16-observe", &observe_source())?;
        let report = Self::observation(&r, "report");
        if report.is_null() {
            return Err(format!("the observer gave no report: {r}"));
        }
        Ok(report)
    }

    fn step(&mut self, name: &str, verdict: &str, detail: &str) {
        println!("   client {name}: {verdict}: {detail}");
        self.steps
            .push(json!({"name": name, "verdict": verdict, "detail": detail}));
    }
}

/// Whether a client report shows the slot changed.
fn slot_changed(report: &Value) -> Option<String> {
    let changes = report["slot_changes"].as_array().map_or(0, Vec::len);
    let samples = report["samples"].as_array().cloned().unwrap_or_default();
    let first = samples.first().map(|s| s["slot"].clone());
    let last = samples.last().map(|s| s["slot"].clone());
    if changes > 0 {
        Some(format!("{changes} slot change(s) reported"))
    } else if first.is_some() && first != last {
        Some(format!("the slot read {} then {}", first?, last?))
    } else {
        None
    }
}

/// The ban code a client's `onNetDisconnect` gets (`ERR_BANNED`, LIVE).
const BAN_CODE: &str = "101";

/// The first disconnect of a client report with a reason that carries
/// `marker` and a code: its description, or why it is not one.
fn disconnect_with_reason(report: &Value, marker: &str) -> Result<String, String> {
    let Some(d) = report["disconnects"].as_array().and_then(|a| a.first()) else {
        return Err(format!("no disconnect callback: {report}"));
    };
    let reason = d["reason"].as_str().unwrap_or("");
    let code = d["code"].as_str().unwrap_or("");
    if !reason.contains(marker) {
        return Err(format!("the reason {reason:?} lacks {marker:?}"));
    }
    if code.is_empty() || code == "nil" {
        return Err(format!("the disconnect has no code: {d}"));
    }
    Ok(format!("reason {reason:?}, code {code}"))
}

/// Whether a client report holds a disconnect with the ban code.
fn saw_ban_code(report: &Value) -> bool {
    report["disconnects"]
        .as_array()
        .is_some_and(|a| a.iter().any(|d| d["code"].as_str() == Some(BAN_CODE)))
}

/// A step from a verdict that is `Ok` for a pass and `Err` for a failure.
fn verdict_step(c: &mut Client, name: &str, v: Result<String, String>) {
    match v {
        Ok(d) => c.step(name, "pass", &d),
        Err(d) => c.step(name, "fail", &d),
    }
}

pub fn run(
    repository: &Path,
    name: &str,
    client: &str,
    miz: Option<&Path>,
) -> Result<ExitCode, String> {
    let w = match crate::target(client)? {
        crate::profile::Target::WindowsSsh { ssh, profile, .. } => {
            Windows::connect(&ssh, &profile)?
        }
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
    let Opened { run, .. } = live::open(repository, name, "commands-moderation")?;
    let started = bundle::utc_now();
    let mut c = Client {
        w,
        name: client.to_owned(),
        server: name.to_owned(),
        steps: Vec::new(),
    };
    let mut notes = Vec::new();
    let result = attempt(&run, name, mission.as_deref(), &mut c, &mut notes);
    // The client leaves whatever happened.
    let _ = c.eval("s16-leave", LEAVE);
    let (verdict, error) = match &result {
        Ok(v) => (v.clone(), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    let ok = result.as_ref().is_ok_and(|v| meets_exit_test(v, &c.steps));
    let out = json!({
        "target": name,
        "client": client,
        "started": started,
        "ended": bundle::utc_now(),
        "controller": notes,
        "client_steps": c.steps,
        "verdict": verdict,
        "error": error,
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("moderation.json");
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

/// Whether the probe's verdict and the client's steps meet the test.
fn meets_exit_test(verdict: &Value, client_steps: &[Value]) -> bool {
    let mut done = passed(verdict);
    done.extend(
        client_steps
            .iter()
            .filter(|s| s["verdict"] == json!("pass"))
            .filter_map(|s| s["name"].as_str().map(str::to_owned)),
    );
    verdict["pass"] == json!(true)
        && client_steps.iter().all(|s| s["verdict"] != json!("fail"))
        && REQUIRED.iter().all(|n| done.iter().any(|d| d == n))
}

fn attempt(
    run: &Run,
    name: &str,
    miz: Option<&[u8]>,
    c: &mut Client,
    notes: &mut Vec<Value>,
) -> Result<Value, String> {
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "command-probe")])?
        .pop()
        .ok_or("command-probe was not built")?;
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let _ = run
        .remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    println!("== restart");
    crate::restart(name)?;
    if let Some(bytes) = miz {
        println!("== load the mission");
        crate::live::load(&run.remote, "np-moderation.miz", bytes)?;
    }
    run.sh(
        &format!("pkill -f '[c]ommand-probe --part players' || true; rm -rf {DIR}"),
        &[],
    )?;
    link::upload_container(&run.remote, DIR, &probe, "command-probe")?;
    run.sh(&format!("mkdir -p {DIR}/control"), &[])?;
    let [map, port, token] = run.link_args();
    println!("== command-probe --part players --scenario moderation");
    run.sh(
        &format!(
            "nohup {DIR}/command-probe --part players --scenario moderation \
             --map \"$1\" --port \"$2\" --token-file \"$3\" --control-dir {DIR}/control \
             --seconds 1500 > {DIR}/moderation.jsonl 2> {DIR}/moderation.err < /dev/null &"
        ),
        &[map, port, token],
    )?;
    let result = drive(
        run,
        "moderation",
        false,
        notes,
        &mut |run, phase, _, notes| {
            notes.push(json!({"phase": phase}));
            on_phase(run, phase, c)
        },
    );
    let _ = run.sh("pkill -f '[c]ommand-probe --part players' || true", &[]);
    for file in ["moderation.jsonl", "moderation.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    result
}

/// What the client does at each phase of the probe.
fn on_phase(run: &Run, phase: &str, c: &mut Client) -> Result<(), String> {
    match phase {
        "ready" => {
            if !c.join()? {
                return Err("the client did not join".into());
            }
            c.step("client_joined", "pass", "client join succeeded");
            answer_file(run, "joined", "ok")
        }
        "connected" => {
            c.eval("s16-take-slot", TAKE_SLOT)?;
            answer_file(run, "slot_taken", "ok")?;
            c.eval("s16-spawn", SPAWN)?;
            answer_file(run, "spawned", "ok")
        }
        "slot_move_armed" => {
            c.arm()?;
            answer_file(run, "slot_move_ready", "ok")
        }
        "slot_moved" => {
            let report = c.report()?;
            match slot_changed(&report) {
                Some(d) => c.step("client_slot_after_move", "pass", &d),
                None => c.step(
                    "client_slot_after_move",
                    "fail",
                    &format!("no slot change seen: {report}"),
                ),
            }
            answer_file(run, "slot_checked", "ok")
        }
        "e35_armed" => {
            c.arm()?;
            // A refused call is the experiment's finding, not the run's
            // failure: the snippet records every return.
            match c.eval("s16-slot-race", SLOT_RACE) {
                Ok(r) => c.step(
                    "client_e35",
                    "note",
                    &format!(
                        "set_slot {}, spawn_player {}, then slot {} and unit {}",
                        Client::observation(&r, "set_slot"),
                        Client::observation(&r, "spawn_player"),
                        Client::observation(&r, "get_slot"),
                        Client::observation(&r, "player_unit"),
                    ),
                ),
                Err(e) => c.step("client_e35", "note", &format!("the snippet ended: {e}")),
            }
            if let Ok(report) = c.report() {
                c.step("client_e35_observed", "note", &report.to_string());
            }
            answer_file(run, "e35_checked", "ok")
        }
        "kick_armed" => {
            c.arm()?;
            answer_file(run, "kick_ready", "ok")
        }
        "kicked" => {
            // Give DCS a moment to run the client's callbacks.
            std::thread::sleep(Duration::from_secs(5));
            let report = c.report()?;
            verdict_step(
                c,
                "client_kick_disconnect",
                disconnect_with_reason(&report, "np-s16-kick"),
            );
            if !c.join()? {
                return Err("the client did not rejoin after the kick".into());
            }
            answer_file(run, "rejoined", "ok")
        }
        "ban_armed" => {
            c.arm()?;
            answer_file(run, "ban_ready", "ok")
        }
        "banned" => {
            std::thread::sleep(Duration::from_secs(5));
            let report = c.report()?;
            verdict_step(
                c,
                "client_ban_disconnect",
                disconnect_with_reason(&report, "np-s16-ban"),
            );
            // Arm again, so the refusal of the rejoin is observed.
            c.arm()?;
            let again = c.join()?;
            if again {
                c.step("client_rejoin_refused", "fail", "the banned client joined");
            } else {
                std::thread::sleep(Duration::from_secs(5));
                match c.report() {
                    Ok(report) if saw_ban_code(&report) => c.step(
                        "client_rejoin_refused",
                        "pass",
                        &format!("the join failed and the client saw code {BAN_CODE}"),
                    ),
                    Ok(report) => c.step(
                        "client_rejoin_refused",
                        "fail",
                        &format!("the join failed and the client saw no ban code: {report}"),
                    ),
                    Err(e) => c.step("client_rejoin_refused", "fail", &e),
                }
            }
            answer_file(
                run,
                "rejoin_tried",
                if again { "joined" } else { "refused" },
            )
        }
        "unbanned" => {
            if !c.join()? {
                return Err("the client did not join after UNBAN".into());
            }
            answer_file(run, "rejoined_after_unban", "ok")
        }
        "finishing" => {
            c.eval("s16-leave", LEAVE)?;
            answer_file(run, "left", "ok")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_observer_runs_armed() {
        let s = observe_source();
        assert!(s.contains("tonumber(0)"));
        assert!(!s.contains("'<wait>'"));
        assert!(OBSERVE.contains(WAIT));
    }

    #[test]
    fn an_exit_code_is_a_join_only_when_it_is_success() {
        assert!(joined(&ExitCode::SUCCESS));
        assert!(!joined(&ExitCode::FAILURE));
    }

    #[test]
    fn a_slot_change_is_read_from_changes_or_samples() {
        let by_callback = json!({"slot_changes": [{"slot": ""}], "samples": []});
        assert!(slot_changed(&by_callback).is_some());
        let by_sample = json!({"slot_changes": [], "samples": [{"slot": "8580"}, {"slot": ""}]});
        assert!(slot_changed(&by_sample).is_some());
        let none = json!({"slot_changes": [], "samples": [{"slot": "8580"}, {"slot": "8580"}]});
        assert!(slot_changed(&none).is_none());
    }

    #[test]
    fn a_kick_or_ban_needs_the_reason_and_a_code() {
        let ok = json!({"disconnects": [{"reason": "kicked: np-s16-kick", "code": "5"}]});
        assert!(disconnect_with_reason(&ok, "np-s16-kick").is_ok());
        let other = json!({"disconnects": [{"reason": "x", "code": "5"}]});
        assert!(disconnect_with_reason(&other, "np-s16-kick").is_err());
        let no_code = json!({"disconnects": [{"reason": "np-s16-kick", "code": "nil"}]});
        assert!(disconnect_with_reason(&no_code, "np-s16-kick").is_err());
        let sampled = json!({"disconnects": [], "samples": [{"multiplayer": false}]});
        assert!(disconnect_with_reason(&sampled, "np-s16-kick").is_err());
    }

    #[test]
    fn a_refused_rejoin_needs_the_ban_code() {
        assert!(saw_ban_code(&json!({"disconnects": [{"code": "101"}]})));
        assert!(!saw_ban_code(&json!({"disconnects": [{"code": "5"}]})));
        assert!(!saw_ban_code(&json!({"disconnects": []})));
    }

    #[test]
    fn the_exit_test_needs_the_probe_and_the_client() {
        let names: Vec<&str> = REQUIRED
            .iter()
            .copied()
            .filter(|n| !n.starts_with("client_"))
            .collect();
        let probe = json!({
            "pass": true,
            "steps": names.iter().map(|n| json!({"name": n, "verdict": "pass"})).collect::<Vec<_>>()
        });
        let client: Vec<Value> = REQUIRED
            .iter()
            .filter(|n| n.starts_with("client_"))
            .map(|n| json!({"name": n, "verdict": "pass"}))
            .collect();
        assert!(meets_exit_test(&probe, &client));
        assert!(!meets_exit_test(&probe, &client[1..]));
        let mut failed = client.clone();
        failed.push(json!({"name": "client_x", "verdict": "fail"}));
        assert!(!meets_exit_test(&probe, &failed));
        let mut bad = probe.clone();
        bad["pass"] = json!(false);
        assert!(!meets_exit_test(&bad, &client));
    }

    #[test]
    fn the_snippets_name_their_state() {
        for s in [TAKE_SLOT, SPAWN, LEAVE, OBSERVE, SLOT_RACE] {
            assert!(s.contains("-- states: hook"));
        }
    }
}
