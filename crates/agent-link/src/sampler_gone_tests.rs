//! The memory of gone rows, the classifier and the existence checks.

use super::*;
use nanoprobe_transport::records::{EnumEnd, Object, ObjectGone};

/// A recorder in place of the link.
struct Rec {
    records: Vec<(u64, Vec<u8>)>,
    began: Instant,
    hellos: u64,
    drops: u64,
}

impl Rec {
    fn new() -> Rec {
        Rec {
            records: Vec::new(),
            began: Instant::now(),
            hellos: 1,
            drops: 0,
        }
    }

    /// The `OBJECT_GONE` records so far.
    fn gones(&self) -> Vec<ObjectGone> {
        self.records
            .iter()
            .filter(|(k, _)| *k == kind::OBJECT_GONE)
            .map(|(_, p)| ObjectGone::decode(p).expect("decodes"))
            .collect()
    }
}

impl Port for Rec {
    fn enqueue(&mut self, kind: u16, payload: Vec<u8>) {
        self.records.push((u64::from(kind), payload));
    }
    fn sample(&mut self, kind: u16, payload: &[u8]) -> bool {
        self.records.push((u64::from(kind), payload.to_vec()));
        true
    }
    fn flush(&mut self) {}
    fn max_payload(&self) -> usize {
        32 << 10
    }
    fn began(&self) -> Instant {
        self.began
    }
    fn frame_interval_us(&self) -> f64 {
        9091.0
    }
    fn overhead_share(&self) -> f64 {
        0.1
    }
    fn syscall_frame(&self) -> bool {
        false
    }
    fn hellos(&self) -> u64 {
        self.hellos
    }
    fn event_drops(&self) -> u64 {
        self.drops
    }
    fn served_boot(&self) -> u64 {
        1
    }
    fn set_sampling(&mut self, _slice_us: f64, _periods: [f64; 5], _rows_per_clock: u64) {}
}

/// What `isDead` and `getLife` gave.
type Reads = (Read<Option<bool>>, Read<Option<f64>>);

const OBJ: f64 = 1000.0;

fn found(object: u64, kind: u32, category: u32) -> Found {
    Found {
        object,
        group: if kind == object_kind::UNIT { 7 } else { 0 },
        kind,
        category,
        coalition: 2,
        country: 2,
        life0: 3.0,
        name: format!("unit {object}"),
        type_name: "M-113".into(),
        group_name: if kind == object_kind::UNIT {
            "group".into()
        } else {
            String::new()
        },
        movable: false,
    }
}

fn sampler() -> Sampler {
    Sampler::new(Rates::default(), 0.07)
}

/// A sampler with a ground unit at `OBJ`.
fn with_unit() -> (Sampler, Rec) {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), false);
    (s, rec)
}

const DEAD_READ: Read<Option<bool>> = Read::Value(Some(true));
const LIFE_READ: Read<Option<f64>> = Read::Value(Some(3.0));

const NONE_LIFE: Read<Option<f64>> = Read::Value(None);

/// The count of `OBJECT` records so far.
fn objects_written(rec: &Rec) -> usize {
    rec.records
        .iter()
        .filter(|(k, _)| *k == kind::OBJECT)
        .count()
}

/// A sampler whose pump samples, with a row of `kind` and `category` at `OBJ`.
fn with_row(kind: u32, category: u32) -> (Sampler, Rec) {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    s.add(&mut rec, found(OBJ as u64, kind, category), false);
    (s, rec)
}

/// A row of `OBJ` as a cargo static.
fn cargo_found() -> Found {
    let mut f = found(OBJ as u64, object_kind::STATIC, 3);
    f.movable = true;
    f
}

/// Queue `OBJ`'s check, hand it out and say it is gone with these reads.
fn check_and_gone(
    s: &mut Sampler,
    rec: &mut Rec,
    dead: Read<Option<bool>>,
    life: Read<Option<f64>>,
) {
    s.watch(Some(OBJ));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_ne!(s.gone(rec, OBJ as u64, dead, life), 0);
}

/// Start an enumeration the way the plan does: every row in the table is a member.
fn begin_enumeration(s: &mut Sampler) -> u64 {
    s.enum_count += 1;
    let n = s.enum_count;
    s.enumeration = Some(n);
    for row in s.rows.iter_mut().flatten() {
        row.enum_member = n;
        row.enum_seen = false;
    }
    n
}

/// A ground unit whose isDead is true and whose getLife returns a number is DEAD.
#[test]
fn a_proven_kill_is_dead() {
    let (mut s, mut rec) = with_unit();
    assert_ne!(s.gone(&mut rec, OBJ as u64, DEAD_READ, LIFE_READ), 0);
    let g = rec.gones();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].reason, gone::DEAD);
    assert!(s.wants(OBJ as u64));
    assert!(s.died(OBJ as u64));
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 0]);
}

/// A ground unit whose two reads both raise is REMOVED.
#[test]
fn a_proven_removal_is_removed() {
    let (mut s, mut rec) = with_unit();
    assert_ne!(s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised), 0);
    assert_eq!(rec.gones()[0].reason, gone::REMOVED);
    assert!(s.wants(OBJ as u64));
    assert!(!s.died(OBJ as u64));
    assert_eq!(s.gone_counts(), [0, 1, 0, 0, 0]);
}

/// Every disagreement between the two reads of a ground unit is MISSING and one anomaly.
#[test]
fn a_disagreement_is_missing_and_an_anomaly() {
    let cases: [Reads; 8] = [
        (DEAD_READ, Read::Raised),
        (Read::Raised, LIFE_READ),
        (Read::Value(Some(false)), LIFE_READ),
        (Read::Value(None), LIFE_READ),
        (DEAD_READ, Read::Value(None)),
        (DEAD_READ, Read::Value(Some(f64::NAN))),
        (Read::Value(Some(false)), Read::Value(None)),
        (Read::Value(Some(false)), Read::Raised),
    ];
    for (dead, life) in cases {
        let (mut s, mut rec) = with_unit();
        assert_ne!(s.gone(&mut rec, OBJ as u64, dead, life), 0);
        assert_eq!(rec.gones()[0].reason, gone::MISSING, "{dead:?} {life:?}");
        assert_eq!(s.gone_counts(), [0, 0, 1, 1, 0], "{dead:?} {life:?}");
    }
}

/// A static's `getLife` at 0 or below is DEAD, whatever it is below.
#[test]
fn a_static_with_no_life_is_dead() {
    for life in [0.0, -1.0, -250.5] {
        let (mut s, mut rec) = with_row(object_kind::STATIC, 3);
        assert_ne!(
            s.gone(
                &mut rec,
                OBJ as u64,
                NONE_LIFE_DEAD,
                Read::Value(Some(life))
            ),
            0
        );
        assert_eq!(rec.gones()[0].reason, gone::DEAD, "{life}");
        assert!(s.died(OBJ as u64));
        assert_eq!(s.gone_counts(), [1, 0, 0, 0, 0]);
    }
}

/// The static rule gets `dead` as `Read::Value(None)`.
const NONE_LIFE_DEAD: Read<Option<bool>> = Read::Value(None);

/// A static whose `getLife` raises is REMOVED.
#[test]
fn a_static_that_raises_is_removed() {
    let (mut s, mut rec) = with_row(object_kind::STATIC, 3);
    s.gone(&mut rec, OBJ as u64, NONE_LIFE_DEAD, Read::Raised);
    assert_eq!(rec.gones()[0].reason, gone::REMOVED);
    assert!(!s.died(OBJ as u64));
    assert_eq!(s.gone_counts(), [0, 1, 0, 0, 0]);
}

/// A static whose `getLife` gives nil, a positive number or NaN is MISSING with an anomaly.
#[test]
fn a_static_with_any_other_answer_is_missing_and_an_anomaly() {
    for life in [
        NONE_LIFE,
        Read::Value(Some(3.0)),
        Read::Value(Some(f64::NAN)),
        Read::Value(Some(f64::INFINITY)),
    ] {
        let (mut s, mut rec) = with_row(object_kind::STATIC, 3);
        s.gone(&mut rec, OBJ as u64, NONE_LIFE_DEAD, life);
        assert_eq!(rec.gones()[0].reason, gone::MISSING, "{life:?}");
        assert_eq!(s.gone_counts(), [0, 0, 1, 1, 0], "{life:?}");
    }
}

/// Cargo, a unit of another category, aircraft and ships are MISSING with no anomaly whatever the reads say.
#[test]
fn a_disabled_class_is_missing_without_an_anomaly() {
    let rows = [
        cargo_found(),
        found(OBJ as u64, object_kind::UNIT, 4),
        found(OBJ as u64, object_kind::UNIT, 0),
        found(OBJ as u64, object_kind::UNIT, 1),
        found(OBJ as u64, object_kind::UNIT, 3),
    ];
    let reads: [Reads; 8] = [
        (DEAD_READ, LIFE_READ),
        (Read::Raised, Read::Raised),
        (Read::Value(None), Read::Raised),
        (Read::Value(None), LIFE_READ),
        (DEAD_READ, Read::Raised),
        (NONE_LIFE_DEAD, Read::Value(Some(0.0))),
        (NONE_LIFE_DEAD, Read::Value(Some(-4.0))),
        (NONE_LIFE_DEAD, NONE_LIFE),
    ];
    for f in rows {
        for (dead, life) in reads {
            let mut s = sampler();
            let mut rec = Rec::new();
            s.add(&mut rec, f.clone(), false);
            assert_ne!(s.gone(&mut rec, f.object, dead, life), 0);
            assert_eq!(rec.gones()[0].reason, gone::MISSING);
            assert_eq!(s.gone_counts(), [0, 0, 1, 0, 0], "{dead:?} {life:?}");
            assert!(!s.died(f.object));
        }
    }
}

/// An aircraft or a ship row left as MISSING gets one DEAD on a late CRASH, UNIT_LOST or DEAD event.
#[test]
fn an_aircraft_or_ship_row_gets_one_dead_on_a_late_event() {
    for category in [0, 1, 3] {
        for event in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
            let (mut s, mut rec) = with_row(object_kind::UNIT, category);
            s.gone(&mut rec, OBJ as u64, DEAD_READ, LIFE_READ);
            assert_eq!(rec.gones()[0].reason, gone::MISSING);
            s.death(&mut rec, Some(OBJ), Some(event));
            s.death(&mut rec, Some(OBJ), Some(event));
            let g = rec.gones();
            assert_eq!(g.len(), 2, "{category} {event}");
            assert_eq!(g[1].reason, gone::DEAD);
            assert_eq!(g[1].entity, g[0].entity);
            assert_eq!(s.gone_counts(), [0, 0, 1, 0, 0]);
            assert!(s.died(OBJ as u64));
        }
    }
}

/// A death event before the check gives DEAD for an aircraft row, whatever the reads.
#[test]
fn a_death_event_before_the_check_gives_dead_for_an_aircraft() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT)), 0);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_ne!(s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised), 0);
    let g = rec.gones();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].reason, gone::DEAD);
    // A mark over MISSING reads counts an anomaly.
    assert_eq!(s.gone_counts(), [1, 0, 0, 1, 0]);
}

/// The rule of each class: its read, and whether the read can say dead or removed.
#[test]
fn the_rule_table() {
    let r = |enabled, dead, removed_on_raise| Rule {
        enabled,
        dead,
        removed_on_raise,
    };
    assert_eq!(rule(Class::Ground), r(true, DeadRead::UnitFlags, true));
    assert_eq!(rule(Class::Static), r(true, DeadRead::StaticLife, true));
    assert!(!rule(Class::Aircraft).enabled);
    assert!(!rule(Class::Ship).enabled);
    assert!(!rule(Class::Cargo).enabled);
    assert!(!rule(Class::Other).enabled);
}

/// A row maps to its class by kind, category and mobility.
#[test]
fn a_row_has_a_class() {
    let class = |kind, category, movable| {
        let mut f = found(1, kind, category);
        f.movable = movable;
        class_of(&Row::new(f, 0, 0, 0))
    };
    assert_eq!(class(object_kind::UNIT, 2, false), Class::Ground);
    assert_eq!(class(object_kind::UNIT, 0, false), Class::Aircraft);
    assert_eq!(class(object_kind::UNIT, 1, false), Class::Aircraft);
    assert_eq!(class(object_kind::UNIT, 3, false), Class::Ship);
    assert_eq!(class(object_kind::UNIT, 4, false), Class::Other);
    assert_eq!(class(object_kind::STATIC, 3, false), Class::Static);
    assert_eq!(class(object_kind::STATIC, 3, true), Class::Cargo);
}

/// `classify` on the static rule, and a disabled rule, in one place.
#[test]
fn classify_of_each_rule() {
    let on = rule(Class::Static);
    for life in [0.0, -2.5] {
        assert_eq!(
            classify(on, &NONE_LIFE_DEAD, &Read::Value(Some(life))),
            (gone::DEAD, false)
        );
    }
    assert_eq!(
        classify(on, &NONE_LIFE_DEAD, &Read::Raised),
        (gone::REMOVED, false)
    );
    assert_eq!(
        classify(on, &NONE_LIFE_DEAD, &NONE_LIFE),
        (gone::MISSING, true)
    );
    assert_eq!(
        classify(on, &NONE_LIFE_DEAD, &Read::Value(Some(3.0))),
        (gone::MISSING, true)
    );
    let off = rule(Class::Cargo);
    assert_eq!(
        classify(off, &NONE_LIFE_DEAD, &Read::Value(Some(-3.0))),
        (gone::MISSING, false)
    );
    assert_eq!(
        classify(off, &NONE_LIFE_DEAD, &Read::Raised),
        (gone::MISSING, false)
    );
}

/// A call for an object the table lacks returns 0 and writes nothing.
#[test]
fn gone_for_an_unknown_object_does_nothing() {
    let mut s = sampler();
    let mut rec = Rec::new();
    assert_eq!(s.gone(&mut rec, 5, DEAD_READ, LIFE_READ), 0);
    assert!(rec.records.is_empty());
    assert_eq!(s.gone_counts(), [0; 5]);
}

/// A late DEAD, UNIT_LOST or CRASH after MISSING gives exactly one second record with the first one's entities.
#[test]
fn a_late_death_after_missing_gives_one_dead_record() {
    for category in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
        let mut s = sampler();
        let mut rec = Rec::new();
        let f = found(OBJ as u64, object_kind::UNIT, 2);
        s.add(&mut rec, f, false);
        s.gone(&mut rec, OBJ as u64, DEAD_READ, NONE_LIFE);
        assert_eq!(s.death(&mut rec, Some(OBJ), Some(category)), 0);
        // A second event for the same ID changes nothing.
        assert_eq!(s.death(&mut rec, Some(OBJ), Some(category)), 0);
        let g = rec.gones();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].reason, gone::MISSING);
        assert_eq!(g[1].reason, gone::DEAD);
        assert_eq!(g[1].entity, g[0].entity);
        assert_eq!(g[1].group_entity, g[0].group_entity);
        assert!(s.wants(OBJ as u64));
        assert!(s.died(OBJ as u64));
        assert_eq!(s.gone_counts(), [0, 0, 1, 1, 0]);
    }
}

/// A late death after REMOVED gives the DEAD record and counts one classifier fault.
#[test]
fn a_late_death_after_removed_is_a_classifier_fault() {
    let (mut s, mut rec) = with_unit();
    s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let g = rec.gones();
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].reason, gone::REMOVED);
    assert_eq!(g[1].reason, gone::DEAD);
    assert_eq!(g[1].entity, g[0].entity);
    assert_eq!(s.gone_counts(), [0, 1, 0, 0, 1]);
}

/// A late death after a static's REMOVED is a fault too.
#[test]
fn a_late_death_after_a_static_removal_is_a_fault() {
    let (mut s, mut rec) = with_row(object_kind::STATIC, 3);
    s.gone(&mut rec, OBJ as u64, NONE_LIFE_DEAD, Read::Raised);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_STATIC));
    assert_eq!(rec.gones().len(), 2);
    assert_eq!(rec.gones()[1].reason, gone::DEAD);
    assert_eq!(s.gone_counts()[4], 1);
}

/// With no death event a MISSING row stays MISSING and writes nothing more.
#[test]
fn no_event_leaves_missing() {
    let (mut s, mut rec) = with_unit();
    s.gone(&mut rec, OBJ as u64, DEAD_READ, Read::Raised);
    assert_eq!(rec.gones().len(), 1);
    assert_eq!(s.gone_counts(), [0, 0, 1, 1, 0]);
}

/// DEAD by the read, then a check that raises with no event, gives one record.
#[test]
fn dead_by_the_read_then_a_raise_gives_one_record() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    // The ID is no longer in the table: a later HIT queues nothing.
    s.watch(Some(OBJ));
    assert_eq!(s.checks_waiting(), 0);
    assert_eq!(s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised), 0);
    assert_eq!(rec.gones().len(), 1);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 0]);
}

/// Late events after a DEAD by the read change nothing.
#[test]
fn late_events_after_a_dead_change_nothing() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    let before = rec.records.len();
    for category in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
        s.death(&mut rec, Some(OBJ), Some(category));
    }
    assert_eq!(rec.records.len(), before);
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 0]);
    assert!(s.died(OBJ as u64));
}

/// A HIT on a row that is dead gives DEAD on the next pump.
#[test]
fn a_hit_on_a_dead_row_gives_dead_on_the_next_pump() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.watch(Some(OBJ));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_eq!(out, vec![OBJ as u64]);
    s.gone(&mut rec, OBJ as u64, DEAD_READ, LIFE_READ);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
}

/// A survivor of a check is not checked again, and a second HIT queues it once more only when it arrives.
#[test]
fn a_survivor_is_checked_once() {
    let (mut s, rec) = with_row(object_kind::UNIT, 2);
    let before = rec.records.len();
    s.watch(Some(OBJ));
    s.watch(Some(OBJ));
    assert_eq!(s.checks_waiting(), 1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_eq!(s.checks(&mut out), 0);
    assert_eq!(s.checks(&mut out), 0);
    assert_eq!(s.checks_waiting(), 0);
    assert_eq!(s.objects(), vec![OBJ as u64]);
    assert_eq!(rec.records.len(), before);
    assert_eq!(s.gone_counts(), [0; 5]);
}

/// More than 20 queued checks: 20 a call, in order, and none dropped unchecked.
#[test]
fn a_carried_over_entry_is_still_checked() {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    for i in 1..=45u64 {
        s.add(&mut rec, found(i, object_kind::UNIT, 2), false);
        s.watch(Some(i as f64));
        // A second HIT does not queue it twice.
        s.watch(Some(i as f64));
    }
    assert_eq!(s.checks_waiting(), 45);
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for want in [20, 20, 5, 0] {
        assert_eq!(s.checks(&mut out), want);
        assert_eq!(out.len(), want);
        seen.extend(out.iter().copied());
    }
    assert_eq!(seen, (1..=45).collect::<Vec<u64>>());
    // A later pump with no new HIT checks nothing.
    assert_eq!(s.checks_waiting(), 0);
}

/// A call for an object the table lacks, a bad number and a row already queued add nothing.
#[test]
fn watch_ignores_what_the_table_lacks() {
    let (mut s, _rec) = with_row(object_kind::UNIT, 2);
    s.watch(Some(5.0));
    s.watch(Some(f64::NAN));
    s.watch(Some(-1.0));
    s.watch(None);
    assert_eq!(s.checks_waiting(), 0);
}

/// KILL, BDA and HIT all use `watch`: a row queued by an earlier event is not queued by a death event again.
#[test]
fn a_row_is_queued_once_across_event_kinds() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.watch(Some(OBJ));
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    assert_eq!(s.checks_waiting(), 1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    // The death event was named before the check handed it out.
    assert_ne!(s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised), 0);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
}

/// An object that left the table drops out of the check queue.
#[test]
fn a_gone_row_is_skipped() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.watch(Some(OBJ));
    s.remove(&mut rec, OBJ as u64, gone::MISSING);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 0);
    assert_eq!(s.checks_waiting(), 0);
}

/// A dead ID found alive gives one OBJECT, leaves the dead set, and counts a revival.
#[test]
fn a_dead_id_found_alive_is_a_revival() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    let first = rec.gones()[0];
    assert!(s.died(OBJ as u64));
    assert!(s.wants(OBJ as u64));
    let objects = objects_written(&rec);
    assert_ne!(
        s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), false),
        0
    );
    assert_eq!(objects_written(&rec), objects + 1);
    assert!(!s.died(OBJ as u64));
    assert!(!s.wants(OBJ as u64));
    assert_eq!(s.revivals(), 1);
    // The OBJECT carries the entity the first one had.
    let (_, payload) = rec
        .records
        .iter()
        .rfind(|(k, _)| *k == kind::OBJECT)
        .expect("an OBJECT");
    let object = Object::decode(payload).expect("decodes");
    assert_eq!(object.entity, first.entity);
    // The ID is in the table: a second add refuses.
    assert_eq!(
        s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), false),
        0
    );
    assert_eq!(s.revivals(), 1);
    assert_eq!(rec.gones().len(), 1);
}

/// An enumeration returns Some(0) for a dead-set ID so the shim can read it, and a revival found there is added as enumerated.
#[test]
fn seen_gives_zero_for_a_dead_id() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    assert_eq!(s.seen(&mut rec, OBJ as u64), None);
    begin_enumeration(&mut s);
    assert_eq!(s.seen(&mut rec, OBJ as u64), Some(0));
    assert!(s.died(OBJ as u64));
    assert_ne!(
        s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), true),
        0
    );
    assert!(!s.died(OBJ as u64));
    assert_ne!(s.seen(&mut rec, OBJ as u64), Some(0));
}

/// A revived row killed again gives a second DEAD.
#[test]
fn a_revived_row_killed_again_gives_a_second_dead() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), false);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    let g = rec.gones();
    assert_eq!(g.len(), 2);
    assert!(g.iter().all(|r| r.reason == gone::DEAD));
    assert_eq!(g[0].entity, g[1].entity);
    assert!(s.died(OBJ as u64));
    assert_eq!(s.gone_counts(), [2, 0, 0, 0, 0]);
    assert_eq!(s.revivals(), 1);
}

/// An ID found again after MISSING leaves the memory, and a later kill is DEAD once.
#[test]
fn an_object_found_again_is_forgotten() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, Read::Raised, NONE_LIFE);
    assert!(s.wants(OBJ as u64));
    assert_ne!(
        s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), false),
        0
    );
    // A late event for the first loss finds a row in the table: a check, no record.
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT)), 0);
    assert_eq!(rec.gones().len(), 1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_ne!(s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised), 0);
    // A second event after the DEAD changes nothing.
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT)), 0);
    let g = rec.gones();
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].reason, gone::MISSING);
    assert_eq!(g[1].reason, gone::DEAD);
    // The mark sat over a REMOVED read: one classifier fault.
    assert_eq!(s.gone_counts()[4], 1);
}

/// A death event for an ID that never was in the table writes nothing.
#[test]
fn a_death_for_a_stranger_writes_nothing() {
    let mut s = sampler();
    let mut rec = Rec::new();
    assert_eq!(s.death(&mut rec, Some(77.0), Some(CATEGORY_UNIT)), 0);
    assert!(rec.records.is_empty());
    assert!(s.wants(77));
    assert!(!s.died(77));
}

/// A death event with a category other than 1, 3 or 6 changes nothing.
#[test]
fn a_death_of_another_category_changes_nothing() {
    let (mut s, mut rec) = with_unit();
    s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised);
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(2.0)), 0);
    assert_eq!(rec.gones().len(), 1);
    let (mut s, mut rec) = with_unit();
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(2.0)), 0);
    assert_eq!(s.checks_waiting(), 0);
}

/// A death event on a live row only triggers a check: the row stays when the check finds it alive.
#[test]
fn a_death_event_on_a_live_row_only_triggers_a_check() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    let before = rec.records.len();
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT)), 0);
    assert_eq!(rec.records.len(), before);
    assert_eq!(s.checks_waiting(), 1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    // isExist was true: the shim says so. The row stays.
    s.alive(&mut rec, OBJ as u64, true);
    assert_eq!(s.objects(), vec![OBJ as u64]);
    assert!(!s.died(OBJ as u64));
    // The next pump's checks forget the mark: a later removal is read, not DEAD.
    assert_eq!(s.checks(&mut out), 0);
    check_and_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    assert_eq!(rec.gones().len(), 1);
    assert_eq!(rec.gones()[0].reason, gone::REMOVED);
    assert_eq!(s.gone_counts(), [0, 1, 0, 0, 0]);
}

/// A new generation in the gap means a late death writes nothing, and the check queue, the named marks, the dead set and the memory are empty.
#[test]
fn a_new_generation_forgets_everything() {
    let (mut s, mut rec) = with_unit();
    s.sampling = true;
    s.add(&mut rec, found(2000, object_kind::UNIT, 2), false);
    s.add(&mut rec, found(3000, object_kind::UNIT, 2), false);
    s.watch(Some(2000.0));
    s.death(&mut rec, Some(3000.0), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 2);
    assert_eq!(s.named, vec![3000]);
    s.watch(Some(2000.0));
    s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised);
    s.gone(&mut rec, 3000, DEAD_READ, LIFE_READ);
    assert!(s.died(3000));
    let before = rec.gones().len();
    s.new_generation(2);
    assert!(s.named.is_empty() && s.checks.is_empty());
    assert!(!s.died(3000));
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    assert_eq!(rec.gones().len(), before);
    assert!(s.wants(OBJ as u64));
    assert_eq!(s.checks(&mut out), 0);
    assert_eq!(s.checks_waiting(), 0);
}

/// A death before an enumeration: the event names a live row, the check finds it gone, and the enumeration never sees it.
#[test]
fn a_death_before_an_enumeration_is_dead() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    s.checks(&mut out);
    s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised);
    begin_enumeration(&mut s);
    assert_eq!(s.seen(&mut rec, OBJ as u64), Some(0));
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(s.checks_waiting(), 0);
    let g = rec.gones();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].reason, gone::DEAD);
}

/// A death during an enumeration: the event arrives after the enumeration began and the row is checked, not removed.
#[test]
fn a_death_during_an_enumeration_is_dead() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    begin_enumeration(&mut s);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    s.gone(&mut rec, OBJ as u64, DEAD_READ, LIFE_READ);
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(s.checks_waiting(), 0);
    let g = rec.gones();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].reason, gone::DEAD);
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 0]);
}

/// A death after an enumeration removed the row as MISSING: MISSING then DEAD with the same entities.
#[test]
fn a_death_after_an_enumeration_is_missing_then_dead() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    begin_enumeration(&mut s);
    s.enumeration_done(&mut rec, Instant::now());
    // The enumeration did not find the row: it is queued, and its reads disagree.
    assert_eq!(s.checks_waiting(), 1);
    let mut out = Vec::new();
    s.checks(&mut out);
    s.gone(&mut rec, OBJ as u64, DEAD_READ, Read::Raised);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let g = rec.gones();
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].reason, gone::MISSING);
    assert_eq!(g[1].reason, gone::DEAD);
    assert_eq!(g[0].entity, g[1].entity);
    assert_eq!(g[0].group_entity, g[1].group_entity);
}

/// An enumeration queues the rows it did not find and removes none of them; a row it found is not queued.
#[test]
fn an_enumeration_queues_a_check_for_what_it_missed() {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    s.add(&mut rec, found(1, object_kind::UNIT, 2), false);
    s.add(&mut rec, found(2, object_kind::UNIT, 2), false);
    s.add(&mut rec, found(3, object_kind::UNIT, 2), false);
    begin_enumeration(&mut s);
    assert_eq!(s.seen(&mut rec, 2), Some(2));
    s.enumeration_done(&mut rec, Instant::now());
    assert!(rec.gones().is_empty());
    assert_eq!(s.len(), 3);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 2);
    out.sort_unstable();
    assert_eq!(out, vec![1, 3]);
    // Alive: nothing is removed.
    assert_eq!(s.len(), 3);
}

/// A row added after the enumeration began is never queued by it.
#[test]
fn a_row_added_during_an_enumeration_is_not_queued() {
    let (mut s, mut rec) = with_unit();
    begin_enumeration(&mut s);
    s.add(&mut rec, found(5, object_kind::UNIT, 2), true);
    s.seen(&mut rec, OBJ as u64);
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(s.checks_waiting(), 0);
}

/// UNIT_LOST, DEAD and CRASH, each late, give one DEAD for an ID the table lost as MISSING.
#[test]
fn each_late_event_gives_one_dead() {
    for category in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
        let (mut s, mut rec) = with_unit();
        s.gone(&mut rec, OBJ as u64, NONE_LIFE_DEAD, NONE_LIFE);
        for _ in 0..3 {
            s.death(&mut rec, Some(OBJ), Some(category));
        }
        let dead = rec
            .gones()
            .iter()
            .filter(|g| g.reason == gone::DEAD)
            .count();
        assert_eq!(dead, 1);
    }
}

/// A new generation in the gap means a late death writes nothing, and a row found again never carries the memory over.
#[test]
fn a_generation_change_in_the_gap_writes_nothing() {
    let (mut s, mut rec) = with_unit();
    s.gone(&mut rec, OBJ as u64, DEAD_READ, Read::Raised);
    let before = rec.records.len();
    s.new_generation(2);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    assert_eq!(rec.records.len(), before);
}

/// A death event while the row is in the table does not remove it, and a gone call without a check names nothing.
#[test]
fn an_event_before_the_check_is_a_trigger_only() {
    let (mut s, mut rec) = with_unit();
    s.sampling = true;
    assert_eq!(s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT)), 0);
    assert_eq!(rec.gones().len(), 0);
    assert!(s.named.is_empty());
    let mut out = Vec::new();
    s.checks(&mut out);
    assert_eq!(s.named, vec![OBJ as u64]);
}

/// Checks hands out at most 20 IDs a call and carries the rest over to the next.
#[test]
fn checks_carry_over() {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    for i in 1..=45u64 {
        s.add(&mut rec, found(i, object_kind::UNIT, 2), false);
        s.check_soon(s.slot_of(i).expect("row"));
    }
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for want in [CHECKS_PER_PUMP, CHECKS_PER_PUMP, 5, 0] {
        assert_eq!(s.checks(&mut out), want);
        assert_eq!(out.len(), want);
        seen.extend(out.iter().copied());
    }
    seen.sort_unstable();
    assert_eq!(seen, (1..=45).collect::<Vec<u64>>());
}

/// A pump-read position with no position queues one check however many sweeps read it before checks hands it out.
#[test]
fn a_missing_position_queues_one_check() {
    let (mut s, _rec) = with_unit();
    s.sampling = true;
    let slot = s.slot_of(OBJ as u64).expect("row");
    for _ in 0..5 {
        s.unmoved(slot);
        s.check_soon(slot);
    }
    assert_eq!(s.checks.len(), 1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_eq!(out, vec![OBJ as u64]);
    // Once handed out, the row may queue again.
    s.check_soon(slot);
    assert_eq!(s.checks.len(), 1);
}

/// A getter that raised on a row queues its check.
#[test]
fn a_suspect_row_queues_a_check() {
    let (mut s, _rec) = with_unit();
    let slot = s.slot_of(OBJ as u64).expect("row");
    s.suspect(slot);
    s.suspect(slot);
    assert_eq!(s.checks.len(), 1);
}

/// Checks returns 0 and hands out nothing when the pump is not sampling.
#[test]
fn checks_wait_for_a_sampling_pump() {
    let (mut s, _rec) = with_unit();
    let slot = s.slot_of(OBJ as u64).expect("row");
    s.check_soon(slot);
    s.watch(Some(OBJ));
    let mut out = vec![9];
    assert_eq!(s.checks(&mut out), 0);
    assert!(out.is_empty());
    assert_eq!(s.checks_waiting(), 1);
    s.sampling = true;
    assert_eq!(s.checks(&mut out), 1);
}

/// Only a row no subscription names is a slow-vitals row.
#[test]
fn slow_vitals_rows_are_the_unwatched_ones() {
    let (mut s, _rec) = with_unit();
    let slot = s.slot_of(OBJ as u64).expect("row");
    assert!(s.slow_vitals_row(slot));
    s.row_mut(slot).expect("row").watched = true;
    assert!(!s.slow_vitals_row(slot));
    assert!(!s.slow_vitals_row(99));
}

/// Hand out the pending checks and say `OBJ` is gone with these reads.
fn next_pump_gone(
    s: &mut Sampler,
    rec: &mut Rec,
    dead: Read<Option<bool>>,
    life: Read<Option<f64>>,
) {
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_ne!(s.gone(rec, OBJ as u64, dead, life), 0);
}

/// Aircraft and ships, with each death event on a live row, give one DEAD on the next pump.
#[test]
fn an_aircraft_or_ship_with_any_event_gives_one_dead() {
    for category in [0, 1, 3] {
        for event in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
            let (mut s, mut rec) = with_row(object_kind::UNIT, category);
            s.death(&mut rec, Some(OBJ), Some(event));
            assert!(rec.gones().is_empty());
            next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
            let g = rec.gones();
            assert_eq!(g.len(), 1, "{category} {event}");
            assert_eq!(g[0].reason, gone::DEAD);
            assert!(s.died(OBJ as u64));
            // MISSING reads under a mark: one anomaly, no fault.
            assert_eq!(s.gone_counts(), [1, 0, 0, 1, 0]);
        }
    }
}

/// A late event on a revived row writes nothing.
#[test]
fn a_late_event_on_a_revived_row_writes_nothing() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    check_and_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 2), false);
    let before = rec.records.len();
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    // The row is alive: nothing is written and the row stays.
    s.alive(&mut rec, OBJ as u64, true);
    assert_eq!(s.checks(&mut out), 0);
    assert_eq!(rec.records.len(), before);
    assert_eq!(s.objects(), vec![OBJ as u64]);
    assert_eq!(s.marks_cleared(), 1);
}

/// A marked survivor's mark is cleared and counted; a later removal reads as MISSING (aircraft) or REMOVED (ground).
#[test]
fn a_marked_survivor_is_cleared_and_counted() {
    for (category, reason) in [(0, gone::MISSING), (2, gone::REMOVED)] {
        let (mut s, mut rec) = with_row(object_kind::UNIT, category);
        s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
        let mut out = Vec::new();
        assert_eq!(s.checks(&mut out), 1);
        assert_eq!(s.marks_cleared(), 0);
        // The shim found the row existing: the mark is cleared and counted.
        s.alive(&mut rec, OBJ as u64, true);
        assert_eq!(s.marks_cleared(), 1);
        assert_eq!(s.checks(&mut out), 0);
        assert_eq!(s.checks(&mut out), 0);
        assert_eq!(s.marks_cleared(), 1);
        s.watch(Some(OBJ));
        next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
        assert_eq!(rec.gones()[0].reason, reason, "{category}");
        assert_eq!(s.gone_counts()[4], 0);
    }
}

/// A mark whose row `gone` consumed is not counted as cleared.
#[test]
fn a_consumed_mark_is_not_counted_cleared() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    next_pump_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    let mut out = Vec::new();
    s.checks(&mut out);
    assert_eq!(s.marks_cleared(), 0);
}

/// Anomaly reads under a mark give DEAD with one anomaly; unmarked they give MISSING.
#[test]
fn marked_anomaly_reads_give_dead() {
    let cases: [Reads; 3] = [
        (DEAD_READ, Read::Raised),
        (Read::Value(Some(false)), LIFE_READ),
        (Read::Value(None), NONE_LIFE),
    ];
    for (dead, life) in cases {
        let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
        s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
        next_pump_gone(&mut s, &mut rec, dead, life);
        assert_eq!(rec.gones()[0].reason, gone::DEAD);
        assert_eq!(s.gone_counts(), [1, 0, 0, 1, 0]);
        let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
        check_and_gone(&mut s, &mut rec, dead, life);
        assert_eq!(rec.gones()[0].reason, gone::MISSING);
        assert_eq!(s.gone_counts(), [0, 0, 1, 1, 0]);
    }
}

/// A mark over REMOVED reads gives DEAD and a classifier fault, not an anomaly; a mark over DEAD changes no count.
#[test]
fn a_mark_over_removed_is_a_fault_and_over_dead_nothing() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 1]);
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    next_pump_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 0]);
    // A static over a removal read: the same fault.
    let (mut s, mut rec) = with_row(object_kind::STATIC, 3);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_STATIC));
    next_pump_gone(&mut s, &mut rec, NONE_LIFE_DEAD, Read::Raised);
    assert_eq!(s.gone_counts(), [1, 0, 0, 0, 1]);
}

/// A marked row the enumeration missed, or a check from the life sweep or a position read, gives DEAD.
#[test]
fn a_marked_row_found_by_any_check_is_dead() {
    // Missed by the enumeration.
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    begin_enumeration(&mut s);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(s.checks_waiting(), 1);
    next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
    // The life sweep and a position read queue through `check_soon` and `suspect`.
    for via_suspect in [false, true] {
        let (mut s, mut rec) = with_row(object_kind::UNIT, 1);
        s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
        let slot = s.slot_of(OBJ as u64).expect("row");
        if via_suspect {
            s.suspect(slot);
        } else {
            s.check_soon(slot);
        }
        assert_eq!(s.checks_waiting(), 1);
        next_pump_gone(&mut s, &mut rec, DEAD_READ, Read::Raised);
        assert_eq!(rec.gones()[0].reason, gone::DEAD);
    }
}

/// Three events for one row give one DEAD.
#[test]
fn three_events_give_one_dead() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    for category in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
        s.death(&mut rec, Some(OBJ), Some(category));
    }
    assert_eq!(s.checks_waiting(), 1);
    next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    for category in [CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO] {
        s.death(&mut rec, Some(OBJ), Some(category));
    }
    assert_eq!(rec.gones().len(), 1);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
}

/// An event for an ID of the dead set writes nothing and sets no mark.
#[test]
fn a_dead_set_event_writes_nothing() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    let before = rec.records.len();
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    assert_eq!(rec.records.len(), before);
    assert_eq!(s.checks_waiting(), 0);
}

/// A generation change drops the marks: a row of the new generation with the same ID is unmarked.
#[test]
fn a_generation_change_drops_marks() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    s.checks(&mut out);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    s.new_generation(2);
    assert!(s.named.is_empty());
    s.add(&mut rec, found(OBJ as u64, object_kind::UNIT, 0), false);
    s.watch(Some(OBJ));
    next_pump_gone(&mut s, &mut rec, DEAD_READ, LIFE_READ);
    assert_eq!(rec.gones().last().expect("a record").reason, gone::MISSING);
    assert_eq!(s.gone_counts()[0], 0);
}

/// An event with no ID, a bad ID or no category does not raise and changes nothing.
#[test]
fn an_event_with_no_id_changes_nothing() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    let before = rec.records.len();
    assert_eq!(s.death(&mut rec, None, Some(CATEGORY_UNIT)), 0);
    assert_eq!(s.death(&mut rec, Some(OBJ), None), 0);
    assert_eq!(s.death(&mut rec, Some(f64::NAN), Some(CATEGORY_UNIT)), 0);
    assert_eq!(s.death(&mut rec, Some(-1.0), Some(CATEGORY_UNIT)), 0);
    assert_eq!(rec.records.len(), before);
    assert_eq!(s.checks_waiting(), 0);
}

/// A campaign change forgets the kept rows: a late death event writes no second record, and the dead set is unchanged.
#[test]
fn a_campaign_change_forgets_the_kept_rows() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 2);
    s.gone(&mut rec, OBJ as u64, Read::Raised, Read::Raised);
    // A second row left as DEAD stays in the dead set.
    s.add(&mut rec, found(2000, object_kind::UNIT, 2), false);
    s.gone(&mut rec, 2000, DEAD_READ, LIFE_READ);
    let before = rec.records.len();
    s.set_campaign(99);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    assert_eq!(rec.records.len(), before);
    assert!(!s.died(OBJ as u64));
    assert!(s.died(2000));
    assert_eq!(s.gone_counts(), [1, 1, 0, 0, 0]);
}

// ---- the held ENUM_END ---------------------------------------------------

/// The kinds of the `OBJECT`, `OBJECT_GONE` and `ENUM_END` records so far.
fn trail(rec: &Rec) -> Vec<u64> {
    rec.records
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| [kind::OBJECT, kind::OBJECT_GONE, kind::ENUM_END].contains(k))
        .collect()
}

/// The `ENUM_END` records so far.
fn ends(rec: &Rec) -> Vec<EnumEnd> {
    rec.records
        .iter()
        .filter(|(k, _)| *k == kind::ENUM_END)
        .map(|(_, p)| EnumEnd::decode(p).expect("decodes"))
        .collect()
}

/// The enumeration numbers of the `OBJECT` records so far.
fn object_numbers(rec: &Rec) -> Vec<u64> {
    rec.records
        .iter()
        .filter(|(k, _)| *k == kind::OBJECT)
        .map(|(_, p)| Object::decode(p).expect("decodes").enumeration)
        .filter(|n| *n != 0)
        .collect()
}

/// A sampler with units 1 to `n`, an enumeration that found none of them,
/// and its end called: every unit is a miss.
fn all_missed(n: u64) -> (Sampler, Rec, u64) {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    for id in 1..=n {
        s.add(&mut rec, found(id, object_kind::UNIT, 2), false);
    }
    let e = begin_enumeration(&mut s);
    s.enum_due = false;
    s.enumeration_done(&mut rec, Instant::now());
    (s, rec, e)
}

/// A miss found gone gives `OBJECT_GONE` and then `ENUM_END`.
#[test]
fn a_miss_found_gone_gives_gone_then_the_end() {
    let (mut s, mut rec, e) = all_missed(1);
    assert!(ends(&rec).is_empty());
    assert!(s.end_held());
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert!(ends(&rec).is_empty());
    s.gone(&mut rec, 1, DEAD_READ, LIFE_READ);
    let end = ends(&rec);
    assert_eq!(end.len(), 1);
    assert_eq!((end[0].enumeration, end[0].objects), (e, 0));
    assert_eq!(
        &trail(&rec)[trail(&rec).len() - 2..],
        [kind::OBJECT_GONE, kind::ENUM_END]
    );
    assert!(!s.end_held());
}

/// A miss found alive gives its `OBJECT` with the enumeration's number, counted in the end.
#[test]
fn a_miss_found_alive_gives_an_object_then_the_end() {
    let (mut s, mut rec, e) = all_missed(2);
    let found_objects = 0;
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 2);
    s.alive(&mut rec, 1, true);
    assert!(ends(&rec).is_empty());
    s.gone(&mut rec, 2, DEAD_READ, LIFE_READ);
    assert_eq!(object_numbers(&rec)[found_objects..], [e]);
    let end = ends(&rec);
    assert_eq!((end.len(), end[0].objects), (1, 1));
    assert_eq!(
        &trail(&rec)[trail(&rec).len() - 2..],
        [kind::OBJECT_GONE, kind::ENUM_END]
    );
    assert_eq!(s.len(), 1);
}

/// A miss whose `isExist` raised, or whose object the shim lacks, gets an `OBJECT` too, and the end waits for it.
#[test]
fn a_miss_that_raised_gives_an_object() {
    let (mut s, mut rec, e) = all_missed(1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert!(ends(&rec).is_empty());
    s.alive(&mut rec, 1, false);
    assert_eq!(object_numbers(&rec), [e]);
    assert_eq!(ends(&rec)[0].objects, 1);
    assert_eq!(s.len(), 1);
}

/// A miss with a death mark that raised keeps the mark; one that survived clears it.
#[test]
fn an_undecided_check_keeps_the_mark() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    s.alive(&mut rec, OBJ as u64, false);
    assert_eq!(s.marks_cleared(), 0);
    // The mark is still on the row: the next check that finds it gone writes DEAD.
    s.watch(Some(OBJ));
    next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
    assert_eq!(s.marks_cleared(), 0);
}

/// A check the shim never reported leaves its mark on the row.
#[test]
fn an_unreported_check_keeps_the_mark() {
    let (mut s, mut rec) = with_row(object_kind::UNIT, 0);
    s.death(&mut rec, Some(OBJ), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    assert_eq!(s.checks(&mut out), 0);
    assert_eq!(s.marks_cleared(), 0);
    s.watch(Some(OBJ));
    next_pump_gone(&mut s, &mut rec, Read::Raised, Read::Raised);
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
}

/// 45 misses are checked over three pumps; the end comes after the last.
#[test]
fn forty_five_misses_take_three_pumps() {
    let (mut s, mut rec, _) = all_missed(45);
    let mut out = Vec::new();
    let mut pumps = 0;
    while s.checks_waiting() > 0 {
        assert!(ends(&rec).is_empty());
        s.sampling = true;
        let n = s.checks(&mut out);
        assert!(n <= CHECKS_PER_PUMP);
        for id in out.clone() {
            s.alive(&mut rec, id, true);
        }
        pumps += 1;
    }
    assert_eq!(pumps, 3);
    let end = ends(&rec);
    assert_eq!((end.len(), end[0].objects), (1, 45));
}

/// The misses go ahead of the slow sweep's checks.
#[test]
fn the_misses_go_ahead_of_the_sweep_checks() {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    for id in 1..=30 {
        s.add(&mut rec, found(id, object_kind::UNIT, 2), false);
    }
    // The sweep queued 1 to 25 for their checks; the enumeration finds
    // 1 to 25 and misses 26 to 30.
    for id in 1..=25 {
        s.suspect(s.slot_of(id).expect("held"));
    }
    begin_enumeration(&mut s);
    for id in 1..=25 {
        s.seen(&mut rec, id);
    }
    s.enumeration_done(&mut rec, Instant::now());
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), CHECKS_PER_PUMP);
    let first: Vec<u64> = out.iter().copied().filter(|o| *o > 25).collect();
    assert_eq!(first.len(), 5, "{out:?}");
    assert!(out[..5].iter().all(|o| *o > 25), "{out:?}");
}

/// A death event or a HIT check during the hold resolves the miss it names.
#[test]
fn an_event_during_the_hold_resolves_its_miss() {
    let (mut s, mut rec, e) = all_missed(2);
    // A death event: its mark and a check that finds it gone.
    s.death(&mut rec, Some(1.0), Some(CATEGORY_UNIT));
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 2);
    s.gone(&mut rec, 1, Read::Raised, Read::Raised);
    assert!(ends(&rec).is_empty());
    assert_eq!(rec.gones()[0].reason, gone::DEAD);
    // A HIT check that finds the other alive.
    s.watch(Some(2.0));
    s.alive(&mut rec, 2, true);
    assert_eq!(object_numbers(&rec), [e]);
    assert_eq!(ends(&rec)[0].objects, 1);
}

/// A row that left by another path, with no check, resolves its miss.
#[test]
fn a_miss_removed_elsewhere_resolves() {
    let (mut s, mut rec, _) = all_missed(1);
    s.remove(&mut rec, 1, gone::REMOVED);
    assert_eq!(
        &trail(&rec)[trail(&rec).len() - 2..],
        [kind::OBJECT_GONE, kind::ENUM_END]
    );
}

/// A miss handed out that the shim never reported is undecided when the pump ends.
#[test]
fn an_unreported_miss_is_undecided_at_the_pump_end() {
    let (mut s, mut rec, e) = all_missed(1);
    let mut out = Vec::new();
    assert_eq!(s.checks(&mut out), 1);
    s.end_pump(&mut rec, Instant::now);
    assert_eq!(object_numbers(&rec), [e]);
    assert_eq!(ends(&rec).len(), 1);
}

/// No `ENUM_BEGIN` comes before the held end; a request during the hold runs after it.
#[test]
fn no_begin_before_the_held_end() {
    let (mut s, mut rec, _) = all_missed(1);
    s.request_enumeration();
    let begins = |r: &Rec| {
        r.records
            .iter()
            .filter(|(k, _)| *k == kind::ENUM_BEGIN)
            .count()
    };
    let before = begins(&rec);
    assert!(!s.enum_due);
    let mut out = Vec::new();
    s.checks(&mut out);
    // The hold keeps the periodic trigger off too.
    s.enum_last_begin = None;
    s.hellos_seen = 1;
    let now = Instant::now();
    s.slice(&rec, now);
    assert!(!s.enum_due);
    // A pump begun with the periodic trigger long past plans no ENUM_BEGIN.
    s.begin(&rec, now);
    assert!(!s.enum_due);
    assert_eq!(s.enumeration, None);
    let mut slots = Vec::new();
    let _ = s.plan(&mut rec, || now, &mut slots);
    assert_eq!(begins(&rec), before);
    s.alive(&mut rec, 1, true);
    assert!(s.enum_due);
    assert_eq!(begins(&rec), before);
}

/// A generation change drops the hold: no end follows.
#[test]
fn a_generation_change_drops_the_hold() {
    let (mut s, mut rec, _) = all_missed(2);
    s.new_generation(2);
    assert!(!s.end_held());
    assert!(ends(&rec).is_empty());
    s.alive(&mut rec, 1, true);
    assert!(ends(&rec).is_empty());
}

/// A detach, and a new HELLO, drop the hold with no end.
#[test]
fn a_detach_and_a_new_hello_drop_the_hold() {
    let (mut s, mut rec, _) = all_missed(2);
    s.detached();
    assert!(!s.end_held());
    assert!(ends(&rec).is_empty());
    let mut out = Vec::new();
    s.checks(&mut out);
    s.alive(&mut rec, 1, true);
    assert!(ends(&rec).is_empty());
    let (mut s, mut rec2, _) = all_missed(2);
    s.hellos_seen = 1;
    s.slice(&rec2, Instant::now());
    assert!(s.end_held());
    rec2.hellos = 2;
    s.slice(&rec2, Instant::now());
    assert!(!s.end_held());
    assert!(ends(&rec2).is_empty());
    assert!(s.enum_due);
    let mut out = Vec::new();
    s.checks(&mut out);
    s.alive(&mut rec2, 1, true);
    assert!(ends(&rec2).is_empty());
}

/// A hold of one enumeration period is abandoned and counted, with no end.
#[test]
fn the_backstop_abandons_a_long_hold() {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    s.add(&mut rec, found(1, object_kind::UNIT, 2), false);
    let t = Instant::now();
    begin_enumeration(&mut s);
    s.enumeration_done(&mut rec, t);
    s.hellos_seen = 1;
    s.slice(&rec, t + Duration::from_secs(59));
    assert!(s.end_held());
    assert_eq!(s.enum_end_abandoned(), 0);
    s.slice(&rec, t + Duration::from_secs(61));
    assert!(!s.end_held());
    assert_eq!(s.enum_end_abandoned(), 1);
    assert!(ends(&rec).is_empty());
}

/// An event names a row with a long check queue behind it at a cost that does not grow with the queue, and urgent checks keep their order.
#[test]
fn an_event_costs_the_same_with_a_long_queue() {
    let mut s = sampler();
    let mut rec = Rec::new();
    s.sampling = true;
    for id in 1..=3000u64 {
        s.add(&mut rec, found(id, object_kind::UNIT, 2), false);
        s.watch(Some(id as f64));
    }
    s.urgent.clear();
    let slots: Vec<u64> = (1..=3000).collect();
    for row in s.rows.iter_mut().flatten() {
        row.urgent = false;
    }
    s.promoted.clear();
    // The cost is structural: a call neither scans nor edits the check
    // queue, and it adds one entry to the urgent queue.
    let queued = s.checks.len();
    for (n, id) in slots.iter().rev().take(2000).enumerate() {
        s.watch(Some(*id as f64));
        assert_eq!(s.checks.len(), queued);
        assert_eq!(s.urgent.len(), n + 1);
    }
    let mut out = Vec::new();
    s.checks(&mut out);
    assert_eq!(out[0], 3000);
    assert_eq!(out[19], 2981);
}
