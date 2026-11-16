//! The live tenth part at k = 1 in the host model: a 2,000-birth fill, then
//! an enumeration that runs past its period, at the live costs of the
//! sampler-live-costs run (LIVE, `sampling` LOG of the tenth part, DCS
//! 2.9.31.29434 dedicated server under Wine 10.0, 2026-11-15):
//!
//! | Figure | Live | Model |
//! |---|---|---|
//! | Frames a second | 112 | 112 |
//! | Birth, `us_births` over `rows_births` | 23-33 µs | 25 µs |
//! | Enumeration item, `us_enumeration` over `rows_enumeration` | 5.5-7.8 µs, 16.6 µs in a window with a 607 µs step | 7 µs, and 120 µs every 100 items |
//! | Enumeration step, `enum_step_max_us` | 44-240 µs, once 607 µs | 120 µs |
//! | Subscribed row with attitude | 25-45 µs | 32 µs |
//! | Vitals row | 1.4-1.6 µs | 1.5 µs |
//! | Position row in the sweep, sweep's fixed part | 0.9-1.4 µs, 4-15 µs | 1.0 µs, 10 µs |
//! | The call, `xcall_seen_us` | 37-48 µs | 42 µs |
//! | Batch fixed part, close | 3-10 µs, 1-3 µs | 4 µs, 1.5 µs |
//! | System-call frames | 160-200 µs a second | 170 µs |
//! | Frame's fixed work, `us_fixed` over frames | 1.9-2.9 µs | 2.5 µs |
//!
//! Twenty units subscribed with attitude at 1 s, four groups in five halted.
//! Live, the fill took the first 90 s and one enumeration then ran past its
//! period (`enumeration_s` 145 s); vitals read 0 rows in two minutes and one
//! window passed the pump bound. The model, over 300 s: in the 110 s of the
//! fill and the overdue enumeration, 9 vitals rows of 2,892 due and
//! subscribed rows at 2.04 s; vitals at 17.4 s as a rate in the 190 s after;
//! 23 of 30 windows past the pump bound, by up to 25 %.

use super::*;

/// The cap against regression on the subscribed period in the fill's and the
/// overdue enumeration's windows, s: main reads 2.04 s there, the ruled gate
/// 1.72-1.90 s. Vitals there read almost no row at the saturated budget, the
/// ruled accepted behavior, and are reported only.
const CAP_SUB_S: f64 = 2.5;

/// One report window of [`fill_run`].
struct Window {
    end: f64,
    secs: f64,
    rate: f64,
    bound: f64,
    births: bool,
    due: f64,
    swept: u64,
    sub: u64,
}

/// What the fill and the enumerations after it showed.
struct FillRun {
    /// Vitals rows due and read by the cursor over the run.
    due: f64,
    swept: u64,
    /// Subscribed rows read.
    sub: u64,
    /// The run's length, s.
    secs: f64,
    /// Every report window: its end and length, s from the start, pumps a
    /// second, the pump bound, whether births waited in it, its vitals rows
    /// due and read by the cursor, and its subscribed rows.
    windows: Vec<Window>,
    /// Frames over 250 µs with no dear enumeration step in them: a
    /// `coalition.getGroups` is one DCS call the clock cannot split.
    over_250: usize,
    /// The mean agent share.
    share: f64,
    /// Seconds from the start until no birth waited.
    fill_s: Option<f64>,
    /// Each enumeration's begin and end, s from the start.
    spans: Vec<(f64, Option<f64>)>,
}

fn fill_run(secs: usize) -> FillRun {
    let fps = 112.0;
    let (syscall, call, pre) = (170.0, 42.0, 2.5);
    let mut s = Sampler::new(Rates::default(), 0.07);
    let mut rec = Rec::new(0.003, 1e6 / fps);
    let mut w = world(1960, 40, 20);
    w.sweep = Some((10.0, 1.0));
    halt_four_in_five(&mut w);
    w.batch_us = 4.0;
    w.close_us = 1.5;
    w.birth_us = Some(25.0);
    w.enum_item_us = Some(7.0);
    w.enum_step_us = Some(120.0);
    w.tier_us[Tier::Subscribed.index()] = Some(32.0);
    w.tier_us[Tier::Vitals.index()] = Some(1.5);
    s.set_campaign(1);
    for i in 1..=20u64 {
        w.objects.get_mut(&(1000 + i)).unwrap().name = unit_name(i);
    }
    // The first births are the subscribed units.
    run_x(&mut s, &mut rec, &mut w, 3.0, fps, syscall, call, pre);
    rec.began += Duration::from_secs_f64(1.0 / fps);
    assert!((1001..=1020).all(|o| s.slot_of(o).is_some()));
    s.subscribe(1.0, true, &(1..=20).collect::<Vec<_>>());
    let start = rec.began;
    let mut r = FillRun {
        due: 0.0,
        swept: 0,
        sub: 0,
        secs: 0.0,
        windows: Vec::new(),
        over_250: 0,
        share: 0.0,
        fill_s: None,
        spans: Vec::new(),
    };
    let steps = secs / 10;
    let mut waited = true;
    for step in 0..steps {
        let run = run_x(&mut s, &mut rec, &mut w, 10.0, fps, syscall, call, pre);
        rec.began += Duration::from_secs_f64(1.0 / fps);
        r.over_250 += run.over_250_plain;
        for rep in &run.reports {
            r.secs += rep.window_s;
            r.due += rep.vitals_due;
            r.swept += rep.vitals_swept;
            let sub = rep.spend.tier_rows[Tier::Subscribed.index()];
            r.sub += sub;
            // Births waited in the window when they waited at its start or
            // its end.
            let births = waited || rep.births > 0;
            waited = rep.births > 0;
            r.windows.push(Window {
                end: r.secs,
                secs: rep.window_s,
                rate: rep.pumps as f64 / rep.window_s,
                bound: 5.0
                    + rep.spend.pump_work_us / rep.window_s / (3.0 * rep.xcall_seen_us.max(1.0)),
                births,
                due: rep.vitals_due,
                swept: rep.vitals_swept,
                sub,
            });
        }
        r.share += run.share / steps as f64;
        if r.fill_s.is_none() && w.births.is_empty() {
            r.fill_s = Some((step * 10 + 10) as f64);
        }
    }
    let at = |x: Instant| x.saturating_duration_since(start).as_secs_f64().round();
    r.spans = w
        .enum_spans
        .iter()
        .map(|&(b, e)| (at(b), e.map(at)))
        .collect();
    r
}

/// Open items 1 and 3 of the sampler-live-costs run, as ruled on
/// 2026-11-15 (the quorum on the sampler during a fill): during a
/// 2,000-birth fill and an enumeration past its period, at live costs,
/// every window keeps the pump bound and no frame passes 250 µs. Such a
/// window is not steady: its vitals and subscribed rates are reported, and
/// held only to a cap against regression. Over the steady windows after it,
/// at least two minutes, vitals keep their band as a rate. Before the gate
/// waited for three calls' worth of work, 19 of 20 windows passed the bound,
/// by up to 25 %.
///
/// It fails on main: 19 of 20 windows past the bound and steady-window
/// vitals at 17.4 s. The ruled gate, the larger of 3 × (`x` + `f`) and
/// `x` + 3 × `xcall_seen` + `f` + the close, at most `reach`, passes the
/// bound here but leaves steady-window vitals at 14-22 s, and breaks
/// `after_an_enumeration_vitals_repays_its_rows`,
/// `a_stall_and_an_enumeration_in_one_cycle_are_repaid` (subscribed 1.05 s
/// against 0.98-1.00 s) and, with the close's mean and the call's estimate
/// for an unmeasured call, the live-attitude test's vitals band at offset
/// 0.37 s (6.58 s). An open item of that ruling.
#[test]
#[ignore = "open item: the ruled gate breaks existing sampler tests; fails on main"]
fn at_live_costs_a_fill_and_an_overdue_enumeration_keep_the_pump_bound() {
    let r = fill_run(300);
    let target = Rates::default().vitals_s;
    // A window is not steady while births wait or an enumeration has run
    // past its period: from its begin plus the period to its end.
    let period = Rates::default().enumeration_s;
    let overdue = |w: &Window| {
        let from = w.end - w.secs;
        r.spans.iter().any(|&(b, e)| {
            let e = e.unwrap_or(f64::INFINITY);
            b + period < w.end && e > from && e - b > period
        })
    };
    let (mut fill, mut steady) = ((0.0, 0u64, 0u64, 0.0), (0.0, 0u64, 0.0));
    for w in &r.windows {
        if w.births || overdue(w) {
            fill.0 += w.due;
            fill.1 += w.swept;
            fill.2 += w.sub;
            fill.3 += w.secs;
        } else {
            steady.0 += w.due;
            steady.1 += w.swept;
            steady.2 += w.secs;
        }
    }
    let rate = |due: f64, swept: u64| target * due / swept.max(1) as f64;
    let fill_vitals = rate(fill.0, fill.1);
    let fill_sub = 20.0 * fill.3 / fill.2.max(1) as f64;
    let steady_vitals = rate(steady.0, steady.1);
    let over: Vec<String> = r
        .windows
        .iter()
        .filter(|w| w.rate > w.bound)
        .map(|w| format!("{:.1}/{:.1} at {:.0} s", w.rate, w.bound, w.end))
        .collect();
    let (worst, bound) = r
        .windows
        .iter()
        .map(|w| (w.rate, w.bound))
        .fold((0.0, 1.0), |a, b| if b.0 / b.1 > a.0 / a.1 { b } else { a });
    let first_end = r.spans.first().and_then(|s| s.1);
    eprintln!(
        "fill and overdue enumeration: in {:.0} s of fill and overdue windows vitals \
         {fill_vitals:.2} s ({} of {:.0} rows), subscribed {fill_sub:.3} s; in {:.0} s of \
         steady windows vitals {steady_vitals:.2} s; share {:.5}; fill done by {:?} s; \
         enumerations {:?}; nearest the pump bound {worst:.2} against {bound:.2}; frames \
         over 250 µs {}",
        fill.3, fill.1, fill.0, steady.2, r.share, r.fill_s, r.spans, r.over_250
    );
    assert!(over.is_empty(), "past the pump bound: {over:?}");
    assert!(r.share <= 0.003, "share {}", r.share);
    assert_eq!(r.over_250, 0, "frames over 250 µs");
    // No later than on main before the gate's change: the fill done by
    // 80 s, the first enumeration ended by 102 s.
    assert!(r.fill_s.is_some_and(|f| f <= 80.0), "fill {:?}", r.fill_s);
    assert!(
        first_end.is_some_and(|e| e <= 102.0),
        "first enumeration {:?}",
        r.spans
    );
    assert!(fill_sub <= CAP_SUB_S, "subscribed {fill_sub} s in the fill");
    assert!(steady.2 >= 120.0, "{} s of steady windows", steady.2);
    assert!(
        (4.5..=6.5).contains(&steady_vitals),
        "vitals {steady_vitals} s as a rate in the steady windows"
    );
}
