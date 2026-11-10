//! One sorted run of an order: fact ids sorted by a key of their facts, with a fence
//! key every [`FENCE`] entries and a coarse fence key every [`COARSE`] fence keys. A
//! probe searches the coarse keys, which are one in 512 entries and stay in cache for a
//! run of tens of millions, then one block of [`COARSE`] fence keys, and then reads the
//! keys of at most [`FENCE`] facts, so it costs a handful of dependent reads whatever
//! the run's size. The entries with one key usually end in the window the lower bound
//! found, and the upper bound is then searched in that window alone.

use std::ops::Range;

/// Entries between two fence keys.
pub(crate) const FENCE: usize = 8;
/// Fence keys between two coarse fence keys.
pub(crate) const COARSE: usize = 64;
/// Registers of a run's sketch of its distinct keys.
pub(crate) const SKETCH: usize = 256;

/// A sorted run. The ids are sorted by `(key1, id)` of their facts, and the run keeps
/// the first key of every [`FENCE`]-th entry, every [`COARSE`]-th of those again, the
/// interval of its ids, the number of distinct first keys and a sketch of them, from
/// which an order of several runs estimates the distinct keys of their union.
pub(crate) struct Run {
    ids: Vec<u32>,
    fence: Vec<u64>,
    /// `fence[j * COARSE]` for every `j`
    coarse: Vec<u64>,
    /// the ids are in `[lo, hi)`
    lo: u32,
    hi: u32,
    distinct: u32,
    /// a HyperLogLog sketch of the distinct first keys, see [`sketch_add`]
    sketch: [u8; SKETCH],
}

/// Add key `k` to a sketch: the top bits of a mix of the key pick a register, which keeps
/// the longest run of leading zeros, plus one, of the bits after them.
#[inline]
fn sketch_add(s: &mut [u8; SKETCH], k: u64) {
    // the finalizer of splitmix64, a mix other than the hashers of the tables
    let mut h = k;
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    let i = (h >> 56) as usize;
    let rank = ((h << 8) | 0x80).leading_zeros() as u8 + 1;
    if rank > s[i] {
        s[i] = rank;
    }
}

/// The distinct keys a sketch has seen, by HyperLogLog, with linear counting for the
/// small range.
pub(crate) fn sketch_estimate(s: &[u8; SKETCH]) -> usize {
    let m = SKETCH as f64;
    let mut sum = 0.0;
    let mut zeros = 0usize;
    for &r in s {
        sum += 2f64.powi(-i32::from(r));
        if r == 0 {
            zeros += 1;
        }
    }
    let alpha = 0.7213 / (1.0 + 1.079 / m);
    let e = alpha * m * m / sum;
    let e = if e <= 2.5 * m && zeros > 0 {
        m * (m / zeros as f64).ln()
    } else {
        e
    };
    e.round() as usize
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
        let mut coarse = Vec::with_capacity(n.div_ceil(FENCE * COARSE));
        let mut sketch = [0u8; SKETCH];
        let (mut lo, mut hi, mut distinct) = (u32::MAX, 0u32, 0u32);
        let mut prev = None;
        for (i, (k, id)) in entries.enumerate() {
            if i % FENCE == 0 {
                if i % (FENCE * COARSE) == 0 {
                    coarse.push(k);
                }
                fence.push(k);
            }
            if prev != Some(k) {
                distinct += 1;
                sketch_add(&mut sketch, k);
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
            coarse,
            lo,
            hi,
            distinct,
            sketch,
        }
    }

    /// Join the run's sketch into `s`, register by register.
    pub fn sketch_into(&self, s: &mut [u8; SKETCH]) {
        for (a, &b) in s.iter_mut().zip(&self.sketch) {
            *a = (*a).max(b);
        }
    }

    /// The heap of a run of `n` entries built at that size.
    pub fn bytes_for(n: usize) -> usize {
        n * size_of::<u32>() + (n.div_ceil(FENCE) + n.div_ceil(FENCE * COARSE)) * size_of::<u64>()
    }

    pub fn heap_bytes(&self) -> usize {
        self.ids.capacity() * size_of::<u32>()
            + (self.fence.capacity() + self.coarse.capacity()) * size_of::<u64>()
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

    /// The number of fence keys before the bound: those below `k`, or with `upper` those
    /// at most `k`. The coarse keys name the block of [`COARSE`] fence keys that holds it.
    #[inline]
    fn fences_before(&self, k: u64, upper: bool) -> usize {
        let below = |x: u64| if upper { x <= k } else { x < k };
        let c = self.coarse.partition_point(|&x| below(x));
        if c == 0 {
            return 0;
        }
        // the fence key at (c - 1) * COARSE is before the bound and the one at c * COARSE,
        // if there is one, is not
        let start = (c - 1) * COARSE + 1;
        let end = (c * COARSE).min(self.fence.len());
        start + self.fence[start..end].partition_point(|&x| below(x))
    }

    /// The first entry at or after `start` in the window that ends before fence key `f`
    /// whose first key is at least `k` (or, with `upper`, more than `k`), given that the
    /// entry before `start` is before the bound and the one at `f * FENCE`, if any, is
    /// not.
    #[inline]
    fn in_window(
        &self,
        start: usize,
        f: usize,
        k: u64,
        key1: &impl Fn(u32) -> u64,
        upper: bool,
    ) -> usize {
        let end = (f * FENCE).min(self.ids.len());
        let w = &self.ids[start..end];
        let i = if upper {
            w.partition_point(|&id| key1(id) <= k)
        } else {
            w.partition_point(|&id| key1(id) < k)
        };
        start + i
    }

    /// The first entry whose first key is at least `k` (or, with `upper`, more than
    /// `k`): the fence keys name the window of [`FENCE`] entries that holds it.
    #[inline]
    fn bound(&self, k: u64, key1: &impl Fn(u32) -> u64, upper: bool) -> usize {
        let f = self.fences_before(k, upper);
        if f == 0 {
            return 0;
        }
        self.in_window((f - 1) * FENCE + 1, f, k, key1, upper)
    }

    /// The entries whose first key is `k`.
    #[inline]
    pub fn range(&self, k: u64, key1: &impl Fn(u32) -> u64) -> Range<usize> {
        let a = self.bound(k, key1, false);
        if a == self.ids.len() || key1(self.ids[a]) != k {
            return a..a;
        }
        // the entries with key `k` end in the window of `a` when the next fence key is
        // past `k` or there is none: then the upper bound is in that window
        let f = a / FENCE + 1;
        if self.fence.get(f).is_none_or(|&x| x > k) {
            return a..self.in_window(a + 1, f, k, key1, true);
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

    /// Ranges over several coarse blocks: short ones, one that spans fences and coarse
    /// blocks, and bounds that fall on the first entry of a block or a window.
    #[test]
    fn coarse_blocks_and_long_ranges() {
        // keys: 0..1000 with three facts each, then 2000 facts with key 1000, then
        // 1..=3 facts with the keys from 1001 on
        let mut keys: Vec<u64> = (0..1000u64).flat_map(|k| [k, k, k]).collect();
        keys.extend(std::iter::repeat_n(1000u64, 2000));
        let mut k = 1001;
        while keys.len() < 7000 {
            for _ in 0..(k % 3 + 1) {
                keys.push(k);
            }
            k += 1;
        }
        let key = |id: u32| keys[id as usize];
        let r = Run::new((0..keys.len() as u32).collect(), &key);
        assert_eq!(r.fence.len(), keys.len().div_ceil(FENCE));
        assert_eq!(r.coarse.len(), keys.len().div_ceil(FENCE * COARSE));
        assert_eq!(r.heap_bytes(), Run::bytes_for(keys.len()));
        let last = *keys.last().unwrap();
        for k in 0..=last + 1 {
            let want = keys.partition_point(|&x| x < k)..keys.partition_point(|&x| x <= k);
            assert_eq!(r.range(k, &key), want, "key {k}");
        }
        // every window start and block start as a key that is or is not present
        for i in (0..keys.len()).step_by(FENCE) {
            let k = keys[i];
            assert_eq!(r.range(k, &key).start, keys.partition_point(|&x| x < k));
            assert!(r.range(k + 100_000, &key).is_empty());
        }
    }

    /// Two runs over the same keys count them once, disjoint runs count both, and the
    /// estimate stays within a tenth over several thousand keys.
    #[test]
    fn sketches_estimate_the_union() {
        let key = |id: u32| (id % 5000) as u64;
        let a = Run::new((0..5000).collect(), &key);
        let b = Run::new((5000..10000).collect(), &key);
        let c = Run::new((10000..15000).map(|i| i + 5000).collect(), &|id| id as u64);
        let union = |runs: &[&Run]| {
            let mut s = [0u8; SKETCH];
            for r in runs {
                r.sketch_into(&mut s);
            }
            sketch_estimate(&s)
        };
        assert_eq!(a.distinct(), 5000);
        let same = union(&[&a, &b]);
        assert!((4500..=5500).contains(&same), "same keys: {same}");
        let both = union(&[&a, &c]);
        assert!((9000..=11000).contains(&both), "disjoint keys: {both}");
        // the small range is linear counting, exact for a few keys
        let one = Run::new(vec![7], &|_| 42);
        let two = Run::new(vec![8], &|_| 43);
        assert_eq!(union(&[&one, &one]), 1);
        assert_eq!(union(&[&one, &two]), 2);
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
