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
//!
//! `commands <name> --part names`: the naming check (spec/world.md, "Names").
//! It deploys and loads a fresh shell mission as above, then runs
//! `command-probe --part world --scenario names` once, for campaign
//! [`NAMES_CAMPAIGN`]. When the probe reports `spawned`, this tool reads the
//! names in the `server` state: the group by name with its units' names, and
//! the static by name. The check passes when the probe's verdict passes and
//! DCS returns every name exactly. Everything goes under
//! `bundles/commands-names-<name>-<time>/`, with `names.json`; the caller
//! restores the target.
//!
//! `commands <name> --part debris`: the `CLEAR_DEBRIS` check. It deploys and
//! loads a fresh shell mission as above, or one made from `--template`, then
//! runs `command-probe --part world --scenario debris` (the probe's module
//! documents it) round the first airbase, or with `--min-ground` the first
//! whose ground is at least that many meters above sea level. When the probe
//! reports `spawned`, this tool kills the grid's units with a scripted
//! explosion 6 m beside each, holds model time [`COOL_S`] so the wrecks stop
//! burning, reads DCS, and writes the ground height at the first unit into
//! the probe's `go` file. The kill must find every unit, and it keeps their
//! handles. When the probe reports `column`, only the column has cleared, and
//! this tool reads DCS again ([`judge_column`]): the column's wrecks are gone,
//! the outside group's are left, and the control units and the managed static
//! stand. After the probe's verdict this tool reads DCS: every killed unit's
//! reads raise (its wreck was removed, LIVE run 1) and every control unit
//! exists alive. The run passes when the probe's verdict and both reads pass.
//! Everything goes under
//! `bundles/commands-debris-<name>-<time>/`, with `debris.json`; the caller
//! restores the target.

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
/// Model seconds for killed ground units' wrecks to stop burning: their
/// death events came up to 103 s after the kill (LIVE,
/// docs/results/2026-11-16-clear-debris.md).
const COOL_S: f64 = 120.0;
/// The naming check's campaign: a creation millisecond of 2025-11-13, tag `jqW`.
const NAMES_CAMPAIGN: u64 = 1_763_000_000_000;
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

/// Lua for the server state: `group` by name, its name and its units' names,
/// and `static` by name and its name, as `group|unit,unit|static`. The units
/// come in DCS's order; the caller sorts them by bytes.
fn names_source(group: &str, stat: &str) -> String {
    format!(
        "local g = Group.getByName('{group}')\n\
         local units = {{}}\n\
         if g then for _, u in ipairs(g:getUnits() or {{}}) do units[#units + 1] = u:getName() end end\n\
         local s = StaticObject.getByName('{stat}')\n\
         return (g and g:getName() or '') .. '|' .. table.concat(units, ',') .. '|' .. (s and s:getName() or '')\n"
    )
}

/// The naming check (module docs).
pub fn run_names(repository: &Path, name: &str, units: u64) -> Result<ExitCode, String> {
    let Opened { run, shell, .. } = live::open(repository, name, "commands-names")?;
    let started = bundle::utc_now();
    let result = attempt_names(&run, name, &shell, units);
    let (verdict, read, error) = match &result {
        Ok((v, r)) => (v.clone(), r.clone(), Value::Null),
        Err(e) => (Value::Null, Value::Null, json!(e)),
    };
    let ok = verdict["pass"] == json!(true) && read["ok"] == json!(true);
    let out = json!({
        "target": name,
        "started": started,
        "ended": bundle::utc_now(),
        "campaign": NAMES_CAMPAIGN,
        "units": units,
        "verdict": verdict,
        "dcs_read": read,
        "error": error,
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("names.json");
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

fn attempt_names(
    run: &Run,
    name: &str,
    shell: &[u8],
    units: u64,
) -> Result<(Value, Value), String> {
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
    let result = (|| {
        let origin = run.eval("server", "names-origin", ORIGIN_SOURCE, 60)?;
        let origin = origin.as_str().ok_or("the origin is not a string")?;
        let (x, z) = origin
            .split_once(' ')
            .ok_or_else(|| format!("the origin is {origin}"))?;
        println!("== names: campaign {NAMES_CAMPAIGN}, {units} units and a static, origin {x} {z}");
        start(
            run,
            "names",
            NAMES_CAMPAIGN,
            &format!(
                "--units {units} --origin-x {x} --origin-z {z} --commanded-ms {budget} \
                 --control-dir {DIR}/control"
            ),
        )?;
        let mut read = Value::Null;
        let v = drive(run, "names", &mut |run, lines| {
            if read.is_null()
                && let Some(p) = Stage::phase(lines, "spawned")
            {
                let group = p["group"].as_str().ok_or("no group name")?;
                let stat = p["static"].as_str().ok_or("no static name")?;
                let mut want: Vec<String> = Stage::strings(&p["objects"])
                    .iter()
                    .filter_map(|o| o.split('|').next().map(str::to_owned))
                    .filter(|n| n != stat)
                    .collect();
                want.sort();
                let got = run.eval("server", "names-read", &names_source(group, stat), 60)?;
                let got = got.as_str().ok_or("the read is not a string")?;
                println!("   DCS reads: {got}");
                // Lua's string order follows the locale: sort here, by bytes.
                let got = match got.splitn(3, '|').collect::<Vec<_>>()[..] {
                    [g, u, s] => {
                        let mut u: Vec<&str> = u.split(',').filter(|n| !n.is_empty()).collect();
                        u.sort();
                        format!("{g}|{}|{s}", u.join(","))
                    }
                    _ => got.to_owned(),
                };
                let expect = format!("{group}|{}|{stat}", want.join(","));
                read = json!({ "read": got, "expected": expect, "ok": got == expect });
                run.sh(&format!("touch {DIR}/control/checked"), &[])?;
            }
            Ok(())
        })?;
        Ok((v, read))
    })();
    let _ = run.sh("pkill -f '[c]ommand-probe --part' || true", &[]);
    for file in ["names.jsonl", "names.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    result
}

/// The debris check's snippets for the server state; each file says what it
/// does. `lua/tests/world_exit.lua debris` runs them on the mock.
const COLUMN_ORIGIN: &str = include_str!("../../../../lua/experiments/junk/column-origin.lua");
const COLUMN_KILL: &str = include_str!("../../../../lua/experiments/junk/column-kill.lua");
const COLUMN_READ: &str = include_str!("../../../../lua/experiments/junk/column-read.lua");

/// Names as a Lua list's items: `'a','b'`.
fn quoted(names: &[String]) -> String {
    let names: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
    names.join(",")
}

/// Lua for the server state: the first airbase whose ground is at least
/// `min_ground` meters up, or the first airbase with no minimum.
fn origin_source(min_ground: Option<f64>) -> String {
    let min = min_ground.map_or("-math.huge".to_owned(), |m| format!("{m}"));
    COLUMN_ORIGIN.replace("__MIN_GROUND__", &min)
}

/// What the origin read gives: `(x, z, ground, airbase)`.
pub(super) fn parse_origin(read: &str) -> Result<(String, String, f64, String), String> {
    let mut parts = read.splitn(4, ' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(x), Some(z), Some(ground), Some(airbase))
            if x.parse::<f64>().is_ok() && z.parse::<f64>().is_ok() =>
        {
            let ground = ground
                .parse()
                .map_err(|_| format!("the origin is {read}"))?;
            Ok((x.into(), z.into(), ground, airbase.into()))
        }
        (Some("none"), Some(highest), None, None) => Err(format!(
            "no airbase stands on ground that high; the highest is at {highest} m"
        )),
        _ => Err(format!("the origin is {read}")),
    }
}

/// Lua for the server state: kill each named unit, keep its handle, and
/// return `hit y`: the units it found and the height of the first, `%.1f`.
fn kill_source(names: &[String]) -> String {
    COLUMN_KILL.replace("__NAMES__", &quoted(names))
}

/// Lua for the server state: `inside outside alive static all`, read before
/// and after the column clear.
fn column_read_source(
    inside: &[String],
    outside: &[String],
    control: &[String],
    managed: &str,
) -> String {
    COLUMN_READ
        .replace("__INSIDE__", &quoted(inside))
        .replace("__OUTSIDE__", &quoted(outside))
        .replace("__CONTROL__", &quoted(control))
        .replace("__STATIC__", &format!("'{managed}'"))
}

/// One read round the column: `(inside_left, outside_left, control_alive,
/// static, all_left)`. `all_left` counts every killed unit's wreck that is
/// left. It is recorded and not judged: it shows how far past its edge the
/// box reached, which no rule fixes.
pub(super) fn column_read(read: &str) -> Option<(u64, u64, u64, &str, u64)> {
    let mut parts = read.split(' ');
    let out = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(out)
}

/// Whether the column cleared its row and nothing else. Before it, every
/// wreck of the row and of the outside group answers its reads, or the read
/// after could not tell a removed wreck from one that was never there. After
/// it, none of the row's does, every one of the outside group's does, every
/// control unit is alive, and the managed static exists with life above 0.
/// The static's life is not compared with its life before: a fire from the
/// kills may still lower it, and a clear that destroyed it leaves none.
pub(super) fn judge_column(
    before: &str,
    after: &str,
    inside: usize,
    outside: usize,
    control: usize,
) -> bool {
    let (Some(b), Some(a)) = (column_read(before), column_read(after)) else {
        return false;
    };
    let standing = |s: &str| {
        let mut parts = s.split(':');
        parts.next() == Some("true")
            && parts
                .next()
                .and_then(|l| l.parse::<f64>().ok())
                .is_some_and(|l| l > 0.0)
    };
    (b.0, b.1) == (inside as u64, outside as u64)
        && (a.0, a.1, a.2) == (0, outside as u64, control as u64)
        && inside > 0
        && outside > 0
        && standing(b.3)
        && standing(a.3)
}

/// Lua for the server state, after the clears: `left alive`, the killed
/// units whose reads still answer (their wreck is in DCS; a removed one
/// raises, LIVE run 1), and the named control units that exist alive.
fn after_source(control: &[String]) -> String {
    let names: Vec<String> = control.iter().map(|n| format!("'{n}'")).collect();
    format!(
        "local E = ...\n\
         local left = 0\n\
         for _, u in ipairs(E.store.debris or {{}}) do\n\
           if pcall(Unit.getLife, u) then left = left + 1 end\n\
         end\n\
         local alive = 0\n\
         for _, n in ipairs({{{}}}) do\n\
           local u = Unit.getByName(n)\n\
           if u and u:isExist() and u:getLife() > 0 then alive = alive + 1 end\n\
         end\n\
         E.store.debris = nil\n\
         return string.format('%d %d', left, alive)\n",
        names.join(",")
    )
}

/// Whether the read after the clears passes: no wreck left, every control
/// unit alive. Returns `(ok, wrecks_left, control_alive)`.
pub(super) fn judge_after(read: &str, control: usize) -> (bool, u64, u64) {
    let mut parts = read.split(' ').map(|v| v.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(l), Some(a)) => (l == 0 && a == control as u64, l, a),
        _ => (false, u64::MAX, 0),
    }
}

pub fn run_debris(
    repository: &Path,
    name: &str,
    groups: u64,
    units: u64,
    template: Option<&Path>,
    min_ground: Option<f64>,
) -> Result<ExitCode, String> {
    let Opened { run, shell, .. } = live::open(repository, name, "commands-debris")?;
    // Another map's template in place of the default shell.
    let shell = match template {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            crate::miz::generate(&bytes, &crate::miz::Changes::default())?
        }
        None => shell,
    };
    let started = bundle::utc_now();
    let result = attempt_debris(&run, name, &shell, groups, units, min_ground);
    let ok = result.as_ref().is_ok_and(|v| {
        v["pass"] == json!(true)
            && v["dcs_after"]["ok"] == json!(true)
            && v["column_check"]["ok"] == json!(true)
    });
    let out = json!({
        "target": name,
        "started": started,
        "ended": bundle::utc_now(),
        "groups": groups,
        "units": units,
        "template": template.map(|p| p.display().to_string()),
        "min_ground": min_ground,
        "verdict": result.as_ref().ok(),
        "error": result.as_ref().err(),
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("debris.json");
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

fn attempt_debris(
    run: &Run,
    name: &str,
    shell: &[u8],
    groups: u64,
    units: u64,
    min_ground: Option<f64>,
) -> Result<Value, String> {
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
    let result = (|| {
        let origin = run.eval("server", "debris-origin", &origin_source(min_ground), 60)?;
        let (x, z, ground, airbase) =
            parse_origin(origin.as_str().ok_or("the origin is not a string")?)?;
        println!(
            "== debris: {groups} groups of {units}, origin {x} {z} at {airbase}, ground \
             {ground} m, budget {budget} ms"
        );
        start(
            run,
            "debris",
            CAMPAIGNS[0],
            &format!(
                "--groups {groups} --units {units} --origin-x {x} --origin-z {z} \
                 --commanded-ms {budget} --control-dir {DIR}/control"
            ),
        )?;
        let mut killed = false;
        let mut control = Vec::new();
        // The read round the column: its source, the read before the clear,
        // and the check once the column has answered.
        let mut read_source = String::new();
        let (mut inside, mut outside) = (0usize, 0usize);
        let mut before = String::new();
        let mut column_check = Value::Null;
        let mut verdict = drive(run, "debris", &mut |run, lines| {
            if !killed {
                let Some(p) = Stage::phase(lines, "spawned") else {
                    return Ok(());
                };
                let names = Stage::strings(&p["kill"]);
                control = Stage::strings(&p["control"]);
                let (row, far) = (Stage::strings(&p["column"]), Stage::strings(&p["outside"]));
                let managed = p["static"].as_str().ok_or("the probe named no static")?;
                (inside, outside) = (row.len(), far.len());
                read_source = column_read_source(&row, &far, &control, managed);
                let read = run.eval("server", "debris-kill", &kill_source(&names), 60)?;
                let (hit, y) = read
                    .as_str()
                    .and_then(|t| t.split_once(' '))
                    .and_then(|(h, y)| Some((h.parse::<usize>().ok()?, y.parse::<f64>().ok()?)))
                    .ok_or_else(|| format!("the kill read is {read}"))?;
                if hit != names.len() {
                    return Err(format!("the kill found {hit} of {} units", names.len()));
                }
                println!("== killed {hit} units; {COOL_S} s for the wrecks to burn out");
                run.hold(COOL_S)?;
                let read = run.eval("server", "debris-column-before", &read_source, 60)?;
                before = read.as_str().unwrap_or("").to_owned();
                println!("   DCS before the column: {before}");
                run.sh(&format!("printf '%s' '{y:.1}' > {DIR}/control/go"), &[])?;
                killed = true;
                return Ok(());
            }
            if column_check.is_null()
                && let Some(p) = Stage::phase(lines, "column")
            {
                // Only the column has cleared: its row's wrecks are gone and
                // the far group's are left.
                let read = run.eval("server", "debris-column-after", &read_source, 60)?;
                let after = read.as_str().unwrap_or("");
                println!("   DCS after the column: {after}");
                let ok = judge_column(&before, after, inside, outside, control.len());
                column_check = json!({
                    "ok": ok,
                    "before": before,
                    "after": after,
                    "inside": inside,
                    "outside": outside,
                    "control": control.len(),
                    "wrecks_left_before": column_read(&before).map(|r| r.4),
                    "wrecks_left_after": column_read(after).map(|r| r.4),
                    "removed": p["removed"],
                    "cost_us": p["cost_us"],
                    "half_width": p["half_width"],
                });
                run.sh(&format!("touch {DIR}/control/column-checked"), &[])?;
            }
            Ok(())
        })?;
        verdict["origin"] = json!({ "x": x, "z": z, "ground": ground, "airbase": airbase });
        verdict["column_check"] = column_check;
        // DCS after the clears: no wreck left, every control unit alive.
        let read = run.eval("server", "debris-after", &after_source(&control), 60)?;
        let (ok, left, alive) = judge_after(read.as_str().unwrap_or(""), control.len());
        println!("   DCS after the clears: {left} wrecks left, {alive} control units alive");
        verdict["dcs_after"] = json!({
            "ok": ok,
            "wrecks_left": left,
            "control_alive": alive,
            "control": control.len(),
        });
        Ok(verdict)
    })();
    let _ = run.sh("pkill -f '[c]ommand-probe --part' || true", &[]);
    for file in ["debris.jsonl", "debris.err"] {
        if let Ok(bytes) = run.sh(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[]) {
            let _ = std::fs::write(run.folder.join(file), bytes);
        }
    }
    let _ = run.sh(&format!("rm -rf {DIR}"), &[]);
    result
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
    fn the_read_after_the_clears_is_judged() {
        // The column: its row's 4 wrecks gone, the outside group's 4 left,
        // 4 control units alive, the static standing.
        let (b, a) = ("4 4 4 true:3600.0:912.5 36", "0 4 4 true:3600.0:912.5 32");
        assert!(judge_column(b, a, 4, 4, 4));
        // The static's life may fall, and the count of all wrecks left is
        // not judged.
        assert!(judge_column(b, "0 4 4 true:3100.0:912.5 30", 4, 4, 4));
        assert!(!judge_column(b, "1 4 4 true:3600.0:912.5 33", 4, 4, 4));
        assert!(!judge_column(b, "0 3 4 true:3600.0:912.5 31", 4, 4, 4));
        assert!(!judge_column(b, "0 4 3 true:3600.0:912.5 32", 4, 4, 4));
        assert!(!judge_column(b, "0 4 4 gone 32", 4, 4, 4));
        assert!(!judge_column(b, "0 4 4 false:0.0:912.5 32", 4, 4, 4));
        assert!(!judge_column("3 4 4 true:3600.0:912.5 35", a, 4, 4, 4));
        assert!(!judge_column("", a, 4, 4, 4));
        assert!(!judge_column(b, "", 4, 4, 4));
        assert!(!judge_column(b, "0 4 4 true:3600.0:912.5", 4, 4, 4));
        assert_eq!(column_read(a).map(|r| r.4), Some(32));
        assert_eq!(
            parse_origin("-100.5 2000.0 964.2 Herat Airfield"),
            Ok((
                "-100.5".into(),
                "2000.0".into(),
                964.2,
                "Herat Airfield".into()
            ))
        );
        assert!(parse_origin("none 480.0").is_err());
        assert!(parse_origin("x").is_err());
        assert!(origin_source(None).contains("local MIN = -math.huge"));
        assert!(origin_source(Some(500.0)).contains("local MIN = 500"));
        assert!(
            !column_read_source(&["a".into()], &["b".into()], &["c".into()], "s").contains("__")
        );
        assert!(kill_source(&["a".into()]).contains("ipairs({ 'a' })"));
        assert_eq!(judge_after("0 4", 4), (true, 0, 4));
        assert_eq!(judge_after("1 4", 4), (false, 1, 4));
        assert_eq!(judge_after("0 3", 4), (false, 0, 3));
        assert!(!judge_after("", 4).0);
        assert!(!judge_after("x 4", 4).0);
    }

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
        let s = read_source("np!xyz1", &["np.xyz2".into(), "np.xyz3".into()]);
        assert!(s.contains("coalition.getGroups(side)"));
        assert!(s.contains("g:getName() == 'np!xyz1'"));
        assert!(s.contains("['np.xyz2']=true,['np.xyz3']=true"));
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
