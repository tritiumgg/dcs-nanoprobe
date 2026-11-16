//! A tier that reads a row only on its turn reads every row once in its
//! `every` passes, whatever the batch sizes, and the slow sweep's period is
//! the time its rows took.

use super::*;
use std::collections::HashMap;

/// Slack on a row's gap between reads, in passes. A pump runs several
/// batches, so the pass a read belongs to is known only to within the passes
/// that pump completed: the old read's bound and the new one's each add up
/// to a few.
const SLACK_PASSES: u64 = 2;

fn rows_table(rows: u64) -> (Sampler, Rec, World) {
    let (mut s, rec, mut w) = table(rows);
    // Nothing moves, so no position changes the rows' state.
    for o in w.objects.values_mut() {
        o.speed = 0.0;
    }
    s.set_direct(false);
    (s, rec, w)
}

/// Pump every `step_ms` of made-up time, with batches of 2 to 40 readable
/// rows, until `tier`'s pass count reaches `passes`. Returns the longest gap
/// in passes between two reads of one row, the slowest row's gap in seconds,
/// the rows that were never read, and the pumps made.
fn longest_gap(
    s: &mut Sampler,
    rec: &mut Rec,
    w: &mut World,
    tier: Tier,
    passes: u64,
) -> (u64, f64, usize, usize) {
    let i = pass_of(tier).unwrap();
    let step = Duration::from_millis(50);
    let mut t = rec.began;
    // The pass range of each row's last read.
    let mut last: HashMap<u64, (u64, Instant)> = HashMap::new();
    let (mut gap, mut gap_s, mut pumps) = (0u64, 0.0f64, 0usize);
    while s.passes[i] < passes && pumps < 200_000 {
        t += step;
        s.max_batch = 2 + (pumps * 7) % 39;
        let before = s.passes[i];
        rec.records.clear();
        one_pump(s, rec, w, t);
        let after = s.passes[i];
        let read = match tier {
            Tier::Vitals => vitals_read(rec),
            _ => positions_read(rec),
        };
        for o in read {
            if let Some((lo, at)) = last.insert(o, (before, t)) {
                gap = gap.max(after.saturating_sub(lo));
                gap_s = gap_s.max(t.saturating_duration_since(at).as_secs_f64());
            }
        }
        pumps += 1;
    }
    let unread = w.objects.keys().filter(|o| !last.contains_key(o)).count();
    (gap, gap_s, unread, pumps)
}

/// Every row's life is read once in `VITALS_EVERY` passes, with
/// batches that end inside a lap at every size.
#[test]
fn the_slow_vitals_sweep_reads_every_row_in_every_cycle() {
    let every = VITALS_EVERY.unwrap();
    let (mut s, mut rec, mut w) = rows_table(1200);
    let (gap, _, unread, pumps) = longest_gap(&mut s, &mut rec, &mut w, Tier::Vitals, 5 * every);
    assert!(
        s.passes[1] >= 5 * every,
        "{pumps} pumps made {} passes",
        s.passes[1]
    );
    eprintln!("vitals: longest gap {gap} passes in {pumps} pumps");
    assert_eq!(unread, 0, "rows never read");
    assert!(
        gap <= every + SLACK_PASSES,
        "a row waited {gap} passes between reads, over {every} + {SLACK_PASSES}"
    );
}

/// An idle unit's position is read once in `IDLE_EVERY` passes.
#[test]
fn the_idle_positions_turn_reads_every_row_in_every_cycle() {
    let (mut s, mut rec, mut w) = rows_table(1200);
    for row in s.rows.iter_mut().flatten() {
        row.unmoved = IDLE_AFTER;
    }
    s.idle_units = 1200;
    let (gap, _, unread, pumps) =
        longest_gap(&mut s, &mut rec, &mut w, Tier::Positions, 12 * IDLE_EVERY);
    assert!(
        s.passes[0] >= 12 * IDLE_EVERY,
        "{pumps} pumps made {} passes",
        s.passes[0]
    );
    eprintln!("positions: longest gap {gap} passes in {pumps} pumps");
    assert_eq!(unread, 0, "rows never read");
    assert!(
        gap <= IDLE_EVERY + SLACK_PASSES,
        "a row waited {gap} passes between reads, over {IDLE_EVERY} + {SLACK_PASSES}"
    );
}

/// The rows that are readable in `every` consecutive passes are every row,
/// once each: a cycle of passes visits each row once.
#[test]
fn a_cycle_of_passes_reads_each_row_once() {
    let every = VITALS_EVERY.unwrap();
    let (mut s, _, _) = rows_table(1200);
    let mut seen = vec![0u32; s.rows.len()];
    for pass in 0..every {
        s.passes[1] = 7 * every + pass;
        for p in 0..s.rows.len() {
            if let Some(slot) = s.readable(Tier::Vitals, p) {
                seen[slot as usize] += 1;
            }
        }
    }
    let present = (0..s.rows.len() as u32)
        .filter(|&i| s.present(i).is_some())
        .count();
    assert_eq!(present, 1200);
    for (slot, n) in seen.iter().enumerate() {
        assert_eq!(
            *n,
            u32::from(s.present(slot as u32).is_some()),
            "slot {slot}"
        );
    }
}

/// The slow sweep's reported period is the time its passes took, and every
/// row was read in it.
#[test]
fn the_slow_period_is_the_time_every_row_took() {
    let every = VITALS_EVERY.unwrap();
    let (mut s, mut rec, mut w) = rows_table(1200);
    let (_, gap_s, unread, _) = longest_gap(&mut s, &mut rec, &mut w, Tier::Vitals, 5 * every);
    assert_eq!(unread, 0);
    let reported = s.slow_period;
    assert!(reported > 0.0, "no period reported");
    // No row waited longer than the period the sweep reports, within a
    // pass's time.
    assert!(
        gap_s <= reported * 1.25 + 1.0,
        "a row waited {gap_s} s, the sweep reports {reported} s"
    );
}

/// A table so small that no row is readable in some passes still completes
/// every lap: each row's slow read comes within `every` passes, and the
/// pass count keeps moving.
#[test]
fn a_small_slow_sweep_completes_laps_with_nothing_due_from_the_head() {
    let every = VITALS_EVERY.unwrap();
    let (mut s, mut rec, mut w) = rows_table(5);
    let (gap, _, unread, pumps) = longest_gap(&mut s, &mut rec, &mut w, Tier::Vitals, 4 * every);
    assert!(
        s.passes[1] >= 4 * every,
        "{pumps} pumps made {} passes",
        s.passes[1]
    );
    assert_eq!(unread, 0, "rows never read");
    assert!(
        gap <= every + SLACK_PASSES,
        "a row waited {gap} passes between reads, over {every} + {SLACK_PASSES}"
    );
}

/// The same for idle positions of a few rows.
#[test]
fn a_small_idle_positions_sweep_completes_laps_with_nothing_due_from_the_head() {
    let (mut s, mut rec, mut w) = rows_table(5);
    for row in s.rows.iter_mut().flatten() {
        row.unmoved = IDLE_AFTER;
    }
    s.idle_units = 5;
    let (gap, _, unread, pumps) =
        longest_gap(&mut s, &mut rec, &mut w, Tier::Positions, 4 * IDLE_EVERY);
    assert!(
        s.passes[0] >= 4 * IDLE_EVERY,
        "{pumps} pumps made {} passes",
        s.passes[0]
    );
    assert_eq!(unread, 0, "rows never read");
    assert!(
        gap <= IDLE_EVERY + SLACK_PASSES,
        "a row waited {gap} passes between reads, over {IDLE_EVERY} + {SLACK_PASSES}"
    );
}
