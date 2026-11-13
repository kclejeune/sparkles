//! One order of a relation: its facts sorted by a key, as a list of immutable sorted
//! runs. A merge of new facts appends one run, and the runs before it are compacted
//! into fewer, larger runs, so that a probe searches at most [`MAX_RUNS`] of them
//! besides the runs an iteration in progress protects.
//!
//! A run holds the ids of one or several merges, and a merge's ids form an interval, so
//! each run's ids lie in an interval and the runs' intervals ascend. A probe over an id
//! range reads the runs whose interval it meets and filters the entries of a run it meets
//! only in part. The semi-naive ranges of the engine are merge boundaries, and a
//! compaction never takes a run above the floor the engine sets to the start of the
//! iteration's delta ([`Tier::set_compaction_floor`](super::Tier::set_compaction_floor)),
//! so those ranges meet whole runs.
//!
//! An order counts the distinct first keys its facts have had, dead facts included, as
//! the hash lists before it did. A merge counts the keys of its run that no older run
//! holds, and a compaction that drops dead facts keeps the keys it leaves without an
//! entry as ghosts, so the count follows the facts and not how they fall into runs.

use super::Cands;
use super::run::Run;
use crate::budget::{MemoryExceeded, Meter};
use std::ops::Range;

/// The most runs an order keeps below the compaction floor after a compaction and the
/// run a merge appends.
pub(crate) const MAX_RUNS: usize = 8;

#[derive(Default)]
pub(crate) struct Order {
    runs: Vec<Run>,
    len: usize,
    /// dead facts still in the runs, kept by the orders over the whole tier (a
    /// relation counts its own)
    pub dead: u32,
    /// the distinct first keys of every fact the order has held
    keys: usize,
    /// first keys whose facts a compaction dropped, dead, and which no run holds, sorted
    ghosts: Vec<u64>,
}

impl Order {
    /// Append `r`, of which `fresh` first keys are new to the order (see
    /// [`holds`](Self::holds)).
    pub fn push(&mut self, r: Run, fresh: usize) {
        if r.len() > 0 {
            self.len += r.len();
            self.keys += fresh;
            self.runs.push(r);
        }
    }

    /// [`push`](Self::push), charging the list's new allocation to `m` before it grows
    /// and releasing the old one after.
    pub fn push_charged(
        &mut self,
        r: Run,
        fresh: usize,
        m: &mut Meter<'_>,
    ) -> Result<(), MemoryExceeded> {
        if r.len() == 0 {
            return Ok(());
        }
        let (len, cap) = (self.runs.len(), self.runs.capacity());
        if len == cap {
            // the vector doubles, from four
            let new = (cap * 2).max(4);
            m.add(new * size_of::<Run>())?;
            self.runs.reserve_exact(new - len);
            m.sub(cap * size_of::<Run>());
        }
        self.push(r, fresh);
        Ok(())
    }

    /// An order of one run, built over facts with `keys` distinct first keys of which
    /// `ghosts`, sorted, are left without a fact in the run, the ghosts charged to `m`.
    pub fn built(
        run: Run,
        keys: usize,
        ghosts: Vec<u64>,
        m: &mut Meter<'_>,
    ) -> Result<Order, MemoryExceeded> {
        m.add(ghosts.capacity() * size_of::<u64>())?;
        let mut o = Order {
            ghosts,
            ..Order::default()
        };
        o.push_charged(run, keys, m)?;
        if o.runs.is_empty() {
            o.keys = keys;
        }
        Ok(o)
    }

    /// Whether a fact the order has held had first key `k`: a run holds it, or a ghost.
    #[inline]
    pub fn holds(&self, k: u64, key1: &impl Fn(u32) -> u64) -> bool {
        self.runs.iter().any(|r| r.holds(k, key1)) || self.ghosts.binary_search(&k).is_ok()
    }

    /// Keep as ghosts the keys of `dropped`, sorted and without repeats, that no run
    /// holds any more, charging the list's growth to `m`.
    pub fn keep_ghosts(
        &mut self,
        dropped: &[u64],
        key1: &impl Fn(u32) -> u64,
        m: &mut Meter<'_>,
    ) -> Result<(), MemoryExceeded> {
        let gone: Vec<u64> = dropped
            .iter()
            .copied()
            .filter(|&k| !self.runs.iter().any(|r| r.holds(k, key1)))
            .collect();
        if gone.is_empty() {
            return Ok(());
        }
        let (cap, n) = (self.ghosts.capacity(), self.ghosts.len() + gone.len());
        m.add(n * size_of::<u64>())?;
        let mut all = Vec::with_capacity(n);
        let (mut i, mut j) = (0, 0);
        while i < self.ghosts.len() || j < gone.len() {
            let k = match (self.ghosts.get(i), gone.get(j)) {
                (Some(&a), Some(&b)) if a < b => {
                    i += 1;
                    a
                }
                (Some(&a), Some(&b)) if b < a => {
                    j += 1;
                    b
                }
                (Some(&a), Some(_)) => {
                    i += 1;
                    j += 1;
                    a
                }
                (Some(&a), None) => {
                    i += 1;
                    a
                }
                (None, Some(&b)) => {
                    j += 1;
                    b
                }
                (None, None) => unreachable!(),
            };
            all.push(k);
        }
        self.ghosts = all;
        m.sub(cap * size_of::<u64>());
        Ok(())
    }

    #[inline]
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Entries of every run, dead facts included.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The distinct first keys of every fact the order has held, dead ones included,
    /// whatever runs hold them.
    pub fn distinct(&self) -> usize {
        self.keys
    }

    pub fn heap_bytes(&self) -> usize {
        self.runs.capacity() * size_of::<Run>()
            + self.runs.iter().map(Run::heap_bytes).sum::<usize>()
            + self.ghosts.capacity() * size_of::<u64>()
    }

    /// Whether a fact with an id of `lo` or more is in the order.
    #[cfg(test)]
    pub fn since(&self, lo: u32) -> bool {
        self.runs.iter().any(|r| r.hi() > lo)
    }

    /// Entries whose id is in `[lo, hi)`.
    #[cfg(test)]
    pub fn count_in(&self, lo: u32, hi: u32) -> usize {
        self.runs
            .iter()
            .map(|r| {
                if !r.overlaps(lo, hi) {
                    0
                } else if r.within(lo, hi) {
                    r.len()
                } else {
                    r.ids().iter().filter(|&&id| id >= lo && id < hi).count()
                }
            })
            .sum()
    }

    /// Add to `out` the entries with first key `k` and an id in `[lo, hi)`, run by run.
    #[inline]
    pub fn probe<'a>(
        &'a self,
        k: u64,
        lo: u32,
        hi: u32,
        key1: &impl Fn(u32) -> u64,
        out: &mut Cands<'a>,
    ) {
        for r in &self.runs {
            if !r.overlaps(lo, hi) {
                continue;
            }
            let g = r.range(k, key1);
            if !g.is_empty() {
                out.push_ids(&r.ids()[g], !r.within(lo, hi));
            }
        }
    }

    /// The runs whose ids are all below `floor`: a prefix, since the intervals ascend.
    fn below(&self, floor: u32) -> usize {
        self.runs.partition_point(|r| r.hi() <= floor)
    }

    /// The runs a compaction merges into one before a merge appends its run, among the
    /// runs below `floor`: with `all`, every one of them, and otherwise from the newest
    /// back while the next older run is at most twice their size, and further back until
    /// at most [`MAX_RUNS`] minus one remain for the appended run. `None` when there is
    /// nothing to merge.
    pub fn compaction(&self, floor: u32, all: bool) -> Option<Range<usize>> {
        let n = self.below(floor);
        if n == 0 || (n < 2 && !all) {
            return None;
        }
        if all {
            return Some(0..n);
        }
        let mut a = n - 1;
        let mut acc = self.runs[a].len();
        while a > 0 && self.runs[a - 1].len() <= 2 * acc {
            a -= 1;
            acc += self.runs[a].len();
        }
        a = a.min(MAX_RUNS - 2);
        (a + 1 < n).then_some(a..n)
    }

    /// Replace the runs `which` by `run`.
    pub fn replace(&mut self, which: Range<usize>, run: Run) {
        let removed: usize = self.runs[which.clone()].iter().map(Run::len).sum();
        self.len = self.len - removed + run.len();
        if run.len() > 0 {
            self.runs.splice(which, [run]);
        } else {
            self.runs.drain(which);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ids: Range<u32>) -> Run {
        Run::new(ids.collect(), &|id| id as u64)
    }

    #[test]
    fn compaction_keeps_geometric_runs_and_the_cap() {
        let mut o = Order::default();
        o.push(run(0..1000), 0);
        assert_eq!(o.compaction(u32::MAX, false), None);
        o.push(run(1000..1600), 0);
        // the older run is within twice the newer: merge both
        assert_eq!(o.compaction(u32::MAX, false), Some(0..2));
        // unless the floor protects the newer one
        assert_eq!(o.compaction(1000, false), None);
        let mut o = Order::default();
        o.push(run(0..1000), 0);
        o.push(run(1000..1100), 0);
        o.push(run(1100..1150), 0);
        // the two small runs merge, the large one stays
        assert_eq!(o.compaction(u32::MAX, false), Some(1..3));
        assert_eq!(o.compaction(1100, false), None);
        assert_eq!(o.compaction(1100, true), Some(0..2));
        assert_eq!(o.compaction(1000, true), Some(0..1));
        let mut o = Order::default();
        let mut at = 0;
        for n in [10_000, 1000, 100, 10, 10, 10, 10, 10, 10, 10] {
            o.push(run(at..at + n), 0);
            at += n;
        }
        assert_eq!(o.runs().len(), 10);
        // the tail of equal runs merges, the run of 100 is within twice the tail and the
        // run of 1000 is not
        assert_eq!(o.compaction(u32::MAX, false), Some(2..10));
        assert_eq!(o.count_in(0, at), at as usize);
        assert_eq!(o.count_in(10_500, 11_050), 550);
        assert!(o.since(at - 1) && !o.since(at));
        // the cap leaves room for the appended run
        let mut o = Order::default();
        let mut at = 0;
        for n in [6561, 2187, 729, 243, 81, 27, 9, 3, 1] {
            o.push(run(at..at + n), 0);
            at += n;
        }
        assert_eq!(o.compaction(u32::MAX, false), Some(6..9));
    }

    /// Append the run of `ids` by `key`, counting its new keys as a merge does.
    fn push_keyed(o: &mut Order, mut ids: Vec<u32>, key: &impl Fn(u32) -> u64) {
        ids.sort_by_key(|&id| (key(id), id));
        let mut keys: Vec<u64> = ids.iter().map(|&id| key(id)).collect();
        keys.dedup();
        let fresh = keys.iter().filter(|&&k| !o.holds(k, key)).count();
        o.push(Run::new(ids, key), fresh);
    }

    /// The distinct keys are those of the facts, however the runs split them, and a
    /// compaction that drops the dead facts of a key keeps the key counted.
    #[test]
    fn distinct_keys_follow_the_facts() {
        let key = |id: u32| (id % 100) as u64;
        let mut o = Order::default();
        for r in [0..1000u32, 1000..2000, 2000..3000] {
            push_keyed(&mut o, r.collect(), &key);
        }
        assert_eq!(o.runs().len(), 3);
        assert_eq!(o.distinct(), 100);
        let mut one = Order::default();
        push_keyed(&mut one, (0..3000).collect(), &key);
        assert_eq!(one.distinct(), 100);
        let mut o = Order::default();
        push_keyed(&mut o, (0..100).collect(), &|id| id as u64);
        push_keyed(&mut o, (100..200).collect(), &|id| id as u64);
        assert_eq!(o.distinct(), 200);
        // ids below 10 are dead: a compaction drops them, and keys 0 to 9 are ghosts
        // and facts 40 and 41, added after, have keys 3 and 25
        let key = |id: u32| match id {
            40 => 3,
            41 => 25,
            _ => id as u64 / 2,
        };
        let mut o = Order::default();
        push_keyed(&mut o, (0..20).collect(), &key);
        push_keyed(&mut o, (20..40).collect(), &key);
        assert_eq!(o.distinct(), 20);
        let budget = crate::budget::Budget::unlimited();
        let mut m = Meter::new(&budget, false);
        let which = o.compaction(u32::MAX, true).unwrap();
        let run = super::super::compact::merged(&o.runs()[which.clone()], &key, &|id| id >= 10);
        o.replace(which, run);
        o.keep_ghosts(&[0, 1, 2, 3, 4], &key, &mut m).unwrap();
        assert_eq!(o.ghosts, [0, 1, 2, 3, 4]);
        assert!(o.holds(3, &key) && o.holds(7, &key) && !o.holds(20, &key));
        // a fact with a ghost's key is not a new key, and one past them is
        push_keyed(&mut o, vec![40, 41], &key);
        assert_eq!(o.distinct(), 21);
    }

    #[test]
    fn probes_filter_partial_runs() {
        let key = |id: u32| (id % 4) as u64;
        let mut o = Order::default();
        for r in [0..40u32, 40..80] {
            let mut ids: Vec<u32> = r.collect();
            ids.sort_by_key(|&id| (key(id), id));
            o.push(Run::new(ids, &key), 0);
        }
        let mut c = Cands::with_bounds(20, 60);
        o.probe(2, 20, 60, &key, &mut c);
        let got: Vec<u32> = c.iter().collect();
        let want: Vec<u32> = (20..60).filter(|&i| key(i) == 2).collect();
        assert_eq!(got, want);
        assert_eq!(c.len(), 20, "an upper bound before the filter");
    }
}
