//! Sorting new facts into a run and merging the sorted runs of an order into one. The key
//! of a fact is read once into a buffer of `(key, id)` entries, so the sort or merge
//! itself compares buffered keys, and the run is built from the sorted entries without
//! reading the keys again. An order sorts by key and then id, which is the order of the
//! entries. Two runs merge in one pass, and more are sorted, which happens only when a
//! compaction takes a tail of small runs. Every buffer is made at the size
//! [`scratch_bytes`] charges for and never grows.

use super::run::Run;
use rayon::prelude::*;

/// Batches smaller than this are sorted on the calling thread.
const PAR_MIN: usize = 1 << 14;

/// A fact's key and its id, which sort by key and then id.
pub(crate) type Entry = (u64, u32);

/// The heap of the buffers a sort or merge of `n` entries holds at once, besides the run
/// it builds: the keyed entries.
pub(crate) fn scratch_bytes(n: usize) -> usize {
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

/// The run of `ids` sorted by `key` and then id: the entries are keyed once and sorted,
/// and the run is built from them.
pub(crate) fn sorted(ids: &[u32], key: &(impl Fn(u32) -> u64 + Sync)) -> Run {
    let mut e = keyed(ids, key);
    sort(&mut e);
    Run::from_sorted(e.iter().copied())
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
            runs.push(sorted(&ids, &key));
        }
        let keep = |id: u32| !id.is_multiple_of(11);
        let want = {
            let ids: Vec<u32> = (0..300).filter(|&id| keep(id)).collect();
            sorted(&ids, &key)
        };
        assert_eq!(merged(&runs, &key, &keep).ids(), want.ids());
        let two = {
            let ids: Vec<u32> = (0..130).filter(|&id| keep(id)).collect();
            sorted(&ids, &key)
        };
        let m = merged(&runs[..2], &key, &keep);
        assert_eq!(m.ids(), two.ids());
        assert_eq!(m.distinct(), two.distinct());
        assert_eq!(merged(&runs[..1], &key, &|_| true).len(), 100);
        assert_eq!(merged(&[], &key, &keep).len(), 0);
        // the entries sort by key and then id
        let ids = want.ids();
        assert!(
            ids.windows(2)
                .all(|w| (key(w[0]), w[0]) < (key(w[1]), w[1]))
        );
    }

    /// The buffers of a sort and of a merge hold what [`scratch_bytes`] charges for,
    /// besides the run.
    #[test]
    fn buffers_fit_the_charge() {
        let ids: Vec<u32> = (0..(1 << 15)).collect();
        let key = |id: u32| (id as u64 * 2_654_435_761) % 1000;
        // the first parallel call of a thread sets up the pool's registry, which is not
        // the sort's
        drop(sorted(&ids, &key));
        let (run, peak) = crate::alloc_probe::peak_of(|| sorted(&ids, &key));
        assert!(
            peak <= scratch_bytes(ids.len()) + Run::bytes_for(ids.len()),
            "sort peak {peak}"
        );
        let b = sorted(&(1 << 15..1 << 16).collect::<Vec<u32>>(), &key);
        let runs = [run, b];
        let total = runs[0].len() + runs[1].len();
        let (_, peak) = crate::alloc_probe::peak_of(|| merged(&runs, &key, &|_| true));
        assert!(
            peak <= scratch_bytes(total) + Run::bytes_for(total),
            "merge peak {peak}"
        );
    }
}
