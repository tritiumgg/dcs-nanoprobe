//! The time from a unit's birth to its row, in the host model at live costs:
//! a materialization's spawn stream, one birth every second frame, as the
//! server test of S20 spawned (LIVE, docs/results/2026-11-17-s20-virtualization.md:
//! births at a median of one every 2 frames at about 112 frames a second).
//! The costs are those of `sampler_fill_tests.rs`.

use super::*;

/// What a spawn stream showed.
struct BindRun {
    /// Each streamed unit's time from its birth to its row, s, sorted.
    lags: Vec<f64>,
    /// Streamed units with no row at the end.
    unbound: usize,
    /// Streamed units whose `OBJECT` record was not written exactly once.
    not_once: usize,
    /// Seconds from the stream's start until no birth waited.
    drained_s: Option<f64>,
    /// Vitals rows due and read by the cursor in the 60 s after the births
    /// drained.
    due: f64,
    swept: u64,
    /// Loadout marks the pumps read, and those still waiting at the end.
    marks_read: u64,
    marks_waiting: usize,
    /// The mean agent share from the stream's start, and frames over 250 µs
    /// with no dear enumeration step in them.
    share: f64,
    over_250: usize,
}

impl BindRun {
    fn median(&self) -> f64 {
        self.lags.get(self.lags.len() / 2).copied().unwrap_or(0.0)
    }

    fn largest(&self) -> f64 {
        self.lags.last().copied().unwrap_or(0.0)
    }

    /// The vitals period as a rate in the 60 s after the births drained, s.
    fn vitals_s(&self) -> f64 {
        Rates::default().vitals_s * self.due / self.swept.max(1) as f64
    }
}

/// One spawn stream.
struct Case {
    /// Units in the table before the stream, and the units it spawns.
    present: u64,
    spawns: usize,
    /// One group in five moves, as in the live runs at 1,200 and 2,000
    /// units; else one in ten, as in the server test of S20.
    moving: bool,
    /// The pump resolves the newest birth first.
    newest_first: bool,
    /// Every birth leaves a loadout mark, as when each was a player's.
    marked: bool,
    /// The spawn's handler adds each unit's row, as `GROUP_SPAWN`'s does.
    /// Without it the units are born as a mission's own are, and wait for
    /// the births tier.
    handler: bool,
}

/// The object ID of the first streamed unit, above every ID of the table.
const FIRST_STREAMED: u64 = 500_000;

/// The case's units in the table, then its births at one every second frame,
/// and `secs` seconds from the stream's start.
fn bind_run(c: &Case, secs: usize) -> BindRun {
    let fps = 112.0;
    let (syscall, call, pre) = (170.0, 42.0, 2.5);
    let mut s = Sampler::new(Rates::default(), 0.07);
    let mut rec = Rec::new(0.003, 1e6 / fps);
    let mut w = world(c.present, 0, 20);
    if c.moving {
        halt_four_in_five(&mut w);
    } else {
        for (i, o) in w.objects.values_mut().enumerate() {
            if (i / 10) % 10 != 0 {
                o.speed = 0.0;
            }
        }
    }
    w.sweep = Some((10.0, 1.0));
    w.batch_us = 4.0;
    w.close_us = 1.5;
    w.birth_us = Some(25.0);
    w.enum_item_us = Some(7.0);
    w.enum_step_us = Some(120.0);
    w.tier_us[Tier::Vitals.index()] = Some(1.5);
    w.newest_first = c.newest_first;
    // A mark's read in the pump: 22 µs (LIVE,
    // docs/results/2026-11-17-s19-spawn-state.md).
    w.mark_us = Some(22.0);
    // The table fills and the first enumeration ends before the stream.
    run_x(&mut s, &mut rec, &mut w, 120.0, fps, syscall, call, pre);
    rec.began += Duration::from_secs_f64(1.0 / fps);
    assert!(w.births.is_empty(), "the table filled before the stream");
    w.stream = Some(Stream {
        next: FIRST_STREAMED,
        left: c.spawns,
        every: 2,
        frame: 0,
        held: c.marked,
        moving_in: if c.moving { 5 } else { 10 },
        handler_adds: c.handler,
    });
    let mut r = BindRun {
        lags: Vec::new(),
        unbound: 0,
        not_once: 0,
        drained_s: None,
        due: 0.0,
        swept: 0,
        marks_read: 0,
        marks_waiting: 0,
        share: 0.0,
        over_250: 0,
    };
    let steps = secs / 10;
    for step in 0..steps {
        let run = run_x(&mut s, &mut rec, &mut w, 10.0, fps, syscall, call, pre);
        rec.began += Duration::from_secs_f64(1.0 / fps);
        let now = (step * 10 + 10) as f64;
        if r.drained_s.is_some_and(|d| now <= d + 60.0) {
            r.due += run.vitals_due;
            r.swept += run.vitals_swept;
        }
        r.share += run.share / steps as f64;
        r.over_250 += run.over_250_plain;
        let streamed = w.stream.as_ref().is_some_and(|st| st.left == 0);
        if r.drained_s.is_none() && streamed && w.births.is_empty() {
            r.drained_s = Some(now);
        }
    }
    r.lags = std::mem::take(&mut w.lags);
    r.lags.sort_by(f64::total_cmp);
    r.unbound = w.born.len();
    // The `OBJECT` records a spawn or a birth gave the streamed units, those
    // with `enumeration` 0: an enumeration writes each row it finds again.
    // The object ID is the record's first field and `enumeration` its fifth
    // (spec/records.md, "Layout: OBJECT").
    let field =
        |p: &[u8], at: usize| u64::from_le_bytes(p[at..at + 8].try_into().expect("a u64 field"));
    let mut written: HashMap<u64, usize> = HashMap::new();
    for (k, p) in &rec.records {
        if *k == kind::OBJECT && field(p, 0x20) == 0 {
            let object = field(p, 0);
            if object >= FIRST_STREAMED {
                *written.entry(object).or_default() += 1;
            }
        }
    }
    r.not_once = (FIRST_STREAMED..FIRST_STREAMED + c.spawns as u64)
        .filter(|object| written.get(object) != Some(&1))
        .count();
    r.marks_read = w.marks_read;
    r.marks_waiting = s.loadouts_waiting();
    r
}

fn show(what: &str, r: &BindRun) {
    eprintln!(
        "{what}: lag median {:.2} s, largest {:.2} s, {} bound, {} unbound; births drained by \
         {:?} s; vitals {:.1} s as a rate in the 60 s after ({} of {:.0} rows); marks read {}, \
         waiting {}; share {:.5}; frames over 250 µs {}",
        r.median(),
        r.largest(),
        r.lags.len(),
        r.unbound,
        r.drained_s,
        r.vitals_s(),
        r.swept,
        r.due,
        r.marks_read,
        r.marks_waiting,
        r.share,
        r.over_250
    );
}

/// The server test of S20 in the model, its units born as a mission's own
/// are: 500 units in the table, one group in ten moving, and 500 births at
/// one every second frame, each waiting for the births tier.
const S20: Case = Case {
    present: 500,
    spawns: 500,
    moving: false,
    newest_first: false,
    marked: false,
    handler: false,
};

/// The same spawns through `GROUP_SPAWN`: the handler reads each unit and
/// the agent adds its row in the spawn's frame, as commanded work
/// (spec/commands.md, "Layout: GROUP_SPAWN"). No row of a spawned unit
/// comes after its spawn, and each unit's `OBJECT` is written once: the
/// later births cost the tier a `wants` check each and add no row. The
/// model's clock holds the agent's own time and leaves the handler's reads
/// out, as commanded work. What the share comparison shows is that the
/// births the tier still receives, each a pump's call and a `wants` check,
/// cost no more than the reads they replace.
#[test]
fn a_spawned_unit_has_its_row_in_the_spawns_frame() {
    let born = bind_run(&S20, 60);
    let spawned = bind_run(
        &Case {
            handler: true,
            ..S20
        },
        60,
    );
    show("S20, births tier", &born);
    show("S20, the handler adds", &spawned);
    assert_eq!(spawned.lags.len(), S20.spawns);
    assert_eq!(spawned.largest(), 0.0, "a row after its spawn's frame");
    assert_eq!(spawned.unbound, 0);
    assert_eq!(spawned.not_once, 0, "units with no OBJECT or a second one");
    assert_eq!(born.not_once, 0, "units with no OBJECT or a second one");
    // Every later birth was counted and dropped: none waits at the end.
    assert!(spawned.drained_s.is_some(), "births still wait");
    assert_eq!(spawned.over_250, 0, "frames over 250 µs");
    assert!(
        spawned.share <= born.share,
        "share {} with the handler's reads, {} with the births tier's",
        spawned.share,
        born.share
    );
    assert!(spawned.share <= 0.003, "share {}", spawned.share);
}

/// The cause of the unbound units of S20 (docs/results/2026-11-17-s20-virtualization.md,
/// "Unbound units"): births arrive at about 56 a second and resolve at
/// about 43 in this model, so a backlog builds. Resolved newest first, as
/// the shim did, the first units of the stream wait until the whole backlog
/// is gone: 12.3 s here. Resolved oldest first, a unit waits only for the
/// births ahead of it: 3.4 s at most, 1.4 s at the median.
///
/// The limits are this test's own, set from the model's figures with room
/// for a change of cost: 5 s for oldest first, against 12.3 s newest first.
#[test]
fn oldest_first_a_units_row_trails_its_birth_by_the_births_ahead_of_it() {
    let oldest = bind_run(&S20, 60);
    show("S20, oldest first", &oldest);
    assert_eq!(oldest.unbound, 0, "every unit has its row");
    assert_eq!(oldest.lags.len(), S20.spawns);
    assert!(
        oldest.largest() <= 5.0,
        "largest lag {} s",
        oldest.largest()
    );
    assert!(oldest.median() <= 2.5, "median lag {} s", oldest.median());
    assert!(oldest.share <= 0.003, "share {}", oldest.share);
    assert_eq!(oldest.over_250, 0, "frames over 250 µs");

    let newest = bind_run(
        &Case {
            newest_first: true,
            ..S20
        },
        60,
    );
    show("S20, newest first", &newest);
    assert_eq!(newest.unbound, 0);
    // The order moves no time: the births drain as fast either way. It
    // decides which unit waits.
    assert_eq!(newest.drained_s, oldest.drained_s);
    assert!(
        newest.largest() >= 2.0 * oldest.largest(),
        "newest first {} s, oldest first {} s",
        newest.largest(),
        oldest.largest()
    );
}

/// A spawn of 1,000 AI units into a table of 1,000, one group in five
/// moving, as the live runs at 2,000 units: no birth is in a slot a player
/// holds, so none leaves a loadout mark, no pump comes for one, and no
/// frame passes 250 µs. With a mark for every birth, 22 µs each (LIVE,
/// docs/results/2026-11-17-s19-spawn-state.md), the model read 1,000 marks
/// and put 1 to 3 frames over 250 µs.
#[test]
fn a_spawn_of_ai_units_leaves_no_loadout_mark() {
    let r = bind_run(
        &Case {
            present: 1000,
            spawns: 1000,
            moving: true,
            newest_first: false,
            marked: false,
            handler: false,
        },
        120,
    );
    show("1,000 AI spawns", &r);
    assert_eq!(r.unbound, 0, "every unit has its row");
    assert_eq!((r.marks_read, r.marks_waiting), (0, 0));
    assert_eq!(r.over_250, 0, "frames over 250 µs");
    assert!(r.share <= 0.003, "share {}", r.share);
    // Vitals repay what the fill held back: at or above their rate in the
    // minute after it.
    assert!(r.vitals_s() <= 6.5, "vitals {} s as a rate", r.vitals_s());
}

/// The measurements behind docs/results: the births' order and the marks
/// for units that wait for the births tier, and the handler's bind, at 500
/// and at 1,000 spawns.
#[test]
#[ignore = "a measurement, run by hand"]
fn bind_lag_at_live_costs() {
    for (present, spawns, moving) in [(500, 500, false), (1000, 1000, false), (1000, 1000, true)] {
        for marked in [true, false] {
            for newest_first in [true, false] {
                let c = Case {
                    present,
                    spawns,
                    moving,
                    newest_first,
                    marked,
                    handler: false,
                };
                show(
                    &format!(
                        "{present} present, {spawns} spawns, moving {moving}, marked {marked}, \
                         newest first {newest_first}"
                    ),
                    &bind_run(&c, 180),
                );
            }
        }
        let c = Case {
            present,
            spawns,
            moving,
            newest_first: false,
            marked: false,
            handler: true,
        };
        show(
            &format!("{present} present, {spawns} spawns, moving {moving}, the handler adds"),
            &bind_run(&c, 180),
        );
    }
}
