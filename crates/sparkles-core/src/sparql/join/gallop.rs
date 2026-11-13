//! The one galloping search of the join kernels.
//!
//! A merge join, a lower-bound zipper and an index join all step through a sorted
//! sequence from a position the previous key reached, and the next key is usually close.
//! Steps of doubling length from the start, followed by a binary search in the last
//! step, find a position `d` places on in about `2 log2 d` tests, so the search costs
//! little per key when the keys are close and still bounds a long gap by its logarithm.

/// The first index in `[from, n)` for which `before` does not hold, or `n`, where
/// `before` holds for a prefix of `[from, n)`.
#[inline]
pub(crate) fn gallop_to(from: usize, n: usize, before: impl Fn(usize) -> bool) -> usize {
    if from >= n || !before(from) {
        return from;
    }
    // `before(lo)` holds; the answer is in `(lo, hi]`
    let (mut lo, mut step) = (from, 1);
    let hi = loop {
        let hi = lo + step;
        if hi >= n {
            break n;
        }
        if !before(hi) {
            break hi;
        }
        lo = hi;
        step *= 2;
    };
    search(lo + 1, hi, before)
}

/// The first index of `slice` at or after `from` whose element fails `before`, or
/// `slice.len()`, where `before` holds for a prefix of the elements from `from`. With
/// `|x| *x < key` over a sorted slice it is the lower bound of `key` from `from`.
#[inline]
pub(crate) fn gallop<T>(slice: &[T], from: usize, before: impl Fn(&T) -> bool) -> usize {
    gallop_to(from, slice.len(), |i| before(&slice[i]))
}

/// The first index in `[lo, hi)` for which `before` is false, or `hi`, by binary search:
/// the last step of the gallop, and the whole search when there is no position to start
/// from.
#[inline]
pub(crate) fn search(mut lo: usize, mut hi: usize, before: impl Fn(usize) -> bool) -> usize {
    while lo < hi {
        let m = lo + (hi - lo) / 2;
        if before(m) {
            lo = m + 1;
        } else {
            hi = m;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift(mut x: u64) -> impl FnMut() -> u64 {
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        }
    }

    /// The galloping search finds what `partition_point` finds, from every start and
    /// for keys inside, between, below and above the slice's values.
    #[test]
    fn the_galloping_search_agrees_with_partition_point_on_random_sorted_slices() {
        let mut next = xorshift(0x2545_f491_4f6c_dd1d);
        for round in 0..400 {
            let len = (next() % [0, 1, 2, 7, 64, 1000][round % 6].max(1)) as usize;
            let domain = [3, 20, 1000, u64::MAX / 2][round % 4];
            let mut slice: Vec<u64> = (0..len).map(|_| next() % domain).collect();
            slice.sort_unstable();
            let mut keys: Vec<u64> = slice.clone();
            keys.extend((0..16).map(|_| next() % domain.saturating_add(2)));
            keys.extend([0, u64::MAX]);
            for &key in &keys {
                let expected = slice.partition_point(|x| *x < key);
                for from in (0..=len).filter(|f| f % 3 == 0 || *f == len || *f == expected) {
                    let got = gallop(&slice, from, |x| *x < key);
                    assert_eq!(got, expected.max(from), "len {len} key {key} from {from}");
                    assert_eq!(
                        gallop_to(from, len, |i| slice[i] < key),
                        got,
                        "len {len} key {key} from {from}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_empty_range_returns_its_start() {
        assert_eq!(gallop_to(5, 5, |_| true), 5);
        assert_eq!(gallop_to(7, 5, |_| true), 7);
        assert_eq!(gallop::<u8>(&[], 0, |_| true), 0);
    }
}
