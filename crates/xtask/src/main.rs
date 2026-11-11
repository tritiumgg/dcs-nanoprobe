//! Development and test tooling for nanoprobe, run as `cargo xtask <command>`.
//!
//! It is built from source and never released. Every target it acts on is a
//! profile in a file outside the repository (see [`profile`]).

mod bundle;
mod check;
mod install;
mod miz;
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
  deploy <name> [--dll P]   Build the agent DLL (or take P), and install it with the
                            hook into the target's profile as the active version
  rollback <name>           Make the previous version active again
  undeploy <name>           Remove everything deploy installed
  roundtrip <name> [--dll P]
                            Deploy twice, undeploy, and check that the profile's
                            tree, outside Logs/, is as it was before
  bundle <name>             Collect the session's logs from the target, redact them,
                            and write a zip under bundles/
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
        ["deploy", name, rest @ ..] => dll_option(rest).and_then(|dll| deploy(name, dll)),
        ["rollback", name] => rollback(name),
        ["undeploy", name] => undeploy(name),
        ["roundtrip", name, rest @ ..] => dll_option(rest).and_then(|dll| roundtrip(name, dll)),
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

fn payload(dll: Option<PathBuf>) -> Result<install::Payload, String> {
    let dll = match dll {
        Some(path) => path,
        None => build_dll()?,
    };
    install::Payload::from_repository(&repository(), &dll)
}

fn deploy(name: &str, dll: Option<PathBuf>) -> Result<ExitCode, String> {
    let payload = payload(dll)?;
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

/// The exit test for deploy: deploy twice, undeploy, compare trees. `Logs/`
/// is left out, because DCS writes it while running.
fn roundtrip(name: &str, dll: Option<PathBuf>) -> Result<ExitCode, String> {
    let payload = payload(dll)?;
    let target = target(name)?;
    let profile = open(name)?;
    let snapshot = || -> Result<Vec<String>, String> {
        let lines = match &target {
            Target::Local { profile } => install::snapshot_local(Path::new(profile))?,
            Target::DockerWine {
                ssh,
                container,
                profile,
                ..
            } => remote::Remote::connect(ssh, container, profile.as_deref().unwrap_or(""))?
                .snapshot()?,
        };
        Ok(lines
            .into_iter()
            .filter(|line| {
                let path = line.rsplit_once(' ').map_or("", |(_, p)| p);
                path != "Logs" && !path.starts_with("Logs/")
            })
            .collect())
    };

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

fn bundle(name: &str) -> Result<ExitCode, String> {
    let target = target(name)?;
    let facts = match &target {
        Target::Local { .. } => bundle::Facts {
            kind: "local".into(),
            ..Default::default()
        },
        Target::DockerWine { .. } => {
            let report = check::run(&target)?;
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
    };
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
