//! Clearing wrecks, craters and debris (spec/commands.md, "Layout: CLEAR_DEBRIS").
//!
//! A clear of a sphere or a box is one `CLEAR_DEBRIS` effect. A clear of the
//! whole map is one effect per tile of [`tiles`], so that no single command
//! removes the junk of the whole map at once: a call costs DCS about 4 us per
//! object it removes (LIVE, docs/results/2026-11-16-clear-debris.md), and the
//! commanded-work budget applies between commands. The tiles' results are
//! folded into one, kept under the last tile's (epoch, seq), which the API
//! waits on as it does for any effect.
//!
//! The reconciler keeps what it sent in memory only. A `CLEAR_DEBRIS` it did
//! not send, one in flight across a sidecar restart, gets no result: its call
//! settles as `unknown`, as for any result the reconciler no longer holds.

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result as rc;
use nanoprobe_transport::records::CommandResult;

use super::{EFFECT_RESULTS_KEPT, EffectResult, Reconciler};
use crate::ids::EntityId;
use crate::sim::SimAdapter;
use crate::sim::wire::{Body, Debris};
use crate::store::{Outcome, Store};

/// Tiles along x and along z.
const TILES_PER_SIDE: u32 = 16;
/// The tiles cover x and z from minus this to this, meters: 2,000 km.
const REACH_M: f64 = 2_000_000.0;
/// The tiles' lowest y: 10 km below sea level.
const FLOOR_M: f64 = -10_000.0;
/// The tiles' highest y: 100 km up.
const CEILING_M: f64 = 100_000.0;

/// The boxes a whole-map clear sends, in order: 250 km squares that tile x
/// and z from -2,000 km to 2,000 km.
pub fn tiles() -> Vec<Debris> {
    let side = 2.0 * REACH_M / f64::from(TILES_PER_SIDE);
    let mut out = Vec::with_capacity((TILES_PER_SIDE * TILES_PER_SIDE) as usize);
    for i in 0..TILES_PER_SIDE {
        for k in 0..TILES_PER_SIDE {
            let x = -REACH_M + f64::from(i) * side;
            let z = -REACH_M + f64::from(k) * side;
            out.push(Debris::Box {
                min: [x, FLOOR_M, z],
                max: [x + side, CEILING_M, z + side],
            });
        }
    }
    out
}

/// A whole-map clear whose tiles are answering.
#[derive(Debug)]
pub(super) struct Batch {
    epoch: u64,
    first: u64,
    last: u64,
    /// Tiles the clear meant to send, and tiles it sent.
    planned: u32,
    sent: u32,
    /// Tiles answered so far.
    done: u32,
    /// Junk objects the applied tiles removed.
    removed: f64,
    cost_us: u32,
    /// The first tile, by sequence number, that did not apply.
    failed: Option<(u64, EffectResult)>,
    /// The last tile's result.
    last_result: Option<CommandResult>,
}

impl Batch {
    /// Fold one tile's answer in. True once every sent tile has answered.
    fn take(&mut self, seq: u64, outcome: Outcome, res: Option<&CommandResult>) -> bool {
        self.done += 1;
        if let Some(r) = res {
            self.cost_us = self.cost_us.saturating_add(r.cost_us);
        }
        if outcome == Outcome::Applied {
            if let Some(Lv::Number(n)) = res.map(|r| &r.value)
                && n.is_finite()
                && *n > 0.0
            {
                self.removed += n;
            }
        } else if self.failed.as_ref().is_none_or(|(s, _)| seq < *s) {
            self.failed = Some((
                seq,
                EffectResult {
                    outcome,
                    result: res.cloned(),
                },
            ));
        }
        if seq == self.last {
            self.last_result = res.cloned();
        }
        self.done >= self.sent
    }

    /// The folded result: the first failure, with the count of what the
    /// applied tiles removed as its value; otherwise `NOT_SENT` when a tile
    /// was never sent; otherwise `APPLIED` with the count.
    fn result(self) -> EffectResult {
        let total = |mut r: CommandResult| {
            r.value = Lv::Number(self.removed);
            r.cost_us = self.cost_us;
            r
        };
        match self.failed {
            Some((_, f)) => EffectResult {
                outcome: f.outcome,
                result: f.result.map(total),
            },
            None if self.sent < self.planned => EffectResult {
                outcome: Outcome::NotSent,
                result: None,
            },
            None => EffectResult {
                outcome: Outcome::Applied,
                result: self.last_result.clone().map(|mut r| {
                    r.code = rc::APPLIED as u16;
                    r.message = String::new();
                    total(r)
                }),
            },
        }
    }
}

impl Reconciler {
    /// Remove the junk inside a sphere or a box: a `CLEAR_DEBRIS`, an
    /// effect, never sent again. None when nothing can be sent now.
    pub fn clear_debris(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
        volume: Debris,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let key = (self.epoch, self.next_seq);
        let sent = self.send(store, sim, EntityId(0), 0, Body::ClearDebris(volume));
        if sent {
            self.debris_single.insert(key);
        }
        sent.then_some(key)
    }

    /// Remove the junk of the whole map: a `CLEAR_DEBRIS` for each of
    /// [`tiles`]. Returns the last sent tile's (epoch, seq), under which
    /// [`Reconciler::effect_result`] gives the folded result once every sent
    /// tile has answered. None when no tile could be sent.
    pub fn clear_debris_everywhere(
        &mut self,
        store: &mut Store,
        sim: &mut dyn SimAdapter,
    ) -> Option<(u64, u64)> {
        if !self.effect_ready(store) {
            return None;
        }
        let tiles = tiles();
        let first = self.next_seq;
        let mut last = None;
        for t in &tiles {
            let key = (self.epoch, self.next_seq);
            if !self.send(store, sim, EntityId(0), 0, Body::ClearDebris(*t)) {
                break;
            }
            last = Some(key);
        }
        let (epoch, last_seq) = last?;
        self.debris_batches.push(Batch {
            epoch,
            first,
            last: last_seq,
            planned: tiles.len() as u32,
            sent: (last_seq - first + 1) as u32,
            done: 0,
            removed: 0.0,
            cost_us: 0,
            failed: None,
            last_result: None,
        });
        Some((epoch, last_seq))
    }

    /// Take a `CLEAR_DEBRIS` result. A clear of one volume is left to the
    /// effects map (false). A tile is folded into its whole-map clear, and a
    /// clear this reconciler did not send is dropped (true).
    pub(super) fn fold_debris(
        &mut self,
        epoch: u64,
        seq: u64,
        outcome: Outcome,
        res: Option<&CommandResult>,
    ) -> bool {
        if self.debris_single.remove(&(epoch, seq)) {
            return false;
        }
        let Some(i) = self
            .debris_batches
            .iter()
            .position(|b| b.epoch == epoch && (b.first..=b.last).contains(&seq))
        else {
            return true;
        };
        if !self.debris_batches[i].take(seq, outcome, res) {
            return true;
        }
        let b = self.debris_batches.remove(i);
        let key = (b.epoch, b.last);
        self.effects.insert(key, b.result());
        while self.effects.len() > EFFECT_RESULTS_KEPT {
            self.effects.pop_first();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(planned: u32, sent: u32) -> Batch {
        Batch {
            epoch: 1,
            first: 10,
            last: 10 + u64::from(sent) - 1,
            planned,
            sent,
            done: 0,
            removed: 0.0,
            cost_us: 0,
            failed: None,
            last_result: None,
        }
    }

    fn answer(seq: u64, code: u64, removed: Lv) -> CommandResult {
        CommandResult {
            seq,
            epoch: 1,
            campaign: 0,
            entity: 0,
            rev: 0,
            object: 0,
            cost_us: 7,
            opcode: crate::sim::wire::opcode::CLEAR_DEBRIS,
            code: code as u16,
            message: String::new(),
            value: removed,
        }
    }

    #[test]
    fn applied_tiles_add_their_counts_and_costs() {
        let mut b = batch(3, 3);
        assert!(!b.take(
            10,
            Outcome::Applied,
            Some(&answer(10, rc::APPLIED, Lv::Number(2.0)))
        ));
        assert!(!b.take(
            11,
            Outcome::Applied,
            Some(&answer(11, rc::APPLIED, Lv::Nil))
        ));
        assert!(b.take(
            12,
            Outcome::Applied,
            Some(&answer(12, rc::APPLIED, Lv::Number(5.0)))
        ));
        let r = b.result();
        assert_eq!(r.outcome, Outcome::Applied);
        let r = r.result.expect("a result");
        assert_eq!((r.seq, r.cost_us, r.value), (12, 21, Lv::Number(7.0)));
    }

    #[test]
    fn the_first_failed_tile_decides_and_keeps_the_count() {
        let mut b = batch(3, 3);
        // Answers may come out of order after a reattach.
        assert!(!b.take(
            12,
            Outcome::Refused,
            Some(&answer(12, rc::DCS_REFUSED, Lv::Nil))
        ));
        assert!(!b.take(11, Outcome::Unknown, None));
        assert!(b.take(
            10,
            Outcome::Applied,
            Some(&answer(10, rc::APPLIED, Lv::Number(4.0)))
        ));
        let r = b.result();
        assert_eq!(r.outcome, Outcome::Unknown);
        assert!(r.result.is_none());
        let mut b = batch(2, 2);
        b.take(
            10,
            Outcome::Applied,
            Some(&answer(10, rc::APPLIED, Lv::Number(4.0))),
        );
        b.take(
            11,
            Outcome::Refused,
            Some(&answer(11, rc::DCS_REFUSED, Lv::Nil)),
        );
        let r = b.result();
        assert_eq!(r.outcome, Outcome::Refused);
        let r = r.result.expect("a result");
        assert_eq!((r.code, r.value), (rc::DCS_REFUSED as u16, Lv::Number(4.0)));
    }

    #[test]
    fn a_tile_never_sent_makes_the_clear_not_sent() {
        let mut b = batch(256, 1);
        assert!(b.take(
            10,
            Outcome::Applied,
            Some(&answer(10, rc::APPLIED, Lv::Number(1.0)))
        ));
        assert_eq!(b.result().outcome, Outcome::NotSent);
    }

    #[test]
    fn the_tiles_cover_the_reach_without_gaps() {
        let t = tiles();
        assert_eq!(t.len(), 256);
        let mut area = 0.0;
        for d in &t {
            let Debris::Box { min, max } = *d else {
                panic!("a tile is a box");
            };
            assert_eq!(max[0] - min[0], 250_000.0);
            assert_eq!(max[2] - min[2], 250_000.0);
            assert_eq!((min[1], max[1]), (FLOOR_M, CEILING_M));
            assert!(min[0] >= -REACH_M && max[0] <= REACH_M);
            assert!(min[2] >= -REACH_M && max[2] <= REACH_M);
            area += (max[0] - min[0]) * (max[2] - min[2]);
        }
        assert_eq!(area, (2.0 * REACH_M) * (2.0 * REACH_M));
        // Neighbors share an edge.
        let Debris::Box { max: a, .. } = t[0] else {
            unreachable!()
        };
        let Debris::Box { min: b, .. } = t[1] else {
            unreachable!()
        };
        assert_eq!(a[2], b[2]);
    }
}
