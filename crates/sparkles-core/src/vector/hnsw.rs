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
//!   parallel insertions meet. A sequential build (`Params::sequential`) inserts the
//!   nodes in order and gives the same graph every time.
//! * **Insertion** follows Algorithm 1: a greedy descent (`ef = 1`) through the layers
//!   above the node's level, then a search with `efConstruction` per layer from there
//!   down, neighbours chosen by the heuristic of Algorithm 4 (a candidate is kept only if
//!   it is closer to the new node than to every neighbour kept so far, and free places go
//!   to the nearest pruned candidates), and reverse links shrunk by the same heuristic.
//!   Layer 0 keeps up to `2M` links, the others `M`. Insertions run in parallel
//!   ([`Builder`]). A writer locks the node whose links it changes, and readers load the
//!   links without a lock, so the many reads of a build do not contend for the nodes
//!   near the entry point.
//! * **Search** is Algorithm 5 with Algorithm 2 per layer. A filter decides which nodes
//!   may be results; rejected nodes are still traversed, so the graph stays connected.
//!   [`search`] runs over any [`Layers`], so a frozen graph and the nodes inserted into it
//!   since (`vector::live`) are searched as one graph.
//! * **Frozen form.** After the build the links are packed: layer 0 with a fixed stride
//!   of `2M + 1` words (a count, then the links) per node, and each upper layer as its
//!   sorted node numbers plus `M + 1` words per node. Both can be mapped from a file.

use super::persist::Slice;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering as Mem};

/// Build parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// links per node on the upper layers (`2M` on layer 0)
    pub m: usize,
    /// candidates kept while inserting
    pub ef_construction: usize,
    pub seed: u64,
    /// insert one node at a time, in order: the graph then does not depend on how
    /// threads are scheduled (tests)
    pub sequential: bool,
}

impl Params {
    /// The most links a node keeps on layer `l`.
    pub fn cap(&self, l: usize) -> usize {
        if l == 0 {
            2 * self.m.max(2)
        } else {
            self.m.max(2)
        }
    }
}

/// What a graph is built over: `len` nodes and the distance between two of them (lower is
/// nearer; it need not be a metric).
#[allow(clippy::len_without_is_empty)]
pub trait Space: Sync {
    fn len(&self) -> usize;
    fn dist(&self, a: u32, b: u32) -> f32;
    /// Hint that node `a`'s vector is read soon (a build calls it for the neighbours a
    /// search is about to score, so their rows load while it scores others).
    fn prefetch(&self, _a: u32) {}
}

/// A graph to search: its nodes, entry point and links.
#[allow(clippy::len_without_is_empty)]
pub trait Layers {
    /// Node numbers are below this.
    fn len(&self) -> usize;
    /// The entry point and the highest layer (`None`: no nodes).
    fn entry(&self) -> Option<(u32, usize)>;
    /// Append node `x`'s neighbours on layer `l` to `out`.
    fn links(&self, l: usize, x: u32, out: &mut Vec<u32>);
}

/// A distance and a node, ordered by distance, then node.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Near(pub f32, pub u32);

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

/// The level of node `i`: a hash of its number and the seed, so it does not depend on
/// the order of insertion.
pub(crate) fn level_of(i: u32, p: &Params) -> usize {
    let u = ((splitmix(p.seed ^ splitmix(i as u64)) >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    let ml = 1.0 / (p.m.max(2) as f64).ln();
    ((-u.ln() * ml) as usize).min(24)
}

thread_local! {
    /// visit marks: a stamp per node, the current stamp
    static VISITED: RefCell<(Vec<u32>, u32)> = const { RefCell::new((Vec::new(), 0)) };
}

/// Run `f` with an empty visited set over `n` nodes.
pub(crate) fn with_visited<R>(n: usize, f: impl FnOnce(&mut dyn FnMut(u32) -> bool) -> R) -> R {
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
pub(crate) fn search_layer(
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
pub(crate) fn select(space: &dyn Space, cands: &[Near], m: usize) -> Vec<u32> {
    select_near(space, cands, m)
        .into_iter()
        .map(|c| c.1)
        .collect()
}

/// [`select`], keeping each chosen node's distance.
pub(crate) fn select_near(space: &dyn Space, cands: &[Near], m: usize) -> Vec<Near> {
    let mut out: Vec<Near> = Vec::with_capacity(m);
    let mut pruned: Vec<Near> = Vec::new();
    for c in cands {
        if out.len() >= m {
            break;
        }
        if out.iter().all(|r| space.dist(c.1, r.1) > c.0) {
            out.push(*c);
        } else {
            pruned.push(*c);
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

/// The ef nearest accepted nodes of `g` to a query whose distance to a node is `dist`,
/// nearest first.
pub fn search(
    g: &dyn Layers,
    dist: &dyn Fn(u32) -> f32,
    ef: usize,
    accept: &dyn Fn(u32) -> bool,
) -> Vec<(f32, u32)> {
    let Some((ep, top)) = g.entry() else {
        return Vec::new();
    };
    let n = g.len();
    let mut eps = vec![Near(dist(ep), ep)];
    let all = |_: u32| true;
    for l in (1..=top).rev() {
        let links = |x: u32, out: &mut Vec<u32>| g.links(l, x, out);
        eps = with_visited(n, |v| search_layer(&eps, 1, dist, &links, &all, v));
    }
    let links = |x: u32, out: &mut Vec<u32>| g.links(0, x, out);
    with_visited(n, |v| {
        search_layer(&eps, ef.max(1), dist, &links, accept, v)
    })
    .into_iter()
    .map(|x| (x.0, x.1))
    .collect()
}

/// A graph being built: nodes with fixed levels whose links insertions add, in
/// parallel. Each node's links per layer are a count and up to `cap` slots of atomics;
/// a writer holds the node's lock, and a reader loads the count and then the slots. A
/// reader that meets a list being rewritten can see a mix of the old and new links, all
/// of them nodes of the graph, which only changes the path a search takes.
pub struct Builder<'a> {
    space: &'a dyn Space,
    p: Params,
    levels: Vec<u8>,
    /// per node: the count, then `2M` slots
    level0: Vec<AtomicU32>,
    /// per node: the start of its layer 1 list in `upper` (unused at level 0)
    upper_at: Vec<usize>,
    /// per node above level 0: `M + 1` words per layer 1..=level
    upper: Vec<AtomicU32>,
    /// the distance (`f32` bits) from the node to each link, in the same places as the
    /// links in `level0` and `upper`, so a full list is shrunk without computing them
    level0_d: Vec<AtomicU32>,
    upper_d: Vec<AtomicU32>,
    locks: Vec<Mutex<()>>,
    /// `entry << 8 | top`, `u64::MAX` before the first node
    entry: AtomicU64,
}

const NO_ENTRY: u64 = u64::MAX;

impl<'a> Builder<'a> {
    /// A builder over `space` whose node `i` has level `levels[i]`, without links or an
    /// entry point.
    pub fn new(space: &'a dyn Space, p: &Params, levels: Vec<u8>) -> Builder<'a> {
        let n = levels.len();
        assert!(n < u32::MAX as usize, "too many nodes for a graph");
        let p = Params {
            m: p.m.max(2),
            ef_construction: p.ef_construction.max(p.m.max(2)),
            ..*p
        };
        let (m0, m) = (p.cap(0), p.cap(1));
        let mut upper_at = vec![0usize; n];
        let mut words = 0usize;
        for (i, &l) in levels.iter().enumerate() {
            upper_at[i] = words;
            words += l as usize * (m + 1);
        }
        Builder {
            space,
            p,
            level0: (0..n * (m0 + 1)).map(|_| AtomicU32::new(0)).collect(),
            upper_at,
            upper: (0..words).map(|_| AtomicU32::new(0)).collect(),
            level0_d: (0..n * (m0 + 1)).map(|_| AtomicU32::new(0)).collect(),
            upper_d: (0..words).map(|_| AtomicU32::new(0)).collect(),
            locks: (0..n).map(|_| Mutex::new(())).collect(),
            levels,
            entry: AtomicU64::new(NO_ENTRY),
        }
    }

    /// The level of every node, drawn from its number ([`level_of`]).
    pub fn levels_of(n: usize, p: &Params) -> Vec<u8> {
        (0..n as u32).map(|i| level_of(i, p) as u8).collect()
    }

    fn slots(&self, l: usize, x: u32) -> &[AtomicU32] {
        self.slots_of(l, x).0
    }

    /// Node `x`'s link slots on layer `l` and the slots of their distances.
    fn slots_of(&self, l: usize, x: u32) -> (&[AtomicU32], &[AtomicU32]) {
        if l == 0 {
            let w = self.p.cap(0) + 1;
            let r = x as usize * w..(x as usize + 1) * w;
            (&self.level0[r.clone()], &self.level0_d[r])
        } else {
            let w = self.p.cap(1) + 1;
            let at = self.upper_at[x as usize] + (l - 1) * w;
            (&self.upper[at..at + w], &self.upper_d[at..at + w])
        }
    }

    /// Node `x`'s links on layer `l` with their distances (the caller holds `x`'s lock).
    fn read_near(&self, l: usize, x: u32, out: &mut Vec<Near>) {
        let (s, d) = self.slots_of(l, x);
        let c = (s[0].load(Mem::Acquire) as usize).min(s.len() - 1);
        out.extend((1..=c).map(|j| {
            Near(
                f32::from_bits(d[j].load(Mem::Relaxed)),
                s[j].load(Mem::Relaxed),
            )
        }));
    }

    /// Append node `x`'s links on layer `l` to `out`.
    pub fn read(&self, l: usize, x: u32, out: &mut Vec<u32>) {
        if l > self.levels[x as usize] as usize {
            return;
        }
        let s = self.slots(l, x);
        let c = (s[0].load(Mem::Acquire) as usize).min(s.len() - 1);
        out.extend(s[1..=c].iter().map(|v| v.load(Mem::Relaxed)));
    }

    /// [`read`](Self::read), and ask the space to load the neighbours' vectors.
    fn read_ahead(&self, l: usize, x: u32, out: &mut Vec<u32>) {
        let start = out.len();
        self.read(l, x, out);
        for &y in &out[start..] {
            self.space.prefetch(y);
        }
    }

    /// Set node `x`'s links on layer `l` (at most the layer's cap; the caller holds
    /// `x`'s lock, or no other thread runs).
    fn write(&self, l: usize, x: u32, links: &[Near]) {
        let (s, d) = self.slots_of(l, x);
        let c = links.len().min(s.len() - 1);
        for (j, v) in links[..c].iter().enumerate() {
            s[j + 1].store(v.1, Mem::Relaxed);
            d[j + 1].store(v.0.to_bits(), Mem::Relaxed);
        }
        s[0].store(c as u32, Mem::Release);
    }

    /// Give node `x` the links `links` on layer `l` before insertions start.
    pub fn seed(&self, l: usize, x: u32, links: &[u32]) {
        if l <= self.levels[x as usize] as usize {
            let near: Vec<Near> = links
                .iter()
                .map(|&y| Near(self.space.dist(x, y), y))
                .collect();
            self.write(l, x, &near);
        }
    }

    /// Make `x` the entry point (its level is the top layer).
    pub fn set_entry(&self, x: u32) {
        let top = self.levels[x as usize] as u64;
        self.entry.store((x as u64) << 8 | top, Mem::Release);
    }

    fn entry(&self) -> Option<(u32, usize)> {
        let e = self.entry.load(Mem::Acquire);
        (e != NO_ENTRY).then_some(((e >> 8) as u32, (e & 0xff) as usize))
    }

    /// Make `i`, of level `li`, the entry point if it is above the top layer.
    fn raise_entry(&self, i: u32, li: usize) {
        let mut cur = self.entry.load(Mem::Acquire);
        loop {
            if cur != NO_ENTRY && li <= (cur & 0xff) as usize {
                return;
            }
            let new = (i as u64) << 8 | li as u64;
            match self
                .entry
                .compare_exchange(cur, new, Mem::AcqRel, Mem::Acquire)
            {
                Ok(_) => return,
                Err(c) => cur = c,
            }
        }
    }

    /// Insert node `i` (Algorithm 1). The first node inserted becomes the entry point.
    pub fn insert(&self, i: u32) {
        let li = self.levels[i as usize] as usize;
        let Some((ep, top)) = self.entry() else {
            let first = (i as u64) << 8 | li as u64;
            if self
                .entry
                .compare_exchange(NO_ENTRY, first, Mem::AcqRel, Mem::Acquire)
                .is_ok()
            {
                return;
            }
            // another node came first
            return self.insert(i);
        };
        if ep == i {
            return;
        }
        let n = self.levels.len();
        let space = self.space;
        let dist = |x: u32| space.dist(i, x);
        let accept = |_: u32| true;
        let mut eps = vec![Near(dist(ep), ep)];
        for l in (li + 1..=top).rev() {
            let links = |x: u32, out: &mut Vec<u32>| self.read_ahead(l, x, out);
            eps = with_visited(n, |v| search_layer(&eps, 1, &dist, &links, &accept, v));
        }
        for l in (0..=li.min(top)).rev() {
            let links = |x: u32, out: &mut Vec<u32>| self.read_ahead(l, x, out);
            let mut w = with_visited(n, |v| {
                search_layer(&eps, self.p.ef_construction, &dist, &links, &accept, v)
            });
            w.retain(|x| x.1 != i);
            let chosen = select_near(space, &w, self.p.cap(l));
            {
                let _g = self.locks[i as usize].lock();
                self.write(l, i, &chosen);
            }
            for e in &chosen {
                self.link(l, e.1, Near(e.0, i));
            }
            if !w.is_empty() {
                eps = w;
            }
        }
        if li > top {
            self.raise_entry(i, li);
        }
    }

    /// Add `i` (with its distance to `e`) to `e`'s links on layer `l`, shrinking them by
    /// the heuristic when full.
    fn link(&self, l: usize, e: u32, i: Near) {
        let cap = self.p.cap(l);
        let _g = self.locks[e as usize].lock();
        let mut list = Vec::with_capacity(cap + 1);
        self.read_near(l, e, &mut list);
        if list.iter().any(|x| x.1 == i.1) {
            return;
        }
        list.push(i);
        if list.len() > cap {
            list.sort_unstable();
            list = select_near(self.space, &list, cap);
        }
        self.write(l, e, &list);
    }

    /// Insert `order`'s nodes: the first ones one at a time, so the parallel ones start
    /// from a graph, and all of them in order with `Params::sequential` (`false`:
    /// cancelled).
    pub fn insert_all(&self, order: &[u32], ctl: &Ctl<'_>) -> bool {
        let done = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let one = |&i: &u32| {
            if stop.load(Mem::Relaxed) {
                return;
            }
            self.insert(i);
            let d = done.fetch_add(1, Mem::Relaxed) + 1;
            if d.is_multiple_of(4096) {
                (ctl.progress)(d);
                if (ctl.cancel)() {
                    stop.store(true, Mem::Relaxed);
                }
            }
        };
        let seq = if self.p.sequential {
            order.len()
        } else {
            order.len().min(512)
        };
        order[..seq].iter().for_each(one);
        order[seq..].par_iter().for_each(one);
        !(stop.load(Mem::Relaxed) || (ctl.cancel)())
    }

    /// The packed graph.
    pub fn freeze(self) -> Graph {
        let n = self.levels.len();
        let (m0, m) = (self.p.cap(0), self.p.cap(1));
        let (entry, top) = self.entry().unwrap_or((0, 0));
        let level0: Vec<u32> = self.level0.into_iter().map(AtomicU32::into_inner).collect();
        let upper_words: Vec<u32> = self.upper.into_iter().map(AtomicU32::into_inner).collect();
        let mut upper: Vec<(Vec<u32>, Vec<u32>)> = vec![(Vec::new(), Vec::new()); top];
        for i in 0..n {
            let li = (self.levels[i] as usize).min(top);
            for l in 1..=li {
                let at = self.upper_at[i] + (l - 1) * (m + 1);
                let (ns, ls) = &mut upper[l - 1];
                ns.push(i as u32);
                ls.extend_from_slice(&upper_words[at..at + m + 1]);
            }
        }
        debug_assert_eq!(level0.len(), n * (m0 + 1));
        Graph {
            m,
            entry,
            top,
            nodes: n,
            level0: Slice::Owned(level0),
            upper: upper
                .into_iter()
                .map(|(a, b)| (Slice::Owned(a), Slice::Owned(b)))
                .collect(),
        }
    }
}

impl Graph {
    /// Build a graph over every node of `space` (`None`: cancelled).
    pub fn build(space: &dyn Space, p: &Params, ctl: &Ctl<'_>) -> Option<Graph> {
        let n = space.len();
        let b = Builder::new(space, p, Builder::levels_of(n, p));
        let order: Vec<u32> = (0..n as u32).collect();
        b.insert_all(&order, ctl).then(|| b.freeze())
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

    /// Append node `x`'s links on layer `l` to `out`.
    pub fn links(&self, l: usize, x: u32, out: &mut Vec<u32>) {
        if l == 0 {
            let s = (x as usize) * (2 * self.m + 1);
            let c = self.level0[s] as usize;
            out.extend_from_slice(&self.level0[s + 1..s + 1 + c]);
        } else if let Some((ns, ls)) = self.upper.get(l - 1)
            && let Ok(j) = ns.binary_search(&x)
        {
            let s = j * (self.m + 1);
            let c = ls[s] as usize;
            out.extend_from_slice(&ls[s + 1..s + 1 + c]);
        }
    }

    /// The level of every node: the highest upper layer that lists it.
    pub fn levels(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.nodes];
        for (l, (ns, _)) in self.upper.iter().enumerate() {
            for &x in ns.iter() {
                out[x as usize] = (l + 1) as u8;
            }
        }
        out
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
        search(self, dist, ef, accept)
    }
}

impl Layers for Graph {
    fn len(&self) -> usize {
        self.nodes
    }

    fn entry(&self) -> Option<(u32, usize)> {
        (self.nodes > 0).then_some((self.entry, self.top))
    }

    fn links(&self, l: usize, x: u32, out: &mut Vec<u32>) {
        Graph::links(self, l, x, out)
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

    /// recall@10 of `g` over `pts` for 200 random queries
    fn recall(g: &dyn Layers, pts: &[[f32; 4]], ef: usize) -> f64 {
        let queries = points(200, 99);
        let mut hits = 0;
        for q in &queries {
            let mut exact: Vec<(f32, u32)> = (0..pts.len() as u32)
                .map(|i| (l2(q, &pts[i as usize]), i))
                .collect();
            exact.sort_by(|a, b| a.0.total_cmp(&b.0));
            let got = search(g, &|x| l2(q, &pts[x as usize]), ef, &|_| true);
            assert!(got.windows(2).all(|w| w[0].0 <= w[1].0));
            hits += exact[..10]
                .iter()
                .filter(|e| got[..10].iter().any(|g| g.1 == e.1))
                .count();
        }
        hits as f64 / 2000.0
    }

    #[test]
    fn finds_nearest_neighbours() {
        let pts = Points(points(5000, 1));
        let p = Params {
            m: 8,
            ef_construction: 64,
            seed: 3,
            sequential: false,
        };
        let g = Graph::build(&pts, &p, &ctl()).unwrap();
        assert!(g.check());
        assert_eq!(g.nodes, 5000);
        let r = recall(&g, &pts.0, 32);
        assert!(r > 0.95, "{r}");
        // a filter: only even nodes
        let q = &points(1, 98)[0];
        let got = g.search(&|x| l2(q, &pts.0[x as usize]), 20, &|x| x % 2 == 0);
        assert_eq!(got.len(), 20);
        assert!(got.iter().all(|x| x.1 % 2 == 0));
        // the levels read back from the packed layers are the ones drawn
        assert_eq!(g.levels(), Builder::levels_of(5000, &p));
    }

    /// A sequential build gives the same graph every time, whatever runs beside it.
    #[test]
    fn sequential_builds_are_reproducible() {
        let pts = Points(points(3000, 5));
        let p = Params {
            m: 8,
            ef_construction: 32,
            seed: 7,
            sequential: true,
        };
        let a = Graph::build(&pts, &p, &ctl()).unwrap();
        let b = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap()
            .install(|| Graph::build(&pts, &p, &ctl()).unwrap());
        assert!(a.check());
        assert_eq!((a.entry, a.top, a.nodes), (b.entry, b.top, b.nodes));
        assert_eq!(&*a.level0, &*b.level0);
        assert_eq!(a.upper.len(), b.upper.len());
        for (x, y) in a.upper.iter().zip(&b.upper) {
            assert_eq!((&*x.0, &*x.1), (&*y.0, &*y.1));
        }
    }

    /// A builder seeded with a graph's links and given more nodes keeps the graph's
    /// quality: the frozen graph of the first nodes, carried into a builder over all of
    /// them, with the rest inserted.
    #[test]
    fn seeded_builders_extend_a_graph() {
        let all = points(6000, 4);
        let p = Params {
            m: 8,
            ef_construction: 64,
            seed: 3,
            sequential: false,
        };
        let first = Points(all[..5000].to_vec());
        let g = Graph::build(&first, &p, &ctl()).unwrap();
        let pts = Points(all.clone());
        let mut levels = g.levels();
        levels.extend((5000..6000).map(|i| level_of(i, &p) as u8));
        let b = Builder::new(&pts, &p, levels.clone());
        let mut buf = Vec::new();
        for x in 0..5000u32 {
            for l in 0..=levels[x as usize] as usize {
                buf.clear();
                g.links(l, x, &mut buf);
                b.seed(l, x, &buf);
            }
        }
        b.set_entry(g.entry);
        let rest: Vec<u32> = (5000..6000).collect();
        assert!(b.insert_all(&rest, &ctl()));
        let h = b.freeze();
        assert!(h.check());
        assert_eq!(h.nodes, 6000);
        let r = recall(&h, &all, 32);
        assert!(r > 0.95, "{r}");
    }

    #[test]
    fn small_and_cancelled() {
        let pts = Points(points(3, 1));
        let p = Params {
            m: 4,
            ef_construction: 8,
            seed: 0,
            sequential: false,
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
