//! `catalog`: the event catalog of experiment E9 (step S9).
//!
//! `catalog <name>` runs on a docker-wine target. It deploys the build with
//! the eval channel, restarts DCS, loads a fresh shell mission, and attaches
//! `link-probe` in the game container as the sidecar, recording the agent's
//! stream (`link-probe --record`). It then runs the scenarios in
//! `lua/catalog` as probes, each of which reports what the Lua event
//! recorder (`lua/dev/lib/recorder.lua`) saw, loads the shell again so the
//! mission's end and start are recorded, and stops the probe.
//!
//! `catalog --build <probes.json> [--recording FILE]... --out STEM` builds
//! the report from those two sources and writes `STEM.md`, the field-presence
//! report, and `STEM.json`, the catalog. For each event type reached it
//! gives how often each field was present and in what shape, which field the
//! allow-list reads it as (spec/records.md), the fields the allow-list names
//! that never came, the fields that came and no list reads, and, from the
//! recording, how often each forwarded field was in the agent's `EVENT`
//! records. Every `EVENT` in a recording must decode; one that does not is
//! named in the report and fails the build.
//!
//! The live part writes everything under `bundles/catalog-<name>-<time>/`:
//! the probes report, the recording, the probe's output, and the built
//! report as `<date>-event-catalog.{md,json}`. A recording holds server
//! data, so nothing from it is committed but the built report, which has
//! field names, shapes and counts only. The report is committed to
//! `docs/results/` under that name, with the header results documents need;
//! the test `every_cataloged_shape_decodes` reads every
//! `docs/results/*event-catalog*.json`. Before each disruptive step the
//! live run waits for the server to have no player.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::kind;
use nanoprobe_transport::records::Event;
use serde_json::{Value, json};

use crate::install::Profile as _;
use crate::profile::Target;
use crate::{bundle, link, live, miz, probes};

/// Where the probe goes in the container.
const DIR: &str = "/tmp/nanoprobe-catalog";
/// How long the probe may take to attach after it starts.
const ATTACH_S: u64 = 60;
/// How long to keep recording after the last load, for its events.
const SETTLE_S: u64 = 10;
/// How long to wait for a connected player to leave before a disruptive
/// step gives up.
const PLAYER_WAIT: Duration = Duration::from_secs(30 * 60);

/// Wait until no player is connected, up to [`PLAYER_WAIT`], before `step`.
fn empty(remote: &crate::remote::Remote, step: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let n = remote.players()?;
        if n == 0 {
            return Ok(());
        }
        if start.elapsed() > PLAYER_WAIT {
            return Err(format!(
                "{step}: {n} player(s) connected for {PLAYER_WAIT:?}; stopped"
            ));
        }
        println!("{step}: {n} player(s) connected; waiting");
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// `catalog <name>` and `catalog --build ...`.
pub fn command(repository: &Path, rest: &[&str]) -> Result<ExitCode, String> {
    match rest {
        ["--build", rest @ ..] => {
            let mut probes = None;
            let mut recordings = Vec::new();
            let mut out = None;
            let mut words = rest.iter();
            while let Some(word) = words.next() {
                match *word {
                    "--recording" | "--out" => {
                        let value = words
                            .next()
                            .ok_or_else(|| format!("{word} needs a value"))?;
                        if *word == "--out" {
                            out = Some(PathBuf::from(value));
                        } else {
                            recordings.push(PathBuf::from(value));
                        }
                    }
                    other if probes.is_none() && !other.starts_with("--") => {
                        probes = Some(PathBuf::from(other));
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            let probes = probes.ok_or("--build needs a probes report")?;
            let out = out.ok_or("--build needs --out STEM")?;
            build_files(repository, &probes, &recordings, &out)
        }
        [name] => live_run(repository, name),
        _ => Err(format!("unknown options: {}", rest.join(" "))),
    }
}

/// The event_field codes and the allow-lists of spec/records.md.
#[derive(Debug, Default)]
pub struct Spec {
    /// Field name to its code and the path it is read from, such as
    /// `initiator.id_`.
    pub fields: BTreeMap<String, (u16, String)>,
    /// Event name to the field names its allow-list reads.
    pub allow: BTreeMap<String, Vec<String>>,
    /// Event name to its allow-list row's Evidence cell.
    pub evidence: BTreeMap<String, String>,
}

/// The section of `text` under the heading `### <title>`, up to the next
/// heading.
fn section<'a>(text: &'a str, title: &str) -> Option<&'a str> {
    let start = text.find(&format!("\n### {title}\n"))? + 1;
    let body = &text[start..];
    let end = body[4..].find("\n#").map_or(body.len(), |e| e + 4);
    Some(&body[..end])
}

/// The cells of each table row of `text` that is not a header or a rule.
fn rows(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut header = true;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            header = true;
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_owned())
            .collect();
        if header {
            header = false;
            continue;
        }
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue;
        }
        out.push(cells);
    }
    out
}

impl Spec {
    /// Read the tables from the text of spec/records.md.
    pub fn parse(text: &str) -> Result<Spec, String> {
        let mut spec = Spec::default();
        let codes = section(text, "Codes: event_field").ok_or("no event_field codes")?;
        for row in rows(codes) {
            let [code, name, _, from] = row.as_slice() else {
                return Err(format!("an event_field row of {} cells", row.len()));
            };
            let code: u16 = code
                .parse()
                .map_err(|_| format!("event_field code {code:?}"))?;
            let path = from
                .trim_matches('`')
                .strip_prefix("event.")
                .map(str::to_owned)
                .unwrap_or_default();
            spec.fields.insert(name.clone(), (code, path));
        }
        let allow = section(text, "Allow-lists").ok_or("no allow-lists")?;
        for row in rows(allow) {
            // Event, fields, and the evidence.
            let [event, fields, evidence] = row.as_slice() else {
                return Err(format!("an allow-list row of {} cells", row.len()));
            };
            spec.evidence.insert(event.clone(), evidence.clone());
            let fields: Vec<String> = fields.split(',').map(|f| f.trim().to_owned()).collect();
            for f in &fields {
                if !spec.fields.contains_key(f) {
                    return Err(format!("{event} allows {f}, which is no event_field"));
                }
            }
            spec.allow.insert(event.clone(), fields);
        }
        Ok(spec)
    }

    /// The field read from `path`, if any.
    fn field_at(&self, path: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(_, (_, p))| p == path)
            .map(|(n, _)| n.as_str())
    }

    /// The field name of `code`.
    fn field_of(&self, code: u16) -> Option<&str> {
        self.fields
            .iter()
            .find(|(_, (c, _))| *c == code)
            .map(|(n, _)| n.as_str())
    }
}

/// The `world.event` names and IDs of an API dump (`lua/mock/api/server.txt`).
pub fn event_names(dump: &str) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for line in dump.lines() {
        let Some(rest) = line.strip_prefix("world.event.") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(" : number = ") else {
            continue;
        };
        if let Ok(id) = value.trim().parse::<u32>()
            && name.starts_with("S_EVENT_")
            && name != "S_EVENT_MAX"
            && name != "S_EVENT_INVALID"
        {
            out.insert(id, name.to_owned());
        }
    }
    out
}

/// One event type in the catalog.
#[derive(Debug, Default)]
struct Entry {
    id: Option<u32>,
    count: u64,
    causes: BTreeMap<String, u64>,
    /// Path to shape to count.
    fields: BTreeMap<String, BTreeMap<String, u64>>,
    /// Cause to path to the events of that cause that carried it.
    by_cause: BTreeMap<String, BTreeMap<String, u64>>,
    /// Path to value to count, for the fields the recorder counts values
    /// of, such as `object_category`.
    values: BTreeMap<String, BTreeMap<String, u64>>,
    /// The same, per cause.
    values_by_cause: BTreeMap<String, BTreeMap<String, BTreeMap<String, u64>>>,
    /// Cause to the largest distance, in seconds, between an event's time
    /// and model time when it arrived.
    skew: BTreeMap<String, f64>,
    /// `EVENT` records in the recordings.
    records: u64,
    /// Field name, or `code <n>`, to the records that carried it.
    forwarded: BTreeMap<String, u64>,
}

/// What the recordings held besides events.
#[derive(Debug, Default)]
struct Recorded {
    records: u64,
    events: u64,
    undecodable: Vec<String>,
    /// Records read before, by agent boot and number, and how many came
    /// again.
    seen: BTreeSet<(String, u64)>,
    duplicates: u64,
    /// A last line cut off by the probe's stop.
    cut_off: u64,
}

/// Merge every `catalog` observation of a probes report into `by_name`.
/// Returns the scenarios that reported, and those that failed or reported
/// no catalog, each with why. A scenario's numeric observations named
/// `fact.<name>` go into `facts`.
fn merge_probes(
    report: &Value,
    by_name: &mut BTreeMap<String, Entry>,
    facts: &mut Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let mut probes = Vec::new();
    let mut failed = Vec::new();
    for result in report["results"].as_array().into_iter().flatten() {
        let probe = result["probe"].as_str().unwrap_or("?");
        if result["ok"] == json!(false) {
            let why = result["error"].as_str().unwrap_or("failed");
            failed.push(format!("{probe}: {why}"));
        }
        let observations = result["result"]["reply"]["observations"].as_array();
        let reported = observations
            .into_iter()
            .flatten()
            .any(|o| o["name"] == "catalog");
        for o in observations.into_iter().flatten() {
            if let Some(fact) = o["name"].as_str().and_then(|n| n.strip_prefix("fact."))
                && let Some(v) = o["value"].as_f64()
            {
                facts.push(format!("{probe}: {fact} = {v}"));
            }
        }
        if !reported && result["ok"] != json!(false) {
            failed.push(format!("{probe}: no catalog observation"));
        }
        for o in observations.into_iter().flatten() {
            if o["name"] != "catalog" {
                continue;
            }
            probes.push(probe.to_owned());
            let by = o["value"]["by"].as_object();
            for (cause, types) in by.into_iter().flatten() {
                for (name, e) in types.as_object().into_iter().flatten() {
                    let entry = by_name.entry(name.clone()).or_default();
                    let n = e["count"].as_u64().unwrap_or(0);
                    entry.count += n;
                    *entry.causes.entry(cause.clone()).or_default() += n;
                    if entry.id.is_none() {
                        entry.id = e["id"].as_u64().and_then(|i| u32::try_from(i).ok());
                    }
                    for (path, shapes) in e["fields"].as_object().into_iter().flatten() {
                        let f = entry.fields.entry(path.clone()).or_default();
                        let mut present = 0;
                        for (shape, c) in shapes.as_object().into_iter().flatten() {
                            *f.entry(shape.clone()).or_default() += c.as_u64().unwrap_or(0);
                            present += c.as_u64().unwrap_or(0);
                        }
                        *entry
                            .by_cause
                            .entry(cause.clone())
                            .or_default()
                            .entry(path.clone())
                            .or_default() += present;
                    }
                    for (path, values) in e["values"].as_object().into_iter().flatten() {
                        for (value, c) in values.as_object().into_iter().flatten() {
                            let c = c.as_u64().unwrap_or(0);
                            *entry
                                .values
                                .entry(path.clone())
                                .or_default()
                                .entry(value.clone())
                                .or_default() += c;
                            *entry
                                .values_by_cause
                                .entry(cause.clone())
                                .or_default()
                                .entry(path.clone())
                                .or_default()
                                .entry(value.clone())
                                .or_default() += c;
                        }
                    }
                    if let Some(skew) = e["skew"].as_f64() {
                        let s = entry.skew.entry(cause.clone()).or_insert(0.0);
                        *s = s.max(skew);
                    }
                }
            }
        }
    }
    (probes, failed)
}

/// Read the `EVENT` records of a recording (`link-probe --record`).
fn merge_recording(
    text: &str,
    spec: &Spec,
    names: &BTreeMap<u32, String>,
    by_name: &mut BTreeMap<String, Entry>,
    recorded: &mut Recorded,
) {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            // A probe stopped in the middle of a write leaves its last line
            // cut off: reported, not a decoding failure.
            if i + 1 == lines.len() && !text.ends_with('\n') {
                recorded.cut_off += 1;
            } else {
                recorded
                    .undecodable
                    .push(format!("line {}: not JSON", i + 1));
            }
            continue;
        };
        // link-probe writes an attach marker line on each attach.
        if r["attach"] == serde_json::Value::Bool(true) {
            continue;
        }
        let (Some(boot), Some(number)) = (r["boot"].as_str(), r["number"].as_u64()) else {
            recorded
                .undecodable
                .push(format!("line {}: no boot or number", i + 1));
            continue;
        };
        let key = (boot.to_owned(), number);
        if !recorded.seen.insert(key) {
            recorded.duplicates += 1;
            continue;
        }
        recorded.records += 1;
        if r["kind"].as_u64() != Some(kind::EVENT) {
            continue;
        }
        recorded.events += 1;
        let payload = r["payload"].as_str().and_then(unhex);
        let decoded = payload.as_deref().map(Event::decode);
        let Some(Ok(event)) = decoded else {
            recorded
                .undecodable
                .push(format!("line {}: an EVENT that does not decode", i + 1));
            continue;
        };
        let name = names
            .get(&event.id)
            .cloned()
            .unwrap_or_else(|| format!("event {}", event.id));
        let entry = by_name.entry(name).or_default();
        entry.id.get_or_insert(event.id);
        entry.records += 1;
        for (code, value) in &event.fields {
            if matches!(value, Lv::Nil) {
                continue;
            }
            let field = spec
                .field_of(*code)
                .map_or_else(|| format!("code {code}"), str::to_owned);
            *entry.forwarded.entry(field).or_default() += 1;
        }
    }
}

fn text_of(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_owned)
}

/// The bucket below which `share` of a histogram's counts fall, as the
/// bucket's upper bound in microseconds (spec/transport.md, "Metrics").
fn quantile_us(hist: &[u64], share: f64) -> Option<u64> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let mut seen = 0;
    for (i, n) in hist.iter().enumerate() {
        seen += n;
        if seen as f64 >= share * total as f64 {
            return Some(1u64 << i.min(62));
        }
    }
    None
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A path is a leaf when nothing was recorded below it: a scalar, or an
/// object whose `id_` is its own path.
fn leaves(fields: &BTreeMap<String, BTreeMap<String, u64>>) -> BTreeSet<&str> {
    fields
        .keys()
        .filter(|p| {
            let prefix = format!("{p}.");
            !fields.keys().any(|q| q.starts_with(&prefix))
        })
        .map(String::as_str)
        .collect()
}

/// Build the catalog as JSON, and whether it is whole: a scenario reported,
/// none failed, and every `EVENT` decoded.
fn build(
    report: &Value,
    recordings: &[String],
    spec: &Spec,
    names: &BTreeMap<u32, String>,
) -> (Value, bool) {
    let mut by_name = BTreeMap::new();
    let mut facts = Vec::new();
    let (probes, failed) = merge_probes(report, &mut by_name, &mut facts);
    // The live run's own names first: its world.event may differ from the
    // API dump's. A disagreement is reported.
    let mut live_names = names.clone();
    let mut name_conflicts = Vec::new();
    for (name, e) in &by_name {
        if let Some(id) = e.id {
            if let Some(dump) = names.get(&id).filter(|d| *d != name) {
                name_conflicts.push(format!("{id}: {name} in the run, {dump} in the API dump"));
            }
            live_names.insert(id, name.clone());
        }
    }
    let names = &live_names;
    let mut recorded = Recorded::default();
    for text in recordings {
        merge_recording(text, spec, names, &mut by_name, &mut recorded);
    }

    let mut events = Vec::new();
    for (name, e) in &by_name {
        let allowed = spec.allow.get(name).cloned().unwrap_or_default();
        let read: BTreeSet<&str> = allowed
            .iter()
            .filter_map(|f| spec.fields.get(f).map(|(_, p)| p.as_str()))
            .collect();
        let fields: Vec<Value> = e
            .fields
            .iter()
            .map(|(path, shapes)| {
                let present: u64 = shapes.values().sum();
                json!({
                    "path": path,
                    "shapes": shapes,
                    "present": present,
                    "read_as": spec.field_at(path).filter(|_| read.contains(path.as_str())),
                })
            })
            .collect();
        let unseen: Vec<&String> = if e.count > 0 {
            allowed
                .iter()
                .filter(|f| {
                    spec.fields
                        .get(*f)
                        .is_some_and(|(_, p)| !e.fields.contains_key(p))
                })
                .collect()
        } else {
            Vec::new()
        };
        let unread: Vec<&str> = leaves(&e.fields)
            .into_iter()
            .filter(|p| !read.contains(p))
            .collect();
        // Forwarded fields the type's allow-list does not name.
        let unexpected: Vec<&String> = e
            .forwarded
            .keys()
            .filter(|f| !allowed.contains(f))
            .collect();
        // Fields forwarded more often than the recorder saw them present.
        // The recorder runs only inside scenarios and the recording spans
        // the whole run, so this is checked only where the two counts of
        // the type agree.
        let over: Vec<&String> = if e.count > 0 && e.count == e.records {
            e.forwarded
                .iter()
                .filter(|(f, n)| {
                    spec.fields
                        .get(*f)
                        .is_some_and(|(_, p)| **n > e.fields.get(p).map_or(0, |s| s.values().sum()))
                })
                .map(|(f, _)| f)
                .collect()
        } else {
            Vec::new()
        };
        // Read fields the recorder saw more often than the agent forwarded
        // them: the shim dropped them, for one a Lua type other than the
        // spec's. Checked where the two counts of the type agree.
        let under: Vec<&String> = if e.count > 0 && e.count == e.records {
            allowed
                .iter()
                .filter(|f| {
                    spec.fields.get(*f).is_some_and(|(_, p)| {
                        let present: u64 = e.fields.get(p).map_or(0, |s| s.values().sum());
                        e.forwarded.get(*f).copied().unwrap_or(0) < present
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        events.push(json!({
            "name": name,
            "id": e.id,
            "count": e.count,
            "causes": e.causes,
            "fields": fields,
            "allow": allowed,
            "allowed_unseen": unseen,
            "seen_unread": unread,
            "records": e.records,
            "forwarded": e.forwarded,
            "forwarded_unexpected": unexpected,
                        "forwarded_over_present": over,
            "forwarded_under_present": under,
            "fields_by_cause": e.by_cause,
            "values": e.values,
            "values_by_cause": e.values_by_cause,
            "time_skew_by_cause": e.skew,
            "stream_only": e.count == 0 && e.records > 0,
        }));
    }
    let reached: BTreeSet<&str> = by_name.keys().map(String::as_str).collect();
    let unreached: Vec<&String> = names
        .values()
        .filter(|n| !reached.contains(n.as_str()))
        .collect();
    let forwarding_ok = events.iter().all(|e| {
        e["forwarded_unexpected"]
            .as_array()
            .is_none_or(Vec::is_empty)
            && e["forwarded_over_present"]
                .as_array()
                .is_none_or(Vec::is_empty)
            && e["forwarded_under_present"]
                .as_array()
                .is_none_or(Vec::is_empty)
    });
    // With a recording, every type the recorder saw that has an allow-list
    // must have reached the stream too.
    let mut unforwarded = Vec::new();
    if !recordings.is_empty() {
        if recorded.events == 0 {
            unforwarded.push("the recording holds no EVENT record".to_owned());
        }
        for e in &events {
            if e["count"].as_u64() > Some(0)
                && e["records"] == json!(0)
                && !e["allow"].as_array().is_none_or(Vec::is_empty)
            {
                unforwarded.push(format!("{}: seen, never forwarded", text_of(&e["name"])));
            }
        }
    }
    let ok = recorded.undecodable.is_empty()
        && failed.is_empty()
        && !probes.is_empty()
        && forwarding_ok
        && unforwarded.is_empty();
    // The target's profile name stays out: it names one person's setup.
    let catalog = json!({
        "source": {
            "kind": report["kind"],
            "dcs": report["dcs"],
            "wine": report["wine"],
            "time": report["time"],
            "probes": probes,
            "failed": failed,
            "unforwarded": unforwarded,
            "facts": facts,
            "agent_metrics": report["agent_metrics"],
            "name_conflicts": name_conflicts,
        },
        "recorded": {
            "records": recorded.records,
            "events": recorded.events,
            "undecodable": recorded.undecodable,
            "duplicates": recorded.duplicates,
            "cut_off": recorded.cut_off,
        },
        "events": events,
        "unreached": unreached,
    });
    (catalog, ok)
}

/// The field-presence report as Markdown.
fn markdown(c: &Value) -> String {
    use std::fmt::Write as _;
    let s = &c["source"];
    let mut out = String::new();
    let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
    let _ = writeln!(out, "# Event catalog (E9)\n");
    let _ = writeln!(out, "| | |\n|---|---|");
    let _ = writeln!(out, "| Target kind | {} |", text(&s["kind"]));
    let _ = writeln!(out, "| DCS | {} |", text(&s["dcs"]));
    let _ = writeln!(out, "| Wine | {} |", text(&s["wine"]));
    let _ = writeln!(out, "| Run | {} |", text(&s["time"]));
    let probes: Vec<String> = s["probes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    let _ = writeln!(
        out,
        "| Script | `cargo xtask catalog`, scenarios {} in `lua/catalog` |",
        probes.join(", ")
    );
    let r = &c["recorded"];
    let _ = writeln!(
        out,
        "| Recording | {} records, {} `EVENT`, {} that did not decode, {} read twice and counted once, {} last line cut off |\n",
        r["records"],
        r["events"],
        r["undecodable"].as_array().map_or(0, Vec::len),
        r["duplicates"],
        r["cut_off"]
    );
    let m = &s["agent_metrics"];
    for (key, label) in [
        ("hist_event", "Agent time per event"),
        ("hist_pump", "Agent time per pump call"),
    ] {
        let hist: Vec<u64> = m[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect();
        if let (Some(p50), Some(p99)) = (quantile_us(&hist, 0.5), quantile_us(&hist, 0.99)) {
            let _ = writeln!(
                out,
                "{label} (`{key}`, status block at the end of the run): {} calls, half under {p50} µs, 99 % under {p99} µs.\n",
                hist.iter().sum::<u64>()
            );
        }
    }
    if let Some(d) = m["events_dropped"].as_u64() {
        let _ = writeln!(
            out,
            "Events dropped by the agent (`events_dropped`): {d}.\n"
        );
    }
    for (key, label) in [
        ("unforwarded", "Not forwarded"),
        ("failed", "Scenario that failed or reported no catalog"),
        (
            "name_conflicts",
            "Event ID named differently in the run and the API dump",
        ),
    ] {
        for v in s[key].as_array().into_iter().flatten() {
            let _ = writeln!(out, "{label}: {}\n", text(v));
        }
    }
    let _ = writeln!(
        out,
        "Present counts the events of the type that carried the field. A shape is a Lua \
         type, `table` for a plain table, or `object:<class>` for a table with a class's \
         metatable. Read as names the `event_field` the allow-list reads it as \
         (spec/records.md). Forwarded counts the agent's `EVENT` records that carried it.\n"
    );
    for e in c["events"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "## {} ({})\n\n{} seen by the recorder, {} `EVENT` records. Causes: {}.\n",
            text(&e["name"]),
            text(&e["id"]),
            e["count"],
            e["records"],
            e["causes"]
                .as_object()
                .map(|m| m
                    .iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect::<Vec<_>>()
                    .join(", "))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "none recorded".into())
        );
        if e["stream_only"] == json!(true) {
            let _ = writeln!(
                out,
                "Seen only in the stream: the Lua recorder was not running, so there is no \
                 field-presence data for this type.\n"
            );
        }
        let fields = e["fields"].as_array().cloned().unwrap_or_default();
        if !fields.is_empty() {
            let _ = writeln!(
                out,
                "| Field | Shapes | Present | By cause | Read as | Forwarded |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            for f in &fields {
                let shapes = f["shapes"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| format!("{k} {v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let path = text(&f["path"]);
                let by_cause = e["fields_by_cause"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter_map(|(cause, paths)| {
                                paths[path.as_str()]
                                    .as_u64()
                                    .map(|n| format!("{cause} {n} of {}", e["causes"][cause]))
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let read = f["read_as"].as_str().unwrap_or("");
                let forwarded = if read.is_empty() {
                    String::new()
                } else {
                    e["forwarded"][read].as_u64().unwrap_or(0).to_string()
                };
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} of {} | {} | {} | {} |",
                    path, shapes, f["present"], e["count"], by_cause, read, forwarded
                );
            }
            out.push('\n');
        } else if let Some(m) = e["forwarded"].as_object().filter(|m| !m.is_empty()) {
            let list: Vec<String> = m.iter().map(|(k, v)| format!("{k} {v}")).collect();
            let _ = writeln!(out, "Forwarded fields: {}.\n", list.join(", "));
        }
        for (path, values) in e["values"].as_object().into_iter().flatten() {
            let list: Vec<String> = values
                .as_object()
                .into_iter()
                .flatten()
                .map(|(v, n)| format!("{v}: {n}"))
                .collect();
            let by_cause: Vec<String> = e["values_by_cause"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(cause, paths)| {
                    let vs = paths[path.as_str()].as_object()?;
                    let l: Vec<String> = vs.iter().map(|(v, n)| format!("{v}: {n}")).collect();
                    Some(format!("{cause} {}", l.join(", ")))
                })
                .collect();
            let _ = writeln!(
                out,
                "Values of `{path}`: {}. By cause: {}.\n",
                list.join(", "),
                by_cause.join("; ")
            );
        }
        if let Some(skew) = e["time_skew_by_cause"]
            .as_object()
            .filter(|m| !m.is_empty())
        {
            let list: Vec<String> = skew
                .iter()
                .map(|(c, v)| format!("{c} {:.3} s", v.as_f64().unwrap_or(f64::NAN)))
                .collect();
            let _ = writeln!(
                out,
                "Largest distance between `time` and model time on arrival: {}.\n",
                list.join(", ")
            );
        }
        for (key, label) in [
            ("allowed_unseen", "Allow-listed, never seen"),
            ("seen_unread", "Seen, read by no allow-list"),
            ("forwarded_unexpected", "Forwarded, not allow-listed"),
            (
                "forwarded_over_present",
                "Forwarded more often than present",
            ),
            (
                "forwarded_under_present",
                "Present more often than forwarded",
            ),
        ] {
            let list: Vec<String> = e[key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| format!("`{}`", text(v)))
                .collect();
            if !list.is_empty() {
                let _ = writeln!(out, "{label}: {}.\n", list.join(", "));
            }
        }
    }
    let unreached: Vec<String> = c["unreached"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    let facts: Vec<String> = s["facts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    if !facts.is_empty() {
        let _ = writeln!(out, "## Facts the scenarios observed\n");
        for f in &facts {
            let _ = writeln!(out, "- {f}");
        }
        out.push('\n');
    }
    let _ = writeln!(out, "## Not reached\n\n{}.", unreached.join(", "));
    for u in r["undecodable"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "\nDid not decode: {}", text(u));
    }
    out
}

/// `catalog --build`.
fn build_files(
    repository: &Path,
    probes: &Path,
    recordings: &[PathBuf],
    out: &Path,
) -> Result<ExitCode, String> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let spec = Spec::parse(&read(&repository.join("spec/records.md"))?)?;
    let names = event_names(&read(&repository.join("lua/mock/api/server.txt"))?);
    let report: Value =
        serde_json::from_str(&read(probes)?).map_err(|e| format!("{}: {e}", probes.display()))?;
    let texts = recordings
        .iter()
        .map(|p| read(p))
        .collect::<Result<Vec<_>, _>>()?;
    let (catalog, ok) = build(&report, &texts, &spec, &names);
    let stem = out.display().to_string();
    let json_path = PathBuf::from(format!("{stem}.json"));
    let md_path = PathBuf::from(format!("{stem}.md"));
    let pretty = serde_json::to_string_pretty(&catalog).map_err(|e| e.to_string())?;
    std::fs::write(&json_path, pretty + "\n")
        .map_err(|e| format!("{}: {e}", json_path.display()))?;
    std::fs::write(&md_path, markdown(&catalog))
        .map_err(|e| format!("{}: {e}", md_path.display()))?;
    println!("wrote {} and {}", md_path.display(), json_path.display());
    if !ok {
        println!(
            "the catalog is not whole: a scenario failed, none reported, or an EVENT did not decode; see the report"
        );
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// `catalog <name>`: the live run.
fn live_run(repository: &Path, name: &str) -> Result<ExitCode, String> {
    let target = crate::target(name)?;
    if !matches!(target, Target::DockerWine { .. }) {
        return Err("catalog runs on a docker-wine target".into());
    }
    let template = std::fs::read(repository.join("tests/fixtures/templates/caucasus.miz"))
        .map_err(|e| format!("tests/fixtures/templates/caucasus.miz: {e}"))?;
    let shell = miz::generate(&template, &miz::Changes::default())?;
    let (remote, _, _) = crate::remote(name)?;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }
    let probe = link::build_linux_bins(&[("nanoprobe-fake-agent", "link-probe")])?
        .pop()
        .ok_or("link-probe was not built")?;

    println!("== deploy");
    crate::deploy(name, None, true, false)?;
    println!("== restart");
    crate::restart(name)?;
    println!("== load a fresh shell mission");
    live::load(&remote, "np-shell.miz", &shell)?;

    let folder = bundle::folder().join(format!("catalog-{name}-{}", bundle::stamp()));
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let toml = remote
        .read(crate::install::AGENT_TOML)?
        .ok_or("no agent.toml on the target")?;
    let config = nanoprobe_agent_link::config::AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    let map = link::linux_path(&config.transport.map_path)?;
    let token = remote.full_path(crate::install::TOKEN);
    let port = config.transport.port.to_string();

    let result = (|| -> Result<(ExitCode, PathBuf), String> {
        // A run that was interrupted leaves its probe attached and its
        // recording behind; neither may enter this run.
        remote.shell(
            &format!("pkill -f '[l]ink-probe --map' || true; rm -rf {DIR}"),
            &[],
            b"",
        )?;
        link::upload_container(&remote, DIR, &probe, "link-probe")?;
        let start = remote.shell(
            &format!(
                "nohup {DIR}/link-probe --map \"$1\" --port \"$2\" --token-file \"$3\" --seconds 86400 \
                 --record {DIR}/recording.jsonl --stop-file {DIR}/stop > {DIR}/probe.jsonl 2> {DIR}/probe.err < /dev/null &"
            ),
            &[&map, &port, &token],
            b"",
        )?;
        if start.code != Some(0) {
            return Err(format!("could not start the probe: {}", start.stderr));
        }
        let deadline = Instant::now() + Duration::from_secs(ATTACH_S);
        loop {
            let lines = remote.shell(
                &format!("cat {DIR}/probe.jsonl 2>/dev/null || true"),
                &[],
                b"",
            )?;
            if link::parse_lines(&lines.stdout)
                .iter()
                .any(|l| l["ev"] == "attached")
            {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("the probe did not attach within {ATTACH_S} s"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("== scenarios");
        let options = probes::ProbeOptions {
            dir: Some("lua/catalog".into()),
            ..Default::default()
        };
        let facts = crate::facts(&target)?;
        empty(&remote, "scenarios")?;
        let (code, report) = probes::run_probes_to(&remote, &facts, name, &options)?;
        println!("== load the shell again, for the mission's end and start");
        empty(&remote, "reload")?;
        live::load(&remote, "np-shell.miz", &shell)?;
        std::thread::sleep(Duration::from_secs(SETTLE_S));
        Ok((code, report))
    })();

    // Stop the probe through its stop file, so it writes its final status
    // line and flushes the recording; kill it only if it does not end.
    let _ = remote.shell(&format!("touch {DIR}/stop"), &[], b"");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let ended = remote
            .shell(
                &format!("cat {DIR}/probe.jsonl 2>/dev/null || true"),
                &[],
                b"",
            )
            .map(|o| {
                link::parse_lines(&o.stdout)
                    .iter()
                    .any(|l| l["ev"] == "end")
            })
            .unwrap_or(false);
        if ended {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = remote.shell("pkill -f '[l]ink-probe --map' || true", &[], b"");
    std::thread::sleep(Duration::from_secs(1));
    for file in ["recording.jsonl", "probe.jsonl", "probe.err"] {
        if let Ok(o) = remote.shell(&format!("cat {DIR}/{file} 2>/dev/null || true"), &[], b"") {
            let path = folder.join(file);
            std::fs::write(&path, &o.stdout).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let _ = remote.shell(&format!("rm -rf {DIR}"), &[], b"");
    let (probes_code, report) = result?;
    let copied = folder.join("probes.json");
    // The probes report, with the agent's metrics from the probe's final
    // status line.
    let mut probes_json: Value = serde_json::from_str(
        &std::fs::read_to_string(&report).map_err(|e| format!("{}: {e}", report.display()))?,
    )
    .map_err(|e| format!("{}: {e}", report.display()))?;
    let probe_out = std::fs::read(folder.join("probe.jsonl")).unwrap_or_default();
    if let Some(last) = link::parse_lines(&probe_out)
        .into_iter()
        .rfind(|l| l["ev"] == "final")
    {
        probes_json["agent_metrics"] = last;
    }
    std::fs::write(
        &copied,
        serde_json::to_string_pretty(&probes_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", copied.display()))?;
    let date = bundle::utc_now().get(..10).unwrap_or("undated").to_owned();
    let built = build_files(
        repository,
        &copied,
        &[folder.join("recording.jsonl")],
        &folder.join(format!("{date}-event-catalog")),
    )?;
    if probes_code != ExitCode::SUCCESS {
        println!(
            "the scenario run failed, or its clean-up left something behind; see {}",
            copied.display()
        );
        return Ok(ExitCode::FAILURE);
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repository")
            .to_path_buf()
    }

    /// Whether `path` is a committed event catalog report with extension
    /// `ext`. The reports committed on 2026-11-13 keep their file names,
    /// `*event-catalogue.*`; later ones are `*event-catalog.*`.
    fn is_catalog(path: &Path, ext: &str) -> bool {
        let name = path.to_string_lossy();
        name.ends_with(&format!("event-catalog.{ext}"))
            || name.ends_with(&format!("event-catalogue.{ext}"))
    }

    fn spec() -> Spec {
        Spec::parse(&std::fs::read_to_string(repo().join("spec/records.md")).expect("spec"))
            .expect("parses")
    }

    fn names() -> BTreeMap<u32, String> {
        event_names(&std::fs::read_to_string(repo().join("lua/mock/api/server.txt")).expect("dump"))
    }

    #[test]
    fn the_spec_tables_are_read() {
        let s = spec();
        assert_eq!(s.fields["INITIATOR"], (2, "initiator.id_".into()));
        assert_eq!(s.fields["POS_X"], (10, "pos.x".into()));
        assert_eq!(
            s.allow["S_EVENT_HIT"],
            [
                "TIME",
                "INITIATOR",
                "INITIATOR_CATEGORY",
                "TARGET",
                "TARGET_CATEGORY",
                "WEAPON",
                "WEAPON_NAME"
            ]
        );
        let names = names();
        assert_eq!(names.get(&15).map(String::as_str), Some("S_EVENT_BIRTH"));
        for event in s.allow.keys() {
            assert!(
                names.values().any(|n| n == event),
                "{event} is no world.event"
            );
        }
    }

    /// A probes report as `probes` writes it, with one `catalog`
    /// observation, and a recording with an EVENT, a FRAME and a bad line.
    fn sample() -> (Value, String) {
        let report = json!({
            "target": "t", "kind": "docker-wine", "dcs": "2.9", "wine": "w", "time": "now",
            "results": [{
                "probe": "10-ground",
                "result": { "reply": { "observations": [
                    { "name": "other", "value": 1 },
                    { "name": "catalog", "value": { "total": 3, "by": {
                        "spawn": { "S_EVENT_BIRTH": { "id": 15, "count": 2, "fields": {
                            "id": { "number": 2 },
                            "time": { "number": 2 },
                            "initiator": { "object:Unit": 2 },
                            "initiator.id_": { "number": 2 },
                            "initiator_data": { "table": 2 },
                            "initiator_data.unit_name": { "string": 2 },
                        }}},
                        "explosion": { "S_EVENT_DEAD": { "id": 8, "count": 1, "fields": {
                            "id": { "number": 1 },
                        }}},
                    }}},
                ]}},
            }],
        });
        let event = Event {
            id: 15,
            fields: vec![
                (1, Lv::Number(1.0)),
                (2, Lv::Number(7.0)),
                (77, Lv::Bool(true)),
            ],
        };
        let hex: String = event.encode().iter().map(|b| format!("{b:02x}")).collect();
        let recording = format!(
            "{{\"t\":1,\"boot\":\"00\",\"number\":1,\"kind\":1,\"payload\":\"00\"}}\n\
             {{\"t\":1,\"boot\":\"00\",\"number\":2,\"kind\":20,\"payload\":\"{hex}\"}}\n"
        );
        (report, recording)
    }

    #[test]
    fn a_catalog_names_presence_reads_and_gaps() {
        let (report, recording) = sample();
        let (c, ok) = build(&report, &[recording], &spec(), &names());
        assert!(!ok, "code 77 is forwarded and allow-listed nowhere");
        let birth = c["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "S_EVENT_BIRTH")
            .unwrap();
        assert_eq!(birth["count"], 2);
        assert_eq!(birth["records"], 1);
        assert_eq!(birth["forwarded"]["INITIATOR"], 1);
        assert_eq!(birth["forwarded"]["code 77"], 1);
        let read: Vec<_> = birth["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["read_as"].as_str())
            .collect();
        assert_eq!(read, ["INITIATOR", "TIME"]);
        assert_eq!(
            birth["allowed_unseen"],
            json!(["INITIATOR_CATEGORY", "PLACE", "SUBPLACE"])
        );
        assert_eq!(
            birth["seen_unread"],
            json!(["id", "initiator_data.unit_name"])
        );
        let unreached = c["unreached"].as_array().unwrap();
        assert!(unreached.iter().any(|n| n == "S_EVENT_SHOT"));
        assert!(!unreached.iter().any(|n| n == "S_EVENT_DEAD"));
        let md = markdown(&c);
        assert!(md.contains("## S_EVENT_BIRTH (15)"), "{md}");
        assert!(
            md.contains("| `initiator.id_` | number 2 | 2 of 2 | spawn 2 of 2 | INITIATOR | 1 |"),
            "{md}"
        );
        assert!(
            md.contains("Allow-listed, never seen: `INITIATOR_CATEGORY`, `PLACE`, `SUBPLACE`."),
            "{md}"
        );
        assert!(
            md.contains("Forwarded, not allow-listed: `code 77`."),
            "{md}"
        );
        assert_eq!(birth["forwarded_unexpected"], json!(["code 77"]));
        assert!(
            c["source"].get("target").is_none(),
            "the profile name leaked"
        );
    }

    #[test]
    fn a_failed_scenario_or_none_reporting_makes_the_catalog_not_whole() {
        let (mut report, recording) = sample();
        report["results"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "probe": "30-air-defense", "ok": false, "error": "timed out" }));
        let (c, ok) = build(&report, std::slice::from_ref(&recording), &spec(), &names());
        assert!(!ok);
        assert_eq!(c["source"]["failed"], json!(["30-air-defense: timed out"]));
        assert!(markdown(&c).contains("30-air-defense: timed out"));
        let empty = json!({ "results": [] });
        let (_, ok) = build(&empty, &[recording], &spec(), &names());
        assert!(!ok, "no scenario reported");
    }

    #[test]
    fn a_record_read_twice_counts_once_and_the_run_names_its_own_ids() {
        let (mut report, recording) = sample();
        // The run's world.event calls ID 15 by another name.
        report["results"][0]["result"]["reply"]["observations"][1]["value"]["by"]["spawn"] =
            json!({ "S_EVENT_RENAMED": { "id": 15, "count": 1, "fields": {} } });
        let (c, _) = build(&report, &[recording.clone(), recording], &spec(), &names());
        assert_eq!(c["recorded"]["duplicates"], 2);
        assert_eq!(c["recorded"]["events"], 1);
        let renamed = c["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "S_EVENT_RENAMED")
            .unwrap();
        assert_eq!(renamed["records"], 1);
        assert_eq!(c["source"]["name_conflicts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_type_seen_only_in_the_stream_says_so() {
        let (report, _) = sample();
        let start = Event {
            id: 11,
            fields: vec![(1, Lv::Number(0.0))],
        };
        let hex: String = start.encode().iter().map(|b| format!("{b:02x}")).collect();
        let rec = format!("{{\"boot\":\"00\",\"number\":9,\"kind\":20,\"payload\":\"{hex}\"}}\n");
        let (c, _) = build(&report, &[rec], &spec(), &names());
        let e = c["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "S_EVENT_MISSION_START")
            .unwrap();
        assert_eq!(e["stream_only"], true);
        assert!(markdown(&c).contains("Seen only in the stream"));
    }

    #[test]
    fn an_event_that_does_not_decode_fails_the_build() {
        let (report, _) = sample();
        let bad = "{\"boot\":\"00\",\"number\":1,\"kind\":20,\"payload\":\"0f000000ffffffff\"}\nnot json\n";
        let (c, ok) = build(&report, &[bad.into()], &spec(), &names());
        assert!(!ok);
        assert_eq!(c["recorded"]["undecodable"].as_array().unwrap().len(), 2);
    }

    /// Every shape a catalog in docs/results recorded, and every shape the
    /// allow-lists allow, written as the agent writes it and as no agent
    /// would, decodes and reads back.
    #[test]
    fn every_cataloged_shape_decodes() {
        let s = spec();
        let names = names();
        let ids: BTreeMap<&str, u32> = names.iter().map(|(i, n)| (n.as_str(), *i)).collect();
        // Shape lists per event: from the allow-lists, then from every
        // catalog committed under docs/results.
        let mut shapes: Vec<(u32, Vec<(u16, String)>)> = Vec::new();
        for (event, fields) in &s.allow {
            let id = ids[event.as_str()];
            let list = fields
                .iter()
                .map(|f| (s.fields[f].0, "number".to_owned()))
                .collect();
            shapes.push((id, list));
        }
        let results = repo().join("docs/results");
        let mut catalogs = 0;
        for entry in std::fs::read_dir(&results).expect("docs/results").flatten() {
            let path = entry.path();
            if !is_catalog(&path, "json") {
                continue;
            }
            catalogs += 1;
            let c: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
            for e in c["events"].as_array().into_iter().flatten() {
                let id = e["id"].as_u64().unwrap_or(0) as u32;
                let mut list = Vec::new();
                for f in e["fields"].as_array().into_iter().flatten() {
                    let code = f["read_as"]
                        .as_str()
                        .and_then(|n| s.fields.get(n))
                        .map_or(999, |(c, _)| *c);
                    for shape in f["shapes"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(k, _)| k)
                    {
                        list.push((code, shape.clone()));
                    }
                }
                shapes.push((id, list));
            }
        }
        // A committed report without its catalog would be skipped silently.
        let reports = std::fs::read_dir(&results)
            .expect("docs/results")
            .flatten()
            .filter(|e| is_catalog(&e.path(), "md"))
            .count();
        assert_eq!(
            catalogs, reports,
            "each committed catalog report has its JSON"
        );
        println!("{catalogs} committed catalog(s) read");
        let values = |shape: &str| -> Vec<Lv> {
            let mut v = vec![
                Lv::Nil,
                Lv::Bool(true),
                Lv::Number(f64::NAN),
                Lv::Number(-0.0),
                Lv::String(vec![0xff, 0xfe]),
                Lv::Other("userdata".into()),
                Lv::Table(vec![(Lv::Number(1.0), Lv::Nil)]),
            ];
            v.push(match shape {
                "number" => Lv::Number(16_777_472.0),
                "string" => Lv::String(b"np".to_vec()),
                "boolean" => Lv::Bool(false),
                other => Lv::Other(other.into()),
            });
            v
        };
        for (id, list) in &shapes {
            for (i, (code, shape)) in list.iter().enumerate() {
                for value in values(shape) {
                    let mut fields: Vec<(u16, Lv)> =
                        list.iter().map(|(c, _)| (*c, Lv::Number(1.0))).collect();
                    fields[i] = (*code, value);
                    let e = Event { id: *id, fields };
                    let bytes = e.encode();
                    let d = Event::decode(&bytes).expect("decodes");
                    assert_eq!(d.fields.len(), e.fields.len());
                    let _ = (d.object(u64::from(*code)), d.text(u64::from(*code)));
                    let cut = Event::decode(&d.cut().encode()).expect("the cut decodes");
                    assert_eq!(cut.fields.len(), e.fields.len());
                    // Every prefix fails cleanly, never panics.
                    for n in 0..bytes.len() {
                        let _ = Event::decode(&bytes[..n]);
                    }
                }
            }
        }
    }

    /// Each committed catalog backs the spec's Evidence column: a LIVE row
    /// names a type the recorder saw, a "stream only" row a type with agent
    /// records, and every field path but `ROUNDS`'s was present somewhere.
    #[test]
    fn the_evidence_column_matches_the_committed_catalogs() {
        let s = spec();
        let results = repo().join("docs/results");
        let mut catalogs: Vec<(PathBuf, Value)> = Vec::new();
        for entry in std::fs::read_dir(&results).expect("docs/results").flatten() {
            let path = entry.path();
            if is_catalog(&path, "json") {
                let c = serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
                    .expect("json");
                catalogs.push((path, c));
            }
        }
        // spec/records.md cites two committed catalogs.
        assert!(
            !catalogs.is_empty(),
            "no committed event catalog in docs/results"
        );
        // Each event type's entries, across every committed catalog.
        let mut by_type: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        for (_, c) in &catalogs {
            for e in c["events"].as_array().into_iter().flatten() {
                if let Some(n) = e["name"].as_str() {
                    by_type.entry(n.to_owned()).or_default().push(e);
                }
            }
        }
        let none = Vec::new();
        for (event, evidence) in &s.evidence {
            let entries = by_type.get(event).unwrap_or(&none);
            let seen: Vec<&&Value> = entries
                .iter()
                .filter(|e| e["count"].as_u64() > Some(0))
                .collect();
            if evidence.contains("stream only") {
                assert!(
                    entries.iter().any(|e| e["records"].as_u64() > Some(0)),
                    "{event}: no agent record in any committed catalog"
                );
            } else if evidence.starts_with("LIVE") {
                assert!(!seen.is_empty(), "{event}: no committed catalog saw it");
                // A listed field no catalog saw for the type must be named
                // in the cell, after "not seen:".
                let excused: Vec<&str> = evidence
                    .split_once("not seen:")
                    .map(|(_, l)| l.split(',').map(str::trim).collect())
                    .unwrap_or_default();
                for f in seen[0]["allowed_unseen"].as_array().into_iter().flatten() {
                    let f = f.as_str().unwrap_or("");
                    let unseen_everywhere = seen.iter().all(|e| {
                        e["allowed_unseen"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x == f))
                    });
                    assert!(
                        !unseen_everywhere || excused.contains(&f),
                        "{event}: {f} is listed as LIVE but no catalog saw it"
                    );
                }
            } else if evidence.starts_with("UNVERIFIED") || evidence.starts_with("PRIOR") {
                assert!(
                    seen.is_empty(),
                    "{event}: tagged {evidence} but a committed catalog saw it"
                );
            } else {
                panic!("{event}: the Evidence cell {evidence:?} is not LIVE, PRIOR or UNVERIFIED");
            }
        }
        let paths: BTreeSet<&str> = by_type
            .values()
            .flatten()
            .flat_map(|e| e["fields"].as_array().into_iter().flatten())
            .filter_map(|f| f["path"].as_str())
            .collect();
        for (name, (_, p)) in &s.fields {
            if name != "ROUNDS" {
                assert!(paths.contains(p.as_str()), "{name} ({p}) present nowhere");
            }
        }
        for (_, c) in &catalogs {
            let events: BTreeMap<&str, &Value> = c["events"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| Some((e["name"].as_str()?, e)))
                .collect();
            // Every object carried its ID as its own `id_` number: each
            // `X.id_` is a number whenever it is present, and at least as
            // often as `X` was an object (a dead killer's initiator is a
            // plain table with an `id_` too).
            for e in events.values() {
                let fields: BTreeMap<&str, &Value> = e["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| Some((f["path"].as_str()?, f)))
                    .collect();
                for (p, f) in &fields {
                    let objects: u64 = f["shapes"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(k, _)| k.starts_with("object"))
                        .filter_map(|(_, n)| n.as_u64())
                        .sum();
                    if objects == 0 {
                        continue;
                    }
                    let id = fields.get(format!("{p}.id_").as_str());
                    let numbers = id.and_then(|i| i["shapes"]["number"].as_u64()).unwrap_or(0);
                    let present = id.and_then(|i| i["present"].as_u64()).unwrap_or(0);
                    assert!(
                        numbers >= objects && numbers == present,
                        "{}: {p} was an object {objects} times, {p}.id_ a number {numbers} of {present}",
                        e["name"]
                    );
                }
            }
        }
    }

    #[test]
    fn presence_is_kept_per_cause() {
        let report = json!({ "results": [{ "probe": "p", "result": { "reply": { "observations": [
            { "name": "catalog", "value": { "by": {
                "unit": { "S_EVENT_DEAD": { "id": 8, "count": 2, "fields": {
                    "initiator_data.unit_name": { "string": 2 }, "time": { "number": 2 } },
                    "values": { "initiator_data.object_category": { "1": 2 } } } },
                                "scenery": { "S_EVENT_DEAD": { "id": 8, "count": 3, "fields": {
                    "time": { "number": 3 } },
                    "values": { "initiator_data.object_category": { "5": 3 } },
                    "skew": 0.25 } },
            } } },
        ] } } }] });
        let (c, ok) = build(&report, &[], &spec(), &names());
        assert!(ok);
        let dead = &c["events"][0];
        assert_eq!(
            dead["fields_by_cause"]["unit"]["initiator_data.unit_name"],
            2
        );
        assert!(
            dead["fields_by_cause"]["scenery"]
                .get("initiator_data.unit_name")
                .is_none()
        );
        assert_eq!(dead["values"]["initiator_data.object_category"]["5"], 3);
        assert_eq!(dead["time_skew_by_cause"]["scenery"], 0.25);
        let md = markdown(&c);
        assert!(
            md.contains("| `time` | number 5 | 5 of 5 | scenery 3 of 3, unit 2 of 2 |"),
            "{md}"
        );
        assert!(
            md.contains(
                "Values of `initiator_data.object_category`: 1: 2, 5: 3. By cause: scenery 5: 3; unit 1: 2."
            ),
            "{md}"
        );
        assert_eq!(
            dead["values_by_cause"]["unit"]["initiator_data.object_category"]["1"],
            2
        );
        assert!(
            md.contains(
                "Largest distance between `time` and model time on arrival: scenery 0.250 s."
            ),
            "{md}"
        );
    }

    #[test]
    fn a_recording_with_no_event_or_a_type_never_forwarded_is_not_whole() {
        let (report, recording) = sample();
        let frames_only = recording.lines().next().unwrap().to_owned() + "\n";
        let (c, ok) = build(&report, &[frames_only], &spec(), &names());
        assert!(!ok);
        let why = c["source"]["unforwarded"].as_array().unwrap();
        assert!(
            why.iter()
                .any(|w| w == "the recording holds no EVENT record")
        );
        assert!(
            why.iter()
                .any(|w| w == "S_EVENT_DEAD: seen, never forwarded")
        );
    }

    #[test]
    fn a_cut_off_last_line_is_counted_not_failed() {
        let (report, recording) = sample();
        let cut = format!("{recording}{{\"t\":1,\"boot\":\"00\",\"num");
        let (c, _) = build(&report, &[cut], &spec(), &names());
        assert_eq!(c["recorded"]["cut_off"], 1);
        assert_eq!(c["recorded"]["undecodable"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn quantiles_read_bucket_upper_bounds() {
        let mut h = vec![0u64; 32];
        h[3] = 90;
        h[10] = 10;
        assert_eq!(quantile_us(&h, 0.5), Some(8));
        assert_eq!(quantile_us(&h, 0.99), Some(1024));
        assert_eq!(quantile_us(&[0; 32], 0.5), None);
    }

    #[test]
    fn a_read_field_present_more_often_than_forwarded_fails_the_build() {
        let report = json!({ "results": [{ "probe": "p", "result": { "reply": { "observations": [
            { "name": "catalog", "value": { "by": { "c": { "S_EVENT_DEAD": {
                "id": 8, "count": 1, "fields": {
                    "time": { "number": 1 },
                    "initiator": { "object:Unit": 1 },
                    "initiator.id_": { "number": 1 } } } } } } },
        ] } } }] });
        let event = Event {
            id: 8,
            fields: vec![(1, Lv::Number(1.0))],
        };
        let hex: String = event.encode().iter().map(|b| format!("{b:02x}")).collect();
        let rec = format!("{{\"boot\":\"00\",\"number\":1,\"kind\":20,\"payload\":\"{hex}\"}}\n");
        let (c, ok) = build(&report, &[rec], &spec(), &names());
        assert!(!ok);
        assert_eq!(
            c["events"][0]["forwarded_under_present"],
            json!(["INITIATOR"])
        );
        assert!(markdown(&c).contains("Present more often than forwarded: `INITIATOR`."));
    }
}
