//! Triangle counts and clustering coefficients (spec F14 §4).
//!
//! The graph is read as undirected and simple. Edges are oriented from the node of lower
//! degree to the one of higher degree (the forward algorithm), which bounds the work by
//! the arboricity of the graph. Each triangle is found once and counted for its three
//! nodes with atomic adds, which do not depend on the order.

use super::{BLOCK, Control, Csr, Graph, Par};
use crate::error::Result;
use std::sync::atomic::{AtomicU32, Ordering};

/// The number of triangles each node is on.
pub fn count(g: &Graph, par: &Par, ctl: &Control) -> Result<Vec<u32>> {
    let s = g.simple(ctl)?;
    count_in(&s, par, ctl)
}

fn count_in(s: &Csr, par: &Par, ctl: &Control) -> Result<Vec<u32>> {
    let n = s.n();
    ctl.charge(n as u64 * 12 + s.nnz() as u64 * 4 + n as u64 * 8)?;
    // rank by (degree, index)
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_unstable_by_key(|&u| (s.deg(u), u));
    let mut rank = vec![0u32; n];
    for (r, &u) in order.iter().enumerate() {
        rank[u as usize] = r as u32;
    }
    // the neighbours of higher rank, sorted by rank
    let mut off = vec![0u64; n + 1];
    let mut adj: Vec<u32> = Vec::with_capacity(s.nnz() / 2);
    for u in 0..n as u32 {
        let start = adj.len();
        for &v in s.row(u) {
            if rank[v as usize] > rank[u as usize] {
                adj.push(rank[v as usize]);
            }
        }
        adj[start..].sort_unstable();
        off[u as usize + 1] = adj.len() as u64;
    }
    // work in rank space: node r has the neighbours up[r]
    let up = |r: u32| -> &[u32] {
        let u = order[r as usize] as usize;
        &adj[off[u] as usize..off[u + 1] as usize]
    };
    let tri: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    par.blocks(n, BLOCK / 4, |lo, hi| {
        ctl.check()?;
        for r in lo as u32..hi as u32 {
            let a = up(r);
            for &q in a {
                let b = up(q);
                // common elements of a and b
                let (mut i, mut j) = (0, 0);
                while i < a.len() && j < b.len() {
                    match a[i].cmp(&b[j]) {
                        std::cmp::Ordering::Less => i += 1,
                        std::cmp::Ordering::Greater => j += 1,
                        std::cmp::Ordering::Equal => {
                            let t = a[i];
                            for x in [r, q, t] {
                                tri[order[x as usize] as usize].fetch_add(1, Ordering::Relaxed);
                            }
                            i += 1;
                            j += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(tri.into_iter().map(|t| t.into_inner()).collect())
}

pub struct Clustering {
    pub coefficient: Vec<f64>,
    pub triangles: Vec<u32>,
}

/// The local clustering coefficient of each node: the triangles on it over the pairs of
/// its neighbours.
pub fn clustering(g: &Graph, par: &Par, ctl: &Control) -> Result<Clustering> {
    let s = g.simple(ctl)?;
    let triangles = count_in(&s, par, ctl)?;
    let coefficient = (0..s.n() as u32)
        .map(|u| {
            let d = s.deg(u) as f64;
            if d < 2.0 {
                0.0
            } else {
                2.0 * triangles[u as usize] as f64 / (d * (d - 1.0))
            }
        })
        .collect();
    Ok(Clustering {
        coefficient,
        triangles,
    })
}

/// The local clustering coefficient as LDBC Graphalytics defines it: the neighbourhood of
/// a node is its in- and out-neighbours, and the coefficient is the number of directed
/// edges among them over the number of ordered pairs of them.
pub fn clustering_graphalytics(g: &Graph, par: &Par, ctl: &Control) -> Result<Vec<f64>> {
    let n = g.n();
    ctl.charge(n as u64 * 8)?;
    let inn = g.inn();
    let mut out = vec![0f64; n];
    par.chunks_mut(&mut out, BLOCK / 4, |lo, chunk| {
        ctl.check()?;
        let mut nb: Vec<u32> = Vec::new();
        for (i, slot) in chunk.iter_mut().enumerate() {
            let v = (lo + i) as u32;
            nb.clear();
            nb.extend(g.out.row(v).iter().copied().filter(|&u| u != v));
            nb.extend(inn.row(v).iter().copied().filter(|&u| u != v));
            nb.sort_unstable();
            nb.dedup();
            let d = nb.len();
            if d < 2 {
                continue;
            }
            let mut edges = 0u64;
            for &u in &nb {
                let mut last = u32::MAX;
                for &w in g.out.row(u) {
                    // parallel edges count once
                    if w != last && w != u && nb.binary_search(&w).is_ok() {
                        edges += 1;
                    }
                    last = w;
                }
            }
            *slot = edges as f64 / (d as f64 * (d as f64 - 1.0));
        }
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn triangle_and_tail() {
        let g = ugraph(5, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4)]);
        let t = count(&g, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(t, vec![1, 1, 1, 0, 0]);
        let c = clustering(&g, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(c.coefficient[0], 1.0);
        assert!((c.coefficient[2] - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(c.coefficient[4], 0.0);
    }

    #[test]
    fn matches_triple_loop() {
        let mut x = 3u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        for _ in 0..20 {
            let n = 30u32;
            let e: Vec<(u32, u32)> = (0..120).map(|_| (rnd(n), rnd(n))).collect();
            let g = ugraph(n, &e);
            let mut adj = vec![vec![false; n as usize]; n as usize];
            for &(a, b) in &e {
                if a != b {
                    adj[a as usize][b as usize] = true;
                    adj[b as usize][a as usize] = true;
                }
            }
            let mut want = vec![0u32; n as usize];
            for a in 0..n as usize {
                for b in a + 1..n as usize {
                    for c in b + 1..n as usize {
                        if adj[a][b] && adj[b][c] && adj[a][c] {
                            want[a] += 1;
                            want[b] += 1;
                            want[c] += 1;
                        }
                    }
                }
            }
            assert_eq!(
                count(&g, &Par::single(), &Control::unlimited()).unwrap(),
                want
            );
            assert_eq!(
                count(&g, &Par::with_threads(3), &Control::unlimited()).unwrap(),
                want
            );
        }
    }

    #[test]
    fn graphalytics_lcc_on_an_undirected_triangle() {
        let g = ugraph(3, &[(0, 1), (1, 2), (2, 0)]);
        let c = clustering_graphalytics(&g, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(c, vec![1.0, 1.0, 1.0]);
    }
}
