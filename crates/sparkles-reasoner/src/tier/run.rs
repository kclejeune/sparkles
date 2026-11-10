//! One sorted run of an order: fact ids sorted by a key of their facts, with a fence
//! key every [`FENCE`] entries. A probe searches the fence keys, which are few and stay
//! in cache, and then reads the keys of at most [`FENCE`] facts, so it costs about three
//! dependent reads of the fact columns whatever the run's size.

use std::ops::Range;

/// Entries between two fence keys.
pub(crate) const FENCE: usize = 8;

/// A sorted run. The ids are sorted by `(key1, id)` of their facts, and the run keeps
/// the first key of every [`FENCE`]-th entry, the interval of its ids and the number of
/// distinct first keys.
pub(crate) struct Run {
    ids: Vec<u32>,
    fence: Vec<u64>,
    /// the ids are in `[lo, hi)`
    lo: u32,
    hi: u32,
    distinct: u32,
}

impl Run {
    /// A run over `ids`, which are sorted by `(key1, id)` already.
    #[cfg(test)]
    pub fn new(ids: Vec<u32>, key1: &impl Fn(u32) -> u64) -> Run {
        Run::from_sorted(ids.iter().map(|&id| (key1(id), id)))
    }

    /// A run over `(key1, id)` entries sorted by key and then id, which are read once:
    /// the buffers are made at the entries' number and never grow.
    pub fn from_sorted(entries: impl ExactSizeIterator<Item = (u64, u32)>) -> Run {
        let n = entries.len();
        let mut ids = Vec::with_capacity(n);
        let mut fence = Vec::with_capacity(n.div_ceil(FENCE));
        let (mut lo, mut hi, mut distinct) = (u32::MAX, 0u32, 0u32);
        let mut prev = None;
        for (i, (k, id)) in entries.enumerate() {
            if i % FENCE == 0 {
                fence.push(k);
            }
            if prev != Some(k) {
                distinct += 1;
                prev = Some(k);
            }
            lo = lo.min(id);
            hi = hi.max(id);
            ids.push(id);
        }
        let (lo, hi) = if ids.is_empty() { (0, 0) } else { (lo, hi + 1) };
        Run {
            ids,
            fence,
            lo,
            hi,
            distinct,
        }
    }

    /// The heap of a run of `n` entries built at that size.
    pub fn bytes_for(n: usize) -> usize {
        n * size_of::<u32>() + n.div_ceil(FENCE) * size_of::<u64>()
    }

    pub fn heap_bytes(&self) -> usize {
        self.ids.capacity() * size_of::<u32>() + self.fence.capacity() * size_of::<u64>()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    #[inline]
    pub fn ids(&self) -> &[u32] {
        &self.ids
    }

    #[inline]
    pub fn hi(&self) -> u32 {
        self.hi
    }

    pub fn distinct(&self) -> u32 {
        self.distinct
    }

    /// Whether every id of the run is in `[lo, hi)`.
    #[inline]
    pub fn within(&self, lo: u32, hi: u32) -> bool {
        self.lo >= lo && self.hi <= hi
    }

    /// Whether an id of the run may be in `[lo, hi)`.
    #[inline]
    pub fn overlaps(&self, lo: u32, hi: u32) -> bool {
        self.lo < hi && self.hi > lo
    }

    /// The first entry whose first key is at least `k` (or, with `upper`, more than
    /// `k`): the fence keys name the window of [`FENCE`] entries that holds it.
    #[inline]
    fn bound(&self, k: u64, key1: &impl Fn(u32) -> u64, upper: bool) -> usize {
        let n = self.ids.len();
        let f = if upper {
            self.fence.partition_point(|&x| x <= k)
        } else {
            self.fence.partition_point(|&x| x < k)
        };
        if f == 0 {
            return 0;
        }
        // the entry at (f - 1) * FENCE is before the bound and the one at f * FENCE, if
        // there is one, is not
        let start = (f - 1) * FENCE + 1;
        let end = (f * FENCE).min(n);
        let w = &self.ids[start..end];
        let i = if upper {
            w.partition_point(|&id| key1(id) <= k)
        } else {
            w.partition_point(|&id| key1(id) < k)
        };
        start + i
    }

    /// The entries whose first key is `k`.
    #[inline]
    pub fn range(&self, k: u64, key1: &impl Fn(u32) -> u64) -> Range<usize> {
        let a = self.bound(k, key1, false);
        if a == self.ids.len() || key1(self.ids[a]) != k {
            return a..a;
        }
        a..self.bound(k, key1, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_the_keys() {
        // fact i has key i / 3, so each key has three facts, over several fences
        let key = |id: u32| (id / 3) as u64;
        let ids: Vec<u32> = (0..100).collect();
        let r = Run::new(ids, &key);
        assert_eq!(r.fence.len(), 13);
        assert_eq!(r.distinct(), 34);
        assert!(r.within(0, 100) && !r.within(1, 100) && r.overlaps(99, 200));
        for k in 0..34u64 {
            let g = r.range(k, &key);
            let want: Vec<u32> = (0..100).filter(|&i| key(i) == k).collect();
            assert_eq!(&r.ids()[g], &want[..], "key {k}");
        }
        assert!(r.range(34, &key).is_empty());
        assert!(r.range(1000, &key).is_empty());
        let e = Run::new(Vec::new(), &key);
        assert!(e.range(0, &key).is_empty());
        assert_eq!((e.lo, e.hi), (0, 0));
    }

    #[test]
    fn sparse_keys() {
        let keys: Vec<u64> = vec![5, 5, 9, 20, 21, 21, 21, 21, 21, 40, 41];
        let key = |id: u32| keys[id as usize];
        let r = Run::new((0..keys.len() as u32).collect(), &key);
        assert_eq!(r.range(5, &key), 0..2);
        assert_eq!(r.range(9, &key), 2..3);
        assert_eq!(r.range(21, &key), 4..9);
        assert_eq!(r.range(41, &key), 10..11);
        assert!(r.range(6, &key).is_empty());
        assert!(r.range(0, &key).is_empty());
        assert!(r.range(50, &key).is_empty());
    }
}
