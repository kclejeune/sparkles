//! The facts the engine reads: an optional base tier (the input graphs, read from the
//! store's index permutations, see [`crate::facts`]) and an in-memory, append-only tier of
//! id-level triples with hash indexes.
//!
//! Rows are numbered from 0. The base tier's facts are the rows `[0, offset)`, which are
//! not addressable one by one: an access reads the whole base tier or none of it. The
//! in-memory rows follow from `offset` on. Semi-naive evaluation restricts any access path
//! to a row range (`[lo, hi)`), so every index list holds rows in ascending order.
//!
//! The indexes are split into shards by a hash of their key, so that a merge of new facts
//! deduplicates and fills them with one task per shard ([`Graph::merge`]). The merge gives
//! rows in the order of its input, whatever the number of threads.
//!
//! Incremental maintenance removes triples by marking their rows dead: a dead row stays
//! in the index lists (evaluation skips it) but leaves the triple index, so the triple
//! can be added again as a new row.
//!
//! The graph's heap is charged to the run's [`Budget`] by capacity
//! ([`Graph::heap_bytes`]). A buffer or table about to grow is charged its new
//! allocation first, with the old one, and the merge charges its working buffers before
//! it allocates them ([`crate::budget`]).

use crate::budget::{
    Budget, Held, MemoryExceeded, Meter, table_bytes, table_capacity, table_grown,
};
use crate::facts::{Base, Memo, ProbeFilter};
use hashbrown::HashTable;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::hash::BuildHasher;
use std::mem::size_of;
use std::sync::Arc;

pub(crate) type Triple = [u64; 3];

const SHARDS: usize = 64;
/// Batches smaller than this are merged on the calling thread.
const PAR_MIN: usize = 1 << 14;
/// Facts a merge found in the base tier that one shard of [`Graph::known`]'s cache
/// remembers. A full shard forgets them all, so the cache has a fixed size whatever
/// the input: a fact it forgot is emitted again and found in the base tier by the next
/// merge's probe.
pub(crate) const IN_BASE_SHARD: usize = 128;

type List = SmallVec<[u32; 4]>;

/// Rows by key; most keys of the per-subject and per-object indexes have a few rows,
/// which fit without an allocation of their own.
type Lists<K> = FxHashMap<K, List>;

/// The heap of a list beyond the rows it keeps inline.
#[inline]
fn spill(cap: usize) -> usize {
    if cap > 4 { cap * size_of::<u32>() } else { 0 }
}

fn lists_bytes<K>(m: &Lists<K>) -> usize {
    table_bytes(m.capacity(), size_of::<(K, List)>())
}

/// Before a map with `len` keys and capacity `cap` takes one more key, the bytes of the
/// table it would grow to (the old one is still allocated then).
#[inline]
fn table_growth<K>(len: usize, cap: usize) -> usize {
    if len < cap {
        0
    } else {
        table_bytes(table_grown(cap, len, 1), size_of::<(K, List)>())
    }
}

/// Push `r` onto list `e`. A list about to grow charges its new allocation to `m` first
/// (the old one stays charged to the graph until it settles), and `spilled` takes the
/// change of the lists' heap.
#[inline]
fn push_row(
    e: &mut List,
    r: u32,
    m: &mut Meter<'_>,
    spilled: &mut isize,
) -> Result<(), MemoryExceeded> {
    let cap = e.capacity();
    if e.len() == cap {
        m.add(spill((cap * 2).max(8)))?;
        e.push(r);
        *spilled += spill(e.capacity()) as isize - spill(cap) as isize;
    } else {
        e.push(r);
    }
    Ok(())
}

/// Append `rows` to list `e` with one copy, charging its growth first as
/// [`push_row`] does (a list grows to the next power of two that holds them all).
#[inline]
fn extend_rows(
    e: &mut List,
    rows: &[u32],
    m: &mut Meter<'_>,
    spilled: &mut isize,
) -> Result<(), MemoryExceeded> {
    let cap = e.capacity();
    let want = e.len() + rows.len();
    if want > cap {
        m.add(spill(want.next_power_of_two()))?;
        e.extend_from_slice(rows);
        *spilled += spill(e.capacity()) as isize - spill(cap) as isize;
    } else {
        e.extend_from_slice(rows);
    }
    Ok(())
}

/// The entry of key `k` in `m`, charging to `meter` first the table it grows to when
/// `k` is new and does not fit.
#[inline]
fn entry<'m, K: std::hash::Hash + Eq>(
    m: &'m mut Lists<K>,
    k: K,
    meter: &mut Meter<'_>,
) -> Result<&'m mut List, MemoryExceeded> {
    if m.len() == m.capacity() && !m.contains_key(&k) {
        meter.add(table_growth::<K>(m.len(), m.capacity()))?;
    }
    Ok(m.entry(k).or_default())
}

#[inline]
fn hash3(t: &Triple) -> u64 {
    rustc_hash::FxBuildHasher.hash_one(t)
}

#[inline]
fn mix(x: u64) -> usize {
    // a multiplier other than the one of the maps' hasher, so that the bits that pick a
    // shard are not the bits a shard's map sorts by
    (x.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize
}

#[inline]
fn shard3(t: &Triple) -> usize {
    mix(t[0] ^ t[1].rotate_left(21) ^ t[2].rotate_left(42))
}

#[inline]
fn shard2(a: u64, b: u64) -> usize {
    mix(a ^ b.rotate_left(29))
}

#[inline]
fn shard1(a: u64) -> usize {
    mix(a)
}

fn shards<T: Default>() -> Vec<T> {
    (0..SHARDS).map(|_| T::default()).collect()
}

pub(crate) struct Graph {
    /// rows before this one are the base tier's
    offset: u32,
    base: Option<Arc<Base>>,
    /// the in-memory rows, from `offset` on
    triples: Vec<Triple>,
    /// the rows of the live triples, hashed by triple, by [`shard3`]
    index: Vec<HashTable<u32>>,
    /// facts a merge found in the base tier, by [`shard3`], so that evaluation does not
    /// emit them again: at most [`IN_BASE_SHARD`] per shard
    pub(crate) in_base: Vec<FxHashSet<Triple>>,
    live: usize,
    /// by [`shard1`] of the predicate
    by_p: Vec<Lists<u64>>,
    /// by [`shard2`] of (predicate, subject) and (predicate, object)
    by_ps: Vec<Lists<(u64, u64)>>,
    by_po: Vec<Lists<(u64, u64)>>,
    /// per predicate: distinct subjects / distinct objects of the in-memory rows
    distinct_s: FxHashMap<u64, u32>,
    distinct_o: FxHashMap<u64, u32>,
    /// by [`shard1`] of the subject / object
    by_s: Option<Vec<Lists<u64>>>,
    by_o: Option<Vec<Lists<u64>>>,
    /// one bit per dead in-memory row (empty while no row is dead)
    dead: Vec<u64>,
    dead_rows: u32,
    /// the heap of the index lists beyond their inline rows
    spilled: usize,
    /// the graph's heap, charged to the run's budget
    held: Held,
}

impl Default for Graph {
    fn default() -> Graph {
        Graph {
            spilled: 0,
            held: Held::new(&Budget::unlimited()),
            offset: 0,
            base: None,
            triples: Vec::new(),
            index: shards(),
            in_base: shards(),
            live: 0,
            by_p: shards(),
            by_ps: shards(),
            by_po: shards(),
            distinct_s: FxHashMap::default(),
            distinct_o: FxHashMap::default(),
            by_s: None,
            by_o: None,
            dead: Vec::new(),
            dead_rows: 0,
        }
    }
}

/// Candidate in-memory rows for one access.
#[derive(Clone, Copy)]
pub(crate) enum Cands<'a> {
    Slice(&'a [u32]),
    Range(u32, u32),
}

impl<'a> Cands<'a> {
    pub const EMPTY: Cands<'static> = Cands::Slice(&[]);

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Cands::Slice(s) => s.len(),
            Cands::Range(a, b) => (b - a) as usize,
        }
    }
    #[inline]
    pub fn get(&self, i: usize) -> u32 {
        match self {
            Cands::Slice(s) => s[i],
            Cands::Range(a, _) => a + i as u32,
        }
    }
    pub fn chunks(self, n: usize) -> Vec<Cands<'a>> {
        let n = n.max(1);
        match self {
            Cands::Slice(s) => s.chunks(n).map(Cands::Slice).collect(),
            Cands::Range(a, b) => (a..b)
                .step_by(n)
                .map(|x| Cands::Range(x, (x + n as u32).min(b)))
                .collect(),
        }
    }
}

#[inline]
fn restrict(list: &[u32], lo: u32, hi: u32) -> &[u32] {
    let a = if lo == 0 {
        0
    } else {
        list.partition_point(|&x| x < lo)
    };
    let b = list.partition_point(|&x| x < hi);
    &list[a..b.max(a)]
}

/// The new rows of one index shard, from the pieces of a merge.
enum Fill<'a> {
    P(&'a mut Lists<u64>, usize),
    Ps(&'a mut Lists<(u64, u64)>, usize),
    Po(&'a mut Lists<(u64, u64)>, usize),
    S(&'a mut Lists<u64>, usize),
    O(&'a mut Lists<u64>, usize),
}

/// Rows of a piece of new rows, by index kind and shard.
struct Piece {
    p: Vec<Vec<u32>>,
    ps: Vec<Vec<u32>>,
    po: Vec<Vec<u32>>,
    s: Vec<Vec<u32>>,
    o: Vec<Vec<u32>>,
}

impl Graph {
    pub fn with_capacity(n: usize) -> Graph {
        let mut g = Graph::default();
        g.triples.reserve(n);
        for m in &mut g.index {
            *m = HashTable::with_capacity(n / SHARDS);
        }
        g.settle();
        g
    }

    /// [`with_capacity`](Self::with_capacity), charged to `budget` before it allocates.
    pub fn try_with_capacity(n: usize, budget: &Arc<Budget>) -> Result<Graph, MemoryExceeded> {
        let bytes = n * size_of::<Triple>() + SHARDS * table_bytes(table_capacity(n / SHARDS), 4);
        let mut held = Held::try_new(budget, bytes)?;
        let mut g = Graph::with_capacity(n);
        held.set(g.heap_bytes());
        g.held = held;
        Ok(g)
    }

    /// Charge the graph's heap to `budget` from now on, or fail when it does not fit.
    pub fn set_budget(&mut self, budget: &Arc<Budget>) -> Result<(), MemoryExceeded> {
        self.held = Held::try_new(budget, self.heap_bytes())?;
        Ok(())
    }

    /// [`set_budget`](Self::set_budget), charging even past the limit.
    pub fn charge_to(&mut self, budget: &Arc<Budget>) {
        let mut h = Held::new(budget);
        h.grow(self.heap_bytes());
        self.held = h;
    }

    /// The budget the graph's heap is charged to.
    pub fn budget(&self) -> &Arc<Budget> {
        self.held.budget()
    }

    /// Bring the charge of the graph's heap in line with [`heap_bytes`](Self::heap_bytes),
    /// releasing what freed allocations held (and charging, past the limit if need be,
    /// what grew beyond the estimates).
    pub fn settle(&mut self) {
        let n = self.heap_bytes();
        self.held.set(n);
    }

    /// The heap of the in-memory tier, from the capacities of its buffers and tables:
    /// the rows, the triple index, the cache of base facts, the index lists and their
    /// heap beyond the inline rows, the per-predicate counts and the dead-row bits.
    pub fn heap_bytes(&self) -> usize {
        let mut n = self.triples.capacity() * size_of::<Triple>();
        n += self
            .index
            .iter()
            .map(|t| table_bytes(t.capacity(), size_of::<u32>()))
            .sum::<usize>();
        n += self
            .in_base
            .iter()
            .map(|s| table_bytes(s.capacity(), size_of::<Triple>()))
            .sum::<usize>();
        n += self.by_p.iter().map(lists_bytes).sum::<usize>();
        n += self.by_ps.iter().map(lists_bytes).sum::<usize>();
        n += self.by_po.iter().map(lists_bytes).sum::<usize>();
        for v in [&self.by_s, &self.by_o].into_iter().flatten() {
            n += v.iter().map(lists_bytes).sum::<usize>();
        }
        n += table_bytes(self.distinct_s.capacity(), 16)
            + table_bytes(self.distinct_o.capacity(), 16);
        n += self.dead.capacity() * size_of::<u64>();
        // the shard vectors themselves
        n += SHARDS
            * (size_of::<HashTable<u32>>()
                + size_of::<FxHashSet<Triple>>()
                + 5 * size_of::<Lists<u64>>());
        n + self.spilled
    }

    /// A graph whose rows `[0, base.len)` are the base tier.
    pub fn with_base(base: Arc<Base>) -> Graph {
        Graph {
            offset: base.len(),
            base: Some(base),
            ..Graph::default()
        }
    }

    /// The base tier, if the graph has one.
    #[inline]
    pub fn base(&self) -> Option<&Base> {
        self.base.as_deref()
    }

    #[inline]
    pub fn len(&self) -> u32 {
        self.offset + self.triples.len() as u32
    }

    /// The in-memory rows, in row order.
    #[inline]
    pub fn derived(&self) -> &[Triple] {
        &self.triples
    }

    /// The triple of an in-memory row.
    #[inline]
    pub fn triple(&self, row: u32) -> Triple {
        self.triples[(row - self.offset) as usize]
    }

    /// Add a triple; returns false if it was already present.
    #[cfg(test)]
    pub fn add(&mut self, t: Triple) -> bool {
        self.add_batch(std::iter::once(t)) == 1
    }

    /// Add triples (duplicates are skipped) on the calling thread; returns the number of
    /// new triples. The base tier is not consulted: the caller adds facts that are not in
    /// it.
    pub fn add_batch(&mut self, ts: impl IntoIterator<Item = Triple>) -> usize {
        let n = self.add_rows(ts, false);
        n.expect("charges that may pass the limit do not fail")
    }

    /// [`add_batch`](Self::add_batch), failing when a buffer or table that has to grow
    /// does not fit the budget. The graph is unusable after an error.
    pub fn try_add_batch(
        &mut self,
        ts: impl IntoIterator<Item = Triple>,
    ) -> Result<usize, MemoryExceeded> {
        self.add_rows(ts, true)
    }

    fn add_rows(
        &mut self,
        ts: impl IntoIterator<Item = Triple>,
        strict: bool,
    ) -> Result<usize, MemoryExceeded> {
        let start = self.triples.len();
        let budget = self.held.budget().clone();
        let mut m = Meter::new(&budget, strict);
        let r = (|| {
            let Graph {
                index,
                triples,
                offset,
                ..
            } = &mut *self;
            let off = *offset;
            for t in ts {
                let h = hash3(&t);
                let tbl = &mut index[shard3(&t)];
                if tbl.find(h, |&r| triples[(r - off) as usize] == t).is_none() {
                    if triples.len() == triples.capacity() {
                        // the new buffer, while the old one is alive
                        let next = (triples.capacity() * 2).max(16);
                        m.add(next * size_of::<Triple>())?;
                        triples.reserve_exact(next - triples.len());
                    }
                    if tbl.len() == tbl.capacity() {
                        m.add(table_bytes(
                            table_grown(tbl.capacity(), tbl.len(), 1),
                            size_of::<u32>(),
                        ))?;
                    }
                    let row = off + triples.len() as u32;
                    triples.push(t);
                    tbl.insert_unique(h, row, |&r| hash3(&triples[(r - off) as usize]));
                }
            }
            Ok(())
        })();
        let new = self.triples.len() - start;
        self.live += new;
        let r = r.and_then(|()| self.fill(start, &mut m));
        self.held.adopt(m.into_bytes());
        self.settle();
        r.map(|()| new)
    }

    /// Merge the outputs of an iteration, in order: each triple that is not in the graph
    /// yet, the base tier included, gets the next row, in the order of its first
    /// occurrence. Returns the number of new triples. Deduplication and the index updates
    /// run with one task per shard.
    ///
    /// `outs_held` holds the charge of the outputs, released when they are dropped. The
    /// merge's own buffers, and the growth of the graph, are charged before they are
    /// allocated; a charge past the limit fails the merge, and the graph is unusable
    /// after an error.
    pub fn merge(&mut self, outs: Vec<Vec<Triple>>, outs_held: Held) -> anyhow::Result<usize> {
        let r = self.merge_charged(outs, outs_held);
        self.settle();
        r
    }

    fn merge_charged(
        &mut self,
        mut outs: Vec<Vec<Triple>>,
        outs_held: Held,
    ) -> anyhow::Result<usize> {
        outs.retain(|o| !o.is_empty());
        let total: usize = outs.iter().map(Vec::len).sum();
        if (total < PAR_MIN && self.base.is_none())
            || outs.len() >= u32::MAX as usize
            || outs.iter().any(|o| o.len() >= u32::MAX as usize)
        {
            let n = self.try_add_batch(outs.into_iter().flatten())?;
            drop(outs_held);
            return Ok(n);
        }
        let budget = self.held.budget().clone();
        // the merge's working buffers, released when it ends
        let mut work = Held::new(&budget);
        let t0 = std::time::Instant::now();
        // a candidate is named by its output and its place there; `offsets` give its
        // position in the order of the outputs
        let mut offsets = Vec::with_capacity(outs.len() + 1);
        let mut n = 0;
        for o in &outs {
            offsets.push(n);
            n += o.len();
        }
        offsets.push(n);
        let outs_ref = &outs;
        let get = |h: u64| outs_ref[(h >> 32) as usize][(h as u32) as usize];
        let offsets_ref = &offsets;
        let gpos = |h: u64| offsets_ref[(h >> 32) as usize] + (h as u32) as usize;
        let threads = rayon::current_num_threads().max(1);
        // contiguous groups of outputs with about `piece` candidates each
        let piece = total.div_ceil(threads * 4).max(4096);
        let mut groups: Vec<(usize, usize)> = Vec::new();
        let mut from = 0;
        for i in 0..outs.len() {
            if offsets[i + 1] - offsets[from] >= piece || i + 1 == outs.len() {
                groups.push((from, i + 1));
                from = i + 1;
            }
        }
        // the candidates of each group by shard: a name of 8 bytes per candidate, in
        // buffers that may double
        work.try_grow(
            2 * total * size_of::<u64>() + groups.len() * SHARDS * size_of::<Vec<u64>>(),
        )?;
        let buckets: Vec<Vec<Vec<u64>>> = groups
            .par_iter()
            .map(|&(a, z)| {
                let mut b: Vec<Vec<u64>> = vec![Vec::new(); SHARDS];
                for (i, o) in outs_ref.iter().enumerate().take(z).skip(a) {
                    for (j, t) in o.iter().enumerate() {
                        b[shard3(t)].push(((i as u64) << 32) | j as u64);
                    }
                }
                b
            })
            .collect();
        let t_bucket = std::time::Instant::now();
        let base = self.base.clone();
        let buckets_ref = &buckets;
        let (triples, off) = (&self.triples, self.offset);
        let budget_ref = &*budget;
        // per shard: the first occurrence of each triple that is new, in position order,
        // and those of them the base tier may hold, with the bytes charged for them
        type Dedup = Result<(Vec<u64>, Vec<u64>, usize), MemoryExceeded>;
        let deduped: Vec<Dedup> = self
            .index
            .par_iter()
            .zip(self.in_base.par_iter())
            .enumerate()
            .map(|(s, (tbl, known))| {
                let mut m = Meter::new(budget_ref, true);
                let c: usize = buckets_ref.iter().map(|b| b[s].len()).sum();
                // the batch's table, sized for every candidate of the shard, and the
                // buffers of the kept ones
                let batch_bytes = table_bytes(table_capacity(c), size_of::<u64>());
                m.add(batch_bytes + 2 * c * size_of::<u64>())?;
                let mut batch: HashTable<u64> = HashTable::with_capacity(c);
                let mut kept: Vec<u64> = Vec::with_capacity(c);
                for b in buckets_ref {
                    for &h in &b[s] {
                        let t = get(h);
                        if !known.is_empty() && known.contains(&t) {
                            continue;
                        }
                        let hash = hash3(&t);
                        if tbl
                            .find(hash, |&r| triples[(r - off) as usize] == t)
                            .is_some()
                        {
                            continue;
                        }
                        if let hashbrown::hash_table::Entry::Vacant(v) =
                            batch.entry(hash, |&x| get(x) == t, |&x| hash3(&get(x)))
                        {
                            v.insert(h);
                            kept.push(h);
                        }
                    }
                }
                drop(batch);
                m.sub(batch_bytes);
                let maybe = match &base {
                    Some(b) => {
                        // the probe filter's tables: an entry per kept candidate at most
                        let filter = 2 * table_bytes(table_capacity(kept.len()), 24);
                        m.add(filter)?;
                        let mut f = ProbeFilter::default();
                        let maybe: Vec<u64> = kept
                            .iter()
                            .copied()
                            .filter(|&h| b.may_contain(&get(h), &mut f))
                            .collect();
                        drop(f);
                        m.sub(filter);
                        maybe
                    }
                    None => Vec::new(),
                };
                Ok((kept, maybe, m.into_bytes()))
            })
            .collect();
        let mut kept: Vec<Vec<u64>> = Vec::with_capacity(SHARDS);
        let mut maybe: Vec<Vec<u64>> = Vec::with_capacity(SHARDS);
        let mut first_err = None;
        for d in deduped {
            match d {
                Ok((k, mb, bytes)) => {
                    work.adopt(bytes);
                    kept.push(k);
                    maybe.push(mb);
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e.into());
        }
        drop(buckets);
        // the new triples that are in the base tier: probed in key order, a contiguous
        // range of keys per task, so that each task reads few blocks
        if let Some(base) = &base {
            let probes: usize = maybe.iter().map(Vec::len).sum();
            // the probe list, the found names and those of them by shard
            work.try_grow(probes * (size_of::<(Triple, u64)>() + 4 * size_of::<u64>()))?;
            let mut probe: Vec<(Triple, u64)> =
                maybe.iter().flatten().map(|&h| (get(h), h)).collect();
            drop(maybe);
            probe.par_sort_unstable();
            let size = probe.len().div_ceil(threads * 4).max(1024);
            let found: Vec<anyhow::Result<Vec<u64>>> = probe
                .par_chunks(size)
                .map(|c| {
                    let ts: Vec<Triple> = c.iter().map(|x| x.0).collect();
                    let f = base.contains_sorted(&ts)?;
                    Ok(c.iter().zip(f).filter(|x| x.1).map(|x| x.0.1).collect())
                })
                .collect();
            drop(probe);
            let mut gone: Vec<u64> = Vec::new();
            for f in found {
                gone.extend(f?);
            }
            if !gone.is_empty() {
                gone.sort_unstable();
                let mut by_shard: Vec<Vec<u64>> = vec![Vec::new(); SHARDS];
                for &h in &gone {
                    by_shard[shard3(&get(h))].push(h);
                }
                let gone_ref = &gone;
                // the cache of base facts grows to its fixed size at most
                let grow: usize = self
                    .in_base
                    .iter()
                    .zip(&by_shard)
                    .filter(|(k, g)| !g.is_empty() && k.capacity() < IN_BASE_SHARD)
                    .map(|_| table_bytes(table_capacity(IN_BASE_SHARD), size_of::<Triple>()))
                    .sum();
                work.try_grow(grow)?;
                self.in_base
                    .par_iter_mut()
                    .zip(kept.par_iter_mut())
                    .zip(by_shard.par_iter())
                    .for_each(|((known, ks), g)| {
                        if g.is_empty() {
                            return;
                        }
                        for &h in g {
                            if known.len() >= IN_BASE_SHARD {
                                known.clear();
                            }
                            known.insert(get(h));
                        }
                        ks.retain(|h| gone_ref.binary_search(h).is_err());
                    });
            }
        }
        let t_dedup = std::time::Instant::now();
        // the kept positions as a bitset with a running count per word: a position's
        // rank among them is its new row
        let words = total.div_ceil(64);
        work.try_grow(words * (size_of::<u64>() + size_of::<u32>()))?;
        let mut bits = vec![0u64; words];
        for k in &kept {
            for &h in k {
                let p = gpos(h);
                bits[p / 64] |= 1 << (p % 64);
            }
        }
        let mut before = Vec::with_capacity(words);
        let mut n = 0u32;
        for w in &bits {
            before.push(n);
            n += w.count_ones();
        }
        let new = n as usize;
        let start = self.triples.len();
        let first_row = self.len();
        // the grown buffer of rows and index tables, while the old ones are alive
        let mut grow = 0;
        if start + new > self.triples.capacity() {
            grow += (start + new) * size_of::<Triple>();
        }
        for (tbl, ks) in self.index.iter().zip(&kept) {
            if tbl.len() + ks.len() > tbl.capacity() {
                grow += table_bytes(
                    table_grown(tbl.capacity(), tbl.len(), ks.len()),
                    size_of::<u32>(),
                );
            }
        }
        work.try_grow(grow)?;
        self.triples.reserve_exact(new);
        let mut item = 0;
        for (i, &w) in bits.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                let p = i * 64 + w.trailing_zeros() as usize;
                while p >= offsets[item + 1] {
                    item += 1;
                }
                self.triples.push(outs[item][p - offsets[item]]);
                w &= w - 1;
            }
        }
        drop(outs);
        drop(outs_held);
        // the new rows in the triple index
        let (bits_ref, before_ref) = (&bits, &before);
        let triples = &self.triples;
        let gpos = |h: u64| offsets_ref[(h >> 32) as usize] + (h as u32) as usize;
        self.index
            .par_iter_mut()
            .zip(kept.par_iter())
            .for_each(|(tbl, ks)| {
                let hasher = |&r: &u32| hash3(&triples[(r - off) as usize]);
                tbl.reserve(ks.len(), hasher);
                for &h in ks {
                    let p = gpos(h);
                    let (w, b) = (p / 64, p % 64);
                    let r =
                        first_row + before_ref[w] + (bits_ref[w] & ((1u64 << b) - 1)).count_ones();
                    tbl.insert_unique(hasher(&r), r, hasher);
                }
            });
        let t_rows = std::time::Instant::now();
        drop(kept);
        self.live += new;
        let mut m = Meter::new(&budget, true);
        let filled = self.fill(start, &mut m);
        work.adopt(m.into_bytes());
        filled?;
        tracing::trace!(
            candidates = total,
            new,
            bucket_ms = (t_bucket - t0).as_millis() as u64,
            dedup_ms = (t_dedup - t_bucket).as_millis() as u64,
            rows_ms = (t_rows - t_dedup).as_millis() as u64,
            fill_ms = t_rows.elapsed().as_millis() as u64,
            "merge"
        );
        Ok(new)
    }

    /// Add the in-memory rows from `start` (an index of `triples`) to the secondary
    /// indexes, with one task per index shard when there are many. Growth is charged to
    /// `m` (and its strictness) before it is allocated.
    fn fill(&mut self, start: usize, m: &mut Meter<'_>) -> Result<(), MemoryExceeded> {
        let n = self.triples.len() - start;
        if n == 0 {
            return Ok(());
        }
        let first_row = self.offset + start as u32;
        let new = &self.triples[start..];
        let (with_s, with_o) = (self.by_s.is_some(), self.by_o.is_some());
        let threads = if n < PAR_MIN {
            1
        } else {
            rayon::current_num_threads().max(1)
        };
        let size = n.div_ceil(threads * 2).max(4096);
        // the pieces: a row number per new row and index, in buffers that may double
        let indexes = 3 + with_s as usize + with_o as usize;
        m.add(
            2 * n * indexes * size_of::<u32>()
                + n.div_ceil(size) * indexes * SHARDS * size_of::<Vec<u32>>(),
        )?;
        let make = |(k, c): (usize, &[Triple])| {
            let empty = || vec![Vec::new(); SHARDS];
            let mut pc = Piece {
                p: empty(),
                ps: empty(),
                po: empty(),
                s: if with_s { empty() } else { Vec::new() },
                o: if with_o { empty() } else { Vec::new() },
            };
            let at = first_row + (k * size) as u32;
            for (j, t) in c.iter().enumerate() {
                let r = at + j as u32;
                pc.p[shard1(t[1])].push(r);
                pc.ps[shard2(t[1], t[0])].push(r);
                pc.po[shard2(t[1], t[2])].push(r);
                if with_s {
                    pc.s[shard1(t[0])].push(r);
                }
                if with_o {
                    pc.o[shard1(t[2])].push(r);
                }
            }
            pc
        };
        let pieces: Vec<Piece> = if threads == 1 {
            new.chunks(size).enumerate().map(make).collect()
        } else {
            new.par_chunks(size).enumerate().map(make).collect()
        };
        let Graph {
            offset,
            triples,
            by_p,
            by_ps,
            by_po,
            by_s,
            by_o,
            distinct_s,
            distinct_o,
            ..
        } = self;
        let mut tasks: Vec<Fill<'_>> = Vec::with_capacity(SHARDS * 5);
        for (s, m) in by_p.iter_mut().enumerate() {
            tasks.push(Fill::P(m, s));
        }
        for (s, m) in by_ps.iter_mut().enumerate() {
            tasks.push(Fill::Ps(m, s));
        }
        for (s, m) in by_po.iter_mut().enumerate() {
            tasks.push(Fill::Po(m, s));
        }
        if let Some(v) = by_s {
            for (s, m) in v.iter_mut().enumerate() {
                tasks.push(Fill::S(m, s));
            }
        }
        if let Some(v) = by_o {
            for (s, m) in v.iter_mut().enumerate() {
                tasks.push(Fill::O(m, s));
            }
        }
        let triples: &[Triple] = triples;
        let offset = *offset;
        let t = |r: u32| triples[(r - offset) as usize];
        let (budget, strict) = (m.budget(), m.strict());
        // per task: whether it counts subjects, the predicates whose distinct subjects (or
        // objects) grew and by how much, the change of the lists' heap beyond their inline
        // rows, and the bytes it charged
        type Filled = Result<(bool, Vec<(u64, u32)>, isize, usize), MemoryExceeded>;
        let run = |task: Fill<'_>| -> Filled {
            let mut grew: FxHashMap<u64, u32> = FxHashMap::default();
            let mut meter = Meter::new(budget, strict);
            let mut spilled = 0isize;
            let subjects = matches!(task, Fill::Ps(..));
            match task {
                Fill::P(m, s) => {
                    for pc in &pieces {
                        let rows = &pc.p[s];
                        let mut i = 0;
                        while i < rows.len() {
                            let p = t(rows[i])[1];
                            let mut j = i + 1;
                            while j < rows.len() && t(rows[j])[1] == p {
                                j += 1;
                            }
                            let e = entry(m, p, &mut meter)?;
                            extend_rows(e, &rows[i..j], &mut meter, &mut spilled)?;
                            i = j;
                        }
                    }
                }
                Fill::Ps(m, s) | Fill::Po(m, s) => {
                    let col = if subjects { 0 } else { 2 };
                    for pc in &pieces {
                        let rows = if subjects { &pc.ps[s] } else { &pc.po[s] };
                        for &r in rows {
                            let x = t(r);
                            let e = entry(m, (x[1], x[col]), &mut meter)?;
                            if e.is_empty() {
                                *grew.entry(x[1]).or_default() += 1;
                            }
                            push_row(e, r, &mut meter, &mut spilled)?;
                        }
                    }
                }
                Fill::S(m, s) => {
                    for pc in &pieces {
                        for &r in &pc.s[s] {
                            let e = entry(m, t(r)[0], &mut meter)?;
                            push_row(e, r, &mut meter, &mut spilled)?;
                        }
                    }
                }
                Fill::O(m, s) => {
                    for pc in &pieces {
                        for &r in &pc.o[s] {
                            let e = entry(m, t(r)[2], &mut meter)?;
                            push_row(e, r, &mut meter, &mut spilled)?;
                        }
                    }
                }
            }
            Ok((
                subjects,
                grew.into_iter().collect(),
                spilled,
                meter.into_bytes(),
            ))
        };
        let grown: Vec<Filled> = if threads == 1 {
            tasks.into_iter().map(run).collect()
        } else {
            tasks.into_par_iter().map(run).collect()
        };
        let mut first_err = None;
        let mut spilled = 0isize;
        for g in grown {
            let (subjects, g, sp, bytes) = match g {
                Ok(x) => x,
                Err(e) => {
                    first_err.get_or_insert(e);
                    continue;
                }
            };
            m.adopt(bytes);
            spilled += sp;
            let d = if subjects {
                &mut *distinct_s
            } else {
                &mut *distinct_o
            };
            for (p, k) in g {
                *d.entry(p).or_default() += k;
            }
        }
        self.spilled = (self.spilled as isize + spilled).max(0) as usize;
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    pub fn ensure_s_index(&mut self) {
        let _ = self.ensure_index(true, false);
    }

    /// Build the per-subject (`subjects`) or per-object index if it is missing, its
    /// growth charged before it is allocated: with `strict`, failing past the limit.
    pub fn ensure_index(&mut self, subjects: bool, strict: bool) -> Result<(), MemoryExceeded> {
        let present = if subjects {
            self.by_s.is_some()
        } else {
            self.by_o.is_some()
        };
        if present {
            return Ok(());
        }
        let budget = self.held.budget().clone();
        let mut meter = Meter::new(&budget, strict);
        let mut spilled = 0isize;
        let col = if subjects { 0 } else { 2 };
        let mut v: Vec<Lists<u64>> = shards();
        let r = (|| {
            for (i, t) in self.triples.iter().enumerate() {
                let e = entry(&mut v[shard1(t[col])], t[col], &mut meter)?;
                push_row(e, self.offset + i as u32, &mut meter, &mut spilled)?;
            }
            Ok(())
        })();
        if r.is_ok() {
            if subjects {
                self.by_s = Some(v);
            } else {
                self.by_o = Some(v);
            }
            self.spilled = (self.spilled as isize + spilled).max(0) as usize;
        }
        self.held.adopt(meter.into_bytes());
        self.settle();
        r
    }
    /// The row of a live in-memory triple.
    #[inline]
    pub fn position(&self, t: &Triple) -> Option<u32> {
        self.index[shard3(t)]
            .find(hash3(t), |&r| self.triple(r) == *t)
            .copied()
    }

    /// Whether a triple is a fact of either tier.
    pub fn contains(&self, t: &Triple) -> bool {
        self.position(t).is_some()
            || self
                .base
                .as_ref()
                .is_some_and(|b| b.contains(t).unwrap_or(false))
    }

    /// Whether a triple is known: an in-memory fact, or one a merge found in the base
    /// tier. Facts of the base tier that no merge met are not known.
    #[inline]
    pub fn known(&self, t: &Triple) -> bool {
        self.position(t).is_some() || (self.base.is_some() && self.in_base[shard3(t)].contains(t))
    }

    /// Whether row `i` holds a live triple.
    #[inline]
    pub fn alive(&self, i: u32) -> bool {
        let Some(i) = i.checked_sub(self.offset) else {
            return true;
        };
        self.dead
            .get(i as usize / 64)
            .is_none_or(|w| w & (1 << (i % 64)) == 0)
    }

    /// Mark the row of a live in-memory triple dead; returns false if the triple is not
    /// live.
    pub fn kill(&mut self, t: &Triple) -> bool {
        let (triples, off) = (&self.triples, self.offset);
        let Ok(e) =
            self.index[shard3(t)].find_entry(hash3(t), |&r| triples[(r - off) as usize] == *t)
        else {
            return false;
        };
        let (i, _) = e.remove();
        let i = i - self.offset;
        let w = i as usize / 64;
        if self.dead.len() <= w {
            let cap = self.dead.capacity();
            self.dead.resize(w + 1, 0);
            // a few bits per row, charged as they grow
            self.held
                .grow((self.dead.capacity() - cap) * size_of::<u64>());
        }
        self.dead[w] |= 1 << (i % 64);
        self.dead_rows += 1;
        self.live -= 1;
        true
    }

    /// Number of dead rows.
    pub fn dead_rows(&self) -> u32 {
        self.dead_rows
    }

    /// Number of live in-memory triples.
    pub fn live(&self) -> usize {
        self.live
    }

    /// Candidate in-memory rows for a pattern with the given bound positions, restricted
    /// to rows `[lo, hi)`. The result may be a superset (callers re-check every
    /// component). The base tier's rows are read with [`Base::for_each`].
    pub fn cands(
        &self,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        lo: u32,
        hi: u32,
    ) -> Cands<'_> {
        let lo = lo.max(self.offset);
        if lo >= hi {
            return Cands::EMPTY;
        }
        fn sl(v: Option<&SmallVec<[u32; 4]>>, lo: u32, hi: u32) -> Cands<'_> {
            v.map_or(Cands::EMPTY, |l| Cands::Slice(restrict(l, lo, hi)))
        }
        let slice = |v| sl(v, lo, hi);
        match (s, p, o) {
            (Some(s), Some(p), Some(o)) => {
                let t = [s, p, o];
                match self.index[shard3(&t)].find(hash3(&t), |&r| self.triple(r) == t) {
                    Some(i) if *i >= lo && *i < hi => Cands::Slice(std::slice::from_ref(i)),
                    _ => Cands::EMPTY,
                }
            }
            (Some(s), Some(p), None) => slice(self.by_ps[shard2(p, s)].get(&(p, s))),
            (None, Some(p), Some(o)) => slice(self.by_po[shard2(p, o)].get(&(p, o))),
            (None, Some(p), None) => slice(self.by_p[shard1(p)].get(&p)),
            (Some(s), None, o) => match (&self.by_s, o, &self.by_o) {
                (Some(m), _, _) => slice(m[shard1(s)].get(&s)),
                (None, Some(o), Some(m)) => slice(m[shard1(o)].get(&o)),
                _ => Cands::Range(lo, hi),
            },
            (None, None, Some(o)) => match &self.by_o {
                Some(m) => slice(m[shard1(o)].get(&o)),
                None => Cands::Range(lo, hi),
            },
            (None, None, None) => Cands::Range(lo, hi),
        }
    }

    /// Whether the range `[lo, hi)` includes the base tier.
    #[inline]
    pub fn reads_base(&self, lo: u32, hi: u32) -> bool {
        self.base.is_some() && lo < self.offset && hi > 0
    }

    /// Number of triples with predicate `p` in `[lo, hi)` (an upper bound for the base
    /// tier).
    pub fn count_p(&self, p: u64, lo: u32, hi: u32) -> usize {
        let base = match &self.base {
            Some(b) if self.reads_base(lo, hi) => b.count_p(p) as usize,
            _ => 0,
        };
        base + self.by_p[shard1(p)]
            .get(&p)
            .map_or(0, |l| restrict(l, lo.max(self.offset), hi).len())
    }

    /// (distinct subjects, distinct objects) of predicate `p`
    pub fn pstats(&self, p: u64) -> (u32, u32) {
        let (bs, bo) = self.base.as_ref().map_or((0, 0), |b| b.pstats(p));
        (
            (self.distinct_s.get(&p).copied().unwrap_or(0) as u64 + bs).clamp(1, u32::MAX as u64)
                as u32,
            (self.distinct_o.get(&p).copied().unwrap_or(0) as u64 + bo).clamp(1, u32::MAX as u64)
                as u32,
        )
    }

    pub fn distinct_predicates(&self) -> usize {
        let base = self.base.as_ref().map_or(0, |b| b.distinct_predicates());
        let mem: usize = self.by_p.iter().map(|m| m.len()).sum();
        (base + mem).max(1)
    }

    pub fn distinct_subjects(&self) -> usize {
        let base = self.offset as usize / 4;
        let mem = self
            .by_s
            .as_ref()
            .map_or(self.triples.len() / 4, |v| v.iter().map(|m| m.len()).sum());
        (base + mem).max(1)
    }

    pub fn distinct_objects(&self) -> usize {
        let base = self.offset as usize / 4;
        let mem = self
            .by_o
            .as_ref()
            .map_or(self.triples.len() / 4, |v| v.iter().map(|m| m.len()).sum());
        (base + mem).max(1)
    }

    /// Whether a live fact matching the pattern is among rows `< hi`.
    pub fn exists(
        &self,
        memo: &mut Memo,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        hi: u32,
    ) -> bool {
        if let Some(b) = &self.base
            && self.reads_base(0, hi)
            && b.any(memo, s, p, o).unwrap_or(false)
        {
            return true;
        }
        let c = self.cands(s, p, o, 0, hi);
        (0..c.len()).map(|i| c.get(i)).any(|r| {
            if !self.alive(r) {
                return false;
            }
            let t = self.triple(r);
            s.is_none_or(|x| x == t[0])
                && p.is_none_or(|x| x == t[1])
                && o.is_none_or(|x| x == t[2])
        })
    }

    /// Whether `t` is a live fact among rows `< hi`.
    pub fn holds_before(&self, memo: &mut Memo, t: &Triple, hi: u32) -> bool {
        self.position(t).is_some_and(|r| r < hi)
            || (self.reads_base(0, hi)
                && self
                    .base
                    .as_ref()
                    .is_some_and(|b| b.contains_with(memo, t).unwrap_or(false)))
    }

    /// The objects of `(s p ?)` among rows `< hi`, the base tier's first.
    pub fn objects(&self, memo: &mut Memo, s: u64, p: u64, hi: u32) -> Vec<u64> {
        let mut out = Vec::new();
        if let Some(b) = &self.base
            && self.reads_base(0, hi)
        {
            let _ = b.for_each_with(memo, Some(s), Some(p), None, |t| {
                out.push(t[2]);
                true
            });
        }
        let c = self.cands(Some(s), Some(p), None, 0, hi);
        out.extend(
            (0..c.len())
                .map(|i| c.get(i))
                .filter(|&i| self.alive(i))
                .map(|i| self.triple(i)[2]),
        );
        out
    }

    /// Members of an RDF list (`rdf:first` / `rdf:rest`), or `None` if `head` is not a
    /// well-formed list (missing links or cycles).
    pub fn list(
        &self,
        memo: &mut Memo,
        head: u64,
        first: u64,
        rest: u64,
        nil: u64,
        hi: u32,
    ) -> Option<Vec<u64>> {
        const MAX_LEN: usize = 1 << 20;
        let mut out = Vec::new();
        let mut cur = head;
        let mut seen = rustc_hash::FxHashSet::default();
        while cur != nil {
            if !seen.insert(cur) || out.len() > MAX_LEN {
                return None;
            }
            // with several values (e.g. copies made by owl:sameAs replacement) the
            // earliest (asserted) one wins
            out.push(*self.objects(memo, cur, first, hi).first()?);
            cur = *self.objects(memo, cur, rest, hi).first()?;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_indexes() {
        let mut g = Graph::default();
        assert!(g.add([1, 10, 2]));
        assert!(!g.add([1, 10, 2]));
        assert_eq!(g.add_batch([[1, 10, 3], [2, 11, 3], [1, 10, 3]]), 2);
        assert_eq!(g.len(), 3);
        assert_eq!(g.cands(Some(1), Some(10), None, 0, 3).len(), 2);
        assert_eq!(g.cands(Some(1), Some(10), None, 1, 3).len(), 1);
        assert_eq!(g.cands(None, Some(10), Some(3), 0, 1).len(), 0);
        assert_eq!(g.cands(Some(2), Some(11), Some(3), 0, 3).len(), 1);
        assert_eq!(g.cands(Some(2), Some(11), Some(3), 0, 2).len(), 0);
        // without the subject index the access falls back to a range scan
        assert_eq!(g.cands(Some(1), None, None, 0, 3).len(), 3);
        g.ensure_s_index();
        assert_eq!(g.cands(Some(1), None, None, 0, 3).len(), 2);
        g.add([1, 12, 4]);
        assert_eq!(g.cands(Some(1), None, None, 0, 4).len(), 3);
        assert_eq!(g.pstats(10), (1, 2));
        assert_eq!(g.count_p(10, 1, 4), 1);
    }

    #[test]
    fn dead_rows() {
        let mut g = Graph::default();
        g.add_batch([[1, 10, 2], [1, 10, 3]]);
        assert!(g.kill(&[1, 10, 2]));
        assert!(!g.kill(&[1, 10, 2]));
        assert!(!g.alive(0) && g.alive(1) && g.alive(5));
        assert_eq!(g.position(&[1, 10, 2]), None);
        assert_eq!(g.objects(&mut Memo::default(), 1, 10, 2), [3]);
        // added again as a new row
        assert!(g.add([1, 10, 2]));
        assert_eq!(g.position(&[1, 10, 2]), Some(2));
        assert_eq!(g.objects(&mut Memo::default(), 1, 10, 3), [3, 2]);
        assert_eq!((g.live(), g.dead_rows()), (2, 1));
    }

    #[test]
    fn lists() {
        let (first, rest, nil) = (100, 101, 102);
        let mut g = Graph::default();
        g.add_batch([
            [1, first, 7],
            [1, rest, 2],
            [2, first, 8],
            [2, rest, nil],
            [5, first, 9],
            [5, rest, 5],
        ]);
        let m = &mut Memo::default();
        assert_eq!(g.list(m, 1, first, rest, nil, g.len()), Some(vec![7, 8]));
        assert_eq!(g.list(m, nil, first, rest, nil, g.len()), Some(vec![]));
        assert_eq!(g.list(m, 5, first, rest, nil, g.len()), None, "cycle");
        assert_eq!(g.list(m, 3, first, rest, nil, g.len()), None, "not a list");
    }

    /// A merge in parallel gives the rows of a merge on one thread.
    #[test]
    fn merge_matches_sequential() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D) % 5000
        };
        let mut outs: Vec<Vec<Triple>> = Vec::new();
        for _ in 0..40 {
            outs.push((0..3000).map(|_| [next(), next() % 7, next()]).collect());
        }
        let mut a = Graph::default();
        a.add_batch([[1, 2, 3], outs[3][5]]);
        let mut b = Graph::default();
        b.add_batch([[1, 2, 3], outs[3][5]]);
        b.ensure_s_index();
        a.add_batch(outs.iter().flatten().copied());
        assert!(
            b.merge(outs.clone(), Held::new(&Budget::unlimited()))
                .unwrap()
                > PAR_MIN
        );
        assert_eq!(a.derived(), b.derived());
        for t in a.derived() {
            assert_eq!(a.position(t), b.position(t));
        }
        let rows = |c: Cands<'_>| (0..c.len()).map(|i| c.get(i)).collect::<Vec<_>>();
        for p in 0..7 {
            assert_eq!(
                rows(a.cands(None, Some(p), None, 0, a.len())),
                rows(b.cands(None, Some(p), None, 0, b.len()))
            );
            assert_eq!(a.pstats(p), b.pstats(p));
            for s in 0..50 {
                assert_eq!(
                    rows(a.cands(Some(s), Some(p), None, 0, a.len())),
                    rows(b.cands(Some(s), Some(p), None, 0, b.len()))
                );
                assert_eq!(
                    rows(a.cands(None, Some(p), Some(s), 0, a.len())),
                    rows(b.cands(None, Some(p), Some(s), 0, b.len()))
                );
            }
        }
        a.ensure_s_index();
        for s in 0..50 {
            assert_eq!(
                rows(a.cands(Some(s), None, None, 0, a.len())),
                rows(b.cands(Some(s), None, None, 0, b.len()))
            );
        }
        // the charge follows the heap
        assert_eq!(b.held.bytes(), b.heap_bytes());
    }

    /// F16 review finding F6: the candidates of an identity rule are all facts of the
    /// base tier. The cache of base facts that merges found keeps a fixed size however
    /// many there are, and a fact it forgot is still found in the base tier, so repeated
    /// merges add nothing to the derived tier, with one input graph or two.
    #[test]
    fn f16_identity_rule_membership_cache_bound() {
        use sparkles_core::id::Id;
        use sparkles_core::io::{RdfFormat, Source};
        use sparkles_core::store::{Store, StoreOptions};
        const N: usize = 20_000;
        let mut nq = String::new();
        for i in 0..N {
            nq.push_str(&format!("<urn:s{i}> <urn:p> <urn:o{}> .\n", i % 97));
            if i % 3 == 0 {
                nq.push_str(&format!("<urn:s{i}> <urn:p> <urn:o{}> <urn:g> .\n", i % 97));
            }
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();
        let snap = store.snapshot();
        let g2 = snap.lookup_iri("urn:g").unwrap();
        for graphs in [vec![Id::DEFAULT_GRAPH], vec![Id::DEFAULT_GRAPH, g2]] {
            let base = Arc::new(Base::new(snap.clone(), &graphs).unwrap());
            let mut all = Vec::new();
            base.for_each(None, None, None, |t| {
                all.push(t);
                true
            })
            .unwrap();
            assert_eq!(all.len(), N);
            let mut g = Graph::with_base(base);
            for round in 0..3 {
                let new = g
                    .merge(vec![all.clone()], Held::new(&Budget::unlimited()))
                    .unwrap();
                assert_eq!(new, 0, "round {round}");
                let cached: usize = g.in_base.iter().map(|s| s.len()).sum();
                assert!(
                    cached <= SHARDS * IN_BASE_SHARD,
                    "{cached} cached base facts"
                );
                let cap: usize = g.in_base.iter().map(|s| s.capacity()).sum();
                assert!(
                    cap <= SHARDS * table_capacity(IN_BASE_SHARD),
                    "capacity {cap}"
                );
                assert!(g.derived().is_empty());
            }
            assert!(all.iter().all(|t| g.contains(t)));
            assert!(g.heap_bytes() < 2 << 20, "{} bytes", g.heap_bytes());
        }
    }
}
