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
}

impl Order {
    pub fn push(&mut self, r: Run) {
        if r.len() > 0 {
            self.len += r.len();
            self.runs.push(r);
        }
    }

    /// [`push`](Self::push), charging the list's new allocation to `m` before it grows
    /// and releasing the old one after.
    pub fn push_charged(&mut self, r: Run, m: &mut Meter<'_>) -> Result<(), MemoryExceeded> {
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
        self.push(r);
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

    /// Distinct first keys, summed over the runs: exact for one run and otherwise an
    /// upper bound.
    pub fn distinct(&self) -> usize {
        self.runs.iter().map(|r| r.distinct() as usize).sum()
    }

    pub fn heap_bytes(&self) -> usize {
        self.runs.capacity() * size_of::<Run>()
            + self.runs.iter().map(Run::heap_bytes).sum::<usize>()
    }

    /// Whether a fact with an id of `lo` or more is in the order.
    pub fn since(&self, lo: u32) -> bool {
        self.runs.iter().any(|r| r.hi() > lo)
    }

    /// Entries whose id is in `[lo, hi)`.
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
        o.push(run(0..1000));
        assert_eq!(o.compaction(u32::MAX, false), None);
        o.push(run(1000..1600));
        // the older run is within twice the newer: merge both
        assert_eq!(o.compaction(u32::MAX, false), Some(0..2));
        // unless the floor protects the newer one
        assert_eq!(o.compaction(1000, false), None);
        let mut o = Order::default();
        o.push(run(0..1000));
        o.push(run(1000..1100));
        o.push(run(1100..1150));
        // the two small runs merge, the large one stays
        assert_eq!(o.compaction(u32::MAX, false), Some(1..3));
        assert_eq!(o.compaction(1100, false), None);
        assert_eq!(o.compaction(1100, true), Some(0..2));
        assert_eq!(o.compaction(1000, true), Some(0..1));
        let mut o = Order::default();
        let mut at = 0;
        for n in [10_000, 1000, 100, 10, 10, 10, 10, 10, 10, 10] {
            o.push(run(at..at + n));
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
            o.push(run(at..at + n));
            at += n;
        }
        assert_eq!(o.compaction(u32::MAX, false), Some(6..9));
    }

    #[test]
    fn probes_filter_partial_runs() {
        let key = |id: u32| (id % 4) as u64;
        let mut o = Order::default();
        for r in [0..40u32, 40..80] {
            let mut ids: Vec<u32> = r.collect();
            ids.sort_by_key(|&id| (key(id), id));
            o.push(Run::new(ids, &key));
        }
        let mut c = Cands::with_bounds(20, 60);
        o.probe(2, 20, 60, &key, &mut c);
        let got: Vec<u32> = c.iter().collect();
        let want: Vec<u32> = (20..60).filter(|&i| key(i) == 2).collect();
        assert_eq!(got, want);
        assert_eq!(c.len(), 20, "an upper bound before the filter");
    }
}
