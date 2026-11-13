//! A spatial hash grid over entity positions (plan 4.4), per theater.
//!
//! Positions are DCS map coordinates: `x` north and `z` east, in meters.
//! Altitude does not count. A query returns the entities within a radius of a
//! point, in `EntityId` order.

use std::collections::{BTreeSet, HashMap};

use crate::ids::EntityId;

type Cell = (u32, i64, i64);

/// The grid.
#[derive(Clone, Debug)]
pub struct Grid {
    cell: f64,
    theaters: Vec<String>,
    cells: HashMap<Cell, BTreeSet<EntityId>>,
    at: HashMap<EntityId, (Cell, f64, f64)>,
}

impl Grid {
    /// A grid with square cells of `cell` meters, above 0.
    pub fn new(cell: f64) -> Grid {
        assert!(cell.is_finite() && cell > 0.0, "cell size {cell}");
        Grid {
            cell,
            theaters: Vec::new(),
            cells: HashMap::new(),
            at: HashMap::new(),
        }
    }

    fn theater(&mut self, name: &str) -> u32 {
        match self.theaters.iter().position(|t| t == name) {
            Some(i) => i as u32,
            None => {
                self.theaters.push(name.to_owned());
                (self.theaters.len() - 1) as u32
            }
        }
    }

    fn coord(&self, v: f64) -> i64 {
        // Saturates for values far outside any map; such a point still lands
        // in a cell and is found by an exact distance check.
        (v / self.cell).floor() as i64
    }

    /// Put `id` at a point, moving it if it is elsewhere. A position that is
    /// not finite removes it.
    pub fn set(&mut self, id: EntityId, theater: &str, x: f64, z: f64) {
        if !x.is_finite() || !z.is_finite() {
            self.remove(id);
            return;
        }
        let t = self.theater(theater);
        let cell = (t, self.coord(x), self.coord(z));
        if let Some((old, _, _)) = self.at.get(&id).copied()
            && old != cell
        {
            self.unlink(id, old);
        }
        self.cells.entry(cell).or_default().insert(id);
        self.at.insert(id, (cell, x, z));
    }

    fn unlink(&mut self, id: EntityId, cell: Cell) {
        if let Some(set) = self.cells.get_mut(&cell) {
            set.remove(&id);
            if set.is_empty() {
                self.cells.remove(&cell);
            }
        }
    }

    /// Take `id` out of the grid.
    pub fn remove(&mut self, id: EntityId) {
        if let Some((cell, _, _)) = self.at.remove(&id) {
            self.unlink(id, cell);
        }
    }

    /// Entities in the grid.
    pub fn len(&self) -> usize {
        self.at.len()
    }

    /// Whether the grid is empty.
    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }

    /// The entities within `radius` meters of a point on `theater`.
    pub fn within(&self, theater: &str, x: f64, z: f64, radius: f64) -> Vec<EntityId> {
        let Some(t) = self.theaters.iter().position(|n| n == theater) else {
            return Vec::new();
        };
        if radius.is_nan() || radius < 0.0 || !x.is_finite() || !z.is_finite() {
            return Vec::new();
        }
        let r2 = radius * radius;
        let (x0, x1) = (self.coord(x - radius), self.coord(x + radius));
        let (z0, z1) = (self.coord(z - radius), self.coord(z + radius));
        let mut out = BTreeSet::new();
        // In i128, so a huge or infinite radius, whose cell coordinates
        // saturate at the ends of i64, cannot overflow.
        let span = (i128::from(x1) - i128::from(x0) + 1)
            .saturating_mul(i128::from(z1) - i128::from(z0) + 1);
        if span > self.cells.len() as i128 {
            // A radius wider than the occupied cells: scan the cells instead.
            for (&(ct, _, _), set) in &self.cells {
                if ct == t as u32 {
                    self.collect(set, x, z, r2, &mut out);
                }
            }
        } else {
            for cx in x0..=x1 {
                for cz in z0..=z1 {
                    if let Some(set) = self.cells.get(&(t as u32, cx, cz)) {
                        self.collect(set, x, z, r2, &mut out);
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    fn collect(
        &self,
        set: &BTreeSet<EntityId>,
        x: f64,
        z: f64,
        r2: f64,
        out: &mut BTreeSet<EntityId>,
    ) {
        for id in set {
            let (_, px, pz) = self.at[id];
            let (dx, dz) = (px - x, pz - z);
            if dx * dx + dz * dz <= r2 {
                out.insert(*id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
        Set(u64, bool, f64, f64),
        Remove(u64),
        Query(bool, f64, f64, f64),
    }

    fn op() -> impl Strategy<Value = Op> {
        let c = -5_000.0..5_000.0f64;
        prop_oneof![
            (1..40u64, any::<bool>(), c.clone(), c.clone())
                .prop_map(|(i, t, x, z)| Op::Set(i, t, x, z)),
            (1..40u64).prop_map(Op::Remove),
            (any::<bool>(), c.clone(), c, 0.0..3_000.0f64)
                .prop_map(|(t, x, z, r)| Op::Query(t, x, z, r)),
        ]
    }

    proptest! {
        /// The grid answers every query as a scan of all positions would.
        #[test]
        fn matches_a_linear_scan(cell in 50.0..2_000.0f64, ops in prop::collection::vec(op(), 1..200)) {
            let mut grid = Grid::new(cell);
            let mut truth: HashMap<u64, (bool, f64, f64)> = HashMap::new();
            let name = |t: bool| if t { "Caucasus" } else { "Syria" };
            for op in ops {
                match op {
                    Op::Set(i, t, x, z) => {
                        grid.set(EntityId(i), name(t), x, z);
                        truth.insert(i, (t, x, z));
                    }
                    Op::Remove(i) => {
                        grid.remove(EntityId(i));
                        truth.remove(&i);
                    }
                    Op::Query(t, x, z, r) => {
                        let mut want: Vec<EntityId> = truth
                            .iter()
                            .filter(|(_, (pt, px, pz))| {
                                *pt == t && (px - x) * (px - x) + (pz - z) * (pz - z) <= r * r
                            })
                            .map(|(i, _)| EntityId(*i))
                            .collect();
                        want.sort();
                        prop_assert_eq!(grid.within(name(t), x, z, r), want);
                    }
                }
                prop_assert_eq!(grid.len(), truth.len());
            }
        }
    }

    #[test]
    fn a_huge_or_infinite_radius_finds_everything() {
        let mut g = Grid::new(100.0);
        g.set(EntityId(1), "Caucasus", -4.0e5, 3.0e5);
        g.set(EntityId(2), "Caucasus", 7.0e5, -9.0e5);
        for r in [f64::INFINITY, 1e300, 1e22] {
            assert_eq!(
                g.within("Caucasus", 0.0, 0.0, r),
                vec![EntityId(1), EntityId(2)],
                "{r}"
            );
        }
        assert!(g.within("Caucasus", 0.0, 0.0, f64::NAN).is_empty());
    }

    #[test]
    fn a_position_that_is_not_finite_removes() {
        let mut g = Grid::new(100.0);
        g.set(EntityId(1), "Caucasus", 0.0, 0.0);
        g.set(EntityId(1), "Caucasus", f64::NAN, 0.0);
        assert!(g.is_empty());
        assert!(g.within("Caucasus", 0.0, 0.0, 1e9).is_empty());
        assert!(g.within("Nowhere", 0.0, 0.0, 1.0).is_empty());
    }
}
