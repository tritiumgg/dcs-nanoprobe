//! The loadouts of the units players occupy, by DCS object ID
//! (spec/world.md, "Ammunition").
//!
//! A player's aircraft is usually an adopted unit with no row in the store,
//! so the store cannot carry its loadout. This table does, for every unit an
//! `AMMO` record names, and `Player.ammo` reads it. The table is volatile: it
//! is never written to disk and a new mission generation or agent boot clears
//! it.
//!
//! An `AMMO` record can arrive before the sidecar has bound its object to an
//! `EntityId`. The read waits here, by object, until the unit is bound in
//! the same generation.

use std::collections::{BTreeMap, VecDeque};

use nanoprobe_sidecar::store::Loadout;

/// The most reads held for objects that are not bound yet. The oldest read
/// goes first when a new one does not fit.
pub const MAX_HELD: usize = 64;

#[derive(Debug)]
struct Held {
    object: u64,
    boot: u64,
    loadout: Loadout,
}

/// The loadouts and the reads waiting for a bind.
#[derive(Debug, Default)]
pub struct Loadouts {
    /// The newest read of each object, with the agent boot it was made in.
    now: BTreeMap<u64, (u64, Loadout)>,
    held: VecDeque<Held>,
}

impl Loadouts {
    /// The loadout of an object, when a read was made.
    pub fn get(&self, object: u64) -> Option<&Loadout> {
        self.now.get(&object).map(|(_, l)| l)
    }

    /// Keep a read of a bound object. It replaces the one before.
    pub fn set(&mut self, object: u64, boot: u64, loadout: Loadout) {
        self.now.insert(object, (boot, loadout));
    }

    /// Hold a read of an object that has no `EntityId` yet. A newer read of
    /// the same object replaces the held one.
    pub fn hold(&mut self, object: u64, boot: u64, loadout: Loadout) {
        self.held.retain(|h| h.object != object);
        while self.held.len() >= MAX_HELD {
            self.held.pop_front();
        }
        self.held.push_back(Held {
            object,
            boot,
            loadout,
        });
    }

    /// Forget an object: its loadout and any held read.
    pub fn forget(&mut self, object: u64) {
        self.now.remove(&object);
        self.held.retain(|h| h.object != object);
    }

    /// Drop every read made in another agent boot or mission generation.
    pub fn retain_generation(&mut self, boot: u64, mission_gen: u32) {
        self.now
            .retain(|_, (b, l)| *b == boot && l.read.mission_gen == mission_gen);
        self.held
            .retain(|h| h.boot == boot && h.loadout.read.mission_gen == mission_gen);
    }

    /// Take the held reads of objects `bound` accepts, oldest first.
    pub fn take_bound(&mut self, bound: impl Fn(u64) -> bool) -> Vec<(u64, u64, Loadout)> {
        if self.held.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut keep = VecDeque::new();
        for h in std::mem::take(&mut self.held) {
            if bound(h.object) {
                out.push((h.object, h.boot, h.loadout));
            } else {
                keep.push_back(h);
            }
        }
        self.held = keep;
        out
    }

    /// Reads held, for tests and counters.
    pub fn held_len(&self) -> usize {
        self.held.len()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nanoprobe_sidecar::store::ReadStamp;

    use super::*;

    fn l(gen_: u32) -> Loadout {
        Loadout {
            cause: 1,
            ammo: BTreeMap::new(),
            read: ReadStamp {
                mission_gen: gen_,
                ..ReadStamp::default()
            },
        }
    }

    #[test]
    fn held_reads_are_bounded_and_replaced_by_object() {
        let mut t = Loadouts::default();
        for o in 0..(MAX_HELD as u64 + 10) {
            t.hold(o, 1, l(1));
        }
        assert_eq!(t.held_len(), MAX_HELD);
        // The oldest went first.
        assert!(t.take_bound(|o| o == 0).is_empty());
        assert_eq!(t.take_bound(|o| o == 10).len(), 1);
        t.hold(500, 1, l(1));
        t.hold(500, 1, l(1));
        assert_eq!(t.take_bound(|o| o == 500).len(), 1);
    }

    #[test]
    fn a_new_generation_or_boot_drops_reads() {
        let mut t = Loadouts::default();
        t.set(1, 1, l(1));
        t.hold(2, 1, l(1));
        t.set(3, 1, l(2));
        t.retain_generation(1, 2);
        assert!(t.get(1).is_none());
        assert!(t.get(3).is_some());
        assert_eq!(t.held_len(), 0);
        t.retain_generation(2, 2);
        assert!(t.get(3).is_none());
    }
}
