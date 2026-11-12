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

const DCS_LOG: &str = "Logs/dcs.log";
const HOOK_LOG: &str = "nanoprobe/logs/hook.log";
const POLL: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The session ID on the last marker line of `dcs.log`, if any.
fn session(profile: &dyn Profile) -> Option<String> {
    let text = profile.read_from_last(DCS_LOG, MARKER).ok()??;
    let text = String::from_utf8_lossy(&text);
    let line = text.lines().next()?;
    Regex::new(r"session ([0-9a-f]+)")
        .unwrap()
        .captures(line)
        .map(|c| c[1].to_owned())
}

/// Whether `hook.log` shows the simulation started after `after` records.
fn simulation_started(profile: &dyn Profile, after: usize) -> Option<Vec<String>> {
    let text = profile.read(HOOK_LOG).ok()??;
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
fn hook_error(profile: &dyn Profile) -> Option<String> {
    let text = profile.read_from_last(DCS_LOG, MARKER).ok()??;
    String::from_utf8_lossy(&text)
        .lines()
        .find(|line| line.contains("ERROR") && line.contains("NANOPROBE"))
        .map(|line| format!("the hook failed: {}", line.get(24..).unwrap_or(line).trim()))
}

fn hook_lines(profile: &dyn Profile) -> usize {
    profile
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

/// How many players the session's part of `dcs.log` shows connected. A
/// player is a client with a name: `added client[N] name=...` (LOG), and
/// leaves with `disconnecting client[N]` or `remove: client[N]` (LOG). Unnamed
/// connection attempts are not players. Prints no name.
pub fn players(profile: &dyn Profile) -> Result<usize, String> {
    let Some(text) = profile.read_from_last(DCS_LOG, MARKER)? else {
        return Ok(0);
    };
    let client = Regex::new(r"client\[(\d+)\]").unwrap();
    let mut on = std::collections::BTreeSet::new();
    for line in String::from_utf8_lossy(&text).lines() {
        let Some(id) = client.captures(line).map(|c| c[1].to_owned()) else {
            continue;
        };
        if line.contains("added client[") && line.contains(" name=") {
            on.insert(id);
        } else if line.contains("disconnecting client[") || line.contains("remove: client[") {
            on.remove(&id);
        }
    }
    Ok(on.len())
}

/// Restart DCS with `restart`, when `players` is zero. When the hook is
/// installed, wait until it has run, and with `mission`, until a mission has
/// started.
pub fn restart(
    profile: &dyn Profile,
    players: usize,
    mission: bool,
    restart: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if players > 0 {
        return Err(format!(
            "{players} player(s) connected; not restarting. Try again when the server is empty."
        ));
    }
    let hook_active = profile.read("nanoprobe/manifest.txt")?.is_some()
        && profile.read("nanoprobe/disable")?.is_none();
    let before = session(profile);

    println!("no player connected; restarting DCS");
    restart()?;
    if !hook_active {
        println!("restarted; the hook is not installed, so there is nothing to wait for");
        return Ok(());
    }

    let (id, t) = wait("the hook's session marker", || {
        session(profile).filter(|id| Some(id) != before.as_ref())
    })?;
    println!("hook ran: session {id}, {}s after the restart", t.as_secs());
    if !mission {
        return Ok(());
    }
    // hook.log may hold records from an earlier process; count from the
    // record this session's hook wrote.
    let (lines, t) = wait("the mission to start", || {
        if let Some(error) = hook_error(profile) {
            return Some(Err(error));
        }
        let text = profile.read(HOOK_LOG).ok()??;
        let text = String::from_utf8_lossy(&text);
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.iter().rposition(|line| line.contains(&id))?;
        simulation_started(profile, start).map(Ok)
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
pub fn load(profile: &dyn Profile, name: &str, miz: &[u8]) -> Result<(), String> {
    if profile.read(DEV_LOADER)?.is_none() {
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
    profile.write(&format!("{DEV}/missions/{name}"), miz)?;
    let after = hook_lines(profile);
    let session_before = session(profile);
    profile.write(&format!("{DEV}/load.txt"), name.as_bytes())?;
    println!("requested {name}");

    let (lines, t) = wait("the shell mission to start", || {
        if session(profile) != session_before {
            // DCS restarted: the request crashed it or the watchdog acted.
            return Some(Err("DCS restarted during the load".to_owned()));
        }
        simulation_started(profile, after).map(Ok)
    })?;
    let lines = lines?;
    println!(
        "simulation started {}s after the request; new hook.log records:",
        t.as_secs()
    );
    for line in lines {
        println!("  {line}");
    }
    if let Some(text) = profile.read_from_last(DCS_LOG, MARKER)? {
        for line in String::from_utf8_lossy(&text).lines() {
            if line.contains("NANOPROBE-DEV") {
                println!("  dcs.log: {}", line.get(24..).unwrap_or(line).trim());
            }
        }
    }
    Ok(())
}
