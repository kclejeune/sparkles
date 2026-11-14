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
//!
//! The base tier of a kept closure also has the closure's [`Lookups`], which answer
//! lookups with few facts without reading a block (see [`crate::lookups`]).

use crate::graph::Triple;
use crate::lookups::{self, Lookups, Read};
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

/// What [`Base::classify`] learned: by predicate, whether the store has it and has few
/// distinct objects for it.
#[derive(Default)]
pub(crate) struct ProbeFilter {
    p: FxHashMap<u64, (bool, bool)>,
}

/// What a probe of a triple in the base tier needs (see [`Base::classify`]).
pub(crate) enum Probe {
    /// none, since the tier certainly lacks it
    No,
    /// a probe
    Maybe,
    /// a probe when the store has its predicate and object
    Pair,
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

/// The blocks a reader used last, and the segments of blocks it read a few rows of.
#[derive(Default)]
pub(crate) struct Memo {
    /// (permutation instance, block, segment plus one or [`WHOLE`], the rows)
    slots: Vec<(u64, usize, usize, Block)>,
    next: usize,
    /// scans made through this memo, not yet added to the tier's count
    pub scans: u64,
    /// the rows of the blocks' key ranges that the scans through this memo entered, an
    /// upper bound of the rows they read, for the tests of bounded reads
    #[cfg(test)]
    pub rows: u64,
}

const MEMO_SLOTS: usize = 8;

/// The count of a pattern whose range of the index could not be read: larger than any
/// store, and small enough to add up.
pub(crate) const UNCOUNTED: u64 = u64::MAX / 4;

/// The rows a start of [`Base::sample`] enters before it stops: it ends with the block,
/// the segment or the row of the delta that takes it past this.
pub(crate) const SAMPLE_ROWS: u64 = 4096;

/// The fewest sorted triples a task of [`Base::contains_sorted`] searches.
const SORTED_RUN: usize = 64;

/// The segment field of a memo slot that holds a whole block.
const WHOLE: usize = 0;

/// Where the rows of a block in a key range are: the whole block, or segments `[s0, s1)`
/// of it, read one by one with [`Memo::segment`].
enum Rows<'a> {
    Whole(&'a Block),
    Segments(usize, usize),
}

impl Memo {
    fn slot(&self, uid: u64, b: usize, s: usize) -> Option<usize> {
        self.slots
            .iter()
            .position(|(u, x, y, _)| *u == uid && *x == b && *y == s)
    }

    fn keep(&mut self, uid: u64, b: usize, s: usize, blk: Block) -> &Block {
        let i = if self.slots.len() < MEMO_SLOTS {
            self.slots.push((uid, b, s, blk));
            self.slots.len() - 1
        } else {
            let i = self.next;
            self.next = (self.next + 1) % MEMO_SLOTS;
            self.slots[i] = (uid, b, s, blk);
            i
        };
        &self.slots[i].3
    }

    fn block(&mut self, snap: &Snapshot, idx: &PermIndex, b: usize) -> anyhow::Result<&Block> {
        if let Some(i) = self.slot(idx.uid, b, WHOLE) {
            return Ok(&self.slots[i].3);
        }
        let blk = snap.cache.get_cols(idx, b, ALL_COLS)?;
        Ok(self.keep(idx.uid, b, WHOLE, blk))
    }

    /// Segment `s` of block `b`, as a block of its rows.
    fn segment(
        &mut self,
        snap: &Snapshot,
        idx: &PermIndex,
        b: usize,
        s: usize,
    ) -> anyhow::Result<&Block> {
        if let Some(i) = self.slot(idx.uid, b, s + 1) {
            return Ok(&self.slots[i].3);
        }
        let blk = snap.cache.get_segment(idx, b, s, ALL_COLS, true)?;
        Ok(self.keep(idx.uid, b, s + 1, blk))
    }

    /// The rows of block `b` with keys in `[lo, hi]`: the whole block when this memo or
    /// the store's cache holds it, or the range covers much of it, and otherwise the few
    /// segments that can hold them. A first read of a few rows then decodes a segment of
    /// the block instead of all of it.
    fn rows(
        &mut self,
        snap: &Snapshot,
        idx: &PermIndex,
        b: usize,
        lo: &Key,
        hi: &Key,
    ) -> anyhow::Result<Rows<'_>> {
        if self.slot(idx.uid, b, WHOLE).is_none()
            && let Some((s0, s1)) = idx.read_span(&snap.cache, b, lo, hi, ALL_COLS)
        {
            return Ok(Rows::Segments(s0, s1));
        }
        self.block(snap, idx, b).map(Rows::Whole)
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
    /// the remembered lookups of a kept closure
    lookups: Option<Lookups>,
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

/// With one input graph, the place of `t` in the scan of the lookup with the bound
/// positions of `k`: its components in the order of the permutation the lookup reads (see
/// [`Base::access`]), which sorts as the scan visits them.
pub(crate) fn rank(k: &lookups::Pattern, t: &Triple) -> Triple {
    let perm = match (k[0] != 0, k[1] != 0, k[2] != 0) {
        (true, _, _) => Perm::Gspo,
        (false, true, true) => Perm::Pos,
        (false, true, false) => Perm::Pso,
        (false, false, true) => Perm::Osp,
        (false, false, false) => Perm::Gspo,
    };
    let mut r = [0; 3];
    let mut i = 0;
    for c in perm.order() {
        if c < 3 {
            r[i] = t[c];
            i += 1;
        }
    }
    r
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
            lookups: None,
        })
    }

    /// This tier, answering its lookups with few facts from `lookups` and keeping their
    /// answers there.
    pub fn with_lookups(mut self, lookups: Lookups) -> Base {
        self.lookups = Some(lookups);
        self
    }

    /// The remembered lookups, to change between runs.
    pub fn lookups_mut(&mut self) -> Option<&mut Lookups> {
        self.lookups.as_mut()
    }

    /// Give the tier remembered lookups.
    pub fn set_lookups(&mut self, lookups: Lookups) {
        self.lookups = Some(lookups);
    }

    /// The remembered lookups, taken out of the tier.
    pub fn take_lookups(&mut self) -> Option<Lookups> {
        self.lookups.take()
    }

    /// Whether the tier reads one input graph, whose scans visit facts in key order.
    pub fn one_graph(&self) -> bool {
        self.single().is_some()
    }

    /// The remembered lookups, when the tier has them.
    pub fn lookups(&self) -> Option<&Lookups> {
        self.lookups.as_ref()
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
        let (perm, prefix, n) = self.prefix(s, p, o);
        (perm, pad(&prefix[..n], 0), pad(&prefix[..n], u64::MAX))
    }

    /// The store's quads in the key range of a pattern: an upper bound of the tier's
    /// facts that match it, as [`count_p`](Self::count_p) is of a predicate's, since a
    /// pattern without a subject counts the quads of every graph and a pattern with a
    /// subject and an object but no predicate counts the subject's. [`UNCOUNTED`] when
    /// the index cannot be read.
    pub fn count(&self, s: Option<u64>, p: Option<u64>, o: Option<u64>) -> u64 {
        let (perm, prefix, n) = self.prefix(s, p, o);
        if let (1, Some(p)) = (n, p) {
            return self.count_p(p);
        }
        self.snap.count(perm, &prefix[..n]).unwrap_or(UNCOUNTED)
    }

    /// The permutation and the key prefix of a pattern.
    fn prefix(&self, s: Option<u64>, p: Option<u64>, o: Option<u64>) -> (Perm, [u64; 4], usize) {
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
        (perm, prefix, n)
    }

    /// Visit the facts matching a pattern, in the order of the module documentation.
    /// `f` returns false to stop; the result is false when it did. The scans count in
    /// [`scans`](Self::scans).
    pub fn for_each(
        &self,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        let mut memo = Memo::default();
        let r = self.for_each_with(&mut memo, s, p, o, f);
        self.add_scans(memo.scans);
        r
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
        let mut f = f;
        if let Some(l) = &self.lookups
            && let Some(k) = Lookups::pattern(s, p, o)
        {
            // the kept first facts, then the blocks past them
            let mut buf = [[0u64; 3]; lookups::COPY_FACTS];
            let (kept, complete): (&[Triple], bool) = match l.read(&k, &mut buf) {
                Some(Read::Frozen(a)) => (&a.facts, a.complete),
                Some(Read::Copied(n, complete)) => (&buf[..n], complete),
                None => (&[], false),
            };
            // a read the kept facts answer counts as the scans it saves, so that the
            // count does not depend on what is remembered: exactly with one input graph,
            // and as one scan when the reader stops early with several
            for t in kept {
                if !f(*t) {
                    memo.scans += 1;
                    return Ok(false);
                }
            }
            if complete {
                memo.scans += self.passes() as u64;
                return Ok(true);
            }
            let skip = kept.len();
            // the answer's first facts, while they can be kept
            let keep = skip < lookups::MAX_FACTS;
            let mut few: smallvec::SmallVec<[Triple; 16]> = smallvec::SmallVec::new();
            if keep {
                few.extend_from_slice(kept);
            }
            let mut seen = 0;
            let mut many = !keep;
            let done = self.scan_all(memo, s, p, o, |t| {
                seen += 1;
                if seen <= skip {
                    return true;
                }
                if !many {
                    if few.len() < lookups::MAX_FACTS {
                        few.push(t);
                    } else {
                        many = true;
                    }
                }
                f(t)
            })?;
            if keep {
                l.put(k, &few, done && !many);
            }
            return Ok(done);
        }
        self.scan_all(memo, s, p, o, f)
    }

    /// Every pass of a pattern's scan, in order.
    fn scan_all(
        &self,
        memo: &mut Memo,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        mut f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        let (perm, lo, hi) = self.access(s, p, o);
        for pass in 0..self.passes() {
            if !self.scan(memo, perm, lo, hi, [s, p, o], Some(pass), None, &mut f)? {
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
        if let Some(l) = &self.lookups
            && let Some(k) = Lookups::pattern(s, p, o)
        {
            if let Some((any, complete)) = l.peek(&k)
                && (complete || any)
            {
                // counted as the scan it saves
                memo.scans += 1;
                return Ok(any);
            }
            // the first fact is enough
            let mut few: Vec<Triple> = Vec::new();
            let done = self.scan_all(memo, s, p, o, |t| {
                few.push(t);
                false
            })?;
            l.put(k, &few, done);
            return Ok(!few.is_empty());
        }
        let (perm, lo, hi) = self.access(s, p, o);
        Ok(!self.scan(memo, perm, lo, hi, [s, p, o], None, None, |_| false)?)
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
        self.scan(memo, r.perm, r.lo, r.hi, [s, p, o], Some(r.pass), None, f)
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
        allow: Option<u64>,
        mut f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        memo.scans += 1;
        // the rows entered so far, and whether the allowance ended the scan
        let allow = allow.unwrap_or(u64::MAX);
        let (mut entered, mut cut) = (0u64, false);
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
                if entered >= allow {
                    cut = true;
                    return Ok(false);
                }
                Ok(match c {
                    Chunk::Block(b, s, e) => {
                        entered += (e - s) as u64;
                        let cols = &b.cols;
                        (s..e)
                            .all(|i| row([cols[0][i], cols[1][i], cols[2][i], cols[3][i]], &mut f))
                    }
                    Chunk::Row(k) => {
                        entered += 1;
                        row(k, &mut f)
                    }
                })
            })?;
        } else {
            let idx = snap.perm(perm);
            let (b0, b1) = idx.key_block_range(&lo, &hi);
            let mut rows_of =
                |blk: &Block, whole: bool, entered: &mut u64, f: &mut dyn FnMut(Triple) -> bool| {
                    let (s, e) = if whole {
                        (0, blk.len())
                    } else {
                        blk.key_range(&lo, &hi)
                    };
                    *entered += (e - s) as u64;
                    let cols = &blk.cols;
                    (s..e).all(|i| row([cols[0][i], cols[1][i], cols[2][i], cols[3][i]], f))
                };
            'blocks: for b in b0..b1 {
                if entered >= allow {
                    cut = true;
                    break;
                }
                let m = &idx.blocks[b];
                let whole = m.first >= lo && m.last <= hi;
                if whole {
                    if !rows_of(memo.block(snap, idx, b)?, true, &mut entered, &mut f) {
                        break;
                    }
                    continue;
                }
                match memo.rows(snap, idx, b, &lo, &hi)? {
                    Rows::Whole(blk) => {
                        if !rows_of(blk, false, &mut entered, &mut f) {
                            break;
                        }
                    }
                    Rows::Segments(s0, s1) => {
                        for s in s0..s1 {
                            if entered >= allow {
                                cut = true;
                                break 'blocks;
                            }
                            let blk = memo.segment(snap, idx, b, s)?;
                            if !rows_of(blk, false, &mut entered, &mut f) {
                                break 'blocks;
                            }
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        {
            memo.rows += entered;
        }
        Ok(!cut && completed && last(cur, &mut f))
    }

    /// The number of blocks the scan of a pattern reads, without reading any.
    pub fn blocks(&self, s: Option<u64>, p: Option<u64>, o: Option<u64>) -> usize {
        let (perm, lo, hi) = self.access(s, p, o);
        let (b0, b1) = self.snap.perm(perm).key_block_range(&lo, &hi);
        b1 - b0
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
            if within
                && m.rows as usize > rows
                && let Some(n) = idx.segment_count(b)
            {
                // a segmented block is cut at the starts of its segments, without a read
                let mut last = 0;
                for sg in 1..n {
                    let (r0, _) = idx.segment_rows(b, sg);
                    let k = idx.segment_first(b, sg);
                    if k > hi {
                        break;
                    }
                    let k = triple_start(k);
                    if k <= lo {
                        last = r0;
                    } else if r0 - last >= rows {
                        cuts.push(k);
                        last = r0;
                    }
                }
            } else if within && m.rows as usize > rows {
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

    /// Offers `f` facts that match a pattern until it has taken `n`, for a sample of the
    /// pattern's facts: runs of facts from the starts of up to `starts` blocks of the
    /// pattern's scan at evenly spaced positions, so that the facts of a long scan come
    /// from along its length without a read of the blocks between them. `f` returns
    /// whether it took the fact. A start reads no further than the end of its own block,
    /// and it stops at the block, the segment or the row of the snapshot's delta that
    /// takes it past [`SAMPLE_ROWS`] rows, whatever share of the rows belongs to other
    /// graphs or fails the pattern. A start also gives up after a few facts that `f`
    /// does not take. Returns whether the sample read every fact of the pattern, which
    /// one start does when its scan runs to the end of the pattern's range.
    pub fn sample(
        &self,
        memo: &mut Memo,
        [s, p, o]: [Option<u64>; 3],
        n: usize,
        starts: usize,
        mut f: impl FnMut(Triple) -> bool,
    ) -> anyhow::Result<bool> {
        if n == 0 {
            return Ok(false);
        }
        let (perm, lo, hi) = self.access(s, p, o);
        let idx = self.snap.perm(perm);
        let (b0, b1) = idx.key_block_range(&lo, &hi);
        let starts = (b1 - b0).clamp(1, starts.clamp(1, n));
        let mut taken = 0usize;
        let mut whole = false;
        for k in 0..starts {
            let b = b0 + k * (b1 - b0) / starts;
            // the first key of the block. With several input graphs it can lie among the
            // rows of one triple, which is then offered if a graph from there on holds it.
            let from = if k == 0 { lo } else { idx.blocks[b].first };
            // the last key of the block, or of the scan in its last block
            let to = if b + 1 < b1 {
                idx.blocks[b].last.min(hi)
            } else {
                hi
            };
            // what is left, over the starts that are left
            let quota = (n - taken).div_ceil(starts - k);
            let (mut got, mut offered) = (0usize, 0usize);
            let rows = Some(SAMPLE_ROWS);
            let ended = self.scan(memo, perm, from.max(lo), to, [s, p, o], None, rows, |t| {
                offered += 1;
                got += usize::from(f(t));
                got < quota && offered < 4 * quota + 8
            })?;
            whole = ended && k == 0 && to == hi;
            taken += got;
            if taken >= n {
                break;
            }
        }
        Ok(whole)
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
        let Some(l) = &self.lookups else {
            return self.probe(memo, t);
        };
        let k = *t;
        if let Some((any, _)) = l.peek(&k) {
            return Ok(any);
        }
        let found = self.probe(memo, t)?;
        l.put(k, if found { std::slice::from_ref(t) } else { &[] }, true);
        Ok(found)
    }

    /// Whether an input graph holds `t`, read from the blocks.
    fn probe(&self, memo: &mut Memo, t: &Triple) -> anyhow::Result<bool> {
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
                    let hit = |blk: &Block| {
                        let (s, e) = blk.prefix_range(&key);
                        s < e
                    };
                    match memo.rows(snap, idx, b, &key, &key)? {
                        Rows::Whole(blk) => hit(blk),
                        Rows::Segments(s0, s1) => {
                            let mut found = false;
                            for s in s0..s1 {
                                if hit(memo.segment(snap, idx, b, s)?) {
                                    found = true;
                                    break;
                                }
                            }
                            found
                        }
                    }
                }
            };
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// [`contains`](Self::contains) for triples in sorted order. Runs of at least
    /// [`SORTED_RUN`] triples are searched in parallel, each with one pass per input graph
    /// over the blocks its triples fall in, searching forward from the previous triple's
    /// row. A block that neither the pass nor the store's cache holds is read by the
    /// segment that can hold the triple, so a few probes in a block decode a few segments
    /// of it instead of all of it.
    pub fn contains_sorted(&self, ts: &[Triple]) -> anyhow::Result<Vec<bool>> {
        use rayon::prelude::*;
        let mut found = vec![false; ts.len()];
        if ts.len() < 2 * SORTED_RUN {
            self.contains_run(ts, &mut found)?;
            return Ok(found);
        }
        let run = ts
            .len()
            .div_ceil(rayon::current_num_threads().max(1) * 4)
            .max(SORTED_RUN);
        ts.par_chunks(run)
            .zip(found.par_chunks_mut(run))
            .try_for_each(|(t, f)| self.contains_run(t, f))?;
        Ok(found)
    }

    /// One run of [`contains_sorted`](Self::contains_sorted), its answers set in `found`.
    fn contains_run(&self, ts: &[Triple], found: &mut [bool]) -> anyhow::Result<()> {
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
            // the block and row the last search of a whole block ended at
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
                let blk = match memo.rows(snap, idx, b, &k, &k)? {
                    Rows::Whole(blk) => blk,
                    Rows::Segments(s0, s1) => {
                        for s in s0..s1 {
                            let (a, z) = memo.segment(snap, idx, b, s)?.prefix_range(&k);
                            if a < z {
                                found[i] = true;
                                break;
                            }
                        }
                        at = Some((b, 0));
                        continue;
                    }
                };
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
        Ok(())
    }

    /// What a probe of `t` needs: none when `t` is certainly not a fact of the tier, since
    /// a term is not in the store or no quad has its predicate, and a check of its
    /// predicate and object with [`po_any`](Self::po_any) first when its predicate has few
    /// distinct objects.
    pub fn classify(&self, t: &Triple, f: &mut ProbeFilter) -> Probe {
        if t.iter().any(|&x| Id(x).tag() == Tag::Local) {
            return Probe::No;
        }
        let (any, few_objects) =
            *f.p.entry(t[1])
                .or_insert_with(|| (self.count_p(t[1]) > 0, self.pstats(t[1]).1 <= FEW_OBJECTS));
        match (any, few_objects) {
            (false, _) => Probe::No,
            (true, false) => Probe::Maybe,
            (true, true) => Probe::Pair,
        }
    }

    /// Whether the store has quads with predicate `p` and object `o`.
    pub fn po_any(&self, p: u64, o: u64) -> bool {
        self.snap.count(Perm::Pos, &[p, o]).map_or(true, |n| n > 0)
    }

    /// Quads with predicate `p` in the store: an upper bound of the tier's facts.
    /// [`UNCOUNTED`] when the index cannot be read.
    pub fn count_p(&self, p: u64) -> u64 {
        if let Some(n) = self.counts.read().get(&p) {
            return *n;
        }
        // the generation's statistics give the count without decoding the blocks at the
        // ends of the predicate's range
        let n = match self.snap.predicate_count(Perm::Pso, &[p]) {
            Some(n) => n,
            None => self.snap.count(Perm::Pso, &[p]).unwrap_or(UNCOUNTED),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};

    /// A pattern's count is the number of its facts with one input graph, and a sample
    /// of a scan that spans several blocks takes matching facts from along it.
    #[test]
    fn counts_and_samples_of_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        let mut nt = String::new();
        for i in 0..600_000u64 {
            nt.push_str(&format!(
                "<urn:s{}> <urn:p{}> <urn:o{}> .\n",
                i / 3,
                i % 3,
                i % 500
            ));
        }
        store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let id = |iri: &str| snap.lookup_iri(iri).unwrap().0;
        let (s, p, o) = (id("urn:s7"), id("urn:p1"), id("urn:o22"));
        let base = Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
        let facts = |s, p, o| {
            let mut out = Vec::new();
            base.for_each(s, p, o, |t| {
                out.push(t);
                true
            })
            .unwrap();
            out
        };
        for pattern in [
            (Some(s), Some(p), None),
            (None, Some(p), Some(o)),
            (None, Some(p), None),
            (Some(s), None, None),
            (None, None, None),
            (Some(s), Some(p), Some(o)),
        ] {
            let (s, p, o) = pattern;
            assert_eq!(
                base.count(s, p, o),
                facts(s, p, o).len() as u64,
                "{pattern:?}"
            );
        }
        // a relation of several blocks: eight facts from four places, not its first eight
        let relation = facts(None, Some(p), None);
        assert!(
            base.blocks(None, Some(p), None) >= 4,
            "{}",
            base.blocks(None, Some(p), None)
        );
        let mut memo = Memo::default();
        let mut got = Vec::new();
        let whole = base
            .sample(&mut memo, [None, Some(p), None], 8, 4, |t| {
                got.push(t);
                true
            })
            .unwrap();
        assert!(!whole);
        assert_eq!(got.len(), 8);
        assert!(got.iter().all(|t| t[1] == p && relation.contains(t)));
        let mut distinct = got.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), 8);
        assert_ne!(got, relation[..8]);
        assert_eq!(got[..2], relation[..2]);
        // a probe within one block: its facts, and no more than asked for
        let mut few = Vec::new();
        let whole = base
            .sample(&mut memo, [Some(s), None, None], 2, 4, |t| {
                few.push(t);
                true
            })
            .unwrap();
        assert_eq!(few, facts(Some(s), None, None)[..2]);
        // it stopped at the two it was asked for, and a sample that reads to the end of
        // a scan within one block says that it read every fact
        assert!(!whole);
        let mut all = Vec::new();
        let whole = base
            .sample(&mut memo, [Some(s), None, None], 100, 4, |t| {
                all.push(t);
                true
            })
            .unwrap();
        assert!(whole);
        assert_eq!(all, facts(Some(s), None, None));
        // facts the caller does not take do not count, and a start gives up after a few
        let mut offered = 0;
        base.sample(&mut memo, [None, Some(p), None], 8, 4, |_| {
            offered += 1;
            false
        })
        .unwrap();
        assert!((1..=4 * (4 * 8 + 8)).contains(&offered), "{offered}");
    }

    /// A sample reads the block of each start and no further, whatever share of the
    /// rows belongs to graphs that are not read. Two input graphs hold a few facts of a
    /// predicate and a third graph, which is not an input, holds 400,000.
    #[test]
    fn a_sample_reads_one_block_per_start() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        let mut nq = String::new();
        for i in 0..400_000u64 {
            nq.push_str(&format!(
                "<urn:s{i}> <urn:p> <urn:o{}> <urn:other> .\n",
                i % 7
            ));
        }
        for i in 0..6u64 {
            nq.push_str(&format!(
                "<urn:s{}> <urn:p> <urn:x> <urn:g{}> .\n",
                i * 60_000,
                i % 2
            ));
        }
        store
            .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();
        let snap = store.snapshot();
        let id = |iri: &str| snap.lookup_iri(iri).unwrap();
        let p = id("urn:p").0;
        let base = Base::new(snap.clone(), &[id("urn:g0"), id("urn:g1")]).unwrap();
        let blocks = base.blocks(None, Some(p), None);
        assert!(blocks >= 4, "{blocks}");
        let mut every = Vec::new();
        base.for_each(None, Some(p), None, |t| {
            every.push(t);
            true
        })
        .unwrap();
        assert_eq!(every.len(), 6);
        for starts in [1, 2] {
            let mut memo = Memo::default();
            let mut got = Vec::new();
            let whole = base
                .sample(&mut memo, [None, Some(p), None], 8, starts, |t| {
                    got.push(t);
                    true
                })
                .unwrap();
            assert!(!whole);
            assert!(got.iter().all(|t| every.contains(t)), "{got:?}");
            // each start stops with the block that takes it past its allowance, among
            // the predicate's blocks, which hold the 400,006 rows of every graph
            let most = starts as u64 * (SAMPLE_ROWS + sparkles_core::index::BLOCK_ROWS as u64);
            assert!(memo.rows > 0 && memo.rows <= most, "{}", memo.rows);
        }
    }

    /// The rows of the snapshot's delta are read within the allowance too: 60,000 quads
    /// of a graph that is not read, inserted in one commit and held in no block.
    #[test]
    fn a_sample_of_a_delta_stops_at_its_allowance() {
        let store = Store::in_memory(StoreOptions::default());
        let iri = |s: String| oxrdf::NamedNode::new_unchecked(s);
        let quad = |i: u64, g: &str| {
            oxrdf::Quad::new(
                iri(format!("urn:s{i}")),
                iri("urn:p".into()),
                iri(format!("urn:o{}", i % 7)),
                oxrdf::GraphName::NamedNode(iri(g.into())),
            )
        };
        let mut txn = store.write();
        let mut labels = Default::default();
        for i in 0..60_000u64 {
            let q = txn.encode_quad(&quad(i, "urn:other"), &mut labels).unwrap();
            txn.insert(q).unwrap();
        }
        for i in 0..6u64 {
            let g = format!("urn:g{}", i % 2);
            let q = txn.encode_quad(&quad(i * 9_000, &g), &mut labels).unwrap();
            txn.insert(q).unwrap();
        }
        txn.commit().unwrap();
        let snap = store.snapshot();
        let id = |iri: &str| snap.lookup_iri(iri).unwrap();
        let p = id("urn:p").0;
        let base = Base::new(snap.clone(), &[id("urn:g0"), id("urn:g1")]).unwrap();
        assert_eq!(
            base.blocks(None, Some(p), None),
            0,
            "the quads are in a block"
        );
        let mut memo = Memo::default();
        let mut got = Vec::new();
        let whole = base
            .sample(&mut memo, [None, Some(p), None], 8, 4, |t| {
                got.push(t);
                true
            })
            .unwrap();
        assert!(!whole);
        assert!(got.len() < 6, "{got:?}");
        assert!(
            memo.rows >= SAMPLE_ROWS && memo.rows <= SAMPLE_ROWS + 1,
            "{}",
            memo.rows
        );
    }

    #[test]
    fn segment_reads_match_whole_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u64| {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r % n
        };
        let mut nt = String::new();
        for _ in 0..90_000 {
            let (s, p, o) = (next(12_000), next(6), next(4_000));
            nt.push_str(&format!("<urn:s{s}> <urn:p{p}> <urn:o{o}> .\n"));
        }
        store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let gspo = snap.perm(Perm::Gspo);
        assert!(gspo.segmented() && gspo.blocks.len() > 1);
        let g = Id::DEFAULT_GRAPH;
        let mut all: Vec<Triple> = Vec::new();
        snap.scan(Perm::Gspo, &[g.0], |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    all.extend((s..e).map(|i| [b.cols[1][i], b.cols[2][i], b.cols[3][i]]))
                }
                Chunk::Row(k) => all.push([k[1], k[2], k[3]]),
            }
            Ok(true)
        })
        .unwrap();
        // a store opened afresh has an empty block cache and no block read whole yet:
        // point and sorted probes of a few facts there decode segments and no whole
        // column, and answer as the scan does
        drop(snap);
        drop(store);
        let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        let snap = store.snapshot();
        let gspo = snap.perm(Perm::Gspo);
        {
            let base = Base::new(snap.clone(), &[g]).unwrap();
            let have: rustc_hash::FxHashSet<Triple> = all.iter().copied().collect();
            let mut probes: Vec<Triple> = Vec::new();
            for _ in 0..3 {
                let t = all[next(all.len() as u64) as usize];
                let other = [t[0], t[1], all[next(all.len() as u64) as usize][2]];
                assert!(base.contains(&t).unwrap());
                assert_eq!(base.contains(&other).unwrap(), have.contains(&other));
                probes.extend([t, other]);
            }
            probes.sort_unstable();
            probes.dedup();
            let want: Vec<bool> = probes.iter().map(|t| have.contains(t)).collect();
            assert_eq!(base.contains_sorted(&probes).unwrap(), want);
            assert!(snap.cache.misses() > 0, "the probes decoded nothing");
            for b in 0..gspo.blocks.len() {
                for c in 0..4 {
                    assert!(
                        !snap.cache.has_cols(gspo, b, 1 << c),
                        "the probes decoded column {c} of block {b} whole"
                    );
                }
            }
        }
        let budget = Budget::unlimited();
        for round in 0..2 {
            for i in 0..100 {
                let t = all[next(all.len() as u64) as usize];
                // a tier of its own reads the blocks afresh, one with lookups keeps answers
                let mut base = Base::new(snap.clone(), &[g]).unwrap();
                if round == 1 {
                    base = base.with_lookups(Lookups::new(&budget, lookups::DEFAULT_MAX_BYTES));
                }
                for mask in 1u8..8 {
                    let at = |c: usize| (mask & (1 << c) != 0).then_some(t[c]);
                    let k = [at(0), at(1), at(2)];
                    let pat = [k[0].unwrap_or(0), k[1].unwrap_or(0), k[2].unwrap_or(0)];
                    let mut want: Vec<Triple> = all
                        .iter()
                        .filter(|x| (0..3).all(|c| k[c].is_none_or(|v| v == x[c])))
                        .copied()
                        .collect();
                    want.sort_by_key(|x| rank(&pat, x));
                    for _ in 0..2 {
                        let mut got = Vec::new();
                        base.for_each(k[0], k[1], k[2], |x| {
                            got.push(x);
                            true
                        })
                        .unwrap();
                        assert_eq!(got, want, "round {round} lookup {i} {pat:?}");
                        assert!(base.any(&mut Memo::default(), k[0], k[1], k[2]).unwrap());
                    }
                    // the shares of the scan, cut at segment starts, list the same facts
                    for rows in [1, 100, 4096] {
                        let mut got = Vec::new();
                        let mut memo = Memo::default();
                        for r in base.ranges(k[0], k[1], k[2], rows).unwrap() {
                            base.for_range(&mut memo, &r, k[0], k[1], k[2], |x| {
                                got.push(x);
                                true
                            })
                            .unwrap();
                        }
                        assert_eq!(got, want, "round {round} shares {i} {pat:?} {rows}");
                    }
                }
                assert!(base.contains(&t).unwrap());
                let other = [t[0], t[1], all[next(all.len() as u64) as usize][2]];
                assert_eq!(base.contains(&other).unwrap(), all.contains(&other));
            }
            // sorted probes, in runs searched in parallel, half of them facts
            let mut probes: Vec<Triple> = (0..3_000)
                .map(|j| {
                    let t = all[next(all.len() as u64) as usize];
                    if j % 2 == 0 {
                        t
                    } else {
                        [t[0], t[1], next(4_000)]
                    }
                })
                .collect();
            probes.sort_unstable();
            probes.dedup();
            let base = Base::new(snap.clone(), &[g]).unwrap();
            let have: rustc_hash::FxHashSet<Triple> = all.iter().copied().collect();
            let want: Vec<bool> = probes.iter().map(|t| have.contains(t)).collect();
            for n in [1, 10, probes.len()] {
                assert_eq!(base.contains_sorted(&probes[..n]).unwrap(), want[..n]);
            }
        }
    }
}
