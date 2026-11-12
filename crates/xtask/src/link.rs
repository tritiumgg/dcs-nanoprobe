//! `link <name>`: the S7 exit test on a docker-wine target.
//!
//! The target runs the deployed build, with the eval channel (`deploy
//! --dev`). The test copies two static Linux programs into the container:
//! `link-probe` (crates/fake-agent), which attaches as the sidecar and prints
//! what it reads with wall times, and `nanoprobe` (crates/cli), the inspector.
//! Both run in the container, so they share DCS's network and its
//! `/dev/shm`. Then, in order:
//!
//! 1. the probe attaches and its command is answered;
//! 2. `nanoprobe inspect` shows the handshake in the file;
//! 3. `lua/live/reloads.lua` reloads the mission N times through the eval
//!    channel; the probe times how long after each mission runs it reads
//!    the agent's `LIFECYCLE` saying so;
//! 4. a shim reload request, which the sidecar sees as `shim reloaded`;
//! 5. the kill switch: the time from creating `disable` to `DISABLED` in the
//!    status block, read in the container on one clock;
//! 6. the kill switch is removed, DCS restarted, and the programs removed.
//!
//! The pass rules are the plan's: the handshake visible, every reload read
//! within 2 s, the kill switch within 2 s.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use nanoprobe_agent_link::config::AgentToml;
use serde_json::{Value, json};

use crate::install::{self, AGENT_TOML, Profile, RUNNING, TOKEN};
use crate::profile::Target;
use crate::{bundle, eval, live, remote, ssh};

/// Where the programs go in the container.
const DIR: &str = "/tmp/nanoprobe-link";
/// Create the file `$1` and print the wall time just before, in milliseconds
/// since the Unix epoch, on the container's clock.
const KILL_SWITCH: &str = r#"python3 -c 'import sys, time; t = int(time.time() * 1000); open(sys.argv[1], "w").close(); print(t)' "$1""#;

/// The longest one mission load may take before the reload step gives up.
/// A limit, not an expectation: load times vary with hardware and mission.
const PER_LOAD_S: u64 = 300;
/// The pass limit for a reload and for the kill switch.
const LIMIT_MS: u64 = 2000;

/// `link <name> [--reloads N]`.
pub fn command(name: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let reloads: u32 = match rest {
        [] => 10,
        ["--reloads", n] => n.parse().map_err(|_| format!("--reloads {n:?}"))?,
        _ => return Err(format!("unknown options: {}", rest.join(" "))),
    };
    let target = crate::target(name)?;
    let Target::DockerWine {
        ssh: destination,
        restart,
        ..
    } = &target
    else {
        return Err("link needs a docker-wine target; Windows runs at the checkpoints".into());
    };
    let restart = restart
        .clone()
        .ok_or_else(|| format!("target {name:?} has no `restart` key"))?;
    let (remote, _, _) = crate::remote(name)?;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }

    let toml = remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target: add `map_path` to the profile and deploy")?;
    let config = AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    let map = linux_path(&config.transport.map_path)?;
    let port = config.transport.port.to_string();
    let token = remote.full_path(TOKEN);

    // The running DCS must have loaded the deployed build, or the test
    // measures another one. Its agent.toml and token are read at start too.
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

    let (probe, inspector) = build()?;
    upload(&remote, &probe, "link-probe")?;
    upload(&remote, &inspector, "nanoprobe")?;
    let result = run(&remote, &map, &port, &token, &active, reloads);
    // A failed run keeps what the probe printed, read before the clean-up
    // removes it.
    let mut report = result.unwrap_or_else(|e| {
        json!({
            "verdict": { "pass": false },
            "error": e,
            "probe": lines(&remote).unwrap_or_default(),
        })
    });
    report["probe_stderr"] = json!(
        remote
            .shell(
                &format!("cat {DIR}/probe.err 2>/dev/null || true"),
                &[],
                b""
            )
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    );
    report["build"] = json!(active);
    // Put the target back whatever happened: no kill switch, no request
    // left, DCS restarted into the build without its kill switch, and the
    // programs gone.
    let _ = remote.remove_file("nanoprobe/disable");
    let _ = remote.remove_file("nanoprobe/reload");
    let restarted = live::restart(&remote, remote.players().unwrap_or(1), true, || {
        let output = ssh::run(destination, &restart, b"")?;
        match output.code {
            Some(0) => Ok(()),
            _ => Err(format!("the restart command failed: {}", output.stderr)),
        }
    });
    // The folder goes first: a pattern pkill matches is also in this
    // shell's own command line, which it may end.
    let cleaned = remote.shell(
        &format!("rm -rf {DIR}; pkill -f '[l]ink-probe --map' || true"),
        &[],
        b"",
    );
    report["cleaned"] = json!(matches!(cleaned, Ok(ref o) if o.code == Some(0)));
    if let Some(e) = report.get("error") {
        println!("failed: {}", e.as_str().unwrap_or_default());
    }
    report["restored"] = match restarted {
        Ok(()) => json!(true),
        Err(e) => json!({ "error": e }),
    };
    report["target"] = json!(name);
    match crate::facts(&target) {
        Ok(facts) => {
            report["dcs"] = json!(facts.dcs);
            report["wine"] = json!(facts.wine);
        }
        Err(e) => report["dcs"] = json!({ "error": e }),
    }
    report["time"] = json!(bundle::utc_now());

    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("link-{name}-{}.json", bundle::stamp()));
    std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report["verdict"]).unwrap_or_default()
    );
    println!("wrote {}", path.display());
    Ok(if report["verdict"]["pass"] == json!(true) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The Linux path of a Windows path on Wine's `Z:` drive, which is the
/// container's root.
fn linux_path(windows: &str) -> Result<String, String> {
    let rest = windows
        .strip_prefix("Z:")
        .or_else(|| windows.strip_prefix("z:"))
        .ok_or_else(|| {
            format!("map_path {windows:?} is not on Z:, so the container cannot reach it")
        })?;
    Ok(rest.replace('\\', "/"))
}

/// Build the probe and the inspector as static Linux programs.
fn build() -> Result<(PathBuf, PathBuf), String> {
    let repo = crate::repository();
    let target_dir = repo.join("target/linux");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
            "-p",
            "nanoprobe-fake-agent",
            "--bin",
            "link-probe",
            "-p",
            "nanoprobe-cli",
            "--bin",
            "nanoprobe",
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(&repo)
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("the Linux build failed ({status})"));
    }
    let out = target_dir.join("x86_64-unknown-linux-musl/release");
    Ok((out.join("link-probe"), out.join("nanoprobe")))
}

fn upload(remote: &remote::Remote, local: &Path, name: &str) -> Result<(), String> {
    let bytes = std::fs::read(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let script = format!(
        "set -e; mkdir -p {DIR}; cat > {DIR}/$1.partial; chmod 755 {DIR}/$1.partial; mv -f {DIR}/$1.partial {DIR}/$1"
    );
    let output = remote.shell(&script, &[name], &bytes)?;
    match output.code {
        Some(0) => Ok(()),
        code => Err(format!("upload {name}: exit {code:?}: {}", output.stderr)),
    }
}

/// The probe's lines so far.
fn lines(remote: &remote::Remote) -> Result<Vec<Value>, String> {
    let output = remote.shell(
        &format!("cat {DIR}/probe.jsonl 2>/dev/null || true"),
        &[],
        b"",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Poll the probe's lines every half second until `done` holds, or `limit`.
fn wait_for(
    remote: &remote::Remote,
    what: &str,
    limit: Duration,
    done: impl Fn(&[Value]) -> bool,
) -> Result<Vec<Value>, String> {
    let start = Instant::now();
    loop {
        let l = lines(remote)?;
        if done(&l) {
            return Ok(l);
        }
        if start.elapsed() >= limit {
            return Err(format!(
                "{what}: not seen within {} s; the probe printed:\n{}",
                limit.as_secs(),
                l.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn ev<'a>(l: &'a [Value], name: &str) -> impl Iterator<Item = &'a Value> {
    let name = name.to_owned();
    l.iter().filter(move |e| e["ev"] == json!(name))
}

fn run(
    remote: &remote::Remote,
    map: &str,
    port: &str,
    token: &str,
    active: &str,
    reloads: u32,
) -> Result<Value, String> {
    // The probe runs in the background in the container and writes its
    // lines to a file; each step reads the file. It runs until the kill
    // switch, or the clean-up's pkill: a load has no fixed duration, so no
    // step's time is known in advance.
    let seconds = "86400";
    let start = remote.shell(
        &format!(
            "nohup {DIR}/link-probe --map \"$1\" --port \"$2\" --token-file \"$3\" --seconds \"$4\" \
             > {DIR}/probe.jsonl 2> {DIR}/probe.err < /dev/null &"
        ),
        &[map, port, token, seconds],
        b"",
    )?;
    if start.code != Some(0) {
        return Err(format!("could not start the probe: {}", start.stderr));
    }

    // 1. The handshake. The probe announces campaign 7; its command for
    // campaign 7 is answered UNKNOWN_OPCODE (20), and its command for
    // campaign 8 WRONG_CAMPAIGN (24).
    let answered = |l: &[Value], seq: u64, code: u64| {
        ev(l, "result").any(|e| e["seq"] == json!(seq) && e["code"] == json!(code))
    };
    let l = wait_for(
        remote,
        "the answered commands",
        Duration::from_secs(60),
        |l| answered(l, 1, 20) && answered(l, 2, 24),
    )?;
    let hello = ev(&l, "hello").next().cloned().unwrap_or(Value::Null);
    println!("attached: {hello}");

    // 2. The inspector, while attached.
    let inspect = remote.shell(
        &format!("{DIR}/nanoprobe inspect \"$1\" --records 5"),
        &[map],
        b"",
    )?;
    let inspect_text = String::from_utf8_lossy(&inspect.stdout).into_owned();
    let handshake_seen = inspect.code == Some(0)
        && inspect_text.contains("ATTACHED")
        && hello["ev"] == json!("hello");
    println!("{inspect_text}");

    // 3. The reloads.
    let profile: &dyn Profile = remote;
    let repo = crate::repository();
    let source = std::fs::read_to_string(repo.join("lua/live/reloads.lua"))
        .map_err(|e| format!("lua/live/reloads.lua: {e}"))?;
    if !source.contains("local RELOADS = 10\n") {
        return Err("lua/live/reloads.lua no longer sets `local RELOADS = 10`".into());
    }
    let source = source.replace(
        "local RELOADS = 10\n",
        &format!("local RELOADS = {reloads}\n"),
    );
    let gen_before = ev(&lines(remote)?, "status")
        .filter_map(|e| e["gen"].as_u64())
        .max()
        .unwrap_or(0);
    let eval_result = eval::run(
        profile,
        "hook",
        "s7-reloads",
        source.as_bytes(),
        // The snippet's own limit, RELOADS * PER_LOAD_S, and a minute more.
        Duration::from_secs(u64::from(reloads) * PER_LOAD_S + 60),
    );
    match &eval_result {
        Ok(_) => println!("reloads done"),
        Err(e) => println!("reloads: {e}"),
    }
    let last_gen = gen_before + u64::from(reloads);
    wait_for(
        remote,
        "the last reload read",
        Duration::from_secs(30),
        |l| {
            ev(l, "lifecycle").any(|e| {
                e["gen"].as_u64() == Some(last_gen) && matches!(e["state"].as_u64(), Some(3 | 4))
            })
        },
    )?;

    // 4. A shim reload request.
    profile.write("nanoprobe/reload", b"")?;
    let shim_reloaded = wait_for(remote, "the shim reload", Duration::from_secs(10), |l| {
        ev(l, "log").any(|e| e["target"] == json!("hook") && e["msg"] == json!("shim reloaded"))
    })
    .is_ok();

    // 5. The kill switch: the time it was created, on the container's clock.
    let disable = remote.full_path("nanoprobe/disable");
    let created = remote.shell(KILL_SWITCH, &[&disable], b"")?;
    let created_ms: u64 = String::from_utf8_lossy(&created.stdout)
        .trim()
        .parse()
        .map_err(|_| format!("could not create the kill switch: {}", created.stderr))?;
    let l = wait_for(remote, "the probe's end", Duration::from_secs(30), |l| {
        ev(l, "end").next().is_some()
    })?;
    let disabled_ms = ev(&l, "status")
        .find(|e| e["lifecycle"] == json!(6))
        .and_then(|e| e["t"].as_u64());
    let frame_at_disable = ev(&l, "status")
        .find(|e| e["lifecycle"] == json!(6))
        .and_then(|e| e["frame"].as_u64());
    let kill_ms = disabled_ms.map(|t| t.saturating_sub(created_ms));
    // The pump stopped: the frame the probe read last is the one that
    // stored DISABLED.
    let final_frame = ev(&l, "final").next().and_then(|e| e["frame"].as_u64());
    let pump_stopped = final_frame.is_some() && final_frame == frame_at_disable;
    let after = remote.shell(
        &format!("{DIR}/nanoprobe inspect \"$1\" --records 0"),
        &[map],
        b"",
    )?;
    let after_text = String::from_utf8_lossy(&after.stdout).into_owned();

    let reconnects: Vec<Value> = ev(&l, "reconnect")
        .filter(|e| e["gen"].as_u64().is_some_and(|g| g > gen_before))
        .cloned()
        .collect();
    let worst = reconnects
        .iter()
        .filter_map(|e| e["ms"].as_u64())
        .max()
        .unwrap_or(u64::MAX);
    let reports: Vec<Value> = ev(&l, "report").cloned().collect();
    // The link stays attached through every load: one attach, and no detach
    // before the kill switch.
    let attaches = ev(&l, "attached").count();
    let early_detaches = reports
        .iter()
        .filter(|e| {
            e["text"].as_str().is_some_and(|s| s.contains("detach"))
                && e["t"].as_u64().is_some_and(|t| t < created_ms)
        })
        .count();
    // The kill switch's reason and last LIFECYCLE reach the sidecar.
    let reason_read = ev(&l, "log").any(|e| e["msg"] == json!("disabled: the kill switch"));
    let disabled_read = ev(&l, "lifecycle").any(|e| e["state"] == json!(6));
    let build_matches = hello["build"] == json!(active);
    let pass = handshake_seen
        && build_matches
        && reason_read
        && disabled_read
        && attaches == 1
        && early_detaches == 0
        && eval_result.is_ok()
        && reconnects.len() == reloads as usize
        && worst < LIMIT_MS
        && kill_ms.is_some_and(|ms| ms < LIMIT_MS)
        && pump_stopped
        && shim_reloaded;
    Ok(json!({
        "verdict": {
            "pass": pass,
            "handshake_in_inspector": handshake_seen,
            "reloads": reloads,
            "reloads_read": reconnects.len(),
            "slowest_reload_ms": worst,
            "attaches": attaches,
            "detaches_before_kill_switch": early_detaches,
            "shim_reloaded": shim_reloaded,
            "kill_switch_ms": kill_ms,
            "pump_stopped": pump_stopped,
            "kill_switch_reason_read": reason_read,
            "disabled_lifecycle_read": disabled_read,
            "build_matches": build_matches,
        },
        "hello": hello,
        "reconnects": reconnects,
        "reports": reports,
        "kill_switch": {
            "created_ms": created_ms,
            "disabled_ms": disabled_ms,
            "frame_at_disable": frame_at_disable,
            "final_frame": final_frame,
        },
        "eval": match eval_result { Ok(v) => v, Err(e) => json!({ "error": e }) },
        "inspect_attached": inspect_text,
        "inspect_disabled": after_text,
        "probe": l,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_z_drive_path_is_the_containers_root() {
        assert_eq!(
            linux_path(r"Z:\dev\shm\nanoprobe.map").unwrap(),
            "/dev/shm/nanoprobe.map"
        );
        assert!(linux_path(r"C:\users\x.map").is_err());
    }

    /// The kill-switch script, run as `Remote::shell` runs it: `sh -c` with
    /// the path as `$1`. Python runs in the game container, not here, so a
    /// stand-in `python3` first on PATH checks that the path reaches it as
    /// its argument, and creates the file and prints a time as the real
    /// script does.
    #[cfg(unix)]
    #[test]
    fn the_kill_switch_script_passes_its_path_to_python() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("nanoprobe-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stand_in = dir.join("python3");
        std::fs::write(
            &stand_in,
            "#!/bin/sh\n[ \"$1\" = -c ] || exit 2\n: > \"$3\" && echo 1794000000000\n",
        )
        .unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("disable");
        let search = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("sh")
            .args(["-c", KILL_SWITCH, "sh"])
            .arg(&path)
            .env("PATH", search)
            .output()
            .unwrap();
        assert!(path.exists(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1794000000000");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
