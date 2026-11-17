//! `persistence <name>|rehearse [--only a,b,...]`: the S18 live tests of
//! persistence and restore (docs/results/2026-11-15-s18-persistence-host.md,
//! "Live tests left for a later step"), as scenarios in [`scenarios`].
//!
//! On a docker-wine target the sidecar runs as `sidecar` runs it (its own
//! container in the game container's network namespace, the mapped file in
//! `nanoprobe/dev/sidecar/`, `[missions]` in `nanoprobe/missions/`), with
//! its store on disk in `nanoprobe/dev/persistence-store/` so a sidecar
//! started again reads it, and with `NANOPROBE_COMMIT_TIMINGS` set so it
//! prints its write times (`nanoprobe_api::timings`). The test kills and
//! restarts the sidecar's container, pauses it or the game container for a
//! few seconds, restarts DCS with `docker restart -t 0` (a kill) only with
//! no player connected, and loads the campaign's mission with
//! `net.load_mission` in the hook state. It removes what it made: the
//! containers, the image, the build cache when it was empty before, the
//! host folder, the three profile folders, and puts `agent.toml` back. The
//! target keeps the deployed build; the caller restores it.
//!
//! `rehearse` runs the same scenarios against `nanoprobe-fake-sidecar` with
//! a store folder here: a reload, a DCS kill and a sidecar kill are each a
//! restart of that process, and the DCS-side checks are not judged.
//!
//! Everything goes under `bundles/persistence-<name>-<time>/`, with
//! `persistence.json`, the run's verdict.

mod api;
mod scenarios;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::bundle;
use crate::install::{AGENT_TOML, Profile as _, TOKEN};
use crate::profile::Target as Profile;
use crate::sampling::live::{self, Run};
use crate::sidecar::{
    API_PORT, Forward, Host, IMAGE, MAP_DIR, MISSIONS_DIR, build_sidecar, hex, run_port, upload,
    with_map_path,
};
use scenarios::{DcsGroup, DcsUnit, Target};

/// The store folder, under the profile.
const STORE_DIR: &str = "nanoprobe/dev/persistence-store";
/// The sidecar's container.
const SIDECAR_C: &str = "nanoprobe-xtask-persistence";
/// Seconds between the sidecar's timing lines.
const TIMINGS_S: u64 = 10;
const THEATER: &str = "Caucasus";

const USAGE: &str = "usage: cargo xtask persistence <name>|rehearse [--only a,b,...]";

pub fn command(repository: &Path, args: &[&str]) -> Result<ExitCode, String> {
    let (name, which): (&str, Vec<&str>) = match args {
        [name] => (*name, scenarios::ALL.to_vec()),
        [name, "--only", list] => (*name, list.split(',').filter(|s| !s.is_empty()).collect()),
        _ => return Err(USAGE.into()),
    };
    if let Some(bad) = which.iter().find(|w| !scenarios::ALL.contains(w)) {
        return Err(format!(
            "no scenario {bad}; they are {}",
            scenarios::ALL.join(", ")
        ));
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let tag = bundle::stamp();
    let started = bundle::utc_now();
    let mut results = json!({});
    let (folder, result, extra) = if name == "rehearse" {
        let folder = repository.join(format!("bundles/persistence-rehearse-{tag}"));
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let r = rehearse(repository, &rt, &folder, &which, &tag, &mut results);
        (folder, r, json!({}))
    } else {
        let Profile::DockerWine { ssh, container, .. } = crate::target(name)? else {
            return Err("persistence runs on a docker-wine target".into());
        };
        let live::Opened { run, shell, .. } = live::open(repository, name, "persistence")?;
        let host = Host { ssh, container };
        let folder = run.folder.clone();
        let mut extra = json!({});
        let r = on_server(
            repository,
            &rt,
            &run,
            &host,
            name,
            &shell,
            &which,
            &tag,
            &mut results,
            &mut extra,
        );
        extra["agent_build"] = json!(crate::soak::agent_build(name));
        extra["provenance"] = json!(crate::soak::platform_provenance(name));
        (folder, r, extra)
    };
    // A scenario the fake cannot run is skipped (`pass` null), only there.
    let ok = result.is_ok()
        && which.iter().all(|w| {
            results[*w]["pass"] == json!(true)
                || (name == "rehearse" && results[*w]["pass"].is_null())
        });
    let out = json!({
        "target": name,
        "scenarios": which,
        "started": started,
        "ended": bundle::utc_now(),
        "results": results,
        "run": extra,
        "error": result.as_ref().err(),
        "ok": ok,
    });
    let path = folder.join("persistence.json");
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

/// The server: set up, run, and put back whatever happened.
#[allow(clippy::too_many_arguments)]
fn on_server(
    repository: &Path,
    rt: &tokio::runtime::Runtime,
    run: &Run,
    host: &Host,
    name: &str,
    shell: &[u8],
    which: &[&str],
    tag: &str,
    results: &mut Value,
    extra: &mut Value,
) -> Result<(), String> {
    let sidecar = build_sidecar(repository)?;
    extra["tree"] = json!(
        Command::new("git")
            .args(["describe", "--always", "--dirty"])
            .current_dir(repository)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    );
    extra["sidecar_sha256"] = json!(hex(&Sha256::digest(
        std::fs::read(&sidecar).map_err(|e| format!("{}: {e}", sidecar.display()))?
    )));
    let profile = run.remote.full_path("");
    let profile = profile.trim_end_matches('/').to_owned();
    for d in [MAP_DIR, MISSIONS_DIR, STORE_DIR] {
        let full = format!("{profile}/{d}");
        if !run
            .sh("test -e \"$1\" && echo yes || true", &[&full])?
            .is_empty()
        {
            return Err(format!(
                "{d} exists on the target; the run makes it and removes it, so it does not run"
            ));
        }
    }
    println!("== deploy --dev");
    crate::deploy(name, None, true, false)?;
    let old = run
        .remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let map_dir = format!("{profile}/{MAP_DIR}");
    let store_dir = format!("{profile}/{STORE_DIR}");
    let missions_dir = format!("{profile}/{MISSIONS_DIR}");
    let dir = format!("/tmp/nanoprobe-persistence-{}", bundle::stamp());
    let cache_was_empty = host
        .text("docker system df --format '{{.Type}}|{{.TotalCount}}'", &[])?
        .lines()
        .any(|l| l == "Build Cache|0");
    let result = (|| {
        run.sh("mkdir -p \"$1\" \"$2\"", &[&map_dir, &store_dir])?;
        let windows_map = format!(
            "Z:{}",
            format!("{map_dir}/nanoprobe.map").replace('/', "\\")
        );
        run.remote.write(
            AGENT_TOML,
            with_map_path(&String::from_utf8_lossy(&old), &windows_map)?.as_bytes(),
        )?;
        println!("== restart, the mapped file in {MAP_DIR}");
        crate::restart(name)?;
        crate::live::load(&run.remote, "np-shell.miz", shell)?;
        run.remote
            .write(&format!("{MISSIONS_DIR}/{THEATER}.miz"), shell)?;
        let dcs_missions = format!("Z:{}", missions_dir.replace('/', "\\"));
        let mission = format!("{dcs_missions}\\{THEATER}.miz");
        extra["mission_path"] = json!(mission);
        let mut t = LiveTarget::new(
            run,
            host,
            &sidecar,
            repository,
            &profile,
            &dir,
            &dcs_missions,
            mission,
        )?;
        let r = t
            .start_sidecar()
            .and_then(|()| scenarios::run(rt, &mut t, which, tag, results));
        extra["sidecar_restarts"] = json!(t.boots);
        let _ = t.freeze_dcs(false);
        let _ = t.freeze_sidecar(false);
        t.save_logs();
        t.forward.stop();
        r
    })();
    let mut cleanup = Vec::new();
    let steps: [(&str, String); 4] = [
        (
            "container",
            format!("docker rm -f {SIDECAR_C} >/dev/null 2>&1 || true"),
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
    for d in [&map_dir, &store_dir, &missions_dir] {
        if let Err(e) = run.sh("rm -rf \"$1\"", &[d]) {
            cleanup.push(format!("{d}: {e}"));
        }
    }
    println!("== containers, image and folders removed; agent.toml put back as found");
    match (result, cleanup.is_empty()) {
        (r, true) => r,
        (Ok(()), false) => Err(format!("not put back: {}", cleanup.join("; "))),
        (Err(e), false) => Err(format!("{e}; not put back: {}", cleanup.join("; "))),
    }
}

struct LiveTarget<'a> {
    run: &'a Run,
    host: &'a Host,
    /// `docker run` arguments `$1`...: game container, uid:gid, map, token,
    /// host folder, missions, store.
    args: Vec<String>,
    uid: (String, String),
    token: String,
    mission: String,
    local: u16,
    forward: Forward,
    /// Log text of the sidecar's earlier containers.
    logs: String,
    boots: u32,
    folder: PathBuf,
}

impl<'a> LiveTarget<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        run: &'a Run,
        host: &'a Host,
        sidecar: &Path,
        repository: &Path,
        profile: &str,
        dir: &str,
        dcs_missions: &str,
        mission: String,
    ) -> Result<LiveTarget<'a>, String> {
        let uid = run.sh("id -u; id -g", &[])?;
        let uid: Vec<String> = String::from_utf8_lossy(&uid)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let [u, g] = uid.as_slice() else {
            return Err("could not read the DCS user's uid and gid".into());
        };
        let host_map = host.host_path(&format!("{profile}/{MAP_DIR}"))?;
        let host_token = host.host_path(&run.remote.full_path(TOKEN))?;
        let host_missions = host.host_path(&format!("{profile}/{MISSIONS_DIR}"))?;
        let host_store = host.host_path(&format!("{profile}/{STORE_DIR}"))?;
        let port = run_port(run)?;
        host.sh("rm -rf \"$1\" && mkdir -m 755 \"$1\"", &[dir], b"")?;
        upload(
            host,
            dir,
            "nanoprobe-sidecar",
            &std::fs::read(sidecar).map_err(|e| e.to_string())?,
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
        let token: String = (0..4)
            .map(|_| format!("{:016x}", nanoprobe_transport::random_nonzero()))
            .collect();
        let hash = hex(&Sha256::digest(token.as_bytes()));
        let tokens = format!(
            "[[token]]\nname = \"xtask-persistence\"\nrole = \"admin\"\nsha256 = \"{hash}\"\n"
        );
        upload(host, dir, "tokens.toml", tokens.as_bytes(), "644")?;
        let toml = format!(
            "[agent]\nmap_path = \"/nanoprobe-shm/nanoprobe.map\"\nhandshake = \"127.0.0.1:{port}\"\n\
             token_path = \"/etc/nanoprobe/agent-token\"\n\n[api]\nlisten = \"0.0.0.0:{API_PORT}\"\n\
             tokens_path = \"/etc/nanoprobe/tokens.toml\"\n\n[store]\ndir = \"/store\"\n\n\
             [missions]\ndir = \"/missions\"\ndcs_dir = '{dcs_missions}'\n"
        );
        upload(host, dir, "sidecar.toml", toml.as_bytes(), "644")?;
        let local = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| e.to_string())?
            .port();
        Ok(LiveTarget {
            run,
            host,
            args: vec![
                host.container.clone(),
                format!("{u}:{g}"),
                host_map,
                host_token,
                dir.to_owned(),
                host_missions,
                host_store,
            ],
            uid: (u.clone(), g.clone()),
            token,
            mission,
            local,
            forward: Forward(None),
            logs: String::new(),
            boots: 0,
            folder: run.folder.clone(),
        })
    }

    fn container_logs(&self) -> String {
        self.host
            .text("docker logs \"$1\" 2>&1 || true", &[SIDECAR_C])
            .unwrap_or_default()
    }

    /// Keep the current container's log with the earlier ones.
    fn save_logs(&mut self) {
        let l = self.container_logs();
        if !l.is_empty() {
            self.logs
                .push_str(&format!("--- boot {}\n{l}\n", self.boots));
        }
        let _ = std::fs::write(self.folder.join(format!("{SIDECAR_C}.log")), &self.logs);
    }

    fn eval(&self, state: &str, stem: &str, source: &str) -> Result<Value, String> {
        self.run.eval(state, stem, source, 120)
    }

    fn no_players(&self) -> Result<(), String> {
        match self.run.remote.players()? {
            0 => Ok(()),
            n => Err(format!(
                "{n} player(s) connected; stopping before a disruptive step"
            )),
        }
    }

    /// Load the campaign's mission and wait until it started.
    fn load_mission(&self) -> Result<(), String> {
        let src = format!("return net.load_mission({})", lua_string(&self.mission));
        crate::live::wait_load(&self.run.remote, || {
            let v = self.eval("hook", "s18-load", &src)?;
            if v == json!(false) {
                return Err(format!("net.load_mission refused {}", self.mission));
            }
            Ok(())
        })
    }
}

/// A Lua long string holding `s`.
fn lua_string(s: &str) -> String {
    let mut eq = String::new();
    while s.contains(&format!("]{eq}]")) || s.ends_with(&format!("]{eq}")) {
        eq.push('=');
    }
    // Lua drops a newline that opens a long string: add one to drop.
    let nl = if s.starts_with('\n') { "\n" } else { "" };
    format!("[{eq}[{nl}{s}]{eq}]")
}

impl Target for LiveTarget<'_> {
    fn theater(&self) -> String {
        THEATER.into()
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.local)
    }

    fn token(&self) -> Option<String> {
        Some(self.token.clone())
    }

    fn live(&self) -> bool {
        true
    }

    /// Run model time, ending early when it goes back: an activation's
    /// mission load during the hold starts it again at 0.
    fn hold(&mut self, seconds: f64) -> Result<(), String> {
        let source = format!(
            "local E = ...\n\
             local t0 = DCS.getModelTime()\n\
             return E.wait(function()\n  \
               local m = DCS.getModelTime()\n  \
               if m - t0 >= {seconds} or m < t0 then return 'done' end\n\
             end, {}, 'running')\n",
            seconds * 4.0 + 60.0
        );
        self.run
            .eval("hook", "s18-hold", &source, live::hold_timeout(seconds))?;
        Ok(())
    }

    fn reload(&mut self) -> Result<(), String> {
        self.no_players()?;
        println!("   reload");
        self.load_mission()
    }

    fn no_players(&mut self) -> Result<(), String> {
        LiveTarget::no_players(self)
    }

    fn kill_dcs(&mut self) -> Result<(), String> {
        self.no_players()?;
        println!("   DCS kill");
        self.kill_sidecar_stopped()?;
        let host = self.host;
        crate::live::restart(&self.run.remote, 0, true, || {
            host.sh(
                "docker restart -t 0 \"$1\" >/dev/null",
                &[&host.container],
                b"",
            )
            .map(|_| ())
        })?;
        // DCS starts its own mission; the campaign's is loaded as an
        // operator would, then the sidecar starts from its store. A visitor
        // may have joined the restarted server.
        LiveTarget::no_players(self)?;
        self.load_mission()?;
        self.start_sidecar()
    }

    fn kill_sidecar(&mut self) -> Result<(), String> {
        println!("   sidecar kill");
        self.kill_sidecar_stopped()?;
        self.start_sidecar()
    }

    fn kill_sidecar_stopped(&mut self) -> Result<(), String> {
        self.forward.stop();
        self.save_logs();
        self.host.sh(
            "docker kill -s KILL \"$1\" >/dev/null 2>&1 || true; docker rm -f \"$1\" >/dev/null 2>&1 || true",
            &[SIDECAR_C],
            b"",
        )?;
        Ok(())
    }

    fn start_sidecar(&mut self) -> Result<(), String> {
        self.boots += 1;
        let a: Vec<&str> = self.args.iter().map(String::as_str).collect();
        // `/missions` is writable: the sidecar writes each campaign's shell
        // under `[missions] dir` (spec/config.md, "[missions]").
        self.host.sh(
            &format!(
                "docker run -d --name {SIDECAR_C} --network container:\"$1\" --user \"$2\" \
                 -e {var}={TIMINGS_S} \
                 -v \"$3\":/nanoprobe-shm -v \"$4\":/etc/nanoprobe/agent-token:ro \
                 -v \"$5/sidecar.toml\":/etc/nanoprobe/sidecar.toml:ro -v \"$5/tokens.toml\":/etc/nanoprobe/tokens.toml:ro \
                 -v \"$6\":/missions:rw -v \"$7\":/store \
                 --tmpfs /var/lib/nanoprobe:rw,size=16m,uid={u},gid={g},mode=0700 {IMAGE} >/dev/null",
                var = nanoprobe_api_timings_var(),
                u = self.uid.0,
                g = self.uid.1,
            ),
            &a,
            b"",
        )?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let logs = self.container_logs();
            if logs.contains("listening on") {
                break;
            }
            if Instant::now() > deadline
                || self.host.text(
                    "docker inspect -f '{{.State.Running}}' \"$1\"",
                    &[SIDECAR_C],
                )? != "true"
            {
                return Err(format!("the sidecar did not listen: {logs}"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let ip = self
            .host
            .text("docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' \"$1\"", &[&self.host.container])?
            .split_whitespace()
            .next()
            .ok_or("the game container has no address")?
            .to_owned();
        self.forward.stop();
        self.forward = Forward::start(&self.host.ssh, self.local, &ip, API_PORT)?;
        Ok(())
    }

    fn freeze_sidecar(&mut self, frozen: bool) -> Result<(), String> {
        if frozen {
            LiveTarget::no_players(self)?;
        }
        let verb = if frozen { "pause" } else { "unpause" };
        self.host.sh(
            &format!(
                "docker {verb} \"$1\" >/dev/null 2>&1 || {}",
                if frozen { "false" } else { "true" }
            ),
            &[SIDECAR_C],
            b"",
        )?;
        Ok(())
    }

    fn freeze_dcs(&mut self, frozen: bool) -> Result<(), String> {
        if frozen {
            self.no_players()?;
            self.host.sh(
                "docker pause \"$1\" >/dev/null",
                &[&self.host.container],
                b"",
            )?;
        } else {
            // Unpause only a paused container.
            self.host.sh(
                "[ \"$(docker inspect -f '{{.State.Paused}}' \"$1\")\" = true ] && docker unpause \"$1\" >/dev/null || true",
                &[&self.host.container],
                b"",
            )?;
        }
        Ok(())
    }

    fn dcs_groups(&mut self) -> Result<Option<Vec<DcsGroup>>, String> {
        let v = self.eval("server", "s18-groups", GROUPS_LUA)?;
        let mut out = Vec::new();
        for g in v.as_array().map(Vec::as_slice).unwrap_or_default() {
            let units = g["units"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|u| DcsUnit {
                    name: u["name"].as_str().unwrap_or_default().to_owned(),
                    north: u["x"].as_f64().unwrap_or(f64::NAN),
                    east: u["z"].as_f64().unwrap_or(f64::NAN),
                    alt: u["y"].as_f64().unwrap_or(f64::NAN),
                    in_air: u["air"] == json!(true),
                })
                .collect();
            out.push(DcsGroup {
                name: g["name"].as_str().unwrap_or_default().to_owned(),
                units,
            });
        }
        Ok(Some(out))
    }

    fn starts_mark(&mut self) -> Option<usize> {
        Some(crate::live::hook_mark(&self.run.remote))
    }

    fn starts_since(&mut self, mark: usize) -> usize {
        crate::live::simulation_starts(&self.run.remote, mark)
    }

    fn paused(&mut self) -> Result<Option<bool>, String> {
        let v = self.eval("hook", "s18-paused", "return DCS.getPause()")?;
        v.as_bool()
            .map(Some)
            .ok_or_else(|| format!("DCS.getPause() gave {v}"))
    }

    fn players(&mut self) -> Result<Option<usize>, String> {
        self.run.remote.players().map(Some)
    }

    fn remove_unit(&mut self, unit: &str) -> Result<(), String> {
        self.no_players()?;
        let v = self.eval(
            "server",
            "s18-remove",
            &format!(
                "local u = Unit.getByName({})\nif not u then return 'none' end\nu:destroy()\nreturn 'ok'\n",
                lua_string(unit)
            ),
        )?;
        if v != json!("ok") {
            return Err(format!("no unit {unit} to remove: {v}"));
        }
        Ok(())
    }

    fn kill_unit(&mut self, unit: &str) -> Result<(), String> {
        self.no_players()?;
        let v = self.eval(
            "server",
            "s18-kill",
            &format!(
                "local u = Unit.getByName({})\nif not u then return 'none' end\n\
                 trigger.action.explosion(u:getPoint(), 300)\nreturn 'ok'\n",
                lua_string(unit)
            ),
        )?;
        if v != json!("ok") {
            return Err(format!("no unit {unit} to kill: {v}"));
        }
        Ok(())
    }

    fn integer_keys(&mut self, units: &[String]) -> Result<Value, String> {
        let list = units
            .iter()
            .map(|u| lua_string(u))
            .collect::<Vec<_>>()
            .join(", ");
        self.eval(
            "server",
            "s18-keys",
            &INTEGER_KEYS_LUA.replace("__UNITS__", &list),
        )
    }

    fn unit_state(&mut self, units: &[String]) -> Result<Value, String> {
        let list = units
            .iter()
            .map(|u| lua_string(u))
            .collect::<Vec<_>>()
            .join(", ");
        self.eval(
            "server",
            "s18-units",
            &UNIT_STATE_LUA.replace("__UNITS__", &list),
        )
    }

    fn option_ids(&mut self) -> Result<Value, String> {
        self.eval(
            "server",
            "s18-options",
            "return { ground_roe = AI.Option.Ground.id.ROE, ground_hold = AI.Option.Ground.val.ROE.WEAPON_HOLD,\n\
             air_roe = AI.Option.Air.id.ROE, air_hold = AI.Option.Air.val.ROE.WEAPON_HOLD }",
        )
    }

    fn timings(&mut self) -> Result<Vec<Value>, String> {
        let text = format!("{}\n{}", self.logs, self.container_logs());
        Ok(timing_lines(&text))
    }

    fn on_land(&mut self, points: &[(f64, f64)]) -> Result<Vec<bool>, String> {
        let pts = points
            .iter()
            .map(|(n, e)| format!("{{{n}, {e}}}"))
            .collect::<Vec<_>>()
            .join(", ");
        let v = self.eval(
            "server",
            "s18-land",
            &format!(
                "local out = {{}}\nfor i, p in ipairs({{{pts}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n"
            ),
        )?;
        // land.SurfaceType: LAND 1, SHALLOW_WATER 2, WATER 3, ROAD 4, RUNWAY 5.
        let types = v.as_array().cloned().unwrap_or_default();
        if types.len() != points.len() {
            return Err(format!(
                "land check: {} answers for {} points",
                types.len(),
                points.len()
            ));
        }
        Ok(types
            .iter()
            .map(|t| matches!(t.as_u64(), Some(1 | 4)))
            .collect())
    }
}

/// The timing lines in a log.
fn timing_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| l.contains("\"ev\":\"commit_timings\""))
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}

fn nanoprobe_api_timings_var() -> &'static str {
    "NANOPROBE_COMMIT_TIMINGS"
}

/// Every group of every coalition whose name starts as a platform name.
const GROUPS_LUA: &str = r#"
local out = {}
for _, side in ipairs({ 0, 1, 2 }) do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    local ok, n = pcall(g.getName, g)
    if ok and type(n) == 'string' and n:sub(1, 2) == 'np' then
      local units = {}
      for _, u in ipairs(g:getUnits() or {}) do
        local p = u:getPoint()
        units[#units + 1] = { name = u:getName(), air = u:inAir(), x = p.x, y = p.y, z = p.z }
      end
      out[#out + 1] = { name = n, units = units }
    end
  end
end
return out
"#;

/// DCS's reads of each unit named: its callsign and life.
const UNIT_STATE_LUA: &str = r#"
local out = {}
for _, n in ipairs({ __UNITS__ }) do
  local u = Unit.getByName(n)
  if u then
    local okc, cs = pcall(u.getCallsign, u)
    out[n] = { callsign = okc and cs or nil, life = u:getLife(), life0 = u:getLife0() }
  end
end
return out
"#;

/// String keys that look like integers in the tables DCS hands the restore
/// path: each unit's `getDesc` and `getPosition`, and the
/// inventory of the first three airbases' warehouses.
const INTEGER_KEYS_LUA: &str = r#"
local bad, seen, tables = {}, 0, 0
local function walk(v, path, depth)
  if type(v) ~= 'table' or depth > 10 or seen > 200000 then return end
  tables = tables + 1
  for k, x in pairs(v) do
    seen = seen + 1
    if type(k) == 'string' and (k:match('^%-?[1-9]%d*$') or k == '0') and #bad < 50 then
      bad[#bad + 1] = path .. '.' .. k
    end
    walk(x, path .. '.' .. tostring(k), depth + 1)
  end
end
local read = {}
for _, n in ipairs({ __UNITS__ }) do
  local u = Unit.getByName(n)
  if u then
    for _, m in ipairs({ 'getDesc', 'getPosition' }) do
      local ok, v = pcall(u[m], u)
      if ok then
        local before = seen
        walk(v, n .. ':' .. m, 0)
        read[#read + 1] = { what = n .. ':' .. m, keys = seen - before }
      end
    end
  end
end
local abs = world.getAirbases() or {}
for i = 1, math.min(#abs, 3) do
  local ok, inv = pcall(function() return abs[i]:getWarehouse():getInventory() end)
  if ok then
    local name = abs[i]:getName()
    local before = seen
    walk(inv, name .. ':getInventory', 0)
    read[#read + 1] = { what = name .. ':getInventory', keys = seen - before }
  end
end
return { string_integer_keys = bad, keys = seen, tables = tables, read = read }
"#;

/// The scenarios against the fake sidecar here.
fn rehearse(
    repository: &Path,
    rt: &tokio::runtime::Runtime,
    folder: &Path,
    which: &[&str],
    tag: &str,
    results: &mut Value,
) -> Result<(), String> {
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-q", "-p", "nanoprobe-fake-sidecar"])
        .current_dir(repository)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("nanoprobe-fake-sidecar did not build".into());
    }
    let target_dir =
        std::env::var("CARGO_TARGET_DIR").map_or_else(|_| repository.join("target"), PathBuf::from);
    let program = target_dir.join("debug/nanoprobe-fake-sidecar");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| e.to_string())?
        .port();
    let mut t = FakeTarget {
        program,
        store: folder.join("store"),
        log: folder.join("fake-sidecar.log"),
        port,
        child: None,
    };
    t.start_sidecar()?;
    let r = scenarios::run(rt, &mut t, which, tag, results);
    t.stop();
    r
}

struct FakeTarget {
    program: PathBuf,
    store: PathBuf,
    log: PathBuf,
    port: u16,
    child: Option<Child>,
}

impl FakeTarget {
    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn signal(&self, sig: &str) -> Result<(), String> {
        if let Some(c) = &self.child {
            Command::new("kill")
                .args([sig, &c.id().to_string()])
                .status()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

impl Target for FakeTarget {
    fn theater(&self) -> String {
        THEATER.into()
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn token(&self) -> Option<String> {
        None
    }

    fn live(&self) -> bool {
        false
    }

    fn hold(&mut self, seconds: f64) -> Result<(), String> {
        std::thread::sleep(Duration::from_secs_f64((seconds / 5.0).min(4.0)));
        Ok(())
    }

    fn pause_wait(&mut self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64((seconds / 5.0).min(4.0)));
    }

    fn reload(&mut self) -> Result<(), String> {
        self.kill_sidecar()
    }

    fn reload_restarts_sidecar(&self) -> bool {
        true
    }

    fn kill_dcs(&mut self) -> Result<(), String> {
        self.kill_sidecar()
    }

    fn kill_sidecar(&mut self) -> Result<(), String> {
        self.kill_sidecar_stopped()?;
        self.start_sidecar()
    }

    fn kill_sidecar_stopped(&mut self) -> Result<(), String> {
        self.stop();
        Ok(())
    }

    fn start_sidecar(&mut self) -> Result<(), String> {
        std::fs::create_dir_all(&self.store).map_err(|e| e.to_string())?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|e| e.to_string())?;
        let child = Command::new(&self.program)
            .args([
                "--listen",
                &format!("127.0.0.1:{}", self.port),
                "--theater",
                THEATER,
                "--store",
            ])
            .arg(&self.store)
            .env(nanoprobe_api_timings_var(), "2")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("{}: {e}", self.program.display()))?;
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            if Instant::now() > deadline {
                return Err("the fake sidecar did not listen".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn freeze_sidecar(&mut self, frozen: bool) -> Result<(), String> {
        self.signal(if frozen { "-STOP" } else { "-CONT" })
    }

    fn freeze_dcs(&mut self, _frozen: bool) -> Result<(), String> {
        // The fake's DCS is in the sidecar's process.
        Ok(())
    }

    fn dcs_groups(&mut self) -> Result<Option<Vec<DcsGroup>>, String> {
        Ok(None)
    }

    fn paused(&mut self) -> Result<Option<bool>, String> {
        Ok(None)
    }

    fn players(&mut self) -> Result<Option<usize>, String> {
        Ok(None)
    }

    fn remove_unit(&mut self, _unit: &str) -> Result<(), String> {
        Err("a scripted removal needs DCS".into())
    }

    fn kill_unit(&mut self, _unit: &str) -> Result<(), String> {
        Err("a kill needs DCS".into())
    }

    fn integer_keys(&mut self, _units: &[String]) -> Result<Value, String> {
        Ok(json!({ "string_integer_keys": [], "keys": 0, "on": "the fake" }))
    }

    /// The life of each unit as the fake DCS holds it (the fake sidecar
    /// logs it), 1 for a unit no `DAMAGE` set; `life0` is 1.
    fn unit_state(&mut self, units: &[String]) -> Result<Value, String> {
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lives = log
            .lines()
            .rev()
            .filter(|l| l.contains("\"ev\":\"fake_lives\""))
            .find_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
            .map_or(json!({}), |v| v["lives"].clone());
        let mut out = serde_json::Map::new();
        for u in units {
            let entity = nanoprobe_transport::names::parse(u).map(|p| p.entity.to_string());
            let percent = entity
                .and_then(|e| lives[e.as_str()].as_f64())
                .unwrap_or(100.0);
            out.insert(u.clone(), json!({ "life": percent / 100.0, "life0": 1.0 }));
        }
        Ok(Value::Object(out))
    }

    fn option_ids(&mut self) -> Result<Value, String> {
        Ok(json!({ "ground_roe": 0, "ground_hold": 4, "air_roe": 0, "air_hold": 4 }))
    }

    fn timings(&mut self) -> Result<Vec<Value>, String> {
        Ok(timing_lines(
            &std::fs::read_to_string(&self.log).unwrap_or_default(),
        ))
    }

    fn on_land(&mut self, points: &[(f64, f64)]) -> Result<Vec<bool>, String> {
        Ok(vec![true; points.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lua_string_holds_any_text() {
        assert_eq!(lua_string("Z:\\a\\b.miz"), "[[Z:\\a\\b.miz]]");
        assert_eq!(lua_string("a]]b"), "[=[a]]b]=]");
        assert_eq!(lua_string("a]"), "[=[a]]=]");
        assert_eq!(lua_string("\nx"), "[[\n\nx]]");
    }

    #[test]
    fn timing_lines_are_read_from_a_log() {
        let log = "listening on x\n{\"ev\":\"commit_timings\",\"commits\":3,\"median_us\":5,\"p99_us\":9,\"max_us\":9,\"checkpoints\":[],\"units\":2}\nother\n";
        let l = timing_lines(log);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0]["units"], 2);
    }
}
