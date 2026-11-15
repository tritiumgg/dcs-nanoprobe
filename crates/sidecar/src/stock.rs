//! A warehouse's inventory as the platform keeps it: counts by item, in the
//! shape of the `WAREHOUSE` record's `stock` and of the `WAREHOUSE_SET` and
//! `WAREHOUSE_ADJUST` payloads (spec/commands.md, "Layout: WAREHOUSE_SET"):
//! `weapon` and `aircraft` map names to counts, `liquids` maps the liquid
//! number to an amount.

use std::collections::BTreeMap;

use nanoprobe_transport::codec::Lv;

use crate::sim::wire::lv;

/// One countable thing in a warehouse.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    /// A weapon, by DCS name.
    Weapon(String),
    /// An aircraft type, by DCS name.
    Aircraft(String),
    /// A liquid, by number (0 jet fuel, 1 gasoline, 2 MW-50, 3 diesel).
    Liquid(u32),
}

/// Counts by item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stock {
    /// Weapons by name.
    pub weapon: BTreeMap<String, f64>,
    /// Aircraft by type name.
    pub aircraft: BTreeMap<String, f64>,
    /// Liquids by number.
    pub liquids: BTreeMap<u32, f64>,
}

impl Stock {
    /// Read a `stock` table. A part that is missing or not a table is empty;
    /// an entry that is not a name and a number is left out.
    pub fn from_lv(v: &Lv) -> Stock {
        let mut s = Stock::default();
        for (key, part) in [("weapon", &mut s.weapon), ("aircraft", &mut s.aircraft)] {
            if let Some(Lv::Table(pairs)) = lv::get(v, key) {
                for (k, n) in pairs {
                    if let (Lv::String(k), Lv::Number(n)) = (k, n)
                        && let Ok(k) = String::from_utf8(k.clone())
                    {
                        part.insert(k, *n);
                    }
                }
            }
        }
        if let Some(Lv::Table(pairs)) = lv::get(v, "liquids") {
            for (k, n) in pairs {
                if let (Lv::Number(k), Lv::Number(n)) = (k, n)
                    && *k >= 0.0
                    && k.fract() == 0.0
                {
                    s.liquids.insert(*k as u32, *n);
                }
            }
        }
        s
    }

    /// The `stock` table. A part with no entries is left out.
    pub fn to_lv(&self) -> Lv {
        let mut parts: Vec<(&str, Lv)> = Vec::new();
        let named = |m: &BTreeMap<String, f64>| {
            Lv::Table(m.iter().map(|(k, v)| (lv::s(k), lv::n(*v))).collect())
        };
        if !self.weapon.is_empty() {
            parts.push(("weapon", named(&self.weapon)));
        }
        if !self.aircraft.is_empty() {
            parts.push(("aircraft", named(&self.aircraft)));
        }
        if !self.liquids.is_empty() {
            parts.push((
                "liquids",
                Lv::Table(
                    self.liquids
                        .iter()
                        .map(|(k, v)| (lv::n(f64::from(*k)), lv::n(*v)))
                        .collect(),
                ),
            ));
        }
        lv::obj(parts)
    }

    /// Whether no item is named.
    pub fn is_empty(&self) -> bool {
        self.weapon.is_empty() && self.aircraft.is_empty() && self.liquids.is_empty()
    }

    /// The count of an item, if named.
    pub fn get(&self, item: &Item) -> Option<f64> {
        match item {
            Item::Weapon(n) => self.weapon.get(n),
            Item::Aircraft(n) => self.aircraft.get(n),
            Item::Liquid(n) => self.liquids.get(n),
        }
        .copied()
    }

    /// Name an item with a count.
    pub fn set(&mut self, item: Item, count: f64) {
        match item {
            Item::Weapon(n) => self.weapon.insert(n, count),
            Item::Aircraft(n) => self.aircraft.insert(n, count),
            Item::Liquid(n) => self.liquids.insert(n, count),
        };
    }

    /// Every named item with its count.
    pub fn items(&self) -> Vec<(Item, f64)> {
        let mut v: Vec<(Item, f64)> = Vec::new();
        v.extend(
            self.weapon
                .iter()
                .map(|(k, c)| (Item::Weapon(k.clone()), *c)),
        );
        v.extend(
            self.aircraft
                .iter()
                .map(|(k, c)| (Item::Aircraft(k.clone()), *c)),
        );
        v.extend(self.liquids.iter().map(|(k, c)| (Item::Liquid(*k), *c)));
        v
    }

    /// Name every item of `other`, replacing counts.
    pub fn merge(&mut self, other: &Stock) {
        for (i, c) in other.items() {
            self.set(i, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stock_round_trips_through_its_table() {
        let mut s = Stock::default();
        s.set(Item::Weapon("AIM-9X".into()), 4.0);
        s.set(Item::Aircraft("F-16C".into()), 2.0);
        s.set(Item::Liquid(0), 1000.0);
        assert_eq!(Stock::from_lv(&s.to_lv()), s);
        assert_eq!(s.get(&Item::Liquid(0)), Some(1000.0));
        assert_eq!(s.get(&Item::Liquid(1)), None);
        assert!(Stock::from_lv(&Lv::Nil).is_empty());
    }

    #[test]
    fn a_table_with_other_keys_reads_what_it_can() {
        let t = lv::obj(vec![
            ("weapon", lv::n(1.0)),
            (
                "aircraft",
                lv::obj(vec![("A", lv::s("x")), ("B", lv::n(3.0))]),
            ),
            ("extra", lv::n(9.0)),
        ]);
        let s = Stock::from_lv(&t);
        assert!(s.weapon.is_empty());
        assert_eq!(s.aircraft.len(), 1);
        assert_eq!(s.aircraft["B"], 3.0);
    }
}
