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
//! window passed the pump bound. The model reads 341 vitals rows of 6,224
//! due in 200 s (a 91 s period as a rate), subscribed rows at 1.61 s, and 19
//! of 20 windows past the pump bound, by up to 25 %.

use super::*;

/// What the fill and the enumerations after it showed.
struct FillRun {
    /// Vitals rows due and read by the cursor over the run.
    due: f64,
    swept: u64,
    /// Subscribed rows read.
    sub: u64,
    /// The run's length, s.
    secs: f64,
    /// Every report window: pumps a second and the pump bound.
    windows: Vec<(f64, f64)>,
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
        share: 0.0,
        fill_s: None,
        spans: Vec::new(),
    };
    let steps = secs / 10;
    for step in 0..steps {
        let run = run_x(&mut s, &mut rec, &mut w, 10.0, fps, syscall, call, pre);
        rec.began += Duration::from_secs_f64(1.0 / fps);
        for rep in &run.reports {
            r.secs += rep.window_s;
            r.due += rep.vitals_due;
            r.swept += rep.vitals_swept;
            r.sub += rep.spend.tier_rows[Tier::Subscribed.index()];
            let rate = rep.pumps as f64 / rep.window_s;
            let bound =
                5.0 + rep.spend.pump_work_us / rep.window_s / (3.0 * rep.xcall_seen_us.max(1.0));
            r.windows.push((rate, bound));
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

/// Open items 1 and 3 of the sampler-live-costs run: during a 2,000-birth
/// fill and an enumeration past its period, at live costs, vitals keep
/// their band as a rate, subscribed rows their period, and every window the
/// pump bound. It fails on main.
#[test]
#[ignore = "open items 1 and 3 of the sampler-live-costs run: fails on main"]
fn at_live_costs_a_fill_and_an_overdue_enumeration_keep_vitals_and_the_pump_bound() {
    let r = fill_run(200);
    let vitals = Rates::default().vitals_s * r.due / r.swept.max(1) as f64;
    let sub = 20.0 * r.secs / r.sub.max(1) as f64;
    let over: Vec<String> = r
        .windows
        .iter()
        .filter(|(rate, bound)| rate > bound)
        .map(|(rate, bound)| format!("{rate:.1}/{bound:.1}"))
        .collect();
    eprintln!(
        "fill and overdue enumeration: vitals {vitals:.2} s ({} of {:.0} rows), subscribed \
         {sub:.3} s, share {:.5}, fill done by {:?} s, enumerations {:?}, past the pump \
         bound in {} of {} windows {over:?}",
        r.swept,
        r.due,
        r.share,
        r.fill_s,
        r.spans,
        over.len(),
        r.windows.len()
    );
    assert!(r.share <= 0.003, "share {}", r.share);
    assert!((4.5..=6.5).contains(&vitals), "vitals {vitals} s as a rate");
    assert!((0.995..=1.206).contains(&sub), "subscribed {sub} s");
    assert!(over.is_empty(), "past the pump bound: {over:?}");
}
