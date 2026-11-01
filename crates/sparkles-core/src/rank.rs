//! Ranks of scored hits and their reciprocal rank fusion (Cormack, Clarke and Büttcher,
//! SIGIR 2009), which `spk:hybridSearch`, the MCP search tools and the memory layer
//! share.

use rustc_hash::FxHashMap;
use std::hash::Hash;

/// The fusion constant of the paper, which `spk:hybridSearch` uses unless a call names
/// another.
pub const RRF_K: f64 = 60.0;

/// What one list adds to a key's fused score: `weight / (k + rank)`.
pub fn rrf_term(weight: f64, k: f64, rank: usize) -> f64 {
    weight / (k + rank as f64)
}

/// The fused score of one key from its rank in each list, with weight 1, summed in the
/// order of `ranks`. A list that does not rank the key adds nothing.
pub fn rrf_score(k: f64, ranks: impl IntoIterator<Item = Option<usize>>) -> f64 {
    ranks
        .into_iter()
        .flatten()
        .map(|r| rrf_term(1.0, k, r))
        .sum()
}

/// Fuse ranked lists, each a weight and its keys' ranks. A key's score is the sum of
/// [`rrf_term`] over the lists that rank it, in list order. The result has each key once,
/// best first, with ties broken by key.
pub fn fuse_rrf<K, L>(lists: impl IntoIterator<Item = (f64, L)>, k: f64) -> Vec<(K, f64)>
where
    K: Ord + Hash + Clone,
    L: IntoIterator<Item = (K, usize)>,
{
    let mut fused: FxHashMap<K, f64> = FxHashMap::default();
    for (weight, list) in lists {
        for (key, rank) in list {
            *fused.entry(key).or_default() += rrf_term(weight, k, rank);
        }
    }
    let mut out: Vec<(K, f64)> = fused.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The rank of each key by its best score: one more than the number of keys with a
/// better score, so that tied keys share a rank. Higher scores are better unless
/// `lower_better` is set, and NaN scores rank nothing.
pub fn competition_ranks<K>(
    hits: impl IntoIterator<Item = (K, f64)>,
    lower_better: bool,
) -> FxHashMap<K, usize>
where
    K: Ord + Hash + Clone,
{
    let better = |a: f64, b: f64| if lower_better { a < b } else { a > b };
    let mut best: FxHashMap<K, f64> = FxHashMap::default();
    for (key, v) in hits {
        if v.is_nan() {
            continue;
        }
        best.entry(key)
            .and_modify(|b| {
                if better(v, *b) {
                    *b = v
                }
            })
            .or_insert(v);
    }
    let mut order: Vec<(K, f64)> = best.into_iter().collect();
    order.sort_by(|a, b| {
        let by = if lower_better {
            a.1.total_cmp(&b.1)
        } else {
            b.1.total_cmp(&a.1)
        };
        by.then_with(|| a.0.cmp(&b.0))
    });
    let mut out = FxHashMap::default();
    let mut rank = 0;
    for (i, (key, v)) in order.iter().enumerate() {
        if i == 0 || *v != order[i - 1].1 {
            rank = i + 1;
        }
        out.insert(key.clone(), rank);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_sums_weighted_terms_and_breaks_ties_by_key() {
        let text = vec![("a", 1), ("b", 2), ("c", 3)];
        let vector = vec![("c", 1), ("a", 3), ("d", 2)];
        let fused = fuse_rrf([(1.0, text), (1.0, vector)], RRF_K);
        let (one, two, three) = (1.0 / 61.0, 1.0 / 62.0, 1.0 / 63.0);
        // a and c tie, as do b and d, and the key breaks each tie
        assert_eq!(
            fused,
            vec![
                ("a", one + three),
                ("c", three + one),
                ("b", two),
                ("d", two)
            ]
        );
        // a weight scales its list's terms
        let fused = fuse_rrf(
            [(2.0, vec![("y", 1)]), (1.0, vec![("x", 1), ("z", 1)])],
            0.0,
        );
        assert_eq!(fused, vec![("y", 2.0), ("x", 1.0), ("z", 1.0)]);
        assert_eq!(rrf_score(RRF_K, [Some(1), None, Some(3)]), one + three);
    }

    #[test]
    fn competition_ranks_share_ties_and_skip_nan() {
        let hits = [
            ("a", 0.5),
            ("b", 0.9),
            ("c", 0.5),
            ("a", 0.7),
            ("d", f64::NAN),
        ];
        let ranks = competition_ranks(hits, false);
        assert_eq!(ranks.len(), 3);
        assert_eq!((ranks["b"], ranks["a"], ranks["c"]), (1, 2, 3));
        let ranks = competition_ranks([("a", 2.0), ("b", 1.0), ("c", 1.0)], true);
        assert_eq!((ranks["b"], ranks["c"], ranks["a"]), (1, 1, 3));
    }
}
