//! `sidecar <name> [--part all|conformance|hello|shell [--template FILE]]`:
//! the S17 live test, and with `--part shell` the S19 one
//! (`shell_live.rs`), on a
//! docker-wine target. The product's sidecar runs in a container of its own
//! that shares the game container's network namespace, as the example
//! compose overlay in `deploy/` sets it up, but started by hand and removed
//! at the end: the target's compose file is not touched.
//!
//! The game container's `/dev/shm` is its own, so the run moves the mapped
//! file to `nanoprobe/dev/sidecar/` in the profile, a folder on a volume the
//! host can bind into the sidecar's container. In order:
//!
//! 1. builds the agent with the eval channel, `conformance-probe` and
//!    `nanoprobe-sidecar` as static Linux programs (the sidecar needs zig as
//!    its C compiler: `mise install`), and deploys the agent;
//! 2. points `agent.toml`'s `map_path` at the folder, restarts DCS, and loads
//!    a fresh shell mission from `templates/caucasus.miz`;
//! 3. checks with `land.getSurfaceType` that the hello's route and the
//!    suite's spawn point are on land;
//! 4. builds the image from `deploy/Dockerfile` in a temporary folder on the
//!    host;
//! 5. `conformance`: runs `conformance-probe` in that image, in the shared
//!    namespace, with the suite's mission load a `MISSION_LOAD` of the shell;
//! 6. `hello`: runs the image's own command (`nanoprobe-sidecar run`) with a
//!    listener on the game container's address and a tokens file holding the
//!    hash of a token made for the run, forwards a loopback port here to it
//!    over SSH, and runs the Rust and the Python hello campaigns against it,
//!    unchanged;
//! 7. removes the containers, the image, the build cache (when it was empty
//!    before), the host folder and the map folder, and puts `agent.toml`
//!    back. The target keeps the deployed build; the caller restores it.
//!
//! The simulation is held running while a probe or a hello runs. Everything
//! goes under `bundles/sidecar-<name>-<time>/`, with `sidecar.json`, the
//! run's verdict.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::install::{AGENT_TOML, Profile as _, TOKEN};
use crate::profile::Target;
use crate::sampling::live::{self, Run};
use crate::ssh::{self, quote};
use crate::{bundle, link};

/// The map folder, under the profile.
const MAP_DIR: &str = "nanoprobe/dev/sidecar";
/// The mission folder of `[missions]`, under the profile: the run makes it
/// and removes it.
const MISSIONS_DIR: &str = "nanoprobe/missions";
/// The image's tag.
const IMAGE: &str = "nanoprobe-sidecar:xtask";
/// The containers' names.
const PROBE_C: &str = "nanoprobe-xtask-conformance";
const SIDECAR_C: &str = "nanoprobe-xtask-sidecar";
/// The API port inside the shared namespace.
const API_PORT: u16 = 47311;
/// Model seconds each hold runs.
const HOLD_S: f64 = 15.0;
/// How long a probe or a hello may take, seconds.
const LIMIT_S: u64 = 900;
/// The hello's route, from examples/hello-rust (start, end); the suite
/// spawns at the start.
pub(crate) const HELLO_POINTS: [(f64, f64); 2] = [(-290_000.0, 680_000.0), (-289_000.0, 680_000.0)];

const USAGE: &str =
    "usage: cargo xtask sidecar <name> [--part all|conformance|hello|shell [--template FILE]]";

pub fn command(repository: &Path, args: &[&str]) -> Result<ExitCode, String> {
    let (name, part, template) = match args {
        [name] => (*name, "all", None),
        [name, "--part", p] if ["all", "conformance", "hello", "shell"].contains(p) => {
            (*name, *p, None)
        }
        [name, "--part", "shell", "--template", t] => (*name, "shell", Some(*t)),
        _ => return Err(USAGE.into()),
    };
    let template = template
        .map(|t| std::fs::read(t).map_err(|e| format!("{t}: {e}")))
        .transpose()?;
    let Target::DockerWine { ssh, container, .. } = crate::target(name)? else {
        return Err("sidecar runs on a docker-wine target".into());
    };
    let live::Opened { run, shell, .. } = live::open(repository, name, "sidecar")?;
    let host = Host { ssh, container };
    let started = bundle::utc_now();
    let mut results = json!({});
    let result = attempt(
        repository,
        &run,
        &host,
        name,
        part,
        &shell,
        template.as_deref(),
        &mut results,
    );
    let keys: &[&str] = if part == "shell" {
        &[
            "reload",
            "theaters",
            "own_template",
            "finished",
            "hung",
            "killed",
        ]
    } else {
        &["conformance", "hello_rust", "hello_python"]
    };
    let ok = result.is_ok()
        && keys
            .iter()
            .filter(|k| part == "all" || part == "shell" || k.starts_with(part))
            // A skipped item is reported, not failed.
            .all(|k| results[k]["pass"] == json!(true) || results[k]["skipped"].is_string())
        // A clean stop of the sidecar's container.
        && (part == "conformance" || results["sidecar"]["exit_code_after_stop"] == json!("0"));
    let out = json!({
        "target": name,
        "part": part,
        "started": started,
        "ended": bundle::utc_now(),
        "results": results,
        "error": result.as_ref().err(),
        "agent_build": crate::soak::agent_build(name),
        "provenance": crate::soak::platform_provenance(name),
        "ok": ok,
    });
    let path = run.folder.join("sidecar.json");
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

/// The SSH host and the game container on it.
struct Host {
    ssh: String,
    container: String,
}

impl Host {
    /// Run `script` with `sh` on the host, `args` as `$1`, `$2`, ...
    fn sh(&self, script: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
        let mut remote = format!("sh -c {} sh", quote(script));
        for a in args {
            remote.push(' ');
            remote.push_str(&quote(a));
        }
        let o = ssh::run(&self.ssh, &remote, input)?;
        if o.code != Some(0) {
            return Err(format!(
                "on the host, `{script}` failed: {}",
                o.stderr.trim()
            ));
        }
        Ok(o.stdout)
    }

    fn text(&self, script: &str, args: &[&str]) -> Result<String, String> {
        Ok(String::from_utf8_lossy(&self.sh(script, args, b"")?)
            .trim()
            .to_owned())
    }

    /// The host path of `inside`, a path in the game container on a bind
    /// mount.
    fn host_path(&self, inside: &str) -> Result<String, String> {
        let mounts = self.text(
            "docker inspect -f '{{range .Mounts}}{{if eq .Type \"bind\"}}{{.Source}}|{{.Destination}}{{println}}{{end}}{{end}}' \"$1\"",
            &[&self.container],
        )?;
        mounts
            .lines()
            .filter_map(|l| l.split_once('|'))
            .filter(|(_, d)| {
                inside == *d || inside.starts_with(&format!("{}/", d.trim_end_matches('/')))
            })
            .max_by_key(|(_, d)| d.len())
            .map(|(s, d)| {
                format!(
                    "{}{}",
                    s.trim_end_matches('/'),
                    &inside[d.trim_end_matches('/').len()..]
                )
            })
            .ok_or_else(|| format!("{inside} is on no bind mount of the game container"))
    }
}

#[allow(clippy::too_many_arguments)]
fn attempt(
    repository: &Path,
    run: &Run,
    host: &Host,
    name: &str,
    part: &str,
    shell: &[u8],
    template: Option<&[u8]>,
    results: &mut Value,
) -> Result<(), String> {
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "conformance-probe")])?
        .pop()
        .ok_or("conformance-probe was not built")?;
    let sidecar = build_sidecar(repository)?;
    // Which code ran: the tree and the sidecar program's hash.
    results["tree"] = json!(
        Command::new("git")
            .args(["describe", "--always", "--dirty"])
            .current_dir(repository)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    );
    results["sidecar_sha256"] = json!(hex(&Sha256::digest(
        std::fs::read(&sidecar).map_err(|e| format!("{}: {e}", sidecar.display()))?
    )));
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let old = run
        .remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let profile = run.remote.full_path("");
    let profile = profile.trim_end_matches('/');
    let map_dir = format!("{profile}/{MAP_DIR}");
    let missions_dir = format!("{profile}/{MISSIONS_DIR}");
    if !run
        .sh("test -e \"$1\" && echo yes || true", &[&missions_dir])?
        .is_empty()
    {
        return Err(format!(
            "{MISSIONS_DIR} exists on the target; the run makes it and removes it, so it does not run"
        ));
    }
    run.sh("mkdir -p \"$1\"", &[&map_dir])?;
    let windows_map = format!(
        "Z:{}",
        format!("{map_dir}/nanoprobe.map").replace('/', "\\")
    );
    run.remote.write(
        AGENT_TOML,
        with_map_path(&String::from_utf8_lossy(&old), &windows_map)?.as_bytes(),
    )?;
    let dir = format!("/tmp/nanoprobe-sidecar-{}", bundle::stamp());
    let cache_was_empty = host
        .text("docker system df --format '{{.Type}}|{{.TotalCount}}'", &[])?
        .lines()
        .any(|l| l == "Build Cache|0");
    let result = with_map(
        repository, run, host, name, part, shell, template, &probe, &sidecar, profile, &map_dir,
        &dir, results,
    );
    // Put everything back, whatever the outcome; each step goes on after a
    // failed one.
    let mut cleanup = Vec::new();
    for c in [PROBE_C, SIDECAR_C] {
        if let Ok(logs) = host.sh("docker logs \"$1\" 2>&1 || true", &[c], b"") {
            let _ = std::fs::write(run.folder.join(format!("{c}.log")), logs);
        }
    }
    let steps: [(&str, String); 4] = [
        (
            "containers",
            format!("docker rm -f {PROBE_C} {SIDECAR_C} >/dev/null 2>&1 || true"),
        ),
        (
            "image",
            format!("docker rmi {IMAGE} >/dev/null 2>&1 || true"),
        ),
        (
            "build cache",
            if cache_was_empty {
                "docker builder prune -af >/dev/null".into()
            } else {
                "true".into()
            },
        ),
        ("host folder", "rm -rf \"$1\"".into()),
    ];
    for (what, script) in steps {
        if let Err(e) = host.sh(&script, &[&dir], b"") {
            cleanup.push(format!("{what}: {e}"));
        }
    }
    if let Err(e) = run.remote.write(AGENT_TOML, &old) {
        cleanup.push(format!("agent.toml: {e}"));
    }
    if let Err(e) = run.sh("rm -rf \"$1\"", &[&map_dir]) {
        cleanup.push(format!("map folder: {e}"));
    }
    if let Err(e) = run.sh("rm -rf \"$1\"", &[&missions_dir]) {
        cleanup.push(format!("mission folder: {e}"));
    }
    println!("== containers, image and host folder removed; agent.toml put back as found");
    if !cache_was_empty {
        println!("   the build cache was not empty before the run; it was left as it is");
    }
    match (result, cleanup.is_empty()) {
        (r, true) => r,
        (Ok(()), false) => Err(format!("not put back: {}", cleanup.join("; "))),
        (Err(e), false) => Err(format!("{e}; not put back: {}", cleanup.join("; "))),
    }
}

/// `old` with `[transport] map_path` set to `map`, every other key kept.
fn with_map_path(old: &str, map: &str) -> Result<String, String> {
    let mut table: toml::Table =
        toml::from_str(old).map_err(|e| format!("agent.toml on the target: {e}"))?;
    let t = table
        .entry("transport")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("agent.toml on the target: transport is not a table")?;
    t.insert("map_path".into(), toml::Value::String(map.into()));
    toml::to_string(&table).map_err(|e| e.to_string())
}

/// Build `nanoprobe-sidecar` as a static Linux program, with zig as the C
/// compiler of the bundled SQLite (mise.toml, `sidecar-linux`).
fn build_sidecar(repository: &Path) -> Result<PathBuf, String> {
    let tools = repository.join("tools");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
            "-p",
            "nanoprobe-api",
            "--bin",
            "nanoprobe-sidecar",
            "--target-dir",
        ])
        .arg(repository.join("target/linux"))
        .env("CC_x86_64_unknown_linux_musl", tools.join("zigcc-musl.sh"))
        .env("AR_x86_64_unknown_linux_musl", tools.join("zigar.sh"))
        .current_dir(repository)
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err("the sidecar's Linux build failed; it needs zig (`mise install`)".into());
    }
    Ok(repository.join("target/linux/x86_64-unknown-linux-musl/release/nanoprobe-sidecar"))
}

#[allow(clippy::too_many_arguments)]
fn with_map(
    repository: &Path,
    run: &Run,
    host: &Host,
    name: &str,
    part: &str,
    shell: &[u8],
    template: Option<&[u8]>,
    probe: &Path,
    sidecar: &Path,
    profile: &str,
    map_dir: &str,
    dir: &str,
    results: &mut Value,
) -> Result<(), String> {
    println!("== restart, the mapped file in {MAP_DIR}");
    crate::restart(name)?;
    println!("== load a fresh shell mission");
    crate::live::load(&run.remote, "np-shell.miz", shell)?;
    // `[missions]`: the templates, the folder named as DCS opens it
    // (spec/config.md). The S17 parts load the shell as the Caucasus
    // template; the shell part writes shells from the committed templates.
    if part == "shell" {
        for (file, bytes) in crate::shell_live::templates() {
            run.remote.write(&format!("{MISSIONS_DIR}/{file}"), bytes)?;
        }
    } else {
        run.remote
            .write(&format!("{MISSIONS_DIR}/Caucasus.miz"), shell)?;
    }
    let dcs_missions = format!(
        "Z:{}",
        format!("{profile}/{MISSIONS_DIR}").replace('/', "\\")
    );
    let shell_path = format!("{dcs_missions}\\Caucasus.miz");
    results["mission_path"] = json!(shell_path);
    // The points must be on land, or a ground group does not drive.
    let surface = run.eval(
        "server",
        "s17-surface",
        &format!(
            "local out = {{}}\nfor i, p in ipairs({{{}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
            HELLO_POINTS.iter().map(|(x, z)| format!("{{{x}, {z}}}")).collect::<Vec<_>>().join(", ")
        ),
        60,
    )?;
    results["surface"] = surface.clone();
    // land.SurfaceType: LAND 1, SHALLOW_WATER 2, WATER 3, ROAD 4, RUNWAY 5.
    if surface
        .as_array()
        .is_none_or(|a| a.iter().any(|v| !matches!(v.as_u64(), Some(1 | 4 | 5))))
    {
        return Err(format!(
            "a hello point is not on land: surface types {surface}"
        ));
    }

    let uid = run.sh("id -u; id -g", &[])?;
    let uid = String::from_utf8_lossy(&uid)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(":");
    // Why the map file is not in /dev/shm: the game container's IPC
    // namespace is its own unless it is `shareable`.
    results["ipc_mode"] = json!(host.text(
        "docker inspect -f '{{.HostConfig.IpcMode}}' \"$1\"",
        &[&host.container]
    )?);
    let host_map = host.host_path(map_dir)?;
    let host_token = host.host_path(&run.remote.full_path(TOKEN))?;
    let host_missions = host.host_path(&format!("{profile}/{MISSIONS_DIR}"))?;
    let port = run_port(run)?;
    host.sh("rm -rf \"$1\" && mkdir -m 755 \"$1\"", &[dir], b"")?;
    upload(
        host,
        dir,
        "nanoprobe-sidecar",
        &std::fs::read(sidecar).map_err(|e| e.to_string())?,
        "755",
    )?;
    upload(
        host,
        dir,
        "conformance-probe",
        &std::fs::read(probe).map_err(|e| e.to_string())?,
        "755",
    )?;
    let dockerfile = std::fs::read(repository.join("deploy/Dockerfile"))
        .map_err(|e| format!("deploy/Dockerfile: {e}"))?;
    upload(host, dir, "Dockerfile", &dockerfile, "644")?;
    println!("== docker build {IMAGE}");
    host.sh(
        "docker build -q -t \"$1\" \"$2\" >/dev/null",
        &[IMAGE, dir],
        b"",
    )?;
    let common = |c: &str| {
        format!(
            "docker run -d --name {c} --network container:\"$1\" --user \"$2\" \
             -v \"$3\":/nanoprobe-shm -v \"$4\":/etc/nanoprobe/agent-token:ro"
        )
    };
    let args = [
        host.container.as_str(),
        uid.as_str(),
        host_map.as_str(),
        host_token.as_str(),
        dir,
    ];

    if part == "all" || part == "conformance" {
        println!("== conformance-probe in the sidecar's image");
        let (x, z) = HELLO_POINTS[0];
        host.sh(
            &format!(
                "{} -v \"$5/conformance-probe\":/conformance-probe:ro --entrypoint /conformance-probe {IMAGE} \
                 --map /nanoprobe-shm/nanoprobe.map --port {port} --token-file /etc/nanoprobe/agent-token \
                 --unit-type M-113 --spawn {x},{z} --patience 1500 --mission \"$6\" --seconds {LIMIT_S} >/dev/null",
                common(PROBE_C)
            ),
            &[args[0], args[1], args[2], args[3], args[4], &shell_path],
            b"",
        )?;
        let deadline = Instant::now() + Duration::from_secs(LIMIT_S + 60);
        let lines = loop {
            let logs = host.sh("docker logs \"$1\" 2>/dev/null || true", &[PROBE_C], b"")?;
            let lines = link::parse_lines(&logs);
            if lines
                .iter()
                .any(|l| l["ev"] == "verdict" || l["ev"] == "error")
            {
                break lines;
            }
            if host.text("docker inspect -f '{{.State.Running}}' \"$1\"", &[PROBE_C])? != "true" {
                break lines;
            }
            if Instant::now() > deadline {
                return Err("conformance-probe gave no verdict in time".into());
            }
            hold(run)?;
        };
        for l in lines.iter().filter(|l| l["ev"] == "check") {
            println!(
                "   {} {}",
                if l["ok"] == json!(true) {
                    "ok  "
                } else {
                    "FAIL"
                },
                l["name"]
            );
        }
        let verdict = lines.iter().find(|l| l["ev"] == "verdict").cloned();
        results["conformance"] = json!({
            "pass": verdict.as_ref().is_some_and(|v| {
                v["pass"] == json!(true)
                    && v["mispredicted"] == json!(0)
                    && v["checks"].as_u64().is_some_and(|n| n >= 9)
                    && v["passed"] == v["checks"]
            }),
            "verdict": verdict,
            "checks": lines.iter().filter(|l| l["ev"] == "check").collect::<Vec<_>>(),
            "error": lines.iter().find(|l| l["ev"] == "error"),
        });
        host.sh(
            "docker rm -f \"$1\" >/dev/null 2>&1 || true",
            &[PROBE_C],
            b"",
        )?;
        // The suite loaded the shell again; start from a fresh one for the
        // hellos too.
        if part == "all" {
            crate::live::load(&run.remote, "np-shell.miz", shell)?;
        }
    }

    if part == "all" || part == "hello" || part == "shell" {
        let token: String = (0..4)
            .map(|_| format!("{:016x}", nanoprobe_transport::random_nonzero()))
            .collect();
        let hash = hex(&Sha256::digest(token.as_bytes()));
        let tokens =
            format!("[[token]]\nname = \"xtask-hello\"\nrole = \"admin\"\nsha256 = \"{hash}\"\n");
        upload(host, dir, "tokens.toml", tokens.as_bytes(), "644")?;
        let toml = format!(
            "[agent]\nmap_path = \"/nanoprobe-shm/nanoprobe.map\"\nhandshake = \"127.0.0.1:{port}\"\n\
             token_path = \"/etc/nanoprobe/agent-token\"\n\n[api]\nlisten = \"0.0.0.0:{API_PORT}\"\n\
             tokens_path = \"/etc/nanoprobe/tokens.toml\"\n\n[store]\ndir = \"/var/lib/nanoprobe/campaigns\"\n\n\
             [missions]\ndir = \"/missions\"\ndcs_dir = '{dcs_missions}'\n\n\
             [health]\nheartbeat_stale_s = {stale}\n",
            stale = crate::shell_live::HEARTBEAT_STALE_S,
        );
        upload(host, dir, "sidecar.toml", toml.as_bytes(), "644")?;
        println!("== nanoprobe-sidecar in its container");
        host.sh(
            &format!(
                "{} -v \"$5/sidecar.toml\":/etc/nanoprobe/sidecar.toml:ro -v \"$5/tokens.toml\":/etc/nanoprobe/tokens.toml:ro \
                 -v \"$6\":/missions:{mode} {restart} \
                 --tmpfs /var/lib/nanoprobe:rw,size=64m,uid={u},gid={g},mode=0700 {IMAGE} >/dev/null",
                common(SIDECAR_C),
                u = uid.split(':').next().unwrap_or("0"),
                g = uid.split(':').nth(1).unwrap_or("0"),
                // The shell part writes shells, and keeps the sidecar
                // across the DCS restarts it makes.
                mode = if part == "shell" { "rw" } else { "ro" },
                restart = if part == "shell" { "--restart on-failure" } else { "" },
            ),
            &[args[0], args[1], args[2], args[3], args[4], &host_missions],
            b"",
        )?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let logs = host.text("docker logs \"$1\" 2>&1 || true", &[SIDECAR_C])?;
            if logs.contains("listening on") {
                println!("   {}", logs.lines().next().unwrap_or(""));
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("the sidecar did not listen within 60 s: {logs}"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let ip = host
            .text("docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' \"$1\"", &[&host.container])?
            .split_whitespace()
            .next()
            .ok_or("the game container has no address")?
            .to_owned();
        let local = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| e.to_string())?
            .port();
        let mut forward = Forward::start(&host.ssh, local, &ip, API_PORT)?;
        let endpoint = format!("http://127.0.0.1:{local}");
        if part == "shell" {
            let container = host.container.clone();
            let r = crate::shell_live::drive(
                run,
                &endpoint,
                &token,
                template,
                || run.remote.players(),
                || crate::restart(name).map(|_| ()),
                || {
                    host.text(
                        "docker exec \"$1\" sh -c 'pkill -9 -f DCS_server.exe; echo killed'",
                        &[&container],
                    )
                },
                results,
            );
            results["sidecar_logs"] =
                json!(host.text("docker logs --tail 200 \"$1\" 2>&1 || true", &[SIDECAR_C])?);
            forward.stop();
            host.sh("docker stop -t 20 \"$1\" >/dev/null", &[SIDECAR_C], b"")?;
            results["sidecar"] = json!({
                "exit_code_after_stop": host.text("docker inspect -f '{{.State.ExitCode}}' \"$1\"", &[SIDECAR_C])?,
                "restarts": host.text("docker inspect -f '{{.RestartCount}}' \"$1\"", &[SIDECAR_C])?,
            });
            return r;
        }
        let hellos: [(&str, Vec<String>); 2] = [
            (
                "hello_rust",
                vec![
                    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()),
                    "run".into(),
                    "-q".into(),
                    "-p".into(),
                    "nanoprobe-hello".into(),
                    "--bin".into(),
                    "hello-campaign".into(),
                    "--".into(),
                    endpoint.clone(),
                ],
            ),
            (
                "hello_python",
                vec![
                    "mise".into(),
                    "run".into(),
                    "hello-python".into(),
                    "--".into(),
                    endpoint.clone(),
                ],
            ),
        ];
        for (key, argv) in hellos {
            println!("== {key}");
            let out = run.folder.join(format!("{key}.out"));
            let file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            let mut child: Child = Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(repository)
                .env("NANOPROBE_TOKEN", &token)
                .stdin(Stdio::null())
                .stdout(file.try_clone().map_err(|e| e.to_string())?)
                .stderr(file)
                .spawn()
                .map_err(|e| format!("{key}: {e}"))?;
            let deadline = Instant::now() + Duration::from_secs(LIMIT_S);
            let status = loop {
                if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                    break Some(s);
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                if let Err(e) = hold(run) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            };
            let text = std::fs::read_to_string(&out).unwrap_or_default();
            for l in text.lines() {
                println!("   {l}");
            }
            // The activation loaded the campaign's mission: after the
            // campaign's line the hello saw DCS loading.
            let mission_loaded = text
                .split_once("\ncampaign ")
                .is_some_and(|(_, after)| after.contains("sim Loading"));
            let materialized_after_load = materialized_after_load(&text);
            results[key] = json!({
                "pass": status.is_some_and(|s| s.success())
                    && mission_loaded
                    && materialized_after_load,
                "mission_loaded": mission_loaded,
                "materialized_after_load": materialized_after_load,
                "exit": status.and_then(|s| s.code()),
                "done": text.lines().find(|l| l.contains("hello campaign done")),
            });
        }
        forward.stop();
        let health = host.text(
            "docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' \"$1\"",
            &[SIDECAR_C],
        )?;
        results["sidecar"] = json!({ "docker_health": health });
        // A clean stop: SIGTERM, and the sidecar's exit code.
        host.sh("docker stop -t 20 \"$1\" >/dev/null", &[SIDECAR_C], b"")?;
        let exit = host.text(
            "docker inspect -f '{{.State.ExitCode}}' \"$1\"",
            &[SIDECAR_C],
        )?;
        results["sidecar"]["exit_code_after_stop"] = json!(exit);
    }
    Ok(())
}

/// Whether a hello's group materialized once, and only after DCS began
/// loading the campaign's mission: no `is Materializing` between the
/// campaign's line and the first `sim Loading`, and one after it.
fn materialized_after_load(text: &str) -> bool {
    let Some((_, after)) = text.split_once("\ncampaign ") else {
        return false;
    };
    let Some((before, loaded)) = after.split_once("sim Loading") else {
        return false;
    };
    let count = |t: &str| t.to_lowercase().matches("is materializing").count();
    count(before) == 0 && count(loaded) == 1
}

/// A loopback port here forwarded over SSH to `ip:port` on the host's side.
struct Forward(Option<Child>);

impl Forward {
    fn start(dest: &str, local: u16, ip: &str, port: u16) -> Result<Forward, String> {
        let child = Command::new("ssh")
            .args([
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
            ])
            .arg(format!("127.0.0.1:{local}:{ip}:{port}"))
            .args(["--", dest])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ssh -L: {e}"))?;
        let mut f = Forward(Some(child));
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::net::TcpStream::connect(("127.0.0.1", local)).is_err() {
            if Instant::now() > deadline {
                f.stop();
                return Err("the SSH forward did not come up".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(f)
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Hold the simulation running for [`HOLD_S`] model seconds, or until a
/// mission load starts model time again: a campaign's activation loads its
/// mission while a hello runs.
fn hold(run: &Run) -> Result<(), String> {
    let source = format!(
        "local E = ...\n\
         local t0 = DCS.getModelTime()\n\
         return E.wait(function()\n  \
           local m = DCS.getModelTime()\n  \
           if m - t0 >= {HOLD_S} or m < t0 then return 'done' end\n\
         end, {}, 'running')\n",
        HOLD_S * 4.0 + 60.0
    );
    run.eval(
        "hook",
        "s17-hold",
        &source,
        crate::sampling::live::hold_timeout(HOLD_S),
    )?;
    Ok(())
}

fn upload(host: &Host, dir: &str, name: &str, bytes: &[u8], mode: &str) -> Result<(), String> {
    host.sh(
        "cat > \"$1/$2.partial\" && chmod \"$3\" \"$1/$2.partial\" && mv -f \"$1/$2.partial\" \"$1/$2\"",
        &[dir, name, mode],
        bytes,
    )
    .map(|_| ())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn run_port(run: &Run) -> Result<u16, String> {
    run.link_args()[1]
        .parse()
        .map_err(|e| format!("agent port: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository;

    /// The pair after `prefix` on the first line of `text` that starts with
    /// it, such as `START = (-290_000.0, 680_000.0)`.
    fn pair(text: &str, prefix: &str) -> (f64, f64) {
        let line = text
            .lines()
            .map(str::trim_start)
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line starts with {prefix:?}"));
        let inner = line
            .split_once('=')
            .and_then(|(_, r)| r.split_once('('))
            .and_then(|(_, r)| r.split_once(')'))
            .map(|(i, _)| i.replace('_', ""))
            .unwrap_or_else(|| panic!("no pair in {line:?}"));
        let (x, z) = inner.split_once(',').expect("two numbers");
        (x.trim().parse().expect("x"), z.trim().parse().expect("z"))
    }

    #[test]
    fn the_hello_programs_and_the_probe_use_the_hello_points() {
        let root = repository();
        let read = |p: &str| std::fs::read_to_string(root.join(p)).expect(p);
        let rust = read("examples/hello-rust/src/lib.rs");
        let python = read("examples/hello-python/hello.py");
        let probe = read("crates/fake-agent/src/bin/conformance-probe.rs");
        assert_eq!(pair(&rust, "const START"), HELLO_POINTS[0]);
        assert_eq!(pair(&rust, "const END"), HELLO_POINTS[1]);
        assert_eq!(pair(&python, "START ="), HELLO_POINTS[0]);
        assert_eq!(pair(&python, "END ="), HELLO_POINTS[1]);
        assert_eq!(pair(&probe, "None =>"), HELLO_POINTS[0]);
    }

    #[test]
    fn a_group_materializes_once_after_the_load() {
        let ok =
            "x\ncampaign 1\ngroup 1 units\nevent 5: sim Loading on ``\ngroup 1 is Materializing\n";
        assert!(materialized_after_load(ok));
        let early = "x\ncampaign 1\ngroup 1 is Materializing\nevent 5: sim Loading on ``\ngroup 1 is Materializing\n";
        assert!(!materialized_after_load(early));
        assert!(!materialized_after_load(
            "x\ncampaign 1\ngroup 1 is Materializing\n"
        ));
    }

    #[test]
    fn map_path_is_set_and_other_keys_kept() {
        let out = with_map_path(
            "[transport]\nmap_path = 'Z:\\dev\\shm\\a.map'\nport = 47310\n[lua]\nraw = true\n",
            "Z:\\x\\nanoprobe.map",
        )
        .expect("toml");
        let t: toml::Table = toml::from_str(&out).expect("parse");
        assert_eq!(
            t["transport"]["map_path"].as_str(),
            Some("Z:\\x\\nanoprobe.map")
        );
        assert_eq!(t["transport"]["port"].as_integer(), Some(47310));
        assert_eq!(t["lua"]["raw"].as_bool(), Some(true));
    }
}
