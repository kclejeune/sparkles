//! Z-sets: rows of ids with signed integer weights (spec F13 §3.1).
//!
//! A relation is a map from rows to non-zero weights. A bag of solutions has positive
//! weights, and a change has positive weights for rows added and negative ones for rows
//! removed. Rows are fixed-width id tuples aligned to the schema of the operator that
//! produced them, with [`Id::UNDEF`] for an unbound variable.

use crate::id::Id;
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;

/// One row: the ids of an operator's variables, in its schema's order.
pub type Row = Box<[Id]>;

/// Changes before consolidation: a row may occur more than once.
pub type Changes = Vec<(Row, i64)>;

/// The approximate memory of one stored row of `width` ids in a hash map.
pub fn row_bytes(width: usize) -> u64 {
    // the boxed ids, the box pointer and length, the weight and the map's slot overhead
    width as u64 * 8 + 16 + 8 + 16
}

/// A map from rows to non-zero weights.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZSet {
    map: FxHashMap<Row, i64>,
    /// the sum of the weights, kept as rows change
    total: i64,
}

impl ZSet {
    pub fn new() -> ZSet {
        ZSet::default()
    }

    /// Add `w` to the weight of `row`, dropping it when the sum is zero. Returns the
    /// weight before and after.
    pub fn add(&mut self, row: Row, w: i64) -> (i64, i64) {
        if w == 0 {
            let cur = self.get(&row);
            return (cur, cur);
        }
        self.total += w;
        match self.map.entry(row) {
            Entry::Occupied(mut e) => {
                let old = *e.get();
                let new = old + w;
                if new == 0 {
                    e.remove();
                } else {
                    *e.get_mut() = new;
                }
                (old, new)
            }
            Entry::Vacant(e) => {
                e.insert(w);
                (0, w)
            }
        }
    }

    pub fn get(&self, row: &[Id]) -> i64 {
        self.map.get(row).copied().unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The sum of the weights: the number of solutions of a bag.
    pub fn total(&self) -> i64 {
        self.total
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Row, i64)> + '_ {
        self.map.iter().map(|(r, w)| (r, *w))
    }

    pub fn into_changes(self) -> Changes {
        self.map.into_iter().collect()
    }

    /// Add every change.
    pub fn apply(&mut self, c: &[(Row, i64)]) {
        for (r, w) in c {
            self.add(r.clone(), *w);
        }
    }

    /// `self − other` as changes: what turns `other` into `self`.
    pub fn minus(&self, other: &ZSet) -> Changes {
        let mut out: Changes = Vec::new();
        for (r, w) in &self.map {
            let d = w - other.get(r);
            if d != 0 {
                out.push((r.clone(), d));
            }
        }
        for (r, w) in &other.map {
            if !self.map.contains_key(r) {
                out.push((r.clone(), -w));
            }
        }
        out
    }

    /// The approximate memory of the rows, each `width` ids wide.
    pub fn bytes(&self, width: usize) -> u64 {
        self.map.len() as u64 * row_bytes(width)
    }

    /// Replace every id by `f(id)` (a remapping between id spaces, which must be one to
    /// one on the ids present).
    pub fn remap(&mut self, f: &mut dyn FnMut(Id) -> Id) {
        let old = std::mem::take(&mut self.map);
        self.total = 0;
        for (r, w) in old {
            let r: Row = r.iter().map(|&i| f(i)).collect();
            self.add(r, w);
        }
    }
}

impl FromIterator<(Row, i64)> for ZSet {
    fn from_iter<T: IntoIterator<Item = (Row, i64)>>(iter: T) -> Self {
        let mut z = ZSet::new();
        for (r, w) in iter {
            z.add(r, w);
        }
        z
    }
}

/// Changes with each row once and no zero weights.
pub fn consolidate(c: Changes) -> Changes {
    if c.len() <= 1 {
        return c.into_iter().filter(|(_, w)| *w != 0).collect();
    }
    c.into_iter().collect::<ZSet>().into_changes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &[u64]) -> Row {
        v.iter().map(|&x| Id(x)).collect()
    }

    #[test]
    fn weights_add_and_cancel() {
        let mut z = ZSet::new();
        assert_eq!(z.add(r(&[1, 2]), 2), (0, 2));
        assert_eq!(z.add(r(&[1, 2]), -2), (2, 0));
        assert!(z.is_empty());
        z.add(r(&[3]), 1);
        let other: ZSet = [(r(&[3]), 2), (r(&[4]), 1)].into_iter().collect();
        let mut d = z.minus(&other);
        d.sort();
        assert_eq!(d, vec![(r(&[3]), -1), (r(&[4]), -1)]);
        assert_eq!(consolidate(vec![(r(&[1]), 1), (r(&[1]), -1)]), vec![]);
    }
}
