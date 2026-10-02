//! Hierarchical navigable small world graphs (Malkov & Yashunin, "Efficient and robust
//! approximate nearest neighbor search using Hierarchical Navigable Small World graphs",
//! arXiv:1603.09320, IEEE TPAMI 2020).
//!
//! The graph holds only links between node numbers. The vectors stay in the segment the
//! graph was built over, and the caller supplies distances, so the index adds about
//! `(2M + 1) · 4` bytes per node to the packed vectors instead of a second copy of them.
//!
//! * **Levels** are drawn from `⌊−ln(U) · mL⌋` with `mL = 1/ln(M)` (§4), from a hash of
//!   the node number and a seed, so a build is reproducible up to the order in which
//!   parallel insertions meet.
//! * **Insertion** follows Algorithm 1: a greedy descent (`ef = 1`) through the layers
//!   above the node's level, then a search with `efConstruction` per layer from there
//!   down, neighbours chosen by the heuristic of Algorithm 4 (a candidate is kept only if
//!   it is closer to the new node than to every neighbour kept so far, and free places go
//!   to the nearest pruned candidates), and reverse links shrunk by the same heuristic. Layer 0 keeps up to `2M` links, the others `M`.
//!   Insertions run in parallel with a lock per node, as hnswlib does.
//! * **Search** is Algorithm 5 with Algorithm 2 per layer. A filter decides which nodes
//!   may be results; rejected nodes are still traversed, so the graph stays connected.
//! * **Frozen form.** After the build the links are packed: layer 0 with a fixed stride
//!   of `2M + 1` words (a count, then the links) per node, and each upper layer as its
//!   sorted node numbers plus `M + 1` words per node. Both can be mapped from a file.

use super::persist::Slice;
use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicUsize};

/// Build parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// links per node on the upper layers (`2M` on layer 0)
    pub m: usize,
    /// candidates kept while inserting
    pub ef_construction: usize,
    pub seed: u64,
}

/// What a graph is built over: `len` nodes and the distance between two of them (lower is
/// nearer; it need not be a metric).
#[allow(clippy::len_without_is_empty)]
pub trait Space: Sync {
    fn len(&self) -> usize;
    fn dist(&self, a: u32, b: u32) -> f32;
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Near(f32, u32);

impl Eq for Near {}

impl PartialOrd for Near {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Near {
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0).then(self.1.cmp(&o.1))
    }
}

/// A frozen graph.
#[derive(Default)]
pub struct Graph {
    pub m: usize,
    pub entry: u32,
    /// the highest layer
    pub top: usize,
    pub nodes: usize,
    /// per node: the link count, then up to `2M` links
    pub level0: Slice<u32>,
    /// per upper layer (1..=top): its sorted nodes, and `M + 1` words per node
    pub upper: Vec<(Slice<u32>, Slice<u32>)>,
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn level_of(i: u32, p: &Params) -> usize {
    let u = ((splitmix(p.seed ^ splitmix(i as u64)) >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    let ml = 1.0 / (p.m.max(2) as f64).ln();
    ((-u.ln() * ml) as usize).min(24)
}

thread_local! {
    /// visit marks: a stamp per node, the current stamp
    static VISITED: RefCell<(Vec<u32>, u32)> = const { RefCell::new((Vec::new(), 0)) };
}

/// Run `f` with an empty visited set over `n` nodes.
fn with_visited<R>(n: usize, f: impl FnOnce(&mut dyn FnMut(u32) -> bool) -> R) -> R {
    VISITED.with(|v| {
        let mut v = v.borrow_mut();
        let (marks, stamp) = &mut *v;
        if marks.len() < n {
            marks.clear();
            marks.resize(n, 0);
            *stamp = 0;
        }
        *stamp = stamp.wrapping_add(1);
        if *stamp == 0 {
            marks.fill(0);
            *stamp = 1;
        }
        let s = *stamp;
        let mut visit = |i: u32| match marks.get_mut(i as usize) {
            Some(m) if *m != s => {
                *m = s;
                true
            }
            _ => false,
        };
        f(&mut visit)
    })
}

/// Algorithm 2: the `ef` nearest accepted nodes reachable from `eps` on one layer,
/// nearest first. `links(node, out)` appends a node's neighbours on the layer.
fn search_layer(
    eps: &[Near],
    ef: usize,
    dist: &dyn Fn(u32) -> f32,
    links: &dyn Fn(u32, &mut Vec<u32>),
    accept: &dyn Fn(u32) -> bool,
    visit: &mut dyn FnMut(u32) -> bool,
) -> Vec<Near> {
    let mut cand: BinaryHeap<Reverse<Near>> = BinaryHeap::with_capacity(ef * 2);
    let mut best: BinaryHeap<Near> = BinaryHeap::with_capacity(ef + 1);
    for &e in eps {
        if !visit(e.1) {
            continue;
        }
        cand.push(Reverse(e));
        if accept(e.1) {
            best.push(e);
            if best.len() > ef {
                best.pop();
            }
        }
    }
    let mut buf = Vec::with_capacity(64);
    while let Some(Reverse(c)) = cand.pop() {
        let bound = best.peek().map_or(f32::INFINITY, |b| b.0);
        if c.0 > bound && best.len() >= ef {
            break;
        }
        buf.clear();
        links(c.1, &mut buf);
        for &n in &buf {
            if !visit(n) {
                continue;
            }
            let d = dist(n);
            let bound = best.peek().map_or(f32::INFINITY, |b| b.0);
            if best.len() < ef || d < bound {
                cand.push(Reverse(Near(d, n)));
                if accept(n) {
                    best.push(Near(d, n));
                    if best.len() > ef {
                        best.pop();
                    }
                }
            }
        }
    }
    let mut v = best.into_vec();
    v.sort_unstable();
    v
}

/// Algorithm 4 with `keepPrunedConnections` (without extending the candidates): `cands`
/// nearest first. Free places are filled with the nearest pruned candidates, which keeps
/// more nodes reachable (recall@10 at ef = 16 went from 0.984 to 0.998 on 50k clustered
/// vectors of dimension 384 with M = 16).
fn select(space: &dyn Space, cands: &[Near], m: usize) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(m);
    let mut pruned: Vec<u32> = Vec::new();
    for c in cands {
        if out.len() >= m {
            break;
        }
        if out.iter().all(|&r| space.dist(c.1, r) > c.0) {
            out.push(c.1);
        } else {
            pruned.push(c.1);
        }
    }
    let room = m - out.len();
    out.extend(pruned.into_iter().take(room));
    out
}

/// Reports progress and asks whether to stop.
pub struct Ctl<'a> {
    /// called with the nodes inserted so far, about every 4096
    pub progress: &'a (dyn Fn(usize) + Sync),
    pub cancel: &'a (dyn Fn() -> bool + Sync),
}

impl Graph {
    /// Build a graph over every node of `space` (`None`: cancelled).
    pub fn build(space: &dyn Space, p: &Params, ctl: &Ctl<'_>) -> Option<Graph> {
        let n = space.len();
        let m = p.m.max(2);
        let m0 = 2 * m;
        let efc = p.ef_construction.max(m);
        assert!(n < u32::MAX as usize, "too many nodes for a graph");
        let levels: Vec<u8> = (0..n as u32).map(|i| level_of(i, p) as u8).collect();
        let nodes: Vec<Mutex<Vec<Vec<u32>>>> = levels
            .iter()
            .map(|&l| Mutex::new(vec![Vec::new(); l as usize + 1]))
            .collect();
        let entry = RwLock::new((0u32, levels.first().copied().unwrap_or(0) as usize));
        let cap = |l: usize| if l == 0 { m0 } else { m };
        let done = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let accept = |_: u32| true;
        let insert = |i: u32| {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let li = levels[i as usize] as usize;
            let (ep, top) = *entry.read();
            let dist = |x: u32| space.dist(i, x);
            let mut eps = vec![Near(dist(ep), ep)];
            for l in (li + 1..=top).rev() {
                let links = |x: u32, out: &mut Vec<u32>| {
                    out.extend_from_slice(&nodes[x as usize].lock()[l]);
                };
                eps = with_visited(n, |v| search_layer(&eps, 1, &dist, &links, &accept, v));
            }
            for l in (0..=li.min(top)).rev() {
                let links = |x: u32, out: &mut Vec<u32>| {
                    out.extend_from_slice(&nodes[x as usize].lock()[l]);
                };
                let mut w = with_visited(n, |v| search_layer(&eps, efc, &dist, &links, &accept, v));
                w.retain(|x| x.1 != i);
                let chosen = select(space, &w, cap(l));
                nodes[i as usize].lock()[l].clone_from(&chosen);
                for &e in &chosen {
                    let mut g = nodes[e as usize].lock();
                    let list = &mut g[l];
                    if list.contains(&i) {
                        continue;
                    }
                    if list.len() < cap(l) {
                        list.push(i);
                    } else {
                        let mut c: Vec<Near> = list
                            .iter()
                            .chain(std::iter::once(&i))
                            .map(|&x| Near(space.dist(e, x), x))
                            .collect();
                        c.sort_unstable();
                        *list = select(space, &c, cap(l));
                    }
                }
                if !w.is_empty() {
                    eps = w;
                }
            }
            if li > top {
                let mut e = entry.write();
                if li > e.1 {
                    *e = (i, li);
                }
            }
            let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if d.is_multiple_of(4096) {
                (ctl.progress)(d);
                if (ctl.cancel)() {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        };
        if n > 1 {
            // the first nodes one at a time, so the parallel ones start from a graph
            let seq = n.min(512) as u32;
            (1..seq).for_each(insert);
            (seq..n as u32).into_par_iter().for_each(insert);
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) || (ctl.cancel)() {
            return None;
        }
        let (entry, top) = *entry.read();
        let mut level0 = vec![0u32; n * (m0 + 1)];
        let mut upper: Vec<(Vec<u32>, Vec<u32>)> = vec![(Vec::new(), Vec::new()); top];
        for (i, node) in nodes.into_iter().enumerate() {
            let node = node.into_inner();
            let s = &mut level0[i * (m0 + 1)..(i + 1) * (m0 + 1)];
            s[0] = node[0].len() as u32;
            s[1..1 + node[0].len()].copy_from_slice(&node[0]);
            for (l, links) in node.iter().enumerate().skip(1) {
                let (ns, ls) = &mut upper[l - 1];
                ns.push(i as u32);
                ls.push(links.len() as u32);
                ls.extend_from_slice(links);
                ls.resize(ns.len() * (m + 1), 0);
            }
        }
        Some(Graph {
            m,
            entry,
            top,
            nodes: n,
            level0: Slice::Owned(level0),
            upper: upper
                .into_iter()
                .map(|(a, b)| (Slice::Owned(a), Slice::Owned(b)))
                .collect(),
        })
    }

    /// Check that the packed links are consistent (after mapping a file): every count
    /// and link in range.
    pub fn check(&self) -> bool {
        let (m, n) = (self.m, self.nodes);
        if m < 2 || self.level0.len() != n * (2 * m + 1) || self.upper.len() != self.top {
            return false;
        }
        if n > 0 && self.entry as usize >= n {
            return false;
        }
        let ok = |s: &[u32], stride: usize| {
            s.chunks_exact(stride).all(|c| {
                (c[0] as usize) < stride
                    && c[1..1 + c[0] as usize].iter().all(|&x| (x as usize) < n)
            })
        };
        ok(&self.level0, 2 * m + 1)
            && self.upper.iter().all(|(ns, ls)| {
                ls.len() == ns.len() * (m + 1)
                    && ns.windows(2).all(|w| w[0] < w[1])
                    && ns.iter().all(|&x| (x as usize) < n)
                    && ok(ls, m + 1)
            })
    }

    fn links(&self, l: usize, x: u32, out: &mut Vec<u32>) {
        if l == 0 {
            let s = (x as usize) * (2 * self.m + 1);
            let c = self.level0[s] as usize;
            out.extend_from_slice(&self.level0[s + 1..s + 1 + c]);
        } else {
            let (ns, ls) = &self.upper[l - 1];
            if let Ok(j) = ns.binary_search(&x) {
                let s = j * (self.m + 1);
                let c = ls[s] as usize;
                out.extend_from_slice(&ls[s + 1..s + 1 + c]);
            }
        }
    }

    /// Bytes of the packed links.
    pub fn bytes(&self) -> u64 {
        4 * (self.level0.len()
            + self
                .upper
                .iter()
                .map(|(a, b)| a.len() + b.len())
                .sum::<usize>()) as u64
    }

    /// The `ef` nearest accepted nodes to a query whose distance to a node is `dist`,
    /// nearest first.
    pub fn search(
        &self,
        dist: &dyn Fn(u32) -> f32,
        ef: usize,
        accept: &dyn Fn(u32) -> bool,
    ) -> Vec<(f32, u32)> {
        let n = self.nodes;
        if n == 0 {
            return Vec::new();
        }
        let mut eps = vec![Near(dist(self.entry), self.entry)];
        let all = |_: u32| true;
        for l in (1..=self.top).rev() {
            let links = |x: u32, out: &mut Vec<u32>| self.links(l, x, out);
            eps = with_visited(n, |v| search_layer(&eps, 1, dist, &links, &all, v));
        }
        let links = |x: u32, out: &mut Vec<u32>| self.links(0, x, out);
        with_visited(n, |v| {
            search_layer(&eps, ef.max(1), dist, &links, accept, v)
        })
        .into_iter()
        .map(|x| (x.0, x.1))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Points(Vec<[f32; 4]>);

    impl Space for Points {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn dist(&self, a: u32, b: u32) -> f32 {
            l2(&self.0[a as usize], &self.0[b as usize])
        }
    }

    fn l2(a: &[f32; 4], b: &[f32; 4]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
    }

    fn points(n: usize, seed: u64) -> Vec<[f32; 4]> {
        (0..n as u64)
            .map(|i| {
                let r = |k: u64| (splitmix(seed ^ (i * 4 + k)) >> 40) as f32 / (1 << 24) as f32;
                [r(0), r(1), r(2), r(3)]
            })
            .collect()
    }

    fn ctl() -> Ctl<'static> {
        Ctl {
            progress: &|_| {},
            cancel: &|| false,
        }
    }

    #[test]
    fn finds_nearest_neighbours() {
        let pts = Points(points(5000, 1));
        let p = Params {
            m: 8,
            ef_construction: 64,
            seed: 3,
        };
        let g = Graph::build(&pts, &p, &ctl()).unwrap();
        assert!(g.check());
        assert_eq!(g.nodes, 5000);
        let queries = points(200, 99);
        let mut hits = 0;
        for q in &queries {
            let mut exact: Vec<(f32, u32)> = (0..5000u32)
                .map(|i| (l2(q, &pts.0[i as usize]), i))
                .collect();
            exact.sort_by(|a, b| a.0.total_cmp(&b.0));
            let got = g.search(&|x| l2(q, &pts.0[x as usize]), 32, &|_| true);
            assert!(got.windows(2).all(|w| w[0].0 <= w[1].0));
            hits += exact[..10]
                .iter()
                .filter(|e| got[..10].iter().any(|g| g.1 == e.1))
                .count();
        }
        let recall = hits as f64 / 2000.0;
        assert!(recall > 0.95, "{recall}");
        // a filter: only even nodes
        let q = &queries[0];
        let got = g.search(&|x| l2(q, &pts.0[x as usize]), 20, &|x| x % 2 == 0);
        assert_eq!(got.len(), 20);
        assert!(got.iter().all(|x| x.1 % 2 == 0));
    }

    #[test]
    fn small_and_cancelled() {
        let pts = Points(points(3, 1));
        let p = Params {
            m: 4,
            ef_construction: 8,
            seed: 0,
        };
        let g = Graph::build(&pts, &p, &ctl()).unwrap();
        let mut got: Vec<u32> = g
            .search(&|_| 0.0, 10, &|_| true)
            .iter()
            .map(|x| x.1)
            .collect();
        got.sort_unstable();
        assert_eq!(got, [0, 1, 2]);
        let empty = Graph::build(&Points(vec![]), &p, &ctl()).unwrap();
        assert!(empty.search(&|_| 0.0, 10, &|_| true).is_empty());
        let big = Points(points(20_000, 2));
        let cancelled = Graph::build(
            &big,
            &p,
            &Ctl {
                progress: &|_| {},
                cancel: &|| true,
            },
        );
        assert!(cancelled.is_none());
    }
}
