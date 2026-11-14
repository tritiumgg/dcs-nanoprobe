//! Development and test tooling for nanoprobe, run as `cargo xtask <command>`.
//!
//! It is built from source and never released. Every target it acts on is a
//! profile in a file outside the repository (see [`profile`]).

mod bundle;
mod catalogue;
mod chaos;
mod check;
mod client;
mod eval;
mod faults;
mod hello;
mod install;
mod link;
mod live;
mod miz;
mod peerkill;
mod probes;
mod profile;
mod proto;
mod remote;
mod sampling;
mod spec;
mod ssh;
mod symbols;
mod windows;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use install::Profile;
use profile::Target;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  target list               List the target profiles
  target check <name>       Read a target's DCS version, Wine version, install and
                            Saved Games profile. Changes nothing on the target.
  deploy <name> [--dll P] [--dev] [--faults]
                            Build the agent DLL (or take P), and install it with the
                            hook into the target's profile as the active version.
                            --dev adds the development mission loader and the
                            eval channel. --faults builds the DLL with fault
                            injection, for `faults`; no other test uses it.
  rollback <name>           Make the previous version active again
  undeploy <name>           Remove everything deploy installed
  roundtrip <name> [--dll P]
                            Deploy twice, undeploy, and check that the profile's
                            tree, outside Logs/, is as it was before
  bundle <name>             Collect the session's logs from the target, redact them,
                            and write a zip under bundles/
  reload <name>             Ask the running hook to reload the shim of the active
                            version without restarting DCS, and wait for its answer.
                            Deploy first to change the Lua; a new DLL needs a restart.
  restart <name>            Restart DCS with the target's `restart` command, only
                            when no player is connected, and wait for the hook
  load <name> <miz>         Load a mission through the development mission loader,
                            and wait until it starts
  run <name> <test> [--dll P] [--template T] [--dir D]
                            Deploy with --dev, restart DCS, load a fresh shell
                            mission from T (default templates/caucasus.miz), run
                            the test, and collect a bundle. <test> is `suite` for
                            every probe, or a text that picks probes by file name
                            as `probes --only` does. Refuses a server with players.
  eval <name> <state> <file> [--timeout S]
                            Run a Lua file in a state of the running DCS through the
                            eval channel, and print the result as JSON
  reset <name>              Run every eval cleanup in every state that holds the prelude
  probes <name> [--dir D] [--only TEXT] [--keep] [--with-players]
                            Run the probes in D (default lua/probes) in their states,
                            print each verdict, clean up, and write
                            bundles/probes-<name>-<time>.json. Refuses a server with
                            players unless --with-players.
  mapped <name>             Experiment E24: run tools/mapped-peer.py in the container
                            and lua/experiments/e24-mapped.lua in DCS together, and
                            write bundles/mapped-<name>-<time>.json
  link <name> [--reloads N] The S7 exit test on a docker-wine target running the
                            deployed --dev build: attach link-probe in the container
                            as the sidecar, check the handshake in the inspector,
                            reload the mission N times (default 10), request a shim
                            reload, set the kill switch, then clear it and restart
                            DCS. Writes bundles/link-<name>-<time>.json. Refuses a
                            server with players.
  gates <name> on|off       Add or remove the net.allow_unsafe_api and
                            net.allow_dostring_in keys of experiment E2 in the
                            profile's Config/autoexec.cfg, between marker lines.
                            Restart DCS afterwards for them to apply.
  symbolize <file>          Resolve the frames of each record in a crash.log, or in a
                            bundle's logs/crash.log, with the kept PDB of its build
  peerkill [--cycles N] [--seed HEX] [--fps N] [--keep DIR]
                            The S6 exit test: kill the fake agent or the stub
                            sidecar N times (default 1000), time each reconnect,
                            and verify the stub's journals. Host only.
  peerkill --verify DIR     Verify the journals a run kept with --keep DIR
  chaos host|<name> [--cycles N] [--seed HEX]
                            The S8 chaos test: kill DCS or the sidecar N times
                            (default 100) at random and time each reconnect. On
                            host, DCS is the mock; on a docker-wine target, the
                            deployed build, killed only while no player is
                            connected. Writes bundles/chaos-<name>-<time>.json.
  faults <name>             The S8 fault test on a docker-wine target running a
                            --dev build: inject each agent fault through the eval
                            channel and each sidecar fault from link-probe, check
                            each is survived or refused with a log line, then
                            restart DCS. Writes bundles/faults-<name>-<time>.json.
  sampling compare <oracle.json> <mirror.json> [--out FILE]
                            Set the oracle's dump (lua/sampling/oracle.lua) against
                            the sidecar's mirror (link-probe --mirror); exit 1 when
                            the mirror does not match
  catalogue <name>          Experiment E9 on a docker-wine target: deploy with --dev,
                            restart DCS, load a fresh shell, record the agent's stream
                            with link-probe, run the scenarios in lua/catalogue, load
                            the shell again, and build the event catalogue under
                            bundles/catalogue-<name>-<time>/. Refuses a server with
                            players.
  catalogue --build <probes.json> [--recording F]... --out STEM
                            Build the field-presence report STEM.md and the catalogue
                            STEM.json from a probes report and stream recordings
  client join <client> <server>
                            Experiment E30: join the docker-wine server <server> from
                            the windows-ssh client <client>, which runs a --dev build
                            at its main menu. Reads the server profile's
                            client_address and client_password_file; the password
                            goes to the client through SSH's standard input and is
                            never printed, logged or kept.
  hello [<name>]            Run the Rust hello campaign against the API server
                            over the fake sim: here, or built for the target and
                            run on its system (the SSH host of a docker-wine
                            target, outside the container; the SSH account of a
                            windows-ssh target), in a temporary folder that is
                            removed after
  proto [--check]           Write the generated API code in crates/proto/src/gen/
                            from proto/ with buf, or with --check fail when it
                            is out of date
  spec [--check]            Write spec/transport.golden from the layout and code tables
                            of the transport specification, or with --check fail
                            when it is out of date
  api-dump <name>           Write lua/mock/api/<state>.txt from each state's globals
  snapshot <name>           List the profile's tree, outside Logs/
  miz <template> <out> [--start-time S]
                            Write a shell mission from an empty Mission Editor
                            template, optionally starting S seconds after midnight

Profiles are read from the file NANOPROBE_TARGETS names, or by default from
~/.config/nanoprobe/targets.toml (%APPDATA%\\nanoprobe\\targets.toml on Windows).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["target", "list"] => target_list(),
        ["target", "check", name] => target_check(name),
        ["deploy", name, rest @ ..] => {
            deploy_options(rest).and_then(|(dll, dev, faults)| deploy(name, dll, dev, faults))
        }
        ["restart", name] => restart(name),
        ["load", name, miz] => load(name, miz),
        ["run", name, test, rest @ ..] => run(name, test, rest),
        ["rollback", name] => rollback(name),
        ["undeploy", name] => undeploy(name),
        ["roundtrip", name, rest @ ..] => dll_option(rest).and_then(|dll| roundtrip(name, dll)),
        ["eval", name, state, file, rest @ ..] => eval_command(name, state, file, rest),
        ["reset", name] => open(name).and_then(|profile| probes::reset(profile.as_ref())),
        ["probes", name, rest @ ..] => probes_command(name, rest),
        ["api-dump", name] => api_dump(name),
        ["snapshot", name] => snapshot_command(name),
        ["miz", template, out, rest @ ..] => miz(template, out, rest),
        ["bundle", name] => bundle(name),
        ["mapped", name] => mapped(name),
        ["link", name, rest @ ..] => link::command(name, rest),
        ["reload", name] => open(name).and_then(|profile| {
            println!("{}", live::reload(profile.as_ref())?);
            Ok(ExitCode::SUCCESS)
        }),
        ["gates", name, "on"] => gates(name, true),
        ["gates", name, "off"] => gates(name, false),
        ["symbolize", file] => symbols::symbolize(Path::new(file)),
        ["peerkill", rest @ ..] => peerkill::command(&repository(), rest),
        ["chaos", name, rest @ ..] => chaos::command(name, rest),
        ["faults", name, rest @ ..] => faults::command(name, rest),
        ["catalogue", rest @ ..] => catalogue::command(&repository(), rest),
        ["sampling", rest @ ..] => sampling::command(&repository(), rest),
        ["client", rest @ ..] => client::command(rest),
        ["hello"] => hello::command(None),
        ["hello", name] => hello::command(Some(name)),
        ["proto"] => proto::command(&repository(), false),
        ["proto", "--check"] => proto::command(&repository(), true),
        ["spec"] => spec::command(&repository(), false),
        ["spec", "--check"] => spec::command(&repository(), true),
        ["help" | "-h" | "--help"] | [] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => Err(format!("unknown command: {}\n\n{USAGE}", args.join(" "))),
    };

    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

/// `--dll P`, `--dev` and `--faults`.
fn deploy_options(rest: &[&str]) -> Result<(Option<PathBuf>, bool, bool), String> {
    let dev = rest.contains(&"--dev");
    let faults = rest.contains(&"--faults");
    let rest: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| *a != "--dev" && *a != "--faults")
        .collect();
    let dll = dll_option(&rest)?;
    if faults && dll.is_some() {
        return Err("--faults builds the DLL; it does not take --dll".into());
    }
    Ok((dll, dev, faults))
}

fn dll_option(rest: &[&str]) -> Result<Option<PathBuf>, String> {
    match rest {
        [] => Ok(None),
        ["--dll", path] => Ok(Some(PathBuf::from(path))),
        _ => Err(format!("unknown options: {}", rest.join(" "))),
    }
}

/// The repository root. xtask is built from source, so its manifest folder
/// is two levels under the root.
fn repository() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest)
        .to_path_buf()
}

fn target(name: &str) -> Result<Target, String> {
    let (path, mut targets) = profile::load()?;
    targets
        .remove(name)
        .ok_or_else(|| format!("no target {name:?} in {}", path.display()))
}

/// The target's Saved Games profile as a [`Profile`].
fn open(name: &str) -> Result<Box<dyn Profile>, String> {
    match target(name)? {
        Target::Local { profile } => Ok(Box::new(install::Local::new(profile)?)),
        Target::DockerWine {
            ssh,
            container,
            profile,
            ..
        } => {
            let profile = profile.ok_or_else(|| {
                format!(
                    "target {name:?} has no `profile` key. Run `cargo xtask target check \
                     {name}` and add the Saved Games profile it prints."
                )
            })?;
            Ok(Box::new(remote::Remote::connect(
                &ssh, &container, &profile,
            )?))
        }
        Target::WindowsSsh { ssh, profile, .. } => {
            Ok(Box::new(windows::Windows::connect(&ssh, &profile)?))
        }
    }
}

/// Cross-build the agent DLL, or on Windows build it natively. With
/// `faults`, the DLL has fault injection (spec/faults.md): only a
/// `--faults` deploy builds it, into target/faults, so the release path keeps
/// the shipping DLL.
fn build_dll(faults: bool) -> Result<PathBuf, String> {
    let repo = repository();
    let dir = if faults {
        repo.join("target/faults")
    } else {
        repo.join("target")
    };
    let args: &[&str] = if cfg!(windows) {
        &["build"]
    } else {
        &["xwin", "build"]
    };
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(args)
        .args([
            "--release",
            "-p",
            "nanoprobe-agent",
            "--target",
            "x86_64-pc-windows-msvc",
        ])
        .args(if faults {
            &["--features", "faults"][..]
        } else {
            &[]
        })
        .arg("--target-dir")
        .arg(&dir)
        .current_dir(&repo)
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "the DLL build failed ({status}). Run it through mise: `mise exec -- cargo xtask ...`"
        ));
    }
    Ok(dir.join("x86_64-pc-windows-msvc/release/nanoprobe.dll"))
}

/// The files to install: the DLL given, or one built with fault injection
/// when `faults`, and the development files when `dev`.
fn payload(dll: Option<PathBuf>, dev: bool, faults: bool) -> Result<install::Payload, String> {
    let dll = match dll {
        Some(path) => path,
        None => build_dll(faults)?,
    };
    // Keep the DLL and its PDB, so a crash record from this build can be
    // symbolized later.
    if let Some(kept) = symbols::keep(&dll)? {
        println!("kept symbols in {}", kept.display());
    }
    install::Payload::from_repository(&repository(), &dll, dev)
}

/// On a Windows target, refuse while DCS runs there: the machine is also
/// someone's to play on, and a running DCS holds the loaded DLL.
fn refuse_while_playing(name: &str) -> Result<(), String> {
    if let Target::WindowsSsh { ssh, profile, .. } = target(name)? {
        let running = windows::Windows::connect(&ssh, &profile)?.dcs_processes()?;
        if running > 0 {
            return Err(format!(
                "DCS is running on {name}; not changing its profile. Try again when DCS is closed."
            ));
        }
    }
    Ok(())
}

fn deploy(name: &str, dll: Option<PathBuf>, dev: bool, faults: bool) -> Result<ExitCode, String> {
    refuse_while_playing(name)?;
    let payload = payload(dll, dev, faults)?;
    let profile = open(name)?;
    let done = install::deploy(profile.as_ref(), &payload)?;
    let map_path = match target(name)? {
        Target::DockerWine { map_path, .. } | Target::WindowsSsh { map_path, .. } => map_path,
        Target::Local { .. } => None,
    };
    let configured = install::configure(profile.as_ref(), map_path.as_deref())?;
    if configured.token_created {
        println!("created the handshake token");
    }
    if configured.agent_toml_written {
        println!("wrote agent.toml");
    }
    println!(
        "deployed {} to {name}: version folder {}, manifest {}",
        done.build,
        if done.wrote_version {
            "written"
        } else {
            "already in place"
        },
        if done.switched {
            "switched"
        } else {
            "unchanged"
        },
    );
    for build in done.pruned {
        println!("removed old version {build}");
    }
    Ok(ExitCode::SUCCESS)
}

fn rollback(name: &str) -> Result<ExitCode, String> {
    refuse_while_playing(name)?;
    let manifest = install::rollback(open(name)?.as_ref())?;
    println!(
        "rolled back {name}: active {}, previous {}",
        manifest.active.unwrap_or_default(),
        manifest.previous.unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

fn undeploy(name: &str) -> Result<ExitCode, String> {
    refuse_while_playing(name)?;
    if install::undeploy(open(name)?.as_ref())? {
        println!("undeployed from {name}");
    } else {
        println!("nothing was installed on {name}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The target's tree listing, without `Logs/`, which DCS writes while running.
fn snapshot(target: &Target) -> Result<Vec<String>, String> {
    let lines = match target {
        Target::Local { profile } => install::snapshot_local(Path::new(profile))?,
        Target::DockerWine {
            ssh,
            container,
            profile,
            ..
        } => {
            remote::Remote::connect(ssh, container, profile.as_deref().unwrap_or(""))?.snapshot()?
        }
        Target::WindowsSsh { .. } => {
            return Err("snapshot and roundtrip read docker-wine and local targets only".into());
        }
    };
    Ok(lines
        .into_iter()
        .filter(|line| {
            let path = line.rsplit_once(' ').map_or("", |(_, p)| p);
            path != "Logs" && !path.starts_with("Logs/")
        })
        .collect())
}

fn snapshot_command(name: &str) -> Result<ExitCode, String> {
    for line in snapshot(&target(name)?)? {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The exit test for deploy: deploy twice, undeploy, compare trees. `Logs/`
/// is left out, because DCS writes it while running.
fn roundtrip(name: &str, dll: Option<PathBuf>) -> Result<ExitCode, String> {
    let payload = payload(dll, false, false)?;
    let target = target(name)?;
    let profile = open(name)?;
    let snapshot = || snapshot(&target);

    if install::manifest(profile.as_ref())?.active.is_some() {
        return Err(format!(
            "{name} already has an install; undeploy it first so the test starts clean"
        ));
    }
    let before = snapshot()?;
    let first = install::deploy(profile.as_ref(), &payload)?;
    let installed = snapshot()?;
    let second = install::deploy(profile.as_ref(), &payload)?;
    let again = snapshot()?;
    install::undeploy(profile.as_ref())?;
    let after = snapshot()?;

    let mut failures = Vec::new();
    if !(first.wrote_version && first.switched) {
        failures.push("the first deploy did not write and switch".to_owned());
    }
    if second.wrote_version || second.switched || installed != again {
        failures.push("the second deploy changed something".to_owned());
    }
    if installed.len() <= before.len() {
        failures.push("the deploy added nothing to the tree".to_owned());
    }
    for line in before.iter().filter(|l| !after.contains(l)) {
        failures.push(format!("missing after undeploy: {line}"));
    }
    for line in after.iter().filter(|l| !before.contains(l)) {
        failures.push(format!("left after undeploy: {line}"));
    }

    println!("build      {}", first.build);
    println!(
        "entries    {} before, {} installed, {} after",
        before.len(),
        installed.len(),
        after.len()
    );
    if failures.is_empty() {
        println!("result     PASS: deploy is idempotent and undeploy restores the tree");
        Ok(ExitCode::SUCCESS)
    } else {
        for failure in &failures {
            println!("FAIL       {failure}");
        }
        Ok(ExitCode::FAILURE)
    }
}

/// The target as a remote docker-wine profile, with its SSH destination and
/// restart command.
fn remote(name: &str) -> Result<(remote::Remote, String, Option<String>), String> {
    match target(name)? {
        Target::DockerWine {
            ssh,
            container,
            profile: Some(profile),
            restart,
            ..
        } => Ok((
            remote::Remote::connect(&ssh, &container, &profile)?,
            ssh,
            restart,
        )),
        Target::DockerWine { .. } => Err(format!("target {name:?} has no `profile` key")),
        _ => Err("this command needs a docker-wine target".into()),
    }
}

/// How many players the target's DCS has connected.
fn players(name: &str) -> Result<usize, String> {
    match target(name)? {
        Target::DockerWine { .. } => remote(name)?.0.players(),
        Target::WindowsSsh { .. } => live::players(open(name)?.as_ref()),
        Target::Local { .. } => Ok(0),
    }
}

fn restart(name: &str) -> Result<ExitCode, String> {
    match target(name)? {
        Target::WindowsSsh {
            ssh,
            profile,
            start_task,
            stop_task,
            ..
        } => {
            // A Windows client starts at its main menu, so there is no
            // mission to wait for.
            let windows = windows::Windows::connect(&ssh, &profile)?;
            let players = live::players(&windows)?;
            live::restart(&windows, players, false, || {
                windows.restart_tasks(&start_task, &stop_task)
            })?;
        }
        _ => {
            let (remote, ssh, command) = remote(name)?;
            let command = command.ok_or_else(|| format!("target {name:?} has no `restart` key"))?;
            let players = remote.players()?;
            live::restart(&remote, players, true, || {
                let output = ssh::run(&ssh, &command, b"")?;
                match output.code {
                    Some(0) => Ok(()),
                    _ => Err(format!("the restart command failed: {}", output.stderr)),
                }
            })?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn load(name: &str, miz: &str) -> Result<ExitCode, String> {
    let (remote, _, _) = remote(name)?;
    let bytes = std::fs::read(miz).map_err(|e| format!("could not read {miz}: {e}"))?;
    let file = Path::new(miz)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    live::load(&remote, &file, &bytes)?;
    Ok(ExitCode::SUCCESS)
}

/// `run <name> <test>`: the remote runner. Every step must succeed before
/// the next starts; the bundle is collected whatever the test's verdict.
fn run(name: &str, test: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let mut dll = None;
    let mut dir = None;
    let mut template = repository().join("templates/caucasus.miz");
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let value = words
            .next()
            .ok_or_else(|| format!("{word} needs a value"))?;
        match *word {
            "--dll" => dll = Some(PathBuf::from(value)),
            "--template" => template = PathBuf::from(value),
            "--dir" => dir = Some((*value).to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let only = (test != "suite").then(|| test.to_owned());
    // Build the shell first, so a bad template stops the run before DCS is
    // touched.
    let bytes = std::fs::read(&template)
        .map_err(|e| format!("could not read {}: {e}", template.display()))?;
    let shell = miz::generate(&bytes, &miz::Changes::default())?;

    let players = remote(name)?.0.players()?;
    if players > 0 {
        return Err(format!(
            "{players} player(s) connected; not running. Try again when the server is empty."
        ));
    }
    println!("== deploy");
    deploy(name, dll, true, false)?;
    println!("== restart");
    restart(name)?;
    println!("== load a fresh shell mission");
    let (remote, _, _) = remote(name)?;
    live::load(&remote, "np-shell.miz", &shell)?;
    println!("== test {test}");
    let options = probes::ProbeOptions {
        only,
        dir,
        ..Default::default()
    };
    let facts = facts(&target(name)?)?;
    let verdict = probes::run_probes(open(name)?.as_ref(), &facts, name, &options);
    println!("== bundle");
    bundle(name)?;
    verdict
}

/// What the target runs: its kind, and for docker-wine its Wine and DCS versions.
fn facts(target: &Target) -> Result<bundle::Facts, String> {
    Ok(match target {
        Target::Local { .. } => bundle::Facts {
            kind: "local".into(),
            ..Default::default()
        },
        Target::DockerWine { .. } | Target::WindowsSsh { .. } => {
            let report = check::run(target)?;
            bundle::Facts {
                kind: match target {
                    Target::WindowsSsh { .. } => "windows-ssh".into(),
                    _ => "docker-wine".into(),
                },
                wine: report.wine.clone(),
                dcs: report.installs.first().map(|install| {
                    format!(
                        "{} ({})",
                        install.version.as_deref().unwrap_or("unknown"),
                        install.branch.as_deref().unwrap_or("unknown branch")
                    )
                }),
            }
        }
    })
}

fn eval_command(name: &str, state: &str, file: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let timeout = match rest {
        [] => 30,
        ["--timeout", seconds] => seconds
            .parse()
            .map_err(|_| format!("--timeout {seconds:?} is not a number of seconds"))?,
        _ => return Err(format!("unknown options: {}", rest.join(" "))),
    };
    probes::eval_file(
        open(name)?.as_ref(),
        state,
        file,
        std::time::Duration::from_secs(timeout),
    )
}

fn probes_command(name: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let options = probes::ProbeOptions::parse(rest)?;
    let target = target(name)?;
    if !matches!(target, Target::Local { .. }) && !options.with_players {
        let players = players(name)?;
        if players > 0 {
            return Err(format!(
                "{players} player(s) connected; not running probes. Use --with-players to run anyway."
            ));
        }
    }
    let facts = facts(&target)?;
    probes::run_probes(open(name)?.as_ref(), &facts, name, &options)
}

fn api_dump(name: &str) -> Result<ExitCode, String> {
    let facts = facts(&target(name)?)?;
    probes::api_dump(open(name)?.as_ref(), &facts)
}

fn bundle(name: &str) -> Result<ExitCode, String> {
    let target = target(name)?;
    let facts = facts(&target)?;
    // Every target's join password is redacted from every bundle: a client's
    // logs may hold the server's.
    let bytes = bundle::build(
        open(name)?.as_ref(),
        &facts,
        &bundle::key()?,
        client::passwords(),
    )?;
    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("{name}-{}.zip", bundle::stamp()));
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
    Ok(ExitCode::SUCCESS)
}

/// The file both sides of E24 map: `/dev/shm` in the container, which Wine
/// reaches as `Z:\dev\shm`. `lua/experiments/e24-mapped.lua` names it too.
const MAPPED_PATH: &str = "/dev/shm/nanoprobe-e24.map";
/// How long the peer reads, from when it sees the agent's magic.
const MAPPED_SECONDS: &str = "20";

/// `mapped <name>` on a windows-ssh target: the file is in the profile,
/// which both the DCS user and the SSH account can reach.
fn mapped_windows(name: &str, ssh: &str, profile_path: &str) -> Result<ExitCode, String> {
    let windows = windows::Windows::connect(ssh, profile_path)?;
    let repo = repository();
    let program = std::fs::read_to_string(repo.join("tools/mapped-peer.ps1"))
        .map_err(|e| format!("tools/mapped-peer.ps1: {e}"))?;
    let snippet = std::fs::read(repo.join("lua/experiments/e24-mapped.lua"))
        .map_err(|e| format!("lua/experiments/e24-mapped.lua: {e}"))?;
    let file = format!(
        "{}\\nanoprobe-e24.map",
        windows.profile().trim_end_matches('\\')
    );
    let peer_side = windows.clone();
    let peer = std::thread::spawn(move || peer_side.powershell(&program, &[&file, MAPPED_SECONDS]));
    let result = eval::run(
        &windows,
        "hook",
        "e24-mapped",
        &snippet,
        std::time::Duration::from_secs(120),
    );
    let output = peer.join().map_err(|_| "the peer thread panicked")??;
    if let Err(e) = windows.remove_file("nanoprobe-e24.map") {
        println!("could not remove the mapped file: {e}");
    }
    mapped_report(name, &output.stdout, &output.stderr, result)
}

/// `mapped <name>`: experiment E24.
fn mapped(name: &str) -> Result<ExitCode, String> {
    if let Target::WindowsSsh { ssh, profile, .. } = target(name)? {
        return mapped_windows(name, &ssh, &profile);
    }
    let (remote, _, _) = remote(name)?;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }
    let repo = repository();
    let program = std::fs::read_to_string(repo.join("tools/mapped-peer.py"))
        .map_err(|e| format!("tools/mapped-peer.py: {e}"))?;
    let snippet = std::fs::read(repo.join("lua/experiments/e24-mapped.lua"))
        .map_err(|e| format!("lua/experiments/e24-mapped.lua: {e}"))?;
    // The peer waits for the agent's magic, so it starts first.
    let peer = std::thread::spawn(move || {
        let output = remote.python(&program, &[MAPPED_PATH, MAPPED_SECONDS]);
        let cleanup = remote.python(
            "import os, sys\ntry:\n    os.remove(sys.argv[1])\nexcept FileNotFoundError:\n    pass\n",
            &[MAPPED_PATH],
        );
        (output, cleanup)
    });
    let profile = open(name)?;
    let result = eval::run(
        profile.as_ref(),
        "hook",
        "e24-mapped",
        &snippet,
        std::time::Duration::from_secs(60),
    );
    let (output, cleanup) = peer.join().map_err(|_| "the peer thread panicked")?;
    let output = output?;
    if let Err(e) = cleanup {
        println!("could not remove {MAPPED_PATH}: {e}");
    }
    mapped_report(name, &output.stdout, &output.stderr, result)
}

/// Print and save the E24 report from the peer's output and the agent's
/// result.
fn mapped_report(
    name: &str,
    stdout: &[u8],
    stderr: &str,
    result: Result<serde_json::Value, String>,
) -> Result<ExitCode, String> {
    let peer_report: serde_json::Value = serde_json::from_slice(stdout).unwrap_or_else(|_| {
        serde_json::json!({
            "error": "the peer printed no JSON",
            "stdout": String::from_utf8_lossy(stdout),
            "stderr": stderr,
        })
    });
    let facts = facts(&target(name)?)?;
    let report = serde_json::json!({
        "target": name,
        "kind": facts.kind,
        "dcs": facts.dcs,
        "wine": facts.wine,
        "time": bundle::utc_now(),
        "peer": peer_report,
        "agent": match &result { Ok(r) => r.clone(), Err(e) => serde_json::json!({ "error": e }) },
    });
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    println!("{text}");
    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("mapped-{name}-{}.json", bundle::stamp()));
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(if result.is_ok() && peer_report.get("error").is_none() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The autoexec.cfg keys of experiment E2: hooks may call
/// `net.dostring_in`; the mission scripting state may not; the states the
/// agent and the eval channel reach are allowed as targets.
const GATES: &str = "\
net.allow_unsafe_api = { 'userhooks' }
net.allow_dostring_in = { 'server', 'mission', 'gui', 'export', 'config' }
";

/// `gates <name> on|off`.
fn gates(name: &str, on: bool) -> Result<ExitCode, String> {
    let text = if let Target::WindowsSsh { ssh, profile, .. } = target(name)? {
        refuse_while_playing(name)?;
        windows::Windows::connect(&ssh, &profile)?.autoexec_block(on.then_some(GATES))?
    } else {
        remote(name)?.0.autoexec_block(on.then_some(GATES))?
    };
    println!("{name}: {text}");
    Ok(ExitCode::SUCCESS)
}

fn miz(template: &str, out: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let changes = match rest {
        [] => miz::Changes::default(),
        ["--start-time", seconds] => miz::Changes {
            start_time: Some(
                seconds
                    .parse()
                    .map_err(|_| format!("--start-time {seconds:?} is not a number of seconds"))?,
            ),
        },
        _ => return Err(format!("unknown options: {}", rest.join(" "))),
    };
    let bytes = std::fs::read(template).map_err(|e| format!("could not read {template}: {e}"))?;
    let shell = miz::generate(&bytes, &changes)?;
    std::fs::write(out, &shell).map_err(|e| format!("could not write {out}: {e}"))?;
    println!("wrote {out} ({} bytes)", shell.len());
    Ok(ExitCode::SUCCESS)
}

fn target_list() -> Result<ExitCode, String> {
    let (path, targets) = profile::load()?;
    if targets.is_empty() {
        println!("no targets in {}", path.display());
    }
    for (name, target) in &targets {
        println!("{name:<16} {}", target.describe());
    }
    Ok(ExitCode::SUCCESS)
}

fn target_check(name: &str) -> Result<ExitCode, String> {
    let target = target(name)?;
    let report = check::run(&target)?;
    println!("target    {name} ({})", target.describe());
    print!("{report}");

    Ok(if report.is_complete() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
