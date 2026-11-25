//! `sidecar <name> --part static-template|payload-preset|saved-files` on a
//! windows-ssh target: the same API test as on docker-wine, with the
//! product's sidecar a Windows program on the client. `saved-files` runs on
//! Windows only: it makes a static template and a payload preset a person
//! saved from the Mission Editor into the profile (`shell_live/saved_files.rs`),
//! read where they are from the folders `sidecar.toml` lists beside `own`.
//!
//! A Windows PC has a DCS client and no dedicated server. The run refuses
//! when DCS already runs (someone may be playing), deploys the agent with
//! the eval channel, starts DCS through the profile's start task, and hosts
//! a private session with a shell mission, as `sampling --part windows`
//! does. It then:
//!
//! 1. writes `nanoprobe-sidecar.exe`, built here for
//!    `x86_64-pc-windows-msvc`, its `sidecar.toml` and a tokens file into
//!    the profile's `nanoprobe/dev/sidecar/`, and the shell mission as the
//!    Caucasus template into `nanoprobe/missions/`;
//! 2. runs the sidecar from that folder, held by an SSH session, with a
//!    loopback listener, and forwards a loopback port here to it over SSH;
//! 3. drives the part (`shell_live/static_template.rs` or
//!    `shell_live/payload_preset.rs`), which writes its file into
//!    `nanoprobe/missions/own/` and removes it.
//!
//! Whatever happens, it stops the sidecar, copies its output into the
//! bundle, removes both folders, closes DCS through the stop task and
//! undeploys, so the profile holds what it held before; a run that leaves a
//! file added or missing fails. The bundle is `bundles/sidecar-<name>-<time>/`.
//!
//! On Windows the sidecar stops cleanly only on Ctrl-C, which a process in
//! another SSH session cannot be sent. The run ends its own process with
//! `Stop-Process -Id` and records the exit code, which is not part of the
//! verdict.

use std::path::Path;
use std::process::ExitCode;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::install::{AGENT_TOML, Profile, TOKEN};
use crate::profile::Target;
use crate::sampling::live::{hold_source, hold_timeout};
use crate::sampling::win::{listing_diff, listings};
use crate::shell_live::Live;
use crate::shell_live::saved_files::Saved;
use crate::sidecar::{API_PORT, LEASE_TTL_S, MAP_DIR, MISSIONS_DIR, hex};
use crate::ssh::{self, Forward};
use crate::windows::Windows;
use crate::{bundle, eval, link, live, miz};

/// How long the sidecar may take to listen.
const LISTEN_S: u64 = 60;
/// The sidecar's output files, in [`MAP_DIR`].
const OUT: &str = "sidecar.out";
const ERR: &str = "sidecar.err";

fn rel(name: &str) -> String {
    format!("{MAP_DIR}/{name}")
}

/// `rel`, a path relative to the profile with `/`, as a Windows path under
/// `profile`.
fn full(profile: &str, rel: &str) -> String {
    format!(
        "{}\\{}",
        profile.trim_end_matches('\\'),
        rel.replace('/', "\\")
    )
}

/// The Windows client as a [`Live`] target.
struct Client {
    windows: Windows,
    profile: String,
}

impl Live for Client {
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
            "sidecar-hold",
            &hold_source(seconds),
            hold_timeout(seconds),
        )
        .map(drop)
    }

    fn profile(&self) -> &dyn Profile {
        &self.windows
    }

    fn template_path(&self, file: &str) -> String {
        full(&self.profile, &format!("{MISSIONS_DIR}/own/{file}"))
    }

    fn config_path(&self) -> String {
        full(&self.profile, &rel("sidecar.toml"))
    }
}

/// The sidecar's `sidecar.toml`: the agent's mapped file and port from
/// `agent.toml`, its token in the profile, a loopback listener, the store
/// and logs in its own folder, and the profile's mission folder as both
/// `dir` and `dcs_dir`, since the sidecar and DCS open it by one path.
/// `extra` folders, paths under the profile, are listed after `own`.
fn sidecar_toml(
    profile: &str,
    map_path: &str,
    port: u16,
    extra: &[String],
) -> Result<String, String> {
    let dir = full(profile, MAP_DIR);
    let missions = full(profile, MISSIONS_DIR);
    let mut listed = vec![format!("{missions}\\own")];
    listed.extend(extra.iter().map(|d| full(profile, d)));
    let v = json!({
        "agent": {
            "map_path": map_path,
            "handshake": format!("127.0.0.1:{port}"),
            "token_path": full(profile, TOKEN),
        },
        "api": {
            "listen": format!("127.0.0.1:{API_PORT}"),
            "tokens_path": format!("{dir}\\tokens.toml"),
            "lease_ttl_s": LEASE_TTL_S,
        },
        "store": { "dir": format!("{dir}\\store") },
        "log": { "dir": format!("{dir}\\logs") },
        "missions": {
            "dir": missions,
            "dcs_dir": missions,
            "template_dirs": listed,
        },
        "health": { "heartbeat_stale_s": crate::shell_live::HEARTBEAT_STALE_S },
    });
    let table: toml::Table = serde_json::from_value(v).map_err(|e| e.to_string())?;
    toml::to_string(&table).map_err(|e| e.to_string())
}

/// The PowerShell that runs the sidecar in the foreground with its output
/// in its folder. It runs under an SSH session the caller holds open, since
/// the SSH server ends a session's processes with it.
fn start_script() -> String {
    format!(
        r#"$d = Full '{MAP_DIR}'
$p = Start-Process -FilePath ($d + '\nanoprobe-sidecar.exe') -ArgumentList ('run --config "' + $d + '\sidecar.toml"') `
    -NoNewWindow -PassThru -RedirectStandardOutput ($d + '\{OUT}') -RedirectStandardError ($d + '\{ERR}')
# Without a handle taken before the exit, ExitCode reads as empty.
$null = $p.Handle
[IO.File]::WriteAllText($d + '\{PID}', [string]$p.Id)
$p.WaitForExit()
[IO.File]::WriteAllText($d + '\{CODE}', [string]$p.ExitCode)"#
    )
}

/// The sidecar's process ID and exit code, which [`start_script`] writes:
/// a process ended by `Stop-Process` exits with -1, which SSH would report
/// as its own failure (255).
const PID: &str = "sidecar.pid";
const CODE: &str = "sidecar.exit";

/// The profile is back as found: every restore step succeeded and the
/// listings show nothing added or missing.
fn restored(restore: &serde_json::Map<String, Value>) -> bool {
    restore
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "profile_diff" | "restored"))
        .all(|(_, v)| v == &json!(true))
        && restore.get("profile_diff") == Some(&json!(Vec::<String>::new()))
}

/// The verdict: the part passed and the profile is back as found.
fn passed(results: &Value, key: &str, restore: &Value) -> bool {
    results[key]["pass"] == json!(true) && restore["restored"] == json!(true)
}

/// Run `part` on the windows-ssh target `name`.
pub fn run(
    repository: &Path,
    name: &str,
    part: &str,
    saved: Option<&Saved>,
) -> Result<ExitCode, String> {
    let key = match part {
        "static-template" => "static_template",
        "payload-preset" => "payload_preset",
        "saved-files" if saved.is_some() => "saved_files",
        _ => return Err(format!("--part {part} runs on a docker-wine target only")),
    };
    let target = crate::target(name)?;
    let Target::WindowsSsh {
        ssh,
        profile,
        start_task,
        stop_task,
        ..
    } = &target
    else {
        return Err(format!("{name} is not a windows-ssh target"));
    };
    let windows = Windows::connect(ssh, profile)?;
    if windows.dcs_processes()? > 0 {
        return Err(format!(
            "DCS is running on {name}: it may be in use. Close it first; the run starts DCS \
             through the start task and closes it through the stop task."
        ));
    }
    // The restore undeploys, which removes the whole `nanoprobe` folder.
    if windows.is_dir("nanoprobe")? {
        return Err(format!(
            "{name} holds a nanoprobe folder: undeploy it first so the run starts clean"
        ));
    }
    if windows.powershell(
        "if (@(Get-Process -Name nanoprobe-sidecar -ErrorAction SilentlyContinue).Count -gt 0) { exit 4 }",
        &[],
    )?.code != Some(0) {
        return Err(format!("a nanoprobe-sidecar runs on {name}; stop it first"));
    }
    let template = std::fs::read(repository.join("tests/fixtures/templates/caucasus.miz"))
        .map_err(|e| format!("tests/fixtures/templates/caucasus.miz: {e}"))?;
    let shell = miz::generate(&template, &miz::Changes::default())?;
    let sidecar = link::build_for(
        "x86_64-pc-windows-msvc",
        true,
        "target/windows-tools",
        ".exe",
        &[("nanoprobe-api", "nanoprobe-sidecar")],
    )?
    .pop()
    .ok_or("nanoprobe-sidecar was not built")?;
    let sidecar = std::fs::read(&sidecar).map_err(|e| format!("{}: {e}", sidecar.display()))?;
    let folder = bundle::folder().join(format!("sidecar-{name}-{}", bundle::stamp()));
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let before = listings(&windows)?;
    let started = bundle::utc_now();

    let client = Client {
        windows: windows.clone(),
        profile: profile.clone(),
    };
    let mut results = json!({});
    results["tree"] = json!(crate::sidecar::tree(repository));
    results["sidecar_sha256"] = json!(hex(&Sha256::digest(&sidecar)));
    // Which harness ran: a dirty tree's description does not say.
    results["xtask_sha256"] = json!(
        std::env::current_exe()
            .and_then(std::fs::read)
            .map_or_else(|e| format!("unreadable: {e}"), |b| hex(&Sha256::digest(b)))
    );
    let mut held = None;
    let result = attempt(
        &client,
        part,
        ssh,
        name,
        start_task,
        stop_task,
        &shell,
        &sidecar,
        saved,
        &mut held,
        &mut results,
    );
    // The build is undeployed below: read it now.
    let agent_build = crate::soak::agent_build(name);

    println!("== restore");
    let mut restore = serde_json::Map::new();
    let (stopped, code) = stop(&windows, held);
    restore.insert("sidecar_stopped".into(), json!(stopped));
    results["sidecar"] = json!({
        "exit_code_after_stop": code,
        "stopped_by": "Stop-Process: on Windows the sidecar stops cleanly only on Ctrl-C, \
                       which the run cannot send; not part of the verdict",
    });
    for file in [OUT, ERR] {
        if let Ok(Some(bytes)) = windows.read(&rel(file)) {
            let _ = std::fs::write(folder.join(format!("windows-{file}")), bytes);
        }
    }
    if let Ok(names) = windows.list(&rel("logs")) {
        for n in names {
            if let Ok(Some(bytes)) = windows.read(&rel(&format!("logs/{n}"))) {
                let _ = std::fs::write(folder.join(format!("windows-sidecar-{n}")), bytes);
            }
        }
    }
    let _ = windows.remove_file("nanoprobe/disable");
    let _ = windows.remove_file("nanoprobe/reload");
    let closed = if windows.dcs_processes().unwrap_or(1) > 0 {
        link::close_dcs(ssh, profile, stop_task)
    } else {
        Ok(())
    };
    restore.insert(
        "dcs_closed".into(),
        closed.map_or_else(|e| json!({ "error": e }), |()| json!(true)),
    );
    // After DCS closed: it holds the mission it loaded from
    // `nanoprobe/missions` open while it runs.
    for dir in [MAP_DIR, MISSIONS_DIR] {
        restore.insert(
            format!("removed {dir}"),
            windows
                .remove_tree(dir)
                .map_or_else(|e| json!({ "error": e }), |()| json!(true)),
        );
    }
    restore.insert(
        "undeployed".into(),
        crate::undeploy(name).map_or_else(|e| json!({ "error": e }), |_| json!(true)),
    );
    let diff = listings(&windows).map(|after| listing_diff(&before, &after));
    restore.insert(
        "profile_diff".into(),
        diff.map_or_else(|e| json!({ "error": e }), |d| json!(d)),
    );
    let back = restored(&restore);
    restore.insert("restored".into(), json!(back));
    let restore = Value::Object(restore);

    let ok = result.is_ok() && passed(&results, key, &restore);
    let out = json!({
        "target": name,
        "part": part,
        "started": started,
        "ended": bundle::utc_now(),
        "results": results,
        "restore": restore,
        "error": result.as_ref().err(),
        "agent_build": agent_build,
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = folder.join("sidecar.json");
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

#[allow(clippy::too_many_arguments)]
fn attempt(
    client: &Client,
    part: &str,
    ssh: &str,
    name: &str,
    start_task: &str,
    stop_task: &str,
    shell: &[u8],
    sidecar: &[u8],
    saved: Option<&Saved>,
    held: &mut Option<JoinHandle<Result<ssh::Output, String>>>,
    results: &mut Value,
) -> Result<(), String> {
    let windows = &client.windows;
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let toml = windows
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let config = nanoprobe_agent_link::config::AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    if config.transport.map_path.is_empty() {
        return Err("agent.toml names no map_path: add one to the target profile".into());
    }
    println!("== start DCS through its task");
    live::restart(windows, 0, false, || {
        windows.restart_tasks(start_task, stop_task)
    })?;
    println!("== host a private session");
    link::host_session(windows)?;

    windows.write(&format!("{MISSIONS_DIR}/Caucasus.miz"), shell)?;
    windows.create_dir(&format!("{MISSIONS_DIR}/own"))?;
    let token: String = (0..4)
        .map(|_| format!("{:016x}", nanoprobe_transport::random_nonzero()))
        .collect();
    let hash = hex(&Sha256::digest(token.as_bytes()));
    windows.write(
        &rel("tokens.toml"),
        format!("[[token]]\nname = \"xtask-hello\"\nrole = \"admin\"\nsha256 = \"{hash}\"\n")
            .as_bytes(),
    )?;
    windows.write(
        &rel("sidecar.toml"),
        sidecar_toml(
            &client.profile,
            &config.transport.map_path,
            config.transport.port,
            &saved
                .map(Saved::folders)
                .transpose()?
                .map(Vec::from)
                .unwrap_or_default(),
        )?
        .as_bytes(),
    )?;
    windows.write(&rel("nanoprobe-sidecar.exe"), sidecar)?;
    results["mission_template_path"] = json!(full(
        &client.profile,
        &format!("{MISSIONS_DIR}/Caucasus.miz")
    ));

    println!("== nanoprobe-sidecar on the client");
    let runner = windows.clone();
    *held = Some(std::thread::spawn(move || {
        runner.powershell(&start_script(), &[])
    }));
    let deadline = Instant::now() + Duration::from_secs(LISTEN_S);
    loop {
        let out = windows.read(&rel(OUT))?.unwrap_or_default();
        let out = String::from_utf8_lossy(&out);
        if out.contains("listening on") {
            println!("   {}", out.lines().next().unwrap_or(""));
            break;
        }
        if held.as_ref().is_some_and(JoinHandle::is_finished) {
            let err = windows.read(&rel(ERR))?.unwrap_or_default();
            return Err(format!(
                "the sidecar ended before it listened: {}",
                String::from_utf8_lossy(&err).trim()
            ));
        }
        if Instant::now() > deadline {
            return Err(format!("the sidecar did not listen within {LISTEN_S} s"));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let local = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| e.to_string())?
        .port();
    let mut forward = Forward::start(ssh, local, "127.0.0.1", API_PORT)?;
    let endpoint = format!("http://127.0.0.1:{local}");
    let r = if let Some(saved) = saved {
        let full = |rel: &str| full(&client.profile, rel);
        crate::shell_live::saved_files::drive(client, &endpoint, &token, saved, &full, results)
    } else if part == "static-template" {
        crate::shell_live::static_template::drive(client, &endpoint, &token, results)
    } else {
        crate::shell_live::payload_preset::drive(client, &endpoint, &token, results)
    };
    forward.stop();
    r
}

/// Stop the sidecar and wait for the session that held it. Returns whether
/// it ended and its exit code. Safe to call when nothing runs.
fn stop(windows: &Windows, held: Option<JoinHandle<Result<ssh::Output, String>>>) -> (bool, Value) {
    let Some(held) = held else {
        return (true, Value::Null);
    };
    // Only the run's own process, by the ID its script wrote.
    let pid = windows
        .read(&rel(PID))
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8_lossy(&b).trim().parse::<u32>().ok());
    if let Some(pid) = pid {
        let _ = windows.powershell(
            "Stop-Process -Id ([int]$a0) -ErrorAction SilentlyContinue\nexit 0",
            &[&pid.to_string()],
        );
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while !held.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    if !held.is_finished() {
        return (false, Value::Null);
    }
    let session = held.join();
    let code = windows
        .read(&rel(CODE))
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8_lossy(&b).trim().parse::<i64>().ok());
    match (code, session) {
        (Some(c), _) => (true, json!(c)),
        (None, Ok(Ok(o))) => (true, json!({ "session_exit": o.code })),
        (None, Ok(Err(e))) => (true, json!({ "error": e })),
        (None, Err(_)) => (true, json!({ "error": "the holding thread panicked" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r"C:\Users\someone's\Saved Games\DCS\";

    #[test]
    fn the_sidecar_toml_names_native_paths_in_the_profile() {
        let text = sidecar_toml(PROFILE, r"C:\p\nanoprobe.map", 47310, &[]).unwrap();
        let t: toml::Table = toml::from_str(&text).unwrap();
        let s = |section: &str, key: &str| t[section][key].as_str().unwrap().to_owned();
        let base = r"C:\Users\someone's\Saved Games\DCS";
        assert_eq!(s("agent", "map_path"), r"C:\p\nanoprobe.map");
        assert_eq!(s("agent", "handshake"), "127.0.0.1:47310");
        assert_eq!(s("agent", "token_path"), format!(r"{base}\nanoprobe\token"));
        assert_eq!(s("api", "listen"), format!("127.0.0.1:{API_PORT}"));
        assert_eq!(
            s("api", "tokens_path"),
            format!(r"{base}\nanoprobe\dev\sidecar\tokens.toml")
        );
        assert_eq!(
            s("store", "dir"),
            format!(r"{base}\nanoprobe\dev\sidecar\store")
        );
        assert_eq!(
            s("log", "dir"),
            format!(r"{base}\nanoprobe\dev\sidecar\logs")
        );
        assert_eq!(s("missions", "dir"), format!(r"{base}\nanoprobe\missions"));
        assert_eq!(s("missions", "dcs_dir"), s("missions", "dir"));
        assert_eq!(
            t["missions"]["template_dirs"].as_array().unwrap()[0].as_str(),
            Some(format!(r"{base}\nanoprobe\missions\own").as_str())
        );
        assert_eq!(
            t["api"]["lease_ttl_s"].as_integer(),
            Some(LEASE_TTL_S as i64)
        );
    }

    #[test]
    fn saved_files_folders_are_listed_after_own() {
        let extra = [
            "StaticTemplate".to_owned(),
            "MissionEditor/UnitPayloads".to_owned(),
        ];
        let text = sidecar_toml(PROFILE, r"C:\p\nanoprobe.map", 47310, &extra).unwrap();
        let t: toml::Table = toml::from_str(&text).unwrap();
        let listed: Vec<&str> = t["missions"]["template_dirs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let base = r"C:\Users\someone's\Saved Games\DCS";
        assert_eq!(
            listed,
            [
                format!(r"{base}\nanoprobe\missions\own"),
                format!(r"{base}\StaticTemplate"),
                format!(r"{base}\MissionEditor\UnitPayloads"),
            ]
        );
        assert_eq!(
            full(PROFILE, "StaticTemplate/a b.stm"),
            format!(r"{}\a b.stm", listed[1])
        );
    }

    #[test]
    fn the_template_path_is_in_the_listed_folder() {
        let text = sidecar_toml(PROFILE, r"C:\p\nanoprobe.map", 47310, &[]).unwrap();
        let t: toml::Table = toml::from_str(&text).unwrap();
        let listed = t["missions"]["template_dirs"].as_array().unwrap()[0]
            .as_str()
            .unwrap()
            .to_owned();
        let path = full(PROFILE, &format!("{MISSIONS_DIR}/own/x.stm"));
        assert_eq!(path, format!(r"{listed}\x.stm"));
        let client_config = full(PROFILE, &rel("sidecar.toml"));
        assert!(!client_config.starts_with(&listed), "{client_config}");
        assert!(client_config.starts_with(r"C:\"));
    }

    #[test]
    fn the_sidecar_runs_from_its_folder_with_its_config() {
        let s = start_script();
        assert!(s.contains("Full 'nanoprobe/dev/sidecar'"));
        assert!(s.contains(r"'\nanoprobe-sidecar.exe'"));
        assert!(s.contains(r#"'run --config "' + $d + '\sidecar.toml"'"#));
        assert!(s.contains(r"'\sidecar.out'") && s.contains(r"'\sidecar.err'"));
        assert!(s.contains("WaitForExit()") && s.contains("$null = $p.Handle"));
    }

    #[test]
    fn a_pass_needs_the_part_and_the_restore() {
        let results = json!({ "static_template": { "pass": true } });
        let restored = json!({ "restored": true });
        assert!(passed(&results, "static_template", &restored));
        assert!(!passed(
            &results,
            "static_template",
            &json!({ "restored": false })
        ));
        assert!(!passed(&results, "payload_preset", &restored));
        let failed = json!({ "static_template": { "pass": false } });
        assert!(!passed(&failed, "static_template", &restored));
    }

    #[test]
    fn the_profile_is_back_only_when_every_step_did_its_part() {
        let map = |v: Value| v.as_object().unwrap().clone();
        let good = json!({ "sidecar_stopped": true, "dcs_closed": true, "undeployed": true, "profile_diff": [] });
        assert!(restored(&map(good.clone())));
        let mut with_verdict = map(good.clone());
        with_verdict.insert("restored".into(), json!(false));
        assert!(restored(&with_verdict));
        let mut failed = map(good.clone());
        failed.insert("dcs_closed".into(), json!({ "error": "x" }));
        assert!(!restored(&failed));
        let mut added = map(good.clone());
        added.insert("profile_diff".into(), json!(["added: <profile>/x"]));
        assert!(!restored(&added));
        let mut unread = map(good);
        unread.insert("profile_diff".into(), json!({ "error": "x" }));
        assert!(!restored(&unread));
        assert!(!restored(&serde_json::Map::new()));
    }

    #[test]
    fn the_script_records_the_pid_and_the_exit_code_and_exits_0() {
        let s = start_script();
        assert!(s.contains(r"'\sidecar.pid'") && s.contains(r"'\sidecar.exit'"));
        assert!(!s.contains("exit $p"));
    }
}
