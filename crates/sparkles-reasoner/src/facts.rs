//! The base tier: the input graphs of a run, read from the snapshot's index permutations
//! instead of a copy in memory.
//!
//! An access with a bound subject reads GSPO when there is one input graph, and the
//! other permutations otherwise pick the bound positions as the key prefix (PSO for a
//! predicate and a subject or a predicate alone, POS for a predicate and an object, OSP
//! for an object alone, SPO for a subject alone). Every permutation key ends with the
//! graph column, so a scan keeps the rows of the input graphs and drops the repeats of a
//! triple held in two of them, which are adjacent. The rows come out in the order an
//! in-memory copy of one graph would list them (`(s, p, o)` order filtered by the bound
//! positions), so evaluation over this tier visits facts in the same order.
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

/// A contiguous key range of one permutation, a share of a scan.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BaseRange {
    perm: Perm,
    lo: Key,
    hi: Key,
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
    /// the input graph ids, sorted
    graphs: Vec<u64>,
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
        let mut ids: Vec<u64> = graphs.iter().map(|g| g.0).collect();
        ids.sort_unstable();
        ids.dedup();
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

    /// The input graphs.
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

    /// Visit the facts matching a pattern, in order. `f` returns false to stop; the
    /// result is false when it did.
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
        self.scan(memo, perm, lo, hi, [s, p, o], f)
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
        self.scan(memo, r.perm, r.lo, r.hi, [s, p, o], f)
    }

    fn scan(
        &self,
        memo: &mut Memo,
        perm: Perm,
        lo: Key,
        hi: Key,
        bound: [Option<u64>; 3],
        mut f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        memo.scans += 1;
        let at = positions(perm);
        let single = self.single();
        // a graph column outside the key prefix needs a check
        let check_graph = !(perm == Perm::Gspo && single.is_some());
        let multi = single.is_none();
        let graphs = &self.graphs[..];
        let mut prev: Option<Triple> = None;
        let mut completed = true;
        let mut row = |k: [u64; 4]| -> bool {
            if check_graph {
                let g = k[at[3]];
                let ok = match single {
                    Some(x) => g == x,
                    None => graphs.binary_search(&g).is_ok(),
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
            if multi {
                if prev == Some(t) {
                    return true;
                }
                prev = Some(t);
            }
            if f(t) {
                true
            } else {
                completed = false;
                false
            }
        };
        let snap = &*self.snap;
        if touched(snap, perm, &lo, &hi) {
            snap.scan_between(perm, lo, hi, |c| {
                Ok(match c {
                    Chunk::Block(b, s, e) => {
                        let cols = &b.cols;
                        (s..e).all(|i| row([cols[0][i], cols[1][i], cols[2][i], cols[3][i]]))
                    }
                    Chunk::Row(k) => row(k),
                })
            })?;
            return Ok(completed);
        }
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
            if !(s..e).all(|i| row([cols[0][i], cols[1][i], cols[2][i], cols[3][i]])) {
                break;
            }
        }
        Ok(completed)
    }

    /// The pattern's scan cut into contiguous shares of about `rows` rows each, in
    /// order.
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
        for b in b0..b1 {
            let m = &idx.blocks[b];
            if b > b0 && m.first > lo {
                cuts.push(m.first);
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
                    let k = blk.key(i);
                    if k > lo {
                        cuts.push(k);
                    }
                    i += rows;
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut out = Vec::with_capacity(cuts.len() + 1);
        let mut from = lo;
        for c in cuts {
            if c <= from || c > hi {
                continue;
            }
            out.push(BaseRange {
                perm,
                lo: from,
                hi: pred(c),
            });
            from = c;
        }
        out.push(BaseRange { perm, lo: from, hi });
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
