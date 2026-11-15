//! `nanoprobe doctor`: what an install looks like from outside DCS.
//!
//! Each check adds one line, marked `ok`, `fail` or `note`, and every check
//! runs whatever an earlier one found, since a broken install often has more
//! than one cause. `doctor` reads the profile and writes nothing. Of
//! `autoexec.cfg` it reports only the two `net.*` keys, and of `dcs.log` only
//! the platform's own lines.

use std::fmt;
use std::path::Path;

use nanoprobe_agent_link::config::AgentToml;
use nanoprobe_install::autoexec::{self, AUTOEXEC};
use nanoprobe_install::{
    AGENT_TOML, DISABLE, LOADER, LOGS, Local, Profile, RUNNING, TOKEN, VERSION_LUA, VERSIONS,
    manifest, sha256,
};

use crate::dcslog::{self, Line};
use crate::profiles::{self, Env, Found, Where};

/// The loader this release installs. It is the same file in every version.
pub const RELEASE_LOADER: &[u8] = include_bytes!("../../../lua/hook/nanoprobe-hook.lua");

/// DCS's own log, relative to the profile.
pub const DCS_LOG: &str = "Logs/dcs.log";

/// How a check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// The check passed.
    Ok,
    /// Something is broken here.
    Fail,
    /// Worth knowing, and not broken.
    Note,
}

/// One line per check, in the order the checks ran.
#[derive(Debug, Default)]
pub struct Report {
    /// The lines.
    pub lines: Vec<(Mark, String)>,
}

impl Report {
    fn ok(&mut self, text: impl Into<String>) {
        self.lines.push((Mark::Ok, text.into()));
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.lines.push((Mark::Fail, text.into()));
    }

    fn note(&mut self, text: impl Into<String>) {
        self.lines.push((Mark::Note, text.into()));
    }

    /// Whether any check failed.
    pub fn failed(&self) -> bool {
        self.lines.iter().any(|(mark, _)| *mark == Mark::Fail)
    }

    /// Whether a line with `mark` contains `text`.
    pub fn has(&self, mark: Mark, text: &str) -> bool {
        self.lines
            .iter()
            .any(|(m, t)| *m == mark && t.contains(text))
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (mark, text) in &self.lines {
            let mark = match mark {
                Mark::Ok => "ok",
                Mark::Fail => "fail",
                Mark::Note => "note",
            };
            writeln!(f, "{mark:<4} {text}")?;
        }
        Ok(())
    }
}

/// The profiles to check: with `--profile`, that one; otherwise every
/// profile found that holds the loader. An error is nothing to check.
pub fn targets(place: &Where, env: &Env) -> Result<Vec<Found>, String> {
    let found = profiles::find(place, env)?;
    if place.profile.is_some() {
        return Ok(found);
    }
    let installed: Vec<Found> = found
        .iter()
        .filter(|f| f.path.join(LOADER).is_file())
        .cloned()
        .collect();
    if !installed.is_empty() {
        return Ok(installed);
    }
    if found.is_empty() {
        Err(format!(
            "no DCS profile found in {}; name one with --profile or --wine-prefix",
            profiles::searched(place, env)
        ))
    } else {
        let list: Vec<String> = found
            .iter()
            .map(|f| format!("  {}", f.path.display()))
            .collect();
        Err(format!(
            "nanoprobe is not installed in any profile found:\n{}\nrun `nanoprobe install`, or name a profile with --profile",
            list.join("\n")
        ))
    }
}

/// Check one profile.
pub fn check(found: &Found) -> Report {
    let mut report = Report::default();
    report.note(format!("profile {}", found.path.display()));
    if let Some(prefix) = &found.wine_prefix {
        report.note(format!("in the Wine prefix {}", prefix.display()));
    }
    let profile = match Local::new(&found.path) {
        Ok(profile) => profile,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    let active = files(&profile, &mut report);
    settings(&profile, &mut report);
    owner(&found.path, &mut report);
    let start = dcs_log(&profile, &found.path, active.as_deref(), &mut report);
    platform_logs(&profile, &start, &mut report);
    gates(&profile, &mut report);
    report
}

/// A read that turns an error into a failed line: Err when the file is
/// there and does not read, which the caller then says nothing more about.
fn read(profile: &dyn Profile, path: &str, report: &mut Report) -> Result<Option<Vec<u8>>, ()> {
    profile.read(path).map_err(|e| {
        report.fail(format!("cannot read {path}: {e}"));
    })
}

/// The loader, the kill switch, the manifest and the active version's
/// files. Returns the active build.
fn files(profile: &dyn Profile, report: &mut Report) -> Option<String> {
    match read(profile, LOADER, report) {
        Err(()) => {}
        Ok(None) => report.fail(format!(
            "no {LOADER}: DCS loads nothing; run `nanoprobe install`"
        )),
        Ok(Some(bytes)) if bytes == RELEASE_LOADER => report.ok(format!("{LOADER} is in place")),
        Ok(Some(_)) => report.note(format!(
            "{LOADER} differs from this release's loader; `nanoprobe install` replaces it"
        )),
    }
    if let Ok(Some(_)) = read(profile, DISABLE, report) {
        report.fail(format!(
            "the kill switch {DISABLE} is there: nothing runs; remove it and restart DCS"
        ));
    }
    let manifest = match manifest(profile) {
        Ok(m) => m,
        Err(e) => {
            report.fail(e);
            return None;
        }
    };
    let Some(active) = manifest.active else {
        report.fail("no active version in nanoprobe/manifest.txt; run `nanoprobe install`");
        return None;
    };
    version(profile, &active, report);
    if let Some(previous) = manifest.previous {
        match profile.is_dir(&format!("{VERSIONS}/{previous}")) {
            Ok(true) => report.note(format!("previous version {previous} kept for rollback")),
            _ => report.note(format!(
                "the manifest names previous version {previous}, which is not installed"
            )),
        }
    }
    Some(active)
}

/// The active version folder against its `SHA256SUMS`, and its build hash.
fn version(profile: &dyn Profile, build: &str, report: &mut Report) {
    let dir = format!("{VERSIONS}/{build}");
    let Ok(sums) = read(profile, &format!("{dir}/SHA256SUMS"), report) else {
        return;
    };
    let Some(sums) = sums else {
        report.fail(format!(
            "active version {build}: no {dir}/SHA256SUMS; run `nanoprobe install`"
        ));
        return;
    };
    let sums = String::from_utf8_lossy(&sums).into_owned();
    let mut listed = Vec::new();
    let mut broken = Vec::new();
    for line in sums.lines() {
        let Some((sum, path)) = line.split_once("  ") else {
            broken.push(format!("a line of SHA256SUMS does not read: {line:?}"));
            continue;
        };
        listed.push(path.to_owned());
        match profile.read(&format!("{dir}/{path}")) {
            Ok(Some(bytes)) if sha256(&bytes) == sum => {}
            Ok(Some(_)) => broken.push(format!("{path} changed")),
            Ok(None) => broken.push(format!("{path} is missing")),
            Err(e) => broken.push(format!("{path}: {e}")),
        }
    }
    let dll = format!("nanoprobe_agent_{build}.dll");
    if !listed.contains(&dll) {
        broken.push(format!("SHA256SUMS lists no {dll}"));
    }
    for name in VERSION_LUA {
        let path = format!("lua/{name}");
        if !listed.contains(&path) {
            broken.push(format!("SHA256SUMS lists no {path}"));
        }
    }
    if build_of(&sums, &dll) != build {
        broken.push("SHA256SUMS does not hash to the folder's build".into());
    }
    if broken.is_empty() {
        report.ok(format!(
            "active version {build}: {} files match SHA256SUMS",
            listed.len()
        ));
    } else {
        report.fail(format!(
            "active version {build}: {}; run `nanoprobe install` again",
            broken.join("; ")
        ));
    }
}

/// The build hash `spec/install.md` defines: the first 16 hex digits of the
/// SHA-256 of the checksum list with the DLL listed as `agent.dll`.
pub fn build_of(sums: &str, dll: &str) -> String {
    let mut lines: Vec<(String, String)> = sums
        .lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(sum, path)| {
            let path = if path == dll { "agent.dll" } else { path };
            (path.to_owned(), sum.to_owned())
        })
        .collect();
    lines.sort();
    let text: String = lines
        .iter()
        .map(|(path, sum)| format!("{sum}  {path}\n"))
        .collect();
    sha256(text.as_bytes())[..16].to_owned()
}

/// The token, `agent.toml`, and the build DCS runs.
fn settings(profile: &dyn Profile, report: &mut Report) {
    match read(profile, TOKEN, report) {
        Err(()) => {}
        Ok(Some(token)) if !token.is_empty() => report.ok(format!("{TOKEN} is there")),
        Ok(Some(_)) => report.fail(format!(
            "{TOKEN} is empty: the agent will not listen; delete it and run `nanoprobe install`"
        )),
        Ok(None) => report.fail(format!(
            "no {TOKEN}: the agent will not listen; run `nanoprobe install`"
        )),
    }
    match read(profile, AGENT_TOML, report) {
        Err(()) => {}
        Ok(None) => report.ok(format!("no {AGENT_TOML}: every setting is its default")),
        Ok(Some(bytes)) => match AgentToml::parse(&String::from_utf8_lossy(&bytes)) {
            Ok(_) => report.ok(format!("{AGENT_TOML} reads")),
            Err(e) => report.fail(format!(
                "{AGENT_TOML}: {e}; the agent starts without its link"
            )),
        },
    }
    if let Ok(Some(running)) = read(profile, RUNNING, report) {
        let running = String::from_utf8_lossy(&running);
        if let Some(build) = running.lines().find_map(|l| l.strip_prefix("build=")) {
            report.note(format!("the last DCS start loaded build {build}"));
        }
    }
}

/// What `dcs.log` says of the newest DCS start.
enum Start {
    /// No `dcs.log`: DCS has not run with this profile.
    NoLog,
    /// The newest start ran no hook, or `dcs.log` does not read.
    NoHook,
    /// The hook's session ID in the newest start.
    Session(String),
}

/// The platform's lines in the newest start of `dcs.log`.
fn dcs_log(profile: &dyn Profile, root: &Path, active: Option<&str>, report: &mut Report) -> Start {
    let bytes = match read(profile, DCS_LOG, report) {
        Err(()) => return Start::NoHook,
        Ok(None) => {
            report.note(format!("no {DCS_LOG}: DCS has not run with this profile"));
            return Start::NoLog;
        }
        Ok(Some(bytes)) => bytes,
    };
    let lines = dcslog::newest(&bytes);
    if lines.is_empty() {
        if before_install(root, &bytes) {
            report.note(format!(
                "{DCS_LOG} is from before the install: restart DCS to load nanoprobe"
            ));
        } else {
            report.fail(format!(
                "{DCS_LOG}: no nanoprobe line in the newest start: DCS did not run the loader; check {LOADER}"
            ));
        }
        return Start::NoHook;
    }
    let mut session = Start::NoHook;
    for Line { level, message } in lines {
        if let Some((id, build)) = dcslog::session(&message) {
            session = Start::Session(id.to_owned());
            match active {
                Some(active) if active != build => report.note(format!(
                    "{DCS_LOG}: the hook started build {build}; restart DCS to load {active}"
                )),
                _ => report.ok(format!("{DCS_LOG}: the hook started build {build}")),
            }
        } else if level == "ERROR" {
            report.fail(format!("{DCS_LOG}: {message}"));
        } else if level == "WARNING" || message.starts_with("disabled by") {
            report.note(format!("{DCS_LOG}: {message}"));
        }
    }
    session
}

/// Whether the newest start in `dcs.log` began before the loader was
/// written: by the time on its `=== Log opened` line, or, without one, by the
/// file's modification time. A running DCS keeps writing the file, so its
/// time alone does not say when the start began.
fn before_install(root: &Path, log: &[u8]) -> bool {
    let Ok(loader) = std::fs::metadata(root.join(LOADER)).and_then(|m| m.modified()) else {
        return false;
    };
    let began = dcslog::newest_opened(log).or_else(|| {
        std::fs::metadata(root.join(DCS_LOG))
            .and_then(|m| m.modified())
            .ok()
    });
    began.is_some_and(|began| began <= loader)
}

/// One record of the platform's JSON-lines logs.
struct Record {
    wall: String,
    level: String,
    msg: String,
    data: serde_json::Map<String, serde_json::Value>,
}

/// A log and the file it rotated to, oldest record first.
fn records(profile: &dyn Profile, name: &str, report: &mut Report) -> Option<Vec<Record>> {
    let path = format!("{LOGS}/{name}");
    let older = read(profile, &format!("{path}.1"), report).ok().flatten();
    let newer = read(profile, &path, report).ok().flatten();
    if older.is_none() && newer.is_none() {
        return None;
    }
    let mut out = Vec::new();
    for bytes in [older, newer].into_iter().flatten() {
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Ok(serde_json::Value::Object(mut value)) = serde_json::from_str(line) else {
                continue;
            };
            let mut text = |key: &str| match value.remove(key) {
                Some(serde_json::Value::String(s)) => s,
                _ => String::new(),
            };
            let (wall, level, msg) = (text("wall"), text("level"), text("msg"));
            let data = match value.remove("data") {
                Some(serde_json::Value::Object(data)) => data,
                _ => serde_json::Map::new(),
            };
            out.push(Record {
                wall,
                level,
                msg,
                data,
            });
        }
    }
    Some(out)
}

/// The hook's and the shim's records from the newest DCS start: the hook's
/// from its `hook loaded` record with that start's session (the last one
/// when there is no `dcs.log`), the shim's from that record's time on. Each
/// error fails; each state's `capabilities` record names what DCS lacks.
fn platform_logs(profile: &dyn Profile, start: &Start, report: &mut Report) {
    let session = match start {
        Start::NoHook => {
            report.note(
                "the newest start ran no hook, so hook.log and shim.log are from an earlier one and are not checked",
            );
            return;
        }
        Start::NoLog => None,
        Start::Session(id) => Some(id.as_str()),
    };
    let Some(hook) = records(profile, "hook.log", report) else {
        report.note("no nanoprobe/logs/hook.log: the hook has not run yet");
        return;
    };
    let loaded = |r: &Record| r.msg == "hook loaded";
    let start = match session {
        Some(id) => hook
            .iter()
            .rposition(|r| loaded(r) && r.data.get("session").and_then(|v| v.as_str()) == Some(id)),
        None => hook.iter().rposition(loaded),
    };
    let Some(start) = start else {
        match session {
            Some(id) if matches!(profile.read(&format!("{LOGS}/hook.log.1")), Ok(Some(_))) => {
                report.note(format!(
                    "nanoprobe/logs/hook.log no longer holds the start of session {id}: it rotated out"
                ));
            }
            Some(id) => report.fail(format!(
                "nanoprobe/logs/hook.log has no record of session {id}, the newest start: the hook could not write its log"
            )),
            None => report.note("nanoprobe/logs/hook.log holds no start of the hook"),
        }
        return;
    };
    let since = hook[start].wall.clone();
    let hook = &hook[start..];
    let shim = records(profile, "shim.log", report).unwrap_or_default();
    // Lua writes whole seconds, so a shim record of the same second counts.
    let shim: Vec<Record> = shim.into_iter().filter(|r| r.wall >= since).collect();

    for (name, state, records) in [
        ("hook.log", "hook state", hook),
        ("shim.log", "server state", &shim[..]),
    ] {
        let mut errors: Vec<&str> = Vec::new();
        for r in records.iter().filter(|r| r.level == "error") {
            if !errors.contains(&r.msg.as_str()) {
                errors.push(&r.msg);
            }
        }
        for msg in errors {
            report.fail(format!(
                "nanoprobe/logs/{name} since the newest start: {msg}"
            ));
        }
        match records.iter().rev().find(|r| r.msg == "capabilities") {
            Some(record) => {
                let mut missing: Vec<&str> = record
                    .data
                    .iter()
                    .filter(|(_, v)| v.as_bool() != Some(true))
                    .map(|(k, _)| k.as_str())
                    .collect();
                missing.sort_unstable();
                if missing.is_empty() {
                    report.ok(format!(
                        "the {state} has all {} DCS names the platform calls",
                        record.data.len()
                    ));
                } else {
                    report.fail(format!(
                        "the {state} lacks {}: DCS changed them",
                        missing.join(", ")
                    ));
                }
            }
            None if name == "shim.log" => report.note(
                "the shim has not started since the newest start: it starts with the first mission; if a mission ran, the hook could not reach the server state",
            ),
            None => report.note(format!("no capabilities record for the {state} since the newest start")),
        }
    }
}

/// The two `net.*` keys in `autoexec.cfg`.
fn gates(profile: &dyn Profile, report: &mut Report) {
    let gates = match autoexec::gates(profile) {
        Ok(g) => g,
        Err(e) => {
            report.fail(format!("cannot read {AUTOEXEC}: {e}"));
            return;
        }
    };
    let place = if gates.block {
        "after the nanoprobe block"
    } else {
        "with no nanoprobe block"
    };
    if gates.damaged {
        report.fail(format!(
            "{AUTOEXEC}: the nanoprobe block was edited or cut short; remove it and run `nanoprobe install`"
        ));
    } else if gates.block {
        report.ok(format!(
            "{AUTOEXEC}: the nanoprobe block allows `userhooks` and `server`"
        ));
    } else if gates.later.is_empty() {
        report.note(format!(
            "{AUTOEXEC}: no nanoprobe block, and nothing sets net.allow_unsafe_api or net.allow_dostring_in; `nanoprobe install` adds the block"
        ));
    } else {
        report.note(format!(
            "{AUTOEXEC}: no nanoprobe block; `nanoprobe install` adds it"
        ));
    }
    for (line, key, names) in gates.overrides() {
        let names = names.as_deref().unwrap_or_default().join(", ");
        report.fail(format!(
            "{AUTOEXEC} line {line}: {key} is set to {{ {names} }} {place}, which takes away what the platform needs"
        ));
    }
    for (line, key, _) in gates.unreadable() {
        report.note(format!(
            "{AUTOEXEC} line {line}: {key} is set {place} in a form doctor cannot read; check that it keeps `userhooks` and `server`"
        ));
    }
}

/// On Unix, the owner of the token against the owner of the profile: DCS
/// runs as the profile's owner and must read the token.
#[cfg(unix)]
fn owner(root: &Path, report: &mut Report) {
    use std::os::unix::fs::MetadataExt as _;
    let (Ok(profile), Ok(token)) = (std::fs::metadata(root), std::fs::metadata(root.join(TOKEN)))
    else {
        return;
    };
    if profile.uid() != token.uid() {
        report.fail(format!(
            "{TOKEN} belongs to user {} and the profile to user {}: DCS cannot read it; uninstall and install again as the profile's owner",
            token.uid(),
            profile.uid()
        ));
    }
}

#[cfg(not(unix))]
fn owner(_root: &Path, _report: &mut Report) {}

/// Run doctor on every target. Returns the exit code: 0 when nothing
/// failed, 1 when a check failed, 2 when there was nothing to check.
pub fn run(
    place: &Where,
    env: &Env,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    let targets = match targets(place, env) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "nanoprobe doctor: {e}");
            return 2;
        }
    };
    let mut failed = false;
    for (i, found) in targets.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        let report = check(found);
        failed |= report.failed();
        let _ = write!(out, "{report}");
    }
    i32::from(failed)
}
