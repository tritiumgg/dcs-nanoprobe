//! Development and test tooling for nanoprobe, run as `cargo xtask <command>`.
//!
//! It is built from source and never released. Every target it acts on is a
//! profile in a file outside the repository (see [`profile`]).

mod bundle;
mod check;
mod eval;
mod install;
mod live;
mod miz;
mod probes;
mod profile;
mod remote;
mod ssh;

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
  deploy <name> [--dll P] [--dev]
                            Build the agent DLL (or take P), and install it with the
                            hook into the target's profile as the active version.
                            --dev adds the development mission loader.
  rollback <name>           Make the previous version active again
  undeploy <name>           Remove everything deploy installed
  roundtrip <name> [--dll P]
                            Deploy twice, undeploy, and check that the profile's
                            tree, outside Logs/, is as it was before
  bundle <name>             Collect the session's logs from the target, redact them,
                            and write a zip under bundles/
  restart <name>            Restart DCS with the target's `restart` command, only
                            when no player is connected, and wait for the hook
  load <name> <miz>         Load a mission through the development mission loader,
                            and wait until it starts
  eval <name> <state> <file> [--timeout S]
                            Run a Lua file in a state of the running DCS through the
                            eval channel, and print the result as JSON
  reset <name>              Run every eval cleanup in every state that holds the prelude
  probes <name> [--only TEXT] [--keep] [--with-players]
                            Run lua/probes/*.lua in their states, print each verdict,
                            clean up, and write bundles/probes-<name>-<time>.json.
                            Refuses a server with players unless --with-players.
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
            deploy_options(rest).and_then(|(dll, dev)| deploy(name, dll, dev))
        }
        ["restart", name] => restart(name),
        ["load", name, miz] => load(name, miz),
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

fn deploy_options(rest: &[&str]) -> Result<(Option<PathBuf>, bool), String> {
    let dev = rest.contains(&"--dev");
    let rest: Vec<&str> = rest.iter().copied().filter(|a| *a != "--dev").collect();
    Ok((dll_option(&rest)?, dev))
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
    }
}

/// Cross-build the agent DLL, or on Windows build it natively.
fn build_dll() -> Result<PathBuf, String> {
    let repo = repository();
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
        .current_dir(&repo)
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "the DLL build failed ({status}). Run it through mise: `mise exec -- cargo xtask ...`"
        ));
    }
    Ok(repo.join("target/x86_64-pc-windows-msvc/release/nanoprobe.dll"))
}

fn payload(dll: Option<PathBuf>, dev: bool) -> Result<install::Payload, String> {
    let dll = match dll {
        Some(path) => path,
        None => build_dll()?,
    };
    install::Payload::from_repository(&repository(), &dll, dev)
}

fn deploy(name: &str, dll: Option<PathBuf>, dev: bool) -> Result<ExitCode, String> {
    let payload = payload(dll, dev)?;
    let profile = open(name)?;
    let done = install::deploy(profile.as_ref(), &payload)?;
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
    let manifest = install::rollback(open(name)?.as_ref())?;
    println!(
        "rolled back {name}: active {}, previous {}",
        manifest.active.unwrap_or_default(),
        manifest.previous.unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

fn undeploy(name: &str) -> Result<ExitCode, String> {
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
    let payload = payload(dll, false)?;
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
        Target::Local { .. } => Err("this command needs a docker-wine target".into()),
    }
}

fn restart(name: &str) -> Result<ExitCode, String> {
    let (remote, ssh, command) = remote(name)?;
    let command = command.ok_or_else(|| format!("target {name:?} has no `restart` key"))?;
    live::restart(&ssh, &command, &remote)?;
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

/// What the target runs: its kind, and for docker-wine its Wine and DCS versions.
fn facts(target: &Target) -> Result<bundle::Facts, String> {
    Ok(match target {
        Target::Local { .. } => bundle::Facts {
            kind: "local".into(),
            ..Default::default()
        },
        Target::DockerWine { .. } => {
            let report = check::run(target)?;
            bundle::Facts {
                kind: "docker-wine".into(),
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
    if matches!(target, Target::DockerWine { .. }) && !options.with_players {
        let players = remote(name)?.0.players()?;
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
    let bytes = bundle::build(open(name)?.as_ref(), &facts, &bundle::key()?)?;
    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("{name}-{}.zip", bundle::stamp()));
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
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
