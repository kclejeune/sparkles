//! The facts the engine reads: an optional base tier (the input graphs, read from the
//! store's index permutations, see [`crate::facts`]) and the derived tier in memory
//! ([`crate::tier`]), with a hash index from triple to fact id over the derived tier.
//!
//! Facts are numbered from 0. The base tier's facts are the ids `[0, offset)`, which are
//! not addressable one by one: an access reads the whole base tier or none of it. The
//! derived facts follow from `offset` on, in the order of their first derivation.
//! Semi-naive evaluation restricts any access path to an id range (`[lo, hi)`).
//!
//! The triple index is split into shards by a hash of the triple, so that a merge of new
//! facts deduplicates and fills them with one task per shard ([`Graph::merge`]). The merge
//! gives ids in the order of its input, whatever the number of threads.
//!
//! Incremental maintenance removes triples by marking their facts dead: a dead fact stays
//! in the tier's orders (evaluation skips it) but leaves the triple index, so the triple
//! can be added again as a new fact.
//!
//! The graph's heap is charged to the run's [`Budget`] by capacity
//! ([`Graph::heap_bytes`]). A buffer or table about to grow is charged its new
//! allocation first, with the old one, and the merge charges its working buffers before
//! it allocates them ([`crate::budget`]).

use crate::budget::{Budget, BudgetFailure, Held, Meter, table_bytes, table_capacity, table_grown};
use crate::facts::{Base, Memo, Probe, ProbeFilter};
use crate::tier::{ROW, Tier};
use hashbrown::HashTable;
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use std::hash::BuildHasher;
use std::mem::size_of;
use std::sync::Arc;

pub(crate) use crate::tier::{Cands, Triple};

const SHARDS: usize = 64;
/// Batches smaller than this are merged on the calling thread.
const PAR_MIN: usize = 1 << 14;
/// Facts a merge found in the base tier that one shard of [`Graph::known`]'s cache
/// remembers. A full shard forgets them all, so the cache has a fixed size whatever
/// the input: a fact it forgot is emitted again and found in the base tier by the next
/// merge's probe.
pub(crate) const IN_BASE_SHARD: usize = 128;

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

fn shards<T: Default>() -> Vec<T> {
    (0..SHARDS).map(|_| T::default()).collect()
}

pub(crate) struct Graph {
    base: Option<Arc<Base>>,
    /// the derived facts, from `offset` on
    tier: Tier,
    /// the ids of the live derived triples, hashed by triple, by [`shard3`]
    index: Vec<HashTable<u32>>,
    /// facts a merge found in the base tier, by [`shard3`], so that evaluation does not
    /// emit them again: at most [`IN_BASE_SHARD`] per shard
    pub(crate) in_base: Vec<FxHashSet<Triple>>,
    live: usize,
    /// the first fact of a tail that [`merge_unindexed`](Self::merge_unindexed) appended
    /// to the columns and the triple index but not to the orders yet
    pending: Option<usize>,
    /// predicates of bookkeeping facts (the magic facts of a demand program) that atoms
    /// and `noValue` builtins with a variable predicate do not see
    hidden: Option<Arc<FxHashSet<u64>>>,
    /// the graph's heap, charged to the run's budget. Returned only after every owned
    /// graph payload above has been destroyed.
    held: Held,
}

impl Default for Graph {
    fn default() -> Graph {
        Graph {
            held: Held::new(&Budget::unlimited()),
            hidden: None,
            base: None,
            tier: Tier::new(0),
            index: shards(),
            in_base: shards(),
            live: 0,
            pending: None,
        }
    }
}

impl Graph {
    /// Hide the facts with these predicates from atoms and `noValue` builtins whose
    /// predicate is a variable.
    pub fn set_hidden(&mut self, preds: Arc<FxHashSet<u64>>) {
        self.hidden = (!preds.is_empty()).then_some(preds);
    }

    /// Whether facts with predicate `p` are hidden from a variable predicate.
    #[inline]
    pub fn is_hidden(&self, p: u64) -> bool {
        self.hidden.as_ref().is_some_and(|h| h.contains(&p))
    }

    pub fn with_capacity(n: usize) -> Graph {
        let mut g = Graph::default();
        g.tier.reserve_exact(n);
        for m in &mut g.index {
            *m = HashTable::with_capacity(n / SHARDS);
        }
        g.settle().expect("unpooled graph construction");
        g
    }

    /// [`with_capacity`](Self::with_capacity), charged to `budget` before it allocates.
    pub fn try_with_capacity(n: usize, budget: &Arc<Budget>) -> Result<Graph, BudgetFailure> {
        let bytes = n
            .checked_mul(ROW)
            .and_then(|rows| {
                (SHARDS * table_bytes(table_capacity(n / SHARDS), 4)).checked_add(rows)
            })
            .and_then(|n| n.checked_add(Self::fixed_heap_bytes()))
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let mut held = Held::try_new(budget, bytes)?;
        let mut g = Graph::with_capacity(n);
        // strict: the estimate above may fall short of the heap, and settling must not
        // pass the limit
        held.try_set(g.heap_bytes())?;
        g.held = held;
        Ok(g)
    }

    /// Charge the graph's heap to `budget` from now on, or fail when it does not fit.
    pub fn set_budget(&mut self, budget: &Arc<Budget>) -> Result<(), BudgetFailure> {
        self.held = Held::try_new(budget, self.heap_bytes())?;
        Ok(())
    }

    /// Install already prepared credit; the caller retires the old owner after
    /// every component has committed and after leaving any enclosing lock.
    pub(crate) fn replace_credit(&mut self, held: Held) -> Held {
        assert_eq!(held.bytes(), self.heap_bytes());
        std::mem::replace(&mut self.held, held)
    }

    /// [`set_budget`](Self::set_budget), charging even past the limit.
    pub fn charge_to(&mut self, budget: &Arc<Budget>) -> Result<(), BudgetFailure> {
        let mut h = Held::new(budget);
        h.grow(self.heap_bytes())?;
        self.held = h;
        Ok(())
    }

    /// The budget the graph's heap is charged to.
    pub fn budget(&self) -> &Arc<Budget> {
        self.held.budget()
    }

    /// Bring the charge of the graph's heap in line with [`heap_bytes`](Self::heap_bytes),
    /// releasing what freed allocations held (and charging, past the limit if need be,
    /// what grew beyond the estimates).
    pub fn settle(&mut self) -> Result<(), BudgetFailure> {
        let n = self.heap_bytes();
        self.held.set(n)
    }

    /// [`settle`](Self::settle), failing instead of charging past the limit.
    pub fn try_settle(&mut self) -> Result<(), BudgetFailure> {
        let n = self.heap_bytes();
        self.held.try_set(n)
    }

    /// The heap of the derived tier, by the capacities of its buffers and tables: the
    /// tier's columns, orders and bitmaps, the triple index and the cache of base facts.
    pub fn heap_bytes(&self) -> usize {
        let mut n = self.tier.heap_bytes();
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
        n + Self::fixed_heap_bytes()
    }

    /// The heap of the shard vectors themselves.
    fn fixed_heap_bytes() -> usize {
        SHARDS * (size_of::<HashTable<u32>>() + size_of::<FxHashSet<Triple>>())
    }

    /// Make room for at least `want` more facts, in the columns and in each shard of
    /// the triple index, when there is less than `min` room. Growing them later copies the
    /// facts and rehashes the index while a run waits, so a closure kept for small runs
    /// gets this room once, after its commit. The growth is charged before it is
    /// allocated, and a charge past the limit fails.
    pub fn try_reserve_rows(&mut self, min: usize, want: usize) -> Result<(), BudgetFailure> {
        let (len, cap) = (self.tier.count(), self.tier.capacity());
        if cap - len < min {
            let new = len + want.max(min);
            self.held.try_grow(new * ROW)?;
            self.tier.reserve_exact(new - len);
            self.held.shrink(cap * ROW);
        }
        let (min, want) = (min.div_ceil(SHARDS), want.div_ceil(SHARDS));
        for s in 0..SHARDS {
            let t = &self.index[s];
            if t.capacity() - t.len() >= min {
                continue;
            }
            let (old, len) = (t.capacity(), t.len());
            let to = table_grown(old, len, want.max(min));
            self.held.try_grow(table_bytes(to, size_of::<u32>()))?;
            let tier = &self.tier;
            self.index[s].reserve(to - len, |&r| hash3(&tier.triple(r)));
            self.held.shrink(table_bytes(old, size_of::<u32>()));
        }
        self.try_settle()
    }

    /// A graph whose ids `[0, base.len)` are the base tier (at least one id, so that
    /// a base tier that grows later, see [`set_base`](Self::set_base), is still read).
    pub fn with_base(base: Arc<Base>) -> Graph {
        Graph {
            tier: Tier::new(base.len().max(1)),
            base: Some(base),
            ..Graph::default()
        }
    }

    /// A graph over the same base tier and id numbers as this one, with room for `n`
    /// derived facts, charged to `budget` before it allocates when `strict` (and
    /// otherwise even past the limit).
    pub fn try_like(
        &self,
        n: usize,
        budget: &Arc<Budget>,
        strict: bool,
    ) -> Result<Graph, BudgetFailure> {
        let mut g = if strict || budget.parent().is_some() {
            Graph::try_with_capacity(n, budget)?
        } else {
            let mut g = Graph::with_capacity(n);
            g.charge_to(budget)?;
            g
        };
        g.tier.set_offset(self.offset());
        g.base = self.base.clone();
        Ok(g)
    }

    /// Replace the base tier by `base`, the input graphs of a later snapshot of the same
    /// generation. The derived facts keep their ids: the base tier stays the ids before
    /// [`offset`](Self::offset), which are only read as a whole, so the new tier may hold
    /// more facts than that. The cache of base facts that merges found is emptied, since
    /// the new tier may not hold them.
    pub fn set_base(&mut self, base: Arc<Base>) -> Result<(), BudgetFailure> {
        debug_assert!(self.base.is_some() && self.offset() > 0);
        debug_assert!(
            self.base
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(&old.snap.generation, &base.snap.generation)),
            "a base tier of another generation"
        );
        self.base = Some(base);
        for k in &mut self.in_base {
            *k = FxHashSet::default();
        }
        self.settle()
    }

    /// [`set_base`](Self::set_base), returning the tier it replaced.
    pub fn swap_base(&mut self, base: Arc<Base>) -> Result<Option<Arc<Base>>, BudgetFailure> {
        let old = self.base.clone();
        self.set_base(base)?;
        Ok(old)
    }

    /// The base tier, to change, while no one else shares it.
    pub fn base_mut(&mut self) -> Option<&mut Base> {
        self.base.as_mut().and_then(Arc::get_mut)
    }

    /// The base tier, if the graph has one.
    #[inline]
    pub fn base(&self) -> Option<&Base> {
        self.base.as_deref()
    }

    /// The base tier, shared.
    pub fn base_arc(&self) -> Option<Arc<Base>> {
        self.base.clone()
    }

    /// The first derived id: the ids before it are the base tier.
    #[inline]
    pub fn offset(&self) -> u32 {
        self.tier.offset()
    }

    #[inline]
    pub fn len(&self) -> u32 {
        self.tier.len()
    }

    /// The derived facts, dead ones included.
    #[inline]
    pub fn derived_len(&self) -> usize {
        self.tier.count()
    }

    /// The derived facts in id order, dead ones included.
    #[inline]
    pub fn facts(&self) -> impl Iterator<Item = Triple> + '_ {
        self.tier.facts()
    }

    /// The derived facts from the `from`-th on, in id order.
    pub fn facts_from(&self, from: usize) -> impl Iterator<Item = Triple> + '_ {
        self.tier.facts_range(from, self.tier.count())
    }

    /// The `lo`-th to the `hi`-th derived facts, in id order.
    pub fn facts_range(&self, lo: usize, hi: usize) -> impl Iterator<Item = Triple> + '_ {
        self.tier.facts_range(lo, hi)
    }

    /// The triple of a derived fact.
    #[inline]
    pub fn triple(&self, id: u32) -> Triple {
        self.tier.triple(id)
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
        let n = self.add_rows(ts, false, true, true);
        n.expect("charges that may pass the limit do not fail")
    }

    /// [`add_batch`](Self::add_batch), failing when a buffer or table that has to grow
    /// does not fit the budget. The graph is unusable after an error.
    pub fn try_add_batch(
        &mut self,
        ts: impl IntoIterator<Item = Triple>,
    ) -> Result<usize, BudgetFailure> {
        self.add_rows(ts, true, true, true)
    }

    /// Protect the derived facts from id `floor` on from compaction, until the next call:
    /// the engine sets it to the start of each iteration's delta, so that the runs the
    /// iteration reads in part are never merged with older ones.
    pub fn set_compaction_floor(&mut self, floor: u32) {
        self.tier.set_compaction_floor(floor);
    }

    /// Add facts in one batch. A `batch` added between runs may be the delta a later run
    /// starts from, so it lowers the compaction floor to its first id, and the engine's
    /// merges set the floor themselves. Without `index`, the new facts wait for
    /// [`index_pending`](Self::index_pending) to enter the orders.
    fn add_rows(
        &mut self,
        ts: impl IntoIterator<Item = Triple>,
        strict: bool,
        batch: bool,
        index: bool,
    ) -> Result<usize, BudgetFailure> {
        let start = self.tier.count();
        let budget = self.held.budget().clone();
        let mut m = Meter::new(&budget, strict);
        let r = (|| {
            let Graph { index, tier, .. } = &mut *self;
            for t in ts {
                let h = hash3(&t);
                let tbl = &mut index[shard3(&t)];
                if tbl.find(h, |&r| tier.eq(r, &t)).is_none() {
                    let (rel_grow, rel_old) = tier.rel_growth(t[1]);
                    m.add(rel_grow)?;
                    if tier.count() == tier.capacity() {
                        // the new buffers, while the old ones are alive
                        let old = tier.capacity();
                        let next = (old * 2).max(16);
                        m.add(next * ROW)?;
                        tier.reserve_exact(next - tier.count());
                        m.sub(old * ROW);
                    }
                    if tbl.len() == tbl.capacity() {
                        let old = table_bytes(tbl.capacity(), size_of::<u32>());
                        m.add(table_bytes(
                            table_grown(tbl.capacity(), tbl.len(), 1),
                            size_of::<u32>(),
                        ))?;
                        tbl.reserve(1, |&r| hash3(&tier.triple(r)));
                        m.sub(old);
                    }
                    let id = tier.len();
                    tier.push(t);
                    m.sub(rel_old);
                    tbl.insert_unique(h, id, |&r| hash3(&tier.triple(r)));
                }
            }
            Ok(())
        })();
        let new = self.tier.count() - start;
        self.live += new;
        let r = r.and_then(|()| self.index_from(start, index, &mut m));
        if batch && new > 0 {
            self.tier.lower_floor(self.offset() + start as u32);
        }
        self.held.adopt(m.into_bytes());
        // after an error the graph is unusable, and settling would charge what was
        // allocated beyond the failed charge, past the limit
        let r = r.and_then(|()| {
            if strict {
                self.try_settle()
            } else {
                self.settle()
            }
        });
        r.map(|()| new)
    }

    /// Merge the outputs of an iteration, in order: each triple that is not in the graph
    /// yet, the base tier included, gets the next id, in the order of its first
    /// occurrence. Returns the number of new triples. Deduplication and the index updates
    /// run with one task per shard.
    ///
    /// `outs_held` holds the charge of the outputs, released when they are dropped. The
    /// merge's own buffers, and the growth of the graph, are charged before they are
    /// allocated; a charge past the limit fails the merge, and the graph is unusable
    /// after an error.
    pub fn merge(&mut self, outs: Vec<Vec<Triple>>, outs_held: Held) -> anyhow::Result<usize> {
        let n = self.merge_charged(outs, outs_held, true)?;
        self.try_settle()?;
        Ok(n)
    }

    /// [`merge`](Self::merge) of one batch of a larger whole, such as a closure that an
    /// operator emits in batches: the new facts enter the columns and the triple index,
    /// which deduplicates the next batch against them, and wait for
    /// [`index_pending`](Self::index_pending) to enter the orders as one run per order,
    /// as one merge of the whole would have added them. No probe may read the graph
    /// before that, so the caller keeps it to itself meanwhile.
    pub fn merge_unindexed(&mut self, out: Vec<Triple>, out_held: Held) -> anyhow::Result<usize> {
        let n = self.merge_charged(vec![out], out_held, false)?;
        self.try_settle()?;
        Ok(n)
    }

    /// Sort the facts [`merge_unindexed`](Self::merge_unindexed) appended into their
    /// relations' orders, charging the sort's scratch and the runs first.
    pub fn index_pending(&mut self) -> Result<(), BudgetFailure> {
        let Some(start) = self.pending.take() else {
            return Ok(());
        };
        let budget = self.held.budget().clone();
        let mut m = Meter::new(&budget, true);
        let r = self.tier.index_new(start, &mut m);
        self.held.adopt(m.into_bytes());
        r?;
        self.try_settle()
    }

    /// Index the facts from column index `start` on, or note them as pending.
    fn index_from(
        &mut self,
        start: usize,
        index: bool,
        m: &mut Meter<'_>,
    ) -> Result<(), BudgetFailure> {
        if index {
            debug_assert!(self.pending.is_none(), "a merge over an unindexed tail");
            self.tier.index_new(start, m)
        } else {
            self.pending = Some(self.pending.map_or(start, |p| p.min(start)));
            Ok(())
        }
    }

    fn merge_charged(
        &mut self,
        mut outs: Vec<Vec<Triple>>,
        outs_held: Held,
        index: bool,
    ) -> anyhow::Result<usize> {
        outs.retain(|o| !o.is_empty());
        let total: usize = outs.iter().map(Vec::len).sum();
        if (total < PAR_MIN && self.base.is_none())
            || outs.len() >= u32::MAX as usize
            || outs.iter().any(|o| o.len() >= u32::MAX as usize)
        {
            let n = self.add_rows(outs.into_iter().flatten(), true, false, index)?;
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
        let tier = &self.tier;
        let budget_ref = &*budget;
        // per shard: the first occurrence of each triple that is new, in position order,
        // those of them the base tier may hold, those whose predicate and object decide
        // it, and the bytes charged for them
        type Dedup = Result<(Vec<u64>, Vec<u64>, Vec<u64>, usize), BudgetFailure>;
        let deduped: Vec<Dedup> = self
            .index
            .par_iter()
            .zip(self.in_base.par_iter())
            .enumerate()
            .map(|(s, (tbl, known))| {
                let mut m = Meter::new(budget_ref, true);
                let c: usize = buckets_ref.iter().map(|b| b[s].len()).sum();
                // the batch's table, sized for every candidate of the shard, and the
                // buffers of the kept ones and of the two kinds of probes
                let batch_bytes = table_bytes(table_capacity(c), size_of::<u64>());
                m.add(batch_bytes + 3 * c * size_of::<u64>())?;
                let mut batch: HashTable<u64> = HashTable::with_capacity(c);
                let mut kept: Vec<u64> = Vec::with_capacity(c);
                for b in buckets_ref {
                    for &h in &b[s] {
                        let t = get(h);
                        if !known.is_empty() && known.contains(&t) {
                            continue;
                        }
                        let hash = hash3(&t);
                        if tbl.find(hash, |&r| tier.eq(r, &t)).is_some() {
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
                let (mut maybe, mut pairs) = (Vec::new(), Vec::new());
                if let Some(b) = &base {
                    // the probe filter's table: an entry per kept candidate at most
                    let filter = table_bytes(table_capacity(kept.len()), 24);
                    m.add(filter)?;
                    maybe.reserve_exact(kept.len());
                    pairs.reserve_exact(kept.len());
                    let mut f = ProbeFilter::default();
                    for &h in &kept {
                        match b.classify(&get(h), &mut f) {
                            Probe::No => {}
                            Probe::Maybe => maybe.push(h),
                            Probe::Pair => pairs.push(h),
                        }
                    }
                    drop(f);
                    m.sub(filter);
                }
                Ok((kept, maybe, pairs, m.into_bytes()))
            })
            .collect();
        let mut kept: Vec<Vec<u64>> = Vec::with_capacity(SHARDS);
        let mut maybe: Vec<Vec<u64>> = Vec::with_capacity(SHARDS);
        let mut pairs: Vec<Vec<u64>> = Vec::with_capacity(SHARDS);
        let mut first_err = None;
        for d in deduped {
            match d {
                Ok((k, mb, pr, bytes)) => {
                    work.adopt(bytes);
                    kept.push(k);
                    maybe.push(mb);
                    pairs.push(pr);
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
        // the candidates whose predicate has few objects in the store are probed when the
        // store has their predicate and object, which is counted once per distinct pair
        // of the merge, in parallel
        if let Some(b) = &base {
            let n: usize = pairs.iter().map(Vec::len).sum();
            if n > 0 {
                work.try_grow(n * (size_of::<(u64, u64)>() + 1))?;
                // allocated at the size charged: a collect from the flattened shards would
                // grow by doubling
                let mut list: Vec<(u64, u64)> = Vec::with_capacity(n);
                list.extend(pairs.iter().flatten().map(|&h| {
                    let t = get(h);
                    (t[1], t[2])
                }));
                list.par_sort_unstable();
                list.dedup();
                let any: Vec<bool> = list.par_iter().map(|&(p, o)| b.po_any(p, o)).collect();
                for (mb, pr) in maybe.iter_mut().zip(&pairs) {
                    for &h in pr {
                        let t = get(h);
                        if list.binary_search(&(t[1], t[2])).is_ok_and(|i| any[i]) {
                            mb.push(h);
                        }
                    }
                }
            }
        }
        drop(pairs);
        // the new triples that are in the base tier: probed in key order, a contiguous
        // range of keys per task, so that each task reads few blocks
        if let Some(base) = &base {
            let probes: usize = maybe.iter().map(Vec::len).sum();
            // the probe list, its triples and answers, the found names and those of them
            // by shard
            work.try_grow(
                probes * (size_of::<(Triple, u64)>() + size_of::<Triple>() + 4 * size_of::<u64>()),
            )?;
            let mut probe: Vec<(Triple, u64)> = Vec::with_capacity(probes);
            probe.extend(maybe.iter().flatten().map(|&h| (get(h), h)));
            drop(maybe);
            probe.par_sort_unstable();
            let ts: Vec<Triple> = probe.iter().map(|x| x.0).collect();
            let found = base.contains_sorted(&ts)?;
            drop(ts);
            let mut gone: Vec<u64> = probe
                .iter()
                .zip(found)
                .filter(|x| x.1)
                .map(|x| x.0.1)
                .collect();
            drop(probe);
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
        // rank among them is its new id
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
        let start = self.tier.count();
        let first_id = self.len();
        // the grown columns and index tables, while the old ones are alive; the columns
        // double at least, as `add_rows` grows them, since the operators merge a closure
        // in batches
        let mut grow = 0;
        let cap = self.tier.capacity();
        let next = if start + new > cap {
            (start + new).max(cap * 2)
        } else {
            cap
        };
        if next > cap {
            grow += next * ROW;
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
        if next > cap {
            self.tier.reserve_exact(next - start);
        }
        let mut item = 0;
        for (i, &w) in bits.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                let p = i * 64 + w.trailing_zeros() as usize;
                while p >= offsets[item + 1] {
                    item += 1;
                }
                let t = outs[item][p - offsets[item]];
                let (rel_grow, rel_old) = self.tier.rel_growth(t[1]);
                work.try_grow(rel_grow)?;
                self.tier.push(t);
                work.shrink(rel_old);
                w &= w - 1;
            }
        }
        drop(outs);
        drop(outs_held);
        // the new facts in the triple index
        let (bits_ref, before_ref) = (&bits, &before);
        let tier = &self.tier;
        let gpos = |h: u64| offsets_ref[(h >> 32) as usize] + (h as u32) as usize;
        self.index
            .par_iter_mut()
            .zip(kept.par_iter())
            .for_each(|(tbl, ks)| {
                let hasher = |&r: &u32| hash3(&tier.triple(r));
                tbl.reserve(ks.len(), hasher);
                for &h in ks {
                    let p = gpos(h);
                    let (w, b) = (p / 64, p % 64);
                    let r =
                        first_id + before_ref[w] + (bits_ref[w] & ((1u64 << b) - 1)).count_ones();
                    tbl.insert_unique(hasher(&r), r, hasher);
                }
            });
        let t_rows = std::time::Instant::now();
        drop(kept);
        self.live += new;
        let mut m = Meter::new(&budget, true);
        let filled = self.index_from(start, index, &mut m);
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

    #[cfg(test)]
    pub fn ensure_s_index(&mut self) {
        let _ = self.ensure_index(true, false);
    }

    /// Build the order by subject (`subjects`) or by object over the whole derived tier
    /// if it is missing, its growth charged before it is allocated: with `strict`,
    /// failing past the limit.
    pub fn ensure_index(&mut self, subjects: bool, strict: bool) -> Result<(), BudgetFailure> {
        if self.tier.has_order(subjects) {
            return Ok(());
        }
        let budget = self.held.budget().clone();
        let mut meter = Meter::new(&budget, strict);
        let r = self.tier.ensure_order(subjects, &mut meter);
        self.held.adopt(meter.into_bytes());
        r?;
        self.settle()
    }

    /// The shape of the triple index, for the trace of an iteration: the sum of the
    /// shards' capacities, the sum of their lengths, and the largest and the smallest
    /// capacity of one shard.
    pub fn index_shape(&self) -> (usize, usize, usize, usize) {
        let caps = self.index.iter().map(|t| t.capacity());
        (
            caps.clone().sum(),
            self.index.iter().map(|t| t.len()).sum(),
            caps.clone().max().unwrap_or(0),
            caps.min().unwrap_or(0),
        )
    }

    /// The id of a live derived triple.
    #[inline]
    pub fn position(&self, t: &Triple) -> Option<u32> {
        self.index[shard3(t)]
            .find(hash3(t), |&r| self.tier.eq(r, t))
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

    /// Whether a triple is known: a derived fact, or one a merge found in the base
    /// tier. Facts of the base tier that no merge met are not known.
    #[inline]
    pub fn known(&self, t: &Triple) -> bool {
        self.position(t).is_some() || (self.base.is_some() && self.in_base[shard3(t)].contains(t))
    }

    /// Whether id `i` holds a live fact.
    #[inline]
    pub fn alive(&self, i: u32) -> bool {
        i < self.offset() || self.tier.alive(i)
    }

    /// Mark a live derived triple dead; returns false if the triple is not live.
    pub fn kill(&mut self, t: &Triple) -> Result<bool, BudgetFailure> {
        let tier = &self.tier;
        let Some(&i) = self.index[shard3(t)].find(hash3(t), |&r| tier.eq(r, t)) else {
            return Ok(false);
        };
        // a few bits per fact, charged as they grow: no fact or index changes before
        // the storage of its dead bit is backed
        let (grow, old) = self.tier.dead_growth(i);
        self.held.try_grow(grow)?;
        let tier = &self.tier;
        self.index[shard3(t)]
            .find_entry(hash3(t), |&r| tier.eq(r, t))
            .expect("exclusive row stayed present")
            .remove();
        self.tier.kill(i);
        self.held.shrink(old);
        self.live -= 1;
        Ok(true)
    }

    /// Number of dead derived facts.
    pub fn dead_rows(&self) -> u32 {
        self.tier.dead_rows()
    }

    /// Number of live derived triples.
    pub fn live(&self) -> usize {
        self.live
    }

    /// Whether derived fact `id` is flagged explicit (see [`crate::incremental::Closure`]).
    #[inline]
    pub fn flagged(&self, id: u32) -> bool {
        id >= self.offset() && self.tier.flagged(id)
    }

    /// Flag derived fact `id` explicit or not, charging the growth of the flags before it
    /// is allocated, and with `strict` failing when it does not fit.
    pub fn set_flag_with(&mut self, id: u32, on: bool, strict: bool) -> Result<(), BudgetFailure> {
        let (grow, old) = self.tier.flag_growth(id);
        if grow > 0 {
            if !on {
                return Ok(());
            }
            if strict {
                self.held.try_grow(grow)?;
            } else {
                self.held.grow(grow)?;
            }
        }
        self.tier.set_flag(id, on);
        self.held.shrink(old);
        Ok(())
    }

    /// Candidate derived ids for a pattern with the given bound positions, restricted
    /// to ids `[lo, hi)`. The result may be a superset (callers re-check every
    /// component). The base tier's facts are read with [`Base::for_each`].
    pub fn cands(
        &self,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        lo: u32,
        hi: u32,
    ) -> Cands<'_> {
        debug_assert!(self.pending.is_none(), "a probe over an unindexed tail");
        let lo = lo.max(self.offset());
        if lo >= hi {
            return Cands::empty();
        }
        if let (Some(s), Some(p), Some(o)) = (s, p, o) {
            let t = [s, p, o];
            return match self.index[shard3(&t)].find(hash3(&t), |&r| self.tier.eq(r, &t)) {
                Some(i) if *i >= lo && *i < hi => Cands::ids(std::slice::from_ref(i)),
                _ => Cands::empty(),
            };
        }
        self.tier.cands(s, p, o, lo, hi)
    }

    /// Whether the range `[lo, hi)` includes the base tier.
    #[inline]
    pub fn reads_base(&self, lo: u32, hi: u32) -> bool {
        self.base.is_some() && lo < self.offset() && hi > 0
    }

    /// The number of predicates of the derived facts.
    pub fn predicate_count(&self) -> usize {
        self.tier.predicate_count()
    }

    /// Calls `f` with each predicate of a derived fact from id `lo` on.
    pub fn predicates_since(&self, lo: u32, f: impl FnMut(u64)) {
        self.tier.predicates_since(lo, f);
    }

    /// Number of triples with predicate `p` in `[lo, hi)` (an upper bound for the base
    /// tier, and dead facts counted).
    pub fn count_p(&self, p: u64, lo: u32, hi: u32) -> usize {
        debug_assert!(self.pending.is_none(), "a count over an unindexed tail");
        let base = match &self.base {
            Some(b) if self.reads_base(lo, hi) => b.count_p(p) as usize,
            _ => 0,
        };
        base + self.tier.count_p(p, lo, hi)
    }

    /// The derived facts from id `lo` on whose predicate is hidden (magic facts), which
    /// the limit of inferred facts does not count.
    pub fn hidden_rows(&self, lo: u32) -> usize {
        let Some(h) = &self.hidden else {
            return 0;
        };
        let len = self.len();
        h.iter().map(|&p| self.tier.count_p(p, lo, len)).sum()
    }

    /// The facts inferred from id `lo` on, without the hidden ones.
    pub fn inferred_since(&self, lo: u32) -> usize {
        (self.len() - lo) as usize - self.hidden_rows(lo)
    }

    /// (distinct subjects, distinct objects) of predicate `p`
    pub fn pstats(&self, p: u64) -> (u32, u32) {
        let (bs, bo) = self.base.as_ref().map_or((0, 0), |b| b.pstats(p));
        let (ds, dos) = self.tier.pstats(p);
        (
            (ds as u64 + bs).clamp(1, u32::MAX as u64) as u32,
            (dos as u64 + bo).clamp(1, u32::MAX as u64) as u32,
        )
    }

    pub fn distinct_predicates(&self) -> usize {
        let base = self.base.as_ref().map_or(0, |b| b.distinct_predicates());
        (base + self.tier.predicate_count()).max(1)
    }

    /// The size of the base tier now (an upper bound), which [`set_base`](Self::set_base)
    /// may have changed since [`offset`](Self::offset) was set.
    fn base_len(&self) -> usize {
        self.base.as_ref().map_or(0, |b| b.len() as usize)
    }

    pub fn distinct_subjects(&self) -> usize {
        let base = self.base_len() / 4;
        let mem = self
            .tier
            .distinct_subjects()
            .unwrap_or(self.tier.count() / 4);
        (base + mem).max(1)
    }

    pub fn distinct_objects(&self) -> usize {
        let base = self.base_len() / 4;
        let mem = self
            .tier
            .distinct_objects()
            .unwrap_or(self.tier.count() / 4);
        (base + mem).max(1)
    }

    /// Whether a live fact matching the pattern is among ids `< hi`.
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
        self.cands(s, p, o, 0, hi).iter().any(|r| {
            if !self.alive(r) {
                return false;
            }
            let t = self.triple(r);
            s.is_none_or(|x| x == t[0])
                && p.map_or(!self.is_hidden(t[1]), |x| x == t[1])
                && o.is_none_or(|x| x == t[2])
        })
    }

    /// Whether `t` is a live fact among ids `< hi`.
    pub fn holds_before(&self, memo: &mut Memo, t: &Triple, hi: u32) -> bool {
        self.position(t).is_some_and(|r| r < hi)
            || (self.reads_base(0, hi)
                && self
                    .base
                    .as_ref()
                    .is_some_and(|b| b.contains_with(memo, t).unwrap_or(false)))
    }

    /// The objects of `(s p ?)` among ids `< hi`, the base tier's first and the derived
    /// ones in id order, so that the earliest derived value comes first whatever order
    /// the tier's runs hold them in.
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
        let mut derived: Vec<(u32, u64)> = Vec::new();
        for i in self.cands(Some(s), Some(p), None, 0, hi).iter() {
            if self.alive(i) {
                let t = self.triple(i);
                if t[0] == s && t[1] == p {
                    derived.push((i, t[2]));
                }
            }
        }
        derived.sort_unstable();
        out.extend(derived.into_iter().map(|x| x.1));
        out
    }

    /// Call `f` with each object of `(s p ?)` among ids `< hi` until it returns
    /// `Ok(false)`, or return the error of `f` or of a base-tier read.
    pub fn each_object(
        &self,
        memo: &mut Memo,
        s: u64,
        p: u64,
        hi: u32,
        f: impl FnMut(u64) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        self.each_end(memo, Some(s), p, None, hi, 2, f)
    }

    /// Call `f` with each subject of `(? p o)` among ids `< hi`, as
    /// [`Graph::each_object`] does.
    pub fn each_subject(
        &self,
        memo: &mut Memo,
        p: u64,
        o: u64,
        hi: u32,
        f: impl FnMut(u64) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        self.each_end(memo, None, p, Some(o), hi, 0, f)
    }

    #[allow(clippy::too_many_arguments)]
    fn each_end(
        &self,
        memo: &mut Memo,
        s: Option<u64>,
        p: u64,
        o: Option<u64>,
        hi: u32,
        at: usize,
        mut f: impl FnMut(u64) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        if let Some(b) = &self.base
            && self.reads_base(0, hi)
        {
            let mut stop: Option<anyhow::Result<()>> = None;
            b.for_each_with(memo, s, Some(p), o, |t| match f(t[at]) {
                Ok(true) => true,
                Ok(false) => {
                    stop = Some(Ok(()));
                    false
                }
                Err(e) => {
                    stop = Some(Err(e));
                    false
                }
            })?;
            if let Some(r) = stop {
                return r;
            }
        }
        for r in self.cands(s, Some(p), o, 0, hi).iter() {
            if !self.alive(r) {
                continue;
            }
            let t = self.triple(r);
            if s.is_none_or(|x| x == t[0]) && t[1] == p && o.is_none_or(|x| x == t[2]) && !f(t[at])?
            {
                break;
            }
        }
        Ok(())
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

    fn rows(c: Cands<'_>) -> Vec<u32> {
        c.iter().collect()
    }

    /// Growing a graph fact by fact from nothing charges each buffer once while it is
    /// resized and not again after: the peak stays near the final heap, plus the keyed
    /// entries of 16 bytes per new fact that sorting the batch's runs holds for a moment.
    #[test]
    fn add_rows_charges_the_final_buffers() {
        let b = Budget::unlimited();
        let mut g = Graph::try_with_capacity(0, &b).unwrap();
        g.try_add_batch((0..100_000u64).map(|i| [i, 1, i + 7]))
            .unwrap();
        assert!(
            b.peak() * 10 <= g.heap_bytes() * 19,
            "peak {} over 1.9 times the heap {}",
            b.peak(),
            g.heap_bytes()
        );
    }

    /// Room made ahead is charged, later facts fit it without growing the buffers, and
    /// a reservation the budget refuses fails without a charge.
    #[test]
    fn reserved_rows_are_charged() {
        let b = Budget::unlimited();
        let mut g = Graph::try_with_capacity(0, &b).unwrap();
        g.try_add_batch((0..10_000u64).map(|i| [i, 1, i + 7]))
            .unwrap();
        g.try_reserve_rows(1_000, 5_000).unwrap();
        let cap = g.tier.capacity();
        assert!(cap - g.tier.count() >= 5_000);
        assert!(b.used() >= cap * ROW + 5_000 * size_of::<u32>());
        let shards: Vec<usize> = g.index.iter().map(|t| t.capacity()).collect();
        g.try_add_batch((0..1_000u64).map(|i| [i, 2, i + 7]))
            .unwrap();
        assert_eq!(g.tier.capacity(), cap);
        assert_eq!(
            g.index.iter().map(|t| t.capacity()).collect::<Vec<_>>(),
            shards
        );
        // enough room already: nothing changes
        let used = b.used();
        g.try_reserve_rows(1_000, 5_000).unwrap();
        assert_eq!((g.tier.capacity(), b.used()), (cap, used));

        let tight = Budget::new(Some(4 << 20));
        let mut g = Graph::try_with_capacity(0, &tight).unwrap();
        g.try_add_batch((0..10_000u64).map(|i| [i, 1, i + 7]))
            .unwrap();
        let used = tight.used();
        assert!(g.try_reserve_rows(1 << 20, 1 << 20).is_err());
        assert!(
            tight.used() <= used,
            "{} after the refusal, {used} before",
            tight.used()
        );
        assert!(tight.peak() <= 4 << 20);
    }

    #[test]
    fn ranges_and_indexes() {
        let mut g = Graph::default();
        assert!(g.add([1, 10, 2]));
        assert!(!g.add([1, 10, 2]));
        assert_eq!(g.add_batch([[1, 10, 3], [2, 11, 3], [1, 10, 3]]), 2);
        assert_eq!(g.len(), 3);
        assert_eq!(rows(g.cands(Some(1), Some(10), None, 0, 3)), [0, 1]);
        assert_eq!(rows(g.cands(Some(1), Some(10), None, 1, 3)), [1]);
        assert_eq!(g.cands(None, Some(10), Some(3), 0, 1).len(), 0);
        assert_eq!(rows(g.cands(Some(2), Some(11), Some(3), 0, 3)), [2]);
        assert_eq!(g.cands(Some(2), Some(11), Some(3), 0, 2).len(), 0);
        // without the order by subject the access falls back to a range scan
        assert_eq!(g.cands(Some(1), None, None, 0, 3).len(), 3);
        g.ensure_s_index();
        assert_eq!(rows(g.cands(Some(1), None, None, 0, 3)), [0, 1]);
        g.add([1, 12, 4]);
        assert_eq!(rows(g.cands(Some(1), None, None, 0, 4)), [0, 1, 3]);
        // two runs hold the facts of predicate 10, with one subject between them
        assert_eq!(g.pstats(10), (1, 2));
        assert_eq!(g.count_p(10, 1, 4), 1);
        assert_eq!(
            g.facts().collect::<Vec<_>>(),
            [[1, 10, 2], [1, 10, 3], [2, 11, 3], [1, 12, 4]]
        );
    }

    #[test]
    fn dead_rows() {
        let mut g = Graph::default();
        g.add_batch([[1, 10, 2], [1, 10, 3]]);
        assert!(g.kill(&[1, 10, 2]).unwrap());
        assert!(!g.kill(&[1, 10, 2]).unwrap());
        assert!(!g.alive(0) && g.alive(1));
        assert_eq!(g.position(&[1, 10, 2]), None);
        assert_eq!(g.objects(&mut Memo::default(), 1, 10, 2), [3]);
        // added again as a new fact
        assert!(g.add([1, 10, 2]));
        assert_eq!(g.position(&[1, 10, 2]), Some(2));
        assert_eq!(g.objects(&mut Memo::default(), 1, 10, 3), [3, 2]);
        assert_eq!((g.live(), g.dead_rows()), (2, 1));
        assert!(!g.flagged(2));
        g.set_flag_with(2, true, true).unwrap();
        assert!(g.flagged(2) && !g.flagged(1));
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

    /// A merge in parallel gives the ids of a merge on one thread, and the same
    /// candidates for every probe.
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
        assert_eq!(a.facts().collect::<Vec<_>>(), b.facts().collect::<Vec<_>>());
        for t in a.facts() {
            assert_eq!(a.position(&t), b.position(&t));
        }
        let sorted = |c: Cands<'_>| {
            let mut v = rows(c);
            v.sort_unstable();
            v
        };
        for p in 0..7 {
            assert_eq!(
                sorted(a.cands(None, Some(p), None, 0, a.len())),
                sorted(b.cands(None, Some(p), None, 0, b.len()))
            );
            assert_eq!(a.count_p(p, 0, a.len()), b.count_p(p, 0, b.len()));
            for s in 0..50 {
                assert_eq!(
                    sorted(a.cands(Some(s), Some(p), None, 0, a.len())),
                    sorted(b.cands(Some(s), Some(p), None, 0, b.len()))
                );
                assert_eq!(
                    sorted(a.cands(None, Some(p), Some(s), 0, a.len())),
                    sorted(b.cands(None, Some(p), Some(s), 0, b.len()))
                );
            }
        }
        a.ensure_s_index();
        for s in 0..50 {
            assert_eq!(
                sorted(a.cands(Some(s), None, None, 0, a.len())),
                sorted(b.cands(Some(s), None, None, 0, b.len()))
            );
        }
        // the charge follows the heap
        assert_eq!(b.held.bytes(), b.heap_bytes());
    }

    /// The probes of a graph built by several merges, whose orders hold several runs
    /// and compact, give the facts a set of the same triples gives.
    #[test]
    fn runs_and_compactions_keep_every_probe() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut g = Graph::default();
        let mut all: Vec<Triple> = Vec::new();
        let mut bounds = vec![0u32];
        for round in 0..30 {
            let n = 1 + (next() % 3000) as usize;
            let batch: Vec<Triple> = (0..n)
                .map(|_| [next() % 400, next() % 5, next() % 400])
                .collect();
            // every other round is like an engine run: everything before it may compact
            if round % 2 == 0 {
                g.set_compaction_floor(u32::MAX);
            }
            g.add_batch(batch.iter().copied());
            for t in batch {
                if !all.contains(&t) {
                    all.push(t);
                }
            }
            bounds.push(g.len());
            if round % 7 == 3 {
                let victim = all[(next() as usize) % all.len()];
                g.kill(&victim).unwrap();
            }
            assert_eq!(g.len() as usize, all.len());
        }
        let live: Vec<(u32, Triple)> = (0..g.len())
            .filter(|&r| g.alive(r))
            .map(|r| (r, g.triple(r)))
            .collect();
        let check =
            |g: &Graph, s: Option<u64>, p: Option<u64>, o: Option<u64>, lo: u32, hi: u32| {
                let mut want: Vec<u32> = live
                    .iter()
                    .filter(|(r, t)| {
                        *r >= lo
                            && *r < hi
                            && s.is_none_or(|x| x == t[0])
                            && p.is_none_or(|x| x == t[1])
                            && o.is_none_or(|x| x == t[2])
                    })
                    .map(|x| x.0)
                    .collect();
                want.sort_unstable();
                let mut got: Vec<u32> = g
                    .cands(s, p, o, lo, hi)
                    .iter()
                    .filter(|&r| g.alive(r))
                    .filter(|&r| {
                        let t = g.triple(r);
                        s.is_none_or(|x| x == t[0])
                            && p.is_none_or(|x| x == t[1])
                            && o.is_none_or(|x| x == t[2])
                    })
                    .collect();
                got.sort_unstable();
                assert_eq!(got, want, "pattern {s:?} {p:?} {o:?} in [{lo}, {hi})");
            };
        for p in 0..5u64 {
            for &(lo, hi) in &[
                (0, g.len()),
                (bounds[10], bounds[20]),
                (bounds[29], g.len()),
                (7, 4000),
            ] {
                check(&g, None, Some(p), None, lo, hi);
                for s in (0..400).step_by(37) {
                    check(&g, Some(s), Some(p), None, lo, hi);
                    check(&g, None, Some(p), Some(s), lo, hi);
                    check(&g, Some(s), Some(p), Some(s + 1), lo, hi);
                }
            }
            assert_eq!(
                g.count_p(p, bounds[10], bounds[20]),
                live.iter()
                    .filter(|(r, t)| t[1] == p && *r >= bounds[10] && *r < bounds[20])
                    .count()
                    + g.facts()
                        .enumerate()
                        .filter(|(r, t)| t[1] == p
                            && (*r as u32) >= bounds[10]
                            && (*r as u32) < bounds[20]
                            && !g.alive(*r as u32))
                        .count()
            );
        }
        g.ensure_s_index();
        g.ensure_index(false, false).unwrap();
        for s in (0..400).step_by(23) {
            check(&g, Some(s), None, None, 0, g.len());
            check(&g, None, None, Some(s), 0, g.len());
            check(&g, Some(s), None, Some(s), bounds[5], bounds[25]);
        }
        assert_eq!(g.held.bytes(), g.heap_bytes());
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
                assert_eq!(g.derived_len(), 0);
            }
            assert!(all.iter().all(|t| g.contains(t)));
            assert!(g.heap_bytes() < 2 << 20, "{} bytes", g.heap_bytes());
        }
    }

    /// P3-5 of the follow-up's review: after [`Graph::set_base`] the distinct-count
    /// estimates follow the new base tier's size, not the size the graph was made with.
    #[test]
    fn f16_estimates_follow_the_new_base() {
        use sparkles_core::io::{RdfFormat, Source};
        use sparkles_core::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:s0> <urn:p> <urn:o0> .\n".to_vec(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let graphs = [sparkles_core::id::Id::DEFAULT_GRAPH];
        let mut g = Graph::with_base(Arc::new(Base::new(store.snapshot(), &graphs).unwrap()));
        let before = (g.distinct_subjects(), g.distinct_objects());
        let mut u = String::from("INSERT DATA {");
        for i in 1..4_000 {
            u.push_str(&format!(" <urn:s{i}> <urn:p> <urn:o{i}> ."));
        }
        u.push('}');
        sparkles_core::sparql::update::update(
            &store,
            &u,
            &sparkles_core::sparql::QueryOptions::default(),
        )
        .unwrap();
        g.set_base(Arc::new(Base::new(store.snapshot(), &graphs).unwrap()))
            .unwrap();
        assert_eq!(before, (1, 1));
        assert_eq!(
            (g.distinct_subjects(), g.distinct_objects()),
            (1_000, 1_000)
        );
    }
}
