//! `sampling <name> --part windows`: the sampling measurement on a Windows
//! client reached over SSH.
//!
//! A Windows PC has a DCS client and no dedicated server, so the run starts
//! DCS at its main menu through the profile's start task, hosts a private
//! session with a shell mission (`lua/experiments/alone/host-session.lua`),
//! and attaches `link-probe --mirror` as the sidecar, a Windows program run
//! by the SSH account from the profile's `nanoprobe/dev/sampling` folder.
//! Then at 1,200 objects and at 2,000 (800 more) of the mixed scenario, one
//! ground group in five moving, at each size:
//!
//! 1. spawn, and wait until the mirror holds what DCS holds;
//! 2. measure DCS's frame rate (`lua/sampling/fps.lua`, which also records
//!    the share of movers) while the agent's `sampling` reports accrue, and
//!    keep collecting them until [`WINDOW_S`] seconds have passed since the
//!    fill;
//! 3. compare the mirror with the oracle, as the docker-wine mirror part
//!    does, after the window so the oracle does not disturb it.
//!
//! The summary is the docker-wine parts': span (mean share), tail (frames
//! over 250 µs, exact), the pump histogram, steps and periods. The pump
//! histogram is the probe's last `final` line, so it covers both sizes.
//!
//! The DCS player is the hosting client itself, so nothing waits for an empty
//! server. The run refuses when DCS already runs (someone may be playing),
//! starts DCS only through the start task, and at the end closes it through
//! the stop task, removes the probe's folder and undeploys, so the profile
//! holds what it held before. The bundle is
//! `bundles/sampling-<name>-<time>/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::live::{
    EARLY_S, LIVE_COUNT_SOURCE, MOVING, SHIPPED_SHARE, TAIL_BYTES, fill_limit, fps_source,
    hold_source, hold_timeout, pump, span, spawn_source, steps, tail, worst_position,
};
use crate::install::{AGENT_TOML, Profile, TOKEN};
use crate::profile::Target;
use crate::windows::Windows;
use crate::{bundle, eval, link, live, miz, ssh};

/// Where the probe goes, in the profile. `undeploy` removes the whole
/// `nanoprobe` folder, and the run removes this one first.
const DIR: &str = "nanoprobe/dev/sampling";
/// How long the probe may take to attach.
const ATTACH_S: u64 = 90;
/// Real seconds, from the end of the fill, that the `sampling` reports of
/// one size are collected for: three minutes and the report that straddles
/// the fill's end.
const WINDOW_S: u64 = 200;
/// The reports of one size must cover this many seconds.
const WINDOW_MIN_S: f64 = 180.0;
/// The Lua that counts the scenario's movers (`lua/scenarios/movers.lua`).
const MOVERS_SOURCE: &str = "local E = ...\nreturn E.lib('movers').count({ prefix = 'np-' })\n";
/// Read the last `$a1` bytes of the profile file `$a0`; exit 3 when missing.
const TAIL_SCRIPT: &str = r#"$p = Full $a0
if (-not [IO.File]::Exists($p)) { exit 3 }
$f = OpenShared $p
$n = [int64]$a1
if ($f.Length -gt $n) { [void]$f.Seek(-$n, 'End') }
$out = [Console]::OpenStandardOutput()
$f.CopyTo($out)
$out.Flush()
$f.Close()"#;
/// The profile folders whose names the run records before and after.
const LISTED: [&str; 3] = ["", "Scripts", "Scripts/Hooks"];

fn rel(name: &str) -> String {
    format!("{DIR}/{name}")
}

/// The PowerShell that runs the probe in the foreground, with `$a0` the map,
/// `$a1` the port and `$a2` the token file. It runs under an SSH session the
/// caller holds open, since the SSH server ends a session's processes with it.
fn probe_script() -> String {
    format!(
        r#"$d = Full '{DIR}'
$line = '--map "' + $a0 + '" --port ' + $a1 + ' --token-file "' + $a2 + '" --seconds 86400' `
    + ' --record "' + $d + '\recording.jsonl" --stop-file "' + $d + '\stop"' `
    + ' --mirror "' + $d + '\mirror.json"'
$p = Start-Process -FilePath ($d + '\link-probe.exe') -ArgumentList $line -NoNewWindow -PassThru -Wait `
    -RedirectStandardOutput ($d + '\probe.jsonl') -RedirectStandardError ($d + '\probe.err')
exit $p.ExitCode"#
    )
}

/// Names under each of [`LISTED`], as the profile holds them.
pub(crate) fn listings(windows: &Windows) -> Result<BTreeMap<String, Vec<String>>, String> {
    LISTED
        .iter()
        .map(|dir| Ok(((*dir).to_owned(), windows.list(dir)?)))
        .collect()
}

/// What `after` holds that `before` did not, and the reverse, one line each.
pub(crate) fn listing_diff(
    before: &BTreeMap<String, Vec<String>>,
    after: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (dir, was) in before {
        let now = after.get(dir).cloned().unwrap_or_default();
        let at = if dir.is_empty() { "<profile>" } else { dir };
        lines.extend(
            now.iter()
                .filter(|n| !was.contains(n))
                .map(|n| format!("added: {at}/{n}")),
        );
        lines.extend(
            was.iter()
                .filter(|n| !now.contains(n))
                .map(|n| format!("missing: {at}/{n}")),
        );
    }
    lines
}

/// The checks of one size, from what it measured. `window` is the span of
/// the reports from the fill's end, `tail` the exact frame counts over them.
fn stage_checks(
    size: usize,
    filled: (bool, f64),
    window: &(bool, Value),
    tail: &Value,
    matches: bool,
) -> Vec<Value> {
    let real = window.1["real_s"].as_f64().unwrap_or(0.0);
    vec![
        json!({"check": format!("the mirror fills within twice the births' expected time at {size}"),
               "ok": filled.0, "detail": format!("{:.0} s", filled.1)}),
        json!({"check": format!("the reports cover {WINDOW_MIN_S:.0} s at {size}"),
               "ok": real >= WINDOW_MIN_S, "detail": format!("{real:.0} s")}),
        json!({"check": format!("mean share <= {SHIPPED_SHARE} and nothing forgiven at {size}, settled"),
               "ok": window.0, "detail": window.1}),
        json!({"check": format!("frames over 250 µs are at most 1 % at {size}"),
               "ok": tail["p99_within_250"] == json!(true), "detail": tail}),
        json!({"check": format!("the mirror matches the oracle at {size}"),
               "ok": matches}),
    ]
}

/// The Windows client, the bundle folder and the probe's session.
struct Win {
    windows: Windows,
    folder: PathBuf,
    repository: PathBuf,
    probe: Option<JoinHandle<Result<ssh::Output, String>>>,
}

impl Win {
    /// Run Lua in a state through the eval channel; its first value.
    fn eval(&self, state: &str, stem: &str, source: &str, timeout_s: u64) -> Result<Value, String> {
        let r = eval::run(
            &self.windows,
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

    fn hold(&self, seconds: f64) -> Result<(), String> {
        self.eval(
            "hook",
            "s11-hold",
            &hold_source(seconds),
            hold_timeout(seconds),
        )
        .map(drop)
    }

    fn spawn(&self, n: usize, label: &str) -> Result<Value, String> {
        let v = self.eval(
            "server",
            &format!("s11-spawn-{label}"),
            &spawn_source(n, label, MOVING),
            300,
        )?;
        println!("  spawned {label}: {v}");
        Ok(v)
    }

    fn live_count(&self) -> Result<u64, String> {
        let v = self.eval("server", "s11-count", LIVE_COUNT_SOURCE, 60)?;
        v.as_u64()
            .ok_or_else(|| format!("the live count is not a number: {v}"))
    }

    fn movers(&self) -> Option<Value> {
        let v = self.eval("server", "movers", MOVERS_SOURCE, 60).ok()?;
        v.is_object().then_some(v)
    }

    /// DCS's frame rate, with the movers' share over the span.
    fn fps_window(&self, label: &str, settle: u32, measure: u32) -> Result<Value, String> {
        let source = fps_source(&self.repository, settle, measure)?;
        let baseline = self.movers();
        let mut v = self.eval("hook", "s11-fps", &source, 300)?;
        if baseline.is_some()
            && let Some(moving) = self.movers()
        {
            v["moving"] = moving;
        }
        println!("  fps {label}: {v}");
        Ok(v)
    }

    fn lines(&self) -> Result<Vec<Value>, String> {
        Ok(self
            .windows
            .read(&rel("probe.jsonl"))?
            .map(|b| link::parse_lines(&b))
            .unwrap_or_default())
    }

    fn reports(&self, from: usize) -> Result<Vec<Value>, String> {
        Ok(self
            .lines()?
            .into_iter()
            .skip(from)
            .filter(|l| l["ev"] == "log" && l["msg"] == "sampling")
            .map(|l| l["data"].clone())
            .collect())
    }

    fn start_probe(
        &mut self,
        probe: &Path,
        map: &str,
        port: &str,
        token: &str,
    ) -> Result<(), String> {
        self.windows.remove_tree(DIR)?;
        let bytes = std::fs::read(probe).map_err(|e| format!("{}: {e}", probe.display()))?;
        self.windows.write(&rel("link-probe.exe"), &bytes)?;
        let windows = self.windows.clone();
        let (map, port, token) = (map.to_owned(), port.to_owned(), token.to_owned());
        self.probe = Some(std::thread::spawn(move || {
            windows.powershell(&probe_script(), &[&map, &port, &token])
        }));
        let deadline = Instant::now() + Duration::from_secs(ATTACH_S);
        loop {
            if self.lines()?.iter().any(|l| l["ev"] == "hello") {
                return Ok(());
            }
            if self.probe.as_ref().is_some_and(JoinHandle::is_finished) {
                return Err("the probe ended before it attached".into());
            }
            if Instant::now() > deadline {
                return Err(format!("the probe did not attach within {ATTACH_S} s"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Stop the probe through its stop file, copy its output into the
    /// bundle, and remove its folder. Safe to call when nothing runs.
    fn stop_probe(&mut self) -> bool {
        let _ = self.windows.write(&rel("stop"), b"");
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
        let _ = self.windows.powershell(
            "Stop-Process -Name link-probe -ErrorAction SilentlyContinue\nexit 0",
            &[],
        );
        let mut joined = true;
        if let Some(probe) = self.probe.take() {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !probe.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
            joined = probe.is_finished();
        }
        for file in ["probe.jsonl", "probe.err"] {
            if let Ok(Some(bytes)) = self.windows.read(&rel(file)) {
                let _ = std::fs::write(self.folder.join(format!("windows-{file}")), bytes);
            }
        }
        joined && self.windows.remove_tree(DIR).is_ok()
    }

    /// Ask the probe for a dump of the mirror.
    fn mirror(&self) -> Result<Vec<u8>, String> {
        self.windows.remove_file(&rel("mirror.json"))?;
        self.windows.write(&rel("mirror.req"), b"")?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(bytes) = self.windows.read(&rel("mirror.json"))?
                && !bytes.is_empty()
            {
                return Ok(bytes);
            }
            if Instant::now() > deadline {
                return Err("the probe wrote no mirror".into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// The last `bytes` of a file in the profile.
    fn file_tail(&self, path: &str, bytes: u64) -> Result<Vec<u8>, String> {
        let o = self
            .windows
            .powershell(TAIL_SCRIPT, &[path, &bytes.to_string()])?;
        match o.code {
            Some(0) => Ok(o.stdout),
            code => Err(format!("{path}: exit {code:?}: {}", o.stderr)),
        }
    }

    /// Wait until the mirror holds as many rows as DCS holds objects, from
    /// line `mark` on. Returns whether it filled in time and the seconds.
    fn fill(&self, mark: usize, births: usize) -> Result<(bool, f64), String> {
        let last = self.reports(mark)?.last().cloned().unwrap_or(Value::Null);
        let limit = fill_limit(&last, births, 1.0);
        let start = Instant::now();
        loop {
            let live = self.live_count()?;
            let rows = serde_json::from_slice::<Value>(&self.mirror()?)
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

    /// The mirror against the oracle, as the docker-wine mirror part does.
    fn compare(&self, label: &str) -> Result<(Value, Value), String> {
        let read = |file: &str| {
            std::fs::read_to_string(self.repository.join(file)).map_err(|e| format!("{file}: {e}"))
        };
        self.eval(
            "server",
            "s11-early",
            &read("lua/sampling/oracle-early.lua")?,
            120,
        )?;
        self.hold(EARLY_S)?;
        // The oracle first: over SSH its eval returns 1.6 s or more after
        // the mirror's dump, and a moving row then reads older than twice its
        // period (LIVE, S11). Ages are compared as absolute differences, so
        // a mirror dumped after the oracle is judged fairly.
        let oracle = self.eval(
            "server",
            "s11-oracle",
            &read("lua/sampling/oracle.lua")?,
            120,
        )?;
        let mirror = self.mirror().map_err(|e| format!("{label}: {e}"))?;
        let m = self.folder.join(format!("mirror-{label}.json"));
        let o = self.folder.join(format!("oracle-{label}.json"));
        std::fs::write(&m, &mirror).map_err(|e| format!("{}: {e}", m.display()))?;
        std::fs::write(&o, oracle.to_string()).map_err(|e| format!("{}: {e}", o.display()))?;
        let tail = self.file_tail(&rel("recording.jsonl"), TAIL_BYTES)?;
        let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
        let r = self.folder.join(format!("recording-{label}.jsonl"));
        std::fs::write(&r, &tail[start..]).map_err(|e| format!("{}: {e}", r.display()))?;
        let verdict = super::compare_files(
            &o,
            &m,
            super::Judge {
                ages: true,
                strict: false,
                recording: Some(&r),
                positions_by_window: false,
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

    /// One size: spawn `births` objects, wait for the fill, measure the
    /// frame rate, collect reports for [`WINDOW_S`] seconds after the fill,
    /// compare, and summarize.
    fn stage(&self, size: usize, births: usize, label: &str) -> Result<Value, String> {
        let spawn_mark = self.lines()?.len();
        let spawned = self.spawn(births, label)?;
        let filled = self.fill(spawn_mark, births)?;
        let mark = self.lines()?.len();
        let from_fill = Instant::now();
        let fps = self.fps_window(&size.to_string(), 30, 60)?;
        while from_fill.elapsed() < Duration::from_secs(WINDOW_S) {
            std::thread::sleep(Duration::from_secs(5));
        }
        let window = self.reports(mark)?;
        let burst = self.reports(spawn_mark)?;
        let settled = span(&window);
        let burst_span = span(&burst);
        let tail = tail(&window);
        let (verdict, mirror) = self.compare(&size.to_string())?;
        let checks = stage_checks(
            size,
            filled,
            &settled,
            &tail,
            verdict["matches"] == json!(true),
        );
        let ok = checks.iter().all(|c| c["ok"] == json!(true));
        Ok(json!({
            "ok": ok,
            "checks": checks,
            "spawned": spawned,
            "moving_share": MOVING,
            "fill_s": filled.1,
            "fps": fps,
            "objects": self.live_count()?,
            "span_settled": settled.1,
            "span_from_spawn": burst_span.1,
            "worst_position_s_while_filling": worst_position(&burst),
            "tail": tail,
            "steps": steps(&window),
            "periods": mirror["periods"],
            "verdict": verdict,
            "reports": window,
        }))
    }
}

/// The measurement proper: deploy, start DCS, host, attach, two sizes.
fn measure(win: &mut Win, name: &str, target: &Target, probe: &Path) -> Result<Value, String> {
    let Target::WindowsSsh {
        profile,
        start_task,
        stop_task,
        ..
    } = target
    else {
        return Err("sampling --part windows runs on a windows-ssh target".into());
    };
    println!("== deploy");
    crate::deploy(name, None, true, false)?;
    // The build is undeployed at the end of the run: read it now.
    let build = crate::open(name)
        .and_then(|p| crate::install::manifest(p.as_ref()))
        .map(|m| m.active);
    let toml = win
        .windows
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let config = nanoprobe_agent_link::config::AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    if config.transport.map_path.is_empty() {
        return Err("agent.toml names no map_path: add one to the target profile".into());
    }
    let token = format!(
        "{}\\{}",
        profile.trim_end_matches('\\'),
        TOKEN.replace('/', "\\")
    );
    println!("== start DCS through its task");
    live::restart(&win.windows, 0, false, || {
        win.windows.restart_tasks(start_task, stop_task)
    })?;
    println!("== host a private session");
    link::host_session(&win.windows)?;
    println!("== attach the probe");
    win.start_probe(
        probe,
        &config.transport.map_path,
        &config.transport.port.to_string(),
        &token,
    )?;
    win.hold(5.0)?;
    let first = win.stage(1200, 1200, "b")?;
    let second = win.stage(2000, 800, "c")?;
    // The probe's last `final` line is read after it stops, so the pump
    // histogram covers both sizes.
    let all_reports = win.reports(0)?;
    Ok(json!({
        "agent_build": build.map_or_else(|e| json!({ "error": e }), |b| json!(b)),
        "moving_share": MOVING,
        "at_1200": first,
        "at_2000": second,
        "steps": steps(&all_reports),
        "tail": tail(&all_reports),
    }))
}

/// `sampling <name> --part windows`.
pub fn run(repository: &Path, name: &str) -> Result<ExitCode, String> {
    let target = crate::target(name)?;
    let Target::WindowsSsh { ssh, profile, .. } = &target else {
        return Err("sampling --part windows runs on a windows-ssh target".into());
    };
    let windows = Windows::connect(ssh, profile)?;
    if windows.dcs_processes()? > 0 {
        return Err(format!(
            "DCS is running on {name}: it may be in use. Close it first; the run starts DCS \
             through the start task and closes it through the stop task."
        ));
    }
    let template = std::fs::read(repository.join("tests/fixtures/templates/caucasus.miz"))
        .map_err(|e| format!("tests/fixtures/templates/caucasus.miz: {e}"))?;
    miz::generate(&template, &miz::Changes::default())?;
    let probe = link::build_for(
        "x86_64-pc-windows-msvc",
        true,
        "target/windows-tools",
        ".exe",
        &[("nanoprobe-fake-agent", "link-probe")],
    )?
    .pop()
    .ok_or("link-probe was not built")?;
    let folder = bundle::folder().join(format!("sampling-{name}-{}", bundle::stamp()));
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let before = listings(&windows)?;

    let mut win = Win {
        windows: windows.clone(),
        folder: folder.clone(),
        repository: repository.to_path_buf(),
        probe: None,
    };
    let started = bundle::utc_now();
    println!("== part: windows, release build");
    let result = measure(&mut win, name, &target, &probe);

    // Put the PC back whatever happened: the probe stopped and its folder
    // gone, no kill switch or reload request, DCS closed through its stop
    // task, and the build undeployed.
    println!("== restore");
    let mut restore = serde_json::Map::new();
    restore.insert("probe_stopped".into(), json!(win.stop_probe()));
    let _ = windows.remove_file("nanoprobe/disable");
    let _ = windows.remove_file("nanoprobe/reload");
    if let Target::WindowsSsh {
        ssh,
        profile,
        stop_task,
        ..
    } = &target
    {
        let closed = if windows.dcs_processes().unwrap_or(1) > 0 {
            link::close_dcs(ssh, profile, stop_task)
        } else {
            Ok(())
        };
        restore.insert(
            "dcs_closed".into(),
            closed.map_or_else(|e| json!({ "error": e }), |()| json!(true)),
        );
    }
    let undeployed = crate::undeploy(name);
    restore.insert(
        "undeployed".into(),
        undeployed.map_or_else(|e| json!({ "error": e }), |_| json!(true)),
    );
    let diff = listings(&windows).map(|after| listing_diff(&before, &after));
    restore.insert(
        "profile_diff".into(),
        diff.map_or_else(|e| json!({ "error": e }), |d| json!(d)),
    );

    // The machine is back as found: DCS closed, the build undeployed and no
    // file added or missing in the profile. A run that leaves it otherwise
    // fails.
    let restored = restore.get("dcs_closed").is_none_or(|v| v == &json!(true))
        && restore.get("undeployed") == Some(&json!(true))
        && restore.get("profile_diff") == Some(&json!(Vec::<String>::new()));
    restore.insert("restored".into(), json!(restored));
    let mut out = json!({ "target": name, "started": started, "restore": restore });
    let measured = match &result {
        Ok(v) => {
            out["windows"] = v.clone();
            v["at_1200"]["ok"] == json!(true) && v["at_2000"]["ok"] == json!(true)
        }
        Err(e) => {
            out["error"] = json!(e);
            false
        }
    };
    let ok = measured && restored;
    // The DCS build, the platform and the date; the agent build is in
    // `windows`.
    match crate::facts(&target) {
        Ok(facts) => {
            out["platform"] = json!(facts.kind);
            out["dcs"] = json!(facts.dcs);
            out["wine"] = json!(facts.wine);
        }
        Err(e) => out["facts_error"] = json!(e),
    }
    // The probe's final histogram, read from the copy in the bundle.
    if let Ok(bytes) = std::fs::read(folder.join("windows-probe.jsonl")) {
        let pump = pump(&link::parse_lines(&bytes));
        if out.get("windows").is_some() {
            out["windows"]["pump"] = pump;
        }
    }
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
    result?;
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
    fn the_probe_runs_with_the_recording_stop_file_and_mirror_in_its_folder() {
        let script = probe_script();
        for flag in [
            "--record",
            "--stop-file",
            "--mirror",
            "--token-file",
            "--map",
        ] {
            assert!(script.contains(flag), "{flag}");
        }
        assert!(script.contains("nanoprobe/dev/sampling"));
        assert!(script.contains("-Wait"));
    }

    #[test]
    fn a_fill_may_take_twice_the_births_expected_time() {
        // 800 births at 37 µs against a quarter of the 0.3 % share.
        let limit = fill_limit(&Value::Null, 800, 1.0);
        assert!((limit - 2.0 * 800.0 * 37.0 / 750.0).abs() < 1e-9, "{limit}");
        // Never under 20 s.
        assert_eq!(fill_limit(&Value::Null, 1, 1.0), 20.0);
        // A report's own birth cost is used.
        let slow = json!({ "cost_births": 75.0 });
        assert!(fill_limit(&slow, 800, 1.0) > limit);
    }

    fn listing(dir: &str, names: &[&str]) -> (String, Vec<String>) {
        (
            dir.to_owned(),
            names.iter().map(|n| (*n).to_owned()).collect(),
        )
    }

    #[test]
    fn the_listing_diff_names_what_was_added_and_what_went() {
        let before = BTreeMap::from([listing("", &["Config"]), listing("Scripts", &["Hooks"])]);
        let after = BTreeMap::from([
            listing("", &["Config", "nanoprobe"]),
            listing("Scripts", &[]),
        ]);
        assert_eq!(
            listing_diff(&before, &after),
            ["added: <profile>/nanoprobe", "missing: Scripts/Hooks"]
        );
        assert!(listing_diff(&before, &before).is_empty());
    }

    #[test]
    fn a_stage_needs_three_minutes_of_reports_and_a_match() {
        let tail = json!({ "p99_within_250": true });
        let span = |real: f64| (true, json!({ "real_s": real, "mean": 0.002 }));
        let ok = |real: f64, matches: bool| {
            stage_checks(1200, (true, 40.0), &span(real), &tail, matches)
                .iter()
                .all(|c| c["ok"] == json!(true))
        };
        assert!(ok(190.0, true));
        assert!(!ok(120.0, true));
        assert!(!ok(190.0, false));
    }
}
