//! Triangle counts and clustering coefficients (spec F14 §4).
//!
//! The graph is read as undirected and simple. Edges are oriented from the node of lower
//! degree to the one of higher degree (the forward algorithm), which bounds the work by
//! the arboricity of the graph. Each triangle is found once and counted for its three
//! nodes with atomic adds, which do not depend on the order.

use super::memory::{Buffer, Growing, Work};
use super::{BLOCK, CHECK_EVERY, Control, Csr, Graph, Par};
use crate::error::Result;
use std::sync::atomic::{AtomicU32, Ordering};

/// The number of triangles each node is on.
pub fn count(g: &Graph, par: &Par, ctl: &Control) -> Result<Vec<u32>> {
    let s = g.simple(ctl)?;
    count_in(&s, par, ctl)
}

fn count_in(s: &Csr, par: &Par, ctl: &Control) -> Result<Vec<u32>> {
    let n = s.n();
    let mut work = Work::new(ctl)?;
    // rank by (degree, index)
    let mut order = Buffer::collect("triangles-order", n, 0..n as u32, ctl)?;
    ctl.check()?;
    order.sort_unstable_by_key(|&u| (s.deg(u), u));
    ctl.check()?;
    let mut rank = Buffer::<u32>::zeroed("triangles-rank", n, ctl)?;
    for (c, chunk) in order.chunks(CHECK_EVERY).enumerate() {
        work.tick_n(chunk.len(), "triangles-rank")?;
        for (i, &u) in chunk.iter().enumerate() {
            rank[u as usize] = (c * CHECK_EVERY + i) as u32;
        }
    }
    // the neighbours of higher rank, sorted by rank: half the stored edges
    let mut off = Buffer::<u64>::zeroed("triangles-offsets", n + 1, ctl)?;
    let mut adj = Buffer::<u32>::new("triangles-edges", s.nnz() / 2, ctl)?;
    // the longest list of higher neighbours, which bounds the steps of one merge
    let mut longest = 0usize;
    for u in 0..n as u32 {
        let start = adj.len();
        let row = s.row(u);
        work.tick_n(row.len() + 1, "triangles-orient")?;
        let ru = rank[u as usize];
        for &v in row {
            let rv = rank[v as usize];
            if rv > ru {
                adj.push(rv)?;
            }
        }
        adj[start..].sort_unstable();
        longest = longest.max(adj.len() - start);
        off[u as usize + 1] = adj.len() as u64;
    }
    // work in rank space: node r has the neighbours up[r]
    let tri_buffer = Buffer::collect(
        "triangles-counts",
        n,
        (0..n).map(|_| AtomicU32::new(0)),
        ctl,
    )?;
    // the hot loops index plain slices
    let (order_s, adj_s, off_s, tri): (&[u32], &[u32], &[u64], &[AtomicU32]) =
        (&order, &adj, &off, &tri_buffer);
    let up = |r: u32| -> &[u32] {
        let u = order_s[r as usize] as usize;
        &adj_s[off_s[u] as usize..off_s[u + 1] as usize]
    };
    par.blocks(n, BLOCK / 4, |lo, hi| {
        let mut work = Work::new(ctl)?;
        // the common elements of a and b, each counted for r, q and itself
        let merge = |r: u32, q: u32, a: &[u32], b: &[u32]| {
            let (mut i, mut j) = (0, 0);
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        let t = a[i];
                        for x in [r, q, t] {
                            tri[order_s[x as usize] as usize].fetch_add(1, Ordering::Relaxed);
                        }
                        i += 1;
                        j += 1;
                    }
                }
            }
        };
        for r in lo as u32..hi as u32 {
            let a = up(r);
            // each merge takes at most a.len() + longest steps
            let bound = a
                .len()
                .saturating_mul(a.len().saturating_add(longest))
                .saturating_add(1);
            if bound <= CHECK_EVERY {
                // the merges of a node this small stay within one interval: they are
                // counted together by their bound before they start
                work.tick_n(bound, "triangles-node")?;
                for &q in a {
                    merge(r, q, a, up(q));
                }
            } else {
                large(&mut work, r, a, &up, &merge)?;
            }
        }
        Ok(())
    })?;
    drop((order, rank, off, adj));
    Ok(into_values(tri_buffer.into_output()))
}

/// The counts as plain values, in the allocation that held them, so the output keeps
/// its charge and is not copied. The standard library collects a map over a vector's
/// own iterator in place when the two types share a layout, as `AtomicU32` and `u32`
/// do, and a test checks that it still does.
fn into_values(counts: Vec<AtomicU32>) -> Vec<u32> {
    counts.into_iter().map(AtomicU32::into_inner).collect()
}

/// The merges of a node whose merges can pass one polling interval: each merge is
/// counted before it starts. Out of line, so the loop over small nodes stays compact.
#[inline(never)]
fn large<'a>(
    work: &mut Work,
    r: u32,
    a: &[u32],
    up: &impl Fn(u32) -> &'a [u32],
    merge: &impl Fn(u32, u32, &[u32], &[u32]),
) -> Result<()> {
    work.tick_n(a.len() + 1, "triangles-node")?;
    for &q in a {
        let b = up(q);
        work.tick_n(a.len() + b.len(), "triangles-intersect")?;
        merge(r, q, a, b);
    }
    Ok(())
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
    let mut coefficient = Buffer::new("clustering-output", s.n(), ctl)?;
    let mut work = Work::new(ctl)?;
    for u in 0..s.n() as u32 {
        work.tick("clustering-output")?;
        let d = s.deg(u) as f64;
        coefficient.push(if d < 2.0 {
            0.0
        } else {
            2.0 * triangles[u as usize] as f64 / (d * (d - 1.0))
        })?;
    }
    Ok(Clustering {
        coefficient: coefficient.into_output(),
        triangles,
    })
}

/// The local clustering coefficient as LDBC Graphalytics defines it: the neighbourhood of
/// a node is its in- and out-neighbours, and the coefficient is the number of directed
/// edges among them over the number of ordered pairs of them.
pub fn clustering_graphalytics(g: &Graph, par: &Par, ctl: &Control) -> Result<Vec<f64>> {
    let n = g.n();
    let mut out = Buffer::<f64>::zeroed("clustering-output", n, ctl)?;
    let incoming = g.inn(ctl)?;
    let inn = &*incoming;
    par.chunks_mut(&mut out, BLOCK / 4, |lo, chunk| {
        let mut work = Work::new(ctl)?;
        let mut nb: Growing<u32> = Growing::new("clustering-neighbours", ctl)?;
        for (i, slot) in chunk.iter_mut().enumerate() {
            let v = (lo + i) as u32;
            nb.clear();
            let (o, r) = (g.out.row(v), inn.row(v));
            work.tick_n(o.len() + r.len() + 1, "clustering-neighbours")?;
            nb.reserve(o.len() + r.len())?;
            for &u in o.iter().chain(r) {
                if u != v {
                    nb.push(u)?;
                }
            }
            nb.sort_unstable();
            let mut kept = 0;
            for i in 0..nb.len() {
                if i == 0 || nb[i] != nb[kept - 1] {
                    nb[kept] = nb[i];
                    kept += 1;
                }
            }
            nb.truncate(kept);
            let d = nb.len();
            if d < 2 {
                continue;
            }
            let mut edges = 0u64;
            for &u in nb.iter() {
                let mut last = u32::MAX;
                let row = g.out.row(u);
                work.tick_n(row.len() + 1, "clustering-edges")?;
                for &w in row {
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
    Ok(out.into_output())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn counts_become_the_output_without_a_copy() {
        let counts: Vec<AtomicU32> = (0..1000).map(AtomicU32::new).collect();
        let (ptr, capacity) = (counts.as_ptr() as usize, counts.capacity());
        let values = into_values(counts);
        assert_eq!(values.as_ptr() as usize, ptr);
        assert_eq!(values.capacity(), capacity);
        assert_eq!(values, (0..1000).collect::<Vec<u32>>());
    }

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
