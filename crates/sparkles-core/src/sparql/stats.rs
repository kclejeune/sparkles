//! Exact counts from the index statistics on any snapshot.
//!
//! A generation's statistics count over every quad of its base index: the distinct
//! subjects, predicates and objects, the quads and the distinct subjects and objects of
//! each predicate, and the distinct instances of each `rdf:type` class. A query sees that
//! base with the snapshot's delta applied, and it may read only some of the graphs. The
//! delta holds inserted quads that are not in the base and deleted quads that are, so a
//! snapshot's quads are the base minus the deleted plus the inserted, without overlap.
//! The counts here start from the statistics and are corrected for both differences:
//!
//! * Base quads in graphs the query does not read are taken out. Each one is a row less.
//!   A value one of them holds is a distinct value less, unless a base quad of a read
//!   graph holds it too, which one index probe per value decides. This part depends only
//!   on the generation and is kept with it.
//! * Each inserted or deleted quad of a read graph is a row more or less. A value it
//!   holds is a distinct value more when no quad of a read graph held it before, and one
//!   less when none holds it after, again one probe per value. This part is kept with the
//!   snapshot, so repeated queries at one commit do not redo it.
//!
//! The work grows with the delta and with the quads of unread graphs, not with the
//! data. The planner takes these counts only when that work is smaller than reading the
//! scan they replace, and the result equals the scan's in every case.

use super::plan::GraphFilter;
use crate::builder::Stats;
use crate::error::Result;
use crate::index::{ALL_COLS, Block, ColMask, Key, Perm, PermIndex, bound_cols, pad};
use crate::store::KeySet;
use crate::store::{Delta, Snapshot};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Rows a scan reads in the time of one index probe, for weighing a correction that
/// probes the index against the scan it replaces. A probe measured at 30 to 60 rows of a
/// run-counting scan; the lower end is taken because later queries at the same snapshot
/// reuse the correction.
pub const PROBE_ROWS: u64 = 32;

#[cfg(test)]
thread_local! {
    /// Lifts the work limit in tests, so that every correction runs on small data.
    pub static UNLIMITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The key column holding the graph in every permutation but GSPO.
const GRAPH_COL: usize = 3;

/// What a count counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Measure {
    /// quads (the rows of a scan that reads a single graph)
    Rows,
    /// distinct values of the first `n` key columns, with `n` at most 3 so that the graph
    /// is not one of them
    Distinct(usize),
}

/// A count the statistics can answer: over a permutation's keys that start with `prefix`
/// and lie in a graph `graphs` accepts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CountKey {
    pub perm: Perm,
    pub prefix: Vec<u64>,
    /// one count per value of the key column after the prefix, instead of a single count
    pub grouped: bool,
    pub measure: Measure,
    pub graphs: GraphFilter,
}

impl CountKey {
    /// The group a key belongs to (0 for a single count).
    fn group(&self, k: &Key) -> u64 {
        if self.grouped {
            k[self.prefix.len()]
        } else {
            0
        }
    }
}

/// The answer to a [`CountKey`].
#[derive(Debug)]
pub struct Counts {
    /// (value of the grouped column, count) in value order, without zero counts; a single
    /// count has the value 0
    pub counts: Vec<(u64, u64)>,
    /// quads of the delta the statistics were corrected for
    pub delta: usize,
    /// base quads of graphs the query does not read that were taken out
    pub unread: u64,
}

impl Counts {
    /// The sum of the counts (the single count when not grouped).
    pub fn total(&self) -> u64 {
        self.counts.iter().map(|c| c.1).sum()
    }

    /// The EXPLAIN note for a plan answered from these counts.
    pub fn note(&self) -> String {
        let mut s = String::from(" [from statistics");
        if self.delta > 0 {
            s.push_str(&format!(", corrected for {} delta quads", self.delta));
        }
        if self.unread > 0 {
            s.push_str(&format!(
                ", without {} quads of graphs not read",
                self.unread
            ));
        }
        s.push(']');
        s
    }
}

/// The named graphs a graph view reads, and whether they are all the snapshot has.
pub(crate) type VisibleGraphs = (Arc<Vec<crate::id::Id>>, bool);

/// Counts already worked out, kept with a generation (the part that depends only on the
/// base) or with a snapshot (the final answers), and the FILTER selectivities and key
/// probes measured on a snapshot (see [`super::sample`] and [`super::keyprobe`]).
#[derive(Default)]
pub struct CountCache {
    counts: Mutex<FxHashMap<CountKey, Arc<Counts>>>,
    sampled: Mutex<FxHashMap<String, super::sample::Sampled>>,
    probed: Mutex<FxHashMap<String, Option<super::keyprobe::Measure>>>,
    /// the named graphs each read rule of a graph view sees (see
    /// [`crate::access::GraphAccess::visible_named`])
    views: Mutex<FxHashMap<String, VisibleGraphs>>,
    /// the masked snapshots of triple-level views, by view key (see
    /// [`crate::access::GraphAccess::masked`])
    masks: Mutex<FxHashMap<String, MaskSlot>>,
}

/// Where the masked snapshot of one view is built once: a second request waits for the
/// first, and builds it again only if the first failed.
pub(crate) type MaskSlot = Arc<Mutex<Option<Arc<Snapshot>>>>;

impl CountCache {
    /// Entries kept before the cache starts over.
    const ENTRIES: usize = 1024;

    fn get(&self, k: &CountKey) -> Option<Arc<Counts>> {
        self.counts.lock().get(k).cloned()
    }

    fn put(&self, k: &CountKey, c: Arc<Counts>) {
        let mut m = self.counts.lock();
        if m.len() >= Self::ENTRIES {
            m.clear();
        }
        m.insert(k.clone(), c);
    }

    /// A selectivity measured on a sample, by the pattern and conjunct it was measured for.
    pub(super) fn sampled(&self, k: &str) -> Option<super::sample::Sampled> {
        self.sampled.lock().get(k).copied()
    }

    pub(super) fn set_sampled(&self, k: String, s: super::sample::Sampled) {
        let mut m = self.sampled.lock();
        if m.len() >= Self::ENTRIES {
            m.clear();
        }
        m.insert(k, s);
    }

    /// What probing a pattern for the values of a small input measured, by both.
    #[allow(clippy::option_option)]
    pub(super) fn probed(&self, k: &str) -> Option<Option<super::keyprobe::Measure>> {
        self.probed.lock().get(k).copied()
    }

    pub(super) fn set_probed(&self, k: String, m: Option<super::keyprobe::Measure>) {
        let mut p = self.probed.lock();
        if p.len() >= Self::ENTRIES {
            p.clear();
        }
        p.insert(k, m);
    }

    /// The visible named graphs of a graph view's read rule, by the rule's key.
    pub(crate) fn view(&self, k: &str) -> Option<VisibleGraphs> {
        self.views.lock().get(k).cloned()
    }

    /// The slot of a view's masked snapshot, by the view's key.
    pub(crate) fn mask_slot(&self, k: &str) -> MaskSlot {
        let mut m = self.masks.lock();
        if let Some(s) = m.get(k) {
            return s.clone();
        }
        // a few views at a time in practice; a burst of distinct ones starts over
        if m.len() >= 64 {
            m.clear();
        }
        let s = MaskSlot::default();
        m.insert(k.to_string(), s.clone());
        s
    }

    pub(crate) fn put_view(&self, k: String, v: VisibleGraphs) {
        let mut m = self.views.lock();
        if m.len() >= Self::ENTRIES {
            m.clear();
        }
        m.insert(k, v);
    }
}

/// The exact answer to `key` on `snap`, or `None` when the statistics do not hold it or
/// correcting them would cost more than `budget` rows of a scan. With `correct` off, only
/// a snapshot without a delta whose base has no quads in unread graphs is answered.
/// `rdf_type` is the snapshot's id of `rdf:type` (class counts); `check` is called now
/// and then for cancellation and deadlines.
pub fn exact_counts(
    snap: &Snapshot,
    key: &CountKey,
    rdf_type: Option<u64>,
    budget: u64,
    correct: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<Arc<Counts>>> {
    if key.perm == Perm::Gspo || !correct && !snap.delta.is_empty() {
        return Ok(None);
    }
    if let Some(c) = snap.counts.get(key) {
        return Ok((correct || c.unread == 0).then_some(c));
    }
    let generation = &snap.generation;
    let stats = &generation.stats;
    let Some(counts) = base_stats(stats, key, rdf_type) else {
        return Ok(None);
    };
    let unread: u64 = stats
        .graphs
        .iter()
        .filter(|(g, _)| !key.graphs.accepts(*g))
        .map(|(_, n)| n)
        .sum();
    if !correct && (unread > 0 || !snap.delta.is_empty()) {
        return Ok(None);
    }
    #[cfg(test)]
    let budget = if UNLIMITED.get() { u64::MAX } else { budget };
    let per_quad = match key.measure {
        Measure::Rows => 1,
        Measure::Distinct(_) => PROBE_ROWS,
    };
    let cached = generation.counts.get(key);
    let mut work = 0u64;
    if unread > 0 && cached.is_none() {
        work = unread.saturating_mul(per_quad);
    }
    let pi = key.perm.index();
    let room = (budget.saturating_sub(work) / per_quad) as usize;
    let changes = (Delta::count_prefix(&snap.delta.ins[pi], &key.prefix)
        + Delta::count_prefix(&snap.delta.del[pi], &key.prefix))
    .min(room.saturating_add(1) as u64);
    work = work.saturating_add(changes.saturating_mul(per_quad));
    if work > budget {
        return Ok(None);
    }
    check()?;
    let base = match cached {
        Some(b) => b,
        None if unread == 0 => Arc::new(Counts {
            counts,
            delta: 0,
            unread: 0,
        }),
        None => {
            let b = Arc::new(without_unread(snap, key, counts, unread, check)?);
            generation.counts.put(key, b.clone());
            b
        }
    };
    let out = if changes == 0 {
        base
    } else {
        Arc::new(with_delta(snap, key, &base, unread == 0, check)?)
    };
    snap.counts.put(key, out.clone());
    Ok(Some(out))
}

/// The statistics' answer to `key` over every graph of the base, in group order and
/// without zero counts.
fn base_stats(stats: &Stats, key: &CountKey, rdf_type: Option<u64>) -> Option<Vec<(u64, u64)>> {
    use Measure::{Distinct, Rows};
    let mut v = match (key.perm, key.prefix.as_slice(), key.grouped, key.measure) {
        (Perm::Spo | Perm::Sop, [], false, Distinct(1)) => vec![(0, stats.distinct_subjects)],
        (Perm::Pso | Perm::Pos, [], false, Distinct(1)) => vec![(0, stats.distinct_predicates)],
        (Perm::Osp | Perm::Ops, [], false, Distinct(1)) => vec![(0, stats.distinct_objects)],
        // the statistics list every predicate of the base: one not listed has no quads
        (Perm::Pso, [p], false, Distinct(2)) => {
            vec![(0, stats.predicate(*p).map_or(0, |s| s.distinct_subjects))]
        }
        (Perm::Pos, [p], false, Distinct(2)) => {
            vec![(0, stats.predicate(*p).map_or(0, |s| s.distinct_objects))]
        }
        (Perm::Pos, [t], true, Distinct(3)) if Some(*t) == rdf_type => {
            let mut c = stats.classes.clone();
            c.sort_unstable();
            c
        }
        (Perm::Pso | Perm::Pos, [], true, Rows) => {
            stats.predicates.iter().map(|s| (s.p, s.count)).collect()
        }
        _ => return None,
    };
    v.retain(|c| c.1 > 0);
    Some(v)
}

/// `counts` (over every graph of the base) without the base quads of graphs that `key`
/// does not read.
fn without_unread(
    snap: &Snapshot,
    key: &CountKey,
    counts: Vec<(u64, u64)>,
    unread: u64,
    check: &dyn Fn() -> Result<()>,
) -> Result<Counts> {
    let generation = &snap.generation;
    let gspo = generation.perm(Perm::Gspo);
    let mut less: FxHashMap<u64, u64> = FxHashMap::default();
    let mut values: Vec<Key> = Vec::new();
    let plen = key.prefix.len();
    for &(g, _) in &generation.stats.graphs {
        if key.graphs.accepts(g) {
            continue;
        }
        gspo.for_each_range_until(&snap.cache, &[g], |b, s, e| {
            for i in s..e {
                let k = key.perm.to_key(&Perm::Gspo.to_quad(&b.key(i)));
                if k[..plen] != key.prefix[..] {
                    continue;
                }
                match key.measure {
                    Measure::Rows => *less.entry(key.group(&k)).or_default() += 1,
                    Measure::Distinct(n) => values.push(project(&k, n)),
                }
            }
            check()?;
            Ok(true)
        })?;
    }
    if let Measure::Distinct(n) = key.measure {
        values.sort_unstable();
        values.dedup();
        let mut probe = Prober::new(snap, key, false);
        for (i, x) in values.iter().enumerate() {
            if i % 1024 == 1023 {
                check()?;
            }
            if !probe.holds(&x[..n], None)? {
                *less.entry(key.group(x)).or_default() += 1;
            }
        }
    }
    let counts = counts
        .into_iter()
        .filter_map(|(g, n)| {
            let n = n - less.get(&g).copied().unwrap_or(0).min(n);
            (n > 0).then_some((g, n))
        })
        .collect();
    Ok(Counts {
        counts,
        delta: 0,
        unread,
    })
}

/// `base` (the counts of the base in the read graphs) with the snapshot's delta applied.
/// `all_read` says that every base quad lies in a read graph.
fn with_delta(
    snap: &Snapshot,
    key: &CountKey,
    base: &Counts,
    all_read: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<Counts> {
    let pi = key.perm.index();
    let del = &snap.delta.del[pi];
    let read = |k: &&Key| key.graphs.accepts(k[GRAPH_COL]);
    let ins = Delta::range(&snap.delta.ins[pi], &key.prefix).filter(read);
    let dels = Delta::range(del, &key.prefix).filter(read);
    let mut change: FxHashMap<u64, i64> = FxHashMap::default();
    let mut delta = 0usize;
    match key.measure {
        Measure::Rows => {
            for k in ins {
                *change.entry(key.group(k)).or_default() += 1;
                delta += 1;
            }
            for k in dels {
                *change.entry(key.group(k)).or_default() -= 1;
                delta += 1;
            }
        }
        Measure::Distinct(n) => {
            // (value, inserted) for each changed quad of a read graph
            let mut values: Vec<(Key, bool)> = ins
                .map(|k| (project(k, n), true))
                .chain(dels.map(|k| (project(k, n), false)))
                .collect();
            delta = values.len();
            values.sort_unstable();
            let mut probe = Prober::new(snap, key, all_read);
            let mut i = 0;
            while i < values.len() {
                if i % 1024 == 0 {
                    check()?;
                }
                let x = values[i].0;
                let mut j = i;
                let (mut inserted, mut deleted) = (false, false);
                while j < values.len() && values[j].0 == x {
                    inserted |= values[j].1;
                    deleted |= !values[j].1;
                    j += 1;
                }
                i = j;
                let x = &x[..n];
                // deleted quads were in the base; inserted ones are there afterwards
                let before = deleted || probe.holds(x, None)?;
                let after = inserted
                    || if deleted {
                        probe.holds(x, Some(del))?
                    } else {
                        before
                    };
                if before != after {
                    let g = key.group(&pad(x, 0));
                    *change.entry(g).or_default() += if after { 1 } else { -1 };
                }
            }
        }
    }
    let mut changed: Vec<(u64, i64)> = change.into_iter().filter(|c| c.1 != 0).collect();
    changed.sort_unstable();
    let mut counts = Vec::with_capacity(base.counts.len() + changed.len());
    let (mut a, mut b) = (base.counts.iter().peekable(), changed.iter().peekable());
    loop {
        let (g, n) = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(&&(ga, na)), Some(&&(gb, nb))) if ga == gb => {
                a.next();
                b.next();
                (ga, na as i64 + nb)
            }
            (Some(&&(ga, na)), Some(&&(gb, _))) if ga < gb => {
                a.next();
                (ga, na as i64)
            }
            (Some(&&(ga, na)), None) => {
                a.next();
                (ga, na as i64)
            }
            (_, Some(&&(gb, nb))) => {
                b.next();
                (gb, nb)
            }
        };
        debug_assert!(n >= 0, "negative count for group {g}");
        if n > 0 {
            counts.push((g, n as u64));
        }
    }
    Ok(Counts {
        counts,
        delta,
        unread: base.unread,
    })
}

/// The first `n` columns of a key, the others zero.
fn project(k: &Key, n: usize) -> Key {
    let mut x = [0; 4];
    x[..n].copy_from_slice(&k[..n]);
    x
}

/// Probes of one permutation of the base, for keys in ascending order: the block of the
/// last probe is kept, so the probes of nearby keys find it without a cache lookup.
struct Prober<'a> {
    snap: &'a Snapshot,
    key: &'a CountKey,
    idx: &'a PermIndex,
    /// every base quad lies in a read graph
    all_read: bool,
    /// (block number, decoded columns, block)
    held: Option<(usize, ColMask, Block)>,
}

impl<'a> Prober<'a> {
    fn new(snap: &'a Snapshot, key: &'a CountKey, all_read: bool) -> Prober<'a> {
        Prober {
            snap,
            key,
            idx: snap.generation.perm(key.perm),
            all_read,
            held: None,
        }
    }

    /// Whether a base quad of a read graph has a key starting with `x`, leaving out the
    /// quads in `deleted` when given. Every quad read before the answer is a quad of an
    /// unread graph or a deleted one, so a probe costs no more than those quads.
    fn holds(&mut self, x: &[u64], deleted: Option<&KeySet>) -> Result<bool> {
        let (lo, hi) = (pad(x, 0), pad(x, u64::MAX));
        let (b0, b1) = self.idx.key_block_range(&lo, &hi);
        let mut mask = bound_cols(&lo, &hi);
        if deleted.is_some() {
            mask = ALL_COLS;
        } else if !self.all_read {
            mask |= 1 << GRAPH_COL;
        }
        for b in b0..b1 {
            if self
                .held
                .as_ref()
                .is_none_or(|h| h.0 != b || h.1 & mask != mask)
            {
                let blk = self.snap.cache.get_cols(self.idx, b, mask)?;
                self.held = Some((b, mask, blk));
            }
            let blk = &self.held.as_ref().unwrap().2;
            let (s, e) = blk.key_range(&lo, &hi);
            if (s..e).any(|i| {
                (self.all_read || self.key.graphs.accepts(blk.cols[GRAPH_COL][i]))
                    && deleted.is_none_or(|d| !d.contains(&blk.key(i)))
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
