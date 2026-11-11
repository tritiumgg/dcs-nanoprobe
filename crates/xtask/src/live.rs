//! Tests against a running DCS: restart it, and load a mission through the
//! development mission loader.
//!
//! A restart happens only when no player is connected. Waiting has no fixed
//! load time to rely on, so each wait polls for the record that proves the
//! step happened, prints progress, and gives up only after a long timeout.

use std::thread::sleep;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::bundle::MARKER;
use crate::install::{DEV, DEV_LOADER, Profile};
use crate::remote::Remote;
use crate::ssh;

const DCS_LOG: &str = "Logs/dcs.log";
const HOOK_LOG: &str = "nanoprobe/logs/hook.log";
const POLL: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The session ID on the last marker line of `dcs.log`, if any.
fn session(remote: &Remote) -> Option<String> {
    let text = remote.read_from_last(DCS_LOG, MARKER).ok()??;
    let text = String::from_utf8_lossy(&text);
    let line = text.lines().next()?;
    Regex::new(r"session ([0-9a-f]+)")
        .unwrap()
        .captures(line)
        .map(|c| c[1].to_owned())
}

/// Whether `hook.log` shows the simulation started after `after` records.
fn simulation_started(remote: &Remote, after: usize) -> Option<Vec<String>> {
    let text = remote.read(HOOK_LOG).ok()??;
    let lines: Vec<String> = String::from_utf8_lossy(&text)
        .lines()
        .skip(after)
        .map(str::to_owned)
        .collect();
    lines
        .iter()
        .any(|line| line.contains("\"msg\":\"simulation start\""))
        .then_some(lines)
}

/// The first error the hook wrote to `dcs.log` in this session, if any.
fn hook_error(remote: &Remote) -> Option<String> {
    let text = remote.read_from_last(DCS_LOG, MARKER).ok()??;
    String::from_utf8_lossy(&text)
        .lines()
        .find(|line| line.contains("ERROR") && line.contains("NANOPROBE"))
        .map(|line| format!("the hook failed: {}", line.get(24..).unwrap_or(line).trim()))
}

fn hook_lines(remote: &Remote) -> usize {
    remote
        .read(HOOK_LOG)
        .ok()
        .flatten()
        .map_or(0, |text| String::from_utf8_lossy(&text).lines().count())
}

/// Poll `check` until it gives a value or the timeout passes.
fn wait<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> Result<(T, Duration), String> {
    let start = Instant::now();
    let mut reported = 0;
    loop {
        if let Some(value) = check() {
            return Ok((value, start.elapsed()));
        }
        let elapsed = start.elapsed();
        if elapsed > TIMEOUT {
            return Err(format!(
                "gave up waiting for {what} after {}s",
                elapsed.as_secs()
            ));
        }
        if elapsed.as_secs() / 60 > reported {
            reported = elapsed.as_secs() / 60;
            println!("  still waiting for {what} ({reported} min)");
        }
        sleep(POLL);
    }
}

/// Restart DCS with the target's command, when no player is connected. When
/// the hook is installed, wait until it has run and a mission has started.
pub fn restart(ssh_dest: &str, command: &str, remote: &Remote) -> Result<(), String> {
    let players = remote.players()?;
    if players > 0 {
        return Err(format!(
            "{players} player(s) connected; not restarting. Try again when the server is empty."
        ));
    }
    let hook_active = remote.read("nanoprobe/manifest.txt")?.is_some()
        && remote.read("nanoprobe/disable")?.is_none();
    let before = session(remote);

    println!("no player connected; restarting DCS");
    let output = ssh::run(ssh_dest, command, b"")?;
    if output.code != Some(0) {
        return Err(format!("the restart command failed: {}", output.stderr));
    }
    if !hook_active {
        println!("restarted; the hook is not installed, so there is nothing to wait for");
        return Ok(());
    }

    let (id, t) = wait("the hook's session marker", || {
        session(remote).filter(|id| Some(id) != before.as_ref())
    })?;
    println!("hook ran: session {id}, {}s after the restart", t.as_secs());
    // hook.log may hold records from an earlier process; count from the
    // record this session's hook wrote.
    let (lines, t) = wait("the mission to start", || {
        if let Some(error) = hook_error(remote) {
            return Some(Err(error));
        }
        let text = remote.read(HOOK_LOG).ok()??;
        let text = String::from_utf8_lossy(&text);
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.iter().rposition(|line| line.contains(&id))?;
        simulation_started(remote, start).map(Ok)
    })?;
    let lines = lines?;
    println!("mission started {}s later; hook.log:", t.as_secs());
    for line in lines {
        println!("  {line}");
    }
    Ok(())
}

/// Upload `miz` and ask the development loader to load it. Wait until the
/// hook logs that the simulation started again.
pub fn load(remote: &Remote, name: &str, miz: &[u8]) -> Result<(), String> {
    if remote.read(DEV_LOADER)?.is_none() {
        return Err(
            "the development loader is not installed: run `deploy <name> --dev`, then `restart`"
                .into(),
        );
    }
    if !name.ends_with(".miz")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(format!("{name:?} must be a plain .miz file name"));
    }
    remote.write(&format!("{DEV}/missions/{name}"), miz)?;
    let after = hook_lines(remote);
    let session_before = session(remote);
    remote.write(&format!("{DEV}/load.txt"), name.as_bytes())?;
    println!("requested {name}");

    let (lines, t) = wait("the shell mission to start", || {
        if session(remote) != session_before {
            // DCS restarted: the request crashed it or the watchdog acted.
            return Some(Err("DCS restarted during the load".to_owned()));
        }
        simulation_started(remote, after).map(Ok)
    })?;
    let lines = lines?;
    println!(
        "simulation started {}s after the request; new hook.log records:",
        t.as_secs()
    );
    for line in lines {
        println!("  {line}");
    }
    if let Some(text) = remote.read_from_last(DCS_LOG, MARKER)? {
        for line in String::from_utf8_lossy(&text).lines() {
            if line.contains("NANOPROBE-DEV") {
                println!("  dcs.log: {}", line.get(24..).unwrap_or(line).trim());
            }
        }
    }
    Ok(())
}
