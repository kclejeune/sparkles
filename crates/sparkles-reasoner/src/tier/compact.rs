//! Merging the sorted runs of an order into one. The keys of a run's facts are read
//! once into a buffer of `(key1, key2, id)` entries, in the run's order, so the merge
//! itself compares buffered keys. Two runs merge in one pass, and more are sorted, which
//! happens only when a compaction takes a tail of small runs. Every buffer is made at
//! the size [`scratch_bytes`] charges for and never grows.

use super::run::Run;
use rayon::prelude::*;

/// Batches smaller than this are sorted on the calling thread.
const PAR_MIN: usize = 1 << 14;

type Entry = (u64, u64, u32);

/// The heap of the buffers a merge or sort of `n` entries holds at once: the keyed
/// entries and the ids.
pub(crate) fn scratch_bytes(n: usize) -> usize {
    n * size_of::<Entry>() + n * size_of::<u32>()
}

/// The ids keyed, in their order, in a buffer of exactly their number.
fn keyed(ids: &[u32], keys: &(impl Fn(u32) -> (u64, u64) + Sync)) -> Vec<Entry> {
    let f = |&id: &u32| {
        let (a, b) = keys(id);
        (a, b, id)
    };
    let mut e: Vec<Entry> = Vec::with_capacity(ids.len());
    if ids.len() >= PAR_MIN {
        ids.par_iter().map(f).collect_into_vec(&mut e);
    } else {
        e.extend(ids.iter().map(f));
    }
    e
}

fn unkeyed(e: &[Entry]) -> Vec<u32> {
    let mut out = Vec::with_capacity(e.len());
    out.extend(e.iter().map(|x| x.2));
    out
}

/// Sort `ids` by `(key1, key2)` of their facts, as a new run: the entries are keyed
/// once, sorted and unkeyed.
pub(crate) fn sorted(ids: &[u32], keys: &(impl Fn(u32) -> (u64, u64) + Sync)) -> Vec<u32> {
    let mut e = keyed(ids, keys);
    if e.len() >= PAR_MIN {
        e.par_sort_unstable();
    } else {
        e.sort_unstable();
    }
    unkeyed(&e)
}

/// The ids of `runs` in one sorted sequence, without the ids `keep` refuses.
pub(crate) fn merged(
    runs: &[Run],
    keys: &(impl Fn(u32) -> (u64, u64) + Sync),
    keep: &(impl Fn(u32) -> bool + Sync),
) -> Vec<u32> {
    let kept = |r: &Run| -> Vec<Entry> {
        let mut e = keyed(r.ids(), keys);
        e.retain(|x| keep(x.2));
        e
    };
    match runs {
        [] => Vec::new(),
        [r] => unkeyed(&kept(r)),
        [a, b] => {
            let (a, b) = (kept(a), kept(b));
            let mut out = Vec::with_capacity(a.len() + b.len());
            let (mut i, mut j) = (0, 0);
            while i < a.len() && j < b.len() {
                if a[i] <= b[j] {
                    out.push(a[i].2);
                    i += 1;
                } else {
                    out.push(b[j].2);
                    j += 1;
                }
            }
            out.extend(a[i..].iter().map(|x| x.2));
            out.extend(b[j..].iter().map(|x| x.2));
            out
        }
        many => {
            let total: usize = many.iter().map(Run::len).sum();
            let mut all: Vec<Entry> = Vec::with_capacity(total);
            for r in many {
                all.extend(r.ids().iter().filter(|&&id| keep(id)).map(|&id| {
                    let (a, b) = keys(id);
                    (a, b, id)
                }));
            }
            if all.len() >= PAR_MIN {
                all.par_sort_unstable();
            } else {
                all.sort_unstable();
            }
            unkeyed(&all)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_equal_a_sort() {
        let keys: Vec<(u64, u64)> = (0..300u32)
            .map(|i| ((i as u64 * 7919) % 50, (i as u64 * 104_729) % 13))
            .collect();
        let key = |id: u32| keys[id as usize];
        let key1 = |id: u32| keys[id as usize].0;
        let mut runs = Vec::new();
        for r in [0..100u32, 100..130, 130..300] {
            let ids: Vec<u32> = r.collect();
            runs.push(Run::new(sorted(&ids, &key), &key1));
        }
        let keep = |id: u32| !id.is_multiple_of(11);
        let want = {
            let ids: Vec<u32> = (0..300).filter(|&id| keep(id)).collect();
            sorted(&ids, &key)
        };
        assert_eq!(merged(&runs, &key, &keep), want);
        let two = {
            let ids: Vec<u32> = (0..130).filter(|&id| keep(id)).collect();
            sorted(&ids, &key)
        };
        assert_eq!(merged(&runs[..2], &key, &keep), two);
        assert_eq!(merged(&runs[..1], &key, &|_| true).len(), 100);
        assert!(merged(&[], &key, &keep).is_empty());
    }

    /// The buffers of a sort and of a merge hold what [`scratch_bytes`] charges for.
    #[test]
    fn buffers_fit_the_charge() {
        let ids: Vec<u32> = (0..(1 << 15)).collect();
        let key = |id: u32| ((id as u64 * 2_654_435_761) % 1000, id as u64);
        let key1 = |id: u32| key(id).0;
        let (run, peak) = crate::alloc_probe::peak_of(|| Run::new(sorted(&ids, &key), &key1));
        assert!(
            peak <= scratch_bytes(ids.len()) + Run::bytes_for(ids.len()),
            "sort peak {peak}"
        );
        let b = Run::new(
            sorted(&(1 << 15..1 << 16).collect::<Vec<u32>>(), &key),
            &key1,
        );
        let runs = [run, b];
        let total = runs[0].len() + runs[1].len();
        let (_, peak) = crate::alloc_probe::peak_of(|| merged(&runs, &key, &|_| true));
        assert!(peak <= scratch_bytes(total), "merge peak {peak}");
    }
}
