//! `routes`: experiment E37, DCS's own cost of moving ground units by route
//! shape, on a docker-wine target.
//!
//! `routes <name> [--units 400] [--sweeps 1] [--shapes a,b] ...` deploys a
//! development build, restarts DCS, loads a fresh shell mission and runs a
//! sampler in the game container. For each shape of
//! `lua/scenarios/routes.lua` it adds the same number of ground units laid
//! out in that shape, lets DCS settle for `--settle` seconds (20), counts
//! frames for `--measure` seconds (45) with `lua/sampling/frames.lua`, and
//! removes everything. Per shape it records:
//!
//! - mean fps: the frames over the real time the window spanned;
//! - p5 fps: the 5th percentile of the frames in each whole second of the
//!   window, the worst sustained rate (rank `floor(0.05 n + 0.5)`, as
//!   `lua/sampling/fps.lua`);
//! - model/real: model seconds over real seconds across the window;
//! - DCS CPU, in percent of one core: the DCS process's user and system time
//!   from `/proc`, sampled once a second in the container by the sampler
//!   `meter.rs` holds, over the samples inside the window `dcs.log` stamps
//!   (the sampler's own clock; the log's stamps are put on it with the
//!   container's offset from UTC);
//! - the share of the scenario's ground groups that moved while it ran
//!   (`lua/scenarios/movers.lua`), which says whether the shape moved.
//!
//! It writes `routes.json`, `routes.txt` (the table) and `samples.txt` (the
//! sampler's whole file) under `bundles/routes-<name>-<time>/`, a folder the
//! repository ignores. It leaves the target running the development build
//! with the shell mission loaded: the caller restores the target. It stops
//! when a player joins.

use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::meter::{self, LogWindow, Parsed, Sample, Sampler};
use crate::sampling::live::{self, Run};
use crate::{bundle, soak};

const USAGE: &str = "usage: cargo xtask routes <name> [--units 400] [--sweeps 1] \
[--shapes a,b,...] [--settle 20] [--measure 45] [--length 4000] [--step 50] [--speed 8] \
[--kind M-113] [--lane 100] [--lanes-per-block 60] [--formations column=Cone,line=Rank,...]";

/// Where the sampler runs in the container.
const SAMPLER_DIR: &str = "/tmp/nanoprobe-routes";

/// The shapes `lua/scenarios/routes.lua` lays out, in the order they run.
pub(crate) const SHAPES: &[&str] = &[
    "halted",
    "offroad-few",
    "offroad-dense",
    "onroad",
    "formation-off",
    "formation-line",
    "formation-column",
    "formation-diamond",
    "size-1",
    "size-4",
    "size-10",
    "held-order",
    "held-zero",
];

/// What a run was asked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Options {
    pub units: usize,
    pub sweeps: usize,
    pub shapes: Vec<String>,
    pub settle: u32,
    pub measure: u32,
    pub length: f64,
    pub step: f64,
    pub speed: f64,
    pub kind: String,
    pub lane: f64,
    pub lanes_per_block: u32,
    pub formations: Vec<(String, String)>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            units: 400,
            sweeps: 1,
            shapes: SHAPES.iter().map(|s| (*s).to_owned()).collect(),
            settle: 20,
            measure: 45,
            length: 4000.0,
            step: 50.0,
            speed: 8.0,
            kind: "M-113".to_owned(),
            lane: 100.0,
            lanes_per_block: 60,
            formations: Vec::new(),
        }
    }
}

/// A word that can go inside a Lua string without escaping: letters,
/// digits, space, and `_ . -`.
fn plain(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" _.-".contains(&b))
}

pub(crate) fn parse_options(args: &[&str]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut rest = args;
    while let [flag, value, tail @ ..] = rest {
        let number = |what: &str| -> Result<f64, String> {
            value
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n > 0.0)
                .ok_or_else(|| format!("{what}: {value:?} is not a positive number\n{USAGE}"))
        };
        match *flag {
            "--units" => {
                let n = number("--units")? as usize;
                if n < 40 {
                    return Err("--units must be at least 40 (four groups of ten)".into());
                }
                o.units = n;
            }
            "--sweeps" => {
                let n = number("--sweeps")? as usize;
                if !(1..=10).contains(&n) {
                    return Err("--sweeps must be from 1 to 10".into());
                }
                o.sweeps = n;
            }
            "--shapes" => {
                let mut shapes = Vec::new();
                for shape in value.split(',') {
                    if !SHAPES.contains(&shape) {
                        return Err(format!(
                            "unknown shape {shape:?}; the shapes are {}",
                            SHAPES.join(", ")
                        ));
                    }
                    shapes.push(shape.to_owned());
                }
                o.shapes = shapes;
            }
            "--settle" => o.settle = number("--settle")? as u32,
            "--measure" => {
                let n = number("--measure")? as u32;
                if n < 10 {
                    return Err(
                        "--measure must be at least 10 seconds (the p5 needs windows)".into(),
                    );
                }
                o.measure = n;
            }
            "--length" => o.length = number("--length")?,
            "--step" => o.step = number("--step")?,
            "--speed" => o.speed = number("--speed")?,
            "--lane" => o.lane = number("--lane")?,
            "--lanes-per-block" => o.lanes_per_block = number("--lanes-per-block")? as u32,
            "--kind" => {
                if !plain(value) {
                    return Err(format!(
                        "--kind: {value:?} has a character a type name does not"
                    ));
                }
                o.kind = (*value).to_owned();
            }
            "--formations" => {
                let mut list = Vec::new();
                for pair in value.split(',') {
                    let (name, action) = pair
                        .split_once('=')
                        .ok_or_else(|| format!("--formations: {pair:?} is not name=Action"))?;
                    if !["off", "line", "column", "diamond"].contains(&name) || !plain(action) {
                        return Err(format!(
                            "--formations: {pair:?}; the names are off, line, column and diamond"
                        ));
                    }
                    list.push((name.to_owned(), action.to_owned()));
                }
                o.formations = list;
            }
            _ => return Err(USAGE.into()),
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(USAGE.into());
    }
    if o.step > o.length {
        return Err("--step must not be longer than --length".into());
    }
    Ok(o)
}

/// The Lua that adds one shape's units and returns its summary.
pub(crate) fn spawn_source(shape: &str, o: &Options) -> String {
    let formations = o
        .formations
        .iter()
        .map(|(k, v)| format!("{k} = '{v}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "local E = ...\n\
         local R = E.lib('routes')\n\
         return R.spawn('{shape}', {}, {{ label = 'r', kind = '{}', speed = {}, length = {}, step = {}, \
         lane_m = {}, lanes_per_block = {}, formations = {{ {formations} }} }})\n",
        o.units, o.kind, o.speed, o.length, o.step, o.lane, o.lanes_per_block
    )
}

const CLEAR_SOURCE: &str = "local E = ...\nreturn E.lib('routes').clear()\n";

/// The 5th percentile of per-second frame counts: the value at rank
/// `max(1, floor(0.05 n + 0.5))` of the sorted list, as
/// `lua/sampling/fps.lua` takes it.
pub(crate) fn percentile5(per_second: &[f64]) -> Option<f64> {
    if per_second.is_empty() {
        return None;
    }
    let mut sorted = per_second.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let rank = ((sorted.len() as f64 * 0.05 + 0.5).floor() as usize).max(1);
    sorted.get(rank - 1).copied()
}

/// What the frame meter's reply says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameStats {
    pub frames: u64,
    pub span_s: f64,
    pub mean_fps: f64,
    pub p5_fps: Option<f64>,
    pub model_per_real: f64,
    pub longest_ms: f64,
    pub paused: u64,
}

/// Read the reply of `lua/sampling/frames.lua`: `frames`, `buckets` (the
/// frames in each whole second), `longest`, `paused`, and the first and last
/// frame's real and model time.
pub(crate) fn frame_stats(v: &Value) -> Result<FrameStats, String> {
    let num = |key: &str| {
        v[key]
            .as_f64()
            .ok_or_else(|| format!("the frame meter's reply has no {key}: {v}"))
    };
    let frames = v["frames"]
        .as_u64()
        .ok_or_else(|| format!("the frame meter's reply has no frames: {v}"))?;
    let span = num("last_real")? - num("first_real")?;
    if span <= 0.0 {
        return Err(format!("the frame meter's window has no length: {v}"));
    }
    let buckets: Vec<f64> = v["buckets"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    Ok(FrameStats {
        frames,
        span_s: span,
        mean_fps: frames as f64 / span,
        p5_fps: percentile5(&buckets),
        model_per_real: (num("last_model")? - num("first_model")?) / span,
        longest_ms: v["longest"].as_f64().unwrap_or(0.0) * 1000.0,
        paused: v["paused"].as_u64().unwrap_or(0),
    })
}

/// The sampler's samples inside a frame meter window. The window's stamps
/// are in the log's time zone read as UTC, so the container's offset is
/// taken off to put them on the sampler's epoch clock. A second of slack on
/// each side keeps the samples that straddle an edge.
pub(crate) fn window_samples(parsed: &Parsed, window: &LogWindow) -> Vec<Sample> {
    let inside = |offset: f64| -> Vec<Sample> {
        let (from, to) = (window.start - offset - 1.0, window.end - offset + 1.0);
        parsed
            .samples
            .iter()
            .filter(|s| s.t >= from && s.t <= to)
            .cloned()
            .collect()
    };
    // DCS under Wine can stamp its log in a zone other than the container's
    // (the Wine prefix keeps its own): the container's offset then puts the
    // window hours away from every sample. Fall back to a log in UTC.
    let found = inside(parsed.utc_offset_s as f64);
    if found.is_empty() && parsed.utc_offset_s != 0 {
        return inside(0.0);
    }
    found
}

/// DCS's CPU over a window, percent of one core, from the samples in it.
/// `None` when fewer than two samples have the process, or the process
/// restarted.
pub(crate) fn window_cpu(parsed: &Parsed, window: &LogWindow) -> Option<f64> {
    let samples = window_samples(parsed, window);
    meter::summarize(&samples, parsed.hz)["process_cpu_percent"].as_f64()
}

/// The figures of one measurement, for the table.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Measure {
    pub shape: String,
    pub groups: u64,
    pub units: u64,
    pub mean_fps: Option<f64>,
    pub p5_fps: Option<f64>,
    pub model_per_real: Option<f64>,
    pub cpu_percent: Option<f64>,
    pub moving_share: Option<f64>,
    pub skipped: Option<String>,
}

/// One row per shape, in the order the shapes first appear, with the
/// figures averaged over the shape's measurements. A figure is averaged over
/// the measurements that have it.
pub(crate) fn aggregate(measures: &[Measure]) -> Vec<(Measure, usize)> {
    let mut order: Vec<&str> = Vec::new();
    for m in measures {
        if !order.contains(&m.shape.as_str()) {
            order.push(&m.shape);
        }
    }
    let mean = |values: Vec<f64>| -> Option<f64> {
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    };
    order
        .into_iter()
        .map(|shape| {
            let of: Vec<&Measure> = measures.iter().filter(|m| m.shape == shape).collect();
            let done: Vec<&&Measure> = of.iter().filter(|m| m.skipped.is_none()).collect();
            let row = Measure {
                shape: shape.to_owned(),
                groups: of.iter().map(|m| m.groups).max().unwrap_or(0),
                units: of.iter().map(|m| m.units).max().unwrap_or(0),
                mean_fps: mean(done.iter().filter_map(|m| m.mean_fps).collect()),
                p5_fps: mean(done.iter().filter_map(|m| m.p5_fps).collect()),
                model_per_real: mean(done.iter().filter_map(|m| m.model_per_real).collect()),
                cpu_percent: mean(done.iter().filter_map(|m| m.cpu_percent).collect()),
                moving_share: mean(done.iter().filter_map(|m| m.moving_share).collect()),
                skipped: of.iter().find_map(|m| m.skipped.clone()),
            };
            (row, done.len())
        })
        .collect()
}

fn cell(value: Option<f64>, digits: usize) -> String {
    value.map_or("-".to_owned(), |v| format!("{v:.digits$}"))
}

/// The table, as text.
pub(crate) fn table(rows: &[(Measure, usize)]) -> String {
    let mut out = format!(
        "{:<18} {:>6} {:>6} {:>8} {:>7} {:>10} {:>8} {:>7} {:>3}\n",
        "shape", "groups", "units", "mean fps", "p5 fps", "model/real", "DCS CPU%", "moving", "n"
    );
    for (m, n) in rows {
        let note = m
            .skipped
            .as_ref()
            .map_or(String::new(), |why| format!("  skipped: {why}"));
        out.push_str(&format!(
            "{:<18} {:>6} {:>6} {:>8} {:>7} {:>10} {:>8} {:>7} {:>3}{note}\n",
            m.shape,
            m.groups,
            m.units,
            cell(m.mean_fps, 1),
            cell(m.p5_fps, 1),
            cell(m.model_per_real, 3),
            cell(m.cpu_percent, 0),
            cell(m.moving_share, 2),
            n
        ));
    }
    out
}

fn measure_of(
    shape: &str,
    spawn: &Value,
    frames: &FrameStats,
    cpu: Option<f64>,
    moving: &Value,
) -> Measure {
    Measure {
        shape: shape.to_owned(),
        groups: spawn["groups"].as_array().map_or(0, |g| g.len() as u64),
        units: spawn["units"].as_u64().unwrap_or(0),
        mean_fps: Some(frames.mean_fps),
        p5_fps: frames.p5_fps,
        model_per_real: Some(frames.model_per_real),
        cpu_percent: cpu,
        moving_share: moving["share"].as_f64(),
        skipped: None,
    }
}

/// What one shape's run left, before its CPU is read from the sampler file.
struct Record {
    shape: String,
    sweep: usize,
    spawn: Value,
    live_units: Option<u64>,
    frames: Option<FrameStats>,
    frames_raw: Value,
    window: Option<LogWindow>,
    moving: Value,
    skipped: Option<String>,
}

/// The run's order: the shapes as given for sweep 0, and for each later
/// sweep rotated by one place, so a drift in the server over the run does
/// not fall on the same shape every sweep.
pub(crate) fn order(shapes: &[String], sweep: usize) -> Vec<String> {
    if shapes.is_empty() {
        return Vec::new();
    }
    let k = sweep % shapes.len();
    shapes[k..].iter().chain(&shapes[..k]).cloned().collect()
}

fn run_shapes(
    run: &Run,
    sampler: &Sampler,
    name: &str,
    shell: &[u8],
    o: &Options,
) -> Result<(Vec<Record>, String), String> {
    run.fresh(name, false, shell)?;
    sampler.start()?;
    run.hold(5.0)?;
    let mut records = Vec::new();
    for sweep in 0..o.sweeps {
        for shape in order(&o.shapes, sweep) {
            let players = run.remote.players()?;
            if players > 0 {
                return Err(format!("{players} player(s) joined; stopped"));
            }
            println!("== {shape} (sweep {})", sweep + 1);
            run.eval("server", "routes-clear", CLEAR_SOURCE, 60)?;
            run.hold(3.0)?;
            let spawn = run.eval("server", "routes-spawn", &spawn_source(&shape, o), 300)?;
            println!("  spawned: {spawn}");
            let mut record = Record {
                shape: shape.clone(),
                sweep,
                spawn: spawn.clone(),
                live_units: None,
                frames: None,
                frames_raw: Value::Null,
                window: None,
                moving: Value::Null,
                skipped: spawn["skipped"].as_str().map(str::to_owned),
            };
            if record.skipped.is_none() {
                record.live_units = run.live_count().ok();
                // The first count only records positions.
                let _ = meter::movers(run);
                let tag = format!("routes-{shape}-{sweep}");
                let raw = meter::frames(run, &tag, o.settle, o.measure)?;
                record.moving = meter::movers(run).unwrap_or(Value::Null);
                record.window = raw.get("log").and_then(window_of);
                record.frames = Some(frame_stats(&raw)?);
                record.frames_raw = raw;
                if let Some(f) = &record.frames {
                    println!(
                        "  mean {:.1} fps, p5 {}, model/real {:.3}",
                        f.mean_fps,
                        cell(f.p5_fps, 1),
                        f.model_per_real
                    );
                }
            } else {
                println!("  skipped: {}", record.skipped.as_deref().unwrap_or(""));
            }
            records.push(record);
        }
    }
    run.eval("server", "routes-clear", CLEAR_SOURCE, 60)?;
    let text = sampler.finish()?;
    Ok((records, text))
}

/// A frame meter window from the `log` object `meter::frames` adds to its
/// reply.
fn window_of(log: &Value) -> Option<LogWindow> {
    Some(LogWindow {
        start: log["start_stamp"].as_f64()?,
        end: log["end_stamp"].as_f64()?,
        real_start: 0.0,
        real_end: log["wall_s"].as_f64()?,
        frames: log["frames"].as_u64()?,
    })
}

/// Build the report from the run's records and the sampler's file.
fn finish(name: &str, o: &Options, records: &[Record], sampler_text: &str) -> (Value, String) {
    let parsed = meter::parse_sampler(sampler_text);
    let mut measures = Vec::new();
    let mut rows = Vec::new();
    for r in records {
        let cpu = r.window.as_ref().and_then(|w| window_cpu(&parsed, w));
        let measure = match (&r.frames, &r.skipped) {
            (Some(f), _) => measure_of(&r.shape, &r.spawn, f, cpu, &r.moving),
            (None, why) => Measure {
                shape: r.shape.clone(),
                skipped: Some(why.clone().unwrap_or_else(|| "not measured".to_owned())),
                ..Measure::default()
            },
        };
        rows.push(json!({
            "shape": r.shape,
            "sweep": r.sweep + 1,
            "spawn": r.spawn,
            "live_units": r.live_units,
            "mean_fps": measure.mean_fps,
            "p5_fps": measure.p5_fps,
            "model_per_real": measure.model_per_real,
            "dcs_cpu_percent": measure.cpu_percent,
            "frames": r.frames.as_ref().map(|f| json!({
                "frames": f.frames, "span_s": f.span_s, "longest_ms": f.longest_ms, "paused": f.paused,
            })),
            "window": r.window.as_ref().map(LogWindow::json),
            "moving": r.moving,
            "skipped": r.skipped,
        }));
        measures.push(measure);
    }
    let table = table(&aggregate(&measures));
    let report = json!({
        "target": name,
        "provenance": soak::provenance(name),
        "started": Value::Null,
        "units": o.units,
        "sweeps": o.sweeps,
        "settle_s": o.settle,
        "measure_s": o.measure,
        "length_m": o.length,
        "step_m": o.step,
        "speed_mps": o.speed,
        "kind": o.kind,
        "lane_m": o.lane,
        "lanes_per_block": o.lanes_per_block,
        "formations": o.formations.iter().map(|(k, v)| json!({ k: v })).collect::<Vec<_>>(),
        "tz_offset_s": parsed.utc_offset_s,
        "records": rows,
    });
    (report, table)
}

pub fn command(repository: &Path, name: &str, args: &[&str]) -> Result<ExitCode, String> {
    let options = parse_options(args)?;
    let (run, shell) = live::open_eval(repository, name, "routes")?;
    println!(
        "== routes: {} units, {} shape(s), {} sweep(s), folder {}",
        options.units,
        options.shapes.len(),
        options.sweeps,
        run.folder.display()
    );
    let started = bundle::utc_now();
    let sampler = Sampler::new(&run.remote, SAMPLER_DIR);
    let result = run_shapes(&run, &sampler, name, &shell, &options);
    let (records, text) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = sampler.finish();
            return Err(e);
        }
    };
    let (mut report, table) = finish(name, &options, &records, &text);
    report["started"] = json!(started);
    report["ended"] = json!(bundle::utc_now());
    let write = |file: &str, bytes: &[u8]| {
        let path = run.folder.join(file);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    write("samples.txt", text.as_bytes())?;
    write("routes.txt", table.as_bytes())?;
    write(
        "routes.json",
        serde_json::to_string_pretty(&report)
            .unwrap_or_default()
            .as_bytes(),
    )?;
    print!("{table}");
    println!("== done: {}", run.folder.join("routes.json").display());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_reply(buckets: &[u64], real: (f64, f64), model: (f64, f64)) -> Value {
        json!({
            "frames": buckets.iter().sum::<u64>(),
            "buckets": buckets,
            "longest": 0.25,
            "paused": 0,
            "first_real": real.0,
            "last_real": real.1,
            "first_model": model.0,
            "last_model": model.1,
        })
    }

    #[test]
    fn the_p5_is_the_rank_the_fps_script_takes() {
        // 45 windows: rank floor(45 * 0.05 + 0.5) = 2.
        let mut windows = vec![60.0; 45];
        windows[7] = 3.0;
        windows[30] = 9.0;
        assert_eq!(percentile5(&windows), Some(9.0));
        // 10 windows: rank floor(0.5 + 0.5) = 1, the minimum.
        let ten = [5.0, 9.0, 8.0, 7.0, 6.0, 5.5, 5.4, 5.3, 5.2, 5.1];
        assert_eq!(percentile5(&ten), Some(5.0));
        assert_eq!(percentile5(&[]), None);
    }

    #[test]
    fn mean_fps_is_frames_over_the_real_span_and_model_over_real_is_a_ratio() {
        let mut buckets = vec![30u64; 45];
        buckets[3] = 12;
        let v = frames_reply(&buckets, (100.0, 145.0), (10.0, 50.5));
        let s = frame_stats(&v).unwrap();
        assert_eq!(s.frames, 30 * 44 + 12);
        assert!((s.mean_fps - (s.frames as f64 / 45.0)).abs() < 1e-9);
        assert_eq!(s.p5_fps, Some(30.0));
        assert!((s.model_per_real - 40.5 / 45.0).abs() < 1e-9);
        assert!((s.longest_ms - 250.0).abs() < 1e-9);
    }

    #[test]
    fn a_reply_without_a_window_is_an_error() {
        let v = frames_reply(&[30], (5.0, 5.0), (0.0, 0.0));
        assert!(frame_stats(&v).unwrap_err().contains("no length"));
        assert!(frame_stats(&json!({ "buckets": [1] })).is_err());
    }

    const FILE: &str = "hz 100\ntz +0200\n\
1000.0 100 20 - 1 3 - - - - - - - - - 9\n\
1001.0 150 30 - 1 3 - - - - - - - - - 9\n\
1002.0 250 50 - 1 3 - - - - - - - - - 9\n\
1003.0 350 70 - 1 3 - - - - - - - - - 9\n\
1004.0 450 90 - 1 3 - - - - - - - - - 9\n\
1005.0 550 110 - 1 3 - - - - - - - - - 9\n";

    #[test]
    fn cpu_is_taken_over_the_window_the_log_stamps_after_the_zone_offset() {
        let parsed = meter::parse_sampler(FILE);
        assert_eq!(parsed.samples.len(), 6);
        // The log reads 2 h ahead of UTC: stamp 1002.5 + 7200 is epoch 1002.5.
        let window = LogWindow {
            start: 1001.5 + 7200.0,
            end: 1004.5 + 7200.0,
            real_start: 0.0,
            real_end: 3.0,
            frames: 180,
        };
        let inside = window_samples(&parsed, &window);
        // The second of slack each side keeps 1001 to 1005.
        assert_eq!(inside.len(), 5);
        let narrow = LogWindow {
            start: 1003.0 + 7200.0,
            end: 1003.0 + 7200.0,
            ..window
        };
        // 1002, 1003, 1004: ticks 300 to 540 over 2 s at 100 Hz.
        let samples = window_samples(&parsed, &narrow);
        assert_eq!(samples.len(), 3);
        let cpu = window_cpu(&parsed, &narrow).unwrap();
        // (utime+stime) 300 -> 540 ticks = 2.4 s of CPU over 2 s = 120 %.
        assert!((cpu - 120.0).abs() < 1e-6, "{cpu}");
    }

    #[test]
    fn a_log_stamped_in_utc_is_matched_when_the_container_has_an_offset() {
        let parsed = meter::parse_sampler(FILE);
        // The same samples, but the log's stamps are plain epoch seconds.
        let window = LogWindow {
            start: 1002.0,
            end: 1004.0,
            real_start: 0.0,
            real_end: 2.0,
            frames: 120,
        };
        assert_eq!(window_samples(&parsed, &window).len(), 5);
        let cpu = window_cpu(&parsed, &window).unwrap();
        assert!((cpu - 120.0).abs() < 1e-6, "{cpu}");
    }

    #[test]
    fn a_window_with_one_sample_has_no_cpu() {
        let parsed = meter::parse_sampler(FILE);
        let far = LogWindow {
            start: 9000.0,
            end: 9001.0,
            real_start: 0.0,
            real_end: 1.0,
            frames: 60,
        };
        assert_eq!(window_cpu(&parsed, &far), None);
    }

    fn measure(shape: &str, fps: f64, cpu: Option<f64>) -> Measure {
        Measure {
            shape: shape.to_owned(),
            groups: 40,
            units: 400,
            mean_fps: Some(fps),
            p5_fps: Some(fps - 5.0),
            model_per_real: Some(1.0),
            cpu_percent: cpu,
            moving_share: Some(1.0),
            skipped: None,
        }
    }

    #[test]
    fn the_table_averages_each_shape_over_its_sweeps_and_keeps_a_skip() {
        let mut ms = vec![
            measure("offroad-few", 40.0, Some(100.0)),
            measure("onroad", 0.0, None),
            measure("offroad-few", 60.0, None),
        ];
        ms[1] = Measure {
            shape: "onroad".to_owned(),
            skipped: Some("no road".to_owned()),
            ..Measure::default()
        };
        let rows = aggregate(&ms);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0.shape, "offroad-few");
        assert_eq!(rows[0].1, 2);
        assert_eq!(rows[0].0.mean_fps, Some(50.0));
        // Averaged over the one measurement that has a CPU figure.
        assert_eq!(rows[0].0.cpu_percent, Some(100.0));
        assert_eq!(rows[1].1, 0);
        let text = table(&rows);
        assert!(text.contains("offroad-few"));
        assert!(text.contains("50.0"));
        assert!(text.contains("skipped: no road"));
    }

    #[test]
    fn sweeps_rotate_the_order_of_the_shapes() {
        let shapes: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(order(&shapes, 0), ["a", "b", "c"]);
        assert_eq!(order(&shapes, 1), ["b", "c", "a"]);
        assert_eq!(order(&shapes, 3), ["a", "b", "c"]);
        assert!(order(&[], 2).is_empty());
    }

    #[test]
    fn options_have_defaults_and_refuse_bad_values() {
        let o = parse_options(&[]).unwrap();
        assert_eq!((o.units, o.sweeps, o.settle, o.measure), (400, 1, 20, 45));
        assert_eq!(o.shapes.len(), SHAPES.len());
        let o = parse_options(&[
            "--units",
            "800",
            "--sweeps",
            "3",
            "--shapes",
            "onroad,size-1",
        ])
        .unwrap();
        assert_eq!((o.units, o.sweeps), (800, 3));
        assert_eq!(o.shapes, ["onroad", "size-1"]);
        assert!(parse_options(&["--units", "10"]).is_err());
        assert!(parse_options(&["--sweeps", "0"]).is_err());
        assert!(parse_options(&["--shapes", "nope"]).is_err());
        assert!(parse_options(&["--measure", "3"]).is_err());
        assert!(parse_options(&["--kind", "x'); os.exit() --"]).is_err());
        assert!(parse_options(&["--units"]).is_err());
        assert!(parse_options(&["--step", "9000"]).is_err());
        let o = parse_options(&["--formations", "column=Vee,line=Rank"]).unwrap();
        assert_eq!(o.formations[0], ("column".to_owned(), "Vee".to_owned()));
        assert!(parse_options(&["--formations", "wedge=Vee"]).is_err());
    }

    #[test]
    fn the_spawn_source_carries_every_parameter() {
        let o = parse_options(&[
            "--units",
            "120",
            "--kind",
            "M-1 Abrams",
            "--formations",
            "column=Vee",
        ])
        .unwrap();
        let src = spawn_source("formation-column", &o);
        assert!(src.contains("R.spawn('formation-column', 120,"));
        assert!(src.contains("kind = 'M-1 Abrams'"));
        assert!(src.contains("length = 4000"));
        assert!(src.contains("step = 50"));
        assert!(src.contains("formations = { column = 'Vee' }"));
    }

    #[test]
    fn every_shape_is_one_the_lua_generator_knows() {
        let lua = include_str!("../../../lua/scenarios/routes.lua");
        for shape in SHAPES {
            assert!(
                lua.contains(&format!("['{shape}'] = {{")),
                "routes.lua has no config for {shape}"
            );
            assert!(lua.contains(&format!("'{shape}'")), "{shape}");
        }
    }
}
