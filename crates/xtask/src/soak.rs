//! `soak <name> [--minutes 30] [--units 1200]`: the S11 churn soak on a
//! docker-wine target, with drift detection.
//!
//! The run deploys the build with the eval channel, restarts DCS, loads a
//! fresh shell mission, attaches `link-probe` in the game container as the
//! sidecar, and spawns `--units` objects with the moving, mixed scenario
//! (`lua/scenarios/scenario.lua`, `spawn_mixed`). Then, for `--minutes`, it
//! destroys groups and spawns the same number of units again at a steady
//! rate (see [`CHURN_INTERVAL_S`]), and once a minute records:
//!
//! - the `server` state's Lua heap, from the `heap` lines `link-probe` prints
//!   from the status block (`lua_heap_kib`, which the agent sets on each
//!   pump): the lowest, mean and highest of the minute;
//! - the DCS process's resident memory and CPU, and the container's CPU,
//!   throttling, memory and load, from the sampler in [`crate::meter`] that
//!   runs beside DCS and finds it by its executable name;
//! - the largest object ID in the sidecar's mirror (`link-probe --mirror`),
//!   the objects spawned so far, and how many exist;
//! - the agent's mean share, `frames_over_250` and `frame_max_us`, from the
//!   `sampling` LOG reports of the minute (`spec/sampling.md`, "Reporting");
//! - DCS's frame rate over 15 s (`lua/sampling/fps.lua`) and the share of
//!   ground groups and ships that moved in that window
//!   (`lua/scenarios/movers.lua`).
//!
//! A run passes only when it is conclusive. The heap verdict wants three
//! floors after the warm-up. The first natural collection cycle after a
//! restart ends about 25 minutes in, and the next had not ended 63 minutes
//! later (LIVE, 2026-11-14, DCS 2.9.31.29434 under Wine 10.0: a 30-minute
//! and a 90-minute run). A run without `--collect-every` cannot reach three
//! floors within [`MAX_MINUTES`], so `--collect-every` is how the heap is
//! judged. With too few floors the run prints INCONCLUSIVE and exits 2; a
//! failure exits 1.
//!
//! `--no-sidecar` runs the same deploy, spawn and churn without `link-probe`.
//! A detached agent takes no samples (`spec/transport.md`, "Detaching"), so
//! the agent stays loaded and idle: the control for the resident memory. The
//! heap then comes from `collectgarbage('count')` over the eval channel, one
//! reading a minute, and the agent's figures and the mirror are absent.
//!
//! `--collect-every <minutes>` forces a full collect in the `server` state
//! through the eval channel at that interval and judges the heap by the
//! floors those collects read ([`forced_heap_verdict`]).
//!
//! At the end it judges the heap by the forced collects' floors with
//! `--collect-every` and by the collection cycles' floors without it (see
//! [`verdict_with`]), fits a least-squares line to the second half of the run for the other
//! series, and writes the verdict ([`verdict`]). Everything goes under
//! `bundles/soak-<name>-<time>/`: `soak.json` (the minutes and the verdict),
//! `verdict.json`, the sampler's file, the probe's output and recording. A
//! recording holds server data and is never committed. The run leaves the
//! target running the build it deployed, with the scenario's objects in the
//! shell mission: the caller restores the target.

use std::collections::VecDeque;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::meter::{self, Sampler};
use crate::sampling::live::{self, DIR, Opened, Run, SHARE};
use crate::{bundle, link};

pub mod series;

const USAGE: &str = "usage: cargo xtask soak <name> [--minutes 30] [--units 1200] \
[--collect-every MINUTES] [--no-sidecar]";

/// Where the sampler runs in the container. Not [`DIR`], which the probe
/// removes and recreates.
const SAMPLER_DIR: &str = "/tmp/nanoprobe-soak";

/// The share of ground groups that move when the scenario is spawned: one
/// in five, as at 1,200 and 2,000 objects in S10 (with every group moving,
/// 2,000 objects held DCS under 2 fps).
const MOVING: f64 = 0.2;

/// Seconds between churn steps. A step destroys about `units / 48` units
/// and spawns as many, so the whole population turns over in 48 steps,
/// 24 minutes: every object the run starts with is replaced once inside a
/// 30-minute run, and the second half, which the drift fit reads, sees
/// the steady state. A slower rate gives a per-object leak (a row, a Lua
/// table, an object ID) too few cycles to rise above the noise. A faster
/// rate makes the births dominate the agent's budget, which then measures
/// the spawn burst and not the long run.
const CHURN_INTERVAL_S: u64 = 30;
/// A churn step handles `units / CHURN_DIVISOR` units, and at least 10.
const CHURN_DIVISOR: usize = 48;
/// Seconds between records.
const SAMPLE_INTERVAL_S: u64 = 60;
/// The frame-rate window of a record, seconds. Long enough for a stable
/// 5th percentile, short enough that the churn is not held for long.
const FPS_S: u32 = 15;
/// The part of the run that is warm-up. The fit reads only what comes after
/// it: the heap and the resident memory fill as the scenario spawns and as
/// the first collections run, and the churn needs one turnover to reach a
/// steady state.
const WARMUP: f64 = 0.5;
/// A series is flat when it grows by less than this share of its mean per
/// hour. One percent of a 200 MB heap is 2 MB an hour: a leak of one
/// kilobyte per spawned object at the churn rate (about 3,000 objects an
/// hour) is 3 MB an hour and shows; a smaller one is below what a Lua
/// collector's own saw-tooth lets a half-hour fit tell from noise, which
/// [`drift`] reports as `resolved`.
pub const FLAT_PER_HOUR: f64 = 0.01;

/// DCS's step between object IDs, 256 (LIVE: the 2026-11-14 soak run, 29
/// minutes at 1,200 units with the sidecar, on a docker-wine target; the
/// build, platform and date are in `docs/results/2026-11-14-s11-budget-and-baseline.md`,
/// section "Soak").
pub const ID_STEP: f64 = 256.0;
/// The share of [`ID_STEP`] by which the fitted IDs per spawned object may
/// exceed it: group and scenery IDs are not units, and a half-hour fit over
/// a few hundred spawns carries noise.
pub const ID_TOLERANCE: f64 = 0.10;

/// The pieces of one record's series that [`verdict`] fits.
const HEAP: &str = "/heap/min_kib";
const RSS: &str = "/process/rss_kb_mean";

fn number(record: &Value, pointer: &str) -> Option<f64> {
    record.pointer(pointer).and_then(Value::as_f64)
}

/// A least-squares line through (x, y).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub n: usize,
    pub slope: f64,
    /// The standard error of the slope.
    pub stderr: f64,
    pub mean_y: f64,
}

/// Fit a line. `None` with fewer than 3 points or when every x is the same.
pub fn fit(points: &[(f64, f64)]) -> Option<Fit> {
    let n = points.len();
    if n < 3 {
        return None;
    }
    let nf = n as f64;
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / nf;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / nf;
    let sxx: f64 = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum();
    if sxx <= f64::EPSILON * mean_x.abs().max(1.0) {
        return None;
    }
    let sxy: f64 = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum();
    let slope = sxy / sxx;
    let sse: f64 = points
        .iter()
        .map(|p| (p.1 - (mean_y + slope * (p.0 - mean_x))).powi(2))
        .sum();
    Some(Fit {
        n,
        slope,
        stderr: (sse / (nf - 2.0) / sxx).sqrt(),
        mean_y,
    })
}

/// The points of a series after warm-up: `(hours, value)` for every record
/// whose minute is past `WARMUP` of `minutes` and that has the value.
fn after_warmup(records: &[Value], minutes: u32, pointer: &str) -> Vec<(f64, f64)> {
    records
        .iter()
        .filter_map(|r| Some((number(r, "/minute")?, number(r, pointer)?)))
        .filter(|(m, _)| *m > f64::from(minutes) * WARMUP)
        .map(|(m, v)| (m / 60.0, v))
        .collect()
}

/// A series' drift: the slope per hour, its standard error, the growth as a
/// share of the mean per hour, whether it is flat, and whether the fit can
/// tell a slope of [`FLAT_PER_HOUR`] from noise (twice the standard error is
/// under it).
pub fn drift(points: &[(f64, f64)]) -> Value {
    let Some(f) = fit(points) else {
        return json!({ "n": points.len(), "status": "no data", "flat": false, "resolved": false });
    };
    let growth = if f.mean_y != 0.0 {
        f.slope / f.mean_y
    } else {
        f64::INFINITY
    };
    let noise = if f.mean_y != 0.0 {
        2.0 * f.stderr / f.mean_y.abs()
    } else {
        f64::INFINITY
    };
    json!({
        "n": f.n,
        "mean": f.mean_y,
        "slope_per_hour": f.slope,
        "stderr_per_hour": f.stderr,
        "growth_percent_per_hour": growth * 100.0,
        "noise_percent_per_hour": noise * 100.0,
        "threshold_percent_per_hour": FLAT_PER_HOUR * 100.0,
        "flat": growth < FLAT_PER_HOUR,
        "status": if growth < FLAT_PER_HOUR { "flat" } else { "growing" },
        "resolved": noise < FLAT_PER_HOUR,
    })
}

/// A minute's heap minimum that falls by more than this share of the
/// minute's mean heap, against the minute before, is a collection finishing.
/// Lua 5.1's collector is incremental: it frees mostly at the end of a
/// cycle, and a cycle takes tens of minutes at this allocation rate, so
/// between cycle ends the minimum rises with the garbage and says nothing
/// about the live set. A normal minute moves the minimum by about 1 %; the
/// end of the cycle in the 2026-11-14 soak run (LIVE; section "Soak" of
/// `docs/results/2026-11-14-s11-budget-and-baseline.md`) moved it by 11 %, 35 % and 10 % over three
/// minutes. 10 % is far above the first and at the edge of the last, which is
/// why consecutive drops count as one cycle.
pub const CYCLE_DROP: f64 = 0.10;
/// Cycle floors a heap verdict needs. A line has a slope with 2 points and a
/// standard error with 3; with fewer the run says nothing about the live
/// set's trend and the verdict is "inconclusive".
pub const MIN_FLOORS: usize = 3;
/// A minute whose share of frames over 250 microseconds passes this is listed
/// in the verdict. A quiet minute is about 0.1 %; the minutes that held a
/// collection in that run (LIVE) had 1.6 % and 2.1 %.
pub const SPIKE_SHARE: f64 = 0.01;

/// One finished collection cycle of the Lua heap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cycle {
    /// The first and the last minute of the fall (consecutive drops of more
    /// than [`CYCLE_DROP`] are one cycle).
    pub first_minute: f64,
    pub last_minute: f64,
    /// The heap minimum of the minute before the fall.
    pub before_kib: f64,
    /// The lowest heap minimum from the fall's last minute until the next
    /// cycle ends, the live set the cycle left, and its minute.
    pub floor_kib: f64,
    pub floor_minute: f64,
}

/// The finished cycles in the records' per-minute heap minimum and mean.
pub fn cycles(records: &[Value]) -> Vec<Cycle> {
    let series: Vec<(f64, f64, f64)> = records
        .iter()
        .filter_map(|r| {
            let min = number(r, HEAP)?;
            Some((
                number(r, "/minute")?,
                min,
                number(r, "/heap/mean_kib").unwrap_or(min),
            ))
        })
        .collect();
    // The index ranges [first, last] of the runs of falling minutes.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for i in 1..series.len() {
        let drop = series[i - 1].1 - series[i].1;
        if drop > CYCLE_DROP * series[i].2 {
            match runs.last_mut() {
                Some(run) if run.1 + 1 == i => run.1 = i,
                _ => runs.push((i, i)),
            }
        }
    }
    runs.iter()
        .enumerate()
        .map(|(k, &(first, last))| {
            let end = runs.get(k + 1).map_or(series.len(), |next| next.0);
            let (floor_minute, floor_kib, _) = series[last..end]
                .iter()
                .copied()
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or(series[last]);
            Cycle {
                first_minute: series[first].0,
                last_minute: series[last].0,
                before_kib: series[first - 1].1,
                floor_kib,
                floor_minute,
            }
        })
        .collect()
}

/// The heap's verdict: the slope of the floors the cycles leave, after
/// warm-up. A fit on each minute's lowest reading judges a series that rises
/// for 24 minutes and then falls in one step: it reads the fall as a trend.
/// Only the floors are the live set.
///
/// With fewer than [`MIN_FLOORS`] floors after warm-up the status is
/// "inconclusive" and `reason` says to run longer. Otherwise the status is
/// [`drift`]'s, over the floors, in KiB per hour.
pub fn heap_verdict(records: &[Value], minutes: u32) -> Value {
    let found = cycles(records);
    let warm = f64::from(minutes) * WARMUP;
    let floors: Vec<(f64, f64)> = found
        .iter()
        .filter(|c| c.floor_minute > warm)
        .map(|c| (c.floor_minute / 60.0, c.floor_kib))
        .collect();
    let ends: Vec<f64> = found.iter().map(|c| c.last_minute).collect();
    let gaps: Vec<f64> = ends.windows(2).map(|w| w[1] - w[0]).collect();
    let length = (!gaps.is_empty()).then(|| gaps.iter().sum::<f64>() / gaps.len() as f64);
    let mut v = drift(&floors);
    if floors.len() < MIN_FLOORS {
        v["status"] = json!("inconclusive");
        v["flat"] = json!(false);
        v["resolved"] = json!(false);
        v["reason"] = json!(format!(
            "{} cycle floor(s) after minute {warm}, {MIN_FLOORS} needed: run longer \
             (a cycle takes {} minutes)",
            floors.len(),
            length.map_or("more than the run's".to_owned(), |l| format!(
                "about {l:.0}"
            )),
        ));
    }
    v["method"] = json!("cycles");
    v["cycles"] = json!(found.len());
    v["cycle_end_minutes"] = json!(ends);
    v["cycle_length_minutes"] = json!(length);
    v["floors_kib"] = json!(
        found
            .iter()
            .map(|c| json!({
                "minute": c.floor_minute,
                "kib": c.floor_kib,
                "fall_kib": c.before_kib - c.floor_kib,
            }))
            .collect::<Vec<_>>()
    );
    v["floors_fitted"] = json!(floors.len());
    v
}

/// The heap's verdict from forced collects: each record's `forced_collect`
/// (`kib` read right after a full collect, so an exact floor) after warm-up,
/// fitted by [`drift`] in KiB per hour. With fewer than [`MIN_FLOORS`] the
/// status is "inconclusive". `method` is "forced".
pub fn forced_heap_verdict(records: &[Value], minutes: u32) -> Value {
    let warm = f64::from(minutes) * WARMUP;
    let all: Vec<(f64, f64, Option<f64>)> = records
        .iter()
        .filter_map(|r| {
            Some((
                number(r, "/minute")?,
                number(r, "/forced_collect/kib")?,
                number(r, "/forced_collect/eval_ms"),
            ))
        })
        .collect();
    let floors: Vec<(f64, f64)> = all
        .iter()
        .filter(|f| f.0 > warm)
        .map(|f| (f.0 / 60.0, f.1))
        .collect();
    let mut v = drift(&floors);
    if floors.len() < MIN_FLOORS {
        v["status"] = json!("inconclusive");
        v["flat"] = json!(false);
        v["resolved"] = json!(false);
        v["reason"] = json!(format!(
            "{} forced floor(s) after minute {warm}, {MIN_FLOORS} needed: run longer \
             or collect more often",
            floors.len(),
        ));
    }
    v["method"] = json!("forced");
    v["floors_kib"] = json!(
        all.iter()
            .map(|f| json!({ "minute": f.0, "kib": f.1, "eval_ms": f.2 }))
            .collect::<Vec<_>>()
    );
    v["floors_fitted"] = json!(floors.len());
    v
}

/// The minutes whose share of frames over 250 microseconds passes
/// [`SPIKE_SHARE`], each with the fall of the heap minimum against the minute
/// before (negative when the heap rose). A collection's frames are long, so
/// the falls and the spikes fall in the same minutes. A forced collect runs
/// in the eval hook, outside the agent's pump, so the agent's counters exclude
/// it and no minute is marked for one; its cost is `forced_collect.eval_ms`.
pub fn frame_spikes(records: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut before: Option<f64> = None;
    for r in records {
        let min = number(r, HEAP);
        if let (Some(frames), Some(over)) = (
            number(r, "/agent/frames"),
            number(r, "/agent/frames_over_250"),
        ) && frames > 0.0
            && over / frames > SPIKE_SHARE
        {
            out.push(json!({
                "minute": r["minute"],
                "frames": frames,
                "frames_over_250": over,
                "share_percent": over / frames * 100.0,
                "frame_max_us": r["agent"]["frame_max_us"],
                "heap_fall_kib": before.zip(min).map(|(b, m)| b - m),
            }));
        }
        before = min.or(before);
    }
    out
}

/// The verdict over the records of a run of `minutes`. `control` is a run
/// without the sidecar: the agent is loaded and idle, so the records have no
/// agent figures or mirror, the heap comes from the eval channel, and the
/// objects are counted in DCS.
///
/// The heap is judged by [`forced_heap_verdict`] with `--collect-every` and by
/// [`heap_verdict`] without it. The resident memory is fitted over
/// the second half. In a control run it is the baseline: growth there is
/// DCS's, and growth beyond it in a run with the sidecar is the agent's.
///
/// Object IDs grow with every spawn by design, so their slope is judged per
/// spawned object: `ids_per_spawned` is the fitted rise in the largest ID per
/// object the run spawned, over the same second half, and `ids_per_hour` the
/// rise per hour. DCS steps its object IDs by [`ID_STEP`] (LIVE, S11 soak
/// run); a rise above [`ID_STEP`] plus [`ID_TOLERANCE`] per object is a leak
/// of IDs. `rows` is the drift of the mirror's row count: with units held
/// constant its slope is 0, and a slope is the agent's table growing. A
/// control has no mirror and no largest ID, so neither is judged there.
///
/// `ok` is true when the heap is not growing, the resident memory is flat,
/// and (with the sidecar) the IDs per spawned object are within the step and
/// the row count is flat. A record after the warm-up with a null `mirror_rows`
/// counts in `object_rows.unparsed` and fails `ok`. `conclusive` is true when
/// the heap has [`MIN_FLOORS`] floors of whichever method ran, the resident
/// memory's and the row count's fits could have told a slope of [`FLAT_PER_HOUR`] from noise, and the ID fit is tight enough to
/// tell a rise of [`ID_TOLERANCE`] from noise.
#[cfg(test)]
pub fn verdict(records: &[Value], minutes: u32, control: bool) -> Value {
    verdict_with(records, minutes, control, false)
}

/// [`verdict`], with the heap judged by the forced collects' floors
/// ([`forced_heap_verdict`]) when `forced` and by the cycles' floors
/// ([`heap_verdict`]) when not.
pub fn verdict_with(records: &[Value], minutes: u32, control: bool, forced: bool) -> Value {
    let heap = if forced {
        forced_heap_verdict(records, minutes)
    } else {
        heap_verdict(records, minutes)
    };
    let rss = drift(&after_warmup(records, minutes, RSS));
    let ids_hour = drift(&after_warmup(records, minutes, "/max_id"));
    let rows = drift(&after_warmup(
        records,
        minutes,
        if control { "/alive" } else { "/mirror_rows" },
    ));
    let per_spawned: Vec<(f64, f64)> = records
        .iter()
        .filter_map(|r| {
            Some((
                number(r, "/minute")?,
                number(r, "/spawned_total")?,
                number(r, "/max_id")?,
            ))
        })
        .filter(|(m, _, _)| *m > f64::from(minutes) * WARMUP)
        .map(|(_, spawned, id)| (spawned, id))
        .collect();
    let id_fit = fit(&per_spawned);
    let ids_per_spawned = id_fit.map(|f| f.slope);
    let ids_ok = id_fit.is_some_and(|f| f.slope <= ID_STEP * (1.0 + ID_TOLERANCE));
    let ids_resolved = id_fit.is_some_and(|f| 2.0 * f.stderr <= ID_STEP * ID_TOLERANCE);
    // A mirror dump that failed to parse leaves `mirror_rows` null and would
    // drop out of the fit silently, so it fails the row verdict.
    let unparsed = if control {
        0
    } else {
        records
            .iter()
            .filter(|r| number(r, "/minute").is_some_and(|m| m > f64::from(minutes) * WARMUP))
            .filter(|r| number(r, "/mirror_rows").is_none())
            .count()
    };
    let mut rows = rows;
    rows["unparsed"] = json!(unparsed);
    if unparsed > 0 {
        rows["ok"] = json!(false);
    }
    let rows_flat = rows["flat"] == json!(true) && unparsed == 0;
    let rss_flat = rss["flat"] == json!(true);
    let (ok, conclusive) = if control {
        (rss_flat, rss["resolved"] == json!(true))
    } else {
        (
            heap["status"] != json!("growing") && rss_flat && ids_ok && rows_flat,
            heap["status"] != json!("inconclusive")
                && rss["resolved"] == json!(true)
                && ids_resolved
                && rows["resolved"] == json!(true),
        )
    };
    json!({
        "ok": ok,
        "conclusive": conclusive,
        "control": control,
        "minutes": minutes,
        "fitted_from_minute": f64::from(minutes) * WARMUP,
        "threshold_percent_per_hour": FLAT_PER_HOUR * 100.0,
        "heap_floors_kib": heap,
        "rss_kb": rss,
        "object_rows": rows,
        "max_object_id_per_hour": ids_hour,
        "ids_per_spawned_object": ids_per_spawned,
        "id_step": ID_STEP,
        "id_tolerance": ID_TOLERANCE,
        "frame_spikes": frame_spikes(records),
    })
}

/// Whether a group name from the scenario is a ground group
/// (`np-<label>-g<N>`), which is what the churn removes.
fn is_ground_group(name: &str) -> bool {
    name.rsplit('-')
        .next()
        .and_then(|last| last.strip_prefix('g'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The ground groups a spawn summary names.
fn ground_groups(summary: &Value) -> Vec<String> {
    summary["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|n| is_ground_group(n))
        .map(str::to_owned)
        .collect()
}

/// The objects a spawn summary counts.
fn objects(summary: &Value) -> u64 {
    ["ground", "ships", "air", "statics"]
        .iter()
        .filter_map(|k| summary[k].as_u64())
        .sum()
}

/// The largest object ID of a mirror dump, and its rows. A dump that does
/// not parse gives neither.
fn mirror_ids(mirror: &[u8]) -> (Option<u64>, Option<usize>) {
    let Ok(v) = serde_json::from_slice::<Value>(mirror) else {
        return (None, None);
    };
    let rows = v["rows"].as_array().map_or(&[][..], Vec::as_slice);
    (
        rows.iter().filter_map(|r| r["object"].as_u64()).max(),
        Some(rows.len()),
    )
}

/// The lowest, mean and highest Lua heap, KiB, of the probe's `heap` lines.
fn heap_window(lines: &[Value]) -> Value {
    fn kib<'a>(lines: &'a [Value], k: &'a str) -> impl Iterator<Item = f64> + 'a {
        lines.iter().filter_map(move |l| l[k].as_f64())
    }
    let last: Vec<f64> = kib(lines, "kib").collect();
    if last.is_empty() {
        return json!({ "lines": 0, "min_kib": null, "mean_kib": null, "max_kib": null });
    }
    json!({
        "lines": last.len(),
        "min_kib": kib(lines, "min").fold(f64::INFINITY, f64::min),
        "mean_kib": last.iter().sum::<f64>() / last.len() as f64,
        "max_kib": kib(lines, "max").fold(0.0, f64::max),
    })
}

/// The agent's figures over the `sampling` reports of a window.
fn agent_window(reports: &[Value]) -> Value {
    let (_, span) = live::span(reports);
    let tail = live::tail(reports);
    let share = match (tail["frames"].as_f64(), tail["over_250"].as_f64()) {
        (Some(frames), Some(over)) if frames > 0.0 => json!(over / frames),
        _ => Value::Null,
    };
    json!({
        "reports": reports.len(),
        "mean_share": span["mean"],
        "frames": tail["frames"],
        "frames_over_250": tail["over_250"],
        "frames_over_250_share": share,
        "frame_max_us": tail["max_us"],
    })
}

/// The lines of the probe's output that contain `needle`, the last `n`, as
/// JSON, with `t` (the probe's milliseconds) after `after_ms`.
fn probe_lines(run: &Run, needle: &str, n: usize, after_ms: f64) -> Result<Vec<Value>, String> {
    let out = run.sh(
        &format!("grep -F '{needle}' {DIR}/probe.jsonl | tail -n {n} || true"),
        &[],
    )?;
    Ok(link::parse_lines(&out)
        .into_iter()
        .filter(|l| l["t"].as_f64().unwrap_or(0.0) > after_ms)
        .collect())
}

/// The churn step: remove groups holding about `want` units, from the front
/// of `names`, and say how many units went and how many names it used. An
/// odd step kills each unit with an explosion, which sends death events; an
/// even step destroys the group by script, which sends none. A real
/// campaign has both, and the agent finds a scripted removal only by its
/// existence checks and enumerations.
fn churn_source(names: &[String], want: usize, kill: bool) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!(
        "local E = ...\n\
         local S = E.lib('scenario')\n\
         local freed, used = 0, 0\n\
         for _, name in ipairs({{ {} }}) do\n  \
           if freed >= {want} then break end\n  \
           used = used + 1\n  \
           local g = Group.getByName(name)\n  \
           if g then\n    \
             local units = g:getUnits() or {{}}\n    \
             freed = freed + #units\n    \
             if {kill} then\n      \
               for _, u in ipairs(units) do\n        \
                 if u:isExist() then trigger.action.explosion(u:getPoint(), 300) end\n      \
               end\n    \
             else\n      \
               S.destroy(name)\n    \
             end\n  \
           end\n\
         end\n\
         return {{ freed = freed, used = used }}\n",
        list.join(", ")
    )
}

struct Options {
    minutes: u32,
    units: usize,
    /// The control run: no `link-probe`.
    control: bool,
    /// Force a full collect in the `server` state every this many minutes
    /// and judge the heap by the live heap read right after each one.
    ///
    /// A run without this option cannot reach three floors within
    /// [`MAX_MINUTES`], so this is how the heap is judged (LIVE, 2026-11-14,
    /// DCS 2.9.31.29434 under Wine 10.0; the cycle lengths are in
    /// `docs/results/2026-11-14-s11-budget-and-baseline.md`, section on the
    /// soak). A forced collect reads the live heap directly, and each reading
    /// is an exact floor. The collect runs in the eval hook, outside the
    /// agent's pump, so the agent's frame counters exclude it. `eval_ms` in the
    /// record's `forced_collect` is the eval's round trip, an upper bound on
    /// the collect: it includes the hook's polling and the SSH hop. A collect
    /// that fails is recorded as `forced_collect.error` and the run continues.
    collect_every: Option<u32>,
}

/// The longest run: three hours. The natural cycle ends after the warm-up are
/// too few for [`MIN_FLOORS`] floors (see `collect_every`; an extrapolation
/// from the observed gap, UNVERIFIED).
const MAX_MINUTES: u32 = 180;

/// The minutes of a run and whether a forced collect precedes each one: minute
/// 1 to `minutes`, with the collect flagged by [`collect_due`].
fn schedule(minutes: u32, every: Option<u32>) -> Vec<(u32, bool)> {
    (1..=minutes).map(|m| (m, collect_due(m, every))).collect()
}

/// How many forced collects a run of `minutes` takes after the warm-up, which
/// is what [`forced_heap_verdict`] judges.
fn floors_after_warmup(minutes: u32, every: u32) -> usize {
    let warm = f64::from(minutes) * WARMUP;
    schedule(minutes, Some(every))
        .iter()
        .filter(|(m, due)| *due && f64::from(*m) > warm)
        .count()
}

fn parse_options(args: &[&str]) -> Result<Options, String> {
    let mut o = Options {
        minutes: 30,
        units: 1200,
        control: false,
        collect_every: None,
    };
    let mut rest = args;
    loop {
        if let ["--no-sidecar", tail @ ..] = rest {
            o.control = true;
            rest = tail;
            continue;
        }
        let [flag, value, tail @ ..] = rest else {
            break;
        };
        let n: u32 = value
            .parse()
            .map_err(|_| format!("{flag}: {value:?} is not a number\n{USAGE}"))?;
        match *flag {
            "--minutes" if (4..=MAX_MINUTES).contains(&n) => o.minutes = n,
            "--minutes" => {
                return Err(format!(
                    "--minutes must be from 4 (the fit needs 3 points after warm-up) to {MAX_MINUTES}"
                ));
            }
            "--collect-every" if n >= 1 => o.collect_every = Some(n),
            "--collect-every" => return Err("--collect-every must be at least 1".into()),
            "--units" if n >= 100 => o.units = n as usize,
            "--units" => return Err("--units must be at least 100".into()),
            _ => return Err(USAGE.into()),
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(USAGE.into());
    }
    if let Some(n) = o.collect_every
        && floors_after_warmup(o.minutes, n) < MIN_FLOORS
    {
        let largest = (1..n)
            .rev()
            .find(|k| floors_after_warmup(o.minutes, *k) >= MIN_FLOORS);
        return Err(match largest {
            Some(k) => format!(
                "--collect-every {n} gives fewer than {MIN_FLOORS} floors after the warm-up \
                 in {} minutes; the largest interval that works is {k}",
                o.minutes
            ),
            None => format!(
                "--minutes {} cannot give {MIN_FLOORS} floors after the warm-up",
                o.minutes
            ),
        });
    }
    Ok(o)
}

pub fn command(repository: &Path, name: &str, args: &[&str]) -> Result<ExitCode, String> {
    // The long soak's commands stand where the churn soak takes a target.
    if crate::persistence::campaign_soak::is_command(name) {
        let all: Vec<&str> = std::iter::once(name).chain(args.iter().copied()).collect();
        return crate::persistence::campaign_soak::command(repository, &all);
    }
    let options = parse_options(args)?;
    let Opened { run, shell, probe } = live::open(repository, name, "soak")?;
    println!(
        "== soak: {} units, {} minutes{}{}",
        options.units,
        options.minutes,
        if options.control {
            ", control (no sidecar)"
        } else {
            ""
        },
        options
            .collect_every
            .map_or(String::new(), |n| format!(", forced collect every {n}"))
            + &format!(", folder {}", run.folder.display())
    );
    let started = bundle::utc_now();
    let sampler = Sampler::new(&run.remote, SAMPLER_DIR);
    let result = soak(&run, &sampler, name, &shell, &probe, &options);
    if result.is_err() {
        let _ = sampler.finish();
        if !options.control {
            run.stop_probe("soak");
        }
    }
    let (records, extra) = result?;
    let verdict = verdict_with(
        &records,
        options.minutes,
        options.control,
        options.collect_every.is_some(),
    );
    let ok = verdict["ok"] == json!(true)
        && extra["players_joined"] != json!(true)
        && (options.control || extra["filled"] == json!(true));
    let conclusive = verdict["conclusive"] == json!(true);
    let out = json!({
        "target": name,
        "provenance": provenance(name),
        "started": started,
        "ended": bundle::utc_now(),
        "units": options.units,
        "minutes": options.minutes,
        "sidecar": !options.control,
        "collect_every_minutes": options.collect_every,
        "churn_interval_s": CHURN_INTERVAL_S,
        "ok": ok,
        "conclusive": conclusive,
        "verdict": verdict,
        "run": extra,
        "records": records,
    });
    let write = |file: &str, v: &Value| {
        let path = run.folder.join(file);
        std::fs::write(&path, serde_json::to_string_pretty(v).unwrap_or_default())
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    write("soak.json", &out)?;
    write("verdict.json", &out["verdict"])?;
    println!("{}", out["verdict"]);
    println!(
        "== {}: {}",
        outcome(ok, conclusive),
        run.folder.join("soak.json").display()
    );
    Ok(ExitCode::from(exit_status(ok, conclusive)))
}

/// The line's word: a run that is not conclusive does not pass.
fn outcome(ok: bool, conclusive: bool) -> &'static str {
    match (ok, conclusive) {
        (false, _) => "FAIL",
        (true, false) => "INCONCLUSIVE",
        (true, true) => "PASS",
    }
}

/// 0 for a pass, 1 for a failure, 2 when nothing failed and the run could
/// not tell (too few heap cycles, or a fit too noisy).
fn exit_status(ok: bool, conclusive: bool) -> u8 {
    match (ok, conclusive) {
        (false, _) => 1,
        (true, false) => 2,
        (true, true) => 0,
    }
}

/// What the result was measured on, which CLAUDE.md requires of every LIVE
/// result: the platform, the DCS build and branch, the Wine version, and the
/// deployed agent build. A figure that cannot be read is an `error`.
pub(crate) fn provenance(name: &str) -> Value {
    let mut v = platform_provenance(name);
    v["agent_build"] = agent_build(name);
    v
}

/// The agent build the target holds now: its name, or `{ "error": .. }`.
pub(crate) fn agent_build(name: &str) -> Value {
    match crate::open(name).and_then(|p| crate::install::manifest(p.as_ref())) {
        Ok(m) => json!(m.active),
        Err(e) => json!({ "error": e }),
    }
}

/// [`provenance`] without the agent build: the platform, the DCS build and
/// the Wine version, which a run's parts share.
pub(crate) fn platform_provenance(name: &str) -> Value {
    let mut v = json!({});
    match crate::target(name).and_then(|t| crate::facts(&t)) {
        Ok(facts) => {
            v["platform"] = json!(facts.kind);
            v["dcs"] = json!(facts.dcs);
            v["wine"] = json!(facts.wine);
        }
        Err(e) => v["facts_error"] = json!(e),
    }
    v
}

/// Deploy, run the churn and take the records. Returns them with the run's
/// totals; the sampler and the probe are stopped on success.
fn soak(
    run: &Run,
    sampler: &Sampler,
    name: &str,
    shell: &[u8],
    probe: &Path,
    options: &Options,
) -> Result<(Vec<Value>, Value), String> {
    run.fresh(name, false, shell)?;
    if !options.control {
        run.start_probe(probe, 0)?;
    }
    sampler.start()?;
    run.hold(5.0)?;
    let mark = if options.control {
        0
    } else {
        run.lines()?.len()
    };
    let initial = run.spawn(options.units, "a", MOVING)?;
    let mut spawned_total = objects(&initial);
    // The control has no mirror to wait for: the spawn call returns when
    // DCS holds the objects.
    let (filled, fill_s) = if options.control {
        (true, 0.0)
    } else {
        run.fill(mark, options.units, 1.0)?
    };
    let mut queue: VecDeque<String> = ground_groups(&initial).into();
    // The first mover count only records positions.
    let _ = meter::movers(run);

    let batch = (options.units / CHURN_DIVISOR).max(10);
    let begin = Instant::now();
    let mut next_tick = begin + Duration::from_secs(CHURN_INTERVAL_S);
    let mut next_sample = begin + Duration::from_secs(SAMPLE_INTERVAL_S);
    let (mut tick, mut destroyed_total) = (0u64, 0u64);
    let mut records: Vec<Value> = Vec::new();
    let (mut last_heap_ms, mut last_report_ms) = (0.0f64, 0.0f64);
    let (mut sampler_seen, mut max_id_seen) = (0usize, 0u64);
    let mut players_joined = false;
    let mut hz = 100.0;

    // Record k is due at begin + 60 k s, so the last one (minute N) is due
    // at the end of the run: the loop runs until it has taken all N.
    let plan = schedule(options.minutes, options.collect_every);
    let provenance = platform_provenance(name);
    while records.len() < plan.len() {
        let now = Instant::now();
        if now >= next_sample {
            next_sample += Duration::from_secs(SAMPLE_INTERVAL_S);
            let (minute, collect) = plan[records.len()];
            let minute = f64::from(minute);
            // The collect runs in the eval hook, outside the agent's pump. A
            // failed one is recorded and the run goes on.
            let forced =
                collect.then(|| forced_collect(run).unwrap_or_else(|e| json!({ "error": e })));
            let (record, heap_ms, report_ms, seen, id, h) = sample(
                run,
                sampler,
                minute,
                (last_heap_ms, last_report_ms, sampler_seen),
                spawned_total,
                destroyed_total,
                !options.control,
            )?;
            (last_heap_ms, last_report_ms, sampler_seen) = (heap_ms, report_ms, seen);
            max_id_seen = max_id_seen.max(id);
            hz = h;
            players_joined |= record["players"].as_u64().unwrap_or(0) > 0;
            let mut record = record;
            record["max_id_seen"] = json!(max_id_seen);
            if let Some(f) = forced {
                record["forced_collect"] = f;
            }
            println!("  minute {minute}: {record}");
            records.push(record);
            // Kept after every record, so an interrupted run leaves its data.
            let partial = json!({
                "records": records,
                "units": options.units,
                "minutes": options.minutes,
                "sidecar": !options.control,
                "collect_every_minutes": options.collect_every,
                "provenance": provenance,
            });
            let _ = std::fs::write(run.folder.join("partial.json"), partial.to_string());
        } else if now >= next_tick {
            next_tick += Duration::from_secs(CHURN_INTERVAL_S);
            tick += 1;
            let names: Vec<String> = queue.iter().take(6).cloned().collect();
            if names.is_empty() {
                continue;
            }
            let freed = run.eval(
                "server",
                "soak-churn",
                &churn_source(&names, batch, tick % 2 == 1),
                120,
            )?;
            let (units, used) = (
                freed["freed"].as_u64().unwrap_or(0) as usize,
                freed["used"].as_u64().unwrap_or(0) as usize,
            );
            queue.drain(..used.min(queue.len()));
            destroyed_total += units as u64;
            if units > 0 {
                let summary = run.spawn(units, &format!("c{tick}"), MOVING)?;
                spawned_total += objects(&summary);
                queue.extend(ground_groups(&summary));
            }
        } else {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    // One last frame meter window, timed from dcs.log's stamps.
    let meter_window =
        meter::frames(run, "soak-final", 0, 30).unwrap_or_else(|e| json!({ "error": e }));
    let reports = if options.control {
        Vec::new()
    } else {
        run.reports(mark)?
    };
    let sampler_text = sampler.finish()?;
    std::fs::write(run.folder.join("samples.txt"), &sampler_text)
        .map_err(|e| format!("samples.txt: {e}"))?;
    let all = meter::parse_sampler(&sampler_text);
    let extra = json!({
        "population": options.units,
        "churn_batch_units": batch,
        "churn_steps": tick,
        "spawned_total": spawned_total,
        "destroyed_total": destroyed_total,
        "filled": filled,
        "fill_s": fill_s,
        "players_joined": players_joined,
        "agent": agent_window(&reports),
        "agent_span": live::span(&reports).1["mean"],
        "agent_within_share": live::span(&reports).0,
        "share_limit": SHARE,
        "whole_run_process": meter::summarize(&all.samples, if hz > 0.0 { hz } else { all.hz }),
        "frame_meter": meter_window,
        "tz_offset_s": all.utc_offset_s,
    });
    if !options.control {
        run.stop_probe("soak");
    }
    Ok((records, extra))
}

/// Whether the record of `minute` is preceded by a forced collect.
fn collect_due(minute: u32, every: Option<u32>) -> bool {
    every.is_some_and(|n| minute.is_multiple_of(n))
}

/// A full collect in the `server` state and the live heap right after it:
/// an exact floor. `eval_ms` is the eval's round trip timed on this side,
/// which holds the collect and the SSH hop.
fn forced_collect(run: &Run) -> Result<Value, String> {
    let begin = Instant::now();
    let v = run.eval(
        "server",
        "soak-collect",
        "collectgarbage('collect'); return collectgarbage('count')",
        120,
    )?;
    let eval_ms = begin.elapsed().as_secs_f64() * 1000.0;
    let kib = v
        .as_f64()
        .ok_or_else(|| format!("the forced heap reading is not a number: {v}"))?;
    Ok(json!({ "kib": kib, "eval_ms": eval_ms }))
}

/// The `server` state's Lua heap by the eval channel, as a window of one
/// reading. The control has no status block to read it from.
fn heap_by_eval(run: &Run) -> Result<Value, String> {
    let v = run.eval("server", "soak-heap", "return collectgarbage('count')", 60)?;
    let kib = v
        .as_f64()
        .ok_or_else(|| format!("the heap reading is not a number: {v}"))?;
    Ok(json!({ "lines": 1, "min_kib": kib, "mean_kib": kib, "max_kib": kib, "source": "eval" }))
}

/// One record. Returns it with the probe times of the last heap and report
/// line it used, the sampler lines it has consumed, the largest object ID,
/// and the sampler's clock rate. Without the sidecar (`sidecar` false) the
/// heap comes from the eval channel and the agent's figures, the mirror and
/// the largest ID are absent.
fn sample(
    run: &Run,
    sampler: &Sampler,
    minute: f64,
    (after_heap, after_report, seen): (f64, f64, usize),
    spawned_total: u64,
    destroyed_total: u64,
    sidecar: bool,
) -> Result<(Value, f64, f64, usize, u64, f64), String> {
    let heap_lines = if sidecar {
        probe_lines(run, "\"ev\":\"heap\"", 20, after_heap)?
    } else {
        Vec::new()
    };
    let heap_ms = heap_lines
        .iter()
        .filter_map(|l| l["t"].as_f64())
        .fold(after_heap, f64::max);
    let reports = if sidecar {
        probe_lines(run, "\"msg\":\"sampling\"", 12, after_report)?
    } else {
        Vec::new()
    };
    let report_ms = reports
        .iter()
        .filter_map(|l| l["t"].as_f64())
        .fold(after_report, f64::max);
    let data: Vec<Value> = reports.iter().map(|l| l["data"].clone()).collect();

    let parsed = meter::parse_sampler(&sampler.read(seen + 3)?);
    let process = meter::summarize(&parsed.samples, parsed.hz);

    let (max_id, rows) = if sidecar {
        mirror_ids(&run.mirror()?)
    } else {
        (None, None)
    };
    let alive = run.live_count().ok();
    let players = run.remote.players().unwrap_or(0);
    let fps = run.fps_window(&format!("minute {minute}"), 0, FPS_S)?;

    let record = json!({
        "minute": minute,
        "heap": if sidecar { heap_window(&heap_lines) } else { heap_by_eval(run)? },
        "process": process,
        "max_id": max_id,
        "mirror_rows": rows,
        "alive": alive,
        "spawned_total": spawned_total,
        "destroyed_total": destroyed_total,
        "agent": sidecar.then(|| agent_window(&data)),
        "fps": fps["fps"],
        "p5_fps": fps["p5_fps"],
        "model_per_real": fps["model_per_real"],
        "moving": fps["moving"],
        "players": players,
    });
    Ok((
        record,
        heap_ms,
        report_ms,
        seen + parsed.samples.len(),
        max_id.unwrap_or(0),
        parsed.hz,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` minutes of records with the given series.
    fn records(
        n: u32,
        heap: impl Fn(u32) -> f64,
        rss: impl Fn(u32) -> f64,
        id: impl Fn(u32) -> f64,
    ) -> Vec<Value> {
        (1..=n)
            .map(|m| {
                json!({
                    "minute": m,
                    "heap": { "min_kib": heap(m), "mean_kib": heap(m) + 1000.0 },
                    "process": { "rss_kb_mean": rss(m) },
                    "max_id": id(m),
                    "mirror_rows": 1200,
                    "spawned_total": 1200.0 + 25.0 * f64::from(m),
                })
            })
            .collect()
    }

    #[test]
    fn a_perfect_line_fits_exactly() {
        let points: Vec<(f64, f64)> = (0..10)
            .map(|i| (f64::from(i), 3.0 + 2.0 * f64::from(i)))
            .collect();
        let f = fit(&points).unwrap();
        assert!((f.slope - 2.0).abs() < 1e-12);
        assert!(f.stderr < 1e-9);
        assert!((f.mean_y - 12.0).abs() < 1e-12);
    }

    #[test]
    fn too_few_points_or_one_x_fit_nothing() {
        assert_eq!(fit(&[(0.0, 1.0), (1.0, 2.0)]), None);
        assert_eq!(fit(&[(1.0, 1.0), (1.0, 2.0), (1.0, 3.0)]), None);
        assert_eq!(drift(&[(0.0, 1.0)])["status"], json!("no data"));
    }

    /// A heap that rises 4,000 KiB a minute from `floor(c)` and falls back at
    /// every tenth minute, where `c` is the minute that started the climb.
    fn sawtooth(floor: impl Fn(u32) -> f64) -> impl Fn(u32) -> f64 {
        move |m| {
            let start = m - m % 10;
            floor(start) + 4_000.0 * f64::from(m % 10)
        }
    }

    /// The `heap.min_kib` and `heap.mean_kib` of the 2026-11-14 soak run (LIVE:
    /// docker-wine target, with the sidecar): 29 minutes at 1,200 units, one
    /// collection cycle, which fell over minutes 25 to 27. Section "Soak" of
    /// `docs/results/2026-11-14-s11-budget-and-baseline.md` records the DCS
    /// build and the Wine version.
    const LIVE_MIN: [f64; 29] = [
        116669.0, 122697.0, 123947.0, 126135.0, 128132.0, 129884.0, 131326.0, 133226.0, 134766.0,
        136824.0, 138281.0, 140023.0, 141958.0, 143345.0, 145339.0, 146716.0, 148315.0, 150055.0,
        151723.0, 153749.0, 155327.0, 156969.0, 158673.0, 160466.0, 142558.0, 101005.0, 91198.0,
        91440.0, 93143.0,
    ];
    const LIVE_MEAN: [f64; 29] = [
        121582.85, 123469.64, 125222.0, 127314.38, 129113.5, 130791.73, 132449.85, 134163.27,
        135961.77, 137729.82, 139358.92, 141149.54, 142810.27, 144517.38, 146209.18, 147790.08,
        149416.08, 151134.17, 152907.23, 154743.45, 156356.0, 158045.92, 159817.67, 161617.77,
        152617.36, 117066.58, 94171.92, 92544.58, 94229.5,
    ];

    fn live_records() -> Vec<Value> {
        (0..29)
            .map(|i| {
                // Minutes 25 and 26 held the collection: 104 and 143 frames
                // over 250 us, and a 3 ms frame.
                let (frames, over, max) = match i + 1 {
                    25 => (6684, 104, 1282.1),
                    26 => (6676, 143, 3013.1),
                    _ => (6700, 6, 600.0),
                };
                json!({
                    "minute": i + 1,
                    "heap": { "min_kib": LIVE_MIN[i], "mean_kib": LIVE_MEAN[i] },
                    "process": { "rss_kb_mean": 4.5e6 },
                    "agent": { "frames": frames, "frames_over_250": over, "frame_max_us": max },
                })
            })
            .collect()
    }

    #[test]
    fn a_heap_whose_floors_hold_is_flat_whatever_the_saw_tooth() {
        let r = records(
            60,
            sawtooth(|_| 100_000.0),
            |_| 8_000_000.0,
            |m| 1000.0 + 25.0 * f64::from(m),
        );
        let v = verdict(&r, 60, false);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["conclusive"], json!(true));
        let h = &v["heap_floors_kib"];
        assert_eq!(h["status"], json!("flat"));
        assert_eq!(h["cycles"], json!(6));
        assert_eq!(h["cycle_length_minutes"], json!(10.0));
        assert_eq!(h["floors_fitted"], json!(3));
        assert_eq!(h["floors_kib"][0]["kib"], json!(100_000.0));
        assert_eq!(v["fitted_from_minute"], json!(30.0));
    }

    #[test]
    fn rising_floors_over_one_percent_an_hour_are_not_flat() {
        // The floor rises 2 % of its mean an hour: 4,000 KiB an hour.
        let r = records(
            60,
            sawtooth(|c| 200_000.0 + 4_000.0 / 60.0 * f64::from(c)),
            |_| 8_000_000.0,
            |_| 1.0,
        );
        let v = verdict(&r, 60, false);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["heap_floors_kib"]["status"], json!("growing"));
        assert_eq!(v["rss_kb"]["status"], json!("flat"));
        let g = v["heap_floors_kib"]["growth_percent_per_hour"]
            .as_f64()
            .unwrap();
        assert!((1.9..2.1).contains(&g), "growth {g}");
    }

    #[test]
    fn rss_growth_alone_fails_the_run() {
        let r = records(
            60,
            sawtooth(|_| 100_000.0),
            |m| 8_000_000.0 * (1.0 + 0.02 * f64::from(m) / 60.0),
            |_| 1.0,
        );
        let v = verdict(&r, 60, false);
        assert_eq!(v["rss_kb"]["status"], json!("growing"));
        assert_eq!(v["ok"], json!(false));
    }

    #[test]
    fn rss_growth_just_under_the_threshold_is_flat() {
        let r = records(
            60,
            sawtooth(|_| 100_000.0),
            |m| 8_000_000.0 * (1.0 + 0.005 * f64::from(m) / 60.0),
            |_| 1.0,
        );
        let v = verdict(&r, 60, false);
        assert_eq!(v["rss_kb"]["status"], json!("flat"));
        assert_eq!(v["ok"], json!(true));
    }

    #[test]
    fn warm_up_cycles_are_not_fitted() {
        // The floor climbs for the first 30 minutes, then holds.
        let r = records(
            60,
            sawtooth(|c| 100_000.0 + 7_000.0 * f64::from(c.min(30))),
            |_| 8_000_000.0,
            |_| 1.0,
        );
        assert_eq!(verdict(&r, 60, false)["ok"], json!(true));
    }

    #[test]
    fn the_live_runs_one_cycle_is_inconclusive() {
        let r = live_records();
        let found = cycles(&r);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].first_minute, 25.0);
        assert_eq!(found[0].last_minute, 27.0);
        assert_eq!(found[0].before_kib, 160_466.0);
        assert_eq!(
            (found[0].floor_minute, found[0].floor_kib),
            (27.0, 91_198.0)
        );
        let v = verdict(&r, 30, false);
        let h = &v["heap_floors_kib"];
        assert_eq!(h["status"], json!("inconclusive"));
        assert_eq!(h["cycles"], json!(1));
        assert_eq!(h["cycle_end_minutes"], json!([27.0]));
        assert!(h["cycle_length_minutes"].is_null());
        assert!(h["reason"].as_str().unwrap().contains("run longer"));
        // A fall is not a pass, and the run is not conclusive.
        assert_eq!(v["conclusive"], json!(false));
    }

    #[test]
    fn the_cycle_floors_are_listed_with_their_falls() {
        let r = records(60, sawtooth(|_| 100_000.0), |_| 1.0, |_| 1.0);
        let v = heap_verdict(&r, 60);
        let floors = v["floors_kib"].as_array().unwrap();
        assert_eq!(floors.len(), 6);
        assert_eq!(floors[1]["minute"], json!(20.0));
        assert_eq!(floors[1]["fall_kib"], json!(36_000.0));
    }

    /// 60 minutes of records with a forced collect every 10 minutes, whose
    /// floors are `floor(minute)`.
    fn forced_records(floor: impl Fn(u32) -> f64) -> Vec<Value> {
        records(
            60,
            |m| 100_000.0 + 3_000.0 * f64::from(m % 10),
            |_| 4.5e6,
            |m| 1000.0 + 256.0 * f64::from(m),
        )
        .into_iter()
        .enumerate()
        .map(|(i, mut r)| {
            let m = i as u32 + 1;
            if m.is_multiple_of(10) {
                r["forced_collect"] = json!({ "kib": floor(m), "eval_ms": 850.0 });
            }
            r
        })
        .collect()
    }

    #[test]
    fn forced_floors_that_hold_are_flat() {
        let v = verdict_with(&forced_records(|_| 100_000.0), 60, false, true);
        let h = &v["heap_floors_kib"];
        assert_eq!(h["method"], json!("forced"));
        assert_eq!(h["status"], json!("flat"));
        // Minutes 40, 50 and 60 are after the warm-up at minute 30.
        assert_eq!(h["floors_fitted"], json!(3));
        assert_eq!(h["floors_kib"].as_array().unwrap().len(), 6);
        assert_eq!(h["floors_kib"][0]["minute"], json!(10.0));
        assert_eq!(h["floors_kib"][0]["eval_ms"], json!(850.0));
        assert_eq!(v["ok"], json!(true));
    }

    #[test]
    fn the_schedule_takes_every_minute_and_flags_the_last_collect() {
        let plan = schedule(60, Some(10));
        assert_eq!(plan.len(), 60);
        assert_eq!(
            plan.iter().map(|p| p.0).collect::<Vec<_>>(),
            (1..=60).collect::<Vec<_>>()
        );
        let collects: Vec<u32> = plan.iter().filter(|p| p.1).map(|p| p.0).collect();
        assert_eq!(collects, [10, 20, 30, 40, 50, 60]);
        assert!(schedule(60, None).iter().all(|p| !p.1));
    }

    #[test]
    fn a_collect_interval_that_cannot_give_three_floors_is_refused() {
        let args = |every: &'static str| ["--minutes", "30", "--collect-every", every];
        // Minutes 20 and 30 are the only collects after minute 15.
        let e = parse_options(&args("10")).err().unwrap();
        assert!(e.contains("largest interval that works is 6"), "{e}");
        assert!(parse_options(&args("6")).is_ok());
        assert!(parse_options(&args("5")).is_ok());
        assert!(parse_options(&args("40")).is_err());
    }

    #[test]
    fn a_failed_collect_leaves_no_floor() {
        let mut r = records(60, |_| 1e5, |_| 4.5e6, f64::from);
        for (i, r) in r.iter_mut().enumerate() {
            let m = i + 1;
            if m % 10 == 0 {
                r["forced_collect"] = if m == 40 {
                    json!({ "error": "timeout" })
                } else {
                    json!({ "kib": 100_000.0, "eval_ms": 850.0 })
                };
            }
        }
        let h = forced_heap_verdict(&r, 60);
        assert_eq!(h["floors_fitted"], json!(2));
        assert_eq!(h["status"], json!("inconclusive"));
    }

    #[test]
    fn an_unparsed_mirror_dump_fails_the_row_verdict() {
        let r: Vec<Value> = records(60, sawtooth(|_| 1e5), |_| 8e6, |m| ID_STEP * f64::from(m))
            .into_iter()
            .map(|mut r| {
                if r["minute"] == json!(45) {
                    r["mirror_rows"] = Value::Null;
                }
                r
            })
            .collect();
        let v = verdict(&r, 60, false);
        assert_eq!(v["object_rows"]["unparsed"], json!(1));
        assert_eq!(v["ok"], json!(false));
        // A null before the warm-up ends is not counted.
        let mut early = r.clone();
        early[44]["mirror_rows"] = json!(1200);
        early[3]["mirror_rows"] = Value::Null;
        let v = verdict(&early, 60, false);
        assert_eq!(v["object_rows"]["unparsed"], json!(0));
    }

    #[test]
    fn rising_forced_floors_are_growing_and_fail_the_run() {
        // 5 % of the mean every 10 minutes is far over 1 % an hour.
        let v = verdict_with(
            &forced_records(|m| 100_000.0 + 500.0 * f64::from(m)),
            60,
            false,
            true,
        );
        assert_eq!(v["heap_floors_kib"]["status"], json!("growing"));
        assert_eq!(v["ok"], json!(false));
    }

    #[test]
    fn too_few_forced_floors_are_inconclusive() {
        // Floors at minutes 40 and 50 only.
        let mut r = forced_records(|_| 100_000.0);
        r.truncate(55);
        let v = verdict_with(&r, 60, false, true);
        let h = &v["heap_floors_kib"];
        assert_eq!(h["status"], json!("inconclusive"));
        assert_eq!(h["floors_fitted"], json!(2));
        assert!(h["reason"].as_str().unwrap().contains("forced floor"));
        assert_eq!(v["conclusive"], json!(false));
        // No collects at all.
        let none = verdict_with(&records(60, |_| 1.0, |_| 4.5e6, f64::from), 60, true, true);
        assert_eq!(none["heap_floors_kib"]["status"], json!("inconclusive"));
        assert_eq!(none["heap_floors_kib"]["floors_fitted"], json!(0));
    }

    #[test]
    fn a_forced_collect_does_not_excuse_a_frame_spike() {
        let mut r = live_records();
        r[23]["forced_collect"] = json!({ "kib": 90_000.0, "eval_ms": 900.0 });
        let spikes = frame_spikes(&r);
        assert_eq!(spikes.len(), 2);
        assert!(spikes.iter().all(|s| s.get("forced_collect").is_none()));
    }

    #[test]
    fn minutes_with_many_long_frames_are_listed_with_the_heap_fall() {
        let spikes = frame_spikes(&live_records());
        let minutes: Vec<&Value> = spikes.iter().map(|s| &s["minute"]).collect();
        assert_eq!(minutes, [&json!(25), &json!(26)]);
        assert_eq!(spikes[0]["heap_fall_kib"], json!(160_466.0 - 142_558.0));
        assert_eq!(spikes[1]["heap_fall_kib"], json!(142_558.0 - 101_005.0));
        assert_eq!(spikes[1]["frame_max_us"], json!(3013.1));
        let share = spikes[1]["share_percent"].as_f64().unwrap();
        assert!((share - 143.0 / 6676.0 * 100.0).abs() < 1e-9);
        // The same figures are in the verdict.
        assert_eq!(
            verdict(&live_records(), 30, false)["frame_spikes"],
            json!(spikes)
        );
        // A share of exactly 1 % is not over it.
        let edge = vec![json!({
            "minute": 1,
            "heap": { "min_kib": 1.0 },
            "agent": { "frames": 10_000, "frames_over_250": 100 },
        })];
        assert!(frame_spikes(&edge).is_empty());
    }

    #[test]
    fn the_control_verdict_has_rss_and_the_live_count_and_no_agent() {
        // No agent, mirror or largest ID; the heap is one reading a minute.
        let r: Vec<Value> = (1..=60u32)
            .map(|m| {
                let heap = sawtooth(|_| 100_000.0)(m);
                json!({
                    "minute": m,
                    "heap": { "min_kib": heap, "mean_kib": heap, "max_kib": heap, "source": "eval" },
                    "process": { "rss_kb_mean": 4.5e6 * (1.0 + 0.05 * f64::from(m) / 60.0) },
                    "max_id": null,
                    "mirror_rows": null,
                    "alive": 1200,
                    "agent": null,
                    "spawned_total": 1200 + 25 * m,
                })
            })
            .collect();
        let v = verdict(&r, 60, true);
        assert_eq!(v["control"], json!(true));
        assert_eq!(v["rss_kb"]["status"], json!("growing"));
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["object_rows"]["status"], json!("flat"));
        assert_eq!(v["object_rows"]["slope_per_hour"], json!(0.0));
        assert_eq!(v["max_object_id_per_hour"]["status"], json!("no data"));
        assert!(v["ids_per_spawned_object"].is_null());
        assert_eq!(v["frame_spikes"], json!([]));
        assert_eq!(v["heap_floors_kib"]["status"], json!("flat"));
        // The heap does not decide a control: one cycle is still a pass on
        // a flat resident memory.
        let r = live_records();
        let v = verdict(&r, 30, true);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["conclusive"], json!(true));
    }

    #[test]
    fn object_ids_are_reported_per_spawned_object() {
        // Each spawned object uses 3 IDs (a unit, and shares of group and
        // scenery IDs).
        let r = records(30, |_| 1.0, |_| 1.0, |m| 5000.0 + 3.0 * 25.0 * f64::from(m));
        let v = verdict(&r, 30, false);
        let per = v["ids_per_spawned_object"].as_f64().unwrap();
        assert!((per - 3.0).abs() < 1e-9, "ids per spawned object {per}");
        assert!(
            v["max_object_id_per_hour"]["slope_per_hour"]
                .as_f64()
                .unwrap()
                > 0.0
        );
    }

    #[test]
    fn ids_beyond_the_step_per_spawned_object_fail_the_run() {
        // 25 objects a minute: DCS's step is 256 an object; 400 is a leak.
        let rise = |per: f64| move |m: u32| 5000.0 + per * 25.0 * f64::from(m);
        let ok = verdict(
            &records(60, sawtooth(|_| 1e5), |_| 8e6, rise(ID_STEP)),
            60,
            false,
        );
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["conclusive"], json!(true));
        let leak = verdict(
            &records(60, sawtooth(|_| 1e5), |_| 8e6, rise(400.0)),
            60,
            false,
        );
        assert_eq!(leak["ok"], json!(false));
    }

    #[test]
    fn a_growing_row_count_fails_the_run() {
        let r: Vec<Value> = records(60, sawtooth(|_| 1e5), |_| 8e6, |_| 1.0)
            .into_iter()
            .map(|mut r| {
                r["mirror_rows"] = json!(1200.0 + 10.0 * r["minute"].as_f64().unwrap());
                r
            })
            .collect();
        let v = verdict(&r, 60, false);
        assert_eq!(v["object_rows"]["status"], json!("growing"));
        assert_eq!(v["ok"], json!(false));
    }

    #[test]
    fn an_inconclusive_run_does_not_pass() {
        assert_eq!(outcome(true, false), "INCONCLUSIVE");
        assert_eq!(outcome(true, true), "PASS");
        assert_eq!(outcome(false, true), "FAIL");
        assert_eq!(outcome(false, false), "FAIL");
        assert_eq!(exit_status(true, false), 2);
        assert_eq!(exit_status(false, false), 1);
        assert_eq!(exit_status(true, true), 0);
        // The LIVE fixture has no IDs or rows, so it fails outright.
        let v = verdict(&live_records(), 30, false);
        assert_eq!(v["conclusive"], json!(false));
        // Flat heap, steady IDs and rows, but a half hour with too few heap
        // floors: ok and not conclusive, the path the 30-minute default takes.
        let flat = records(
            30,
            sawtooth(|_| 1e5),
            |_| 8e6,
            |m| 5000.0 + ID_STEP * 25.0 * f64::from(m),
        );
        let v = verdict(&flat, 30, false);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["conclusive"], json!(false));
        assert_eq!(
            exit_status(v["ok"] == json!(true), v["conclusive"] == json!(true)),
            2
        );
    }

    #[test]
    fn the_mirror_rows_slope_is_zero_when_the_units_are_constant() {
        let r: Vec<Value> = records(30, |_| 1.0, |_| 1.0, |_| 1.0)
            .into_iter()
            .map(|mut r| {
                r["mirror_rows"] = json!(1200);
                r
            })
            .collect();
        let v = verdict(&r, 30, false);
        assert_eq!(v["object_rows"]["slope_per_hour"], json!(0.0));
        assert_eq!(v["object_rows"]["status"], json!("flat"));
    }

    #[test]
    fn ground_groups_are_told_from_the_rest() {
        assert!(is_ground_group("np-a-g12"));
        assert!(is_ground_group("np-c3-g1"));
        assert!(!is_ground_group("np-a-sg1"));
        assert!(!is_ground_group("np-a-a1"));
        assert!(!is_ground_group("np-a-st4"));
        assert!(!is_ground_group("np-a-g"));
        let s = json!({ "groups": ["np-a-g1", "np-a-sg1", "np-a-a1", "np-a-g2"], "ground": 20, "ships": 2, "air": 1, "statics": 3 });
        assert_eq!(ground_groups(&s), ["np-a-g1", "np-a-g2"]);
        assert_eq!(objects(&s), 26);
    }

    #[test]
    fn the_mirror_gives_the_largest_object_id() {
        let m = br#"{"rows":[{"object":5},{"object":4096},{"object":77}]}"#;
        assert_eq!(mirror_ids(m), (Some(4096), Some(3)));
        assert_eq!(mirror_ids(b"not json"), (None, None));
        assert_eq!(mirror_ids(br#"{"rows":[]}"#), (None, Some(0)));
    }

    #[test]
    fn the_heap_window_takes_the_lowest_and_highest_of_the_lines() {
        let lines = vec![
            json!({ "kib": 150.0, "min": 140.0, "max": 160.0 }),
            json!({ "kib": 170.0, "min": 130.0, "max": 180.0 }),
        ];
        let w = heap_window(&lines);
        assert_eq!(w["min_kib"], json!(130.0));
        assert_eq!(w["max_kib"], json!(180.0));
        assert_eq!(w["mean_kib"], json!(160.0));
        assert!(heap_window(&[])["min_kib"].is_null());
    }

    #[test]
    fn options_are_checked() {
        let o = parse_options(&[]).unwrap();
        assert_eq!((o.minutes, o.units), (30, 1200));
        let o = parse_options(&["--minutes", "10", "--units", "500"]).unwrap();
        assert_eq!((o.minutes, o.units), (10, 500));
        assert!(!o.control);
        assert!(parse_options(&["--minutes", "2"]).is_err());
        assert!(parse_options(&["--minutes", "180"]).is_ok());
        assert!(parse_options(&["--minutes", "181"]).is_err());
        let o = parse_options(&["--no-sidecar", "--minutes", "40"]).unwrap();
        assert!(o.control);
        assert_eq!(o.minutes, 40);
        assert!(
            parse_options(&["--minutes", "40", "--no-sidecar"])
                .unwrap()
                .control
        );
        assert!(parse_options(&["--no-sidecar", "--minutes"]).is_err());
        assert_eq!(o.collect_every, None);
        let o = parse_options(&["--collect-every", "5", "--no-sidecar"]).unwrap();
        assert_eq!((o.collect_every, o.control), (Some(5), true));
        assert!(parse_options(&["--collect-every", "0"]).is_err());
        assert!(parse_options(&["--collect-every"]).is_err());
        assert!(parse_options(&["--units", "10"]).is_err());
        assert!(parse_options(&["--minutes"]).is_err());
        assert!(parse_options(&["--bogus", "1"]).is_err());
        assert!(parse_options(&["--minutes", "x"]).is_err());
    }

    #[test]
    fn the_churn_snippet_names_its_groups_and_the_kill_mode() {
        let names = vec!["np-a-g1".to_owned(), "np-a-g2".to_owned()];
        let s = churn_source(&names, 25, true);
        assert!(s.contains("{ \"np-a-g1\", \"np-a-g2\" }"));
        assert!(s.contains("freed >= 25"));
        assert!(s.contains("if true then"));
        assert!(churn_source(&names, 25, false).contains("if false then"));
    }
}
