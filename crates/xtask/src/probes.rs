//! Probes and the commands that run snippets through the eval channel:
//! `eval`, `reset`, `probes` and `api-dump`. The file protocol is in
//! [`crate::eval`]; the shapes are in `spec/eval.md`.

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::bundle::{self, Facts, Redactor};
use crate::eval;
use crate::install::Profile;

/// Seconds a probe waits for each result unless its header says otherwise.
const DEFAULT_TIMEOUT: u64 = 30;
/// Seconds `uninstall` waits for its result.
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Seconds `api-dump` waits for each state.
const DUMP_TIMEOUT: Duration = Duration::from_secs(120);
/// The states `api-dump` visits.
const DUMP_STATES: [&str; 7] = [
    "hook", "gui", "server", "mission", "export", "config", "sse",
];
/// The width, in characters, to which differ lines are cut.
const CUT: usize = 160;
/// How many consecutive eval errors end a probe run.
const MAX_ERRORS: usize = 3;

/// A probe's settings, from its leading comment lines.
#[derive(Debug, PartialEq, Eq)]
pub struct Header {
    /// The states to run it in, in order.
    pub states: Vec<String>,
    /// How long to wait for each result.
    pub timeout: Duration,
}

/// Read the header: the leading run of lines that start with `--`.
pub fn parse_header(text: &str) -> Result<Header, String> {
    let mut states = Vec::new();
    let mut timeout = DEFAULT_TIMEOUT;
    for line in text.lines().take_while(|line| line.starts_with("--")) {
        let body = line.trim_start_matches('-').trim();
        if let Some(list) = body.strip_prefix("states:") {
            states = list.split_whitespace().map(str::to_owned).collect();
        } else if let Some(seconds) = body.strip_prefix("timeout:") {
            timeout = seconds
                .trim()
                .parse()
                .map_err(|_| format!("timeout {:?} is not a number of seconds", seconds.trim()))?;
        }
    }
    if states.is_empty() {
        return Err("the header has no `-- states:` line with at least one state".into());
    }
    Ok(Header {
        states,
        timeout: Duration::from_secs(timeout),
    })
}

/// An observation whose value differs from the expected one.
#[derive(Debug, PartialEq)]
pub struct Differ {
    /// The observation's name.
    pub name: String,
    /// The value recorded.
    pub value: Value,
    /// The value expected in the chosen column.
    pub expect: Value,
}

/// How a result's observations came out in one verdict column.
#[derive(Debug, Default, PartialEq)]
pub struct Tally {
    /// Observations whose verdict is `match`.
    pub matched: usize,
    /// Observations whose verdict is `differ`.
    pub differ: usize,
    /// Observations with no verdict in the chosen columns.
    pub recorded: usize,
    /// Each observation that differs.
    pub differs: Vec<Differ>,
}

/// Count the observations in a result. The column is `wine` with a fallback
/// to `windows` when `wine` is true, and `windows` otherwise.
pub fn tally(result: &Value, wine: bool) -> Tally {
    let mut tally = Tally::default();
    let columns: &[&str] = if wine {
        &["wine", "windows"]
    } else {
        &["windows"]
    };
    let empty = Vec::new();
    let observations = result["reply"]["observations"].as_array().unwrap_or(&empty);
    for observation in observations {
        let verdict = &observation["verdict"];
        // `wine` first; `windows` only when the observation has no wine verdict.
        let column = columns
            .iter()
            .find(|c| verdict.get(**c).is_some_and(|v| !v.is_null()));
        match column.and_then(|c| verdict[*c].as_str().map(|v| (*c, v))) {
            Some((_, "match")) => tally.matched += 1,
            Some((column, "differ")) => {
                tally.differ += 1;
                tally.differs.push(Differ {
                    name: observation["name"].as_str().unwrap_or("?").to_owned(),
                    value: observation["value"].clone(),
                    expect: observation["expect"][column].clone(),
                });
            }
            _ => tally.recorded += 1,
        }
    }
    tally
}

/// Why a result counts as failed, or None.
fn failure(result: &Value) -> Option<String> {
    if result["ok"] != json!(true) {
        let why = result["error"].as_str().unwrap_or("no error given");
        return Some(format!("the request failed: {why}"));
    }
    if result["reply"]["ok"] == json!(false) {
        let why = result["reply"]["error"]
            .as_str()
            .unwrap_or("no error given");
        return Some(format!("the snippet failed: {why}"));
    }
    None
}

/// `value` as compact JSON, cut to about [`CUT`] characters.
fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > CUT {
        let cut: String = text.chars().take(CUT).collect();
        format!("{cut}...")
    } else {
        text
    }
}

/// What one cleanup report says was left behind.
fn left_behind(report: &Value) -> Vec<String> {
    let mut left = Vec::new();
    if report["new_globals"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        left.push(format!("new globals {}", compact(&report["new_globals"])));
    }
    if !(report["handlers"].is_null() || report["handlers"] == json!(0)) {
        left.push(format!("handlers {}", report["handlers"]));
    }
    if report["errors"].as_array().is_some_and(|a| !a.is_empty()) {
        left.push(format!("errors {}", compact(&report["errors"])));
    }
    left
}

/// Print a result's cleanup reports and return whether anything was left.
fn print_cleanup(reports: &[Value]) -> bool {
    let mut dirty = false;
    for report in reports {
        let state = report["state"].as_str().unwrap_or("?");
        println!("cleanup {state}: cleaned {}", compact(&report["cleaned"]));
        for left in left_behind(report) {
            dirty = true;
            println!("  LEFT BEHIND in {state}: {left}");
        }
    }
    dirty
}

/// `eval <name> <state> <file>`: run one file and print the result.
pub fn eval_file(
    profile: &dyn Profile,
    state: &str,
    file: &str,
    timeout: Duration,
) -> Result<ExitCode, String> {
    let source = std::fs::read(file).map_err(|e| format!("could not read {file}: {e}"))?;
    let stem = std::path::Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let result = eval::run(profile, state, &stem, &source, timeout)?;
    println!("{}", pretty(&result)?);
    Ok(if failure(&result).is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn pretty(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| e.to_string())
}

/// `reset <name>`: clean every state, and print the result.
pub fn reset(profile: &dyn Profile) -> Result<ExitCode, String> {
    let result = eval::run(profile, "reset", "reset", b"-- reset\n", UNINSTALL_TIMEOUT)?;
    println!("{}", pretty(&result)?);
    let errors = result["reply"].as_array().is_some_and(|reports| {
        reports
            .iter()
            .any(|r| r["errors"].as_array().is_some_and(|a| !a.is_empty()))
    });
    Ok(if result["ok"] != json!(true) || errors {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The options of `probes`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Only probes whose file stem contains this text.
    pub only: Option<String>,
    /// Leave the prelude installed afterwards.
    pub keep: bool,
    /// Run although players are connected.
    pub with_players: bool,
    /// The folder of probe files, relative to the repository. `lua/probes`
    /// when not given.
    pub dir: Option<String>,
}

impl ProbeOptions {
    /// Parse the words after the target name.
    pub fn parse(rest: &[&str]) -> Result<ProbeOptions, String> {
        let mut options = ProbeOptions::default();
        let mut words = rest.iter();
        while let Some(word) = words.next() {
            match *word {
                "--keep" => options.keep = true,
                "--with-players" => options.with_players = true,
                "--only" => {
                    let text = words.next().ok_or("--only needs a text")?;
                    options.only = Some((*text).to_owned());
                }
                "--dir" => {
                    let dir = words.next().ok_or("--dir needs a folder")?;
                    options.dir = Some((*dir).to_owned());
                }
                other => return Err(format!("unknown option: {other}")),
            }
        }
        Ok(options)
    }
}

/// One probe file.
struct Probe {
    stem: String,
    source: Vec<u8>,
    header: Header,
}

/// The probes in `dir` (default `lua/probes/`), by file name, filtered by
/// `only`. Files in subfolders are not probes.
fn load_probes(dir: Option<&str>, only: Option<&str>) -> Result<Vec<Probe>, String> {
    let folder = crate::repository().join(dir.unwrap_or("lua/probes"));
    let entries = std::fs::read_dir(&folder)
        .map_err(|e| format!("could not read {}: {e}", folder.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "lua") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut probes = Vec::new();
    for path in paths {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if only.is_some_and(|text| !stem.contains(text)) {
            continue;
        }
        let source =
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let header = parse_header(&String::from_utf8_lossy(&source))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        probes.push(Probe {
            stem,
            source,
            header,
        });
    }
    Ok(probes)
}

/// `probes <name>`: run the probes and report. `wine` picks the verdict
/// column. The caller has already checked for players.
pub fn run_probes(
    profile: &dyn Profile,
    facts: &Facts,
    name: &str,
    options: &ProbeOptions,
) -> Result<ExitCode, String> {
    run_probes_to(profile, facts, name, options).map(|(code, _)| code)
}

/// As [`run_probes`], and the path of the report it wrote.
pub fn run_probes_to(
    profile: &dyn Profile,
    facts: &Facts,
    name: &str,
    options: &ProbeOptions,
) -> Result<(ExitCode, std::path::PathBuf), String> {
    let wine = facts.kind == "docker-wine";
    let probes = load_probes(options.dir.as_deref(), options.only.as_deref())?;
    if probes.is_empty() {
        return Err("no probes match".into());
    }

    let mut failed = false;
    let mut results = Vec::new();
    let mut consecutive = 0;
    'run: for probe in &probes {
        for state in &probe.header.states {
            let stem = format!("{}-{state}", probe.stem);
            let outcome = eval::run(profile, state, &stem, &probe.source, probe.header.timeout);
            let label = format!("{}@{state}", probe.stem);
            match outcome {
                Err(error) => {
                    failed = true;
                    consecutive += 1;
                    println!("{label}  FAILED  0 match, 0 differ, 0 recorded");
                    println!("  {error}");
                    results.push(json!({
                        "probe": probe.stem, "state": state, "ok": false, "error": error,
                    }));
                    if consecutive >= MAX_ERRORS {
                        println!("{MAX_ERRORS} errors in a row; DCS is probably gone. Stopping.");
                        break 'run;
                    }
                }
                Ok(result) => {
                    consecutive = 0;
                    let tally = tally(&result, wine);
                    let why = failure(&result);
                    failed |= why.is_some();
                    println!(
                        "{label}  {}  {} match, {} differ, {} recorded",
                        if why.is_some() { "FAILED" } else { "ok" },
                        tally.matched,
                        tally.differ,
                        tally.recorded
                    );
                    if let Some(why) = &why {
                        println!("  {why}");
                    }
                    for differ in &tally.differs {
                        println!(
                            "  differ {}: value {}, expected {}",
                            differ.name,
                            compact(&differ.value),
                            compact(&differ.expect)
                        );
                    }
                    results.push(json!({
                        "probe": probe.stem, "state": state, "ok": why.is_none(),
                        "error": why, "result": result,
                    }));
                }
            }
        }
    }

    let mut cleanup = Value::Null;
    if !options.keep {
        match eval::run(
            profile,
            "uninstall",
            "uninstall",
            b"-- uninstall\n",
            UNINSTALL_TIMEOUT,
        ) {
            Ok(result) => {
                let reports = result["reply"].as_array().cloned().unwrap_or_default();
                if let Some(why) = failure(&result) {
                    failed = true;
                    println!("cleanup FAILED: {why}");
                }
                failed |= print_cleanup(&reports);
                cleanup = result;
            }
            Err(error) => {
                failed = true;
                println!("cleanup FAILED: {error}");
                cleanup = json!({ "ok": false, "error": error });
            }
        }
    }

    let report = json!({
        "target": name,
        "kind": facts.kind,
        "dcs": facts.dcs,
        "wine": facts.wine,
        "time": bundle::utc_now(),
        "results": results,
        "cleanup": cleanup,
    });
    let text = pretty(&report)?;
    let text = Redactor::new(&bundle::key()?, &[&text]).redact(&text);
    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("probes-{name}-{}.json", bundle::stamp()));
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    let code = if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    Ok((code, path))
}

/// The lines of an existing dump file, headers aside.
fn body_lines(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// A dump file's text.
fn dump_text(facts: &Facts, state: &str, lines: &[String], truncated: bool) -> String {
    let mut out = format!(
        "# state {state}\n# dcs {}\n# platform {}\n# date {}\n# script lua/probes/tools/api-dump.lua\n",
        facts.dcs.as_deref().unwrap_or("unknown"),
        facts.wine.as_deref().unwrap_or("windows"),
        &bundle::utc_now()[..10],
    );
    if truncated {
        out.push_str("# truncated\n");
    }
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `api-dump <name>`: write `lua/mock/api/<state>.txt` for each state.
pub fn api_dump(profile: &dyn Profile, facts: &Facts) -> Result<ExitCode, String> {
    let script_path = crate::repository().join("lua/probes/tools/api-dump.lua");
    let source = std::fs::read(&script_path)
        .map_err(|e| format!("could not read {}: {e}", script_path.display()))?;
    let folder = crate::repository().join("lua/mock/api");
    let mut failed = false;
    for state in DUMP_STATES {
        match dump_state(profile, facts, state, &source, &folder) {
            Ok(()) => {}
            Err(error) => {
                failed = true;
                println!("{state}: FAILED: {error}");
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Dump one state and write its file.
fn dump_state(
    profile: &dyn Profile,
    facts: &Facts,
    state: &str,
    source: &[u8],
    folder: &std::path::Path,
) -> Result<(), String> {
    let result = eval::run(
        profile,
        state,
        &format!("api-dump-{state}"),
        source,
        DUMP_TIMEOUT,
    )?;
    if let Some(why) = failure(&result) {
        return Err(why);
    }
    let value = &result["reply"]["values"][0];
    let text = value["text"]
        .as_str()
        .ok_or("the snippet returned no `text` string")?;
    let mut lines: Vec<String> = text
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    lines.sort();
    let truncated = value["truncated"] == json!(true);

    let path = folder.join(format!("{state}.txt"));
    let old = match std::fs::read_to_string(&path) {
        Ok(text) => Some(body_lines(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let new: BTreeSet<String> = lines.iter().cloned().collect();
    println!(
        "{state}: {} lines{}",
        lines.len(),
        if truncated { " (truncated)" } else { "" }
    );
    if let Some(old) = old {
        let added: Vec<&String> = new.difference(&old).collect();
        let removed: Vec<&String> = old.difference(&new).collect();
        println!("  {} added, {} removed", added.len(), removed.len());
        for line in added.iter().take(20) {
            println!("  + {line}");
        }
        for line in removed.iter().take(20) {
            println!("  - {line}");
        }
    } else {
        println!("  new file");
    }
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    std::fs::write(&path, dump_text(facts, state, &lines, truncated))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_header() {
        let header = parse_header(
            "-- a probe\n-- states: gui server\n-- timeout: 60\nreturn 1\n-- states: x\n",
        )
        .unwrap();
        assert_eq!(header.states, vec!["gui", "server"]);
        assert_eq!(header.timeout, Duration::from_secs(60));

        let header = parse_header("-- states: mission\nlocal x\n").unwrap();
        assert_eq!(header.timeout, Duration::from_secs(30));
    }

    #[test]
    fn a_header_needs_a_state() {
        assert!(parse_header("-- timeout: 5\n").is_err());
        assert!(parse_header("return 1\n-- states: gui\n").is_err());
        assert!(parse_header("-- states: gui\n-- timeout: soon\n").is_err());
    }

    fn result() -> Value {
        json!({ "ok": true, "reply": { "ok": true, "observations": [
            { "name": "a", "value": 1, "verdict": { "windows": "match", "wine": "differ" },
              "expect": { "windows": 1, "wine": 2 } },
            { "name": "b", "value": 1, "verdict": { "windows": "differ" }, "expect": { "windows": 3 } },
            { "name": "c", "value": 1, "verdict": { "windows": "match" } },
            { "name": "d", "value": 1, "verdict": { "mock": "match" } },
            { "name": "e", "value": 1 },
        ] } })
    }

    #[test]
    fn counts_verdicts_with_the_column_fallback() {
        let local = tally(&result(), false);
        assert_eq!((local.matched, local.differ, local.recorded), (2, 1, 2));
        assert_eq!(local.differs[0].name, "b");
        assert_eq!(local.differs[0].expect, json!(3));

        let wine = tally(&result(), true);
        assert_eq!((wine.matched, wine.differ, wine.recorded), (1, 2, 2));
        assert_eq!(wine.differs[0].name, "a");
        assert_eq!(wine.differs[0].expect, json!(2));
        assert_eq!(wine.differs[1].name, "b");
    }

    #[test]
    fn finds_failures_and_leftovers() {
        assert!(failure(&result()).is_none());
        assert!(failure(&json!({ "ok": false, "error": "x" })).is_some());
        assert!(failure(&json!({ "ok": true, "reply": { "ok": false } })).is_some());

        let clean = json!({ "new_globals": [], "handlers": 0, "errors": [] });
        assert!(left_behind(&clean).is_empty());
        assert!(left_behind(&json!({ "handlers": null })).is_empty());
        let dirty = json!({ "new_globals": ["g"], "handlers": 1, "errors": ["e"] });
        assert_eq!(left_behind(&dirty).len(), 3);
    }

    #[test]
    fn cuts_long_values() {
        assert_eq!(compact(&json!("x".repeat(500))).chars().count(), CUT + 3);
        assert_eq!(compact(&json!([1, 2])), "[1,2]");
    }

    #[test]
    fn parses_probe_options() {
        let options = ProbeOptions::parse(&["--only", "net", "--keep"]).unwrap();
        assert_eq!(options.only.as_deref(), Some("net"));
        assert!(options.keep && !options.with_players);
        assert!(ProbeOptions::parse(&["--only"]).is_err());
        assert!(ProbeOptions::parse(&["--x"]).is_err());
    }

    #[test]
    fn writes_a_dump_file() {
        let facts = Facts {
            kind: "docker-wine".into(),
            wine: Some("wine-10.0".into()),
            dcs: Some("2.9 (branch)".into()),
        };
        let text = dump_text(&facts, "gui", &["a".into(), "b".into()], true);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("# state gui"));
        assert_eq!(lines.next(), Some("# dcs 2.9 (branch)"));
        assert_eq!(lines.next(), Some("# platform wine-10.0"));
        assert!(lines.next().unwrap().starts_with("# date "));
        assert_eq!(lines.next(), Some("# script lua/probes/tools/api-dump.lua"));
        assert_eq!(lines.next(), Some("# truncated"));
        assert_eq!(body_lines(&text).len(), 2);
    }
}
