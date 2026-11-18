//! The loadout marks a birth or a takeoff event leaves, the pairs a pump
//! hands out, and the `AMMO` record the read writes (spec/sampling.md,
//! "Loadout reads").
//!
//! Only a unit a player occupies is marked, so every birth marked here is
//! one in a slot a player holds (`slot_held` true), and a takeoff is marked
//! only after such a birth of its unit.

use super::*;
use nanoprobe_transport::records::Ammo;

/// A recorder in place of the link.
struct Rec {
    records: Vec<(u64, Vec<u8>)>,
    began: Instant,
    /// The ring has no room for a sample.
    full: bool,
}

impl Rec {
    fn new() -> Rec {
        Rec {
            records: Vec::new(),
            began: Instant::now(),
            full: false,
        }
    }

    /// The `AMMO` records so far, decoded.
    fn ammo(&self) -> Vec<Ammo> {
        self.records
            .iter()
            .filter(|(k, _)| *k == kind::AMMO)
            .map(|(_, p)| Ammo::decode(p).expect("decodes"))
            .collect()
    }
}

impl Port for Rec {
    fn enqueue(&mut self, kind: u16, payload: Vec<u8>) {
        self.records.push((u64::from(kind), payload));
    }
    fn sample(&mut self, kind: u16, payload: &[u8]) -> bool {
        if self.full {
            return false;
        }
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
        1
    }
    fn event_drops(&self) -> u64 {
        0
    }
    fn served_boot(&self) -> u64 {
        1
    }
    fn set_sampling(&mut self, _slice_us: f64, _periods: [f64; 4], _rows_per_clock: u64) {}
    fn set_frame_commanded(&mut self, _us: f64) {}
}

fn found(object: u64, kind: u32) -> Found {
    Found {
        object,
        group: 7,
        kind,
        category: 2,
        coalition: 2,
        country: 2,
        life0: 3.0,
        name: format!("unit {object}"),
        type_name: "F-16C_50".into(),
        group_name: "group".into(),
        movable: false,
    }
}

/// A sampler in a sampling pump with units 1 to `n`.
fn table(n: u64) -> (Sampler, Rec) {
    let mut s = Sampler::new(Rates::default(), 0.07);
    let mut rec = Rec::new();
    s.sampling = true;
    s.hellos_seen = 1;
    // A pump with its budget ahead of it.
    s.budget.deadline = Some(Instant::now() + Duration::from_secs(1));
    for id in 1..=n {
        s.add(&mut rec, found(id, object_kind::UNIT), false);
    }
    (s, rec)
}

fn slot_of(s: &Sampler, object: u64) -> u32 {
    *s.by_object.get(&object).expect("a row")
}

#[test]
fn a_marked_unit_is_handed_out_once_with_its_cause() {
    let (mut s, _rec) = table(3);
    let mut out = Vec::new();
    // Unit 3 was born in a held slot and its birth was read.
    s.mark_loadout(Some(3.0), Ammo::BIRTH, true);
    assert_eq!(s.loadouts(&mut out), 1);
    s.mark_loadout(Some(2.0), Ammo::BIRTH, true);
    s.mark_loadout(Some(3.0), Ammo::TAKEOFF, false);
    assert_eq!(s.loadouts(&mut out), 2);
    assert_eq!(
        out,
        [
            (slot_of(&s, 2), Ammo::BIRTH),
            (slot_of(&s, 3), Ammo::TAKEOFF)
        ]
    );
    // Handed out, a mark is gone: no pump reads a unit again by itself.
    assert_eq!(s.loadouts(&mut out), 0);
    assert_eq!(s.loadouts_waiting(), 0);
}

#[test]
fn nothing_marks_a_unit_but_a_mark_call() {
    let (mut s, _rec) = table(3);
    let mut out = Vec::new();
    for _ in 0..50 {
        assert_eq!(s.loadouts(&mut out), 0);
    }
    // A life mark and an existence check leave no loadout mark.
    s.mark(Some(1.0));
    s.watch(Some(2.0));
    assert_eq!(s.loadouts_waiting(), 0);
    assert_eq!(s.loadouts(&mut out), 0);
}

#[test]
fn the_later_cause_replaces_the_earlier_for_a_unit() {
    let (mut s, _rec) = table(1);
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    s.mark_loadout(Some(1.0), Ammo::TAKEOFF, false);
    assert_eq!(s.loadouts_waiting(), 1);
    let mut out = Vec::new();
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot_of(&s, 1), Ammo::TAKEOFF)]);
}

#[test]
fn a_pump_hands_out_at_most_a_bounded_number_and_the_rest_wait() {
    let n = LOADOUTS_PER_PUMP as u64 * 2 + 3;
    let (mut s, _rec) = table(n);
    for id in 1..=n {
        s.mark_loadout(Some(id as f64), Ammo::BIRTH, true);
    }
    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut pumps = 0;
    while s.loadouts_waiting() > 0 {
        let k = s.loadouts(&mut out);
        assert!((1..=LOADOUTS_PER_PUMP).contains(&k), "{k} in a pump");
        seen.extend(out.iter().map(|(slot, _)| *slot));
        pumps += 1;
        assert!(pumps <= 3 * n, "marks never drain");
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len() as u64, n, "every marked unit comes out once");
    assert!(pumps >= 3);
}

#[test]
fn the_mark_list_is_bounded() {
    let (mut s, _rec) = table(1);
    for id in 0..(LOADOUT_MARKS_MAX as u64 + 5) {
        s.mark_loadout(Some(10_000.0 + id as f64), Ammo::BIRTH, true);
    }
    assert_eq!(s.loadouts_waiting(), LOADOUT_MARKS_MAX);
    assert_eq!(s.loadouts_dropped(), 5);
}

#[test]
fn a_full_list_drops_its_oldest_mark_so_a_players_mark_is_kept() {
    let (mut s, _rec) = table(1);
    s.direct = true;
    // The AI births of a large spawn have no row in the table here.
    for id in 0..1100u64 {
        s.mark_loadout(Some(10_000.0 + id as f64), Ammo::BIRTH, true);
    }
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    assert_eq!(s.loadouts_waiting(), LOADOUT_MARKS_MAX);
    assert_eq!(s.loadouts_dropped(), 1101 - LOADOUT_MARKS_MAX as u64);
    let mut out = Vec::new();
    s.set_pending(5, 0);
    assert_eq!(s.loadouts(&mut out), 0);
    s.set_pending(0, 0);
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot_of(&s, 1), Ammo::BIRTH)]);
}

#[test]
fn a_mark_for_a_suspect_unit_row_waits_for_the_row_to_clear() {
    let (mut s, _rec) = table(1);
    let slot = slot_of(&s, 1);
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    s.rows[slot as usize].as_mut().expect("row").suspect = true;
    let mut out = Vec::new();
    for _ in 0..5 {
        assert_eq!(s.loadouts(&mut out), 0);
        assert_eq!(s.loadouts_waiting(), 1, "the mark waits");
    }
    s.rows[slot as usize].as_mut().expect("row").suspect = false;
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot, Ammo::BIRTH)]);
}

#[test]
fn a_birth_mark_waits_for_its_row_and_a_static_is_ignored() {
    let (mut s, mut rec) = table(1);
    // The birth event comes before the pump adds the row.
    s.mark_loadout(Some(9.0), Ammo::BIRTH, true);
    let mut out = Vec::new();
    assert_eq!(s.loadouts(&mut out), 0);
    assert_eq!(s.loadouts_waiting(), 1);
    s.add(&mut rec, found(9, object_kind::UNIT), false);
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot_of(&s, 9), Ammo::BIRTH)]);
    // A mark for a row that is no unit is not kept.
    s.add(&mut rec, found(20, object_kind::STATIC), false);
    s.mark_loadout(Some(20.0), Ammo::BIRTH, true);
    assert_eq!(s.loadouts_waiting(), 0);
}

#[test]
fn a_mark_with_no_row_is_dropped_after_its_wait() {
    let (mut s, _rec) = table(1);
    s.mark_loadout(Some(99.0), Ammo::BIRTH, true);
    let mut out = Vec::new();
    for _ in 0..LOADOUT_WAIT_PUMPS {
        assert_eq!(s.loadouts(&mut out), 0);
    }
    assert_eq!(s.loadouts_waiting(), 1);
    assert_eq!(s.loadouts(&mut out), 0);
    assert_eq!(s.loadouts_waiting(), 0);
}

#[test]
fn bad_objects_mark_nothing_and_a_removed_row_loses_its_mark() {
    let (mut s, mut rec) = table(2);
    s.mark_loadout(None, Ammo::BIRTH, true);
    s.mark_loadout(Some(f64::NAN), Ammo::BIRTH, true);
    s.mark_loadout(Some(-1.0), Ammo::BIRTH, true);
    assert_eq!(s.loadouts_waiting(), 0);
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    assert_eq!(s.loadouts_waiting(), 1);
    s.remove(&mut rec, 1, gone::REMOVED);
    assert_eq!(s.loadouts_waiting(), 0);
}

#[test]
fn a_unit_no_player_occupies_gets_no_mark() {
    let (mut s, mut rec) = table(3);
    let mut out = Vec::new();
    // An AI unit: its birth is in no held slot, and neither its birth nor
    // its takeoff leaves a mark, so neither gives a pump work.
    s.mark_loadout(Some(1.0), Ammo::BIRTH, false);
    s.mark_loadout(Some(1.0), Ammo::TAKEOFF, false);
    assert_eq!(s.loadouts_waiting(), 0);
    assert!(!s.loadouts_ready());
    assert_eq!(s.loadouts(&mut out), 0);
    // A player's unit: its birth and then its takeoff are each read.
    s.mark_loadout(Some(2.0), Ammo::BIRTH, true);
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot_of(&s, 2), Ammo::BIRTH)]);
    s.mark_loadout(Some(2.0), Ammo::TAKEOFF, false);
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot_of(&s, 2), Ammo::TAKEOFF)]);
    // The unit left: an object that takes its ID is no player's unit.
    s.remove(&mut rec, 2, gone::REMOVED);
    s.add(&mut rec, found(2, object_kind::UNIT), false);
    s.mark_loadout(Some(2.0), Ammo::TAKEOFF, false);
    assert_eq!(s.loadouts_waiting(), 0);
    // A new mission generation forgets every player's unit.
    s.mark_loadout(Some(3.0), Ammo::BIRTH, true);
    s.new_generation(2);
    s.mark_loadout(Some(3.0), Ammo::TAKEOFF, false);
    assert_eq!(s.loadouts_waiting(), 0);
}

#[test]
fn a_loadout_marks_a_pump_as_worth_its_call_and_a_pump_that_is_not_sampling_hands_out_nothing() {
    let (mut s, _rec) = table(1);
    s.direct = true;
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    assert!(s.pump_worth(Instant::now()));
    s.sampling = false;
    let mut out = Vec::new();
    assert_eq!(s.loadouts(&mut out), 0);
    assert_eq!(s.loadouts_waiting(), 1);
}

#[test]
fn the_read_writes_one_ammo_record_with_its_cause_and_rows() {
    let (mut s, mut rec) = table(2);
    let slot = slot_of(&s, 2);
    s.ammo(
        &mut rec,
        slot,
        Ammo::TAKEOFF,
        &[
            (b"weapons.missiles.AIM_9M".to_vec(), 2.0),
            (b"gun".to_vec(), 500.0),
        ],
    );
    let a = rec.ammo();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].object, a[0].cause), (2, Ammo::TAKEOFF));
    let Lv::Table(rows) = &a[0].ammo else {
        panic!("a table")
    };
    assert_eq!(rows.len(), 2);
    // A slot with no unit row writes nothing; a unit with no rows writes an
    // empty array.
    s.ammo(&mut rec, 77, Ammo::BIRTH, &[]);
    assert_eq!(rec.ammo().len(), 1);
    s.ammo(&mut rec, slot_of(&s, 1), Ammo::BIRTH, &[]);
    assert_eq!(rec.ammo().len(), 2);
}

#[test]
fn a_pump_whose_tiers_used_the_budget_hands_out_one_loadout() {
    let (mut s, _rec) = table(4);
    for id in 1..=4 {
        s.mark_loadout(Some(id as f64), Ammo::BIRTH, true);
    }
    s.tiers[Tier::Vitals.index()].cost = 10.0;
    // The whole budget of the frame is 1 s; what the tiers left is none.
    s.budget.budget = 1e6;
    s.budget.deadline = Some(Instant::now());
    let mut out = Vec::new();
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(s.loadouts_waiting(), 3);
}

#[test]
fn a_dropped_ammo_keeps_its_mark_until_a_read_is_written() {
    let (mut s, mut rec) = table(1);
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    let mut out = Vec::new();
    rec.full = true;
    assert_eq!(s.loadouts(&mut out), 1);
    s.ammo(&mut rec, out[0].0, out[0].1, &[(b"gun".to_vec(), 5.0)]);
    assert!(rec.ammo().is_empty());
    assert_eq!(s.loadout_reads(), 1);
    assert_eq!(s.loadouts_waiting(), 1, "the mark is kept");
    // The next pump reads the unit again, and the record is written.
    rec.full = false;
    assert_eq!(s.loadouts(&mut out), 1);
    s.ammo(&mut rec, out[0].0, out[0].1, &[(b"gun".to_vec(), 5.0)]);
    assert_eq!(rec.ammo().len(), 1);
    assert_eq!(s.loadout_reads(), 2);
    assert_eq!(s.loadouts_waiting(), 0);
}

#[test]
fn the_retries_of_a_dropped_ammo_end_with_the_wait() {
    let (mut s, mut rec) = table(1);
    s.mark_loadout(Some(1.0), Ammo::BIRTH, true);
    rec.full = true;
    let mut out = Vec::new();
    let mut pumps = 0;
    while s.loadouts(&mut out) == 1 {
        s.ammo(&mut rec, out[0].0, out[0].1, &[]);
        pumps += 1;
        assert!(pumps <= LOADOUT_WAIT_PUMPS + 1, "retries never end");
    }
    assert_eq!(s.loadouts_waiting(), 0);
    assert_eq!(s.loadout_reads(), u64::from(pumps));
}

#[test]
fn no_loadout_is_handed_out_while_a_birth_is_pending() {
    let (mut s, _rec) = table(3);
    s.direct = true;
    for id in 1..=3 {
        s.mark_loadout(Some(id as f64), Ammo::BIRTH, true);
    }
    s.set_pending(5, 0);
    let mut out = Vec::new();
    assert_eq!(s.loadouts(&mut out), 0);
    assert_eq!(s.loadouts_waiting(), 3);
    // A mark with a row does not make the pump worth its call.
    assert!(!s.loadouts_ready());
    s.set_pending(0, 0);
    assert!(s.loadouts_ready());
}

#[test]
fn a_marks_wait_does_not_run_down_while_births_are_pending() {
    let (mut s, mut rec) = table(1);
    s.mark_loadout(Some(9.0), Ammo::BIRTH, true);
    let mut out = Vec::new();
    s.set_pending(1, 0);
    for _ in 0..LOADOUT_WAIT_PUMPS * 2 {
        assert_eq!(s.loadouts(&mut out), 0);
    }
    assert_eq!(s.loadouts_waiting(), 1);
    // The births drain and the row appears: the mark is still there.
    s.set_pending(0, 0);
    s.add(&mut rec, found(9, object_kind::UNIT), false);
    assert_eq!(s.loadouts(&mut out), 1);
    assert_eq!(out, [(slot_of(&s, 9), Ammo::BIRTH)]);
}

#[test]
fn loadouts_are_handed_out_after_the_births_drain() {
    let n = LOADOUTS_PER_PUMP as u64 * 2;
    let (mut s, _rec) = table(n);
    for id in 1..=n {
        s.mark_loadout(Some(id as f64), Ammo::BIRTH, true);
    }
    s.set_pending(3, 0);
    let mut out = Vec::new();
    assert_eq!(s.loadouts(&mut out), 0);
    s.set_pending(0, 0);
    let mut total = 0;
    while s.loadouts_waiting() > 0 {
        let k = s.loadouts(&mut out);
        assert!((1..=LOADOUTS_PER_PUMP).contains(&k));
        total += k;
    }
    assert_eq!(total as u64, n);
}
