//! Development and test tooling for nanoprobe, run as `cargo xtask <command>`.
//!
//! It is built from source and never released. Every target it acts on is a
//! profile in a file outside the repository (see [`profile`]).

mod check;
mod profile;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  target list           List the target profiles
  target check <name>   Read a target's DCS version, Wine version, install and
                        Saved Games profile. Changes nothing on the target.

Profiles are read from the file NANOPROBE_TARGETS names, or by default from
~/.config/nanoprobe/targets.toml (%APPDATA%\\nanoprobe\\targets.toml on Windows).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["target", "list"] => target_list(),
        ["target", "check", name] => target_check(name),
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
    let (path, targets) = profile::load()?;
    let target = targets
        .get(name)
        .ok_or_else(|| format!("no target {name:?} in {}", path.display()))?;

    let report = check::run(target)?;
    println!("target    {name} ({})", target.describe());
    print!("{report}");

    Ok(if report.is_complete() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
