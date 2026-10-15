//! Node similarity by Jaccard and overlap coefficients on the out-neighbour sets of the
//! nodes (spec F14 §4).
//!
//! Candidate pairs are the nodes that share a neighbour, found through the in-edges of
//! each neighbour, so pairs with nothing in common are never looked at. Each node keeps
//! its `top_k` most similar nodes, and the pair `(u, v)` is reported from the side of `u`.

use super::{BLOCK, Control, Csr, Graph, Par, bad};
use crate::error::Result;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Jaccard,
    Overlap,
}

#[derive(Clone, Debug)]
pub struct Params {
    pub metric: Metric,
    pub top_k: usize,
    pub similarity_cutoff: f64,
    /// nodes with fewer neighbours are left out
    pub degree_cutoff: usize,
    /// the most candidate pairs one run looks at
    pub max_pairs: u64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            metric: Metric::Jaccard,
            top_k: 10,
            similarity_cutoff: 1e-42,
            degree_cutoff: 1,
            max_pairs: 10_000_000,
        }
    }
}

/// The rows of `c` without repeated neighbours (the same rows when there are none).
fn distinct(c: &Csr, ctl: &Control) -> Result<Option<Csr>> {
    let has_repeat = (0..c.n() as u32).any(|u| c.row(u).windows(2).any(|w| w[0] == w[1]));
    if !has_repeat {
        return Ok(None);
    }
    ctl.charge(c.bytes())?;
    let mut off = vec![0u64; c.n() + 1];
    let mut adj = Vec::with_capacity(c.nnz());
    for u in 0..c.n() as u32 {
        let start = adj.len();
        for &v in c.row(u) {
            if adj.len() == start || adj[adj.len() - 1] != v {
                adj.push(v);
            }
        }
        off[u as usize + 1] = adj.len() as u64;
    }
    Ok(Some(Csr { off, adj, w: None }))
}

/// The similar pairs `(node, other, similarity)`, each node's best first, in node order.
pub fn run(g: &Graph, p: &Params, par: &Par, ctl: &Control) -> Result<Vec<(u32, u32, f64)>> {
    if p.top_k == 0 {
        return Err(bad("nodeSimilarity", "topK must be above 0"));
    }
    let n = g.n();
    let owned = distinct(&g.out, ctl)?;
    let out: &Csr = owned.as_ref().unwrap_or(&g.out);
    // Keep the one call-owned incoming reservation through all worker joins.
    let local;
    let shared;
    let inn: &Csr = if g.symmetric {
        out
    } else if owned.is_some() {
        local = out.transpose(ctl)?;
        &local
    } else {
        shared = g.inn(ctl)?;
        &shared
    };
    ctl.charge(n as u64 * 12)?;
    let pairs = AtomicU64::new(0);
    let parts = par.blocks(n, BLOCK / 4, |lo, hi| {
        ctl.check()?;
        let mut count = vec![0u32; n];
        let mut touched: Vec<u32> = Vec::new();
        let mut found: Vec<(u32, u32, f64)> = Vec::new();
        let mut mine: Vec<(u32, f64)> = Vec::new();
        for u in lo as u32..hi as u32 {
            let du = out.deg(u);
            if du < p.degree_cutoff.max(1) {
                continue;
            }
            touched.clear();
            for &x in out.row(u) {
                for &v in inn.row(x) {
                    if v == u {
                        continue;
                    }
                    if count[v as usize] == 0 {
                        touched.push(v);
                    }
                    count[v as usize] += 1;
                }
            }
            let seen = pairs.fetch_add(touched.len() as u64, Ordering::Relaxed) + touched.len() as u64;
            if seen > p.max_pairs {
                return Err(bad(
                    "nodeSimilarity",
                    format!(
                        "more than {} candidate pairs; raise algo:maxPairs, or project fewer predicates",
                        p.max_pairs
                    ),
                ));
            }
            mine.clear();
            for &v in &touched {
                let inter = count[v as usize] as f64;
                count[v as usize] = 0;
                let dv = out.deg(v);
                if dv < p.degree_cutoff.max(1) {
                    continue;
                }
                let sim = match p.metric {
                    Metric::Jaccard => inter / ((du + dv) as f64 - inter),
                    Metric::Overlap => inter / du.min(dv) as f64,
                };
                if sim >= p.similarity_cutoff {
                    mine.push((v, sim));
                }
            }
            mine.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            mine.truncate(p.top_k);
            found.extend(mine.iter().map(|&(v, s)| (u, v, s)));
        }
        Ok(found)
    })?;
    let total: usize = parts.iter().map(|p| p.len()).sum();
    ctl.charge(total as u64 * 16)?;
    Ok(parts.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn jaccard_of_the_spec_example() {
        // u1 = 0, u2 = 1, u3 = 2; x = 3, y = 4
        let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
        let r = run(
            &g,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert!(r.contains(&(0, 1, 1.0)));
        assert!(r.contains(&(0, 2, 0.5)));
        assert!(r.contains(&(1, 2, 0.5)));
        let o = run(
            &g,
            &Params {
                metric: Metric::Overlap,
                ..Params::default()
            },
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        // u3's single neighbour is inside u1's set
        assert!(o.contains(&(0, 2, 1.0)));
    }

    #[test]
    fn top_k_and_cutoff() {
        let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
        let p = Params {
            top_k: 1,
            similarity_cutoff: 0.75,
            ..Params::default()
        };
        let r = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(r, vec![(0, 1, 1.0), (1, 0, 1.0)]);
    }

    #[test]
    fn candidate_limit_fails() {
        let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
        let p = Params {
            max_pairs: 1,
            ..Params::default()
        };
        assert!(run(&g, &p, &Par::single(), &Control::unlimited()).is_err());
    }

    #[test]
    fn matches_set_operations() {
        let mut x = 21u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        let n = 20u32;
        let mut e: Vec<(u32, u32)> = (0..80).map(|_| (rnd(n), rnd(n))).collect();
        e.sort_unstable();
        e.dedup();
        let g = digraph(n, &e);
        let p = Params {
            top_k: 100,
            ..Params::default()
        };
        let r = run(&g, &p, &Par::with_threads(2), &Control::unlimited()).unwrap();
        for u in 0..n {
            for v in 0..n {
                if u == v {
                    continue;
                }
                let a: std::collections::BTreeSet<u32> =
                    e.iter().filter(|x| x.0 == u).map(|x| x.1).collect();
                let b: std::collections::BTreeSet<u32> =
                    e.iter().filter(|x| x.0 == v).map(|x| x.1).collect();
                let inter = a.intersection(&b).count() as f64;
                let want = if inter > 0.0 {
                    inter / ((a.len() + b.len()) as f64 - inter)
                } else {
                    0.0
                };
                let got = r.iter().find(|x| x.0 == u && x.1 == v).map(|x| x.2);
                match got {
                    Some(s) => assert!((s - want).abs() < 1e-12),
                    None => assert_eq!(inter, 0.0),
                }
            }
        }
    }
    #[test]
    fn r7_deduplicated_transpose_reserves_retention_and_scratch() {
        let g = digraph(4, &[(0, 2), (0, 2), (1, 2)]);
        let ctl = Control::new(Default::default(), None, 100);
        assert!(matches!(
            run(&g, &Params::default(), &Par::single(), &ctl),
            Err(crate::Error::BudgetExceeded(_))
        ));
        // The existing distinct outgoing charge remains outside this R7 owner.
        assert_eq!(ctl.remaining(), 48);
        let got = run(
            &g,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert!(got.iter().any(|&(u, v, s)| u == 0 && v == 1 && s == 1.0));
        let sym = ugraph(3, &[(0, 1), (0, 1), (1, 2)]);
        let _ = run(
            &sym,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
    }
}
