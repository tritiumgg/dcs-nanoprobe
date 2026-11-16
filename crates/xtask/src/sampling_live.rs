//! `sampling <name>`: the S10 exit test on a docker-wine target.
//!
//! Each part deploys a build with the eval channel, restarts DCS, loads a
//! fresh shell mission, and attaches `link-probe --mirror` in the game
//! container as the sidecar, recording the stream. It spawns the moving,
//! mixed scenario (`lua/scenarios/scenario.lua`, `spawn_mixed`) and compares
//! the mirror with the oracle (`lua/sampling/oracle.lua`) with
//! [`super::compare_files`].
//!
//! - `mirror`, on the release build: 500 objects with every ground group
//!   moving, compared; cleared; 1,200 objects with one ground group in five
//!   moving and explosions, compared; 800 more, compared at 2,000.
//! - `tenth`, on the build with fault injection: 2,000 objects (one ground
//!   group in five moving) with 20 units subscribed, compared at k = 1; then
//!   `inject('sampling', 0.1)`, more spawns and explosions, and compared at
//!   k = 0.1.
//!
//! Before every comparison it measures DCS's own frame rate with
//! `lua/sampling/fps.lua` (30 s to settle, 60 s measured, the simulation
//! running), which also gives the enumeration time to find statics
//! that send no birth. It checks the conditions
//!   of the S10 ruling, as the ruling on live row costs (2026-11-15) left
//!   them: at k = 0.1 the subscribed positions read rows and keep at least
//!   the positions' ratio, while the order below the positions, the
//!   subscribed rows' 90 % keep, the fill time and an enumeration's completion are reported. At
//!   both scales, since they bound the agent's time and the frame cap and
//!   not a tier's rate, these stay judged: the share stays under
//!   `overhead_share`, no debt is forgiven, debt stays under a second's
//!   budget, sampling runs past `k·B` by more than one clock interval in at
//!   most 1 % of the frames that only swept rows, and the frames over 250 µs
//!   are at most 1 % of the frames (the agent's exact count, `tail`; the
//!   histogram's 256 µs bucket edge is reported, not judged). At k = 1 it
//!   judges the vitals period as a rate against its 4.5-6.5 s band and the
//!   pump bound, pumps a second at most 5 + `pump_work_us` a second ÷ (3 ×
//!   `xcall_seen_us`) in every report, here and in the mirror part.
//!
//! Everything goes under `bundles/sampling-<name>-<time>/`: every mirror and
//! oracle dump, the verdicts, the probe's output and the recording, and
//! `sampling.json`, the run's verdict. A recording holds server data and is
//! never committed. Before each disruptive step the run waits for the server
//! to have no player. It leaves the target running the build it deployed,
//! with the scenario's objects in the shell mission: the caller restores the
//! target.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Part;
use super::positions;
use crate::install::Profile as _;
use crate::profile::Target;
use crate::remote::Remote;
use crate::{bundle, eval, link, live, miz};

/// Where the probe goes in the container.
pub(crate) const DIR: &str = "/tmp/nanoprobe-sampling";
/// How long the probe may take to attach.
const ATTACH_S: u64 = 90;
/// Units the tenth part subscribes.
const SUBSCRIBED: usize = 20;
/// The share of ground groups that move at 1,200 and 2,000 objects: with
/// every group moving, 2,000 objects held DCS under 2 fps (LIVE, S10), so
/// by the S10 ruling one in five moves and the rest stay alive and halted.
pub(crate) const MOVING: f64 = 0.2;
/// The share in the shipping `agent.toml`.
pub(crate) const SHARE: f64 = 0.003;
/// Model seconds between the oracle's early read and the compare: a slow
/// position period and one more (6 s at the targets), and a second.
pub(crate) const EARLY_S: f64 = 7.0;
/// Bytes of the recording's tail a compare reads.
pub(crate) const TAIL_BYTES: u64 = 12_000_000;

pub(crate) struct Run {
    pub(crate) remote: Remote,
    pub(crate) folder: PathBuf,
    map: String,
    port: String,
    token: String,
    pub(crate) repository: PathBuf,
}

impl Run {
    pub(crate) fn sh(&self, script: &str, args: &[&str]) -> Result<Vec<u8>, String> {
        let o = self.remote.shell(script, args, b"")?;
        if o.code != Some(0) {
            return Err(format!("`{script}` failed: {}", o.stderr));
        }
        Ok(o.stdout)
    }

    /// Run Lua in a state through the eval channel; its first value.
    pub(crate) fn eval(
        &self,
        state: &str,
        stem: &str,
        source: &str,
        timeout_s: u64,
    ) -> Result<Value, String> {
        let r = eval::run(
            &self.remote,
            state,
            stem,
            source.as_bytes(),
            Duration::from_secs(timeout_s),
        )?;
        if r["ok"] != json!(true) || r["reply"]["ok"] != json!(true) {
            return Err(format!("{stem}: {r}"));
        }
        Ok(r["reply"]["values"][0].clone())
    }

    /// Run model time for `seconds`, resuming the simulation if paused.
    pub(crate) fn hold(&self, seconds: f64) -> Result<(), String> {
        self.eval(
            "hook",
            "s10-hold",
            &hold_source(seconds),
            hold_timeout(seconds),
        )?;
        Ok(())
    }

    /// Spawn the mixed scenario. Returns its summary.
    pub(crate) fn spawn(&self, n: usize, label: &str, moving: f64) -> Result<Value, String> {
        let summary = self.eval(
            "server",
            &format!("s10-spawn-{label}"),
            &spawn_source(n, label, moving),
            300,
        )?;
        println!("  spawned {label}: {summary}");
        Ok(summary)
    }

    /// Destroy every object the scenario library added.
    pub(crate) fn clear(&self) -> Result<Value, String> {
        self.eval(
            "server",
            "s10-clear",
            "local E = ...\nreturn E.lib('scenario').clear()\n",
            60,
        )
    }

    /// DCS's own frame rate (`lua/sampling/fps.lua`): settle 30 s, then 60 s
    /// of one-second windows, the simulation running. Returns the mean,
    /// the p5 and model/real.
    pub(crate) fn fps(&self, label: &str) -> Result<Value, String> {
        self.fps_window(label, 30, 60)
    }

    /// [`Run::fps`] with the settle and measure seconds given. The record
    /// also carries `moving`: the share of the scenario's ground groups and
    /// ships that moved more than 20 m over the whole settle and measure
    /// span (`lua/scenarios/movers.lua`), which says what the rate was
    /// measured against. It is left out when the count fails.
    pub(crate) fn fps_window(
        &self,
        label: &str,
        settle: u32,
        measure: u32,
    ) -> Result<Value, String> {
        let source = fps_source(&self.repository, settle, measure)?;
        let baseline = crate::meter::movers(self);
        let mut v = self.eval("hook", "s10-fps", &source, 300)?;
        if baseline.is_some()
            && let Some(moving) = crate::meter::movers(self)
        {
            v["moving"] = moving;
        }
        println!("  fps {label}: {v}");
        Ok(v)
    }

    /// Explosions at `count` units of the groups a spawn summary names.
    pub(crate) fn churn(&self, summary: &Value, count: usize) -> Result<Value, String> {
        let groups: Vec<String> = summary["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| g.as_str().map(|s| format!("{s:?}")))
            .collect();
        let source = format!(
            "local E = ...\n\
             local S = E.lib('scenario')\n\
             return S.churn({{ {} }}, {count})\n",
            groups.join(", ")
        );
        self.eval("server", "s10-churn", &source, 60)
    }

    /// The objects that exist in DCS now, counted in the server state: the
    /// oracle's own count, independent of the agent.
    pub(crate) fn live_count(&self) -> Result<u64, String> {
        let v = self.eval("server", "s10-count", LIVE_COUNT_SOURCE, 60)?;
        v.as_u64()
            .ok_or_else(|| format!("the live count is not a number: {v}"))
    }

    /// Wait until the mirror holds as many rows as DCS holds objects after a
    /// spawn of `births` objects, from line `mark` on (S10 ruling 3c). The
    /// deadline is twice the births' expected time: each costs
    /// `cost_births` (LIVE, about 37 µs), and births get half the sampling
    /// time, itself about half the agent's budget, times the scale `k`. At
    /// `k` under 1 new objects may enter only through the enumeration, so the
    /// expected time is at least the last reported enumeration period.
    /// Returns whether it filled in time and the seconds it took.
    pub(crate) fn fill(&self, mark: usize, births: usize, k: f64) -> Result<(bool, f64), String> {
        let last = self.reports(mark)?.last().cloned().unwrap_or(Value::Null);
        let limit = fill_limit(&last, births, k);
        let start = Instant::now();
        loop {
            let live = self.live_count()?;
            let mirror = self.mirror()?;
            let rows = serde_json::from_slice::<Value>(&mirror)
                .ok()
                .and_then(|m| m["rows"].as_array().map(Vec::len))
                .unwrap_or(0) as u64;
            let took = start.elapsed().as_secs_f64();
            if rows == live {
                println!("  filled: {rows} rows in {took:.0} s (deadline {limit:.0} s)");
                return Ok((took <= limit, took));
            }
            if took > limit {
                println!("  not filled after {took:.0} s: {rows} rows against {live} objects");
                return Ok((false, took));
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    /// Ask the probe for a dump of the mirror.
    pub(crate) fn mirror(&self) -> Result<Vec<u8>, String> {
        self.sh(
            &format!("rm -f {DIR}/mirror.json; touch {DIR}/mirror.req"),
            &[],
        )?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let o = self
                .remote
                .shell(&format!("cat {DIR}/mirror.json 2>/dev/null"), &[], b"")?;
            if o.code == Some(0) && !o.stdout.is_empty() {
                return Ok(o.stdout);
            }
            if Instant::now() > deadline {
                return Err("the probe wrote no mirror".into());
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// Read the oracle early, wait one slow position period and two more
    /// sweeps, ask the probe for the mirror, read the oracle, take the
    /// recording's tail, and compare. Writes the dumps and the verdict under
    /// `label`. At 500 objects every sample is held to its target
    /// (`strict`); at a tenth of the slice ages are reported, not failed on.
    fn compare(&self, label: &str) -> Result<(Value, Value), String> {
        let read = |file: &str| {
            std::fs::read_to_string(self.repository.join(file)).map_err(|e| format!("{file}: {e}"))
        };
        self.eval(
            "server",
            "s10-early",
            &read("lua/sampling/oracle-early.lua")?,
            120,
        )?;
        self.hold(EARLY_S)?;
        let mirror = self.mirror().map_err(|e| format!("{label}: {e}"))?;
        let oracle = self.eval(
            "server",
            "s10-oracle",
            &read("lua/sampling/oracle.lua")?,
            120,
        )?;
        let m = self.folder.join(format!("mirror-{label}.json"));
        let o = self.folder.join(format!("oracle-{label}.json"));
        std::fs::write(&m, &mirror).map_err(|e| format!("{}: {e}", m.display()))?;
        std::fs::write(&o, oracle.to_string()).map_err(|e| format!("{}: {e}", o.display()))?;
        // The recording's last megabytes, from its first whole line: enough
        // for the intervals the compare reads.
        let tail = self.sh(&format!("tail -c {TAIL_BYTES} {DIR}/recording.jsonl"), &[])?;
        let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
        let r = self.folder.join(format!("recording-{label}.jsonl"));
        std::fs::write(&r, &tail[start..]).map_err(|e| format!("{}: {e}", r.display()))?;
        let verdict = super::compare_files(
            &o,
            &m,
            super::Judge {
                ages: label != "k01",
                strict: label == "500",
                recording: Some(&r),
                // Judged over stall-free windows (`positions_checks`).
                positions_by_window: label == "2000" || label == "k1",
            },
        )?;
        let v = verdict.json();
        std::fs::write(
            self.folder.join(format!("verdict-{label}.json")),
            v.to_string(),
        )
        .map_err(|e| e.to_string())?;
        println!("  {label}: {v}");
        let mirror: Value = serde_json::from_slice(&mirror).map_err(|e| e.to_string())?;
        Ok((v, mirror))
    }

    /// The link arguments a probe in the container takes: the map path, the
    /// handshake port and the token file.
    pub(crate) fn link_args(&self) -> [&str; 3] {
        [&self.map, &self.port, &self.token]
    }

    pub(crate) fn start_probe(&self, probe: &Path, subscribe: usize) -> Result<(), String> {
        self.sh(
            &format!("pkill -f '[l]ink-probe --map' || true; rm -rf {DIR}"),
            &[],
        )?;
        link::upload_container(&self.remote, DIR, probe, "link-probe")?;
        let sub = if subscribe > 0 {
            format!(" --subscribe {subscribe}")
        } else {
            String::new()
        };
        self.sh(
            &format!(
                "nohup {DIR}/link-probe --map \"$1\" --port \"$2\" --token-file \"$3\" --seconds 86400 \
                 --record {DIR}/recording.jsonl --stop-file {DIR}/stop --mirror {DIR}/mirror.json{sub} \
                 > {DIR}/probe.jsonl 2> {DIR}/probe.err < /dev/null &"
            ),
            &[&self.map, &self.port, &self.token],
        )?;
        let deadline = Instant::now() + Duration::from_secs(ATTACH_S);
        loop {
            if self.lines()?.iter().any(|l| l["ev"] == "hello") {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("the probe did not attach within {ATTACH_S} s"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    pub(crate) fn lines(&self) -> Result<Vec<Value>, String> {
        let o = self.remote.shell(
            &format!("cat {DIR}/probe.jsonl 2>/dev/null || true"),
            &[],
            b"",
        )?;
        Ok(link::parse_lines(&o.stdout))
    }

    /// Stop the probe through its stop file, and copy its files to
    /// `part`-named files in the folder.
    pub(crate) fn stop_probe(&self, part: &str) {
        let _ = self.remote.shell(&format!("touch {DIR}/stop"), &[], b"");
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self
                .lines()
                .map(|l| l.iter().any(|l| l["ev"] == "end"))
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let _ = self
            .remote
            .shell("pkill -f '[l]ink-probe --map' || true", &[], b"");
        for file in ["recording.jsonl", "probe.jsonl", "probe.err"] {
            if let Ok(o) =
                self.remote
                    .shell(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[], b"")
            {
                let _ = std::fs::write(self.folder.join(format!("{part}-{file}")), &o.stdout);
            }
        }
        let _ = self.remote.shell(&format!("rm -rf {DIR}"), &[], b"");
    }

    /// The agent's `sampling` reports the probe read, from line `from`.
    pub(crate) fn reports(&self, from: usize) -> Result<Vec<Value>, String> {
        Ok(self
            .lines()?
            .into_iter()
            .skip(from)
            .filter(|l| l["ev"] == "log" && l["msg"] == "sampling")
            .map(|l| l["data"].clone())
            .collect())
    }

    /// Deploy, restart DCS, and load a fresh shell mission, running.
    pub(crate) fn fresh(&self, name: &str, faults: bool, shell: &[u8]) -> Result<(), String> {
        empty(&self.remote, "deploy")?;
        println!("== deploy{}", if faults { " --faults" } else { "" });
        crate::deploy(name, None, true, faults)?;
        println!("== restart");
        crate::restart(name)?;
        println!("== load a fresh shell mission");
        live::load(&self.remote, "np-shell.miz", shell)?;
        Ok(())
    }
}

/// Lua for `Run::hold`: run model time for `seconds`.
pub(crate) fn hold_source(seconds: f64) -> String {
    format!(
        "local E = ...\n\
         local t0 = DCS.getModelTime()\n\
         return E.wait(function()\n  \
           if DCS.getModelTime() - t0 >= {seconds} then return 'done' end\n\
         end, {}, 'running')\n",
        seconds * 4.0 + 60.0
    )
}

/// The eval timeout for [`hold_source`], seconds.
pub(crate) fn hold_timeout(seconds: f64) -> u64 {
    (seconds * 4.0) as u64 + 120
}

/// Lua that spawns the mixed scenario.
pub(crate) fn spawn_source(n: usize, label: &str, moving: f64) -> String {
    format!(
        "local E = ...\n\
         local S = E.lib('scenario')\n\
         return S.spawn_mixed({n}, {{ label = '{label}', moving = {moving} }})\n"
    )
}

/// Lua that counts the objects that exist in DCS now, in the server state.
pub(crate) const LIVE_COUNT_SOURCE: &str = "local n = 0\n\
     for side = 0, 2 do\n  \
       for _, g in ipairs(coalition.getGroups(side) or {}) do\n    \
         for _, u in ipairs(g:getUnits() or {}) do\n      \
           if u:isExist() then n = n + 1 end\n    \
         end\n  \
       end\n  \
       for _, o in ipairs(coalition.getStaticObjects(side) or {}) do\n    \
         if o:isExist() then n = n + 1 end\n  \
       end\n\
     end\n\
     return n\n";

/// `lua/sampling/fps.lua` with the settle and measure seconds filled in.
pub(crate) fn fps_source(repository: &Path, settle: u32, measure: u32) -> Result<String, String> {
    Ok(
        std::fs::read_to_string(repository.join("lua/sampling/fps.lua"))
            .map_err(|e| format!("lua/sampling/fps.lua: {e}"))?
            .replace("@SETTLE@", &settle.to_string())
            .replace("@MEASURE@", &measure.to_string()),
    )
}

/// How long a mirror may take to fill after a spawn of `births` objects, at
/// scale `k`: twice the births' expected time (see [`Run::fill`]), and at
/// least 20 s. `last` is the last `sampling` report, or null.
pub(crate) fn fill_limit(last: &Value, births: usize, k: f64) -> f64 {
    let cost = last["cost_births"]
        .as_f64()
        .filter(|c| *c > 0.0)
        .unwrap_or(37.0);
    let mut expected = births as f64 * cost / (0.25 * SHARE * 1e6 * k);
    if k < 1.0 {
        expected = expected.max(last["enumeration_s"].as_f64().unwrap_or(0.0));
    }
    2.0 * expected.max(10.0)
}

/// Wait until no player is connected before `step`, up to 30 minutes.
fn empty(remote: &Remote, step: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let n = remote.players()?;
        if n == 0 {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(30 * 60) {
            return Err(format!(
                "{step}: {n} player(s) connected for 30 min; stopped"
            ));
        }
        println!("{step}: {n} player(s) connected; waiting");
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// The value at the edge of the histogram bucket that holds the quantile
/// `q`, µs: the bucket's upper edge.
fn quantile(hist: &[Value], q: f64) -> Option<f64> {
    let counts: Vec<u64> = hist.iter().map(|v| v.as_u64().unwrap_or(0)).collect();
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let want = (total as f64 * q).ceil() as u64;
    let mut seen = 0;
    for (i, c) in counts.iter().enumerate() {
        seen += c;
        if seen >= want {
            return Some(2f64.powi(i as i32));
        }
    }
    None
}

/// The probe's last `final` line's pump histogram quantiles.
/// The agent's time over real time across a span of `sampling` reports,
/// fixed before the run by a line mark (S10 ruling: total agent time over
/// total real time, not a mean of the windows). Passes when the mean is
/// within `overhead_share` and no debt was forgiven in the span. Time the
/// births' and the enumeration's balance forgave is reported beside it with
/// its windows, not judged: it marks a DCS stall inside one of their steps.
pub(crate) fn span(reports: &[Value]) -> (bool, Value) {
    let (mut agent, mut real, mut forgiven, mut events) = (0.0, 0.0, 0.0, 0.0);
    let mut windows = Vec::new();
    let (mut stalls, mut stall_us) = (Vec::new(), 0.0);
    for (i, r) in reports.iter().enumerate() {
        let lost = r["maintenance_forgiven_us"].as_f64().unwrap_or(0.0);
        if lost > 0.0 {
            stall_us += lost;
            stalls.push(json!({"window": i, "maintenance_forgiven_us": lost}));
        }
        let share = r["share"].as_f64().unwrap_or(0.0);
        let w = r["window_s"].as_f64().unwrap_or(10.0);
        agent += share * w;
        real += w;
        forgiven += r["forgiven_us"].as_f64().unwrap_or(0.0);
        events += r["events_us"].as_f64().unwrap_or(0.0);
        windows.push(json!({"share": share, "window_s": w, "events_us": r["events_us"]}));
    }
    // D at the span's edges: debt above 0, credit below.
    let d = |r: Option<&Value>| {
        r.map_or(0.0, |r| {
            r["debt_us"].as_f64().unwrap_or(0.0) - r["credit_us"].as_f64().unwrap_or(0.0)
        })
    };
    let mean = if real > 0.0 {
        agent / real
    } else {
        f64::INFINITY
    };
    let max = windows
        .iter()
        .filter_map(|w| w["share"].as_f64())
        .fold(0.0f64, f64::max);
    let ok = !reports.is_empty() && mean <= SHARE && forgiven == 0.0;
    (
        ok,
        json!({
            "mean": mean, "max_window": max, "real_s": real, "agent_us": agent * 1e6,
            "events_us": events, "forgiven_us": forgiven,
            "maintenance_forgiven_us": stall_us, "maintenance_forgiven_windows": stalls,
            "debt_start_us": d(reports.first()), "debt_end_us": d(reports.last()),
            "windows": windows,
        }),
    )
}

/// The worst position period the reports show (S10 ruling 3c: at most 3 s
/// while a spawn fills the table).
pub(crate) fn worst_position(reports: &[Value]) -> f64 {
    reports
        .iter()
        .filter_map(|r| r["position_s"].as_f64())
        .fold(0.0f64, f64::max)
}

pub(crate) fn pump(lines: &[Value]) -> Value {
    let Some(last) = lines.iter().rfind(|l| l["ev"] == "final") else {
        return Value::Null;
    };
    let h = last["hist_pump"].as_array().cloned().unwrap_or_default();
    let s = last["hist_syscall"].as_array().cloned().unwrap_or_default();
    // Every frame but the system-call frames: the pump histogram less the
    // system-call one, bucket by bucket.
    let rest: Vec<Value> = h
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!(
                v.as_u64()
                    .unwrap_or(0)
                    .saturating_sub(s.get(i).and_then(Value::as_u64).unwrap_or(0))
            )
        })
        .collect();
    json!({
        "p50_under_us": quantile(&h, 0.5),
        "p99_under_us": quantile(&h, 0.99),
        "p99_without_syscall_under_us": quantile(&rest, 0.99),
        "max_under_us": quantile(&h, 1.0),
        "syscall_p50_under_us": quantile(&s, 0.5),
        "syscall_p99_under_us": quantile(&s, 0.99),
        "hist_pump": last["hist_pump"],
        "hist_syscall": last["hist_syscall"],
    })
}

/// The frames past 250 µs across the reports, counted exactly by the agent
/// (`frames_over_250`): the p99 meets the cap when they are at most 1 % of
/// the frames. The histogram's 256 µs bucket edge cannot show it. A report
/// without `frames` or `frames_over_250` (an agent built before the count
/// existed) is counted in `missing`, and any such report fails the check.
pub(crate) fn tail(reports: &[Value]) -> Value {
    let sum = |k: &str| reports.iter().filter_map(|r| r[k].as_u64()).sum::<u64>();
    let (frames, over) = (sum("frames"), sum("frames_over_250"));
    let max = reports
        .iter()
        .filter_map(|r| r["frame_max_us"].as_f64())
        .fold(0.0f64, f64::max);
    let missing = reports
        .iter()
        .filter(|r| r["frames"].as_u64().is_none() || r["frames_over_250"].as_u64().is_none())
        .count();
    let share = if frames > 0 {
        over as f64 / frames as f64
    } else {
        0.0
    };
    json!({
        "frames": frames, "over_250": over, "syscall_over_250": sum("syscall_over_250"),
        "over_share": share, "missing": missing,
        "p99_within_250": frames > 0 && missing == 0 && share <= 0.01, "max_us": max,
    })
}

/// The frames over 250 µs at k = 1 and at a tenth, apart: the p99 is judged
/// over the k = 1 reports, and the tail at k = 0.1 is reported. A report
/// without a `scale` counts as k = 1.
pub(crate) fn tail_by_scale(reports: &[Value]) -> (Value, Value) {
    let k1: Vec<Value> = reports
        .iter()
        .filter(|r| r["scale"].as_f64().is_none_or(|k| k == 1.0))
        .cloned()
        .collect();
    let rest: Vec<Value> = reports
        .iter()
        .filter(|r| r["scale"].as_f64().is_some_and(|k| k != 1.0))
        .cloned()
        .collect();
    (tail(&k1), tail(&rest))
}

/// The `sampling` reports among the probe's lines from line `from`.
fn reports_in(lines: &[Value], from: usize) -> Vec<Value> {
    lines
        .iter()
        .skip(from)
        .filter(|l| l["ev"] == "log" && l["msg"] == "sampling")
        .map(|l| l["data"].clone())
        .collect()
}

/// The positions measure ([`positions`]) over `reports` from `settled` on,
/// with the intervals between samples of the objects moving in the oracle
/// dump `oracle-<label>.json`, from the recording `recording-<label>.jsonl`,
/// both in `folder`. Returns the checks and their detail.
fn positions_checks(
    folder: &Path,
    label: &str,
    reports: &[(f64, Value)],
    settled: usize,
    proxy: bool,
    suffix: &str,
) -> Result<(Vec<Value>, Value), String> {
    let read = |file: String| {
        std::fs::read_to_string(folder.join(&file)).map_err(|e| format!("{file}: {e}"))
    };
    let oracle: Value = serde_json::from_str(&read(format!("oracle-{label}.json"))?)
        .map_err(|e| format!("oracle-{label}.json: {e}"))?;
    let (_, truth, _) = super::read_oracle(&oracle)?;
    // The objects moving at the oracle read, as the compare takes them.
    let moving = truth
        .iter()
        .filter(|t| (t.kind == 1 || t.cargo) && t.speed > super::MOVING_M_S)
        .map(|t| t.id)
        .collect();
    let samples = positions::intervals(&read(format!("recording-{label}.jsonl"))?, &moving);
    Ok(positions::judge(reports, settled, &samples, proxy, suffix))
}

/// `sampling --rejudge <bundle dir>`: apply the positions measure to a stored
/// run, from `mirror-probe.jsonl` (the reports with their times),
/// `oracle-2000.json` and `recording-2000.jsonl`, and the tail split to
/// `tenth-probe.jsonl`. A report without `gap_max_s` has its stall windows
/// found by a proxy (fewer frames than seconds in the window, or a window
/// over 1 s past 10 s), and the result says it was judged by proxy: a stall
/// with many frames around it is missed.
pub(crate) fn rejudge(dir: &Path) -> Result<ExitCode, String> {
    let file = |name: &str| {
        std::fs::read(dir.join(name))
            .map(|b| link::parse_lines(&b))
            .map_err(|e| format!("{}: {e}", dir.join(name).display()))
    };
    let mut ok = true;
    let mut out = json!({ "bundle": dir.display().to_string() });
    if dir.join("mirror-probe.jsonl").exists() {
        let timed = positions::timed(&file("mirror-probe.jsonl")?, 0);
        let (checks, detail) = positions_checks(
            dir,
            "2000",
            &timed,
            positions::settled_from(&timed),
            true,
            " at 2,000, settled",
        )?;
        ok &= checks.iter().all(|c| c["ok"] == json!(true));
        println!("{}", json!({"mirror": {"checks": checks}}));
        out["mirror"] = detail;
    }
    if dir.join("tenth-probe.jsonl").exists() {
        let lines = file("tenth-probe.jsonl")?;
        let reports = reports_in(&lines, 0);
        let (k1, k01) = tail_by_scale(&reports);
        ok &= k1["p99_within_250"] == json!(true);
        println!(
            "{}",
            json!({"tenth": {"tail_k1_judged": k1, "tail_k01_reported": k01}})
        );
    }
    println!("== {}", if ok { "PASS" } else { "FAIL" });
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The birth and enumeration steps across the reports: the last p50 and
/// p99 bucket edges, the largest step, and whether one passed 250 µs, which
/// is a finding for S11 (the step is one DCS call the budget cannot split).
pub(crate) fn steps(reports: &[Value]) -> Value {
    let last = |k: &str| {
        reports
            .iter()
            .rev()
            .find_map(|r| r[k].as_f64())
            .unwrap_or(0.0)
    };
    let max = |k: &str| {
        reports
            .iter()
            .filter_map(|r| r[k].as_f64())
            .fold(0.0f64, f64::max)
    };
    json!({
        "birth": { "p50_under_us": last("birth_step_p50_us"), "p99_under_us": last("birth_step_p99_us"),
                   "max_us": max("birth_step_max_us") },
        "enumeration": { "p50_under_us": last("enum_step_p50_us"), "p99_under_us": last("enum_step_p99_us"),
                         "max_us": max("enum_step_max_us") },
        "over_250_us": max("birth_step_max_us") > 250.0 || max("enum_step_max_us") > 250.0,
    })
}

/// The read-only cost probes, in the order they run at each size.
const PROBES: [(&str, &str); 4] = [
    ("perf-server", "server"),
    ("perf-hook", "hook"),
    ("getters", "hook"),
    ("reads", "hook"),
];

/// The probes part: the cost probes of `lua/sampling` at 500, 1,200 and
/// 2,000 objects of the mixed scenario, with no sidecar attached, so the
/// agent samples nothing while they measure. Each probe's figures go to
/// `probe-<name>-<size>.json` in the bundle.
fn part_probes(run: &Run, name: &str, shell: &[u8]) -> Result<Value, String> {
    run.fresh(name, false, shell)?;
    run.hold(5.0)?;
    let mut out = json!({ "ok": true, "moving_share_500": 1.0, "moving_share_1200_2000": MOVING });
    let mut stage = |label: &str, spawn: (usize, &str, f64)| -> Result<(), String> {
        let spawned = run.spawn(spawn.0, spawn.1, spawn.2)?;
        let fps = run.fps(label)?;
        let mut probes = json!({});
        for (probe, state) in PROBES {
            let file = format!("lua/sampling/{probe}.lua");
            let source = std::fs::read_to_string(run.repository.join(&file))
                .map_err(|e| format!("{file}: {e}"))?;
            let v = run.eval(state, &format!("s10-{probe}"), &source, 600)?;
            let path = run.folder.join(format!("probe-{probe}-{label}.json"));
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("{}: {e}", path.display()))?;
            println!("  {probe} at {label}: {}", path.display());
            probes[probe] = v;
        }
        out[label] = json!({ "spawned": spawned, "fps": fps, "objects": run.live_count()?, "probes": probes });
        Ok(())
    };
    stage("500", (500, "a", 1.0))?;
    run.clear()?;
    run.hold(5.0)?;
    stage("1200", (1200, "b", MOVING))?;
    stage("2000", (800, "c", MOVING))?;
    Ok(out)
}

/// The design probes, in the order they run.
const DESIGN: [&str; 2] = ["design-objects", "design-combat"];

/// The design part: `lua/sampling/design-*.lua` in the server state at 500
/// objects of the mixed scenario with every group moving, with no sidecar
/// attached. Each probe's findings go to `probe-<name>.json` in the bundle.
fn part_design(run: &Run, name: &str, shell: &[u8]) -> Result<Value, String> {
    run.fresh(name, false, shell)?;
    run.hold(5.0)?;
    let spawned = run.spawn(500, "a", 1.0)?;
    run.hold(10.0)?;
    let mut probes = json!({});
    for probe in DESIGN {
        let file = format!("lua/sampling/{probe}.lua");
        let source = std::fs::read_to_string(run.repository.join(&file))
            .map_err(|e| format!("{file}: {e}"))?;
        let v = run.eval("server", &format!("s10-{probe}"), &source, 600)?;
        let path = run.folder.join(format!("probe-{probe}.json"));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("  {probe}: {}", path.display());
        probes[probe] = v;
    }
    Ok(json!({ "ok": true, "spawned": spawned, "objects": run.live_count()?, "probes": probes }))
}

/// The death probe (`lua/sampling/deaths.lua`), with the stand-in sidecar
/// attached and recording, so the agent keeps its table: the probe's own
/// findings go to `probe-deaths.json`, and the verdict
/// ([`super::deaths`]) sets them against the recording.
fn part_deaths(run: &Run, name: &str, shell: &[u8], probe: &Path) -> Result<Value, String> {
    run.fresh(name, false, shell)?;
    run.start_probe(probe, 0)?;
    run.hold(5.0)?;
    let source = std::fs::read_to_string(run.repository.join("lua/sampling/deaths.lua"))
        .map_err(|e| format!("lua/sampling/deaths.lua: {e}"))?
        .replace("@WATCH@", "300")
        .replace("@GIVE_UP@", "240")
        .replace("@WRECK@", "65");
    let found = run.eval("server", "s10-deaths", &source, 900)?;
    let path = run.folder.join("probe-deaths.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&found).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("  deaths: {}", path.display());
    // The last records reach the recording before the probe stops.
    run.hold(3.0)?;
    let fps = run.fps("deaths")?;
    run.stop_probe("deaths");
    let mut verdict = super::deaths(
        &found,
        &run.folder.join("deaths-recording.jsonl"),
        None,
        None,
    )?;
    verdict["fps_after"] = fps;
    Ok(verdict)
}

fn part_mirror(
    run: &Run,
    name: &str,
    shell: &[u8],
    probe: &Path,
    only_500: bool,
) -> Result<Value, String> {
    run.fresh(name, false, shell)?;
    run.start_probe(probe, 0)?;
    run.hold(5.0)?;
    // The cross-state call in parts, for S11 (lua/sampling/xcall.lua).
    let xcall_src = std::fs::read_to_string(run.repository.join("lua/sampling/xcall.lua"))
        .map_err(|e| format!("lua/sampling/xcall.lua: {e}"))?;
    let xcall = run.eval("hook", "s10-xcall", &xcall_src, 60)?;
    println!("  cross-state call: {xcall}");
    // 500 with every ground group moving.
    let first = run.spawn(500, "a", 1.0)?;
    let fps500 = run.fps("500")?;
    let (v500, m500) = run.compare("500")?;
    if only_500 {
        let reports = run.reports(0)?;
        run.stop_probe("mirror");
        let lines = std::fs::read(run.folder.join("mirror-probe.jsonl"))
            .map(|b| link::parse_lines(&b))
            .unwrap_or_default();
        return Ok(json!({
            "ok": v500["matches"] == json!(true),
            "spawned": [first],
            "verdict_500": v500,
            "fps_500": fps500,
            "periods": m500["periods"],
            "steps": steps(&reports),
            "tail": tail(&reports),
            "reports": reports,
            "pump": pump(&lines),
        }));
    }
    // 1,200 and then 2,000 with one ground group in five moving (the rule
    // of the S10 ruling: 2,000 with every group moving held under 2 fps).
    let cleared = run.clear()?;
    let mark_b = run.lines()?.len();
    let second = run.spawn(1200, "b", MOVING)?;
    // The cleared objects leave the table at the next enumeration, and the
    // new ones enter by births: compare once the table holds what DCS holds.
    let (filled1200, fill1200_s) = run.fill(mark_b, 1200, 1.0)?;
    run.hold(5.0)?;
    let hit = run.churn(&second, 20)?;
    let fps1200 = run.fps("1200")?;
    let (v1200, m1200) = run.compare("1200")?;
    let mark = run.lines()?.len();
    let third = run.spawn(800, "c", MOVING)?;
    let (filled2000, fill2000_s) = run.fill(mark, 800, 1.0)?;
    let steady = run.lines()?.len();
    let burst_positions = worst_position(&run.reports(mark)?[..]);
    let fps2000 = run.fps("2000")?;
    let (v2000, m) = run.compare("2000")?;
    let at2000 = run.reports(mark)?;
    let (burst_ok, burst_span) = span(&at2000);
    let settled = run.reports(steady)?;
    let (steady_ok, steady_span) = span(&settled);
    let reports = run.reports(0)?;
    let lines = run.lines()?;
    run.stop_probe("mirror");
    let lines_final = std::fs::read(run.folder.join("mirror-probe.jsonl"))
        .map(|b| link::parse_lines(&b))
        .unwrap_or(lines);
    let pump = pump(&lines_final);
    // The ruled checks at 2,000 (S10 ruling on the pump's credit).
    let sum = |k: &str| at2000.iter().filter_map(|r| r[k].as_f64()).sum::<f64>();
    let exact_tail = tail(&reports);
    let timed_reports = positions::timed(&lines_final, 0);
    let (position_checks, _) = positions_checks(
        &run.folder,
        "2000",
        &timed_reports,
        positions::settled_from(&timed_reports),
        false,
        " at 2,000, settled",
    )?;
    let p99 = pump["p99_under_us"].as_f64().unwrap_or(f64::INFINITY);
    let p99_rest = pump["p99_without_syscall_under_us"]
        .as_f64()
        .unwrap_or(f64::INFINITY);
    let all_match = [&v500, &v1200, &v2000]
        .iter()
        .all(|v| v["matches"] == json!(true));
    let mut checks = vec![
        json!({"check": "mirror matches at 500, 1,200 and 2,000", "ok": all_match}),
        // The budget holds the mean over a span, not each 10 s window: a
        // window can spend credit earned before it, or run up debt that the
        // next one repays (spec/sampling.md, "Slice").
        json!({"check": "mean share <= 0.003 and nothing forgiven, from the 800 spawn on", "ok": burst_ok,
               "detail": burst_span}),
        json!({"check": "mean share <= 0.003 and nothing forgiven at 2,000, settled", "ok": steady_ok,
               "detail": steady_span}),
        json!({"check": "the mirror fills within twice the births' expected time", "ok": filled2000,
               "detail": format!("{fill2000_s:.0} s for 800 births")}),
        json!({"check": "position period <= 3 s while the table fills", "ok": burst_positions > 0.0 && burst_positions <= 3.0,
               "detail": format!("worst {burst_positions:.2} s")}),
        json!({"check": "pump p99 <= 250 µs (frames over 250 µs at most 1 %, counted exactly)",
               "ok": exact_tail["p99_within_250"] == json!(true),
               "detail": format!("{} of {} frames over; histogram: under {p99} µs, without system-call frames under {p99_rest} µs",
                                 exact_tail["over_250"], exact_tail["frames"])}),
        json!({"check": "pump rate reported", "ok": sum("frames") > 0.0,
               "detail": format!("{} pumps in {} frames: one every {:.2}", sum("pumps"), sum("frames"),
                                 sum("frames") / sum("pumps").max(1.0))}),
    ];
    checks.splice(1..1, position_checks);
    checks.extend(k1_rates(&settled, " at 2,000, settled"));
    let ok = checks.iter().all(|c| c["ok"] == json!(true));
    Ok(json!({
        "ok": ok,
        "moving_share_500": 1.0,
        "moving_share_1200_2000": MOVING,
        "spawned": [first, second, third],
        "cleared_before_1200": cleared,
        "hit": hit,
        "verdict_500": v500,
        "verdict_1200": v1200,
        "verdict_2000": v2000,
        "fps_500": fps500,
        "fps_1200": fps1200,
        "fps_2000": fps2000,
        "fill_2000_s": fill2000_s,
        "fill_1200": [filled1200, fill1200_s],
        "periods_500": m500["periods"],
        "periods_1200": m1200["periods"],
        "periods": m["periods"],
        "steps": steps(&reports),
        "tail": tail(&reports),
        "reports": reports,
        "checks": checks,
        "xcall": xcall,
        "pump": pump,
    }))
}

/// Rows a second of one tier over a set of reports.
fn row_rate(reports: &[Value], key: &str) -> f64 {
    let rows: f64 = reports.iter().filter_map(|r| r[key].as_f64()).sum();
    let window: f64 = reports.iter().filter_map(|r| r["window_s"].as_f64()).sum();
    if window > 0.0 { rows / window } else { 0.0 }
}

/// The mean of a reported key over a set of reports.
fn mean_of(reports: &[Value], key: &str) -> f64 {
    if reports.is_empty() {
        return 0.0;
    }
    reports
        .iter()
        .map(|r| r[key].as_f64().unwrap_or(0.0))
        .sum::<f64>()
        / reports.len() as f64
}

/// The vitals tier's default target period, s (spec/sampling.md, "Rates").
/// The runs here leave it at its default; `due_vitals` counts against the
/// agent's configured target, so a run with another target reports a wrong
/// rate.
const VITALS_TARGET_S: f64 = 5.0;

/// Two judged checks over steady reports at k = 1 (ruling on sampler rules
/// at live row costs, 2026-11-15): the vitals band, 4.5 to 6.5 s, as a rate
/// (the default target × the rows due at it over the rows the vitals cursor
/// read, `due_vitals` and `swept_vitals`), beside the mean, median and
/// maximum of the reported `vitals_s`, which shows an enumeration's wait;
/// and the pump bound in every report: pumps a second at most 5 +
/// `pump_work_us` a second ÷ (3 × `xcall_seen_us`). The pumps a second and
/// the call share (`pumps` × `xcall_seen_us` over `us_xcall` + `us_pump`) are
/// reported with it. A span whose reports lack the keys fails, not measured.
/// `what` names the span.
fn k1_rates(reports: &[Value], what: &str) -> Vec<Value> {
    let sum = |k: &str| reports.iter().filter_map(|r| r[k].as_f64()).sum::<f64>();
    let (due, read, window) = (sum("due_vitals"), sum("swept_vitals"), sum("window_s"));
    let mut readings: Vec<f64> = reports
        .iter()
        .filter_map(|r| r["vitals_s"].as_f64())
        .collect();
    readings.sort_by(f64::total_cmp);
    let n = readings.len();
    let median = match n {
        0 => 0.0,
        _ if n % 2 == 1 => readings[n / 2],
        _ => (readings[n / 2 - 1] + readings[n / 2]) / 2.0,
    };
    let shown = format!(
        "readings: mean {:.2} s, median {median:.2} s, maximum {:.2} s over {n} reports",
        readings.iter().sum::<f64>() / n.max(1) as f64,
        readings.last().copied().unwrap_or(0.0),
    );
    let has = |k: &str| reports.iter().any(|r| r[k].as_f64().is_some());
    let (vitals_ok, vitals) = if !(has("due_vitals") && has("swept_vitals")) || due <= 0.0 {
        (
            false,
            format!(
                "not measured: {read:.0} rows swept, {due:.0} due (`swept_vitals`, `due_vitals`); {shown}"
            ),
        )
    } else if read <= 0.0 {
        (
            false,
            format!("outside the band: 0 rows swept of {due:.0} due in {window:.0} s; {shown}"),
        )
    } else {
        let rate = VITALS_TARGET_S * due / read;
        let inside = (4.5..=6.5).contains(&rate);
        let band = if inside { "inside" } else { "outside" };
        (
            inside,
            format!(
                "{rate:.2} s as a rate against the default 5 s target, {band} the band: \
                 {read:.0} rows swept of {due:.0} due in {window:.0} s; {shown}"
            ),
        )
    };
    let (pumps_ok, pumps) = pump_bound(reports);
    vec![
        json!({"check": format!("vitals band 4.5-6.5 s as a rate at k = 1{what}"),
               "ok": vitals_ok, "detail": vitals}),
        json!({"check": format!("pumps a second within the pump bound at k = 1{what}"),
               "ok": pumps_ok, "detail": pumps}),
    ]
}

/// The pump bound over `reports`, each a 10 s window: pumps a second at
/// most 5 + `pump_work_us` ÷ `window_s` ÷ (3 × `xcall_seen_us`). Returns the
/// verdict and the detail: the span's pumps a second, call share and pump
/// work, and every window past its bound.
fn pump_bound(reports: &[Value]) -> (bool, String) {
    let sum = |k: &str| reports.iter().filter_map(|r| r[k].as_f64()).sum::<f64>();
    let window = sum("window_s");
    if window <= 0.0 {
        return (false, "no reports".to_string());
    }
    let measured = reports
        .iter()
        .all(|r| r["pump_work_us"].as_f64().is_some() && r["xcall_seen_us"].as_f64().is_some());
    let pumps = sum("pumps");
    let rate = pumps / window;
    let spent = sum("us_xcall") + sum("us_pump");
    let calls: f64 = reports
        .iter()
        .filter_map(|r| Some(r["pumps"].as_f64()? * r["xcall_seen_us"].as_f64()?))
        .sum();
    let share = if spent > 0.0 { calls / spent } else { 0.0 };
    let work = sum("pump_work_us");
    let empty = sum("empty_pumps");
    let mut over = Vec::new();
    for r in reports {
        let (Some(w), Some(p), Some(u), Some(x)) = (
            r["window_s"].as_f64(),
            r["pumps"].as_f64(),
            r["pump_work_us"].as_f64(),
            r["xcall_seen_us"].as_f64(),
        ) else {
            continue;
        };
        if w <= 0.0 {
            continue;
        }
        let bound = 5.0 + u / w / (3.0 * x.max(1.0));
        if p / w > bound {
            over.push(format!("{:.2} against {bound:.2}", p / w));
        }
    }
    let head = format!(
        "{rate:.2} pumps a second in {window:.0} s, call share {share:.2}, \
         pump work {:.0} µs a second, {empty:.0} of {pumps:.0} pumps planned no row",
        work / window
    );
    if !measured {
        (
            false,
            format!("not measured (`pump_work_us`, `xcall_seen_us`): {head}"),
        )
    } else if over.is_empty() {
        (true, format!("{head}; every window within its bound"))
    } else {
        (
            false,
            format!("{head}; past the bound: {}", over.join(", ")),
        )
    }
}

/// The order of the tiers at k = 0.1, judged over the span of the reports
/// and not at one instant, when a tier's reported period includes any stall
/// in progress. `k1` holds the steady reports before the change and `k01`
/// the reports after it, less the first, whose window straddles it. Returns
/// each check's name, verdict and detail.
fn tenth_order(
    k1: &[Value],
    k01: &[Value],
    subscribed_interval: f64,
) -> Vec<(String, bool, String)> {
    // The share of its rows a tier kept: None when it owed none at k = 1.
    let kept = |key: &str| {
        let base = row_rate(k1, key);
        (base > 0.0).then(|| row_rate(k01, key) / base)
    };
    let (pos, sub, vit) = (
        kept("rows_positions"),
        kept("rows_subscribed"),
        kept("rows_vitals"),
    );
    let wh = kept("rows_warehouses");
    let tol = 0.02;
    let le = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => a <= b + tol,
        _ => true,
    };
    let show = |v: Option<f64>| v.map_or("not judged".to_string(), |v| format!("{v:.2}"));
    // At live row costs (ruling of 2026-11-15) k = 0.1 is judged on two
    // things only: the subscribed rows read rows, and they keep at least the
    // positions' ratio (the rank). The lone-row rule bounds their absolute
    // keep, and the order below the positions is reported. A k = 1 base
    // with no subscribed rows measures nothing, and fails.
    let rank_ok = sub.is_some_and(|s| s > 0.0) && le(pos, sub);
    let order_ok = le(vit, pos) && le(wh, vit);
    // The periods are reported beside the rates and judged by nothing: a
    // reported period holds any stall in progress when the report is made.
    let periods = format!(
        "mean periods: warehouses {:.2}, vitals {:.2}, positions {:.2}, \
         subscribed up to {subscribed_interval:.2}",
        mean_of(k01, "warehouse_s"),
        mean_of(k01, "vitals_s"),
        mean_of(k01, "position_s"),
    );
    vec![
        (
            "subscribed positions read rows and keep at least the positions' ratio".to_string(),
            rank_ok,
            format!("kept: subscribed {}, positions {}", show(sub), show(pos)),
        ),
        (
            "the lower tiers stretch first, reported".to_string(),
            true,
            format!("{}; ", if order_ok { "in order" } else { "out of order" })
                + &format!(
                    "kept: subscribed {}, positions {}, vitals {}, warehouses {}; \
                 {} reports at k = 1, {} at k = 0.1; {periods}",
                    show(sub),
                    show(pos),
                    show(vit),
                    show(wh),
                    k1.len(),
                    k01.len()
                ),
        ),
        (
            "subscribed positions keep 90 % of their rows a second, reported".to_string(),
            true,
            format!("kept {}", show(sub)),
        ),
    ]
}

fn part_tenth(run: &Run, name: &str, shell: &[u8], probe: &Path) -> Result<Value, String> {
    run.fresh(name, true, shell)?;
    run.start_probe(probe, SUBSCRIBED)?;
    run.hold(5.0)?;
    let mark0 = run.lines()?.len();
    let spawned = run.spawn(2000, "a", MOVING)?;
    let (filled_k1, fill_k1_s) = run.fill(mark0, 2000, 1.0)?;
    let burst_k1_positions = worst_position(&run.reports(mark0)?[..]);
    let fps_k1 = run.fps("k1")?;
    let (v1, m1) = run.compare("k1")?;
    let mark = run.lines()?.len();
    let (k1_ok, k1_span) = span(&run.reports(mark0)?[..]);
    let armed = run.eval(
        "server",
        "s10-inject",
        "local shim = __nanoprobe_shim\n\
         if not shim then error('no shim in this state', 0) end\n\
         return shim.agent.inject('sampling', 0.1)\n",
        30,
    )?;
    if armed != json!(true) {
        return Err(format!("inject sampling 0.1: {armed}"));
    }
    run.hold(20.0)?;
    let more = run.spawn(50, "c", MOVING)?;
    let hit = run.churn(&spawned, 10)?;
    let (filled_k01, fill_k01_s) = run.fill(mark, 50, 0.1)?;
    let fps_k01 = run.fps("k01")?;
    let (v2, m2) = run.compare("k01")?;
    let reports = run.reports(mark)?;
    let lines = run.lines()?;
    run.stop_probe("tenth");
    let lines_final = std::fs::read(run.folder.join("tenth-probe.jsonl"))
        .map(|b| link::parse_lines(&b))
        .unwrap_or(lines);
    let pump = pump(&lines_final);
    // Read from the probe's own copy: `stop_probe` removed the files on the
    // target, so `run.reports` after it returns nothing.
    let all_reports = reports_in(&lines_final, mark0);
    let timed_all = positions::timed(&lines_final, mark0);

    let at = |m: &Value, i: usize| m["periods"][i].as_f64().unwrap_or(0.0);
    let subscribed: Vec<u64> = m2["subscribed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    let sub_interval = m2["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            r["object"]
                .as_u64()
                .is_some_and(|o| subscribed.contains(&o))
        })
        .filter_map(|r| r["interval"].as_f64())
        .fold(0.0f64, f64::max);
    let en = at(&m2, 4);
    let mut checks = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        checks.push(json!({"check": name, "ok": ok, "detail": detail}));
    };
    check(
        "mirror matches at k = 1",
        v1["matches"] == json!(true),
        v1.to_string(),
    );
    check(
        "mirror matches at k = 0.1",
        v2["matches"] == json!(true),
        v2.to_string(),
    );
    check(
        "subscribed positions keep a rate",
        sub_interval > 0.0 && sub_interval.is_finite() && subscribed.len() == SUBSCRIBED,
        format!(
            "{} subscribed, at most {sub_interval:.2} s apart",
            subscribed.len()
        ),
    );
    // The window of the first k = 0.1 report straddles the change, and the
    // first reports at k = 1 hold the spawn burst.
    let k1: Vec<Value> = all_reports
        .iter()
        .take_while(|r| r["scale"].as_f64() == Some(1.0))
        .cloned()
        .collect();
    // The positions target at k = 1 by the stall-free measure, over the k = 1
    // windows after the table filled; the last window alone can be an
    // enumeration's.
    let k1_timed: Vec<(f64, Value)> = timed_all
        .iter()
        .take_while(|(_, r)| r["scale"].as_f64() == Some(1.0))
        .cloned()
        .collect();
    let (position_checks, _) = positions_checks(
        &run.folder,
        "k1",
        &k1_timed,
        positions::settled_from(&k1_timed),
        false,
        " at k = 1, settled",
    )?;
    // The stall-free window ceiling is judged in the mirror part, the judged
    // scenario. Here the budget runs near its cap at k = 1: while an
    // enumeration is due or running the sweep takes at most 1 − m of a frame
    // (`higher_waits`), and once it ends the sweep waits for the subscribed
    // pump to repay the rows that fell behind (an open item of the ruling on
    // live row costs), so a window holding an enumeration can read a long
    // period with no stall. The ceiling is reported; the per-object median
    // is judged (coordinator ruling, 2026-11-14, on the tenth part's k = 1).
    for c in position_checks {
        let name = c["check"].as_str().unwrap_or_default();
        if name.starts_with("no stall-free window's position period") {
            check(&format!("{name}, reported"), true, c["detail"].to_string());
        } else {
            check(name, c["ok"] == json!(true), c["detail"].to_string());
        }
    }
    for c in k1_rates(
        &k1[positions::settled_from(&k1_timed).min(k1.len())..],
        ", settled",
    ) {
        check(
            c["check"].as_str().unwrap_or_default(),
            c["ok"] == json!(true),
            c["detail"].as_str().unwrap_or_default().to_string(),
        );
    }
    let k1 = &k1[k1.len().saturating_sub(2)..];
    let k01_all: Vec<Value> = reports
        .iter()
        .filter(|r| r["scale"] == json!(0.1))
        .cloned()
        .collect();
    let k01 = k01_all.get(1..).unwrap_or(&[]);
    for (name, ok, detail) in tenth_order(k1, k01, sub_interval) {
        check(&name, ok, detail);
    }
    // At k = 0.1 subscribed positions keep a rate before the enumeration
    // keeps its period (S10 ruling 3b): it must still complete.
    let used = |m: &Value| m["counters"]["enumerations_used"].as_u64().unwrap_or(0);
    check(
        "the mirror fills at k = 0.1 within twice the expected time, reported",
        true,
        format!(
            "{}: {fill_k01_s:.0} s for 50 births",
            if filled_k01 {
                "within it"
            } else {
                "not within it"
            }
        ),
    );
    check(
        "an enumeration completes at k = 0.1, reported",
        true,
        format!(
            "{}; {} ended between the two compares; period {en:.1} s against 60 s",
            if used(&m2) > used(&m1) {
                "completed"
            } else {
                "none completed"
            },
            used(&m2).saturating_sub(used(&m1))
        ),
    );
    // The held ENUM_END (spec/records.md, "Layout: ENUM_END"): the agent
    // abandoned no hold in any report of this part (the log also holds
    // reports of builds before the deploy), and the mirror's rule removed no
    // row the agent still holds.
    let abandoned_vals: Vec<u64> = all_reports
        .iter()
        .filter_map(|r| r["enum_end_abandoned"].as_u64())
        .collect();
    // The part's first reports can come from the build before the deploy;
    // the deployed build's last report must carry the key.
    let abandoned_present = all_reports
        .last()
        .is_some_and(|r| r["enum_end_abandoned"].as_u64().is_some());
    let abandoned = abandoned_vals.iter().copied().max().unwrap_or(0);
    let counter_opt = |m: &Value, k: &str| m["counters"][k].as_u64();
    let counter = |m: &Value, k: &str| counter_opt(m, k).unwrap_or(0);
    let counters_present = counter_opt(&m2, "missing_by_rule").is_some()
        && counter_opt(&m2, "disagreements").is_some();
    check(
        "no ENUM_END hold was abandoned and the mirror's rule removed nothing",
        abandoned_present
            && counters_present
            && abandoned == 0
            && counter(&m2, "missing_by_rule") == 0
            && counter(&m2, "disagreements") == 0,
        format!(
            "enum_end_abandoned {abandoned}{}, missing_by_rule {}, disagreements {}{}; \
             enumerations used {}, refused {} (SEEN ID unknown to the mirror {}, guarded {})",
            if abandoned_present {
                ""
            } else {
                " (key missing)"
            },
            counter(&m2, "missing_by_rule"),
            counter(&m2, "disagreements"),
            if counters_present {
                ""
            } else {
                " (key missing)"
            },
            used(&m2),
            counter(&m2, "enumerations_refused"),
            counter(&m2, "seen_unknown"),
            counter(&m2, "seen_guarded")
        ),
    );
    let reps: Vec<&Value> = reports
        .iter()
        .filter(|r| r["scale"] == json!(0.1))
        .collect();
    let max = |k: &str| {
        reps.iter()
            .filter_map(|r| r[k].as_f64())
            .fold(0.0f64, f64::max)
    };
    let sum = |k: &str| reps.iter().filter_map(|r| r[k].as_f64()).sum::<f64>();
    check(
        "sampling reports at k = 0.1",
        reps.len() >= 3,
        format!("{} reports", reps.len()),
    );
    let reps_owned: Vec<Value> = reps.iter().map(|r| (*r).clone()).collect();
    let (k01_ok, k01_span) = span(&reps_owned);
    check(
        "mean share <= overhead_share and nothing forgiven at k = 0.1",
        k01_ok,
        k01_span.to_string(),
    );
    check(
        "mean share <= overhead_share and nothing forgiven at k = 1, spawn included",
        k1_ok,
        k1_span.to_string(),
    );
    check(
        "the mirror fills within twice the births' expected time at k = 1",
        filled_k1,
        format!("{fill_k1_s:.0} s for 2,000 births"),
    );
    // Reported, not judged: here the table fills from empty, so the reported
    // period (the time since the last completed lap) equals the fill's
    // duration by definition. The mirror part judges the fill onto a
    // populated table, and the fill time above stays judged.
    check(
        "position period while the table fills at k = 1, reported",
        true,
        format!("worst {burst_k1_positions:.2} s"),
    );
    check(
        "no debt forgiven",
        max("forgiven_us") == 0.0,
        format!("max {}", max("forgiven_us")),
    );
    // Reported, not judged: a nonzero value is a DCS stall inside a birth or
    // enumeration step, past the balance's bound (spec/sampling.md, "Tiers").
    check(
        "births' and enumeration time forgiven (a DCS stall), reported",
        true,
        format!(
            "{} µs in {} windows",
            sum("maintenance_forgiven_us"),
            reps.iter()
                .filter(|r| r["maintenance_forgiven_us"].as_f64().unwrap_or(0.0) > 0.0)
                .count()
        ),
    );
    // The debt, overrun and sweep-budget checks below stay judged at k = 0.1
    // too: they bound the agent's time and the frame cap, the owner's
    // targets, not a tier's rate, which the ruling on live row costs (item
    // 3) makes reported there.
    check(
        "debt bounded",
        max("debt_us") <= SHARE * 1e6,
        format!("max {:.1} µs", max("debt_us")),
    );
    let (rows, over) = (sum("row_frames"), sum("overrun_frames"));
    // The count of row frames past k·B is reported, not failed (coordinator
    // ruling, 2026-11-14): at k = 0.1 the gate is judged unscaled, so the
    // few row frames are mostly single-row pumps; the largest overrun stays
    // under the slice cap, and a gate that knows the scale is an S11 item.
    check(
        "sampling past k·B reported; the largest overrun under the slice cap",
        max("overrun_us") < 250.0,
        format!(
            "{over} of {rows} row frames past it; largest overrun {:.1} µs, clock interval up to {:.1} µs; \
             maintenance frames {} (largest {:.1} µs), lone rows {}",
            max("overrun_us"),
            max("clock_interval_us"),
            sum("maintenance_frames"),
            max("maintenance_overrun_us"),
            sum("lone_rows")
        ),
    );
    let (sweeps, sweeps_over) = (sum("sweeps"), sum("sweep_overruns"));
    check(
        "the sweep <= its budget + the rows between two clock reads",
        sweeps > 0.0 && sweeps_over <= 0.01 * sweeps,
        // A forced one-row sweep (its fixed part alone passed its budget)
        // is counted apart by the agent, in `sweep_forced`, and is in
        // neither count (spec/sampling.md, "Reporting").
        format!(
            "{sweeps_over} of {sweeps} sweeps past it; largest overrun {:.1} µs; \
             {} forced one-row sweeps counted apart{}",
            max("sweep_overrun_us"),
            sum("sweep_forced"),
            if reps.iter().all(|r| r["sweep_forced"].is_null()) {
                " (key absent from these reports)"
            } else {
                ""
            }
        ),
    );
    let p99 = pump["p99_under_us"].as_f64().unwrap_or(f64::INFINITY);
    // Judged over the reports at k = 1; the tail at k = 0.1 is reported.
    let (exact_tail, tail_k01) = tail_by_scale(&all_reports);
    check(
        "pump p99 <= 250 µs at k = 1 (frames over 250 µs at most 1 %, counted exactly)",
        exact_tail["p99_within_250"] == json!(true),
        format!(
            "{} of {} frames over; histogram: p99 under {p99} µs; at k = 0.1, reported: {} of {} over",
            exact_tail["over_250"], exact_tail["frames"], tail_k01["over_250"], tail_k01["frames"]
        ),
    );
    let ok = checks.iter().all(|c| c["ok"] == json!(true));
    Ok(json!({
        "ok": ok,
        "checks": checks,
        "spawned": [spawned, more],
        "hit": hit,
        "moving_share": MOVING,
        "fps_k1": fps_k1,
        "fill_k1_s": fill_k1_s,
        "fill_k01_s": fill_k01_s,
        "fps_k01": fps_k01,
        "periods_k1": m1["periods"],
        "periods_k01": m2["periods"],
        "steps": steps(&reports),
        "tail": tail(&reports),
        "tail_k1": exact_tail,
        "tail_k01": tail_k01,
        "reports": reports,
        "pump": pump,
    }))
}

/// `sampling <name> [--part ...]`.
/// What a live run needs: the connection, the shell mission and the probe.
pub(crate) struct Opened {
    pub(crate) run: Run,
    pub(crate) shell: Vec<u8>,
    pub(crate) probe: PathBuf,
}

/// Check the target, build the shell mission and the Linux probe, and make
/// the run's bundle folder `bundles/<kind>-<name>-<time>/`. Refuses a server
/// with players.
pub(crate) fn open(repository: &Path, name: &str, kind: &str) -> Result<Opened, String> {
    let target = crate::target(name)?;
    if !matches!(target, Target::DockerWine { .. }) {
        return Err(format!("{kind} runs on a docker-wine target"));
    }
    let template = std::fs::read(repository.join("templates/caucasus.miz"))
        .map_err(|e| format!("templates/caucasus.miz: {e}"))?;
    let shell = miz::generate(&template, &miz::Changes::default())?;
    let (remote, _, _) = crate::remote(name)?;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "link-probe")])?
        .pop()
        .ok_or("link-probe was not built")?;
    let folder = bundle::folder().join(format!("{kind}-{name}-{}", bundle::stamp()));
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let toml = remote
        .read(crate::install::AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let config = nanoprobe_agent_link::config::AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    let run = Run {
        map: link::linux_path(&config.transport.map_path)?,
        port: config.transport.port.to_string(),
        token: remote.full_path(crate::install::TOKEN),
        remote,
        folder,
        repository: repository.to_path_buf(),
    };
    Ok(Opened { run, shell, probe })
}

/// [`open`] for a run that needs the eval channel only: no link probe and no
/// `agent.toml`, so the connection fields the probe uses are empty. Returns
/// the run and the shell mission.
pub(crate) fn open_eval(
    repository: &Path,
    name: &str,
    kind: &str,
) -> Result<(Run, Vec<u8>), String> {
    let target = crate::target(name)?;
    if !matches!(target, Target::DockerWine { .. }) {
        return Err(format!("{kind} runs on a docker-wine target"));
    }
    let template = std::fs::read(repository.join("templates/caucasus.miz"))
        .map_err(|e| format!("templates/caucasus.miz: {e}"))?;
    let shell = miz::generate(&template, &miz::Changes::default())?;
    let (remote, _, _) = crate::remote(name)?;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }
    let folder = bundle::folder().join(format!("{kind}-{name}-{}", bundle::stamp()));
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let run = Run {
        map: String::new(),
        port: String::new(),
        token: String::new(),
        remote,
        folder,
        repository: repository.to_path_buf(),
    };
    Ok((run, shell))
}

/// The part to run on a target: a windows-ssh target runs the windows part,
/// which is also its default, and the others run on docker-wine only.
pub(crate) fn resolve_part(part: Part, windows: bool) -> Result<Part, String> {
    match (part, windows) {
        (Part::Both | Part::Windows, true) => Ok(Part::Windows),
        (Part::Windows, false) => Err("--part windows runs on a windows-ssh target".into()),
        (_, true) => Err("this part runs on a docker-wine target; use --part windows".into()),
        (part, false) => Ok(part),
    }
}

pub fn run(repository: &Path, name: &str, part: Part) -> Result<ExitCode, String> {
    let part = resolve_part(
        part,
        matches!(crate::target(name)?, Target::WindowsSsh { .. }),
    )?;
    if part == Part::Windows {
        return super::win::run(repository, name);
    }
    let Opened { run, shell, probe } = open(repository, name, "sampling")?;
    let folder = run.folder.clone();
    let started = bundle::utc_now();
    let mut out = json!({ "target": name, "started": started });
    let mut ok = true;
    if part == Part::Probes {
        println!("== part: probes, release build, no sidecar");
        out["probes"] = part_probes(&run, name, &shell)?;
        out["probes"]["agent_build"] = crate::soak::agent_build(name);
    }
    if part == Part::Design {
        println!("== part: design, release build, no sidecar");
        out["design"] = part_design(&run, name, &shell)?;
        out["design"]["agent_build"] = crate::soak::agent_build(name);
    }
    if part == Part::Deaths {
        println!("== part: deaths, release build");
        let r = part_deaths(&run, name, &shell, &probe);
        if r.is_err() {
            run.stop_probe("deaths");
        }
        let mut r = r?;
        ok &= r["ok"] == json!(true);
        // Read before the next part deploys, so it names this part's build.
        r["agent_build"] = crate::soak::agent_build(name);
        out["deaths"] = r;
    }
    if matches!(part, Part::Both | Part::Mirror | Part::Mirror500) {
        println!("== part: mirror, release build");
        let r = part_mirror(&run, name, &shell, &probe, part == Part::Mirror500);
        if r.is_err() {
            run.stop_probe("mirror");
        }
        let mut r = r?;
        ok &= r["ok"] == json!(true);
        // Read before the next part deploys, so it names this part's build.
        r["agent_build"] = crate::soak::agent_build(name);
        out["mirror"] = r;
    }
    if matches!(part, Part::Both | Part::Tenth) {
        println!("== part: tenth, build with fault injection");
        let r = part_tenth(&run, name, &shell, &probe);
        if r.is_err() {
            run.stop_probe("tenth");
        }
        let mut r = r?;
        ok &= r["ok"] == json!(true);
        // Read before the next part deploys, so it names this part's build.
        r["agent_build"] = crate::soak::agent_build(name);
        out["tenth"] = r;
    }
    out["provenance"] = crate::soak::platform_provenance(name);
    out["ok"] = json!(ok);
    out["ended"] = json!(bundle::utc_now());
    let path = folder.join("sampling.json");
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
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_target_runs_the_windows_part_by_default() {
        assert_eq!(resolve_part(Part::Both, true), Ok(Part::Windows));
        assert_eq!(resolve_part(Part::Windows, true), Ok(Part::Windows));
        assert!(resolve_part(Part::Mirror, true).is_err());
        assert!(resolve_part(Part::Windows, false).is_err());
        assert_eq!(resolve_part(Part::Both, false), Ok(Part::Both));
        assert_eq!(resolve_part(Part::Tenth, false), Ok(Part::Tenth));
    }

    #[test]
    fn the_tail_passes_at_one_percent_and_fails_above_it() {
        let report = |frames: u64, over: u64| {
            json!({ "frames": frames, "frames_over_250": over, "syscall_over_250": 0,
                    "frame_max_us": 400.0 })
        };
        let t = tail(&[report(5_000, 50), report(5_000, 50)]);
        assert_eq!(t["over_250"], json!(100));
        assert_eq!(t["frames"], json!(10_000));
        assert_eq!(t["p99_within_250"], json!(true));
        assert_eq!(t["max_us"], json!(400.0));
        // 2 % of the frames over 250 µs fails, though the bucket says 256.
        let t = tail(&[report(5_000, 100), report(5_000, 100)]);
        assert_eq!(t["p99_within_250"], json!(false));
        let t = tail(&[report(10_000, 101)]);
        assert_eq!(t["p99_within_250"], json!(false));
    }

    #[test]
    fn the_tail_is_judged_over_k_one_and_reported_at_a_tenth() {
        let report = |scale: f64, over: u64| {
            json!({ "scale": scale, "frames": 1_000, "frames_over_250": over,
                    "syscall_over_250": 0, "frame_max_us": 400.0 })
        };
        // 1.5 % over at k = 0.1 does not fail the k = 1 judgement.
        let (k1, k01) = tail_by_scale(&[report(1.0, 5), report(1.0, 5), report(0.1, 30)]);
        assert_eq!(k1["p99_within_250"], json!(true));
        assert_eq!(k1["frames"], json!(2_000));
        assert_eq!(k01["over_250"], json!(30));
        // Over 1 % at k = 1 fails.
        let (k1, _) = tail_by_scale(&[report(1.0, 25), report(0.1, 0)]);
        assert_eq!(k1["p99_within_250"], json!(false));
    }

    #[test]
    fn a_report_without_the_exact_count_is_not_a_pass() {
        let old = json!({ "frames": 5_000 });
        let t = tail(&[
            old.clone(),
            json!({ "frames": 5_000, "frames_over_250": 0 }),
        ]);
        assert_eq!(t["missing"], json!(1));
        assert_eq!(t["p99_within_250"], json!(false));
        assert_eq!(tail(&[old])["p99_within_250"], json!(false));
        // No reports at all is no pass either.
        assert_eq!(tail(&[])["p99_within_250"], json!(false));
    }

    #[test]
    fn a_quantile_is_its_bucket_s_upper_edge() {
        let mut h = vec![json!(0); 32];
        h[3] = json!(98);
        h[8] = json!(2);
        assert_eq!(quantile(&h, 0.5), Some(8.0));
        assert_eq!(quantile(&h, 0.99), Some(256.0));
        assert_eq!(quantile(&[json!(0)], 0.5), None);
    }

    fn report(window: f64, rows: [f64; 4], periods: [f64; 3]) -> Value {
        json!({
            "scale": 1.0, "window_s": window,
            "rows_positions": rows[0], "rows_subscribed": rows[1], "rows_vitals": rows[2],
            "rows_warehouses": rows[3],
            "position_s": periods[0], "vitals_s": periods[1],
            "warehouse_s": periods[2],
        })
    }

    #[test]
    fn the_k1_rates_judge_the_vitals_band_as_a_rate_and_the_pump_bound() {
        let r = |due: f64, read: f64, vit: f64| {
            json!({"window_s": 10.0, "pumps": 30.0, "due_vitals": due,
                   "swept_vitals": read, "rows_vitals": read + 50.0, "vitals_s": vit,
                   "pump_work_us": 1000.0, "xcall_seen_us": 35.0,
                   "us_xcall": 1050.0, "us_pump": 2000.0})
        };
        let reports = [
            r(370.0, 380.0, 5.1),
            r(370.0, 360.0, 13.0),
            r(370.0, 370.0, 5.0),
        ];
        let c = k1_rates(&reports, "");
        assert!(c.iter().all(|c| c["ok"] == json!(true)), "{c:?}");
        let vitals = c[0]["detail"].as_str().unwrap();
        assert!(
            vitals.starts_with("5.00 s as a rate against the default 5 s target, inside"),
            "{vitals}"
        );
        assert!(
            vitals.contains("median 5.10 s, maximum 13.00 s"),
            "{vitals}"
        );
        let pumps = c[1]["detail"].as_str().unwrap();
        assert!(
            pumps.starts_with("3.00 pumps a second in 30 s, call share 0.34"),
            "{pumps}"
        );
        assert!(pumps.ends_with("every window within its bound"), "{pumps}");
        let slow = k1_rates(&[r(370.0, 200.0, 9.0)], "");
        assert_eq!(slow[0]["ok"], json!(false));
        assert!(
            slow[0]["detail"]
                .as_str()
                .unwrap()
                .contains("outside the band")
        );
        let starved = k1_rates(&[r(370.0, 0.0, 30.0)], "");
        assert_eq!(starved[0]["ok"], json!(false));
        assert!(
            starved[0]["detail"]
                .as_str()
                .unwrap()
                .starts_with("outside the band: 0 rows swept of 370 due"),
            "{}",
            starved[0]["detail"]
        );
        let old = k1_rates(
            &[json!({"window_s": 10.0, "pumps": 30.0, "rows_vitals": 300.0, "due_vitals": 370.0})],
            "",
        );
        assert!(old.iter().all(|c| c["ok"] == json!(false)));
        assert!(
            old[0]["detail"]
                .as_str()
                .unwrap()
                .starts_with("not measured")
        );
        assert!(
            old[1]["detail"]
                .as_str()
                .unwrap()
                .starts_with("not measured")
        );
        // 10 pumps a second with 300 µs a second of work: the bound is
        // 5 + 300 / 105, about 7.86.
        let mut busy = r(370.0, 370.0, 5.0);
        busy["pumps"] = json!(100.0);
        busy["pump_work_us"] = json!(3000.0);
        let c = k1_rates(&[busy], "");
        assert_eq!(c[1]["ok"], json!(false));
        assert!(
            c[1]["detail"]
                .as_str()
                .unwrap()
                .contains("past the bound: 10.00 against 7.86"),
            "{}",
            c[1]["detail"]
        );
    }

    fn verdicts(k1: &[Value], k01: &[Value], sub: f64) -> Vec<bool> {
        tenth_order(k1, k01, sub).into_iter().map(|c| c.1).collect()
    }

    fn k1() -> Vec<Value> {
        vec![report(10.0, [1000.0, 200.0, 200.0, 50.0], [1.0, 5.0, 7.0])]
    }

    #[test]
    fn the_tenth_order_passes_when_vitals_stretch_more_than_positions() {
        // Positions keep 0.8 of their rows and vitals 0.37.
        let k01 = vec![report(10.0, [800.0, 200.0, 74.0, 15.0], [3.0, 10.5, 14.0])];
        assert_eq!(verdicts(&k1(), &k01, 1.0), [true, true, true]);
        assert!(tenth_order(&k1(), &k01, 1.0)[1].2.starts_with("in order"));
    }

    /// The order below the positions is reported, not judged, at live row
    /// costs.
    #[test]
    fn the_tenth_order_reports_vitals_that_keep_more_than_positions() {
        let k01 = vec![report(10.0, [400.0, 200.0, 190.0, 15.0], [3.0, 10.5, 14.0])];
        let checks = tenth_order(&k1(), &k01, 1.0);
        assert_eq!(checks[1].0, "the lower tiers stretch first, reported");
        assert!(checks[1].1);
        assert!(checks[1].2.starts_with("out of order"), "{}", checks[1].2);
        assert!(checks[1].2.contains("vitals 0.95"), "{}", checks[1].2);
    }

    #[test]
    fn the_tenth_order_passes_a_starved_tier() {
        let k01 = vec![report(10.0, [800.0, 200.0, 0.0, 0.0], [3.0, 40.0, 60.0])];
        assert_eq!(verdicts(&k1(), &k01, 1.0), [true, true, true]);
    }

    #[test]
    fn the_tenth_order_does_not_judge_a_tier_that_owed_nothing() {
        let mut base = k1();
        base[0]["rows_vitals"] = json!(0.0);
        let k01 = vec![report(10.0, [400.0, 200.0, 190.0, 15.0], [3.0, 0.0, 14.0])];
        assert_eq!(verdicts(&base, &k01, 1.0), [true, true, true]);
    }

    /// The subscribed rows must read rows and keep at least the positions'
    /// ratio (the rank); their 90 % keep is reported.
    #[test]
    fn the_tenth_order_holds_the_subscribed_tier_to_the_rank() {
        let kept = |sub: f64| vec![report(10.0, [800.0, sub, 74.0, 15.0], [3.0, 2.0, 14.0])];
        // Half their rows against the positions' 0.8: the rank fails.
        assert_eq!(verdicts(&k1(), &kept(100.0), 1.0), [false, true, true]);
        assert_eq!(verdicts(&k1(), &kept(0.0), 1.0), [false, true, true]);
        // 0.8, the positions' ratio, passes; under 90 % is reported.
        let checks = tenth_order(&k1(), &kept(160.0), 1.0);
        assert_eq!(
            checks.iter().map(|c| c.1).collect::<Vec<_>>(),
            [true, true, true]
        );
        assert_eq!(checks[2].2, "kept 0.80");
        assert_eq!(verdicts(&k1(), &kept(160.0)[..0], 1.0).len(), 3);
        // A k = 1 base with no subscribed rows measures nothing: it fails.
        let mut base = k1();
        base[0]["rows_subscribed"] = json!(0.0);
        assert_eq!(verdicts(&base, &kept(160.0), 1.0), [false, true, true]);
    }
}
