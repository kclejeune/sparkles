//! Betweenness centrality by Brandes' algorithm, exact or sampled (spec F14 §4).
//!
//! Sources run in parallel, each thread adding its dependencies to an array of its own,
//! and the arrays are added in thread order at the end. With one thread the result is
//! exactly reproducible. With several threads, floating-point sums may differ in the last
//! digits between thread counts. A sampled run picks `samples` sources with a seeded
//! shuffle and scales the scores by `n / samples` (Brandes and Pich, 2007). A graph that
//! is read both ways counts each pair twice, so its scores are halved.

use super::Rng;
use super::traverse::OrdF64;
use super::{Control, Graph, Par, bad};
use crate::error::Result;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Clone, Debug, Default)]
pub struct Params {
    /// sampled sources; 0: every node
    pub samples: usize,
    pub seed: u64,
}

pub fn run(g: &Graph, p: &Params, par: &Par, ctl: &Control) -> Result<Vec<f64>> {
    let n = g.n();
    let weighted = g.out.w.is_some();
    if let Some(w) = &g.out.w
        && w.iter().any(|&x| x < 0.0 || x.is_nan())
    {
        return Err(bad("betweenness", "a negative edge weight"));
    }
    let mut sources: Vec<u32> = (0..n as u32).collect();
    if p.samples > 0 && p.samples < n {
        let mut rng = Rng::new(p.seed);
        // a partial Fisher-Yates shuffle
        for i in 0..p.samples {
            let j = i + rng.below(n - i);
            sources.swap(i, j);
        }
        sources.truncate(p.samples);
        sources.sort_unstable();
    }
    let incoming = g.inn(ctl)?;
    let inn = &*incoming;
    let threads = par.threads();
    // per thread: the score array and the working arrays of one source
    ctl.charge(threads as u64 * n as u64 * (8 + 8 + 8 + 4 + 8))?;
    let per = sources.len().div_ceil(threads.max(1)).max(1);
    let parts = par.blocks(sources.len(), per, |lo, hi| {
        let mut acc = vec![0f64; n];
        let mut w = Work::new(n);
        for (k, &s) in sources[lo..hi].iter().enumerate() {
            if k % 16 == 0 {
                ctl.check()?;
            }
            if weighted {
                w.dijkstra(g, s);
            } else {
                w.bfs(g, s);
            }
            w.accumulate(inn, s, &mut acc);
        }
        Ok(acc)
    })?;
    let mut score = vec![0f64; n];
    for part in parts {
        for (s, x) in score.iter_mut().zip(part) {
            *s += x;
        }
    }
    let mut scale = 1.0;
    if p.samples > 0 && p.samples < n {
        scale *= n as f64 / p.samples as f64;
    }
    if g.symmetric {
        scale *= 0.5;
    }
    if scale != 1.0 {
        for s in &mut score {
            *s *= scale;
        }
    }
    Ok(score)
}

/// The arrays of one single-source search.
struct Work {
    dist: Vec<f64>,
    sigma: Vec<f64>,
    delta: Vec<f64>,
    /// nodes in the order they were settled
    order: Vec<u32>,
    done: Vec<bool>,
}

impl Work {
    fn new(n: usize) -> Work {
        Work {
            dist: vec![f64::INFINITY; n],
            sigma: vec![0.0; n],
            delta: vec![0.0; n],
            order: Vec::new(),
            done: vec![false; n],
        }
    }

    fn reset(&mut self) {
        for &v in &self.order {
            self.dist[v as usize] = f64::INFINITY;
            self.sigma[v as usize] = 0.0;
            self.delta[v as usize] = 0.0;
            self.done[v as usize] = false;
        }
        self.order.clear();
    }

    fn bfs(&mut self, g: &Graph, s: u32) {
        self.reset();
        self.dist[s as usize] = 0.0;
        self.sigma[s as usize] = 1.0;
        self.order.push(s);
        let mut head = 0;
        while head < self.order.len() {
            let u = self.order[head];
            head += 1;
            let du = self.dist[u as usize];
            for &v in g.out.row(u) {
                if self.dist[v as usize].is_infinite() {
                    self.dist[v as usize] = du + 1.0;
                    self.order.push(v);
                }
                if self.dist[v as usize] == du + 1.0 {
                    self.sigma[v as usize] += self.sigma[u as usize];
                }
            }
        }
    }

    fn dijkstra(&mut self, g: &Graph, s: u32) {
        self.reset();
        self.dist[s as usize] = 0.0;
        self.sigma[s as usize] = 1.0;
        let mut heap = BinaryHeap::new();
        heap.push(Reverse((OrdF64(0.0), s)));
        // nodes settle in order of distance and are collected in `order` as they do
        while let Some(Reverse((OrdF64(d), u))) = heap.pop() {
            // a stale entry, or a node reached by two paths of equal length that already
            // settled
            if d > self.dist[u as usize] || self.done[u as usize] {
                continue;
            }
            self.done[u as usize] = true;
            self.order.push(u);
            let (row, ws) = (g.out.row(u), g.out.wrow(u).unwrap());
            for (i, &v) in row.iter().enumerate() {
                let nd = d + ws[i] as f64;
                let dv = self.dist[v as usize];
                if nd < dv {
                    self.dist[v as usize] = nd;
                    self.sigma[v as usize] = self.sigma[u as usize];
                    heap.push(Reverse((OrdF64(nd), v)));
                } else if nd == dv {
                    self.sigma[v as usize] += self.sigma[u as usize];
                }
            }
        }
    }

    /// Add the dependencies of source `s` to `acc`, walking the settled nodes backwards.
    fn accumulate(&mut self, inn: &super::Csr, s: u32, acc: &mut [f64]) {
        for i in (0..self.order.len()).rev() {
            let w = self.order[i];
            // the predecessors of w: nodes u with an edge u -> w on a shortest path. Walk
            // the in-edges.
            let (row, ws) = (inn.row(w), inn.wrow(w));
            let dw = self.dist[w as usize];
            let coeff = (1.0 + self.delta[w as usize]) / self.sigma[w as usize];
            for (k, &u) in row.iter().enumerate() {
                let du = self.dist[u as usize];
                if du.is_infinite() {
                    continue;
                }
                let len = ws.map_or(1.0, |ws| ws[k] as f64);
                if du + len == dw {
                    self.delta[u as usize] += self.sigma[u as usize] * coeff;
                }
            }
            if w != s {
                acc[w as usize] += self.delta[w as usize];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn path_graph() {
        // 0 - 1 - 2 - 3 - 4: the middle node is on 4 pairs' shortest paths
        let g = ugraph(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let s = run(
            &g,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert_eq!(s, vec![0.0, 3.0, 4.0, 3.0, 0.0]);
    }

    #[test]
    fn directed_star_counts_pairs_once() {
        let g = digraph(4, &[(0, 1), (1, 2), (1, 3)]);
        let s = run(
            &g,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        // node 1 is between 0 and each of 2 and 3
        assert_eq!(s, vec![0.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn matches_counting_shortest_paths() {
        let mut x = 11u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        for _ in 0..15 {
            let n = 12u32;
            let e: Vec<(u32, u32)> = (0..24)
                .map(|_| (rnd(n), rnd(n)))
                .filter(|e| e.0 != e.1)
                .collect();
            let mut e = e;
            e.sort_unstable();
            e.dedup();
            let g = digraph(n, &e);
            let got = run(
                &g,
                &Params::default(),
                &Par::single(),
                &Control::unlimited(),
            )
            .unwrap();
            let got4 = run(
                &g,
                &Params::default(),
                &Par::with_threads(3),
                &Control::unlimited(),
            )
            .unwrap();
            // reference: count shortest paths between every pair by BFS layers
            let nn = n as usize;
            let inf = usize::MAX;
            let mut d = vec![vec![inf; nn]; nn];
            let mut c = vec![vec![0f64; nn]; nn];
            for s in 0..nn {
                d[s][s] = 0;
                c[s][s] = 1.0;
                let mut q = std::collections::VecDeque::from([s]);
                while let Some(u) = q.pop_front() {
                    for &(a, b) in &e {
                        if a as usize == u {
                            let v = b as usize;
                            if d[s][v] == inf {
                                d[s][v] = d[s][u] + 1;
                                q.push_back(v);
                            }
                            if d[s][v] == d[s][u] + 1 {
                                c[s][v] += c[s][u];
                            }
                        }
                    }
                }
            }
            for v in 0..nn {
                let mut b = 0.0;
                for s in 0..nn {
                    for t in 0..nn {
                        if s != v
                            && t != v
                            && s != t
                            && d[s][t] != inf
                            && d[s][v] != inf
                            && d[v][t] != inf
                            && d[s][v] + d[v][t] == d[s][t]
                        {
                            b += c[s][v] * c[v][t] / c[s][t];
                        }
                    }
                }
                assert!((got[v] - b).abs() < 1e-9, "node {v}: {} vs {b}", got[v]);
                assert!((got4[v] - b).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn sampling_with_all_sources_is_exact() {
        let g = ugraph(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let p = Params {
            samples: 5,
            seed: 1,
        };
        let s = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(s, vec![0.0, 3.0, 4.0, 3.0, 0.0]);
    }
}
