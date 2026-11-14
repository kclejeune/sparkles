//! Sorting new facts into a run and merging the sorted runs of an order into one. The key
//! of a fact is read once into a buffer of `(key, id)` entries, so the sort or merge
//! itself compares buffered keys, and the run is built from the sorted entries without
//! reading the keys again. An order sorts by key and then id, which is the order of the
//! entries. The ids of new facts ascend, so a sort by key that keeps the order of equal
//! keys sorts them by key and then id: a large batch on few threads sorts by least
//! significant digit, in one pass per digit of eleven bits in which its keys differ, and
//! other batches sort by comparison, in parallel. Two runs merge in one pass, and more are
//! sorted, which happens only when a compaction takes a tail of small runs. Every buffer
//! is made at the size [`sort_bytes`] or [`merge_bytes`] charges for and never grows.

use super::run::Run;
use crate::budget::{MemoryExceeded, Meter};
use rayon::prelude::*;

/// Batches smaller than this are sorted on the calling thread.
const PAR_MIN: usize = 1 << 14;

/// Batches of this size or more sort by digits when the pool has at most
/// [`RADIX_THREADS`] threads: a pass per digit over the entries costs less than a
/// comparison sort of them on one thread, and more threads sort by comparison in parallel.
const RADIX_MIN: usize = 1 << 16;
const RADIX_THREADS: usize = 3;

/// The bits of a digit: a pass counts into 2,048 buckets.
const DIGIT: u32 = 11;
const BUCKETS: usize = 1 << DIGIT;

/// A fact's key and its id, which sort by key and then id.
pub(crate) type Entry = (u64, u32);

/// Whether a batch of `n` new facts sorts by digits.
fn by_digits(n: usize) -> bool {
    n >= RADIX_MIN && rayon::current_num_threads() <= RADIX_THREADS
}

/// The heap of the buffers a sort of `n` new facts holds at once, besides the run it
/// builds: the keyed entries, and for a sort by digits a second buffer of them and the
/// counts of a pass.
pub(crate) fn sort_bytes(n: usize) -> usize {
    let e = merge_bytes(n);
    if by_digits(n) {
        2 * e + BUCKETS * size_of::<usize>()
    } else {
        e
    }
}

/// The heap of the buffers a merge of runs with `n` entries in all holds at once, besides
/// the run it builds: the keyed entries. A merge never sorts by digits, since the ids of
/// several runs do not ascend.
pub(crate) fn merge_bytes(n: usize) -> usize {
    n * size_of::<Entry>()
}

/// The ids keyed, in their order, in a buffer of exactly their number.
fn keyed(ids: &[u32], key: &(impl Fn(u32) -> u64 + Sync)) -> Vec<Entry> {
    let f = |&id: &u32| (key(id), id);
    let mut e: Vec<Entry> = Vec::with_capacity(ids.len());
    if ids.len() >= PAR_MIN {
        ids.par_iter().map(f).collect_into_vec(&mut e);
    } else {
        e.extend(ids.iter().map(f));
    }
    e
}

fn sort(e: &mut [Entry]) {
    if e.len() >= PAR_MIN {
        e.par_sort_unstable();
    } else {
        e.sort_unstable();
    }
}

/// Sort entries whose ids ascend by key, and so by key and then id, by least significant
/// digit: a counting pass per digit in which the keys differ, between the entries and a
/// second buffer of their size.
fn digit_sorted(e: Vec<Entry>) -> Vec<Entry> {
    let Some(&(first, _)) = e.first() else {
        return e;
    };
    let differ = e.iter().fold(0u64, |d, x| d | (x.0 ^ first));
    let mut src = e;
    let mut dst: Vec<Entry> = Vec::with_capacity(src.len());
    let mask = (BUCKETS - 1) as u64;
    for shift in (0..u64::BITS).step_by(DIGIT as usize) {
        if (differ >> shift) & mask == 0 {
            continue;
        }
        if dst.is_empty() {
            dst.resize(src.len(), (0, 0));
        }
        let digit = |x: &Entry| ((x.0 >> shift) & mask) as usize;
        let mut at = vec![0usize; BUCKETS];
        for x in &src {
            at[digit(x)] += 1;
        }
        let mut sum = 0;
        for c in &mut at {
            let n = *c;
            *c = sum;
            sum += n;
        }
        for &x in &src {
            let b = digit(&x);
            dst[at[b]] = x;
            at[b] += 1;
        }
        std::mem::swap(&mut src, &mut dst);
    }
    src
}

/// The entries of `ids`, which ascend, sorted by key and then id.
fn sorted_entries(ids: &[u32], key: &(impl Fn(u32) -> u64 + Sync)) -> Vec<Entry> {
    debug_assert!(ids.is_sorted(), "a batch's ids ascend");
    let mut e = keyed(ids, key);
    if by_digits(e.len()) {
        e = digit_sorted(e);
    } else {
        sort(&mut e);
    }
    e
}

/// The distinct keys of sorted entries for which `fresh` holds, in parallel for many.
fn count_fresh(e: &[Entry], fresh: &(impl Fn(u64) -> bool + Sync)) -> usize {
    let first = |i: usize| i == 0 || e[i - 1].0 != e[i].0;
    if e.len() >= PAR_MIN {
        (0..e.len())
            .into_par_iter()
            .with_min_len(PAR_MIN)
            .filter(|&i| first(i) && fresh(e[i].0))
            .count()
    } else {
        (0..e.len()).filter(|&i| first(i) && fresh(e[i].0)).count()
    }
}

/// The run of `ids`, which ascend, sorted by `key` and then id, and the number of its
/// distinct keys for which `fresh` holds, or of all of them when `all_fresh`: the
/// entries are keyed once and sorted, and the run is built from them.
pub(crate) fn sorted(
    ids: &[u32],
    key: &(impl Fn(u32) -> u64 + Sync),
    all_fresh: bool,
    fresh: &(impl Fn(u64) -> bool + Sync),
) -> (Run, usize) {
    let e = sorted_entries(ids, key);
    if all_fresh {
        return Run::from_sorted_counting(e.iter().copied());
    }
    let n = count_fresh(&e, fresh);
    (Run::from_sorted(e.iter().copied()), n)
}

/// Each key of sorted entries once, with whether `keep` accepts an id that has it.
fn key_groups<'a>(
    e: &'a [Entry],
    keep: &'a impl Fn(u32) -> bool,
) -> impl Iterator<Item = (u64, bool)> + 'a {
    let mut i = 0;
    std::iter::from_fn(move || {
        let k = e.get(i)?.0;
        let mut live = false;
        while i < e.len() && e[i].0 == k {
            live |= keep(e[i].1);
            i += 1;
        }
        Some((k, live))
    })
}

/// The run of the ids among `ids`, which ascend, that `keep` accepts, sorted by `key`
/// and then id, with the distinct keys of all of `ids` and those of them that only
/// refused ids have, sorted, in a list charged to `m` before it is made. The entries of
/// all of `ids` are sorted, so the sort's buffers are those of `ids.len()` entries.
pub(crate) fn sorted_live(
    ids: &[u32],
    key: &(impl Fn(u32) -> u64 + Sync),
    keep: &(impl Fn(u32) -> bool + Sync),
    m: &mut Meter<'_>,
) -> Result<(Run, usize, Vec<u64>), MemoryExceeded> {
    let mut e = sorted_entries(ids, key);
    let (keys, dead) =
        key_groups(&e, keep).fold((0, 0), |(n, d), (_, live)| (n + 1, d + usize::from(!live)));
    m.add(dead * size_of::<u64>())?;
    let mut ghosts = Vec::with_capacity(dead);
    ghosts.extend(key_groups(&e, keep).filter(|g| !g.1).map(|g| g.0));
    e.retain(|x| keep(x.1));
    Ok((Run::from_sorted(e.iter().copied()), keys, ghosts))
}

/// The keys of the ids of `runs` that `keep` refuses, `dropped` of them, sorted and
/// without repeats, in a list of capacity `dropped`.
pub(crate) fn dropped_keys(
    runs: &[Run],
    key: &impl Fn(u32) -> u64,
    keep: &impl Fn(u32) -> bool,
    dropped: usize,
) -> Vec<u64> {
    let mut k: Vec<u64> = Vec::with_capacity(dropped);
    for r in runs {
        k.extend(r.ids().iter().copied().filter(|&id| !keep(id)).map(key));
    }
    debug_assert_eq!(k.len(), dropped);
    k.sort_unstable();
    k.dedup();
    k
}

/// The runs merged into one, without the ids `keep` refuses.
pub(crate) fn merged(
    runs: &[Run],
    key: &(impl Fn(u32) -> u64 + Sync),
    keep: &(impl Fn(u32) -> bool + Sync),
) -> Run {
    let kept = |r: &Run| -> Vec<Entry> {
        let mut e = keyed(r.ids(), key);
        e.retain(|x| keep(x.1));
        e
    };
    match runs {
        [] => Run::from_sorted(std::iter::empty()),
        [r] => Run::from_sorted(kept(r).into_iter()),
        [a, b] => {
            let (a, b) = (kept(a), kept(b));
            Run::from_sorted(Merge {
                a: &a,
                b: &b,
                i: 0,
                j: 0,
            })
        }
        many => {
            let total: usize = many.iter().map(Run::len).sum();
            let mut all: Vec<Entry> = Vec::with_capacity(total);
            for r in many {
                all.extend(
                    r.ids()
                        .iter()
                        .filter(|&&id| keep(id))
                        .map(|&id| (key(id), id)),
                );
            }
            sort(&mut all);
            Run::from_sorted(all.into_iter())
        }
    }
}

/// The entries of two sorted buffers, in order.
struct Merge<'a> {
    a: &'a [Entry],
    b: &'a [Entry],
    i: usize,
    j: usize,
}

impl Iterator for Merge<'_> {
    type Item = Entry;

    #[inline]
    fn next(&mut self) -> Option<Entry> {
        let (a, b) = (self.a.get(self.i), self.b.get(self.j));
        match (a, b) {
            (Some(&x), Some(&y)) if x <= y => {
                self.i += 1;
                Some(x)
            }
            (Some(&x), None) => {
                self.i += 1;
                Some(x)
            }
            (_, Some(&y)) => {
                self.j += 1;
                Some(y)
            }
            (None, None) => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.a.len() - self.i + self.b.len() - self.j;
        (n, Some(n))
    }
}

impl ExactSizeIterator for Merge<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_equal_a_sort() {
        let keys: Vec<u64> = (0..300u32).map(|i| (i as u64 * 7919) % 50).collect();
        let key = |id: u32| keys[id as usize];
        let mut runs = Vec::new();
        for r in [0..100u32, 100..130, 130..300] {
            let ids: Vec<u32> = r.collect();
            runs.push(sorted(&ids, &key, true, &|_| true).0);
        }
        let keep = |id: u32| !id.is_multiple_of(11);
        let want = {
            let ids: Vec<u32> = (0..300).filter(|&id| keep(id)).collect();
            sorted(&ids, &key, true, &|_| true).0
        };
        assert_eq!(merged(&runs, &key, &keep).ids(), want.ids());
        // a new order counts the keys as it builds the run, and the probe counts the same
        let ids: Vec<u32> = (0..300).collect();
        assert_eq!(sorted(&ids, &key, true, &|_| true).1, 50);
        assert_eq!(sorted(&ids, &key, false, &|_| true).1, 50);
        assert_eq!(sorted(&ids, &key, false, &|k| k < 10).1, 10);
        let two = {
            let ids: Vec<u32> = (0..130).filter(|&id| keep(id)).collect();
            sorted(&ids, &key, true, &|_| true).0
        };
        let m = merged(&runs[..2], &key, &keep);
        assert_eq!(m.ids(), two.ids());
        assert_eq!(merged(&runs[..1], &key, &|_| true).len(), 100);
        assert_eq!(merged(&[], &key, &keep).len(), 0);
        // the entries sort by key and then id
        let ids = want.ids();
        assert!(
            ids.windows(2)
                .all(|w| (key(w[0]), w[0]) < (key(w[1]), w[1]))
        );
    }

    /// A sort by digits equals the comparison sort, over keys that differ in few digits,
    /// in every digit, or not at all, and its second buffer is all it allocates.
    #[test]
    fn digits_sort_like_comparisons() {
        let n = RADIX_MIN;
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for (lo, hi) in [(0u64, 1 << 20), (0, u64::MAX), (1 << 40, (1 << 40) + 1000)] {
            let keys: Vec<u64> = (0..n).map(|_| lo + next() % (hi - lo)).collect();
            let mut e: Vec<Entry> = Vec::with_capacity(n);
            e.extend(keys.iter().enumerate().map(|(i, &k)| (k, i as u32)));
            let mut want = e.clone();
            want.sort_unstable();
            let (got, peak) = crate::alloc_probe::peak_of(|| digit_sorted(e));
            assert_eq!(got, want);
            assert!(
                peak <= n * size_of::<Entry>() + BUCKETS * size_of::<usize>(),
                "peak {peak}"
            );
        }
        let same: Vec<Entry> = (0..n as u32).map(|i| (7, i)).collect();
        assert_eq!(digit_sorted(same.clone()), same);
        assert!(digit_sorted(Vec::new()).is_empty());
    }

    /// The buffers of a sort and of a merge hold what [`sort_bytes`] and [`merge_bytes`]
    /// charge for, besides the run. The batches stay under [`PAR_MIN`]: the parallel path
    /// makes the same buffers, and the probe would also count the pool's jobs the calling
    /// thread runs while it waits, which are not the sort's.
    #[test]
    fn buffers_fit_the_charge() {
        let n = PAR_MIN / 2;
        let ids: Vec<u32> = (0..n as u32).collect();
        let key = |id: u32| (id as u64 * 2_654_435_761) % 1000;
        let (run, peak) = crate::alloc_probe::peak_of(|| sorted(&ids, &key, true, &|_| true).0);
        assert!(
            peak <= sort_bytes(ids.len()) + Run::bytes_for(ids.len()),
            "sort peak {peak}"
        );
        let b = sorted(
            &(n as u32..2 * n as u32).collect::<Vec<u32>>(),
            &key,
            true,
            &|_| true,
        )
        .0;
        let runs = [run, b];
        let total = runs[0].len() + runs[1].len();
        let (_, peak) = crate::alloc_probe::peak_of(|| merged(&runs, &key, &|_| true));
        assert!(
            peak <= merge_bytes(total) + Run::bytes_for(total),
            "merge peak {peak}"
        );
        // a merge of runs over the digit threshold is charged its keyed entries alone, on
        // any number of threads
        assert_eq!(merge_bytes(RADIX_MIN), RADIX_MIN * size_of::<Entry>());
        assert!(sort_bytes(RADIX_MIN) >= merge_bytes(RADIX_MIN));
    }
}
