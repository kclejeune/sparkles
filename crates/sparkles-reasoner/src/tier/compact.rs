//! Merging the sorted runs of an order into one. The keys of a run's facts are read
//! once into a buffer of `(key1, key2, id)` entries, in the run's order, so the merge
//! itself compares buffered keys. Two runs merge in one pass, and more are sorted, which
//! happens only when a compaction takes a tail of small runs.

use super::run::Run;
use rayon::prelude::*;

/// Batches smaller than this are sorted on the calling thread.
const PAR_MIN: usize = 1 << 14;

type Entry = (u64, u64, u32);

/// The heap of the buffers a merge of `n` entries uses: the keyed entries and the ids.
pub(crate) fn scratch_bytes(n: usize) -> usize {
    n * size_of::<Entry>() + n * size_of::<u32>()
}

/// Sort `ids` by `(key1, key2)` of their facts, as a new run: the entries are keyed
/// once, sorted and unkeyed.
pub(crate) fn sorted(ids: &[u32], keys: &(impl Fn(u32) -> (u64, u64) + Sync)) -> Vec<u32> {
    let mut e: Vec<Entry> = Vec::with_capacity(ids.len());
    if ids.len() >= PAR_MIN {
        e.par_extend(ids.par_iter().map(|&id| {
            let (a, b) = keys(id);
            (a, b, id)
        }));
        e.par_sort_unstable();
    } else {
        e.extend(ids.iter().map(|&id| {
            let (a, b) = keys(id);
            (a, b, id)
        }));
        e.sort_unstable();
    }
    e.iter().map(|x| x.2).collect()
}

/// The ids of `runs` in one sorted sequence, without the ids `keep` refuses.
pub(crate) fn merged(
    runs: &[Run],
    keys: &(impl Fn(u32) -> (u64, u64) + Sync),
    keep: &(impl Fn(u32) -> bool + Sync),
) -> Vec<u32> {
    let keyed = |r: &Run| -> Vec<Entry> {
        let f = |&id: &u32| {
            keep(id).then(|| {
                let (a, b) = keys(id);
                (a, b, id)
            })
        };
        if r.len() >= PAR_MIN {
            r.ids().par_iter().filter_map(f).collect()
        } else {
            r.ids().iter().filter_map(f).collect()
        }
    };
    match runs {
        [] => Vec::new(),
        [r] => keyed(r).iter().map(|x| x.2).collect(),
        [a, b] => {
            let (a, b) = (keyed(a), keyed(b));
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
            let mut all: Vec<Entry> = Vec::with_capacity(many.iter().map(Run::len).sum());
            for r in many {
                all.extend(keyed(r));
            }
            if all.len() >= PAR_MIN {
                all.par_sort_unstable();
            } else {
                all.sort_unstable();
            }
            all.iter().map(|x| x.2).collect()
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
        let keep = |id: u32| id % 11 != 0;
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
}
