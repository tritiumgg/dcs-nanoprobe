//! The compact `SEEN` record: which rows of an enumeration it carries, the
//! `full` flag, and where it goes among the other event-class records.

use super::*;
use nanoprobe_transport::records::{EnumEnd, Object, ObjectGone};

/// A recorder in place of the link, with a `HELLO` count, a drop count and a
/// payload limit a test can change.
struct Rec {
    records: Vec<(u64, Vec<u8>)>,
    began: Instant,
    hellos: u64,
    drops: u64,
    max: usize,
}

impl Rec {
    fn new() -> Rec {
        Rec {
            records: Vec::new(),
            began: Instant::now(),
            hellos: 1,
            drops: 0,
            max: 32 << 10,
        }
    }

    /// The kinds of the records so far, in order.
    fn trail(&self) -> Vec<u64> {
        self.records.iter().map(|(k, _)| *k).collect()
    }

    /// The `SEEN` IDs, one list per record.
    fn seen(&self) -> Vec<Vec<u64>> {
        self.records
            .iter()
            .filter(|(k, _)| *k == kind::SEEN)
            .map(|(_, p)| Seen::decode(p).expect("decodes").objects)
            .collect()
    }

    /// The `OBJECT` IDs, in order.
    fn objects(&self) -> Vec<u64> {
        self.records
            .iter()
            .filter(|(k, _)| *k == kind::OBJECT)
            .map(|(_, p)| Object::decode(p).expect("decodes").object)
            .collect()
    }

    /// The last `ENUM_END`.
    fn end(&self) -> EnumEnd {
        let (_, p) = self
            .records
            .iter()
            .rfind(|(k, _)| *k == kind::ENUM_END)
            .expect("an ENUM_END");
        EnumEnd::decode(p).expect("decodes")
    }

    /// Forget the records so far.
    fn clear(&mut self) {
        self.records.clear();
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
        self.max
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

fn found(object: u64) -> Found {
    Found {
        object,
        group: 7,
        kind: object_kind::UNIT,
        category: 2,
        coalition: 2,
        country: 2,
        life0: 3.0,
        name: format!("unit {object}"),
        type_name: "M-113".into(),
        group_name: "group".into(),
        movable: false,
    }
}

/// A sampler with units 1 to `n`, past its first attach.
fn table(n: u64) -> (Sampler, Rec) {
    let mut s = Sampler::new(Rates::default(), 0.07);
    let mut rec = Rec::new();
    s.sampling = true;
    s.hellos_seen = rec.hellos;
    for id in 1..=n {
        s.add(&mut rec, found(id), false);
    }
    (s, rec)
}

/// Start the next enumeration as the pump does, taking the `full` flag.
fn begin(s: &mut Sampler, rec: &mut Rec) -> u64 {
    s.enum_due = true;
    let mut slots = Vec::new();
    s.plan_tier(rec, Tier::Enumeration, Instant::now(), 1e9, &mut slots)
        .expect("a plan");
    s.enumeration.expect("an enumeration")
}

/// Report each of `ids` as an enumeration item.
fn items(s: &mut Sampler, rec: &mut Rec, ids: &[u64]) {
    for &id in ids {
        assert_ne!(s.seen(rec, id), Some(0));
    }
}

/// An enumeration over every row, from `ENUM_BEGIN` to `ENUM_END`.
fn enumerate(s: &mut Sampler, rec: &mut Rec) {
    begin(s, rec);
    let all = s.objects();
    let mut ids = all;
    ids.sort_unstable();
    items(s, rec, &ids);
    s.enumeration_done(rec, Instant::now());
}

/// The first enumeration of a table is whole, the next ones are `SEEN`, and
/// `ENUM_END` counts both.
#[test]
fn a_repeated_enumeration_reports_unchanged_rows_in_seen() {
    let (mut s, mut rec) = table(3);
    enumerate(&mut s, &mut rec);
    assert_eq!(rec.seen(), Vec::<Vec<u64>>::new());
    assert_eq!(
        (rec.end().objects, rec.end().seen),
        (3, 0),
        "the first enumeration, after a HELLO, is all OBJECT"
    );
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!(rec.trail(), [kind::ENUM_BEGIN, kind::SEEN, kind::ENUM_END]);
    assert_eq!(rec.seen(), [[1, 2, 3]]);
    assert_eq!((rec.end().objects, rec.end().seen), (0, 3));
    let seen = Seen::decode(&rec.records[1].1).unwrap();
    assert_eq!(seen.enumeration, rec.end().enumeration);
}

/// A batch holds what the payload limit takes; the IDs of every record add up
/// to `seen`, and an enumeration's `SEEN` records come before its `ENUM_END`.
#[test]
fn seen_batches_follow_the_payload_limit() {
    let (mut s, mut rec) = table(10);
    enumerate(&mut s, &mut rec);
    rec.clear();
    rec.max = Seen::FIXED + 4 * 8;
    enumerate(&mut s, &mut rec);
    assert_eq!(
        rec.seen().iter().map(Vec::len).collect::<Vec<_>>(),
        [4, 4, 2]
    );
    assert_eq!(rec.end().seen, 10);
    assert_eq!(*rec.trail().last().unwrap(), kind::ENUM_END);
    assert!(
        rec.records
            .iter()
            .filter(|(k, _)| *k == kind::SEEN)
            .all(|(_, p)| p.len() <= rec.max)
    );
}

/// A death and a `BIND` next to an open batch: the batch goes first, and the
/// bound row is written whole.
#[test]
fn a_death_and_a_bind_come_after_the_open_batch() {
    let (mut s, mut rec) = table(4);
    enumerate(&mut s, &mut rec);
    rec.clear();
    begin(&mut s, &mut rec);
    items(&mut s, &mut rec, &[1, 2]);
    // The open batch holds 1 and 2; a death of row 2 queues its
    // `OBJECT_GONE`.
    assert_ne!(
        s.gone(&mut rec, 2, Read::Value(Some(true)), Read::Value(Some(3.0))),
        0
    );
    assert_eq!(
        rec.trail(),
        [kind::ENUM_BEGIN, kind::SEEN, kind::OBJECT_GONE]
    );
    assert_eq!(rec.seen(), [[1, 2]]);
    // A binding clears the row's payload: its item is a whole `OBJECT`,
    // after the batch of the rows before it.
    items(&mut s, &mut rec, &[3]);
    s.bind(object_kind::UNIT, &[(99, 4)]);
    items(&mut s, &mut rec, &[4]);
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(
        rec.trail(),
        [
            kind::ENUM_BEGIN,
            kind::SEEN,
            kind::OBJECT_GONE,
            kind::SEEN,
            kind::OBJECT,
            kind::ENUM_END
        ]
    );
    assert_eq!(rec.seen(), [vec![1, 2], vec![3]]);
    assert_eq!(rec.objects(), [4]);
    let end = rec.end();
    assert_eq!((end.objects, end.seen), (1, 3));
    let gone = ObjectGone::decode(&rec.records[2].1).unwrap();
    assert_eq!(gone.object, 2);
    // The bound row's `OBJECT` carries its entity.
    let bound = Object::decode(&rec.records[4].1).unwrap();
    assert_eq!((bound.object, bound.entity), (4, 99));
}

/// The pump closes an enumeration batch with the `SEEN` batch queued, so an
/// event after the pump does not go ahead of it.
#[test]
fn a_closed_enumeration_batch_queues_its_seen() {
    let (mut s, mut rec) = table(3);
    enumerate(&mut s, &mut rec);
    rec.clear();
    begin(&mut s, &mut rec);
    items(&mut s, &mut rec, &[1, 2, 3]);
    assert_eq!(rec.trail(), [kind::ENUM_BEGIN]);
    s.batch.tier = Some(Tier::Enumeration);
    s.close_batch(&mut rec, Instant::now());
    assert_eq!(rec.trail(), [kind::ENUM_BEGIN, kind::SEEN]);
}

/// `ENUMERATE` makes the next enumeration whole, once.
#[test]
fn an_enumerate_command_makes_the_next_enumeration_whole() {
    let (mut s, mut rec) = table(3);
    enumerate(&mut s, &mut rec);
    s.request_enumeration();
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!(rec.objects(), [1, 2, 3]);
    assert_eq!((rec.end().objects, rec.end().seen), (3, 0));
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (0, 3));
}

/// A request that comes while an enumeration runs keeps the flag for the one
/// that follows it; the running one is not changed.
#[test]
fn a_request_during_an_enumeration_makes_the_following_one_whole() {
    let (mut s, mut rec) = table(2);
    enumerate(&mut s, &mut rec);
    rec.clear();
    begin(&mut s, &mut rec);
    s.request_enumeration();
    items(&mut s, &mut rec, &[1, 2]);
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!((rec.end().objects, rec.end().seen), (0, 2));
    assert!(s.enum_due, "the request runs after the enumeration");
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (2, 0));
}

/// A `HELLO` sets the flag, and a `HELLO` mid-enumeration makes the running
/// enumeration write whole `OBJECT` records for the rows it has not sent.
#[test]
fn a_hello_mid_enumeration_writes_the_rest_whole() {
    let (mut s, mut rec) = table(4);
    enumerate(&mut s, &mut rec);
    rec.clear();
    begin(&mut s, &mut rec);
    items(&mut s, &mut rec, &[1, 2]);
    rec.hellos = 2;
    s.check_attach(&rec);
    items(&mut s, &mut rec, &[3, 4]);
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(rec.seen(), [[1, 2]]);
    assert_eq!(rec.objects(), [3, 4]);
    assert_eq!((rec.end().objects, rec.end().seen), (2, 2));
    // The enumeration the HELLO queued is whole.
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (4, 0));
    // And the one after it is `SEEN` again.
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (0, 4));
}

/// A row whose `OBJECT` was queued before the last `HELLO` is written whole,
/// without the flag.
#[test]
fn a_row_not_sent_since_the_hello_is_written_whole() {
    let (mut s, mut rec) = table(3);
    enumerate(&mut s, &mut rec);
    rec.hellos = 2;
    s.hellos_seen = 2;
    // A row added after the HELLO has its OBJECT in this attach.
    s.add(&mut rec, found(4), false);
    rec.clear();
    s.full = false;
    begin(&mut s, &mut rec);
    s.enum_seen_ids = 0;
    // Row 4 has no cached payload yet, so it is whole as well; rows 1 to 3
    // were sent before the HELLO.
    items(&mut s, &mut rec, &[1, 2, 3, 4]);
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(rec.objects(), [1, 2, 3, 4]);
    assert_eq!(rec.end().seen, 0);
    // After that each row has a payload and an OBJECT of this attach.
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (0, 4));
}

/// A dropped event-class record makes the next enumeration whole, once. A
/// drop during an enumeration counts for the next one.
#[test]
fn a_dropped_record_makes_the_next_enumeration_whole() {
    let (mut s, mut rec) = table(3);
    enumerate(&mut s, &mut rec);
    rec.clear();
    rec.drops += 1;
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (3, 0));
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (0, 3));
    // A drop while it runs.
    rec.clear();
    begin(&mut s, &mut rec);
    items(&mut s, &mut rec, &[1, 2, 3]);
    rec.drops += 1;
    s.enumeration_done(&mut rec, Instant::now());
    assert_eq!(rec.end().seen, 3);
    rec.clear();
    enumerate(&mut s, &mut rec);
    assert_eq!((rec.end().objects, rec.end().seen), (3, 0));
}

/// A miss found alive gets a whole `OBJECT`, as before.
#[test]
fn a_miss_found_alive_gets_a_whole_object() {
    let (mut s, mut rec) = table(3);
    enumerate(&mut s, &mut rec);
    rec.clear();
    begin(&mut s, &mut rec);
    items(&mut s, &mut rec, &[1, 2]);
    s.enumeration_done(&mut rec, Instant::now());
    assert!(s.end_held());
    s.alive(&mut rec, 3, true);
    assert_eq!(rec.seen(), [[1, 2]]);
    assert_eq!(rec.objects(), [3]);
    assert_eq!((rec.end().objects, rec.end().seen), (1, 2));
    assert_eq!(
        rec.trail(),
        [kind::ENUM_BEGIN, kind::SEEN, kind::OBJECT, kind::ENUM_END]
    );
}

/// A new generation drops the open batch: it names rows that are gone.
#[test]
fn a_new_generation_drops_the_open_batch() {
    let (mut s, mut rec) = table(2);
    enumerate(&mut s, &mut rec);
    rec.clear();
    begin(&mut s, &mut rec);
    items(&mut s, &mut rec, &[1, 2]);
    s.new_generation(2);
    s.close_batch(&mut rec, Instant::now());
    assert!(rec.seen().is_empty());
    // The ids are dropped, not held: the next record flushes none of them
    // under the old enumeration's number.
    s.flush_seen(&mut rec);
    assert!(s.seen_ids.is_empty());
    assert!(rec.seen().is_empty());
}
