//! The verdict of a long soak run: for each measured series, a slope, its
//! noise, a verdict, the difference from a control run, and the value
//! projected to 24 hours.
//!
//! The input is one JSON record per sampling interval. Every record has
//! `t_s` (seconds since the run began) and `segment` (an integer; a DCS
//! restart during the run starts a new one), and values at JSON pointers
//! such as `/dcs/rss_kb`. A value may be missing or null in any record.
//!
//! The longest segment is fitted, because a restart resets DCS's memory and
//! a fit across the reset reads as a fall or a jump. Times inside it count
//! from its first record (the uptime), so a warm-up applies to each segment.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Fit, fit};

/// How a series is judged.
pub enum Limit {
    /// The slope (the platform's, when the series has a control) may be at
    /// most this, in the series' unit per unit of x (x is hours unless the
    /// series names an `x` pointer).
    Slope(f64),
    /// As `Slope`, with the limit this share of the series' mean (of the
    /// fitted points).
    ShareOfMean(f64),
    /// Growth by design or bounded by a cap: no slope limit. Judged by its
    /// shape (not rising) and by its projection against `cap`.
    Shape,
}

/// One measured series and the rule it is held to.
pub struct Def {
    pub key: &'static str,
    /// JSON pointer of y in a record.
    pub pointer: &'static str,
    /// JSON pointer of x; `None` means time, in hours (`t_s / 3600`).
    pub x: Option<&'static str>,
    pub unit: &'static str,
    pub limit: Limit,
    /// Whether the series is also measured in the control run (a run without
    /// the agent), so the platform's slope is the soak's less the control's.
    pub control: bool,
    /// The largest value the series may reach when projected to 24 hours;
    /// `None` for no cap. `Options::caps` can add or replace one by key.
    pub cap: Option<f64>,
    /// One sentence: why this limit.
    pub why: &'static str,
}

/// The series a soak measures. The limits are the smallest a run can resolve
/// or the owner's targets; each `why` gives the reason.
pub const DEFS: &[Def] = &[
    Def {
        key: "heap_server_floor_kib",
        pointer: "/heap/server_floor_kib",
        x: None,
        unit: "KiB",
        limit: Limit::Slope(1024.0),
        control: true,
        cap: None,
        why: "The server state's Lua heap after a forced full collect; DCS's own floor grew about 7.2 and 7.8 MB an hour under churn in two paired runs, which differ by 0.6 MB an hour, so 1 MiB an hour is the smallest platform slope a pair of runs can show.",
    },
    Def {
        key: "heap_hook_floor_kib",
        pointer: "/heap/hook_floor_kib",
        x: None,
        unit: "KiB",
        limit: Limit::Slope(1024.0),
        control: true,
        cap: None,
        why: "The hook state's Lua heap after a forced full collect, where the agent's loader and the eval hook run; same limit as the server state.",
    },
    Def {
        key: "dcs_rss_kb",
        pointer: "/dcs/rss_kb",
        x: None,
        unit: "kB",
        limit: Limit::ShareOfMean(0.02),
        control: true,
        cap: None,
        why: "DCS's resident memory; paired runs with and without the agent differed by up to 1.6 % of the mean an hour in either direction, so 2 % is the smallest difference one pair resolves.",
    },
    Def {
        key: "container_working_set_kb",
        pointer: "/container/working_set_kb",
        x: None,
        unit: "kB",
        limit: Limit::Shape,
        control: true,
        cap: None,
        why: "What the container's memory limit counts; with no swap, reaching the limit kills DCS, so the 24-hour projection must stay under 80 % of it.",
    },
    Def {
        key: "dcs_fds",
        pointer: "/dcs/fds",
        x: None,
        unit: "descriptors",
        limit: Limit::Slope(1.0),
        control: true,
        cap: None,
        why: "Open file descriptors of the DCS process; a leak of one an hour reaches no limit in 24 hours and anything faster is a defect.",
    },
    Def {
        key: "dcs_threads",
        pointer: "/dcs/threads",
        x: None,
        unit: "threads",
        limit: Limit::Slope(1.0),
        control: true,
        cap: None,
        why: "Threads of the DCS process; the agent starts none.",
    },
    Def {
        key: "wineserver_fds",
        pointer: "/wineserver/fds",
        x: None,
        unit: "descriptors",
        limit: Limit::Slope(1.0),
        control: true,
        cap: None,
        why: "Under Wine a Windows handle is an object in the Wine server, so a leaked handle shows here or in its memory.",
    },
    Def {
        key: "wineserver_rss_kb",
        pointer: "/wineserver/rss_kb",
        x: None,
        unit: "kB",
        limit: Limit::Slope(1024.0),
        control: true,
        cap: None,
        why: "The Wine server's resident memory, where Windows handles and their objects live.",
    },
    Def {
        key: "sidecar_rss_kb",
        pointer: "/sidecar/rss_kb",
        x: None,
        unit: "kB",
        limit: Limit::Slope(16384.0),
        control: false,
        cap: Some(1_048_576.0),
        why: "The sidecar's resident memory; the idempotency index keeps every changing call for 24 hours by design, and 16 MiB an hour allows that while a leak of 100 bytes per driver step (500 steps a second) is 172 MiB an hour.",
    },
    Def {
        key: "sidecar_fds",
        pointer: "/sidecar/fds",
        x: None,
        unit: "descriptors",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "Open file descriptors of the sidecar.",
    },
    Def {
        key: "sidecar_threads",
        pointer: "/sidecar/threads",
        x: None,
        unit: "threads",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "Threads of the sidecar; a backup worker comes and goes.",
    },
    Def {
        key: "ids_per_spawned_unit",
        pointer: "/ids/max_object_id",
        x: Some("/ids/spawned_units"),
        unit: "object IDs per unit",
        limit: Limit::Slope(281.6),
        control: false,
        cap: None,
        why: "DCS steps its object IDs by 256 per object; 10 % above that per spawned unit is IDs spent on something else.",
    },
    Def {
        key: "agent_rows",
        pointer: "/agent/rows",
        x: None,
        unit: "rows",
        limit: Limit::ShareOfMean(0.01),
        control: false,
        cap: None,
        why: "The agent's object table; the churn holds the population constant, so the table is flat.",
    },
    Def {
        key: "agent_pending_destroy",
        pointer: "/agent/pending_destroy",
        x: None,
        unit: "entries",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "The agent's set of destroys waiting for DCS to let the objects go.",
    },
    Def {
        key: "agent_pending_deaths",
        pointer: "/agent/pending_deaths",
        x: None,
        unit: "entries",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "The agent's memory of gone rows waiting for a late death event.",
    },
    Def {
        key: "agent_loadouts_waiting",
        pointer: "/agent/loadouts_waiting",
        x: None,
        unit: "entries",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "Loadout reads the agent still owes.",
    },
    Def {
        key: "agent_births_waiting",
        pointer: "/agent/births",
        x: None,
        unit: "entries",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "Births the shim holds for the sampler.",
    },
    Def {
        key: "enum_step_max_us",
        pointer: "/agent/enum_step_max_us",
        x: None,
        unit: "µs",
        limit: Limit::Shape,
        control: false,
        cap: Some(2000.0),
        why: "The slowest step of the enumeration grew from 484 to 1,138 µs in 90 minutes and then flattened; projected to 24 hours it must stay under the 2 ms the platform allows itself for commanded work in a frame, and must not be rising at the end.",
    },
    Def {
        key: "birth_step_max_us",
        pointer: "/agent/birth_step_max_us",
        x: None,
        unit: "µs",
        limit: Limit::Shape,
        control: false,
        cap: Some(2000.0),
        why: "The slowest birth step, judged as the enumeration's.",
    },
    Def {
        key: "agent_share_percent",
        pointer: "/agent/share_percent",
        x: None,
        unit: "% of real time",
        limit: Limit::Shape,
        control: false,
        cap: Some(0.3),
        why: "The agent's overhead per window; the owner's target is a mean of at most 0.3 %, which the 24-hour projection must hold.",
    },
    Def {
        key: "frames_over_250_percent",
        pointer: "/agent/frames_over_250_percent",
        x: None,
        unit: "% of frames",
        limit: Limit::Shape,
        control: false,
        cap: Some(1.0),
        why: "The share of frames in which the agent took over 250 µs; the owner's target of a 99th percentile of at most 250 µs is at most 1 %.",
    },
    Def {
        key: "store_bytes",
        pointer: "/store/store_bytes",
        x: None,
        unit: "bytes",
        limit: Limit::Shape,
        control: false,
        cap: None,
        why: "The campaign store file; with the population constant its growth is the journal's, by design until the journal retention deletes.",
    },
    Def {
        key: "store_wal_bytes",
        pointer: "/store/store_wal_bytes",
        x: None,
        unit: "bytes",
        limit: Limit::Slope(1_048_576.0),
        control: false,
        cap: None,
        why: "The store's write-ahead log, which SQLite checkpoints and reuses, so it holds its high-water mark.",
    },
    Def {
        key: "events_bytes",
        pointer: "/store/events_bytes",
        x: None,
        unit: "bytes",
        limit: Limit::Shape,
        control: false,
        cap: None,
        why: "The event history file, which grows with every event by design until the event retention deletes.",
    },
    Def {
        key: "events_wal_bytes",
        pointer: "/store/events_wal_bytes",
        x: None,
        unit: "bytes",
        limit: Limit::Slope(1_048_576.0),
        control: false,
        cap: None,
        why: "The event history's write-ahead log.",
    },
    Def {
        key: "journal_rows",
        pointer: "/store/journal_rows",
        x: None,
        unit: "rows",
        limit: Limit::Shape,
        control: false,
        cap: None,
        why: "Journal entries; the growth per hour is the figure the journal retention is set from.",
    },
    Def {
        key: "event_rows",
        pointer: "/store/event_rows",
        x: None,
        unit: "rows",
        limit: Limit::Shape,
        control: false,
        cap: None,
        why: "Events recorded.",
    },
    Def {
        key: "world_groups",
        pointer: "/world/groups",
        x: None,
        unit: "groups",
        limit: Limit::Slope(1.0),
        control: false,
        cap: None,
        why: "Groups in the campaign; the churn removes and creates the same number, so a slope is entities the platform failed to remove.",
    },
    Def {
        key: "store_entity_rows",
        pointer: "/store/entity_rows",
        x: None,
        unit: "rows",
        limit: Limit::Shape,
        control: false,
        cap: None,
        why: "Entity rows the export holds; the store keeps a removed entity for good (spec/world.md, Removal), so the rows grow with every removal by design, and the growth per hour is the figure.",
    },
    Def {
        key: "sidecar_cpu_percent",
        pointer: "/sidecar/cpu_percent",
        x: None,
        unit: "% of a core",
        limit: Limit::Shape,
        control: false,
        cap: Some(50.0),
        why: "The sidecar's CPU; its driver is one thread whose step walks every group the store holds, removed ones included, so the 24-hour projection must leave it half a core.",
    },
];

/// Run-time settings of a verdict.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Seconds at the start of a segment that are warm-up and are not fitted.
    pub warmup_s: f64,
    /// The control's warm-up, seconds.
    pub control_warmup_s: f64,
    /// Block length, seconds (see [`fitted_points`]).
    pub block_s: f64,
    /// Caps by series key, known only at run time (for example 80 % of the
    /// container's memory limit).
    pub caps: BTreeMap<String, f64>,
}

/// A fit needs this many points before its slope and noise mean anything:
/// three give a standard error with one degree of freedom, a fourth makes it
/// a little less fragile.
const MIN_POINTS: usize = 4;
/// Raw samples per block below which a series is fitted as raw points.
const MIN_PER_BLOCK: f64 = 3.0;
/// The noise of a slope is this many standard errors.
const SIGMAS: f64 = 2.0;
/// A slope that increases by less than this share of itself from the first
/// half to the second is not rising. A count that grows with the churn (the
/// journal's rows) has almost no noise, so the noise test alone calls a
/// change of 3 % rising (the rehearsal: 52,700 against 54,200 rows an hour).
/// Under 10 % the 24-hour projection moves by under 10 %, which no cap here
/// is set that close to.
const RISING_SHARE: f64 = 0.10;

/// One record of the fitted segment, with its uptime.
struct Rec<'a> {
    uptime_s: f64,
    value: &'a Value,
}

/// A sample of a series: x, y and the uptime it was taken at.
#[derive(Clone, Copy)]
struct Pt {
    x: f64,
    y: f64,
    uptime_s: f64,
}

/// A fit with the figures a projection needs.
struct Line {
    fit: Fit,
    mean_x: f64,
    min_x: f64,
    max_x: f64,
}

impl Line {
    fn noise(&self) -> f64 {
        SIGMAS * self.fit.stderr
    }

    fn at(&self, x: f64) -> f64 {
        self.fit.mean_y + self.fit.slope * (x - self.mean_x)
    }
}

/// A fit of at least [`MIN_POINTS`] points.
fn line(points: &[(f64, f64)]) -> Option<Line> {
    if points.len() < MIN_POINTS {
        return None;
    }
    let f = fit(points)?;
    let n = points.len() as f64;
    Some(Line {
        fit: f,
        mean_x: points.iter().map(|p| p.0).sum::<f64>() / n,
        min_x: points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
        max_x: points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
    })
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// The segments of a run: a summary of each, and the records of the one with
/// the longest span of `t_s` (the later one on a tie), with their uptimes.
fn segments(records: &[Value]) -> (Vec<Value>, Option<(i64, Vec<Rec<'_>>)>) {
    let mut by_segment: BTreeMap<i64, Vec<(f64, &Value)>> = BTreeMap::new();
    for r in records {
        let (Some(t), Some(s)) = (
            r.get("t_s").and_then(Value::as_f64),
            r.get("segment").and_then(Value::as_i64),
        ) else {
            continue;
        };
        by_segment.entry(s).or_default().push((t, r));
    }
    let mut summary = Vec::new();
    let mut best: Option<(i64, f64, f64)> = None;
    for (s, rs) in &by_segment {
        let from = rs.iter().map(|r| r.0).fold(f64::INFINITY, f64::min);
        let to = rs.iter().map(|r| r.0).fold(f64::NEG_INFINITY, f64::max);
        summary.push(json!({ "segment": s, "from_s": from, "to_s": to, "records": rs.len() }));
        if best.is_none_or(|b| to - from >= b.2 - b.1) {
            best = Some((*s, from, to));
        }
    }
    let used = best.map(|(s, from, _)| {
        let recs = by_segment[&s]
            .iter()
            .map(|(t, r)| Rec {
                uptime_s: t - from,
                value: r,
            })
            .collect();
        (s, recs)
    });
    (summary, used)
}

/// The samples of a series with an uptime past `after_s` and a numeric x and
/// y. For a time series x is the uptime in hours.
fn raw_points(def: &Def, recs: &[Rec<'_>], after_s: f64) -> Vec<Pt> {
    recs.iter()
        .filter(|r| r.uptime_s > after_s)
        .filter_map(|r| {
            let y = r.value.pointer(def.pointer)?.as_f64()?;
            let x = match def.x {
                Some(p) => r.value.pointer(p)?.as_f64()?,
                None => r.uptime_s / 3600.0,
            };
            Some(Pt {
                x,
                y,
                uptime_s: r.uptime_s,
            })
        })
        .collect()
}

/// The points a series is fitted through, and whether they are blocks.
///
/// Minute samples of process memory are autocorrelated, so an ordinary
/// least-squares standard error over raw samples understates the noise. When
/// the series has at least [`MIN_PER_BLOCK`] samples per non-empty block, one
/// point per block is fitted: the mean x and the median y of the block's
/// samples. Blocks are consecutive windows of `block_s` of uptime that start
/// where the warm-up ends. A sparse series, such as heap floors read every
/// 15 minutes, is fitted as raw points.
fn fitted_points(points: &[Pt], start_s: f64, block_s: f64) -> (Vec<(f64, f64)>, bool) {
    let raw = || points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    if block_s <= 0.0 || points.is_empty() {
        return (raw(), false);
    }
    let mut blocks: BTreeMap<i64, Vec<&Pt>> = BTreeMap::new();
    for p in points {
        blocks
            .entry(((p.uptime_s - start_s) / block_s).floor() as i64)
            .or_default()
            .push(p);
    }
    if (points.len() as f64) / (blocks.len() as f64) < MIN_PER_BLOCK {
        return (raw(), false);
    }
    let fitted = blocks
        .values()
        .map(|b| {
            let x = b.iter().map(|p| p.x).sum::<f64>() / b.len() as f64;
            let mut ys: Vec<f64> = b.iter().map(|p| p.y).collect();
            (x, median(&mut ys))
        })
        .collect();
    (fitted, true)
}

/// The slope limit of a series, given the mean of the points it is judged on.
fn slope_limit(limit: &Limit, mean: f64) -> Option<f64> {
    match limit {
        Limit::Slope(v) => Some(*v),
        Limit::ShareOfMean(s) => Some(s * mean),
        Limit::Shape => None,
    }
}

/// The early and late fits: the first and second half of the points by x.
/// Each half needs 3 points; otherwise both are `None`.
fn halves(points: &[(f64, f64)]) -> (Option<Fit>, Option<Fit>) {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let k = sorted.len() / 2;
    match (fit(&sorted[..k]), fit(&sorted[k..])) {
        (Some(e), Some(l)) => (Some(e), Some(l)),
        _ => (None, None),
    }
}

/// The platform's figures: the control's fit, the soak's fit over the same
/// uptime window, and the soak's fit over the uptime after that window, which
/// the control does not reach.
struct Platform {
    control: Line,
    matched: Line,
    after: Option<Line>,
}

fn platform(
    def: &Def,
    soak: &[Rec<'_>],
    control: &[Rec<'_>],
    options: &Options,
) -> Option<Platform> {
    let control_pts = raw_points(def, control, options.control_warmup_s);
    let (fitted, _) = fitted_points(&control_pts, options.control_warmup_s, options.block_s);
    let control_line = line(&fitted)?;
    let window_end = control_pts
        .iter()
        .map(|p| p.uptime_s)
        .fold(f64::NEG_INFINITY, f64::max);
    let (matched_pts, after_pts): (Vec<Pt>, Vec<Pt>) =
        raw_points(def, soak, options.control_warmup_s)
            .into_iter()
            .partition(|p| p.uptime_s <= window_end);
    let (fitted, _) = fitted_points(&matched_pts, options.control_warmup_s, options.block_s);
    let matched = line(&fitted)?;
    let (fitted, _) = fitted_points(&after_pts, window_end, options.block_s);
    Some(Platform {
        control: control_line,
        matched,
        after: line(&fitted),
    })
}

fn limit_json(limit: &Limit, per_x: Option<f64>) -> Value {
    let (kind, value) = match limit {
        Limit::Slope(v) => ("slope", Some(*v)),
        Limit::ShareOfMean(s) => ("share_of_mean", Some(*s)),
        Limit::Shape => ("shape", None),
    };
    json!({ "kind": kind, "value": value, "per_x": per_x })
}

/// The verdict of one series. `soak` is the fitted segment's records and
/// `control` the control's, when there is one.
fn judge_series(
    def: &Def,
    soak: &[Rec<'_>],
    control: Option<&[Rec<'_>]>,
    options: &Options,
) -> Value {
    let samples = raw_points(def, soak, options.warmup_s);
    let (points, blocks) = fitted_points(&samples, options.warmup_s, options.block_s);
    let cap = options.caps.get(def.key).copied().or(def.cap);
    let header = |per_x: Option<f64>| {
        json!({
            "unit": def.unit,
            "pointer": def.pointer,
            "why": def.why,
            "limit": limit_json(&def.limit, per_x),
            "fitted": if blocks { "blocks" } else { "points" },
            "n": points.len(),
            "samples": samples.len(),
            "cap": cap,
        })
    };
    let Some(whole) = line(&points) else {
        let mut v = header(None);
        v["verdict"] = json!("no data");
        v["rising"] = json!(false);
        v["control_missing"] = json!(false);
        v["projected_ok"] = Value::Null;
        return v;
    };

    let (early, late) = halves(&points);
    let plat = if def.control {
        control.and_then(|c| platform(def, soak, c, options))
    } else {
        None
    };
    let control_missing = def.control && plat.is_none();

    // The limit and the pair judged against it. The mean is that of the
    // points the pair's slope was fitted through.
    let judged_mean = plat
        .as_ref()
        .map_or(whole.fit.mean_y, |p| p.matched.fit.mean_y);
    let limit = slope_limit(&def.limit, judged_mean);
    let (slope, noise) = match &plat {
        Some(p) => (
            p.matched.fit.slope - p.control.fit.slope,
            SIGMAS * p.matched.fit.stderr.hypot(p.control.fit.stderr),
        ),
        None => (whole.fit.slope, whole.noise()),
    };

    // A series is rising when its slope increases beyond what noise explains
    // and by more than [`RISING_SHARE`] of the early slope, and the late
    // slope is clear of the limit, or of the early slope when the series has
    // no limit. For a series the control also has, DCS's own slope is the
    // control's: the late slope must be clear of that slope plus the limit,
    // and without a control nothing says what DCS's own slope is, so the
    // series is not called rising.
    let rising = match (&early, &late) {
        _ if control_missing => false,
        (Some(e), Some(l)) => {
            let gain = l.slope - e.slope;
            let accelerates =
                gain > SIGMAS * e.stderr.hypot(l.stderr) && gain > RISING_SHARE * e.slope.abs();
            let late_low = l.slope - SIGMAS * l.stderr;
            let clear = match (slope_limit(&def.limit, whole.fit.mean_y), &plat) {
                (Some(lim), Some(p)) => {
                    late_low > p.control.fit.slope + p.control.noise() + lim.max(0.0)
                }
                (Some(lim), None) => late_low > lim.max(0.0),
                (None, _) => late_low > e.slope + SIGMAS * e.stderr,
            };
            accelerates && clear
        }
        _ => false,
    };

    // After the control's window the control has no samples, and DCS's own
    // slope is taken to stay at the control's: the soak's slope there, less
    // the control's, is judged against the same limit. A platform leak that
    // starts after the window is growing, whatever the window showed.
    let after = plat.as_ref().and_then(|p| {
        let a = p.after.as_ref()?;
        Some((
            a.fit.slope - p.control.fit.slope,
            SIGMAS * a.fit.stderr.hypot(p.control.fit.stderr),
        ))
    });
    let grows_after = match (limit, after) {
        (Some(lim), Some((excess, n))) => excess - n > lim,
        _ => false,
    };

    let verdict = match limit {
        None => "by design",
        Some(_) if grows_after => "growing",
        Some(lim) if slope + noise <= lim => "flat",
        Some(lim) if slope - noise > lim => {
            let settled = plat.is_none()
                && late
                    .as_ref()
                    .is_some_and(|l| l.slope + SIGMAS * l.stderr <= lim);
            if settled { "settled" } else { "growing" }
        }
        Some(_) => "unresolved",
    };

    let projection = def.x.is_none().then(|| {
        let ahead = 24.0 - whole.max_x;
        (
            whole.at(whole.max_x) + whole.fit.slope * ahead,
            whole.at(whole.max_x) + whole.fit.slope * ahead + whole.noise() * ahead,
        )
    });
    let projected_ok = match (cap, projection) {
        (Some(c), Some((_, upper))) => json!(upper <= c),
        _ => Value::Null,
    };

    let mut v = header(limit);
    let obj = v.as_object_mut().expect("the header is an object");
    let mut put = |k: &str, x: Value| {
        obj.insert(k.to_string(), x);
    };
    put("mean", json!(whole.fit.mean_y));
    put("slope", json!(whole.fit.slope));
    put("noise", json!(whole.noise()));
    put("early_slope", json!(early.map(|f| f.slope)));
    put("late_slope", json!(late.map(|f| f.slope)));
    put("late_noise", json!(late.map(|f| SIGMAS * f.stderr)));
    put("rising", json!(rising));
    put("after_window_slope", json!(after.map(|a| a.0)));
    put("after_window_noise", json!(after.map(|a| a.1)));
    put(
        "control_slope",
        json!(plat.as_ref().map(|p| p.control.fit.slope)),
    );
    put(
        "control_noise",
        json!(plat.as_ref().map(|p| p.control.noise())),
    );
    put(
        "matched_slope",
        json!(plat.as_ref().map(|p| p.matched.fit.slope)),
    );
    put("platform_slope", json!(plat.as_ref().map(|_| slope)));
    put("platform_noise", json!(plat.as_ref().map(|_| noise)));
    put(
        "basis",
        json!(if plat.is_some() { "platform" } else { "whole" }),
    );
    put("control_missing", json!(control_missing));
    put("verdict", json!(verdict));
    put("first", json!(whole.at(whole.min_x)));
    put("last", json!(whole.at(whole.max_x)));
    put("projected_24h", json!(projection.map(|p| p.0)));
    put("projected_24h_upper", json!(projection.map(|p| p.1)));
    put("projected_ok", projected_ok);
    v
}

/// The keys of the series for which `pick` is true, in the order of `defs`.
fn keys_where(defs: &[Def], series: &Value, pick: impl Fn(&Value) -> bool) -> Vec<&'static str> {
    defs.iter()
        .filter(|d| series.get(d.key).is_some_and(&pick))
        .map(|d| d.key)
        .collect()
}

/// The verdict over a soak's records and, when there is one, its control's.
///
/// `extend` counts the unresolved and the rising series only. A series with
/// no data, and one that lacks its control, are for the caller to count: they
/// are in `no_data` and under each series' `control_missing`.
pub fn judge(defs: &[Def], soak: &[Value], control: Option<&[Value]>, options: &Options) -> Value {
    let (summary, used) = segments(soak);
    let control_used = control.and_then(|c| segments(c).1);
    let (segment_used, recs) = match &used {
        Some((s, r)) => (Some(*s), r.as_slice()),
        None => (None, [].as_slice()),
    };
    let control_recs = control_used.as_ref().map(|(_, r)| r.as_slice());

    let mut series = serde_json::Map::new();
    for d in defs {
        series.insert(
            d.key.to_string(),
            judge_series(d, recs, control_recs, options),
        );
    }
    let series = Value::Object(series);

    let verdict_is = |want: &'static str| move |s: &Value| s["verdict"] == want;
    let growing = keys_where(defs, &series, verdict_is("growing"));
    let unresolved = keys_where(defs, &series, verdict_is("unresolved"));
    let no_data = keys_where(defs, &series, verdict_is("no data"));
    let rising = keys_where(defs, &series, |s| s["rising"] == true);
    let over_cap = keys_where(defs, &series, |s| s["projected_ok"] == false);
    json!({
        "segments": summary,
        "segment_used": segment_used,
        "fitted_from_s": options.warmup_s,
        "fitted_to_s": recs.iter().map(|r| r.uptime_s).fold(0.0, f64::max),
        "control": control_used.is_some(),
        "ok": growing.is_empty() && over_cap.is_empty(),
        "extend": !unresolved.is_empty() || !rising.is_empty(),
        "growing": growing,
        "unresolved": unresolved,
        "rising": rising,
        "over_cap": over_cap,
        "no_data": no_data,
        "series": series,
    })
}

/// A number with 3 significant digits; `-` for null or a non-number.
fn sig3(v: &Value) -> String {
    let Some(x) = v.as_f64().filter(|x| x.is_finite()) else {
        return "-".to_string();
    };
    if x == 0.0 {
        return "0".to_string();
    }
    let exp = x.abs().log10().floor() as i32;
    if exp >= 2 {
        let step = 10f64.powi(exp - 2);
        format!("{:.0}", (x / step).round() * step)
    } else {
        format!("{:.*}", (2 - exp) as usize, x)
    }
}

/// A figure with its noise, `slope ± noise`.
fn with_noise(v: &Value, slope: &str, noise: &str) -> String {
    if v[slope].is_null() {
        "-".to_string()
    } else {
        format!("{} ± {}", sig3(&v[slope]), sig3(&v[noise]))
    }
}

/// The verdict as a Markdown table, one row per series: key, unit, verdict,
/// slope ± noise per hour, control slope, platform slope ± noise, limit,
/// value at the end, projected 24 h (upper), cap.
pub fn table(verdict: &Value) -> String {
    let mut out = String::from(
        "| Series | Unit | Verdict | Slope ± noise per hour | Control slope | Platform slope ± noise | Limit | End | Projected 24 h (upper) | Cap |\n\
         |---|---|---|---|---|---|---|---|---|---|\n",
    );
    let Some(series) = verdict.get("series").and_then(Value::as_object) else {
        return out;
    };
    for (key, s) in series {
        let projected = if s["projected_24h"].is_null() {
            "-".to_string()
        } else {
            format!(
                "{} ({})",
                sig3(&s["projected_24h"]),
                sig3(&s["projected_24h_upper"])
            )
        };
        let verdict_text = match (s["verdict"].as_str().unwrap_or("-"), s["rising"] == true) {
            (v, true) => format!("{v}, rising"),
            (v, false) => v.to_string(),
        };
        let limit = if s["limit"]["per_x"].is_null() {
            "-".to_string()
        } else {
            sig3(&s["limit"]["per_x"])
        };
        out.push_str(&format!(
            "| {key} | {} | {verdict_text} | {} | {} | {} | {limit} | {} | {projected} | {} |\n",
            s["unit"].as_str().unwrap_or("-"),
            with_noise(s, "slope", "noise"),
            sig3(&s["control_slope"]),
            with_noise(s, "platform_slope", "platform_noise"),
            sig3(&s["last"]),
            sig3(&s["cap"]),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic noise in [-1, 1): a linear congruential generator.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    const HOUR: f64 = 3600.0;

    fn def(limit: Limit, control: bool, cap: Option<f64>) -> Def {
        Def {
            key: "v",
            pointer: "/v/y",
            x: None,
            unit: "u",
            limit,
            control,
            cap,
            why: "test",
        }
    }

    fn options() -> Options {
        Options {
            warmup_s: 600.0,
            control_warmup_s: 600.0,
            block_s: 600.0,
            caps: BTreeMap::new(),
        }
    }

    fn rec(t: f64, segment: i64, y: f64) -> Value {
        json!({ "t_s": t, "segment": segment, "v": { "y": y } })
    }

    /// One record a minute for `hours` hours from `t0`, with y from the hour.
    fn run(t0: f64, hours: f64, segment: i64, mut f: impl FnMut(f64) -> f64) -> Vec<Value> {
        let n = (hours * 60.0) as usize;
        (0..=n)
            .map(|i| {
                let h = i as f64 / 60.0;
                rec(t0 + h * HOUR, segment, f(h))
            })
            .collect()
    }

    fn one(defs: &[Def], soak: &[Value], control: Option<&[Value]>, o: &Options) -> Value {
        judge(defs, soak, control, o)["series"]["v"].clone()
    }

    fn near(a: &Value, b: f64, tol: f64) -> bool {
        a.as_f64().is_some_and(|a| (a - b).abs() <= tol)
    }

    #[test]
    fn a_flat_noisy_series_is_flat() {
        let mut n = Noise(1);
        let soak = run(0.0, 4.0, 0, |_| 100.0 + n.next());
        let v = one(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(v["verdict"], "flat", "{v}");
        assert_eq!(v["basis"], "whole");
    }

    #[test]
    fn a_leak_at_three_times_the_limit_is_growing() {
        let mut n = Noise(2);
        let soak = run(0.0, 4.0, 0, |h| 100.0 + 3.0 * h + n.next());
        let r = judge(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(r["series"]["v"]["verdict"], "growing");
        assert_eq!(r["growing"], json!(["v"]));
        assert_eq!(r["ok"], false);
    }

    #[test]
    fn a_noisy_short_series_at_the_limit_is_unresolved() {
        let mut n = Noise(3);
        let soak = run(0.0, 1.5, 0, |h| 100.0 + h + 20.0 * n.next());
        let r = judge(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(
            r["series"]["v"]["verdict"], "unresolved",
            "{}",
            r["series"]["v"]
        );
        assert_eq!(r["extend"], true);
        assert_eq!(r["ok"], true);
    }

    #[test]
    fn a_share_of_mean_limit_scales_with_the_mean() {
        let mut n = Noise(4);
        // 1.5 % of the mean an hour: under 2 %, over 1 %.
        let soak = run(0.0, 4.0, 0, |h| 1000.0 + 15.0 * h + n.next());
        let ok = one(
            &[def(Limit::ShareOfMean(0.02), false, None)],
            &soak,
            None,
            &options(),
        );
        let bad = one(
            &[def(Limit::ShareOfMean(0.01), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(ok["verdict"], "flat");
        assert_eq!(bad["verdict"], "growing");
    }

    #[test]
    fn dense_series_are_fitted_as_blocks_and_sparse_ones_as_points() {
        let soak = run(0.0, 4.0, 0, |h| 100.0 + h);
        let d = [def(Limit::Slope(1.0), false, None)];
        let dense = one(&d, &soak, None, &options());
        assert_eq!(dense["fitted"], "blocks");
        assert!(dense["n"].as_u64() < dense["samples"].as_u64());
        // One sample every 15 minutes against blocks of 30 minutes: 2 per block.
        let sparse: Vec<Value> = soak.iter().step_by(15).cloned().collect();
        let o = Options {
            block_s: 1800.0,
            ..options()
        };
        let v = one(&d, &sparse, None, &o);
        assert_eq!(v["fitted"], "points");
        assert_eq!(v["n"], v["samples"]);
    }

    #[test]
    fn block_noise_exceeds_raw_noise_for_an_autocorrelated_series() {
        let soak = run(0.0, 4.0, 0, |h| {
            100.0 + 5.0 * (h * std::f64::consts::TAU).sin()
        });
        let d = [def(Limit::Slope(1.0), false, None)];
        let v = one(&d, &soak, None, &options());
        assert_eq!(v["fitted"], "blocks");
        let raw: Vec<(f64, f64)> = soak
            .iter()
            .filter(|r| r["t_s"].as_f64().unwrap() > 600.0)
            .map(|r| {
                (
                    r["t_s"].as_f64().unwrap() / HOUR,
                    r["v"]["y"].as_f64().unwrap(),
                )
            })
            .collect();
        let raw_noise = 2.0 * fit(&raw).unwrap().stderr;
        assert!(
            v["noise"].as_f64().unwrap() > raw_noise,
            "{v} vs {raw_noise}"
        );
    }

    #[test]
    fn a_planted_leak_against_a_flat_control_is_growing_on_the_platform_basis() {
        let mut n = Noise(5);
        let soak = run(0.0, 4.0, 0, |h| 100.0 + 5.0 * h + n.next());
        let control = run(0.0, 4.0, 0, |_| 100.0 + n.next());
        let r = judge(
            &[def(Limit::Slope(1.0), true, None)],
            &soak,
            Some(&control),
            &options(),
        );
        let v = &r["series"]["v"];
        assert_eq!(v["basis"], "platform");
        assert_eq!(v["verdict"], "growing", "{v}");
        assert_eq!(v["control_missing"], false);
        assert_eq!(r["control"], true);
        assert!(near(&v["platform_slope"], 5.0, 0.5));
    }

    #[test]
    fn growth_present_in_both_soak_and_control_cancels() {
        let mut n = Noise(6);
        let soak = run(0.0, 4.0, 0, |h| 100.0 + 8.0 * h + n.next());
        let control = run(0.0, 4.0, 0, |h| 100.0 + 8.0 * h + n.next());
        let v = one(
            &[def(Limit::Slope(1.0), true, None)],
            &soak,
            Some(&control),
            &options(),
        );
        assert_eq!(v["basis"], "platform");
        assert_eq!(v["verdict"], "flat", "{v}");
        assert!(near(&v["slope"], 8.0, 0.5));
    }

    #[test]
    fn the_matched_fit_uses_only_the_soak_points_inside_the_control_window() {
        // The soak runs 8 hours and steepens after 4; the control runs 4.
        let soak = run(0.0, 8.0, 0, |h| {
            if h < 4.0 {
                100.0 + h
            } else {
                104.0 + 20.0 * (h - 4.0)
            }
        });
        let control = run(0.0, 4.0, 0, |_| 100.0);
        let v = one(
            &[def(Limit::Slope(1.0), true, None)],
            &soak,
            Some(&control),
            &options(),
        );
        assert!(near(&v["matched_slope"], 1.0, 0.05), "{v}");
        assert!(near(&v["control_slope"], 0.0, 0.05));
        assert!(v["slope"].as_f64().unwrap() > 5.0);
        // The window alone is within the limit; what follows it is not.
        assert!(near(&v["after_window_slope"], 20.0, 0.5), "{v}");
        assert_eq!(v["verdict"], "growing", "{v}");
    }

    #[test]
    fn a_leak_that_starts_after_the_controls_window_is_growing() {
        // DCS's own growth is 7 an hour in both runs. The soak follows its
        // 2-hour control and then leaks 3 an hour more, against a limit of 1.
        let d = [def(Limit::Slope(1.0), true, None)];
        let control = run(0.0, 2.0, 0, |h| 100.0 + 7.0 * h);
        let same = run(0.0, 8.0, 0, |h| 100.0 + 7.0 * h);
        let leaks = run(0.0, 8.0, 0, |h| 100.0 + 7.0 * h + 3.0 * (h - 2.0).max(0.0));
        let v = one(&d, &same, Some(&control), &options());
        assert_eq!(v["verdict"], "flat", "{v}");
        assert_eq!(v["rising"], false, "DCS's own slope is not a rise: {v}");
        assert!(near(&v["after_window_slope"], 0.0, 0.05), "{v}");
        let v = one(&d, &leaks, Some(&control), &options());
        assert!(near(&v["platform_slope"], 0.0, 0.05), "{v}");
        assert!(near(&v["after_window_slope"], 3.0, 0.05), "{v}");
        assert_eq!(v["verdict"], "growing", "{v}");
    }

    #[test]
    fn rising_is_judged_against_the_controls_slope_and_never_without_a_control() {
        let d = [def(Limit::Slope(1.0), true, None)];
        let control = run(0.0, 2.0, 0, |h| 100.0 + 7.0 * h);
        // DCS's own slope changes by a fifth between the halves: over the
        // limit of 1 as a raw slope, under the control's slope plus the limit.
        let own = run(0.0, 8.0, 0, |h| 100.0 + 6.0 * h + 1.2 * (h - 4.0).max(0.0));
        assert_eq!(one(&d, &own, Some(&control), &options())["rising"], false);
        // A late slope clear of the control's slope plus the limit is rising.
        let more = run(0.0, 8.0, 0, |h| 100.0 + 7.0 * h + 6.0 * (h - 4.0).max(0.0));
        assert_eq!(one(&d, &more, Some(&control), &options())["rising"], true);
        // Without the control nothing says what DCS's own slope is.
        let v = one(&d, &more, None, &options());
        assert_eq!(v["rising"], false, "{v}");
        assert_eq!(v["control_missing"], true);
    }

    #[test]
    fn a_control_series_without_control_records_is_judged_on_the_whole_run() {
        let mut n = Noise(7);
        let soak = run(0.0, 4.0, 0, |_| 100.0 + n.next());
        let v = one(
            &[def(Limit::Slope(1.0), true, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(v["basis"], "whole");
        assert_eq!(v["control_missing"], true);
        assert!(v["platform_slope"].is_null());
        assert_eq!(v["verdict"], "flat");
    }

    #[test]
    fn a_series_that_grows_early_and_stops_is_settled() {
        let mut n = Noise(8);
        let o = Options {
            warmup_s: 0.0,
            ..options()
        };
        let soak = run(0.0, 8.0, 0, |h| 100.0 + 10.0 * h.min(2.0) + 0.3 * n.next());
        let v = one(&[def(Limit::Slope(1.0), false, None)], &soak, None, &o);
        assert_eq!(v["verdict"], "settled", "{v}");
    }

    #[test]
    fn a_series_whose_slope_increases_is_rising_and_extends_the_run() {
        let mut n = Noise(9);
        let soak = run(0.0, 4.0, 0, |h| 100.0 + 2.0 * h * h + 0.2 * n.next());
        let r = judge(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(r["series"]["v"]["rising"], true, "{}", r["series"]["v"]);
        assert_eq!(r["rising"], json!(["v"]));
        assert_eq!(r["extend"], true);
    }

    #[test]
    fn a_shape_series_is_by_design_and_a_straight_line_is_not_rising() {
        let soak = run(0.0, 4.0, 0, |h| 1000.0 + 10.0 * h);
        let r = judge(
            &[def(Limit::Shape, false, Some(2000.0))],
            &soak,
            None,
            &options(),
        );
        let v = &r["series"]["v"];
        assert_eq!(v["verdict"], "by design");
        assert_eq!(v["rising"], false);
        assert_eq!(v["projected_ok"], true);
        assert_eq!(r["ok"], true);
    }

    #[test]
    fn a_shape_series_that_flattens_is_not_rising_and_one_that_accelerates_is() {
        let flat = run(0.0, 6.0, 0, |h| 450.0 + 700.0 * h.min(1.5) / 1.5);
        let accel = run(0.0, 6.0, 0, |h| 450.0 + 20.0 * h * h * h);
        let d = [def(Limit::Shape, false, None)];
        assert_eq!(one(&d, &flat, None, &options())["rising"], false);
        assert_eq!(one(&d, &accel, None, &options())["rising"], true);
    }

    #[test]
    fn a_projection_past_the_cap_fails_the_run() {
        let soak = run(0.0, 4.0, 0, |h| 1000.0 + 50.0 * h);
        let r = judge(
            &[def(Limit::Shape, false, Some(1500.0))],
            &soak,
            None,
            &options(),
        );
        // 1000 + 50 * 24 = 2200.
        assert!(near(&r["series"]["v"]["projected_24h"], 2200.0, 1e-6));
        assert_eq!(r["series"]["v"]["projected_ok"], false);
        assert_eq!(r["over_cap"], json!(["v"]));
        assert_eq!(r["ok"], false);
    }

    #[test]
    fn a_run_time_cap_replaces_the_cap_of_the_definition() {
        let soak = run(0.0, 4.0, 0, |h| 1000.0 + 50.0 * h);
        let mut o = options();
        o.caps.insert("v".to_string(), 3000.0);
        let r = judge(&[def(Limit::Shape, false, Some(1500.0))], &soak, None, &o);
        assert_eq!(r["series"]["v"]["cap"], 3000.0);
        assert_eq!(r["series"]["v"]["projected_ok"], true);
        // A cap for a series without one in its definition.
        let r = judge(&[def(Limit::Shape, false, None)], &soak, None, &o);
        assert_eq!(r["series"]["v"]["cap"], 3000.0);
    }

    #[test]
    fn the_projection_of_a_perfect_line_is_exact() {
        let soak = run(0.0, 4.0, 0, |h| 10.0 + 2.0 * h);
        let v = one(&[def(Limit::Shape, false, None)], &soak, None, &options());
        assert!(near(&v["projected_24h"], 58.0, 1e-6), "{v}");
        assert!(near(&v["projected_24h_upper"], 58.0, 1e-6));
        assert!(near(&v["slope"], 2.0, 1e-9));
        assert!(near(
            &v["first"],
            10.0 + 2.0 * (v["first"].as_f64().unwrap() - 10.0) / 2.0,
            1e-9
        ));
        assert!(near(&v["last"], 18.0, 0.05));
    }

    #[test]
    fn the_longest_segment_is_fitted_with_uptime_restarting_at_each_segment() {
        // A leaking first segment of 2 hours, then a restart: the second
        // segment runs 5 hours and starts low and flat.
        let mut soak = run(0.0, 2.0, 0, |h| 500.0 + 100.0 * h);
        soak.extend(run(2.0 * HOUR + 60.0, 5.0, 1, |_| 100.0));
        let r = judge(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(r["segment_used"], 1);
        assert_eq!(r["segments"].as_array().unwrap().len(), 2);
        assert_eq!(r["segments"][1]["from_s"], 2.0 * HOUR + 60.0);
        assert_eq!(r["series"]["v"]["verdict"], "flat");
        assert!(near(&r["fitted_to_s"], 5.0 * HOUR, 1e-6));
        assert!(near(&r["series"]["v"]["last"], 100.0, 1e-6));
    }

    #[test]
    fn the_later_segment_wins_a_tie() {
        let mut soak = run(0.0, 2.0, 0, |_| 1.0);
        soak.extend(run(HOUR * 3.0, 2.0, 1, |_| 2.0));
        let r = judge(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert_eq!(r["segment_used"], 1);
    }

    #[test]
    fn a_series_with_an_x_pointer_is_fitted_against_it_and_has_no_projection() {
        let d = Def {
            key: "ids",
            pointer: "/ids/max",
            x: Some("/ids/spawned"),
            limit: Limit::Slope(281.6),
            ..def(Limit::Shape, false, Some(1.0))
        };
        let soak: Vec<Value> = (0..300)
            .map(|i| {
                let spawned = f64::from(i) * 10.0;
                json!({ "t_s": f64::from(i) * 60.0, "segment": 0,
                        "ids": { "max": 5000.0 + 256.0 * spawned, "spawned": spawned } })
            })
            .collect();
        let r = judge(&[d], &soak, None, &options());
        let v = &r["series"]["ids"];
        assert_eq!(v["verdict"], "flat", "{v}");
        assert!(near(&v["slope"], 256.0, 1e-6));
        assert!(v["projected_24h"].is_null());
        assert!(v["projected_ok"].is_null());
        assert_eq!(r["ok"], true);
    }

    #[test]
    fn missing_and_null_values_are_skipped() {
        let mut soak = run(0.0, 4.0, 0, |h| 100.0 + 0.2 * h);
        let all = soak.len();
        for (i, r) in soak.iter_mut().enumerate() {
            match i % 5 {
                0 => r["v"]["y"] = Value::Null,
                1 => {
                    r.as_object_mut().unwrap().remove("v");
                }
                _ => {}
            }
        }
        let v = one(
            &[def(Limit::Slope(1.0), false, None)],
            &soak,
            None,
            &options(),
        );
        assert!(v["samples"].as_u64().unwrap() < all as u64 * 3 / 5 + 1);
        assert_eq!(v["verdict"], "flat", "{v}");
    }

    #[test]
    fn a_key_absent_everywhere_is_no_data_and_does_not_fail_the_run() {
        let soak = run(0.0, 4.0, 0, |h| 100.0 + h);
        let d = Def {
            key: "gone",
            pointer: "/gone",
            ..def(Limit::Slope(1.0), false, Some(1.0))
        };
        let r = judge(&[d], &soak, None, &options());
        assert_eq!(r["series"]["gone"]["verdict"], "no data");
        assert_eq!(r["no_data"], json!(["gone"]));
        assert_eq!(r["ok"], true);
        assert_eq!(r["extend"], false);
    }

    #[test]
    fn no_records_give_no_data_for_every_series() {
        let r = judge(DEFS, &[], None, &options());
        assert_eq!(r["segment_used"], Value::Null);
        assert_eq!(r["no_data"].as_array().unwrap().len(), DEFS.len());
        assert_eq!(r["ok"], true);
    }

    #[test]
    fn the_table_has_the_header_and_one_row_per_series() {
        let soak = run(0.0, 4.0, 0, |h| 1234.5678 + 0.2 * h);
        let defs = [
            def(Limit::Slope(1.0), false, None),
            Def {
                key: "w",
                pointer: "/w",
                ..def(Limit::Shape, false, None)
            },
        ];
        let t = table(&judge(&defs, &soak, None, &options()));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 2 + 2, "{t}");
        assert!(lines[0].starts_with("| Series |"));
        assert!(lines[1].starts_with("|---"));
        assert!(t.contains("| v | u | flat |"));
        assert!(t.contains("| w | u | no data |"));
    }

    #[test]
    fn numbers_print_with_three_significant_digits() {
        assert_eq!(sig3(&json!(1234.5678)), "1230");
        assert_eq!(sig3(&json!(12.3456)), "12.3");
        assert_eq!(sig3(&json!(0.012345)), "0.0123");
        assert_eq!(sig3(&Value::Null), "-");
    }

    #[test]
    fn every_definition_key_is_unique_and_every_pointer_starts_with_a_slash() {
        let mut keys = std::collections::BTreeSet::new();
        for d in DEFS {
            assert!(keys.insert(d.key), "duplicate key {}", d.key);
            assert!(d.pointer.starts_with('/'), "{}", d.key);
            assert!(d.x.is_none_or(|x| x.starts_with('/')), "{}", d.key);
            assert!(!d.why.is_empty());
        }
        assert_eq!(DEFS.len(), 30);
    }
}
