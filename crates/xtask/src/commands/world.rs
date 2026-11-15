//! `commands <name> --part world`: the S15 exit test on a docker-wine target.
//!
//! Deploys a build with the eval channel, restarts DCS and loads a fresh shell
//! mission, then runs `command-probe --part world` in the game container as
//! the sidecar, twice (the probe's own module documents both runs):
//!
//! - `groups`: `--groups` (100) groups of `--units` (4) units spawned and
//!   removed, with every over-budget frame attributed to a command. The
//!   simulation is held running while the probe works. The spawn origin is the
//!   first airbase's point, read from the loaded mission.
//! - `paused`: the probe spawns a group, this tool pauses the simulation through
//!   the eval channel, the probe sends one `GROUP_SPAWN` under the group's name
//!   with other units, this tool resumes and holds model time, the probe
//!   enumerates the world, and this tool reads the group in the `server`
//!   state: one group of that name across every coalition, holding as many units
//!   as it was spawned with, each of the new type. The replacement keeps the
//!   unit names, so the read counts groups and units and reads each unit's
//!   type, which names alone cannot tell apart. The agent's `pending_destroy`
//!   (after the settle) and `pending_expired` and `pending_overflow` (at most
//!   0 in the run), all from the `sampling` log the probe prints, must read 0. The probe then destroys the group.
//!
//! The agent's `commanded_ms` is read from `agent.toml` (2.0 when absent) and
//! given to the probe as the budget. `agent.toml` is not changed. The target is
//! left running the build it deployed, with the shell mission: the caller
//! restores the target. Everything goes under
//! `bundles/commands-world-<name>-<time>/`, with `world.json`, the verdict.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::install::{AGENT_TOML, Profile as _};
use crate::sampling::live::{self, Opened, Run};
use crate::{bundle, link};

/// Where the probe goes in the container.
const DIR: &str = "/tmp/nanoprobe-world";
/// The probe's own deadline for one scenario, seconds.
const PROBE_S: u64 = 900;
/// Model seconds a hold runs while the probe works.
const HOLD_S: f64 = 3.0;
/// Model seconds the probe waits after the resume before it enumerates.
const SETTLE_S: f64 = 5.0;
/// The unit type the replacement gives (the probe's `BTR-80`).
const NEW_TYPE: &str = "BTR-80";
/// Wall seconds to wait, after the settle, for a `sampling` log that shows the
/// agent's pending destroys drained. The agent reports every 10 s, so this is
/// two reports and a margin; a destroy still pending after it has not drained.
const DRAIN_WAIT_S: u64 = 35;
/// Campaigns of the two runs, so one run's names are not the other's.
const CAMPAIGNS: [u64; 2] = [7, 8];

/// The agent's `commanded_ms` in an `agent.toml` text, 2.0 when it has none.
pub(super) fn commanded_ms(toml_text: &str) -> Result<f64, String> {
    let table: toml::Table =
        toml::from_str(toml_text).map_err(|e| format!("agent.toml on the target: {e}"))?;
    Ok(table
        .get("budget")
        .and_then(|b| b.get("commanded_ms"))
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(2.0))
}

/// Whether the two verdicts and the read of DCS meet the exit test.
pub(super) fn meets_exit_test(groups: &Value, paused: &Value, dcs: &Value, want: u64) -> bool {
    let n = |v: &Value, k: &str| v[k].as_u64();
    groups["pass"] == json!(true)
        && n(groups, "spawned").is_some_and(|s| s >= want)
        && n(groups, "removed").is_some_and(|r| r >= want)
        && groups["unattributed_frames"] == json!("[]")
        && groups["counter_ok"] == json!(true)
        && paused["pass"] == json!(true)
        && dcs["ok"] == json!(true)
}

/// Lua for the server state: walks `coalition.getGroups` for every side and
/// returns `groups|units|types`: the number of groups named `group`, the
/// number of units in all groups whose name is one of `names`, and
/// `name=type` for each unit of the groups named `group`.
fn read_source(group: &str, names: &[String]) -> String {
    let names: Vec<String> = names.iter().map(|n| format!("['{n}']=true")).collect();
    format!(
        "local want = {{{}}}\n\
         local groups, units, types = 0, 0, {{}}\n\
         for _, side in pairs(coalition.side) do\n\
           for _, g in ipairs(coalition.getGroups(side) or {{}}) do\n\
             local named = g:getName() == '{group}'\n\
             if named then groups = groups + 1 end\n\
             for _, u in ipairs(g:getUnits() or {{}}) do\n\
               local n = u:getName()\n\
               if want[n] then units = units + 1 end\n\
               if named then types[#types + 1] = n .. '=' .. u:getTypeName() end\n\
             end\n\
           end\n\
         end\n\
         table.sort(types)\n\
         return string.format('%d|%d|%s', groups, units, table.concat(types, ','))\n",
        names.join(",")
    )
}

/// What a read of DCS shows, and whether it is the replacement: one group of
/// the name, `want` units across all groups under the group's unit names,
/// and every unit of the new type. Returns `(ok, groups, units, types)`.
pub(super) fn judge_read(read: &str, want: usize) -> (bool, u64, u64, Vec<String>) {
    let mut parts = read.splitn(3, '|');
    let groups = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let units = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let types: Vec<String> = parts
        .next()
        .unwrap_or("")
        .split(',')
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    let new = types
        .iter()
        .all(|t| t.rsplit_once('=').is_some_and(|(_, ty)| ty == NEW_TYPE));
    let ok = want > 0 && groups == 1 && units == want as u64 && types.len() == want && new;
    (ok, groups, units, types)
}

/// Lua for the hook state: pause, and return once DCS says paused.
const PAUSE_SOURCE: &str = "local E = ...\n\
    DCS.setPause(true)\n\
    return E.wait(function() if DCS.getPause() then return 'paused' end end, 60)\n";

/// The first airbase's point, `x z`, in the server state.
const ORIGIN_SOURCE: &str = "local p = world.getAirbases()[1]:getPoint()\n\
    return string.format('%.1f %.1f', p.x, p.z)\n";

pub fn run(repository: &Path, name: &str, groups: u64, units: u64) -> Result<ExitCode, String> {
    let Opened { run, shell, .. } = live::open(repository, name, "commands-world")?;
    let started = bundle::utc_now();
    let result = attempt(&run, name, &shell, groups, units);
    let (groups_v, paused_v, dcs_v, error) = match &result {
        Ok((g, p, d)) => (g.clone(), p.clone(), d.clone(), Value::Null),
        Err(e) => (Value::Null, Value::Null, Value::Null, json!(e)),
    };
    let ok = result
        .as_ref()
        .is_ok_and(|(g, p, d)| meets_exit_test(g, p, d, groups));
    let out = json!({
        "target": name,
        "started": started,
        "ended": bundle::utc_now(),
        "groups": groups,
        "units": units,
        "groups_verdict": groups_v,
        "paused_verdict": paused_v,
        "dcs_read": dcs_v,
        "error": error,
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("world.json");
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

type Verdicts = (Value, Value, Value);

/// What the paused run does on each poll of the probe's lines.
type OnLines<'a> = dyn FnMut(&Run, &[Value]) -> Result<(), String> + 'a;

fn attempt(
    run: &Run,
    name: &str,
    shell: &[u8],
    groups: u64,
    units: u64,
) -> Result<Verdicts, String> {
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "command-probe")])?
        .pop()
        .ok_or("command-probe was not built")?;
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let agent_toml = run
        .remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let budget = commanded_ms(&String::from_utf8_lossy(&agent_toml))?;
    println!("== restart");
    crate::restart(name)?;
    println!("== load a fresh shell mission");
    crate::live::load(&run.remote, "np-shell.miz", shell)?;
    run.sh(
        &format!("pkill -f '[c]ommand-probe --part' || true; rm -rf {DIR}"),
        &[],
    )?;
    link::upload_container(&run.remote, DIR, &probe, "command-probe")?;
    run.sh(&format!("mkdir -p {DIR}/control"), &[])?;
    let result = scenarios(run, budget, groups, units);
    let _ = run.sh("pkill -f '[c]ommand-probe --part' || true", &[]);
    for file in ["groups.jsonl", "groups.err", "paused.jsonl", "paused.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    result
}

fn scenarios(run: &Run, budget: f64, groups: u64, units: u64) -> Result<Verdicts, String> {
    let origin = run.eval("server", "s15-origin", ORIGIN_SOURCE, 60)?;
    let origin = origin.as_str().ok_or("the origin is not a string")?;
    let (x, z) = origin
        .split_once(' ')
        .ok_or_else(|| format!("the origin is {origin}"))?;
    println!("== groups: {groups} groups of {units}, origin {x} {z}, budget {budget} ms");
    let g = start(
        run,
        "groups",
        CAMPAIGNS[0],
        &format!("--groups {groups} --units {units} --origin-x {x} --origin-z {z} --commanded-ms {budget}"),
    )
    .and_then(|()| drive(run, "groups", &mut |_, _| Ok(())))?;
    println!("== paused replace");
    start(
        run,
        "paused",
        CAMPAIGNS[1],
        &format!(
            "--units {units} --origin-x {x} --origin-z {z} --settle {SETTLE_S} \
             --commanded-ms {budget} --control-dir {DIR}/control"
        ),
    )?;
    let mut stage = Stage::default();
    let p = drive(run, "paused", &mut |run, lines| stage.advance(run, lines))?;
    let dcs = stage
        .dcs
        .ok_or("the paused run never read DCS (no settled phase)")?;
    Ok((g, p, dcs))
}

/// The paused run's steps on this side, in order.
#[derive(Default)]
struct Stage {
    step: u8,
    /// The read of DCS, until the agent's counters are in.
    read: Option<Value>,
    /// The line count at the settle: only reports after it show the drain.
    settled_at: usize,
    /// When to stop waiting for the drain.
    drain_by: Option<Instant>,
    dcs: Option<Value>,
}

/// The agent's `sampling` reports among the probe's lines, with their
/// position: `(index, pending_destroy, pending_expired, pending_overflow)`.
fn reports(lines: &[Value]) -> Vec<(usize, u64, u64, u64)> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l["ev"] == "log" && l["msg"] == "sampling")
        .map(|(i, l)| {
            let n = |k: &str| l["data"][k].as_u64().unwrap_or(u64::MAX);
            (
                i,
                n("pending_destroy"),
                n("pending_expired"),
                n("pending_overflow"),
            )
        })
        .collect()
}

impl Stage {
    fn phase<'a>(lines: &'a [Value], name: &str) -> Option<&'a Value> {
        lines
            .iter()
            .find(|l| l["ev"] == "phase" && l["name"] == name)
    }

    fn strings(v: &Value) -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn advance(&mut self, run: &Run, lines: &[Value]) -> Result<(), String> {
        match self.step {
            0 if Self::phase(lines, "spawned").is_some() => {
                println!("== pause");
                run.eval("hook", "s15-pause", PAUSE_SOURCE, 90)?;
                run.sh(&format!("touch {DIR}/control/paused"), &[])?;
                self.step = 1;
            }
            1 if Self::phase(lines, "replaced").is_some() => {
                let r = Self::phase(lines, "replaced").expect("checked");
                println!("   replaced while paused: {r}");
                println!("== resume");
                run.sh(&format!("touch {DIR}/control/resumed"), &[])?;
                run.hold(SETTLE_S + 3.0)?;
                self.step = 2;
            }
            2 if Self::phase(lines, "settled").is_some() => {
                let spawned = Self::phase(lines, "spawned").expect("seen");
                let replaced = Self::phase(lines, "replaced").expect("seen");
                let group = spawned["group"].as_str().ok_or("no group name")?;
                let mut new = Self::strings(&replaced["new_units"]);
                new.sort();
                // The replacement keeps the unit names, so names cannot tell
                // old from new or one group from two. Count the groups of the
                // name and the units of those names across every coalition,
                // and read each unit's type.
                let read = run.eval("server", "s15-read", &read_source(group, &new), 60)?;
                let read = read.as_str().ok_or("the read is not a string")?.to_owned();
                println!("   DCS reads: {read}");
                self.read = Some(json!({ "group": group, "read": read, "want_units": new.len() }));
                self.settled_at = lines.len();
                self.drain_by = Some(Instant::now() + Duration::from_secs(DRAIN_WAIT_S));
                self.step = 3;
            }
            // The agent's pending destroys drain within a report or two. Wait
            // for a report after the settle that shows none pending.
            3 => {
                let all = reports(lines);
                let after: Vec<_> = all.iter().filter(|r| r.0 >= self.settled_at).collect();
                let drained = after.last().is_some_and(|r| r.1 == 0);
                let late = self.drain_by.is_some_and(|t| Instant::now() > t);
                if drained || late {
                    let mut v = self.read.take().expect("read");
                    let want = v["want_units"].as_u64().unwrap_or(0) as usize;
                    let (read_ok, groups, units, types) =
                        judge_read(v["read"].as_str().unwrap_or(""), want);
                    let destroy = after.last().map_or(u64::MAX, |r| r.1);
                    // An expired pending destroy anywhere in the run fails it.
                    let expired = all.iter().map(|r| r.2).max().unwrap_or(u64::MAX);
                    // The same for a destroy the agent could not hold.
                    let overflow = all.iter().map(|r| r.3).max().unwrap_or(u64::MAX);
                    v["ok"] = json!(read_ok && destroy == 0 && expired == 0 && overflow == 0);
                    v["groups_named"] = json!(groups);
                    v["units_under_names"] = json!(units);
                    v["unit_types"] = json!(types);
                    v["new_type"] = json!(NEW_TYPE);
                    // u64::MAX: no report came.
                    v["pending_destroy"] = json!(destroy);
                    v["pending_expired"] = json!(expired);
                    v["pending_overflow"] = json!(overflow);
                    println!(
                        "   agent: pending_destroy {destroy}, pending_expired {expired}, \
                         pending_overflow {overflow}"
                    );
                    self.dcs = Some(v);
                    run.sh(&format!("touch {DIR}/control/checked"), &[])?;
                    self.step = 4;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Start the probe for a scenario in the container.
fn start(run: &Run, scenario: &str, campaign: u64, extra: &str) -> Result<(), String> {
    let [map, port, token] = run.link_args();
    run.sh(
        &format!(
            "nohup {DIR}/command-probe --part world --scenario {scenario} --campaign {campaign} \
             --map \"$1\" --port \"$2\" --token-file \"$3\" --seconds {PROBE_S} --trace {extra} \
             > {DIR}/{scenario}.jsonl 2> {DIR}/{scenario}.err < /dev/null &"
        ),
        &[map, port, token],
    )?;
    Ok(())
}

/// Poll the probe's lines until its verdict. `on_lines` runs on each poll, and
/// may pause and resume the simulation; the simulation is held running between
/// polls except while the paused run has it paused.
fn drive(run: &Run, scenario: &str, on_lines: &mut OnLines) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(PROBE_S + 120);
    let mut printed = 0usize;
    loop {
        let lines = link::parse_lines(&run.sh(
            &format!("cat {DIR}/{scenario}.jsonl 2>/dev/null || true"),
            &[],
        )?);
        for l in lines.iter().skip(printed).filter(|l| {
            matches!(
                l["ev"].as_str(),
                Some("phase" | "progress" | "over_budget_frame")
            )
        }) {
            println!("   {l}");
        }
        printed = lines.len();
        if let Some(v) = lines.iter().find(|l| l["ev"] == "verdict") {
            return Ok(v.clone());
        }
        if let Some(e) = lines.iter().find(|l| l["ev"] == "error") {
            return Err(format!("command-probe: {e}"));
        }
        on_lines(run, &lines)?;
        let alive = run.sh(
            "pgrep -f '[c]ommand-probe --part' >/dev/null && echo yes || true",
            &[],
        )?;
        if !String::from_utf8_lossy(&alive).contains("yes") {
            let err = run.sh(
                &format!("tail -c 2000 {DIR}/{scenario}.err 2>/dev/null || true"),
                &[],
            )?;
            // The verdict may have been written just before it exited.
            let last = link::parse_lines(&run.sh(
                &format!("cat {DIR}/{scenario}.jsonl 2>/dev/null || true"),
                &[],
            )?);
            if let Some(v) = last.iter().find(|l| l["ev"] == "verdict") {
                return Ok(v.clone());
            }
            return Err(format!(
                "command-probe ended with no verdict: {}",
                String::from_utf8_lossy(&err).trim()
            ));
        }
        if Instant::now() > deadline {
            return Err("command-probe gave no verdict in time".into());
        }
        // While the paused run holds the simulation paused, no hold.
        if scenario == "paused" && paused_now(&lines) {
            std::thread::sleep(Duration::from_millis(500));
        } else {
            run.hold(HOLD_S)?;
        }
    }
}

/// The simulation must stay paused: the paused run's `spawned` phase has come
/// and its `replaced` phase has not.
fn paused_now(lines: &[Value]) -> bool {
    Stage::phase(lines, "spawned").is_some() && Stage::phase(lines, "replaced").is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_comes_from_agent_toml() {
        assert_eq!(commanded_ms("[transport]\nport = 1\n").unwrap(), 2.0);
        assert_eq!(commanded_ms("[budget]\ncommanded_ms = 0.5\n").unwrap(), 0.5);
        assert_eq!(commanded_ms("[budget]\ncommanded_ms = 3\n").unwrap(), 3.0);
    }

    #[test]
    fn the_exit_test_needs_every_part() {
        let g = json!({"pass": true, "spawned": 100, "removed": 100,
            "unattributed_frames": "[]", "counter_ok": true});
        let p = json!({"pass": true});
        let d = json!({"ok": true});
        assert!(meets_exit_test(&g, &p, &d, 100));
        assert!(!meets_exit_test(&g, &p, &d, 101));
        assert!(!meets_exit_test(&g, &json!({"pass": false}), &d, 100));
        assert!(!meets_exit_test(&g, &p, &json!({"ok": false}), 100));
        let mut late = g.clone();
        late["unattributed_frames"] = json!("[7]");
        assert!(!meets_exit_test(&late, &p, &d, 100));
    }

    #[test]
    fn the_read_walks_every_side_for_the_group_and_its_unit_names() {
        let s = read_source("np.g.x.1", &["np.u.x.2".into(), "np.u.x.3".into()]);
        assert!(s.contains("coalition.getGroups(side)"));
        assert!(s.contains("g:getName() == 'np.g.x.1'"));
        assert!(s.contains("['np.u.x.2']=true,['np.u.x.3']=true"));
        assert!(s.contains("u:getTypeName()"));
    }

    #[test]
    fn the_replacement_is_one_group_of_the_new_type() {
        let two = "1|2|a=BTR-80,b=BTR-80";
        assert!(judge_read(two, 2).0);
        // The old type left, one unit of the old type among the new.
        assert!(!judge_read("1|2|a=BTR-80,b=M-113", 2).0);
        // The old group and the new one both exist.
        assert!(!judge_read("2|4|a=BTR-80,b=BTR-80,a=M-113,b=M-113", 2).0);
        // Names exist in another group.
        assert!(!judge_read("1|3|a=BTR-80,b=BTR-80", 2).0);
        assert!(!judge_read("0|0|", 2).0);
        assert!(!judge_read("1|2|a=BTR-80", 2).0);
        // Nothing wanted is not a replacement.
        assert!(!judge_read("1|0|", 0).0);
    }

    #[test]
    fn the_drain_reads_the_reports_after_the_settle() {
        let l = |d: u64, e: u64| {
            json!({"ev": "log", "msg": "sampling",
                   "data": {"pending_destroy": d, "pending_expired": e,
                            "pending_overflow": 0}})
        };
        let lines = [l(1, 0), json!({"ev": "phase"}), l(0, 0)];
        assert_eq!(reports(&lines), vec![(0, 1, 0, 0), (2, 0, 0, 0)]);
        // A report without the field reads as the maximum, which fails.
        let bare = json!({"ev": "log", "msg": "sampling", "data": {"pending_destroy": 0}});
        assert_eq!(reports(&[bare])[0].3, u64::MAX);
    }
}
