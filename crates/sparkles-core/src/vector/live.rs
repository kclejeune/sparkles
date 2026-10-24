//! Nodes inserted into a built HNSW graph after its build: the rows that commits added
//! to an indexed predicate since the generation's base.
//!
//! A configured index's graph covers the base rows of its generation and is frozen (it
//! can be mapped from the index file). Rows inserted since sit in each snapshot's delta,
//! and a search scores them exactly, which costs a parse and a distance per row on every
//! query. A background task ([`catch_up`]) inserts them into a mutable layer of the
//! index, in commit order and after each commit is published, so writes never wait.
//!
//! * **Nodes.** A row's (subject, vector) pair becomes a node numbered after the frozen
//!   ones, as the rows of one pair in several graphs share a frozen node. The layer keeps
//!   its vector, its links per layer, and its key, by which a search finds the snapshot's
//!   rows of the pair.
//! * **Links.** New nodes are inserted by Algorithm 1 with the neighbour heuristic, in
//!   batches of up to [`BATCH`]: each batch member searches the graph as it stands and
//!   also considers the other members, its links are chosen by the heuristic, and the
//!   reverse links it adds are merged per target and shrunk by the heuristic when a list
//!   is full. A frozen node whose links change gets a replacement list in a side table,
//!   which searches read instead of the frozen one. The search and selection run in
//!   parallel under a read lock, so searches go on, and only the copy of the results
//!   into the layer takes the write lock.
//! * **Visibility.** The layer is shared by every snapshot of the generation. A search
//!   accepts a live node only when its own snapshot's delta holds a row of the pair, so
//!   it sees exactly its own rows: a row inserted later is not in its delta, a row
//!   deleted since is not either (the node stays and is still traversed), and a row in
//!   its delta that the layer lacks is still scored exactly. A row scores the same
//!   through the layer as exactly, because the layer keeps the vector parsed from the
//!   literal and its norm.
//! * **Lifetime.** The layer lives in memory with its build and is not written to the
//!   index file. Reopening a store inserts the delta's rows again, and a compaction
//!   carries the layer's nodes into the new generation's frozen graph (see
//!   [`super::index`]).

use super::hnsw::{self, Graph, Layers, Near, Params, Space};
use super::index::{Built, Segment};
use super::{Metric, graph_dist, norm};
use crate::index::Perm;
use crate::store::Snapshot;
use crate::store::keyset::KeySet;
use parking_lot::{RwLock, RwLockReadGuard, RwLockUpgradableReadGuard};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// Rows inserted per batch.
pub const BATCH: usize = 512;

/// The `by_key` value of a pair that was looked at but is no node: its vector cannot be
/// scored under the index's metric (a zero norm under cosine), or has another dimension.
const SKIPPED: u32 = u32::MAX;

/// A (subject, object) pair to insert and its vector (`None`: not a vector).
pub(crate) type Item = ((u64, u64), Option<Arc<[f32]>>);

/// The mutable layer of a built index.
#[derive(Default)]
pub struct Live {
    state: RwLock<LiveState>,
}

/// The nodes inserted since the build.
#[derive(Clone, Default)]
pub struct LiveState {
    /// the frozen graph's node count: live node `i` is node `base + i`
    base: u32,
    dim: usize,
    /// (subject, object) per live node
    keys: Vec<(u64, u64)>,
    /// the vectors, `dim` values per live node, and their norms
    data: Vec<f32>,
    norms: Vec<f32>,
    levels: Vec<u8>,
    /// links per live node and layer (0..=level)
    links: Vec<Vec<Vec<u32>>>,
    /// the node of each pair looked at, or `SKIPPED`
    by_key: FxHashMap<(u64, u64), u32>,
    /// replacement links of frozen nodes, by (layer, node)
    side: FxHashMap<(u8, u32), Vec<u32>>,
    /// the entry point when a live node is above the frozen top (or the frozen graph is
    /// empty)
    entry: Option<(u32, usize)>,
    /// bytes of links in `links` and `side`
    link_bytes: u64,
    /// the last snapshot whose rows were all looked at: a search of a snapshot with the
    /// same delta has no row to score exactly
    caught: Option<Caught>,
}

/// The snapshot a layer last caught up with.
#[derive(Clone)]
struct Caught {
    /// its commit
    commit: u64,
    /// its inserted rows under the predicate
    rows: u64,
    /// its inserted keys in PSO order, which identify the delta: a transaction's view at
    /// the same commit that changed the delta, or an access view that masks part of it,
    /// has another root. Holding the set keeps its root alive, so a later tree cannot
    /// reuse the address and match falsely.
    ins: KeySet,
}

impl Live {
    /// An empty layer over a frozen graph of `base` nodes of dimension `dim`.
    pub fn new(base: usize, dim: usize) -> Live {
        Live {
            state: RwLock::new(LiveState {
                base: base as u32,
                dim,
                ..Default::default()
            }),
        }
    }

    /// The layer as it stands (searches hold this while they walk the graph).
    pub fn read(&self) -> RwLockReadGuard<'_, LiveState> {
        self.state.read()
    }

    /// Insert `items`, (subject, object) pairs and their vectors (`None`: not a vector),
    /// into the graph of `b`. Pairs already looked at are skipped. Returns the nodes
    /// added.
    pub(crate) fn insert(&self, b: &Built, items: Vec<Item>, p: &Params) -> usize {
        let Some(g) = b.graph.as_deref() else {
            return 0;
        };
        let st = self.state.upgradable_read();
        let metric = b.config.metric;
        let dim = st.dim;
        let mut skipped: Vec<(u64, u64)> = Vec::new();
        // the pairs that become nodes, with their vectors and norms
        let (mut valid, mut bdata, mut bnorms) = (Vec::new(), Vec::new(), Vec::new());
        let mut batch_keys: FxHashSet<(u64, u64)> = FxHashSet::default();
        for (key, v) in items {
            if st.by_key.contains_key(&key) || !batch_keys.insert(key) {
                continue;
            }
            match v {
                Some(v) if v.len() == dim => {
                    let n = norm(&v);
                    if metric == Metric::Cosine && n == 0.0 {
                        skipped.push(key);
                    } else {
                        valid.push(key);
                        bdata.extend_from_slice(&v);
                        bnorms.push(n);
                    }
                }
                _ => skipped.push(key),
            }
        }
        let first = st.base + st.keys.len() as u32;
        let nb = valid.len();
        let levels: Vec<u8> = (0..nb as u32)
            .map(|j| hnsw::level_of(first + j, p) as u8)
            .collect();
        let vecs = Vectors {
            seg: &b.segment,
            node_rows: &b.node_rows,
            live: &st,
            batch: &bdata,
            batch_norms: &bnorms,
            first,
            metric,
            len: first as usize + nb,
        };
        let view = View {
            graph: g,
            live: &st,
        };
        let chosen = choose(&vecs, &view, &levels, first, p);
        let updates = reverse(&vecs, &view, &chosen, first, p);
        // only the copy into the layer waits for the searches under way
        let mut w = RwLockUpgradableReadGuard::upgrade(st);
        let top = w.entry.or(Layers::entry(g)).map(|e| e.1);
        let mut best: Option<(u32, usize)> = None;
        w.data.extend_from_slice(&bdata);
        w.norms.extend_from_slice(&bnorms);
        for (j, (key, links)) in valid.into_iter().zip(chosen).enumerate() {
            let id = first + j as u32;
            let l = levels[j] as usize;
            w.keys.push(key);
            w.levels.push(levels[j]);
            w.link_bytes += links.iter().map(|x| x.len() as u64 * 4 + 24).sum::<u64>();
            w.links.push(links);
            w.by_key.insert(key, id);
            if top.is_none_or(|t| l > t) && best.is_none_or(|b| l > b.1) {
                best = Some((id, l));
            }
        }
        for key in skipped {
            w.by_key.insert(key, SKIPPED);
        }
        let base = w.base;
        for ((l, e), list) in updates {
            if e < base {
                w.link_bytes += list.len() as u64 * 4 + 32;
                if let Some(old) = w.side.insert((l, e), list) {
                    w.link_bytes -= old.len() as u64 * 4 + 32;
                }
            } else {
                let i = (e - base) as usize;
                let old = std::mem::replace(&mut w.links[i][l as usize], list);
                let new = w.links[i][l as usize].len() as u64;
                w.link_bytes = w.link_bytes + new * 4 - old.len() as u64 * 4;
            }
        }
        if best.is_some() {
            w.entry = best;
        }
        nb
    }
}

impl LiveState {
    /// Live nodes.
    pub fn nodes(&self) -> usize {
        self.keys.len()
    }

    /// The node of pair (s, o), if it is one.
    #[inline]
    pub fn node_of(&self, s: u64, o: u64) -> Option<u32> {
        if self.by_key.is_empty() {
            return None;
        }
        self.by_key.get(&(s, o)).copied().filter(|&x| x != SKIPPED)
    }

    /// Whether pair (s, o) was looked at (a node, or skipped).
    pub fn seen(&self, s: u64, o: u64) -> bool {
        self.by_key.contains_key(&(s, o))
    }

    /// Whether the layer holds every row it can score of `snap`'s delta under the
    /// predicate, of which there are `rows`: the layer last caught up with a snapshot at
    /// the same commit, with as many rows and the same inserted keys. A transaction's
    /// view shares the published commit and may hold as many rows after a replacement,
    /// so the keys' identity tells it apart.
    pub fn is_caught_up(&self, snap: &Snapshot, rows: u64) -> bool {
        self.caught.as_ref().is_some_and(|c| {
            c.commit == snap.commit
                && c.rows == rows
                && c.ins.same(&snap.delta.ins[Perm::Pso.index()])
        })
    }

    /// The first live node's number (the frozen graph's node count).
    pub fn base(&self) -> u32 {
        self.base
    }

    /// The (subject, object) pair of live node `x` (`x ≥ base`).
    pub fn key(&self, x: u32) -> (u64, u64) {
        self.keys[(x - self.base) as usize]
    }

    /// The vector and norm of live node `x` (`x ≥ base`).
    #[inline]
    pub fn vector(&self, x: u32) -> (&[f32], f32) {
        let i = (x - self.base) as usize;
        (&self.data[i * self.dim..(i + 1) * self.dim], self.norms[i])
    }

    /// The level of live node `x`.
    pub fn level(&self, x: u32) -> usize {
        self.levels[(x - self.base) as usize] as usize
    }

    /// The links of node `x` on layer `l` as the layer holds them: a live node's, or a
    /// frozen node's replacement list.
    pub fn links_of(&self, l: usize, x: u32) -> Option<&[u32]> {
        if x >= self.base {
            let i = (x - self.base) as usize;
            self.links
                .get(i)
                .and_then(|v| v.get(l))
                .map(|v| v.as_slice())
        } else if self.side.is_empty() {
            None
        } else {
            self.side.get(&(l as u8, x)).map(|v| v.as_slice())
        }
    }

    /// The bottom layer as the projection's neighbour layout reads it: the frozen node
    /// count, the pair of each live node, the links of each live node, and the
    /// replacement links of frozen nodes. A copy, so the layout does not hold the lock.
    pub fn layer0(&self) -> Layer0 {
        Layer0 {
            base: self.base,
            keys: self.keys.clone(),
            links: self
                .links
                .iter()
                .map(|l| l.first().cloned().unwrap_or_default())
                .collect(),
            side: self
                .side
                .iter()
                .filter(|((l, _), _)| *l == 0)
                .map(|((_, x), v)| (*x, v.clone()))
                .collect(),
        }
    }

    /// Heap bytes of the layer.
    pub fn bytes(&self) -> u64 {
        (self.data.len() * 4 + self.norms.len() * 4 + self.keys.len() * 16 + self.levels.len())
            as u64
            + self.link_bytes
            + self.by_key.len() as u64 * 24
    }
}

/// A copy of the bottom layer of a live layer ([`LiveState::layer0`]).
pub struct Layer0 {
    /// the frozen graph's node count
    pub base: u32,
    /// the (subject, object) pair of each live node
    pub keys: Vec<(u64, u64)>,
    /// the bottom-layer links of each live node
    pub links: Vec<Vec<u32>>,
    /// replacement bottom-layer links of frozen nodes
    pub side: FxHashMap<u32, Vec<u32>>,
}

impl Layer0 {
    /// The bottom-layer links of node `x` with the frozen graph `g` below.
    pub fn links(&self, g: &Graph, x: u32, out: &mut Vec<u32>) {
        if x >= self.base {
            if let Some(v) = self.links.get((x - self.base) as usize) {
                out.extend_from_slice(v);
            }
        } else if let Some(v) = self.side.get(&x) {
            out.extend_from_slice(v);
        } else {
            g.links(0, x, out);
        }
    }
}

/// A frozen graph and its live layer, searched as one graph.
pub struct View<'a> {
    pub graph: &'a Graph,
    pub live: &'a LiveState,
}

impl Layers for View<'_> {
    fn len(&self) -> usize {
        self.graph.nodes + self.live.keys.len()
    }

    fn entry(&self) -> Option<(u32, usize)> {
        self.live.entry.or_else(|| Layers::entry(self.graph))
    }

    #[inline]
    fn links(&self, l: usize, x: u32, out: &mut Vec<u32>) {
        match self.live.links_of(l, x) {
            Some(v) => out.extend_from_slice(v),
            None if x < self.live.base => self.graph.links(l, x, out),
            None => {}
        }
    }
}

/// The vectors of every node a batch insertion meets: frozen ones in the segment, live
/// ones in the layer, and the batch's own.
struct Vectors<'a> {
    seg: &'a Segment,
    node_rows: &'a [u32],
    live: &'a LiveState,
    batch: &'a [f32],
    batch_norms: &'a [f32],
    first: u32,
    metric: Metric,
    len: usize,
}

impl Vectors<'_> {
    #[inline]
    fn get(&self, x: u32) -> (&[f32], f32) {
        if x < self.live.base {
            let r = self.node_rows[x as usize] as usize;
            (self.seg.row(r), self.seg.norms[r])
        } else if x < self.first {
            self.live.vector(x)
        } else {
            let j = (x - self.first) as usize;
            let d = self.live.dim;
            (&self.batch[j * d..(j + 1) * d], self.batch_norms[j])
        }
    }
}

impl Space for Vectors<'_> {
    fn len(&self) -> usize {
        self.len
    }

    fn dist(&self, a: u32, b: u32) -> f32 {
        let ((va, na), (vb, nb)) = (self.get(a), self.get(b));
        graph_dist(self.metric, va, na, vb, nb)
    }
}

/// Algorithm 1's search and selection for every batch member, in parallel: the links
/// per layer of member `j` (node `first + j`), from the graph and the other members.
fn choose(
    vecs: &Vectors<'_>,
    view: &View<'_>,
    levels: &[u8],
    first: u32,
    p: &Params,
) -> Vec<Vec<Vec<u32>>> {
    let nb = levels.len();
    let entry = view.entry();
    let n = view.len();
    let efc = p.ef_construction.max(p.m);
    let all = |_: u32| true;
    (0..nb)
        .into_par_iter()
        .map(|j| {
            let id = first + j as u32;
            let lj = levels[j] as usize;
            let dist = |x: u32| vecs.dist(id, x);
            let mut per: Vec<Vec<Near>> = vec![Vec::new(); lj + 1];
            if let Some((ep, top)) = entry {
                let mut eps = vec![Near(dist(ep), ep)];
                for l in (lj + 1..=top).rev() {
                    let links = |x: u32, out: &mut Vec<u32>| view.links(l, x, out);
                    eps = hnsw::with_visited(n, |v| {
                        hnsw::search_layer(&eps, 1, &dist, &links, &all, v)
                    });
                }
                for l in (0..=lj.min(top)).rev() {
                    let links = |x: u32, out: &mut Vec<u32>| view.links(l, x, out);
                    let w = hnsw::with_visited(n, |v| {
                        hnsw::search_layer(&eps, efc, &dist, &links, &all, v)
                    });
                    if !w.is_empty() {
                        eps.clone_from(&w);
                    }
                    per[l] = w;
                }
            }
            // the other members, which the graph does not hold yet
            let mates: Vec<(Near, usize)> = (0..nb)
                .filter(|&k| k != j)
                .map(|k| {
                    (
                        Near(dist(first + k as u32), first + k as u32),
                        levels[k] as usize,
                    )
                })
                .collect();
            per.iter_mut()
                .enumerate()
                .map(|(l, c)| {
                    c.extend(mates.iter().filter(|m| m.1 >= l).map(|m| m.0));
                    c.sort_unstable();
                    c.truncate(efc);
                    hnsw::select(vecs, c, p.cap(l))
                })
                .collect()
        })
        .collect()
}

/// The reverse links the members add: per (layer, target), the target's links with the
/// new nodes, shrunk by the heuristic when over the layer's cap.
fn reverse(
    vecs: &Vectors<'_>,
    view: &View<'_>,
    chosen: &[Vec<Vec<u32>>],
    first: u32,
    p: &Params,
) -> Vec<((u8, u32), Vec<u32>)> {
    let mut adds: FxHashMap<(u8, u32), Vec<u32>> = FxHashMap::default();
    for (j, layers) in chosen.iter().enumerate() {
        for (l, links) in layers.iter().enumerate() {
            for &e in links {
                adds.entry((l as u8, e)).or_default().push(first + j as u32);
            }
        }
    }
    adds.into_par_iter()
        .map(|((l, e), new)| {
            let cap = p.cap(l as usize);
            let mut cur: Vec<u32> = if e >= first {
                chosen[(e - first) as usize][l as usize].clone()
            } else {
                let mut v = Vec::new();
                view.links(l as usize, e, &mut v);
                v
            };
            for x in new {
                if x != e && !cur.contains(&x) {
                    cur.push(x);
                }
            }
            if cur.len() > cap {
                let mut c: Vec<Near> = cur.iter().map(|&x| Near(vecs.dist(e, x), x)).collect();
                c.sort_unstable();
                cur = hnsw::select(vecs, &c, cap);
            }
            ((l, e), cur)
        })
        .collect()
}

/// The build parameters of `b`'s graph.
pub(crate) fn params_of(b: &Built) -> Option<Params> {
    b.config.hnsw.map(|h| Params {
        m: h.m,
        ef_construction: h.ef_construction,
        seed: super::index::SEED,
        sequential: false,
    })
}

/// The id of `b`'s predicate in `snap`: the one the build found, or, when the predicate
/// had no row in the generation's base, the one the delta's vocabulary gave it since.
/// Delta ids are stable within a generation, so the id is valid for every later
/// snapshot of it.
pub(crate) fn pred_in(b: &Built, snap: &Snapshot) -> Option<u64> {
    b.pred
        .or_else(|| snap.lookup_iri(&b.config.predicate).map(|i| i.0))
}

/// Insert the rows of `snap`'s delta that `b`'s graph lacks, in batches, until none is
/// left, `cancel` says stop, or the next batch would pass the vector memory budget, which
/// the layer counts against. Returns the nodes added.
pub(crate) fn catch_up(snap: &Snapshot, b: &Built, cancel: &dyn Fn() -> bool) -> usize {
    let (Some(_), Some(pred), Some(p)) = (b.graph.as_ref(), pred_in(b, snap), params_of(b)) else {
        return 0;
    };
    let gv = &snap.generation.vectors;
    let pi = Perm::Pso.index();
    let (lo, hi) = ([pred, 0, 0, 0], [pred, u64::MAX, u64::MAX, u64::MAX]);
    let mut total = 0;
    loop {
        if cancel() {
            return total;
        }
        let keys: Vec<(u64, u64)> = {
            let st = b.live.read();
            let mut seen: FxHashSet<(u64, u64)> = FxHashSet::default();
            let mut out = Vec::new();
            for k in snap.delta.ins[pi].range(lo..=hi) {
                let key = (k[1], k[2]);
                if !st.seen(key.0, key.1) && seen.insert(key) {
                    out.push(key);
                    if out.len() >= BATCH {
                        break;
                    }
                }
            }
            out
        };
        if keys.is_empty() {
            let rows = snap.delta.ins[pi].count_between(&lo, &hi) as u64;
            let mut st = b.live.state.write();
            if st.caught.as_ref().is_none_or(|c| c.commit <= snap.commit) {
                st.caught = Some(Caught {
                    commit: snap.commit,
                    rows,
                    ins: snap.delta.ins[pi].clone(),
                });
            }
            return total;
        }
        // the rows that become nodes, as many as fit in the budget; the others stay
        // unseen for a later pass
        let dim = b.segment.dim;
        let per_row = row_bytes(b, &p);
        let room = super::budget().saturating_sub(gv.used_bytes());
        let mut fit = room / per_row.max(1);
        let mut items: Vec<Item> = Vec::with_capacity(keys.len());
        let mut left = false;
        for key in keys {
            let v = gv.overlay_vector(snap, key.1);
            if v.as_ref().is_some_and(|v| v.len() == dim) {
                if fit == 0 {
                    left = true;
                    continue;
                }
                fit -= 1;
            }
            items.push((key, v));
        }
        if !items.is_empty() {
            total += b.live.insert(b, items, &p);
        }
        if left {
            return total;
        }
    }
}

/// Whether `b`'s live layer cannot take another node within the vector memory budget as
/// it stands now, so catch-up leaves the rows it lacks pending. Computed on each call,
/// so it turns false as soon as the budget is raised or other indexes shrink.
pub(crate) fn at_budget(snap: &Snapshot, b: &Built) -> bool {
    params_of(b)
        .is_some_and(|p| snap.generation.vectors.used_bytes() + row_bytes(b, &p) > super::budget())
}

/// An upper bound of the bytes one node adds to `b`'s live layer: its vector, norm,
/// key, level and map entry, and a full list of links on layer 0.
fn row_bytes(b: &Built, p: &Params) -> u64 {
    b.segment.dim as u64 * 4 + 4 + 16 + 1 + 24 + (2 * p.m as u64 + 1) * 4
}

/// The rows of `snap`'s delta under `b`'s predicate that its graph holds, and those
/// still pending (vectors of the index's dimension it can score but does not hold yet).
pub(crate) fn overlay_split(snap: &Snapshot, b: &Built) -> (u64, u64) {
    let (Some(_), Some(pred)) = (b.graph.as_ref(), pred_in(b, snap)) else {
        return (0, 0);
    };
    let gv = &snap.generation.vectors;
    let st = b.live.read();
    let pi = Perm::Pso.index();
    let (mut indexed, mut pending) = (0, 0);
    for k in snap.delta.ins[pi].range([pred, 0, 0, 0]..=[pred, u64::MAX, u64::MAX, u64::MAX]) {
        if st.node_of(k[1], k[2]).is_some() {
            indexed += 1;
        } else if !st.seen(k[1], k[2])
            && gv.overlay_vector(snap, k[2]).is_some_and(|v| {
                v.len() == b.segment.dim && !(b.config.metric == Metric::Cosine && norm(&v) == 0.0)
            })
        {
            pending += 1;
        }
    }
    (indexed, pending)
}
