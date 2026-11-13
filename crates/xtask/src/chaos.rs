//! `chaos host|<name> [--cycles N] [--seed HEX]`: the S8 chaos test
//! (spec/faults.md, "Chaos"). `chaos-runner` (crates/fake-agent) kills DCS
//! or the sidecar N times (default 100), at random, and times each
//! reconnect.
//!
//! On `host`, DCS is `lua/chaos/host.lua`: the hook and the shim in the
//! strict mock with the host-native agent, the runner's child. Unix only.
//!
//! On a docker-wine target, the runner and `link-probe` are static Linux
//! programs run in the game container, beside DCS. The runner kills DCS with
//! `pkill -9`, and the container's own start loop brings it back. Before each
//! DCS kill the runner asks, and this side lets it go on only when no player
//! is connected (CLAUDE.local.md, the owner's rule). DCS stays up at least
//! 70 s between DCS kills, so the server's watchdog, which restarts the
//! container after several failed checks a minute apart, sees it up.
//!
//! The pass rule is the plan's: every reconnect under 2 s, no DCS crash, no
//! record or position the sidecar found wrong. After the run, an error record
//! in the agent's, the hook's or the shim's log, any record in the agent's
//! crash log, or a DCS crash file written during the run also fails it.
//! Writes `bundles/chaos-<name>-<time>.json`, and keeps the runner's folder:
//! on the host as `bundles/chaos-host-<time>/`, from a server as
//! `bundles/chaos-<name>-<time>.tar.gz`.

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use nanoprobe_agent_link::config::AgentToml;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::install::{self, AGENT_TOML, EVAL_HOOK, Profile, RUNNING, TOKEN};
use crate::link::{build_linux_bins, linux_path, parse_lines, upload_container};
use crate::profile::Target;
use crate::{bundle, remote};

/// Where the programs and their output go in the container.
const DIR: &str = "/tmp/nanoprobe-chaos";
/// Kills DCS in the container. The brackets keep the pattern from matching
/// the shell that runs it.
/// Only the runner's own user's processes: DCS runs as that user, and a
/// wrapper that started it may belong to root.
const DCS_KILL: &str = "pkill -9 -u \"$(id -u)\" -f '[D]CS_server[.]exe'";
/// Stop the runner and its probe: resume them first, since a stopped
/// process does not act on a TERM. The brackets keep the patterns from
/// matching the shell that runs them.
const STOP: &str = "pkill -CONT -f '[c]haos-runner --map' ; pkill -CONT -f '[l]ink-probe --map' ; \
pkill -f '[c]haos-runner --map' ; pkill -f '[l]ink-probe --map' ; true";
/// Seconds DCS runs between its reconnect and the next DCS kill on a
/// server.
const DCS_GAP_S: u64 = 70;
/// How long one DCS may take to come back and pump.
const DCS_LIMIT_S: u64 = 600;
/// How long a DCS kill may wait for the server to be empty.
const PLAYER_WAIT: Duration = Duration::from_secs(30 * 60);

struct Options {
    cycles: u64,
    seed: Option<String>,
}

fn options(rest: &[&str]) -> Result<Options, String> {
    let mut o = Options {
        cycles: 100,
        seed: None,
    };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--cycles" => {
                o.cycles = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--cycles takes a number")?;
            }
            "--seed" => {
                let v = it.next().ok_or("--seed takes hex digits")?;
                u64::from_str_radix(v, 16).map_err(|_| "--seed takes hex digits")?;
                o.seed = Some((*v).to_owned());
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

/// The agent's logs a run checks, under the profile. Each is read with the
/// file it rotated into, `<name>.1` (spec/log.md).
const LOGS: &[&str] = &[
    "nanoprobe/logs/agent.log",
    "nanoprobe/logs/hook.log",
    "nanoprobe/logs/shim.log",
    "nanoprobe/logs/crash.log",
];
/// How many consecutive failed polls of the server end the run: two minutes.
const POLL_FAILURES: u32 = 24;

/// Read every log in [`LOGS`] and its rotated file through `read`, a path
/// under the profile. `existed` names the logs that held records before the
/// run. Returns the failures (see [`log_failures`]) and how many times the
/// hook started since `since`. A log that cannot be read is a failure; one
/// that does not exist is not. A log that existed and holds no record from
/// before `since` lost its start to rotation. A start count that does not
/// match the kills shows a missing or rotated-away hook log.
fn read_logs(
    read: impl Fn(&str) -> Result<Option<Vec<u8>>, String>,
    existed: &[&str],
    since: &str,
) -> (Vec<String>, usize) {
    let mut failures = Vec::new();
    let mut starts = 0;
    for log in LOGS {
        let mut earliest: Option<String> = None;
        for path in [format!("{log}.1"), (*log).to_owned()] {
            match read(&path) {
                Ok(Some(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    failures.extend(log_failures(&path, &text, since));
                    if log.ends_with("hook.log") {
                        starts += records_since(&text, since)
                            .filter(|r| r["msg"] == json!("hook loaded"))
                            .count();
                    }
                    let first = text
                        .lines()
                        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                        .find_map(|r| r["wall"].as_str().map(str::to_owned));
                    if let Some(first) = first
                        && earliest.as_ref().is_none_or(|e| first < *e)
                    {
                        earliest = Some(first);
                    }
                }
                Ok(None) => {}
                Err(e) => failures.push(format!("{path}: {e}")),
            }
        }
        let before = |w: &str| w.get(..19).unwrap_or(w) < since.get(..19).unwrap_or(since);
        if existed.contains(log) && !earliest.as_deref().is_some_and(before) {
            failures.push(format!(
                "{log}: no record from before the run; its first records were rotated away"
            ));
        }
    }
    (failures, starts)
}

/// The JSON records of a log from `since` on (RFC 3339 UTC, the same clock
/// as the log).
pub(crate) fn records_since<'a>(text: &'a str, since: &'a str) -> impl Iterator<Item = Value> + 'a {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(move |r| {
            let wall = r["wall"].as_str().unwrap_or_default();
            wall.get(..19).unwrap_or(wall) >= since.get(..19).unwrap_or(since)
        })
}

/// The records in a log that fail a run: from `since` on (RFC 3339 UTC, the
/// same clock as the log), every record of `crash.log`, and the error
/// records of the others (spec/log.md).
fn log_failures(name: &str, text: &str, since: &str) -> Vec<String> {
    let crash = name.contains("crash.log");
    records_since(text, since)
        .filter(|r| crash || r["level"] == json!("error"))
        .map(|r| format!("{name}: {}", r["msg"].as_str().unwrap_or_default()))
        .collect()
}

/// What code, toolchain and machine ran the test.
pub(crate) fn provenance() -> Value {
    let run = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .current_dir(crate::repository())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    let dirty = !run("git", &["status", "--porcelain"]).is_empty();
    // On uncommitted work, a hash of the difference from the commit and of
    // every untracked file names the code that ran.
    let changes = dirty.then(|| {
        let mut h = Sha256::new();
        h.update(run("git", &["diff", "HEAD", "--binary"]));
        for file in run("git", &["ls-files", "--others", "--exclude-standard"]).lines() {
            h.update(file);
            h.update(std::fs::read(crate::repository().join(file)).unwrap_or_default());
        }
        format!("{:x}", h.finalize())
    });
    json!({
        "commit": run("git", &["rev-parse", "HEAD"]),
        "dirty": dirty,
        "changes_sha256": changes,
        "rustc": run("rustc", &["-V"]),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
}

pub fn command(name: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let o = options(rest)?;
    let stamp = bundle::stamp();
    let (lines, mut extra) = if name == "host" {
        host(&o, &stamp)?
    } else {
        server(name, &o, &stamp)?
    };
    extra["code"] = provenance();
    let end = lines
        .iter()
        .rev()
        .find(|l| l["ev"] == json!("end"))
        .cloned();
    // Each DCS kill starts the hook once more; on the host the first start
    // is in the run too.
    if let Some(kills) = end.as_ref().and_then(|e| e["dcs_kills"].as_u64()) {
        let expected = kills + u64::from(name == "host");
        let starts = extra["hook_starts"].as_u64().unwrap_or(0);
        if starts != expected
            && let Some(f) = extra["log_failures"].as_array_mut()
        {
            f.push(json!(format!(
                "hook.log shows {starts} start(s) in the run, not {expected}: a log is missing or rotated away"
            )));
        }
    }
    let logs_clean = extra["log_failures"]
        .as_array()
        .is_some_and(|a| a.is_empty());
    let fails: Vec<&Value> = lines.iter().filter(|l| l["ev"] == json!("fail")).collect();
    let pass = end.is_some() && fails.is_empty() && logs_clean && extra.get("error").is_none();
    let mut report = json!({
        "verdict": { "pass": pass, "end": end, "failures": fails },
        "target": name,
        "time": bundle::utc_now(),
        "runner": lines,
    });
    if let (Some(r), Some(e)) = (report.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            r.insert(k.clone(), v.clone());
        }
    }
    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("chaos-{name}-{stamp}.json"));
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report["verdict"]).unwrap_or_default()
    );
    println!("wrote {}", path.display());
    Ok(if pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Print a runner line, except the frequent `kill` lines.
fn show(line: &Value) {
    if line["ev"] != json!("kill") {
        println!("{line}");
    }
}

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

fn build(args: &[&str]) -> Result<(), String> {
    let status = cargo()
        .args(args)
        .current_dir(crate::repository())
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {} failed", args.join(" ")))
    }
}

/// The host run: the mock as DCS, in `bundles/chaos-host-<stamp>/`.
fn host(o: &Options, stamp: &str) -> Result<(Vec<Value>, Value), String> {
    if cfg!(windows) {
        return Err("chaos host runs the mock under a POSIX shell: Linux or macOS".into());
    }
    let repo = crate::repository();
    build(&["build", "-p", "nanoprobe-agent", "--no-default-features"])?;
    build(&[
        "build",
        "--release",
        "-p",
        "nanoprobe-fake-agent",
        "--bin",
        "link-probe",
        "--bin",
        "chaos-runner",
    ])?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("target"));
    let module = ["libnanoprobe.so", "libnanoprobe.dylib"]
        .iter()
        .map(|f| target.join("debug").join(f))
        .find(|p| p.exists())
        .ok_or("no host-native agent under target/debug")?;
    let since = bundle::utc_now();
    let dir = bundle::folder().join(format!("chaos-host-{stamp}"));
    std::fs::create_dir_all(dir.join("profile")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let token: String = (0..4)
        .map(|_| format!("{:016x}", nanoprobe_transport::random_nonzero()))
        .collect();
    std::fs::write(dir.join("token"), format!("{token}\n")).map_err(|e| e.to_string())?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("pick a port: {e}"))?
        .port()
        .to_string();
    let map = dir.join("map");
    let q = |p: &std::path::Path| crate::ssh::quote(&p.display().to_string());
    let dcs = format!(
        "lua {} {} {} {} {port} {token}",
        q(&repo.join("lua/chaos/host.lua")),
        q(&module),
        q(&dir.join("profile")),
        q(&map),
    );
    let release = target.join("release");
    let mut cmd = Command::new(release.join("chaos-runner"));
    cmd.arg("--map")
        .arg(&map)
        .arg("--dir")
        .arg(dir.join("run"))
        .args(["--cycles", &o.cycles.to_string()])
        .args(["--dcs-start", &dcs, "--dcs-limit", "30"]);
    if let Some(seed) = &o.seed {
        cmd.args(["--seed", seed]);
    }
    cmd.arg("--")
        .arg(release.join("link-probe"))
        .arg("--map")
        .arg(&map)
        .args(["--port", &port, "--token-file"])
        .arg(dir.join("token"))
        .args(["--seconds", "86400"])
        .current_dir(&repo)
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("chaos-runner: {e}"))?;
    let stdout = child.stdout.take().ok_or("no runner output")?;
    let mut lines = Vec::new();
    for text in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            show(&v);
            lines.push(v);
        }
    }
    let _ = child.wait();
    let profile = dir.join("profile");
    // The host's profile is new: no log existed before the run.
    let (failures, starts) = read_logs(
        |path| match std::fs::read(profile.join(path)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        },
        &[],
        &since,
    );
    Ok((
        lines,
        json!({
            "agent": "host-native, --no-default-features (no fault injection)",
            "kept": dir.display().to_string(),
            "log_failures": failures,
            "hook_starts": starts,
        }),
    ))
}

/// The server run, in the game container.
fn server(name: &str, o: &Options, stamp: &str) -> Result<(Vec<Value>, Value), String> {
    let target = crate::target(name)?;
    if !matches!(target, Target::DockerWine { .. }) {
        return Err("chaos runs on `host` or a docker-wine target".into());
    }
    let remote = crate::remote(name)?.0;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }
    let toml = remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target: add `map_path` to the profile and deploy")?;
    let config = AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    let map = linux_path(&config.transport.map_path)?;
    let token = remote.full_path(TOKEN);
    let port = config.transport.port.to_string();
    let active = install::manifest(&remote)?
        .active
        .ok_or("nothing is deployed on the target")?;
    let running = remote
        .read(RUNNING)?
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned());
    if running.as_deref() != Some(&format!("build={active}")) {
        return Err(format!(
            "DCS runs {}, not the deployed {active}: run `cargo xtask restart {name}` first",
            running.as_deref().unwrap_or("no hook")
        ));
    }
    // The run proves the shipping DLL, so it refuses one with fault
    // injection.
    let dll = remote.full_path(&format!(
        "nanoprobe/versions/{active}/nanoprobe_agent_{active}.dll"
    ));
    let marked = remote.shell("grep -a -q 'injected fault' \"$1\"", &[&dll], b"")?;
    match marked.code {
        Some(1) => {}
        Some(0) => {
            return Err(format!(
                "the deployed build {active} has fault injection: deploy without --faults and restart"
            ));
        }
        code => {
            return Err(format!(
                "could not check {dll} for fault injection: exit {code:?}: {}",
                marked.stderr
            ));
        }
    }
    let bins = build_linux_bins(&[
        ("nanoprobe-fake-agent", "link-probe"),
        ("nanoprobe-fake-agent", "chaos-runner"),
    ])?;
    // Nothing from an earlier, interrupted run may still run or be read as
    // this one's.
    remote.shell(&format!("{STOP}; rm -rf {DIR}"), &[], b"")?;
    let left = remote.shell(
        "pgrep -f '[c]haos-runner --map' ; pgrep -f '[l]ink-probe --map' ; true",
        &[],
        b"",
    )?;
    if !String::from_utf8_lossy(&left.stdout).trim().is_empty() {
        return Err("a runner or probe from an earlier run is still running".into());
    }
    upload_container(&remote, DIR, &bins[0], "link-probe")?;
    upload_container(&remote, DIR, &bins[1], "chaos-runner")?;
    // The start on the container's clock, which the logs use, and a file to
    // find DCS's files written after it.
    let since = remote
        .shell(
            &format!("touch {DIR}/start && date -u +%Y-%m-%dT%H:%M:%SZ"),
            &[],
            b"",
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())?;
    // The logs that already hold records, so a lost start can be told from
    // a log that is new.
    let paths: Vec<String> = LOGS.iter().map(|l| remote.full_path(l)).collect();
    let args: Vec<&str> = paths.iter().map(String::as_str).collect();
    let sized = remote.shell(
        "for f; do [ -s \"$f\" ] && echo y || echo n; done",
        &args,
        b"",
    )?;
    let existed: Vec<&str> = LOGS
        .iter()
        .zip(String::from_utf8_lossy(&sized.stdout).lines())
        .filter(|(_, y)| *y == "y")
        .map(|(l, _)| *l)
        .collect();
    let programs: Value = bins
        .iter()
        .map(|p| {
            let hash = std::fs::read(p)
                .map(|b| format!("{:x}", Sha256::digest(&b)))
                .unwrap_or_default();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, json!(hash))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();

    let result = run_server(&remote, o, &map, &port, &token);
    let mut extra = json!({
        "build": active,
        "dev_deploy": remote.read(EVAL_HOOK)?.is_some(),
        "programs": programs,
    });
    let lines = match result {
        Ok(lines) => lines,
        Err((e, lines)) => {
            println!("failed: {e}");
            extra["error"] = json!(e);
            lines
        }
    };
    // The logs, and any crash file DCS wrote in its Logs folder.
    let (mut failures, starts) = read_logs(|path| remote.read(path), &existed, &since);
    extra["hook_starts"] = json!(starts);
    let written = remote
        .shell(
            &format!("find \"$1\" -type f -newer {DIR}/start 2>/dev/null; true"),
            &[&remote.full_path("Logs")],
            b"",
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for file in written.lines() {
        let lower = file.to_ascii_lowercase();
        if lower.contains("crash") || lower.ends_with(".dmp") {
            failures.push(format!("DCS wrote {file}"));
        }
    }
    extra["log_failures"] = json!(failures);
    extra["dcs_logs_written"] = json!(written.lines().collect::<Vec<_>>());
    // Keep the runner's folder: every sidecar's output.
    if let Ok(out) = remote.shell(
        &format!(
            "cd {DIR} && tar -czf - $(for f in run runner.jsonl runner.err; do [ -e \"$f\" ] && echo \"$f\"; done)"
        ),
        &[],
        b"",
    )
        && out.code == Some(0)
    {
        let path = bundle::folder().join(format!("chaos-{name}-{stamp}.tar.gz"));
        if std::fs::create_dir_all(bundle::folder()).is_ok()
            && std::fs::write(&path, &out.stdout).is_ok()
        {
            extra["kept"] = json!(path.display().to_string());
        }
    }
    // Stop the programs and remove them. The folder goes first: a pattern
    // pkill matches is also in this shell's own command line.
    let cleaned = remote.shell(&format!("rm -rf {DIR}; {STOP}"), &[], b"");
    extra["cleaned"] = json!(matches!(cleaned, Ok(ref o) if o.code == Some(0)));
    // A run that ended with DCS down leaves it to be brought back.
    if extra.get("error").is_some() {
        extra["restored"] = match crate::restart(name) {
            Ok(_) => json!(true),
            Err(e) => json!({ "error": e }),
        };
    }
    match crate::facts(&target) {
        Ok(facts) => {
            extra["dcs"] = json!(facts.dcs);
            extra["wine"] = json!(facts.wine);
        }
        Err(e) => extra["dcs"] = json!({ "error": e }),
    }
    Ok((lines, extra))
}

/// Start the runner and follow it: answer its asks, until its `end` or its
/// exit. On a failure, returns what it printed so far.
fn run_server(
    remote: &remote::Remote,
    o: &Options,
    map: &str,
    port: &str,
    token: &str,
) -> Result<Vec<Value>, (String, Vec<Value>)> {
    let seed = o
        .seed
        .clone()
        .unwrap_or_else(|| format!("{:x}", nanoprobe_transport::random_nonzero()));
    let start = remote.shell(
        &format!(
            "nohup {DIR}/chaos-runner --map \"$1\" --dir {DIR}/run --cycles \"$2\" --seed \"$3\" \
             --dcs-kill \"$4\" --gate {DIR}/go --dcs-limit {DCS_LIMIT_S} --dcs-gap {DCS_GAP_S} \
             -- {DIR}/link-probe --map \"$1\" --port \"$5\" --token-file \"$6\" --seconds 86400 \
             > {DIR}/runner.jsonl 2> {DIR}/runner.err < /dev/null &"
        ),
        &[map, &o.cycles.to_string(), &seed, DCS_KILL, port, token],
        b"",
    );
    match start {
        Ok(ref out) if out.code == Some(0) => {}
        Ok(out) => {
            return Err((
                format!("could not start the runner: {}", out.stderr),
                vec![],
            ));
        }
        Err(e) => return Err((e, vec![])),
    }
    let mut shown = 0;
    let mut granted: Option<u64> = None;
    let mut asked_at: Option<Instant> = None;
    let mut failed_polls = 0;
    // The lines read so far, kept for a run that ends in an error.
    let mut last: Vec<Value> = Vec::new();
    loop {
        // A poll every 5 s also keeps the shared SSH connection open.
        std::thread::sleep(Duration::from_secs(5));
        let read = remote.shell(
            &format!("cat {DIR}/runner.jsonl; echo; pgrep -f '[c]haos-runner --map' >/dev/null && echo RUNNING"),
            &[],
            b"",
        );
        let out = match read {
            Ok(out) => {
                failed_polls = 0;
                out
            }
            Err(e) => {
                failed_polls += 1;
                if failed_polls >= POLL_FAILURES {
                    return Err((format!("{failed_polls} polls failed in a row: {e}"), last));
                }
                println!("poll failed, trying again: {e}");
                continue;
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let running = text.lines().any(|l| l == "RUNNING");
        let lines = parse_lines(&out.stdout);
        for l in &lines[shown.min(lines.len())..] {
            show(l);
        }
        shown = lines.len();
        last.clone_from(&lines);
        if lines.iter().any(|l| l["ev"] == json!("end")) || !running {
            if !running && !lines.iter().any(|l| l["ev"] == json!("end")) {
                let err = remote
                    .shell(&format!("cat {DIR}/runner.err"), &[], b"")
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
                return Err((format!("the runner stopped with no end: {err}"), lines));
            }
            return Ok(lines);
        }
        // The runner's latest ask, if this side has not answered it.
        let ask = lines
            .iter()
            .rev()
            .find(|l| l["ev"] == json!("ask"))
            .and_then(|l| l["cycle"].as_u64());
        if let Some(cycle) = ask
            && granted != Some(cycle)
        {
            let since = *asked_at.get_or_insert_with(Instant::now);
            match remote.players() {
                Ok(0) => {
                    if let Err(e) = remote.shell(&format!("touch {DIR}/go"), &[], b"") {
                        println!("could not answer the ask: {e}");
                        continue;
                    }
                    granted = Some(cycle);
                    asked_at = None;
                }
                Ok(n) if since.elapsed() < PLAYER_WAIT => {
                    println!("cycle {cycle}: {n} player(s) connected; waiting before killing DCS");
                }
                Ok(n) => {
                    return Err((
                        format!(
                            "cycle {cycle}: {n} player(s) connected for {PLAYER_WAIT:?}; stopped"
                        ),
                        lines,
                    ));
                }
                Err(e) if since.elapsed() < PLAYER_WAIT => {
                    println!("could not read the player list: {e}");
                }
                Err(e) => {
                    return Err((
                        format!(
                            "cycle {cycle}: could not read the player list for {PLAYER_WAIT:?}: {e}"
                        ),
                        lines,
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_after_the_start_fail_and_earlier_ones_do_not() {
        let text = [
            r#"{"wall":"2026-11-13T10:00:00Z","level":"error","target":"hook","msg":"before"}"#,
            r#"{"wall":"2026-11-13T10:05:00.123Z","level":"error","target":"agent","msg":"after"}"#,
            r#"{"wall":"2026-11-13T10:05:01Z","level":"info","target":"agent","msg":"fine"}"#,
            "not json",
        ]
        .join("\n");
        assert_eq!(
            log_failures("agent.log", &text, "2026-11-13T10:01:00Z"),
            vec!["agent.log: after".to_owned()]
        );
        let crash =
            r#"{"wall":"2026-11-13T10:05:00Z","level":"info","target":"agent","msg":"exception"}"#;
        assert_eq!(
            log_failures("nanoprobe/logs/crash.log", crash, "2026-11-13T10:01:00Z").len(),
            1
        );
    }

    #[test]
    fn a_log_whose_start_was_rotated_away_fails() {
        let after =
            r#"{"wall":"2026-11-13T10:05:00Z","level":"info","target":"hook","msg":"hook loaded"}"#;
        let before =
            r#"{"wall":"2026-11-13T09:00:00Z","level":"info","target":"hook","msg":"hook loaded"}"#;
        let since = "2026-11-13T10:01:00Z";
        let hook = "nanoprobe/logs/hook.log";
        let only = |text: String| {
            move |path: &str| {
                Ok(path
                    .ends_with("hook.log")
                    .then(|| text.clone().into_bytes()))
            }
        };
        let (failures, starts) = read_logs(only(format!("{after}\n")), &[hook], since);
        assert_eq!(starts, 1);
        assert!(
            failures.iter().any(|f| f.contains("rotated away")),
            "{failures:?}"
        );
        let (failures, starts) = read_logs(only(format!("{before}\n{after}\n")), &[hook], since);
        assert_eq!((failures.len(), starts), (0, 1), "{failures:?}");
        let (failures, _) = read_logs(only(format!("{after}\n")), &[], since);
        assert!(failures.is_empty(), "a new log: {failures:?}");
    }
}
