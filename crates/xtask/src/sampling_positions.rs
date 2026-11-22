//! The positions measure of the `sampling` live parts, and its re-judge of a
//! stored bundle.
//!
//! The target is one position per object a second. It is judged as the
//! median real-time interval between an object's successive samples over the
//! stall-free windows of the settled span, pooled over every interval of every
//! moving object (not a median of per-object medians), and no stall-free 10 s window may
//! report a position period over [`POSITION_WINDOW_S`]. A stall window is a
//! `sampling` report whose `gap_max_s` (the longest real time between two
//! consecutive DCS frames the agent pumped in the window) is over the target
//! period. Stall windows are reported, not judged, and so is every window
//! whose position period overlaps a stall's possible span: the window where
//! the stall begins as well as the one where it ends.
//!
//! The real time of a window and of a sample is the probe's read time, `t`
//! in milliseconds: the `sampling` LOG record ends its window, and a
//! `POSITIONS` record in the recording carries the time the probe read it
//! (the probe polls every millisecond).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// The median per-object position interval over stall-free windows passes at
/// this many seconds. The sweep is paced to exactly 1.000 s, so a single
/// reading passes or fails by chance. Medians across seven LIVE runs were
/// 0.977-1.017 s. The owner's measure, the working session's limit.
pub(crate) const POSITION_MEDIAN_S: f64 = 1.05;
/// No stall-free 10 s window's `position_s` passes this many seconds.
/// Stall-free window readings reached 1.14 s in LIVE runs (at 1,200 rows).
/// The owner's measure, the working session's limit.
pub(crate) const POSITION_WINDOW_S: f64 = 1.25;
/// A window with a gap between DCS frames over this many seconds (the target
/// period) is a stall window.
pub(crate) const STALL_GAP_S: f64 = 1.0;
/// Fewer intervals than this in the stall-free windows decide nothing.
const INTERVALS_MIN: usize = 20;
/// The nominal length of a report window, s.
const WINDOW_S: f64 = 10.0;

/// One `sampling` report with its real time.
#[derive(Clone, Debug)]
pub(crate) struct Win {
    pub start_ms: f64,
    pub end_ms: f64,
    pub window_s: f64,
    pub position_s: f64,
    pub gap_max_s: Option<f64>,
    pub frames: Option<f64>,
}

/// What a window is to the measure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Free,
    Stall,
    /// The window after a stall window.
    After,
    /// A window whose position period overlaps a stall's possible span, such
    /// as the one in which the stall began: counted as stalled.
    Overlap,
    /// No `gap_max_s` and no proxy allowed.
    Unknown,
}

/// The `sampling` reports in the probe's lines from line `from`, each with
/// the probe's read time `t` in milliseconds.
pub(crate) fn timed(lines: &[Value], from: usize) -> Vec<(f64, Value)> {
    lines
        .iter()
        .skip(from)
        .filter(|l| l["ev"] == "log" && l["msg"] == "sampling")
        .filter_map(|l| Some((l["t"].as_f64()?, l["data"].clone())))
        .collect()
}

pub(crate) fn windows(reports: &[(f64, Value)]) -> Vec<Win> {
    reports
        .iter()
        .map(|(t, r)| {
            let w = r["window_s"].as_f64().unwrap_or(WINDOW_S);
            Win {
                start_ms: t - w * 1000.0,
                end_ms: *t,
                window_s: w,
                position_s: r["position_s"].as_f64().unwrap_or(0.0),
                gap_max_s: r["gap_max_s"].as_f64(),
                frames: r["frames"].as_f64(),
            }
        })
        .collect()
}

/// A proxy marks a window with fewer frames than this share of the run's
/// median window as a stall window. Not a proof: a stall of a second costs a
/// tenth of a window's frames at 100 fps, so a short stall can be missed and a
/// slow window can be taken for one. LIVE stall windows held 5-60 % of the
/// usual frames.
const PROXY_FRAME_SHARE: f64 = 0.7;

/// Whether a window is a stall window, and whether a proxy said so. Without
/// `gap_max_s`, and with `proxy`, a window is a stall window when
/// - it holds fewer frames than it has seconds (the gaps ending in it sum to
///   at least its length, so one is over 1 s: proof);
/// - it runs over 1 s past its nominal 10 s (the frame that closed it ended a
///   gap that long: proof); or
/// - it holds fewer than [`PROXY_FRAME_SHARE`] of the median frames of a
///   window (not proof).
fn stalled(w: &Win, proxy: bool, median_frames: f64) -> Option<(bool, bool)> {
    match w.gap_max_s {
        Some(g) => Some((g > STALL_GAP_S, false)),
        None if proxy => {
            let few = w.frames.is_some_and(|f| f < w.window_s);
            let long = w.window_s > WINDOW_S + STALL_GAP_S;
            let drop = w
                .frames
                .is_some_and(|f| f < PROXY_FRAME_SHARE * median_frames);
            Some((few || long || drop, true))
        }
        None => None,
    }
}

/// The kind of every window. A window right after a stall window is
/// stall-affected: the reported period is the time between the last two
/// sweep completions, so the stall's gap carries into the next window's
/// reading. Returns the kinds and whether a proxy decided any of them.
pub(crate) fn classify(wins: &[Win], proxy: bool) -> (Vec<Kind>, bool) {
    let mut kinds = Vec::with_capacity(wins.len());
    let mut by_proxy = false;
    let mut prev_stall = false;
    let mut frames: Vec<f64> = wins.iter().filter_map(|w| w.frames).collect();
    frames.sort_by(f64::total_cmp);
    let median_frames = frames.get(frames.len() / 2).copied().unwrap_or(0.0);
    for w in wins {
        let kind = match stalled(w, proxy, median_frames) {
            None => Kind::Unknown,
            Some((s, p)) => {
                by_proxy |= p;
                if s {
                    Kind::Stall
                } else if prev_stall {
                    Kind::After
                } else {
                    Kind::Free
                }
            }
        };
        prev_stall = kind == Kind::Stall;
        kinds.push(kind);
    }
    // A gap counts in the window where it ends (`gap_max_s`), so the window
    // in which the stall began reads a position period that spans the stall
    // while its own gap is short (LIVE, 2026-11-22: a window at 3.03 s with
    // `gap_max_s` 0.02 before a window of 5 frames in 10.98 s). The stall's
    // gap ended somewhere in its window, so it lies within the window's
    // span widened by the gap before its start; a proxy, without
    // `gap_max_s`, cannot place the gap and takes the window's own span. Any
    // other window whose
    // position period, the `position_s` before its end, meets that span is
    // stalled too.
    let spans: Vec<(f64, f64)> = wins
        .iter()
        .zip(&kinds)
        .filter(|(_, k)| **k == Kind::Stall)
        .map(|(w, _)| {
            let gap = w.gap_max_s.unwrap_or(0.0);
            (w.start_ms - gap * 1000.0, w.end_ms)
        })
        .collect();
    for (w, kind) in wins.iter().zip(kinds.iter_mut()) {
        if matches!(*kind, Kind::Free | Kind::After) {
            let period = (w.end_ms - w.position_s.max(0.0) * 1000.0, w.end_ms);
            if spans.iter().any(|(a, b)| period.0 < *b && period.1 > *a) {
                *kind = Kind::Overlap;
            }
        }
    }
    (kinds, by_proxy)
}

/// The index of the first settled window: the one after the last window
/// that held births.
pub(crate) fn settled_from(reports: &[(f64, Value)]) -> usize {
    let born = |r: &Value| {
        r["births"].as_f64().unwrap_or(0.0) > 0.0 || r["rows_births"].as_f64().unwrap_or(0.0) > 0.0
    };
    reports
        .iter()
        .rposition(|(_, r)| born(r))
        .map_or(0, |i| i + 1)
}

/// The real-time intervals (start and end, ms) between an object's
/// successive `POSITIONS` records in a recording, for the objects in `ids`.
pub(crate) fn intervals(recording: &str, ids: &BTreeSet<u64>) -> Vec<(f64, f64)> {
    use nanoprobe_transport::layout::kind;
    use nanoprobe_transport::records::Positions;

    let mut last: BTreeMap<u64, f64> = BTreeMap::new();
    let mut out = Vec::new();
    for line in recording.lines() {
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if r["kind"].as_u64() != Some(kind::POSITIONS) {
            continue;
        }
        let (Some(t), Some(payload)) = (
            r["t"].as_f64(),
            r["payload"].as_str().and_then(super::unhex),
        ) else {
            continue;
        };
        let Ok(p) = Positions::decode(&payload) else {
            continue;
        };
        for id in p.object.into_iter().filter(|id| ids.contains(id)) {
            if let Some(before) = last.insert(id, t)
                && t > before
            {
                out.push((before, t));
            }
        }
    }
    out
}

/// The two judges and the stall report over the windows from `settled` on.
/// `label` ends each check's name (for example "at 2,000"). Returns the
/// checks and the detail they share.
pub(crate) fn judge(
    reports: &[(f64, Value)],
    settled: usize,
    samples: &[(f64, f64)],
    proxy: bool,
    label: &str,
) -> (Vec<Value>, Value) {
    let wins = windows(reports);
    let (kinds, by_proxy) = classify(&wins, proxy);
    let settled = settled.min(wins.len());
    let from = settled.saturating_sub(1);
    let unknown = kinds[from..]
        .iter()
        .filter(|k| **k == Kind::Unknown)
        .count();
    let in_span = &wins[settled..];
    let kinds_span = &kinds[settled..];
    let free: Vec<&Win> = in_span
        .iter()
        .zip(kinds_span)
        .filter(|(_, k)| **k == Kind::Free)
        .map(|(w, _)| w)
        .collect();
    let stall_list: Vec<Value> = in_span
        .iter()
        .zip(kinds_span)
        .filter(|(_, k)| matches!(**k, Kind::Stall | Kind::Overlap))
        .map(|(w, k)| {
            json!({"position_s": w.position_s, "gap_max_s": w.gap_max_s,
                   "frames": w.frames, "window_s": w.window_s,
                   "overlaps_stall": *k == Kind::Overlap})
        })
        .collect();
    let after = kinds_span.iter().filter(|k| **k == Kind::After).count();
    // The windows that spoil an interval.
    let excluded: Vec<&Win> = wins
        .iter()
        .zip(&kinds)
        .filter(|(_, k)| {
            matches!(
                **k,
                Kind::Stall | Kind::After | Kind::Overlap | Kind::Unknown
            )
        })
        .map(|(w, _)| w)
        .collect();
    let (lo, hi) = (
        in_span.first().map_or(f64::INFINITY, |w| w.start_ms),
        in_span.last().map_or(f64::NEG_INFINITY, |w| w.end_ms),
    );
    let total = samples.len();
    let mut kept: Vec<f64> = samples
        .iter()
        .filter(|(a, b)| *a >= lo && *b <= hi)
        .filter(|(a, b)| !excluded.iter().any(|w| *a < w.end_ms && *b > w.start_ms))
        .map(|(a, b)| (b - a) / 1000.0)
        .collect();
    kept.sort_by(f64::total_cmp);
    let median = kept.get(kept.len() / 2).copied().unwrap_or(0.0);
    let worst = free.iter().map(|w| w.position_s).fold(0.0f64, f64::max);
    let missing = unknown > 0;
    let detail = json!({
        "windows": in_span.len(), "stall_free": free.len(),
        "stall_windows": stall_list, "after_stall_windows": after,
        "intervals": total, "intervals_kept": kept.len(),
        "median_s": median, "worst_window_position_s": worst,
        "limits_s": {"median": POSITION_MEDIAN_S, "window": POSITION_WINDOW_S,
                     "stall_gap": STALL_GAP_S},
        "judged_by_proxy": by_proxy,
        "gap_max_s_missing_in": unknown,
    });
    let reason = format!("key missing: gap_max_s absent from {unknown} report(s); {detail}");
    let judge1 = json!({
        "check": format!("median position interval <= {POSITION_MEDIAN_S} s over stall-free windows{label}"),
        "ok": !missing && kept.len() >= INTERVALS_MIN && median > 0.0 && median <= POSITION_MEDIAN_S,
        "detail": if missing { Value::String(reason.clone()) } else { detail.clone() },
    });
    let judge2 = json!({
        "check": format!("no stall-free window's position period over {POSITION_WINDOW_S} s{label}"),
        "ok": !missing && !free.is_empty() && worst <= POSITION_WINDOW_S,
        "detail": if missing { Value::String(reason) } else { detail.clone() },
    });
    let report = json!({
        "check": format!("stall windows{label} reported, not judged"),
        "ok": true,
        "detail": detail,
    });
    (vec![judge1, judge2, report], detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report ending at second `end_s`, 10 s long.
    fn report(end_s: f64, position: f64, gap: Option<f64>) -> (f64, Value) {
        let mut r = json!({"window_s": 10.0, "position_s": position, "frames": 600,
                           "births": 0, "rows_births": 0});
        if let Some(g) = gap {
            r["gap_max_s"] = json!(g);
        }
        (end_s * 1000.0, r)
    }

    /// `n` objects' intervals of `period` s, starting at second `from`.
    fn samples(from: f64, to: f64, period: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for k in 0..30 {
            let mut t = from + k as f64 * 0.01;
            while t + period <= to {
                out.push((t * 1000.0, (t + period) * 1000.0));
                t += period;
            }
        }
        out
    }

    fn ok(checks: &[Value]) -> (bool, bool) {
        (
            checks[0]["ok"] == json!(true),
            checks[1]["ok"] == json!(true),
        )
    }

    fn free_windows(n: usize, position: f64) -> Vec<(f64, Value)> {
        (1..=n)
            .map(|i| report(10.0 * i as f64, position, Some(0.05)))
            .collect()
    }

    #[test]
    fn the_median_passes_at_its_limit_and_fails_over_it() {
        let reports = free_windows(5, 1.0);
        let (c, _) = judge(&reports, 0, &samples(0.0, 50.0, 1.05), false, "");
        assert_eq!(ok(&c), (true, true));
        let (c, _) = judge(&reports, 0, &samples(0.0, 50.0, 1.06), false, "");
        assert_eq!(ok(&c), (false, true));
    }

    #[test]
    fn a_stall_free_window_passes_at_its_limit_and_fails_over_it() {
        let mut reports = free_windows(5, 1.0);
        reports[2].1["position_s"] = json!(1.25);
        let (c, _) = judge(&reports, 0, &samples(0.0, 50.0, 1.0), false, "");
        assert_eq!(ok(&c), (true, true));
        reports[2].1["position_s"] = json!(1.26);
        let (c, _) = judge(&reports, 0, &samples(0.0, 50.0, 1.0), false, "");
        assert_eq!(ok(&c), (true, false));
    }

    #[test]
    fn a_stall_window_and_the_next_are_reported_and_not_judged() {
        let mut reports = free_windows(6, 1.0);
        // The stall window and the one after it read long; so do the
        // intervals that lie in them.
        reports[1] = report(20.0, 10.9, Some(10.9));
        reports[2] = report(30.0, 5.0, Some(0.1));
        let mut s = samples(0.0, 10.0, 1.0);
        s.extend(samples(10.0, 30.0, 6.0));
        s.extend(samples(30.0, 60.0, 1.0));
        let (c, d) = judge(&reports, 0, &s, false, "");
        assert_eq!(ok(&c), (true, true), "{d}");
        // The stall of 10.9 s may have begun before its window: the window
        // before it is stalled too.
        assert_eq!(d["stall_windows"].as_array().map(Vec::len), Some(2));
        assert_eq!(d["after_stall_windows"], json!(1));
        assert_eq!(d["stall_free"], json!(3));
        // The same readings in a window with a small gap fail the window judge.
        reports[1] = report(20.0, 10.9, Some(0.2));
        let (c, _) = judge(&reports, 0, &s, false, "");
        assert_eq!(ok(&c), (true, false));
    }

    /// The live shape of 2026-11-22: a window reads 3.03 s with a 0.02 s gap
    /// because the stall that follows began in it; the next window holds the
    /// gap. Both edges of the stall are stalled: the window where it began
    /// and the one where it ended. A window whose period ends before the
    /// stall could have begun stays free and is judged, and so is a later
    /// window whose period starts after the stall.
    #[test]
    fn the_windows_where_a_stall_begins_and_ends_are_both_stalled() {
        let mut reports = free_windows(7, 1.0);
        reports[2] = report(30.0, 3.03, Some(0.02));
        reports[3] = report(40.0, 14.0, Some(3.03));
        let s = samples(0.0, 70.0, 1.0);
        let kinds = classify(&windows(&reports), false).0;
        assert_eq!(
            kinds,
            [
                Kind::Free,
                Kind::Free,
                Kind::Overlap,
                Kind::Stall,
                Kind::After,
                Kind::Free,
                Kind::Free
            ]
        );
        let (c, d) = judge(&reports, 0, &s, false, "");
        assert_eq!(ok(&c), (true, true), "{d}");
        assert_eq!(d["stall_windows"].as_array().map(Vec::len), Some(2));
        assert_eq!(d["stall_free"], json!(4));
        // A long period in a window before the stall's possible span is
        // judged: 1.26 s ending 3.03 s before the stall window starts.
        let mut early = reports.clone();
        early[1] = report(20.0, 1.26, Some(0.02));
        assert_eq!(classify(&windows(&early), false).0[1], Kind::Free);
        assert_eq!(ok(&judge(&early, 0, &s, false, "").0), (true, false));
        // A later window whose period reaches back into the stall's span is
        // stalled; one whose period starts after the stall is judged.
        let mut late = reports.clone();
        late[5] = report(60.0, 25.0, Some(0.02));
        assert_eq!(classify(&windows(&late), false).0[5], Kind::Overlap);
        late[5] = report(60.0, 1.26, Some(0.02));
        assert_eq!(classify(&windows(&late), false).0[5], Kind::Free);
        assert_eq!(ok(&judge(&late, 0, &s, false, "").0), (true, false));
    }

    #[test]
    fn a_report_without_gap_max_s_fails_with_the_key_missing() {
        let mut reports = free_windows(5, 1.0);
        reports[3].1.as_object_mut().map(|o| o.remove("gap_max_s"));
        let (c, _) = judge(&reports, 0, &samples(0.0, 50.0, 1.0), false, "");
        assert_eq!(ok(&c), (false, false));
        assert!(
            c[0]["detail"]
                .as_str()
                .is_some_and(|d| d.contains("key missing"))
        );
    }

    #[test]
    fn a_proxy_finds_stalls_by_frames_and_by_window_length() {
        let mut reports = free_windows(5, 1.0);
        for r in &mut reports {
            r.1.as_object_mut().map(|o| o.remove("gap_max_s"));
        }
        reports[1].1["frames"] = json!(9);
        reports[3].1["window_s"] = json!(11.2);
        let (kinds, used) = classify(&windows(&reports), true);
        assert!(used);
        assert_eq!(
            kinds,
            [
                Kind::Free,
                Kind::Stall,
                // Its period meets the long window that follows.
                Kind::Overlap,
                Kind::Stall,
                Kind::After
            ]
        );
        let (c, d) = judge(&reports, 0, &samples(0.0, 50.0, 1.0), true, "");
        assert_eq!(ok(&c), (true, true));
        assert_eq!(d["judged_by_proxy"], json!(true));
    }

    #[test]
    fn the_settled_span_starts_after_the_last_births() {
        let mut reports = free_windows(5, 1.0);
        reports[1].1["rows_births"] = json!(104);
        assert_eq!(settled_from(&reports), 2);
        assert_eq!(settled_from(&free_windows(3, 1.0)), 0);
    }
}
