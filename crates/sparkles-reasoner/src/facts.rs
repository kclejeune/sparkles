//! The base tier: the input graphs of a run, read from the snapshot's index permutations
//! instead of a copy in memory.
//!
//! An access with a bound subject reads GSPO when there is one input graph, and the
//! other permutations otherwise pick the bound positions as the key prefix (PSO for a
//! predicate and a subject or a predicate alone, POS for a predicate and an object, OSP
//! for an object alone, SPO for a subject alone). Every permutation key ends with the
//! graph column, so the rows of one triple in several input graphs are adjacent.
//!
//! Facts come out in the order an in-memory copy of the input graphs would list them:
//! graph by graph, in the order the run resolved them, each graph's facts in `(s, p, o)`
//! order filtered by the bound positions, and a triple held in several input graphs at
//! its first graph only. With one input graph a scan reads its rows once. With several,
//! a scan makes one pass per input graph over the key range and emits, in pass `i`, the
//! triples whose first input graph is graph `i`. Rule evaluation and the list builtins
//! therefore see facts in the same order as when the input was copied into memory. The
//! passes cost one read of the range per input graph, a tradeoff of scan time for not
//! holding the input in memory.
//!
//! Rule evaluation looks facts up once per candidate of the step before, so lookups go
//! through a [`Memo`] of the blocks the caller read last: a lookup in one of them is a
//! binary search, without the shared block cache and its reference counts. Key ranges
//! with changes in the snapshot's delta are read with the snapshot's merging scan.

use crate::graph::Triple;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::{ALL_COLS, Block, Key, Perm, PermIndex, pad};
use sparkles_core::store::{Chunk, Snapshot};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Predicates with at most this many distinct objects are checked by predicate and
/// object before a probe of the base tier.
const FEW_OBJECTS: u64 = 4096;

/// What [`Base::may_contain`] learned: by predicate, whether the store has it and has
/// few distinct objects for it, and by predicate and object, whether the store has them.
#[derive(Default)]
pub(crate) struct ProbeFilter {
    p: FxHashMap<u64, (bool, bool)>,
    po: FxHashMap<(u64, u64), bool>,
}

/// A contiguous key range of one permutation, a share of a scan, in the pass of one
/// input graph.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BaseRange {
    perm: Perm,
    lo: Key,
    hi: Key,
    /// the position of the input graph whose triples the share emits
    pass: usize,
}

/// The blocks a reader used last.
#[derive(Default)]
pub(crate) struct Memo {
    slots: Vec<(u64, usize, Block)>,
    next: usize,
    /// scans made through this memo, not yet added to the tier's count
    pub scans: u64,
}

const MEMO_SLOTS: usize = 8;

impl Memo {
    fn block(&mut self, snap: &Snapshot, idx: &PermIndex, b: usize) -> anyhow::Result<&Block> {
        if let Some(i) = self
            .slots
            .iter()
            .position(|(u, x, _)| *u == idx.uid && *x == b)
        {
            return Ok(&self.slots[i].2);
        }
        let blk = snap.cache.get_cols(idx, b, ALL_COLS)?;
        let i = if self.slots.len() < MEMO_SLOTS {
            self.slots.push((idx.uid, b, blk));
            self.slots.len() - 1
        } else {
            let i = self.next;
            self.next = (self.next + 1) % MEMO_SLOTS;
            self.slots[i] = (idx.uid, b, blk);
            i
        };
        Ok(&self.slots[i].2)
    }
}

pub(crate) struct Base {
    pub snap: Arc<Snapshot>,
    /// the input graph ids, in the order the run resolved them, without repeats
    graphs: Vec<u64>,
    /// the input graph ids, sorted, with their positions in `graphs`
    sorted: Vec<(u64, usize)>,
    /// an upper bound of the number of facts (the sum over the input graphs)
    len: u32,
    /// per predicate: quads in the store (an upper bound of the tier's)
    counts: RwLock<FxHashMap<u64, u64>>,
    scans: AtomicU64,
}

/// The positions of a key's columns: `at[c]` is the key column holding quad component
/// `c` (subject, predicate, object, graph).
fn positions(perm: Perm) -> [usize; 4] {
    let mut at = [0; 4];
    for (i, c) in perm.order().into_iter().enumerate() {
        at[c] = i;
    }
    at
}

/// The key before `k` (`k` must not be all zeros).
fn pred(mut k: Key) -> Key {
    for c in (0..4).rev() {
        if k[c] > 0 {
            k[c] -= 1;
            return k;
        }
        k[c] = u64::MAX;
    }
    k
}

/// Whether the delta of `snap` changes a key of `perm` in `[lo, hi]`.
fn touched(snap: &Snapshot, perm: Perm, lo: &Key, hi: &Key) -> bool {
    let i = perm.index();
    let (ins, del) = (&snap.delta.ins[i], &snap.delta.del[i]);
    (!ins.is_empty() && ins.intersects(*lo..=*hi)) || (!del.is_empty() && del.intersects(*lo..=*hi))
}

impl Base {
    /// The base tier of `graphs` in `snap`.
    pub fn new(snap: Arc<Snapshot>, graphs: &[Id]) -> anyhow::Result<Base> {
        // the first occurrence of each graph keeps its place
        let mut ids: Vec<u64> = Vec::with_capacity(graphs.len());
        for g in graphs {
            if !ids.contains(&g.0) {
                ids.push(g.0);
            }
        }
        let mut sorted: Vec<(u64, usize)> = ids.iter().enumerate().map(|(i, &g)| (g, i)).collect();
        sorted.sort_unstable();
        let mut n = 0u64;
        for g in &ids {
            n += snap.count(Perm::Gspo, &[*g])?;
        }
        if n >= (u32::MAX / 2) as u64 {
            anyhow::bail!("the input graphs hold {n} triples, more than the reasoner's row space");
        }
        Ok(Base {
            snap,
            graphs: ids,
            sorted,
            len: n as u32,
            counts: RwLock::new(FxHashMap::default()),
            scans: AtomicU64::new(0),
        })
    }

    /// An upper bound of the number of facts.
    #[inline]
    pub fn len(&self) -> u32 {
        self.len
    }

    /// The input graphs, in the order the run resolved them.
    pub fn graph_ids(&self) -> Vec<Id> {
        self.graphs.iter().map(|&g| Id(g)).collect()
    }

    /// Scans issued so far (by readers that reported them).
    pub fn scans(&self) -> u64 {
        self.scans.load(Ordering::Relaxed)
    }

    /// Add the scans a reader made.
    pub fn add_scans(&self, n: u64) {
        if n > 0 {
            self.scans.fetch_add(n, Ordering::Relaxed);
        }
    }

    fn single(&self) -> Option<u64> {
        match self.graphs[..] {
            [g] => Some(g),
            _ => None,
        }
    }

    /// The position of input graph `g` in the run's order.
    #[inline]
    fn order_of(&self, g: u64) -> Option<usize> {
        self.sorted
            .binary_search_by_key(&g, |x| x.0)
            .ok()
            .map(|i| self.sorted[i].1)
    }

    /// The passes of an ordered scan: one per input graph.
    #[inline]
    fn passes(&self) -> usize {
        self.graphs.len().max(1)
    }

    /// The permutation and the key range of a pattern.
    fn access(&self, s: Option<u64>, p: Option<u64>, o: Option<u64>) -> (Perm, Key, Key) {
        let (perm, prefix, n): (Perm, [u64; 4], usize) = match (self.single(), s, p, o) {
            (Some(g), Some(s), Some(p), Some(o)) => (Perm::Gspo, [g, s, p, o], 4),
            (Some(g), Some(s), Some(p), None) => (Perm::Gspo, [g, s, p, 0], 3),
            (Some(g), Some(s), None, _) => (Perm::Gspo, [g, s, 0, 0], 2),
            (Some(g), None, None, None) => (Perm::Gspo, [g, 0, 0, 0], 1),
            (_, Some(s), Some(p), Some(o)) => (Perm::Pso, [p, s, o, 0], 3),
            (_, Some(s), Some(p), None) => (Perm::Pso, [p, s, 0, 0], 2),
            (_, None, Some(p), Some(o)) => (Perm::Pos, [p, o, 0, 0], 2),
            (_, None, Some(p), None) => (Perm::Pso, [p, 0, 0, 0], 1),
            (_, Some(s), None, _) => (Perm::Spo, [s, 0, 0, 0], 1),
            (_, None, None, Some(o)) => (Perm::Osp, [o, 0, 0, 0], 1),
            (_, None, None, None) => (Perm::Spo, [0; 4], 0),
        };
        (perm, pad(&prefix[..n], 0), pad(&prefix[..n], u64::MAX))
    }

    /// Visit the facts matching a pattern, in the order of the module documentation.
    /// `f` returns false to stop; the result is false when it did.
    pub fn for_each(
        &self,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        self.for_each_with(&mut Memo::default(), s, p, o, f)
    }

    /// [`for_each`](Self::for_each), reading blocks through `memo`.
    pub fn for_each_with(
        &self,
        memo: &mut Memo,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        let (perm, lo, hi) = self.access(s, p, o);
        let mut f = f;
        for pass in 0..self.passes() {
            if !self.scan(memo, perm, lo, hi, [s, p, o], Some(pass), &mut f)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether a fact matches the pattern: one pass, whatever the number of input
    /// graphs.
    pub fn any(
        &self,
        memo: &mut Memo,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
    ) -> anyhow::Result<bool> {
        let (perm, lo, hi) = self.access(s, p, o);
        Ok(!self.scan(memo, perm, lo, hi, [s, p, o], None, |_| false)?)
    }

    /// [`for_each`](Self::for_each) over one share of a pattern's scan.
    pub fn for_range(
        &self,
        memo: &mut Memo,
        r: &BaseRange,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        self.scan(memo, r.perm, r.lo, r.hi, [s, p, o], Some(r.pass), f)
    }

    /// Scan `[lo, hi]` of `perm`. With several input graphs, pass `Some(i)` emits the
    /// triples whose first input graph is the one at position `i`, and `None` emits each
    /// triple once, in key order.
    #[allow(clippy::too_many_arguments)]
    fn scan(
        &self,
        memo: &mut Memo,
        perm: Perm,
        lo: Key,
        hi: Key,
        bound: [Option<u64>; 3],
        pass: Option<usize>,
        mut f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        memo.scans += 1;
        let at = positions(perm);
        let single = self.single();
        // a graph column outside the key prefix needs a check
        let check_graph = !(perm == Perm::Gspo && single.is_some());
        let multi = single.is_none();
        // the triple of the rows read last, and the first input graph position among
        // them: its rows are adjacent, so it is emitted once they end
        let mut cur: Option<(Triple, usize)> = None;
        let mut completed = true;
        let mut row = |k: [u64; 4], f: &mut dyn FnMut(Triple) -> bool| -> bool {
            let mut order = 0;
            if check_graph {
                let g = k[at[3]];
                let ok = match single {
                    Some(x) => g == x,
                    None => match self.order_of(g) {
                        Some(i) => {
                            order = i;
                            true
                        }
                        None => false,
                    },
                };
                if !ok {
                    return true;
                }
            }
            let t = [k[at[0]], k[at[1]], k[at[2]]];
            for c in 0..3 {
                if bound[c].is_some_and(|v| v != t[c]) {
                    return true;
                }
            }
            if !multi {
                if f(t) {
                    return true;
                }
                completed = false;
                return false;
            }
            match &mut cur {
                Some((x, first)) if *x == t => {
                    *first = (*first).min(order);
                    true
                }
                _ => match cur.replace((t, order)) {
                    Some((x, first)) if pass.is_none_or(|p| p == first) && !f(x) => {
                        completed = false;
                        cur = None;
                        false
                    }
                    _ => true,
                },
            }
        };
        // the last triple of a multi-graph scan that ran to the end
        let last = |cur: Option<(Triple, usize)>, f: &mut dyn FnMut(Triple) -> bool| match cur {
            Some((x, first)) if pass.is_none_or(|p| p == first) => f(x),
            _ => true,
        };
        let snap = &*self.snap;
        if touched(snap, perm, &lo, &hi) {
            snap.scan_between(perm, lo, hi, |c| {
                Ok(match c {
                    Chunk::Block(b, s, e) => {
                        let cols = &b.cols;
                        (s..e)
                            .all(|i| row([cols[0][i], cols[1][i], cols[2][i], cols[3][i]], &mut f))
                    }
                    Chunk::Row(k) => row(k, &mut f),
                })
            })?;
        } else {
            let idx = snap.perm(perm);
            let (b0, b1) = idx.key_block_range(&lo, &hi);
            for b in b0..b1 {
                let m = &idx.blocks[b];
                let whole = m.first >= lo && m.last <= hi;
                let blk = memo.block(snap, idx, b)?;
                let (s, e) = if whole {
                    (0, blk.len())
                } else {
                    blk.key_range(&lo, &hi)
                };
                let cols = &blk.cols;
                if !(s..e).all(|i| row([cols[0][i], cols[1][i], cols[2][i], cols[3][i]], &mut f)) {
                    break;
                }
            }
        }
        Ok(completed && last(cur, &mut f))
    }

    /// The pattern's scan cut into contiguous shares of about `rows` rows each, in the
    /// order of [`for_each`](Self::for_each): the shares of the first input graph's pass,
    /// then those of the next. No cut separates the rows of one triple, so a share sees
    /// every input graph that holds its triples.
    pub fn ranges(
        &self,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        rows: usize,
    ) -> anyhow::Result<Vec<BaseRange>> {
        let (perm, lo, hi) = self.access(s, p, o);
        let idx = self.snap.perm(perm);
        let (b0, b1) = idx.key_block_range(&lo, &hi);
        let rows = rows.max(1);
        // keys that start a share, after the first: block boundaries, and rows within
        // the blocks when there are few
        let within = b1 - b0 < 64;
        let mut cuts: Vec<Key> = Vec::new();
        // with several input graphs, the first key of the triple of `k`: the graph
        // column, which is last, at its least
        let multi = self.single().is_none();
        let triple_start = |mut k: Key| {
            if multi {
                k[3] = 0;
            }
            k
        };
        for b in b0..b1 {
            let m = &idx.blocks[b];
            if b > b0 && m.first > lo {
                cuts.push(triple_start(m.first));
            }
            if within && m.rows as usize > rows {
                let blk = self.snap.cache.get_cols(idx, b, ALL_COLS)?;
                let (s0, e0) = if m.first >= lo && m.last <= hi {
                    (0, blk.len())
                } else {
                    blk.key_range(&lo, &hi)
                };
                let mut i = s0 + rows;
                while i < e0 {
                    let k = triple_start(blk.key(i));
                    if k > lo {
                        cuts.push(k);
                    }
                    i += rows;
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut keys = Vec::with_capacity(cuts.len() + 1);
        let mut from = lo;
        for c in cuts {
            if c <= from || c > hi {
                continue;
            }
            keys.push((from, pred(c)));
            from = c;
        }
        keys.push((from, hi));
        let mut out = Vec::with_capacity(keys.len() * self.passes());
        for pass in 0..self.passes() {
            out.extend(
                keys.iter()
                    .map(|&(lo, hi)| BaseRange { perm, lo, hi, pass }),
            );
        }
        Ok(out)
    }

    /// Whether a triple is a fact of the tier.
    pub fn contains(&self, t: &Triple) -> anyhow::Result<bool> {
        self.contains_with(&mut Memo::default(), t)
    }

    /// [`contains`](Self::contains), reading blocks through `memo`.
    pub fn contains_with(&self, memo: &mut Memo, t: &Triple) -> anyhow::Result<bool> {
        if t.iter().any(|&x| Id(x).tag() == Tag::Local) {
            return Ok(false);
        }
        let snap = &*self.snap;
        for &g in &self.graphs {
            let (perm, key) = if self.single().is_some() {
                (Perm::Gspo, [g, t[0], t[1], t[2]])
            } else {
                (Perm::Spo, [t[0], t[1], t[2], g])
            };
            let found = if touched(snap, perm, &key, &key) {
                snap.contains(&[Id(t[0]), Id(t[1]), Id(t[2]), Id(g)])?
            } else {
                let idx = snap.perm(perm);
                let b = idx.blocks.partition_point(|m| m.last < key);
                if b >= idx.blocks.len() || idx.blocks[b].first > key {
                    false
                } else {
                    let (s, e) = memo.block(snap, idx, b)?.prefix_range(&key);
                    s < e
                }
            };
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// [`contains`](Self::contains) for triples in sorted order: one pass per input
    /// graph over the blocks they fall in, searching forward from the previous triple's
    /// row.
    pub fn contains_sorted(&self, ts: &[Triple]) -> anyhow::Result<Vec<bool>> {
        let mut found = vec![false; ts.len()];
        let snap = &*self.snap;
        let single = self.single().is_some();
        let mut memo = Memo::default();
        for &g in &self.graphs {
            let (perm, key): (Perm, fn(u64, &Triple) -> Key) = if single {
                (Perm::Gspo, |g, t| [g, t[0], t[1], t[2]])
            } else {
                (Perm::Spo, |g, t| [t[0], t[1], t[2], g])
            };
            let idx = snap.perm(perm);
            // the block and row the last search ended at
            let mut at: Option<(usize, usize)> = None;
            for (i, t) in ts.iter().enumerate() {
                if found[i] || t.iter().any(|&x| Id(x).tag() == Tag::Local) {
                    continue;
                }
                let k = key(g, t);
                if touched(snap, perm, &k, &k) {
                    found[i] = snap.contains(&[Id(t[0]), Id(t[1]), Id(t[2]), Id(g)])?;
                    continue;
                }
                let b = match at {
                    Some((b, _)) if idx.blocks[b].last >= k => b,
                    Some((b, _)) => b + idx.blocks[b..].partition_point(|m| m.last < k),
                    None => idx.blocks.partition_point(|m| m.last < k),
                };
                if b >= idx.blocks.len() {
                    continue;
                }
                if idx.blocks[b].first > k {
                    at = Some((b, 0));
                    continue;
                }
                let from = match at {
                    Some((ab, r)) if ab == b => r,
                    _ => 0,
                };
                let blk = memo.block(snap, idx, b)?;
                // gallop forward from `from` to the first row not less than `k`
                let n = blk.len();
                let less = |r: usize| blk.key(r) < k;
                let mut step = 1;
                let mut lo = from;
                let mut hi = from;
                while hi < n && less(hi) {
                    lo = hi + 1;
                    hi = (hi + step).min(n);
                    step *= 2;
                }
                let (mut a, mut z) = (lo, hi.min(n));
                while a < z {
                    let m = (a + z) / 2;
                    if less(m) { a = m + 1 } else { z = m }
                }
                found[i] = a < n && blk.key(a) == k;
                at = Some((b, a));
            }
        }
        Ok(found)
    }

    /// False when `t` is certainly not a fact of the tier: a term is not in the store,
    /// no quad has its predicate, or, for a predicate with few distinct objects, no quad
    /// has its predicate and object.
    pub fn may_contain(&self, t: &Triple, f: &mut ProbeFilter) -> bool {
        if t.iter().any(|&x| Id(x).tag() == Tag::Local) {
            return false;
        }
        let (any, few_objects) =
            *f.p.entry(t[1])
                .or_insert_with(|| (self.count_p(t[1]) > 0, self.pstats(t[1]).1 <= FEW_OBJECTS));
        if !any || !few_objects {
            return any;
        }
        *f.po.entry((t[1], t[2])).or_insert_with(|| {
            self.snap
                .count(Perm::Pos, &[t[1], t[2]])
                .map_or(true, |n| n > 0)
        })
    }

    /// Quads with predicate `p` in the store: an upper bound of the tier's facts.
    pub fn count_p(&self, p: u64) -> u64 {
        if let Some(n) = self.counts.read().get(&p) {
            return *n;
        }
        let n = self.snap.count(Perm::Pso, &[p]).unwrap_or(u64::MAX / 4);
        self.counts.write().insert(p, n);
        n
    }

    /// (distinct subjects, distinct objects) of predicate `p` in the store.
    pub fn pstats(&self, p: u64) -> (u64, u64) {
        self.snap
            .predicate_stat(p)
            .map_or((1, 1), |s| (s.distinct_subjects, s.distinct_objects))
    }

    pub fn distinct_predicates(&self) -> usize {
        self.snap.generation.stats.distinct_predicates as usize
    }
}
