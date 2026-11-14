//! The sidecar's mirror of the agent's object table (spec/records.md, "The
//! object table" and "Samples").
//!
//! A sink feeds the mirror one record at a time, in ring order, `FRAME`
//! records included. The mirror keeps one [`Row`] per object and one [`Base`]
//! per airbase, each with the last sample of every kind and the stamps the
//! sample was read under. It holds unmanaged objects: it keeps the `entity`
//! fields the records carry, usually 0 (spec/world.md, "Managed and
//! unmanaged").
//!
//! The mirror owns no I/O. When it cannot trust its table it sets
//! [`Mirror::wants_enumeration`]; the owner sends `ENUMERATE` and calls
//! [`Mirror::clear_wants`].

use std::collections::{BTreeMap, BTreeSet};

use nanoprobe_transport::codec::{DecodeError, Lv};
use nanoprobe_transport::layout::{gap_class, kind};
use nanoprobe_transport::records::{
    Airbase, Ammo, Attitude, EnumBegin, EnumEnd, Frame, Gap, Hello, Lifecycle, Object, ObjectGone,
    Positions, Vitals, Warehouse,
};

/// The stamps of the `FRAME` a record follows (spec/records.md, "Stamps").
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stamp {
    /// Frame sequence number.
    pub frame: u64,
    /// Model time, seconds.
    pub model_time: f64,
    /// Real time, seconds.
    pub real_time: f64,
    /// Mission generation.
    pub mission_gen: u32,
    /// Active campaign, 0 for none.
    pub campaign: u64,
    /// True when the stamps are of an earlier frame.
    pub replay: bool,
}

/// A value and the stamps it was read under.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample<T> {
    /// The value.
    pub value: T,
    /// The stamps of the record that carried it.
    pub at: Stamp,
}

/// One object of the mirrored table.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The last `OBJECT` record for it.
    pub object: Object,
    /// The last enumeration that found it, 0 if none.
    pub found_in: u64,
    /// The last position: DCS `x`, `y`, `z`, meters.
    pub position: Option<Sample<[f64; 3]>>,
    /// The last life and fuel. Fuel is NaN for a static.
    pub vitals: Option<Sample<(f32, f32)>>,
    /// The last heading, pitch and roll, radians.
    pub attitude: Option<Sample<[f32; 3]>>,
    /// The last ammunition table.
    pub ammo: Option<Sample<Lv>>,
    /// Positions taken for it.
    pub position_samples: u64,
    /// Real-time seconds between its last two positions, 0 before two.
    pub position_interval: f64,
    /// The stamps of the record that added the row.
    pub since: Stamp,
}

/// One airbase.
#[derive(Clone, Debug, PartialEq)]
pub struct Base {
    /// The airbase's DCS object ID.
    pub object: u64,
    /// The airbase's DCS name.
    pub name: String,
    /// DCS coalition number of the owner.
    pub coalition: u32,
    /// The last inventory.
    pub stock: Option<Lv>,
    /// The stamps of the last record that changed the base.
    pub at: Stamp,
}

/// What the mirror dropped or decided. Every counter only grows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Sample records read after `attached` and before the next `HELLO`.
    pub samples_before_hello: u64,
    /// Sample rows for an object or airbase with no row.
    pub samples_unknown_object: u64,
    /// Object-table and sample records stamped with an older generation.
    pub old_generation: u64,
    /// Enumerations the mirror applied.
    pub enumerations_used: u64,
    /// Enumerations the mirror could not use.
    pub enumerations_refused: u64,
    /// Rows an applied enumeration removed because it did not find them.
    pub missing_by_rule: u64,
    /// Rows the agent's `OBJECT_GONE` records had not removed by `ENUM_END`
    /// and the rule removed. Each also counts in `missing_by_rule`.
    pub disagreements: u64,
}

/// An enumeration between its `ENUM_BEGIN` and `ENUM_END`.
#[derive(Debug)]
struct Open {
    number: u64,
    /// The rows present at `ENUM_BEGIN` and not removed since.
    snapshot: BTreeSet<u64>,
    /// The objects an `OBJECT` of this enumeration named.
    found: BTreeSet<u64>,
    /// `OBJECT` records of this enumeration.
    count: u64,
    /// A `GAP` of events came after `ENUM_BEGIN`.
    gap: bool,
}

/// The mirror. See the module documentation.
#[derive(Debug, Default)]
pub struct Mirror {
    generation: u32,
    stamp: Stamp,
    awaiting_hello: bool,
    wants: bool,
    lifecycle: Option<Lifecycle>,
    rows: BTreeMap<u64, Row>,
    bases: BTreeMap<String, Base>,
    open: Option<Open>,
    counters: Counters,
}

impl Mirror {
    /// An empty mirror at generation 0.
    pub fn new() -> Mirror {
        Mirror::default()
    }

    /// A new session attached. Sample records are dropped, and counted in
    /// `samples_before_hello`, until this attach's `HELLO`.
    pub fn attached(&mut self) {
        self.awaiting_hello = true;
    }

    /// Apply one record in ring order, `FRAME` and `HELLO` included.
    ///
    /// A kind the mirror does not use is ignored. A payload that does not
    /// decode returns the error and leaves the mirror as it was.
    pub fn record(&mut self, kind: u16, payload: &[u8]) -> Result<(), DecodeError> {
        match u64::from(kind) {
            kind::FRAME => self.frame(Frame::decode(payload)?),
            kind::HELLO => {
                Hello::decode(payload)?;
                self.hello();
            }
            kind::LIFECYCLE => self.lifecycle = Some(Lifecycle::decode(payload)?),
            kind::GAP => self.gap(Gap::decode(payload)?),
            kind::OBJECT => {
                let o = Object::decode(payload)?;
                if self.current() {
                    self.object(o);
                }
            }
            kind::OBJECT_GONE => {
                let g = ObjectGone::decode(payload)?;
                if self.current() {
                    self.gone(g);
                }
            }
            kind::ENUM_BEGIN => {
                let b = EnumBegin::decode(payload)?;
                if self.current() {
                    self.enum_begin(b);
                }
            }
            kind::ENUM_END => {
                let e = EnumEnd::decode(payload)?;
                if self.current() {
                    self.enum_end(e);
                }
            }
            kind::POSITIONS => {
                let p = Positions::decode(payload)?;
                if self.sampling() {
                    self.positions(p);
                }
            }
            kind::VITALS => {
                let v = Vitals::decode(payload)?;
                if self.sampling() {
                    self.vitals(v);
                }
            }
            kind::ATTITUDE => {
                let a = Attitude::decode(payload)?;
                if self.sampling() {
                    self.attitude(a);
                }
            }
            kind::AMMO => {
                let a = Ammo::decode(payload)?;
                if self.sampling() {
                    self.ammo(a);
                }
            }
            kind::WAREHOUSE => {
                let w = Warehouse::decode(payload)?;
                if self.sampling() {
                    self.warehouse(w);
                }
            }
            kind::AIRBASE => {
                let a = Airbase::decode(payload)?;
                if self.sampling() {
                    self.airbase(a);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The mission generation the table belongs to.
    /// The stamps of the last `FRAME` read.
    pub fn stamp(&self) -> Stamp {
        self.stamp
    }

    /// The current mission generation.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The last `LIFECYCLE` read.
    pub fn lifecycle(&self) -> Option<&Lifecycle> {
        self.lifecycle.as_ref()
    }

    /// The rows, sorted by object ID.
    pub fn objects(&self) -> impl Iterator<Item = &Row> {
        self.rows.values()
    }

    /// The row of `object`.
    pub fn get(&self, object: u64) -> Option<&Row> {
        self.rows.get(&object)
    }

    /// The airbases, sorted by name.
    pub fn airbases(&self) -> impl Iterator<Item = &Base> {
        self.bases.values()
    }

    /// True when the mirror cannot trust its table: a `GAP` of events, or an
    /// enumeration it could not use (spec/records.md, "The object table"). The
    /// owner sends `ENUMERATE` and calls [`Mirror::clear_wants`]. A `HELLO`
    /// does not set it, because the agent enumerates after every `HELLO`
    /// (spec/transport.md, "Attaching").
    pub fn wants_enumeration(&self) -> bool {
        self.wants
    }

    /// The owner asked for an enumeration.
    pub fn clear_wants(&mut self) {
        self.wants = false;
    }

    /// What the mirror dropped or decided.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// `FRAME`: set the stamps. A newer generation empties the table, because
    /// the agent writes no `OBJECT_GONE` for it, and ends an open enumeration,
    /// which has no `ENUM_END` (spec/sampling.md, "Full enumeration").
    fn frame(&mut self, f: Frame) {
        self.stamp = Stamp {
            frame: f.frame,
            model_time: f.model_time,
            real_time: f.real_time,
            mission_gen: f.mission_gen,
            campaign: f.campaign,
            replay: f.replay != 0,
        };
        if f.mission_gen > self.generation {
            self.generation = f.mission_gen;
            self.rows.clear();
            self.bases.clear();
            self.open = None;
        }
    }

    /// `HELLO`: the before-hello window ends and an open enumeration is
    /// abandoned.
    fn hello(&mut self) {
        self.awaiting_hello = false;
        self.open = None;
    }

    /// `GAP`: a gap of events makes the table and an open enumeration
    /// untrustworthy. A gap of logs does not (spec/records.md, "Layout: GAP").
    fn gap(&mut self, g: Gap) {
        if u64::from(g.class) == gap_class::EVENTS {
            self.wants = true;
            if let Some(open) = &mut self.open {
                open.gap = true;
            }
        }
    }

    /// True when the current stamp is not of an older generation. Counts the
    /// record when it is.
    fn current(&mut self) -> bool {
        if self.stamp.mission_gen < self.generation {
            self.counters.old_generation += 1;
            return false;
        }
        true
    }

    /// True when a sample record applies: current generation, after a `HELLO`.
    fn sampling(&mut self) -> bool {
        if !self.current() {
            return false;
        }
        if self.awaiting_hello {
            self.counters.samples_before_hello += 1;
            return false;
        }
        true
    }

    /// `OBJECT`: add or replace a row, keeping its samples.
    fn object(&mut self, o: Object) {
        let id = o.object;
        if let Some(open) = &mut self.open
            && open.number == o.enumeration
        {
            open.count += 1;
            open.found.insert(id);
        }
        match self.rows.get_mut(&id) {
            Some(row) => {
                if o.enumeration != 0 {
                    row.found_in = o.enumeration;
                }
                row.object = o;
            }
            None => {
                let found_in = o.enumeration;
                let since = self.stamp;
                self.rows.insert(
                    id,
                    Row {
                        object: o,
                        found_in,
                        position: None,
                        vitals: None,
                        attitude: None,
                        ammo: None,
                        position_samples: 0,
                        position_interval: 0.0,
                        since,
                    },
                );
            }
        }
    }

    /// `OBJECT_GONE`: remove the row. An open enumeration forgets it, so a row
    /// added again later is not one the enumeration could remove.
    fn gone(&mut self, g: ObjectGone) {
        self.rows.remove(&g.object);
        if let Some(open) = &mut self.open {
            open.snapshot.remove(&g.object);
        }
    }

    /// `ENUM_BEGIN`: snapshot the rows present. An open enumeration is
    /// abandoned and counts as refused.
    fn enum_begin(&mut self, b: EnumBegin) {
        if self.open.take().is_some() {
            self.counters.enumerations_refused += 1;
            self.wants = true;
        }
        self.open = Some(Open {
            number: b.enumeration,
            snapshot: self.rows.keys().copied().collect(),
            found: BTreeSet::new(),
            count: 0,
            gap: false,
        });
    }

    /// `ENUM_END`: apply the enumeration when the mirror read it whole
    /// (spec/records.md, "Layout: ENUM_END").
    fn enum_end(&mut self, e: EnumEnd) {
        let open = match self.open.take() {
            Some(o) if o.number == e.enumeration => o,
            // An end with no begin leaves another open enumeration alone.
            other => {
                self.open = other;
                self.counters.enumerations_refused += 1;
                self.wants = true;
                return;
            }
        };
        if open.count != u64::from(e.objects) || open.gap {
            self.counters.enumerations_refused += 1;
            self.wants = true;
            return;
        }
        self.counters.enumerations_used += 1;
        for id in open.snapshot.difference(&open.found) {
            if self.rows.remove(id).is_some() {
                self.counters.missing_by_rule += 1;
                self.counters.disagreements += 1;
            }
        }
    }

    /// `POSITIONS`: one sample per row with a known object.
    fn positions(&mut self, p: Positions) {
        let at = self.stamp;
        let n = [p.object.len(), p.x.len(), p.y.len(), p.z.len()]
            .into_iter()
            .min()
            .unwrap_or(0);
        for i in 0..n {
            let Some(row) = self.rows.get_mut(&p.object[i]) else {
                self.counters.samples_unknown_object += 1;
                continue;
            };
            if let Some(last) = &row.position {
                row.position_interval = (at.real_time - last.at.real_time).max(0.0);
            }
            row.position_samples += 1;
            row.position = Some(Sample {
                value: [p.x[i], p.y[i], p.z[i]],
                at,
            });
        }
    }

    /// `VITALS`: one sample per row with a known object.
    fn vitals(&mut self, v: Vitals) {
        let at = self.stamp;
        let n = v.object.len().min(v.life.len()).min(v.fuel.len());
        for i in 0..n {
            match self.rows.get_mut(&v.object[i]) {
                Some(row) => {
                    row.vitals = Some(Sample {
                        value: (v.life[i], v.fuel[i]),
                        at,
                    });
                }
                None => self.counters.samples_unknown_object += 1,
            }
        }
    }

    /// `ATTITUDE`: one sample per row with a known object.
    fn attitude(&mut self, a: Attitude) {
        let at = self.stamp;
        let n = [a.object.len(), a.heading.len(), a.pitch.len(), a.roll.len()]
            .into_iter()
            .min()
            .unwrap_or(0);
        for i in 0..n {
            match self.rows.get_mut(&a.object[i]) {
                Some(row) => {
                    row.attitude = Some(Sample {
                        value: [a.heading[i], a.pitch[i], a.roll[i]],
                        at,
                    });
                }
                None => self.counters.samples_unknown_object += 1,
            }
        }
    }

    /// `AMMO`: the ammunition of a known object.
    fn ammo(&mut self, a: Ammo) {
        let at = self.stamp;
        match self.rows.get_mut(&a.object) {
            Some(row) => {
                row.ammo = Some(Sample { value: a.ammo, at });
            }
            None => self.counters.samples_unknown_object += 1,
        }
    }

    /// `AIRBASE`: add an airbase or update its object and owner, keeping its
    /// stock.
    fn airbase(&mut self, a: Airbase) {
        let at = self.stamp;
        match self.bases.get_mut(&a.airbase) {
            Some(base) => {
                base.object = a.object;
                base.coalition = a.coalition;
                base.at = at;
            }
            None => {
                self.bases.insert(
                    a.airbase.clone(),
                    Base {
                        object: a.object,
                        name: a.airbase,
                        coalition: a.coalition,
                        stock: None,
                        at,
                    },
                );
            }
        }
    }

    /// `WAREHOUSE`: the stock of a known airbase. An `AIRBASE` record
    /// introduces an airbase, so a stock for an unknown name is dropped and
    /// counted in `samples_unknown_object`.
    fn warehouse(&mut self, w: Warehouse) {
        match self.bases.get_mut(&w.airbase) {
            Some(base) => {
                base.stock = Some(w.stock);
                base.at = self.stamp;
            }
            None => self.counters.samples_unknown_object += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn frame(gen_: u32, real: f64) -> (u64, Vec<u8>) {
        let f = Frame {
            frame: 1,
            real_time: real,
            mission_gen: gen_,
            ..Frame::default()
        };
        (kind::FRAME, f.encode())
    }

    fn hello() -> (u64, Vec<u8>) {
        (kind::HELLO, Hello::default().encode())
    }

    fn object(id: u64, enumeration: u64) -> (u64, Vec<u8>) {
        let o = Object {
            object: id,
            enumeration,
            kind: 1,
            name: format!("u{id}"),
            ..Object::default()
        };
        (kind::OBJECT, o.encode())
    }

    fn gone(id: u64) -> (u64, Vec<u8>) {
        let g = ObjectGone {
            object: id,
            reason: 3,
            ..ObjectGone::default()
        };
        (kind::OBJECT_GONE, g.encode())
    }

    fn begin(n: u64) -> (u64, Vec<u8>) {
        (kind::ENUM_BEGIN, EnumBegin { enumeration: n }.encode())
    }

    fn end(n: u64, objects: u32) -> (u64, Vec<u8>) {
        (
            kind::ENUM_END,
            EnumEnd {
                enumeration: n,
                objects,
            }
            .encode(),
        )
    }

    fn gap(class: u32) -> (u64, Vec<u8>) {
        let g = Gap {
            class,
            count: 3,
            ..Gap::default()
        };
        (kind::GAP, g.encode())
    }

    fn pos(ids: &[u64], x: f64) -> (u64, Vec<u8>) {
        let p = Positions {
            object: ids.to_vec(),
            x: vec![x; ids.len()],
            y: vec![2.0; ids.len()],
            z: vec![3.0; ids.len()],
        };
        (kind::POSITIONS, p.encode())
    }

    fn vitals(ids: &[u64]) -> (u64, Vec<u8>) {
        let v = Vitals {
            object: ids.to_vec(),
            life: vec![10.0; ids.len()],
            fuel: vec![f32::NAN; ids.len()],
        };
        (kind::VITALS, v.encode())
    }

    fn feed(m: &mut Mirror, records: &[(u64, Vec<u8>)]) {
        for (k, p) in records {
            m.record(*k as u16, p).unwrap();
        }
    }

    fn ids(m: &Mirror) -> Vec<u64> {
        m.objects().map(|r| r.object.object).collect()
    }

    #[test]
    fn stream_builds_rows() {
        let mut m = Mirror::new();
        m.attached();
        let life = Lifecycle {
            state: 2,
            mission_gen: 1,
            theater: "Caucasus".into(),
            mission: "m".into(),
        };
        feed(
            &mut m,
            &[
                frame(1, 10.0),
                hello(),
                (kind::LIFECYCLE, life.encode()),
                begin(7),
                object(2, 7),
                object(1, 7),
                end(7, 2),
                pos(&[1, 2], 5.0),
                vitals(&[2]),
            ],
        );
        assert_eq!(m.generation(), 1);
        assert_eq!(m.lifecycle(), Some(&life));
        assert_eq!(ids(&m), vec![1, 2]);
        let r = m.get(1).unwrap();
        assert_eq!(r.found_in, 7);
        let p = r.position.as_ref().unwrap();
        assert_eq!(p.value, [5.0, 2.0, 3.0]);
        assert_eq!(p.at.real_time, 10.0);
        assert_eq!(p.at.mission_gen, 1);
        assert!(r.vitals.is_none());
        let v = m.get(2).unwrap().vitals.as_ref().unwrap();
        assert_eq!(v.value.0, 10.0);
        assert!(v.value.1.is_nan());
        assert_eq!(m.counters().enumerations_used, 1);
        assert!(!m.wants_enumeration());
    }

    #[test]
    fn samples_before_hello_are_dropped() {
        let mut m = Mirror::new();
        m.attached();
        feed(&mut m, &[frame(0, 1.0), object(1, 0), pos(&[1], 5.0)]);
        assert!(m.get(1).unwrap().position.is_none());
        assert_eq!(m.counters().samples_before_hello, 1);
        feed(&mut m, &[hello(), pos(&[1], 6.0)]);
        assert_eq!(m.get(1).unwrap().position.as_ref().unwrap().value[0], 6.0);
        assert_eq!(m.counters().samples_before_hello, 1);
    }

    #[test]
    fn unknown_object_samples_are_counted() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(0, 1.0), object(1, 0), pos(&[1, 9], 5.0)]);
        assert_eq!(m.counters().samples_unknown_object, 1);
        assert_eq!(m.get(1).unwrap().position_samples, 1);
    }

    #[test]
    fn generation_change_empties_the_mirror() {
        let mut m = Mirror::new();
        let ab = Airbase {
            object: 5,
            coalition: 1,
            airbase: "Batumi".into(),
        };
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                object(1, 0),
                (kind::AIRBASE, ab.encode()),
                begin(1),
            ],
        );
        assert_eq!(m.airbases().count(), 1);
        feed(&mut m, &[frame(2, 2.0)]);
        assert_eq!(m.generation(), 2);
        assert_eq!(m.objects().count(), 0);
        assert_eq!(m.airbases().count(), 0);
        // The open enumeration ended with the generation.
        feed(&mut m, &[end(1, 0)]);
        assert_eq!(m.counters().enumerations_refused, 1);
    }

    #[test]
    fn older_generation_is_ignored() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(2, 1.0), object(1, 0)]);
        feed(
            &mut m,
            &[frame(1, 2.0), object(2, 0), pos(&[1], 5.0), gone(1)],
        );
        assert_eq!(ids(&m), vec![1]);
        assert_eq!(m.counters().old_generation, 3);
        assert_eq!(m.generation(), 2);
    }

    #[test]
    fn gap_of_events_refuses_enumeration() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                begin(1),
                object(1, 1),
                gap(gap_class::EVENTS as u32),
                end(1, 1),
            ],
        );
        assert_eq!(m.counters().enumerations_refused, 1);
        assert_eq!(m.counters().enumerations_used, 0);
        assert!(m.wants_enumeration());
        m.clear_wants();
        assert!(!m.wants_enumeration());
    }

    #[test]
    fn gap_of_logs_changes_nothing() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                begin(1),
                gap(gap_class::LOGS as u32),
                end(1, 0),
            ],
        );
        assert!(!m.wants_enumeration());
        assert_eq!(m.counters().enumerations_used, 1);
    }

    #[test]
    fn gap_of_events_sets_wants_outside_an_enumeration() {
        let mut m = Mirror::new();
        feed(&mut m, &[gap(gap_class::EVENTS as u32)]);
        assert!(m.wants_enumeration());
    }

    #[test]
    fn count_mismatch_refuses_enumeration() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                object(3, 0),
                begin(1),
                object(1, 1),
                end(1, 2),
            ],
        );
        assert_eq!(m.counters().enumerations_refused, 1);
        assert!(m.wants_enumeration());
        // Nothing was removed.
        assert_eq!(ids(&m), vec![1, 3]);
    }

    #[test]
    fn end_without_begin_is_refused() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(1, 1.0), end(4, 0)]);
        assert_eq!(m.counters().enumerations_refused, 1);
        assert!(m.wants_enumeration());
    }

    #[test]
    fn new_begin_abandons_the_open_one() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(1, 1.0), begin(1), begin(2), end(2, 0)]);
        assert_eq!(m.counters().enumerations_refused, 1);
        assert_eq!(m.counters().enumerations_used, 1);
        assert!(m.wants_enumeration());
    }

    #[test]
    fn hello_abandons_the_open_enumeration_without_wanting() {
        let mut m = Mirror::new();
        m.attached();
        feed(&mut m, &[frame(1, 1.0), begin(1), hello()]);
        assert!(!m.wants_enumeration());
        feed(&mut m, &[end(1, 0)]);
        assert_eq!(m.counters().enumerations_refused, 1);
    }

    #[test]
    fn used_enumeration_removes_a_missing_row() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                object(1, 0),
                object(2, 0),
                begin(1),
                object(1, 1),
                end(1, 1),
            ],
        );
        assert_eq!(ids(&m), vec![1]);
        let c = m.counters();
        assert_eq!(c.enumerations_used, 1);
        assert_eq!(c.missing_by_rule, 1);
        assert_eq!(c.disagreements, 1);
        assert!(!m.wants_enumeration());
    }

    #[test]
    fn agent_gone_before_end_is_not_a_disagreement() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                object(1, 0),
                object(2, 0),
                begin(1),
                object(1, 1),
                gone(2),
                end(1, 1),
            ],
        );
        assert_eq!(ids(&m), vec![1]);
        let c = m.counters();
        assert_eq!(c.enumerations_used, 1);
        assert_eq!(c.missing_by_rule, 0);
        assert_eq!(c.disagreements, 0);
    }

    #[test]
    fn row_added_after_begin_is_kept() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(1, 1.0), begin(1), object(9, 0), end(1, 0)]);
        assert_eq!(ids(&m), vec![9]);
        assert_eq!(m.counters().missing_by_rule, 0);
    }

    #[test]
    fn row_removed_and_added_again_is_kept() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[
                frame(1, 1.0),
                object(2, 0),
                begin(1),
                gone(2),
                object(2, 0),
                end(1, 0),
            ],
        );
        assert_eq!(ids(&m), vec![2]);
    }

    #[test]
    fn object_gone_removes() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(1, 1.0), object(1, 0), gone(1), gone(1)]);
        assert!(m.get(1).is_none());
    }

    #[test]
    fn object_replace_keeps_samples() {
        let mut m = Mirror::new();
        feed(
            &mut m,
            &[frame(1, 1.0), object(1, 4), pos(&[1], 5.0), object(1, 0)],
        );
        let r = m.get(1).unwrap();
        assert!(r.position.is_some());
        assert_eq!(r.found_in, 4);
    }

    #[test]
    fn position_interval_uses_real_time() {
        let mut m = Mirror::new();
        feed(&mut m, &[frame(1, 10.0), object(1, 0), pos(&[1], 1.0)]);
        assert_eq!(m.get(1).unwrap().position_interval, 0.0);
        feed(&mut m, &[frame(1, 12.5), pos(&[1], 2.0)]);
        let r = m.get(1).unwrap();
        assert_eq!(r.position_interval, 2.5);
        assert_eq!(r.position_samples, 2);
        feed(&mut m, &[frame(1, 13.0), pos(&[1], 3.0)]);
        assert_eq!(m.get(1).unwrap().position_interval, 0.5);
    }

    #[test]
    fn airbase_and_warehouse() {
        let mut m = Mirror::new();
        let ab = |c| {
            (
                kind::AIRBASE,
                Airbase {
                    object: 5,
                    coalition: c,
                    airbase: "Batumi".into(),
                }
                .encode(),
            )
        };
        let wh = |name: &str| {
            (
                kind::WAREHOUSE,
                Warehouse {
                    airbase: name.into(),
                    stock: Lv::Number(4.0),
                }
                .encode(),
            )
        };
        feed(
            &mut m,
            &[frame(1, 1.0), wh("Batumi"), ab(1), wh("Batumi"), ab(2)],
        );
        let b = m.airbases().next().unwrap();
        assert_eq!(b.coalition, 2);
        assert_eq!(b.stock, Some(Lv::Number(4.0)));
        assert_eq!(m.counters().samples_unknown_object, 1);
    }

    #[test]
    fn decode_error_leaves_state_unchanged() {
        let mut m = Mirror::new();
        m.attached();
        feed(&mut m, &[frame(3, 1.0), object(1, 0), begin(1)]);
        let before = format!("{m:?}");
        for k in [
            kind::FRAME,
            kind::HELLO,
            kind::LIFECYCLE,
            kind::GAP,
            kind::OBJECT,
            kind::OBJECT_GONE,
            kind::ENUM_BEGIN,
            kind::ENUM_END,
            kind::POSITIONS,
            kind::VITALS,
            kind::ATTITUDE,
            kind::AMMO,
            kind::WAREHOUSE,
            kind::AIRBASE,
        ] {
            assert!(m.record(k as u16, &[1, 2, 3]).is_err(), "kind {k}");
            assert_eq!(format!("{m:?}"), before, "kind {k}");
        }
        // A truncated batch with a valid prefix.
        let mut p = pos(&[1], 5.0).1;
        p.truncate(p.len() - 1);
        assert!(m.record(kind::POSITIONS as u16, &p).is_err());
        assert_eq!(format!("{m:?}"), before);
    }

    #[test]
    fn unknown_kinds_are_ignored() {
        let mut m = Mirror::new();
        assert!(m.record(999, &[1, 2, 3]).is_ok());
        assert!(m.record(kind::EVENT as u16, &[]).is_ok());
    }

    proptest! {
        #[test]
        fn arbitrary_records_never_panic(
            records in proptest::collection::vec(
                (
                    prop_oneof![0u16..40, any::<u16>()],
                    proptest::collection::vec(any::<u8>(), 0..160),
                ),
                0..40,
            )
        ) {
            let mut m = Mirror::new();
            m.attached();
            for (k, p) in &records {
                let _ = m.record(*k, p);
            }
            let _ = m.objects().count();
            let _ = m.airbases().count();
        }

        #[test]
        fn arbitrary_valid_streams_never_panic(
            steps in proptest::collection::vec((0u8..9, 0u64..6, 0u32..4), 0..60)
        ) {
            let mut m = Mirror::new();
            m.attached();
            for (op, id, n) in steps {
                let (k, p) = match op {
                    0 => frame(n, f64::from(n)),
                    1 => hello(),
                    2 => object(id, u64::from(n)),
                    3 => gone(id),
                    4 => begin(u64::from(n) + 1),
                    5 => end(u64::from(n) + 1, n),
                    6 => gap(n),
                    7 => pos(&[id, id + 1], f64::from(n)),
                    _ => vitals(&[id]),
                };
                m.record(k as u16, &p).unwrap();
            }
        }
    }
}
