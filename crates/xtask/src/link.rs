//! `link <name>`: the S7 exit test, on a docker-wine target or a Windows
//! target reached over SSH.
//!
//! The target runs the deployed build, with the eval channel (`deploy
//! --dev`). The test copies two programs to the target: `link-probe`
//! (crates/fake-agent), which attaches as the sidecar and prints what it reads
//! with wall times, and `nanoprobe` (crates/cli), the inspector. On a
//! docker-wine target they are static Linux programs run in the game
//! container, which shares DCS's network and `/dev/shm`. On Windows they are
//! Windows programs run by the SSH account, in the profile's
//! `nanoprobe/dev/link` folder, and the mapped file sits in the profile where
//! that account can open it. Then, in order:
//!
//! 1. the probe attaches and its commands are answered;
//! 2. `nanoprobe inspect` shows the handshake in the file;
//! 3. `lua/live/reloads.lua` reloads the mission N times through the eval
//!    channel; the probe times how long after each mission runs it reads
//!    the agent's `LIFECYCLE` saying so;
//! 4. a shim reload request, which the sidecar sees as `shim reloaded`;
//! 5. the kill switch: the time from creating `disable` to `DISABLED` in the
//!    status block, both on the target's clock;
//! 6. the kill switch is removed, the programs removed, and DCS restarted
//!    (docker-wine) or closed (Windows, which is someone's to play on).
//!
//! A Windows client starts at its main menu with no mission, so the test
//! first hosts a private session there (`lua/experiments/alone/host-session.lua`)
//! with a shell mission from `templates/caucasus.miz`.
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
use crate::{bundle, eval, live, miz, remote, ssh, windows};

/// Where the programs go in the container.
const DIR: &str = "/tmp/nanoprobe-link";
/// Where the programs go in a Windows profile.
const WIN_DIR: &str = "nanoprobe/dev/link";
/// Create the file `$1` and print the wall time just before, in milliseconds
/// since the Unix epoch, on the container's clock.
const KILL_SWITCH: &str = r#"python3 -c 'import sys, time; t = int(time.time() * 1000); open(sys.argv[1], "w").close(); print(t)' "$1""#;

/// The longest one mission load may take before the reload step gives up.
/// A limit, not an expectation: load times vary with hardware and mission.
const PER_LOAD_S: u64 = 300;
/// The pass limit for a reload and for the kill switch.
const LIMIT_MS: u64 = 2000;

/// Where the probe and the inspector run, and how the test reaches them.
trait Peer {
    /// The DCS profile.
    fn profile(&self) -> &dyn Profile;
    /// Copy the probe and the inspector to the target.
    fn upload(&self) -> Result<(), String>;
    /// Start the probe in the background.
    fn start_probe(&mut self, map: &str, port: &str, token: &str) -> Result<(), String>;
    /// The probe's lines so far.
    fn lines(&self) -> Result<Vec<Value>, String>;
    /// `nanoprobe inspect`: whether it succeeded, and its text.
    fn inspect(&self, map: &str, records: &str) -> Result<(bool, String), String>;
    /// Create the kill switch and return the wall time just before, in
    /// milliseconds, on the target's clock.
    fn kill_switch(&self) -> Result<u64, String>;
    /// The probe's standard error, and how its session ended where that is
    /// known.
    fn stderr(&self) -> String;
    /// Whether the probe may still be running. A probe that ended before a
    /// step's condition held ends that step at once.
    fn probe_alive(&self) -> bool {
        true
    }
    /// Stop the probe and remove the programs. Returns whether that worked.
    fn cleanup(&mut self) -> bool;
}

/// The docker-wine side: programs in the game container.
struct Container {
    remote: remote::Remote,
}

impl Peer for Container {
    fn profile(&self) -> &dyn Profile {
        &self.remote
    }

    fn upload(&self) -> Result<(), String> {
        let (probe, inspector) = build_linux()?;
        upload_container(&self.remote, DIR, &probe, "link-probe")?;
        upload_container(&self.remote, DIR, &inspector, "nanoprobe")
    }

    fn start_probe(&mut self, map: &str, port: &str, token: &str) -> Result<(), String> {
        // It runs until the kill switch, or the clean-up's pkill: a load has
        // no fixed duration, so no step's time is known in advance.
        let start = self.remote.shell(
            &format!(
                "nohup {DIR}/link-probe --map \"$1\" --port \"$2\" --token-file \"$3\" --seconds 86400 \
                 > {DIR}/probe.jsonl 2> {DIR}/probe.err < /dev/null &"
            ),
            &[map, port, token],
            b"",
        )?;
        match start.code {
            Some(0) => Ok(()),
            _ => Err(format!("could not start the probe: {}", start.stderr)),
        }
    }

    fn lines(&self) -> Result<Vec<Value>, String> {
        let output = self.remote.shell(
            &format!("cat {DIR}/probe.jsonl 2>/dev/null || true"),
            &[],
            b"",
        )?;
        Ok(parse_lines(&output.stdout))
    }

    fn inspect(&self, map: &str, records: &str) -> Result<(bool, String), String> {
        let o = self.remote.shell(
            &format!("{DIR}/nanoprobe inspect \"$1\" --records \"$2\""),
            &[map, records],
            b"",
        )?;
        Ok((
            o.code == Some(0),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        ))
    }

    fn kill_switch(&self) -> Result<u64, String> {
        let disable = self.remote.full_path("nanoprobe/disable");
        let created = self.remote.shell(KILL_SWITCH, &[&disable], b"")?;
        String::from_utf8_lossy(&created.stdout)
            .trim()
            .parse()
            .map_err(|_| format!("could not create the kill switch: {}", created.stderr))
    }

    fn stderr(&self) -> String {
        self.remote
            .shell(
                &format!("cat {DIR}/probe.err 2>/dev/null || true"),
                &[],
                b"",
            )
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn cleanup(&mut self) -> bool {
        // The folder goes first: a pattern pkill matches is also in this
        // shell's own command line, which it may end.
        let cleaned = self.remote.shell(
            &format!("rm -rf {DIR}; pkill -f '[l]ink-probe --map' || true"),
            &[],
            b"",
        );
        matches!(cleaned, Ok(ref o) if o.code == Some(0))
    }
}

/// The Windows side: programs in the profile, run by the SSH account. The
/// probe runs under one SSH session held open by a thread, since Windows'
/// SSH server ends a session's processes with it.
struct WindowsPeer {
    windows: windows::Windows,
    probe: Option<std::thread::JoinHandle<Result<ssh::Output, String>>>,
    /// How the probe's SSH session ended, once joined.
    session: Option<String>,
}

impl WindowsPeer {
    fn rel(name: &str) -> String {
        format!("{WIN_DIR}/{name}")
    }
}

impl Peer for WindowsPeer {
    fn profile(&self) -> &dyn Profile {
        &self.windows
    }

    fn upload(&self) -> Result<(), String> {
        // Nothing from an earlier run, whose clean-up may have failed, may be
        // read as this probe's output.
        self.windows.remove_tree(WIN_DIR)?;
        let (probe, inspector) = build_windows()?;
        for (local, name) in [(probe, "link-probe.exe"), (inspector, "nanoprobe.exe")] {
            let bytes = std::fs::read(&local).map_err(|e| format!("{}: {e}", local.display()))?;
            self.windows.write(&Self::rel(name), &bytes)?;
        }
        Ok(())
    }

    fn start_probe(&mut self, map: &str, port: &str, token: &str) -> Result<(), String> {
        let windows = self.windows.clone();
        let (map, port, token) = (map.to_owned(), port.to_owned(), token.to_owned());
        let script = format!(
            r#"$exe = Full '{WIN_DIR}/link-probe.exe'
$line = '--map "' + $a0 + '" --port ' + $a1 + ' --token-file "' + $a2 + '" --seconds 86400'
$p = Start-Process -FilePath $exe -ArgumentList $line -NoNewWindow -PassThru -Wait `
    -RedirectStandardOutput (Full '{WIN_DIR}/probe.jsonl') -RedirectStandardError (Full '{WIN_DIR}/probe.err')
exit $p.ExitCode"#
        );
        self.probe = Some(std::thread::spawn(move || {
            windows.powershell(&script, &[&map, &port, &token])
        }));
        Ok(())
    }

    fn lines(&self) -> Result<Vec<Value>, String> {
        Ok(self
            .windows
            .read(&Self::rel("probe.jsonl"))?
            .map(|b| parse_lines(&b))
            .unwrap_or_default())
    }

    fn inspect(&self, map: &str, records: &str) -> Result<(bool, String), String> {
        let o = self.windows.powershell(
            &format!(
                "$o = & (Full '{WIN_DIR}/nanoprobe.exe') inspect $a0 --records $a1 | Out-String\n$c = $LASTEXITCODE\nEmitText $o\nexit $c"
            ),
            &[map, records],
        )?;
        Ok((
            o.code == Some(0),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        ))
    }

    fn kill_switch(&self) -> Result<u64, String> {
        let o = self.windows.powershell(
            "$t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()\n[IO.File]::WriteAllBytes((Full 'nanoprobe/disable'), [byte[]]@())\nEmitText ([string]$t)",
            &[],
        )?;
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .map_err(|_| format!("could not create the kill switch: {}", o.stderr))
    }

    fn stderr(&self) -> String {
        let mut text = self
            .windows
            .read(&Self::rel("probe.err"))
            .ok()
            .flatten()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if let Some(session) = &self.session {
            text.push_str(&format!("\nprobe session: {session}"));
        }
        text
    }

    fn probe_alive(&self) -> bool {
        self.probe.as_ref().is_some_and(|p| !p.is_finished())
    }

    fn cleanup(&mut self) -> bool {
        // Stop the probe, a few tries, and wait for its session at most 20 s:
        // a probe left running ends by itself after its --seconds.
        let mut stopped = false;
        for _ in 0..3 {
            if self
                .windows
                .powershell(
                    "Stop-Process -Name link-probe -ErrorAction SilentlyContinue\nexit 0",
                    &[],
                )
                .is_ok()
            {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        let mut joined = true;
        if let Some(probe) = self.probe.take() {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !probe.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
            if probe.is_finished() {
                self.session = Some(match probe.join() {
                    Ok(Ok(o)) => format!("exit {:?} {}", o.code, o.stderr),
                    Ok(Err(e)) => e,
                    Err(_) => "the session thread panicked".into(),
                });
            } else {
                joined = false;
                self.session = Some("still running after 20 s; not waited for".into());
            }
        }
        stopped && joined && self.windows.remove_tree(WIN_DIR).is_ok()
    }
}

/// `link <name> [--reloads N]`.
pub fn command(name: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let reloads: u32 = match rest {
        [] => 10,
        ["--reloads", n] => n.parse().map_err(|_| format!("--reloads {n:?}"))?,
        _ => return Err(format!("unknown options: {}", rest.join(" "))),
    };
    let target = crate::target(name)?;
    let mut peer: Box<dyn Peer> = match &target {
        Target::DockerWine { .. } => Box::new(Container {
            remote: crate::remote(name)?.0,
        }),
        Target::WindowsSsh { ssh, profile, .. } => Box::new(WindowsPeer {
            windows: windows::Windows::connect(ssh, profile)?,
            probe: None,
            session: None,
        }),
        Target::Local { .. } => return Err("link needs a docker-wine or windows-ssh target".into()),
    };
    let players = match &target {
        Target::DockerWine { .. } => crate::remote(name)?.0.players()?,
        _ => live::players(peer.profile())?,
    };
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }

    let profile = peer.profile();
    let toml = profile
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target: add `map_path` to the profile and deploy")?;
    let config = AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    let (map, token) = match &target {
        Target::DockerWine { .. } => (
            linux_path(&config.transport.map_path)?,
            crate::remote(name)?.0.full_path(TOKEN),
        ),
        Target::WindowsSsh { profile, .. } => {
            if config.transport.map_path.is_empty() {
                return Err(
                    "agent.toml names no map_path: add one to the target profile and deploy".into(),
                );
            }
            (
                config.transport.map_path.clone(),
                format!(
                    "{}\\{}",
                    profile.trim_end_matches('\\'),
                    TOKEN.replace('/', "\\")
                ),
            )
        }
        Target::Local { .. } => unreachable!("refused above"),
    };
    let port = config.transport.port.to_string();

    // The running DCS must have loaded the deployed build, or the test
    // measures another one. Its agent.toml and token are read at start too.
    let active = install::manifest(profile)?
        .active
        .ok_or("nothing is deployed on the target")?;
    let running = profile
        .read(RUNNING)?
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned());
    if running.as_deref() != Some(&format!("build={active}")) {
        return Err(format!(
            "DCS runs {}, not the deployed {active}: run `cargo xtask restart {name}` first",
            running.as_deref().unwrap_or("no hook")
        ));
    }
    if let Target::WindowsSsh {
        ssh, profile: path, ..
    } = &target
    {
        at_main_menu(&windows::Windows::connect(ssh, path)?, name)?;
    }

    // From here on the target is changed, so every failure still runs the
    // restore below and writes the report.
    let windows_target = matches!(target, Target::WindowsSsh { .. });
    let result = (|| {
        if windows_target {
            host_session(peer.profile())?;
        }
        peer.upload()?;
        run(peer.as_mut(), &map, &port, &token, &active, reloads)
    })();
    // A failed run keeps what the probe printed, read before the clean-up
    // removes it.
    let mut report = result.unwrap_or_else(|e| {
        json!({
            "verdict": { "pass": false },
            "error": e,
            "probe": peer.lines().unwrap_or_default(),
        })
    });
    report["probe_stderr"] = json!(peer.stderr());
    report["build"] = json!(active);
    // Put the target back whatever happened: no kill switch, no request
    // left, the programs gone, and DCS restarted (docker-wine) or closed
    // (Windows).
    let _ = peer.profile().remove_file("nanoprobe/disable");
    let _ = peer.profile().remove_file("nanoprobe/reload");
    report["cleaned"] = json!(peer.cleanup());
    let restored = match &target {
        Target::DockerWine {
            ssh: destination,
            restart,
            ..
        } => {
            let remote = crate::remote(name)?.0;
            match restart {
                Some(restart) => {
                    live::restart(&remote, remote.players().unwrap_or(1), true, || {
                        let output = ssh::run(destination, restart, b"")?;
                        match output.code {
                            Some(0) => Ok(()),
                            _ => Err(format!("the restart command failed: {}", output.stderr)),
                        }
                    })
                }
                None => Err(format!("target {name:?} has no `restart` key")),
            }
        }
        Target::WindowsSsh {
            ssh,
            profile,
            stop_task,
            ..
        } => close_dcs(ssh, profile, stop_task),
        Target::Local { .. } => unreachable!("refused above"),
    };
    if let Some(e) = report.get("error") {
        println!("failed: {}", e.as_str().unwrap_or_default());
    }
    report["restored"] = match restored {
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

/// Refuse unless DCS runs on the Windows PC and sits at its main menu: the
/// hook started and has seen no mission load since. A client someone plays
/// is not the test's to use.
fn at_main_menu(windows: &windows::Windows, name: &str) -> Result<(), String> {
    if windows.dcs_processes()? == 0 {
        return Err(format!(
            "DCS is not running on {name}: run `cargo xtask restart {name}` first"
        ));
    }
    let log = windows
        .read("nanoprobe/logs/hook.log")?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let since = log
        .rfind("\"msg\":\"hook loaded\"")
        .map_or("", |i| &log[i..]);
    if since.is_empty() || since.contains("\"msg\":\"mission load begin\"") {
        return Err(format!(
            "DCS on {name} is not at its main menu since the hook started: not using it. \
             Run `cargo xtask restart {name}` first"
        ));
    }
    Ok(())
}

/// Host a private session on a Windows client at its main menu, with a shell
/// mission, and wait until the shim has started in it.
fn host_session(profile: &dyn Profile) -> Result<(), String> {
    let repo = crate::repository();
    let template = std::fs::read(repo.join("templates/caucasus.miz"))
        .map_err(|e| format!("templates/caucasus.miz: {e}"))?;
    let shell = miz::generate(&template, &miz::Changes::default())?;
    profile.write("nanoprobe/dev/missions/np-shell.miz", &shell)?;
    let started = |p: &dyn Profile| {
        p.read("nanoprobe/logs/hook.log")
            .ok()
            .flatten()
            .map_or(0, |t| {
                String::from_utf8_lossy(&t)
                    .matches("\"msg\":\"shim started\"")
                    .count()
            })
    };
    let before = started(profile);
    let source = std::fs::read(repo.join("lua/experiments/alone/host-session.lua"))
        .map_err(|e| format!("host-session.lua: {e}"))?;
    eval::run(
        profile,
        "gui",
        "host-session",
        &source,
        Duration::from_secs(300),
    )?;
    println!("hosting a private session; waiting for the shim to start");
    let deadline = Instant::now() + Duration::from_secs(PER_LOAD_S);
    while started(profile) <= before {
        if Instant::now() > deadline {
            return Err("the hosted session did not start the shim".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(())
}

/// Close DCS on the Windows PC with its stop task, and wait until it is gone.
fn close_dcs(ssh: &str, profile: &str, stop_task: &str) -> Result<(), String> {
    let windows = windows::Windows::connect(ssh, profile)?;
    windows.run_task(stop_task)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while windows.dcs_processes()? > 0 {
        if Instant::now() > deadline {
            return Err("DCS still runs 120 s after the stop task".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(())
}

/// The Linux path of a Windows path on Wine's `Z:` drive, which is the
/// container's root.
pub(crate) fn linux_path(windows: &str) -> Result<String, String> {
    let rest = windows
        .strip_prefix("Z:")
        .or_else(|| windows.strip_prefix("z:"))
        .ok_or_else(|| {
            format!("map_path {windows:?} is not on Z:, so the container cannot reach it")
        })?;
    Ok(rest.replace('\\', "/"))
}

/// Build `bins`, each a package and a binary, for `target`, with cargo-xwin
/// for Windows. Returns their paths, in order.
fn build_for(
    target: &str,
    xwin: bool,
    dir: &str,
    ext: &str,
    bins: &[(&str, &str)],
) -> Result<Vec<PathBuf>, String> {
    let repo = crate::repository();
    let target_dir = repo.join(dir);
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    if xwin && !cfg!(windows) {
        command.arg("xwin");
    }
    command.args(["build", "--release", "--target", target]);
    for (package, bin) in bins {
        command.args(["-p", package, "--bin", bin]);
    }
    let status = command
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(&repo)
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("the {target} build failed ({status})"));
    }
    let out = target_dir.join(target).join("release");
    Ok(bins
        .iter()
        .map(|(_, bin)| out.join(format!("{bin}{ext}")))
        .collect())
}

/// The probe and the inspector.
const PROBE_BINS: &[(&str, &str)] = &[
    ("nanoprobe-fake-agent", "link-probe"),
    ("nanoprobe-cli", "nanoprobe"),
];

/// Build `bins` as static Linux programs, for the game container.
pub(crate) fn build_linux_bins(bins: &[(&str, &str)]) -> Result<Vec<PathBuf>, String> {
    build_for("x86_64-unknown-linux-musl", false, "target/linux", "", bins)
}

fn build_linux() -> Result<(PathBuf, PathBuf), String> {
    let mut paths = build_linux_bins(PROBE_BINS)?;
    let inspector = paths.pop().ok_or("no inspector built")?;
    Ok((paths.pop().ok_or("no probe built")?, inspector))
}

fn build_windows() -> Result<(PathBuf, PathBuf), String> {
    let mut paths = build_for(
        "x86_64-pc-windows-msvc",
        true,
        "target/windows-tools",
        ".exe",
        PROBE_BINS,
    )?;
    let inspector = paths.pop().ok_or("no inspector built")?;
    Ok((paths.pop().ok_or("no probe built")?, inspector))
}

/// Copy `local` into the container as `dir/name`, executable.
pub(crate) fn upload_container(
    remote: &remote::Remote,
    dir: &str,
    local: &Path,
    name: &str,
) -> Result<(), String> {
    let bytes = std::fs::read(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let script = format!(
        "set -e; mkdir -p {dir}; cat > {dir}/$1.partial; chmod 755 {dir}/$1.partial; mv -f {dir}/$1.partial {dir}/$1"
    );
    let output = remote.shell(&script, &[name], &bytes)?;
    match output.code {
        Some(0) => Ok(()),
        code => Err(format!("upload {name}: exit {code:?}: {}", output.stderr)),
    }
}

pub(crate) fn parse_lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Poll the probe's lines every half second until `done` holds, or `limit`.
fn wait_for(
    peer: &dyn Peer,
    what: &str,
    limit: Duration,
    done: impl Fn(&[Value]) -> bool,
) -> Result<Vec<Value>, String> {
    let start = Instant::now();
    loop {
        let alive = peer.probe_alive();
        let l = peer.lines()?;
        if done(&l) {
            return Ok(l);
        }
        if !alive {
            return Err(format!(
                "{what}: the probe ended first; it printed:\n{}",
                l.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
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
    peer: &mut dyn Peer,
    map: &str,
    port: &str,
    token: &str,
    active: &str,
    reloads: u32,
) -> Result<Value, String> {
    // The probe runs in the background and writes its lines to a file; each
    // step reads the file.
    peer.start_probe(map, port, token)?;
    let peer: &dyn Peer = peer;

    // 1. The handshake. The probe announces campaign 7; its command for
    // campaign 7 is answered UNKNOWN_OPCODE (20), and its command for
    // campaign 8 WRONG_CAMPAIGN (24).
    let answered = |l: &[Value], seq: u64, code: u64| {
        ev(l, "result").any(|e| e["seq"] == json!(seq) && e["code"] == json!(code))
    };
    let l = wait_for(
        peer,
        "the answered commands",
        Duration::from_secs(60),
        |l| answered(l, 1, 20) && answered(l, 2, 24),
    )?;
    let hello = ev(&l, "hello").next().cloned().unwrap_or(Value::Null);
    println!("attached: {hello}");

    // 2. The inspector, while attached.
    let (inspected, inspect_text) = peer.inspect(map, "5")?;
    let handshake_seen =
        inspected && inspect_text.contains("ATTACHED") && hello["ev"] == json!("hello");
    println!("{inspect_text}");

    // 3. The reloads.
    let profile = peer.profile();
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
    let gen_before = ev(&peer.lines()?, "status")
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
    wait_for(peer, "the last reload read", Duration::from_secs(30), |l| {
        ev(l, "lifecycle").any(|e| {
            e["gen"].as_u64() == Some(last_gen) && matches!(e["state"].as_u64(), Some(3 | 4))
        })
    })?;

    // 4. A shim reload request.
    profile.write("nanoprobe/reload", b"")?;
    let shim_reloaded = wait_for(peer, "the shim reload", Duration::from_secs(10), |l| {
        ev(l, "log").any(|e| e["target"] == json!("hook") && e["msg"] == json!("shim reloaded"))
    })
    .is_ok();

    // 5. The kill switch: the time it was created, on the target's clock.
    let created_ms = peer.kill_switch()?;
    let l = wait_for(peer, "the probe's end", Duration::from_secs(30), |l| {
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
    let (_, after_text) = peer.inspect(map, "0")?;

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
