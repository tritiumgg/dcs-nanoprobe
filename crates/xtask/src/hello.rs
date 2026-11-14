//! `cargo xtask hello <target>`: run the Rust hello campaign against the API
//! server over the fake sim on a target's own system.
//!
//! It builds `nanoprobe-fake-sidecar` and `hello-campaign` for the target,
//! copies them to a new temporary folder there, runs the server on a
//! loopback port with the hello campaign as its child, and deletes the
//! folder. Nothing touches DCS or its profile: on a `docker-wine` target the
//! programs run on the SSH host, outside the game container; on a
//! `windows-ssh` target they run as the SSH account in its temp folder.
//! `local` runs them here.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use crate::link::build_for;
use crate::profile::Target;
use crate::ssh;
use crate::windows::Windows;

/// The two programs, by package and binary.
const BINS: &[(&str, &str)] = &[
    ("nanoprobe-fake-sidecar", "nanoprobe-fake-sidecar"),
    ("nanoprobe-hello", "hello-campaign"),
];

fn built(target: &str, xwin: bool, dir: &str, ext: &str) -> Result<(PathBuf, PathBuf), String> {
    let mut v = build_for(target, xwin, dir, ext, BINS)?;
    let hello = v.pop().ok_or("no hello-campaign built")?;
    let server = v.pop().ok_or("no server built")?;
    Ok((server, hello))
}

fn read(p: &PathBuf) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn report(code: Option<i32>, stdout: &[u8], stderr: &str) -> Result<ExitCode, String> {
    print!("{}", String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }
    match code {
        Some(0) => {
            println!("ok  the hello campaign ran on the fake sim");
            Ok(ExitCode::SUCCESS)
        }
        c => Err(format!("the hello campaign failed (exit {c:?})")),
    }
}

/// Run on a Linux host over SSH.
fn linux(ssh_dest: &str) -> Result<ExitCode, String> {
    let (server, hello) = built("x86_64-unknown-linux-musl", false, "target/linux", "")?;
    let dir = ssh::script(
        ssh_dest,
        "sh -s",
        "set -e\nmktemp -d /tmp/nanoprobe-hello.XXXXXX\n",
    )?
    .trim()
    .to_owned();
    if !dir.starts_with("/tmp/nanoprobe-hello.") {
        return Err(format!("unexpected temporary folder `{dir}`"));
    }
    let q = ssh::quote(&dir);
    let result = (|| {
        for (local, name) in [
            (&server, "nanoprobe-fake-sidecar"),
            (&hello, "hello-campaign"),
        ] {
            let out = ssh::run(
                ssh_dest,
                &format!("cat > {q}/{name} && chmod 755 {q}/{name}"),
                &read(local)?,
            )?;
            if out.code != Some(0) {
                return Err(format!("upload of {name} failed: {}", out.stderr));
            }
        }
        let out = ssh::run(
            ssh_dest,
            &format!("{q}/nanoprobe-fake-sidecar --listen 127.0.0.1:0 --run {q}/hello-campaign"),
            b"",
        )?;
        report(out.code, &out.stdout, &out.stderr)
    })();
    // Nothing of the run may be left: no process started from the folder,
    // and no folder.
    // The bracket keeps the pattern from matching the shell that runs it.
    let pattern = ssh::quote(&format!("[{}]{}/", &dir[..1], &dir[1..]));
    // pgrep exits 1 when nothing matches; anything else, or no pgrep, means
    // the check did not run.
    let left = ssh::run(
        ssh_dest,
        &format!(
            "command -v pgrep >/dev/null || exit 3; pgrep -fl -- {pattern}; s=$?; [ $s -le 1 ]"
        ),
        b"",
    )?;

    let cleanup = ssh::run(ssh_dest, &format!("rm -rf -- {q} && test ! -e {q}"), b"")?;
    if cleanup.code != Some(0) {
        return Err(format!("could not remove {dir}: {}", cleanup.stderr));
    }
    if left.code != Some(0) {
        return Err(format!(
            "removed {dir}, but could not check for processes left by the run (exit {:?}): {}",
            left.code, left.stderr
        ));
    }
    let left = String::from_utf8_lossy(&left.stdout).trim().to_owned();
    if !left.is_empty() {
        return Err(format!("processes of the run are still running: {left}"));
    }
    println!("removed {dir}; no process of the run is left");
    result
}

/// Run on a Windows machine over SSH.
fn windows(ssh_dest: &str, profile: &str) -> Result<ExitCode, String> {
    let (server, hello) = built(
        "x86_64-pc-windows-msvc",
        true,
        "target/windows-tools",
        ".exe",
    )?;
    let w = Windows::connect(ssh_dest, profile)?;
    let made = w.powershell(
        "$d = Join-Path ([IO.Path]::GetTempPath()) ('nanoprobe-hello-' + [Guid]::NewGuid().ToString('N'))\n\
         [void][IO.Directory]::CreateDirectory($d)\n\
         [Console]::Out.Write($d)",
        &[],
    )?;
    let dir = String::from_utf8_lossy(&made.stdout).trim().to_owned();
    if made.code != Some(0) || !dir.contains("nanoprobe-hello-") {
        return Err(format!(
            "could not make a temporary folder: {}",
            made.stderr
        ));
    }
    let result = (|| {
        w.send(
            &format!("{dir}\\nanoprobe-fake-sidecar.exe"),
            &read(&server)?,
        )?;
        w.send(&format!("{dir}\\hello-campaign.exe"), &read(&hello)?)?;
        let out = w.powershell(
            "& (Join-Path $a0 'nanoprobe-fake-sidecar.exe') --listen 127.0.0.1:0 --run (Join-Path $a0 'hello-campaign.exe')\n\
             exit $LASTEXITCODE",
            &[&dir],
        )?;
        report(out.code, &out.stdout, &out.stderr)
    })();
    // Nothing of the run may be left: no process started from the folder,
    // and no folder.
    let left = w.powershell(
        "$left = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($a0 + '\\') })\n\
         [Console]::Out.Write(($left | ForEach-Object { $_.Id.ToString() + ' ' + $_.Path }) -join '; ')",
        &[&dir],
    )?;
    let gone = w.powershell(
        "Remove-Item -LiteralPath $a0 -Recurse -Force -ErrorAction SilentlyContinue\n\
         if (Test-Path -LiteralPath $a0) { exit 1 }",
        &[&dir],
    )?;
    if gone.code != Some(0) {
        return Err(format!("could not remove {dir}: {}", gone.stderr));
    }
    let left = String::from_utf8_lossy(&left.stdout).trim().to_owned();
    if !left.is_empty() {
        return Err(format!("processes of the run are still running: {left}"));
    }
    println!("removed {dir}; no process of the run is left");
    result
}

/// Run here.
fn local() -> Result<ExitCode, String> {
    let repo = crate::repository();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(&repo)
        .args([
            "build",
            "-p",
            "nanoprobe-fake-sidecar",
            "-p",
            "nanoprobe-hello",
        ])
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err("the build failed".into());
    }
    let ext = std::env::consts::EXE_SUFFIX;
    let bin = repo.join("target").join("debug");
    let out = Command::new(bin.join(format!("nanoprobe-fake-sidecar{ext}")))
        .args(["--listen", "127.0.0.1:0", "--run"])
        .arg(bin.join(format!("hello-campaign{ext}")))
        .output()
        .map_err(|e| format!("could not run the server: {e}"))?;
    report(
        out.status.code(),
        &out.stdout,
        String::from_utf8_lossy(&out.stderr).trim(),
    )
}

/// `cargo xtask hello [<target>]`.
pub fn command(name: Option<&str>) -> Result<ExitCode, String> {
    let Some(name) = name else {
        return local();
    };
    match crate::target(name)? {
        Target::DockerWine { ssh, .. } => linux(&ssh),
        Target::WindowsSsh { ssh, profile, .. } => windows(&ssh, &profile),
        Target::Local { .. } => local(),
    }
}
