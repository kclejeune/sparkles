//! Where an incremental run finds the previous closure, and what it keeps for the next.
//!
//! The explicit facts are the union of the input graphs (the default graph unless the run
//! names others). A closure holds them as its base tier, read from the store, and keeps
//! only the derived facts in memory. The previous closure comes from memory when the
//! caller keeps a [`Cache`] (the server does), and otherwise from the dataset: the
//! inferred graph as of the previous run, recovered from its state now and the commit
//! diff since, plus the derived facts that are not valid RDF. Those generalized facts (a literal subject, a
//! blank node predicate) are never written to the inferred graph but can take part in
//! derivations, so a persistent dataset keeps them in `reasoning-generalized.bin`, bound
//! to the run's commit and rules by `reasoning-generalized.json`.

use crate::INFERRED_GRAPH;
use crate::budget::{Budget, Held, MemoryExceeded, table_bytes, table_capacity};
use crate::diagnostics::LiteralTypings;
use crate::facts::Base;
use crate::graph::{Graph, Triple};
use crate::incremental::{Closure, Fallback};
use crate::inputs::GraphRef;
use crate::terms::Terms;
use oxrdf::vocab::rdf;
use oxrdf::{GraphName, Literal, NamedNode, Term};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::history::At;
use sparkles_core::id::Id;
use sparkles_core::index::Perm;
use sparkles_core::store::{Chunk, DiffOp, DiffOptions, Generation, Snapshot, Store};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The closure of a dataset's last materialization, kept in memory for the next run.
///
/// The closure keeps its derived facts in memory and reads the explicit facts from the
/// snapshot of its commit. A run that updates the closure takes it out and puts the new
/// one back after its commit. Only closures of at most `max_triples` derived facts are
/// kept.
pub struct Cache {
    kept: Mutex<Option<Kept>>,
    max_triples: AtomicUsize,
}

/// Default of [`Cache::new`]'s limit.
pub const DEFAULT_CACHE_TRIPLES: usize = 10_000_000;

impl Kept {
    /// The heap of the closure and the generalized facts.
    fn heap_bytes(&self) -> usize {
        self.closure.heap_bytes() + set_bytes(&self.generalized)
    }
}

impl Default for Cache {
    fn default() -> Cache {
        Cache::new(DEFAULT_CACHE_TRIPLES)
    }
}

impl Cache {
    /// A cache that keeps closures of at most `max_triples` derived facts (0 keeps
    /// none).
    pub fn new(max_triples: usize) -> Cache {
        Cache {
            kept: Mutex::new(None),
            max_triples: AtomicUsize::new(max_triples),
        }
    }

    /// Change the limit (a kept closure over it is dropped at the next run).
    pub fn set_max_triples(&self, n: usize) {
        self.max_triples.store(n, Ordering::Relaxed);
    }

    /// The largest closure kept, in derived facts.
    pub(crate) fn limit(&self) -> usize {
        self.max_triples.load(Ordering::Relaxed)
    }

    /// Drop the kept closure.
    pub fn clear(&self) {
        *self.kept.lock() = None;
    }

    /// Derived facts in the kept closure, and the commit it belongs to.
    pub fn kept(&self) -> Option<(usize, u64)> {
        self.kept
            .lock()
            .as_ref()
            .map(|k| (k.closure.graph.live(), k.commit))
    }

    /// Take the kept closure out for a run that wants the closure of `want`. A closure of
    /// another dataset, commit or generation, or for other rules or input graphs, is of no
    /// use to the run: it is dropped, and the run does not fail for its memory. One the run
    /// can use is taken only when its heap fits `budget`, and is charged there; one that
    /// does not fit stays for a later run, and the result is the error.
    fn take_for(&self, budget: &Arc<Budget>, want: &Want<'_>) -> Taken {
        let mut k = self.kept.lock();
        let Some(kept) = k.as_ref() else {
            return Taken::None;
        };
        if kept.dataset_id != want.dataset_id
            || kept.commit != want.since
            || !Arc::ptr_eq(&kept.snap.generation, want.generation)
        {
            *k = None;
            return Taken::None;
        }
        if kept.digest != want.digest {
            *k = None;
            return Taken::RulesChanged;
        }
        if kept.inputs != want.inputs {
            *k = None;
            return Taken::InputsChanged;
        }
        let need = kept.heap_bytes();
        if let Err(e) = budget.try_charge(need) {
            return Taken::Refused(
                e.with_context(" (taking the closure kept in memory)".into())
                    .into(),
            );
        }
        budget.release(need);
        let Some(mut kept) = k.take() else {
            return Taken::None;
        };
        if let Err(e) = kept.closure.set_budget(budget) {
            *k = Some(kept);
            return Taken::Refused(e.into());
        }
        Taken::Kept(Box::new(kept))
    }

    fn put(&self, k: Kept) {
        if k.closure.graph.live() <= self.max_triples.load(Ordering::Relaxed) {
            *self.kept.lock() = Some(k);
        }
    }
}

/// The state a run needs the kept closure to be the materialization of.
struct Want<'a> {
    dataset_id: uuid::Uuid,
    since: u64,
    generation: &'a Arc<Generation>,
    digest: u64,
    inputs: &'a [GraphRef],
}

/// What [`Cache::take_for`] found.
enum Taken {
    /// no closure is kept, or one of another dataset, commit or generation
    None,
    /// the kept closure is for other rules
    RulesChanged,
    /// the kept closure is for other input graphs
    InputsChanged,
    /// the kept closure does not fit the budget, and stays
    Refused(anyhow::Error),
    Kept(Box<Kept>),
}

pub(crate) struct Kept {
    dataset_id: uuid::Uuid,
    /// the commit the closure is the materialization of
    commit: u64,
    digest: u64,
    /// the input graphs, sorted
    inputs: Vec<GraphRef>,
    /// the snapshot at `commit`
    snap: Arc<Snapshot>,
    closure: Closure,
    generalized: FxHashSet<Triple>,
    /// the generalized facts as saved with the dataset for `commit`
    saved: Option<SavedMeta>,
}

/// The changes of the union of the input graphs and of the inferred graph between two
/// commits, as ids of the later one (or local ids for terms it lacks).
#[derive(Default)]
pub(crate) struct Changes {
    pub base_added: Vec<Triple>,
    pub base_removed: Vec<Triple>,
    pub inferred_added: Vec<Triple>,
    pub inferred_removed: Vec<Triple>,
}

/// The previous closure, ready for an update.
pub(crate) struct Previous {
    pub closure: Closure,
    pub generalized: FxHashSet<Triple>,
    pub changes: Changes,
    /// `memory` or `store`
    pub source: &'static str,
    /// the generalized facts as the dataset keeps them, when read from it
    pub saved: Option<SavedMeta>,
    /// the charge of the changes and the generalized facts
    pub held: Held,
}

/// Find the closure of the materialization at commit `since`, and the changes from
/// there to `snap`.
/// The changes from commit `since` to `snap`, read before the writer lock is taken (the
/// commit diff takes it): from the commit diff, or from the deltas of the closure kept
/// for `since` when the diff cannot reach it (in-memory datasets keep no log). `Err`
/// says why neither can.
pub(crate) fn changes(
    store: &Store,
    since: u64,
    snap: &Arc<Snapshot>,
    cache: Option<&Cache>,
    inputs: &[GraphRef],
) -> Result<RawChanges, String> {
    let e = match logged_changes(store, since, snap.commit, inputs) {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    match kept_changes(store, since, snap, cache, inputs) {
        Some(r) => r,
        None => Err(format!(
            "commit {since} cannot be compared with the head: {e:#}"
        )),
    }
}

/// The changes from `since` to `snap` from the deltas of the closure kept for `since`,
/// when it is kept and `snap` is of the same generation. This takes no lock.
pub(crate) fn kept_changes(
    store: &Store,
    since: u64,
    snap: &Arc<Snapshot>,
    cache: Option<&Cache>,
    inputs: &[GraphRef],
) -> Option<Result<RawChanges, String>> {
    let old = cache.and_then(|c| {
        let k = c.kept.lock();
        k.as_ref()
            .filter(|k| {
                k.dataset_id == store.dataset_id()
                    && k.commit == since
                    && Arc::ptr_eq(&k.snap.generation, &snap.generation)
            })
            .map(|k| k.snap.clone())
    })?;
    Some(delta_changes(&old, snap, inputs).map_err(|e| e.to_string()))
}

/// The heap of a set of facts.
fn set_bytes(s: &FxHashSet<Triple>) -> usize {
    table_bytes(s.capacity(), size_of::<Triple>())
}

impl Changes {
    fn heap_bytes(&self) -> usize {
        (self.base_added.capacity()
            + self.base_removed.capacity()
            + self.inferred_added.capacity()
            + self.inferred_removed.capacity())
            * size_of::<Triple>()
    }
}

/// Find the closure of the materialization at commit `since`, for an update with the
/// `raw` changes from there to `snap`, charged to `budget`. Its base tier is the input
/// graphs of `snap`, `ids` in the order the run resolved them.
///
/// A closure kept in memory for this dataset, commit, generation, rules and inputs is
/// taken out of the cache only when its heap fits the budget, and otherwise stays there
/// and this run fails. A kept closure for another state is dropped. Once taken, the
/// closure is changed by the run, so a run that fails later drops it. A closure read
/// from the store is charged before each buffer is allocated.
#[allow(clippy::too_many_arguments)]
pub(crate) fn previous(
    store: &Store,
    snap: &Arc<Snapshot>,
    since: u64,
    digest: u64,
    inputs: &[GraphRef],
    run_ids: &[Id],
    cache: Option<&Cache>,
    raw: Result<RawChanges, String>,
    budget: &Arc<Budget>,
) -> anyhow::Result<Previous> {
    let dataset_id = store.dataset_id();
    let rules_changed = || Fallback("the rules changed since the previous run".into());
    let inputs_changed = || Fallback("the input graphs changed since the previous run".into());
    let ids = input_ids(snap, None, inputs);
    let want = Want {
        dataset_id,
        since,
        generation: &snap.generation,
        digest,
        inputs,
    };
    let kept = match cache.map_or(Taken::None, |c| c.take_for(budget, &want)) {
        Taken::None => None,
        Taken::RulesChanged => return Err(rules_changed().into()),
        Taken::InputsChanged => return Err(inputs_changed().into()),
        Taken::Refused(e) => return Err(e),
        Taken::Kept(k) => Some(*k),
    };
    if let Some(k) = kept {
        let Kept {
            mut closure,
            mut generalized,
            saved,
            ..
        } = k;
        let raw = raw.map_err(Fallback)?;
        let mut held = Held::new(budget);
        held.grow(set_bytes(&generalized));
        let moved = closure.terms.rebase(snap.clone());
        closure.remap(&moved);
        closure.set_inputs(snap.clone(), run_ids)?;
        remap_set(&mut generalized, &moved, None)?;
        held.set(set_bytes(&generalized));
        let changes = raw.resolve(&closure.terms, snap, &ids)?;
        held.grow(changes.heap_bytes());
        closure.settle();
        budget.check()?;
        return Ok(Previous {
            closure,
            generalized,
            changes,
            source: "memory",
            saved,
            held,
        });
    }
    let Some(root) = store.root() else {
        return Err(Fallback("no closure of the previous run is kept in memory".into()).into());
    };
    let (meta, keys) = match read_saved(root, dataset_id, since, digest)? {
        Saved::Found(meta, _) if meta.input_graphs() != inputs => {
            return Err(inputs_changed().into());
        }
        Saved::Found(meta, keys) => (meta, keys),
        Saved::OtherRules => return Err(rules_changed().into()),
        Saved::Missing => {
            return Err(Fallback(format!("no closure state is saved for commit {since}")).into());
        }
    };
    let raw = raw.map_err(Fallback)?;
    let terms = Terms::new(snap.clone());
    terms.set_budget(budget)?;
    let changes = raw.resolve(&terms, snap, &ids)?;
    let mut held = Held::new(budget);
    held.grow(changes.heap_bytes());
    held.try_grow(table_bytes(table_capacity(keys.len()), size_of::<Triple>()))?;
    let generalized: FxHashSet<Triple> = keys
        .iter()
        .map(|k| {
            let id = |b: &[u8]| terms.id_for(&sparkles_core::id::key_to_term(b));
            [id(&k[0]), id(&k[1]), id(&k[2])]
        })
        .collect();
    held.set(changes.heap_bytes() + set_bytes(&generalized));
    budget.check()?;
    let closure = reconstruct(snap, terms, &changes, &generalized, run_ids, budget)?;
    Ok(Previous {
        closure,
        generalized,
        changes,
        source: "store",
        saved: Some(meta),
        held,
    })
}

/// Replace ids that moved in `s`. The rebuilt set is charged to `held` before it is
/// built, beside the old one, and released once the old one is dropped.
fn remap_set(
    s: &mut FxHashSet<Triple>,
    moved: &FxHashMap<u64, u64>,
    held: Option<&mut Held>,
) -> Result<(), MemoryExceeded> {
    if moved.is_empty() || !s.iter().flatten().any(|x| moved.contains_key(x)) {
        return Ok(());
    }
    let bytes = table_bytes(table_capacity(s.len()), size_of::<Triple>());
    let held = match held {
        Some(h) => {
            h.try_grow(bytes)?;
            Some(h)
        }
        None => None,
    };
    let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
    *s = s.iter().map(|t| [m(t[0]), m(t[1]), m(t[2])]).collect();
    if let Some(h) = held {
        h.shrink(bytes);
    }
    Ok(())
}

/// The closure as of the previous run, over the input graphs of `snap` (`inputs`, in the
/// order the run resolved them): the inferred graph then and the generalized facts, as
/// its derived tier. A fact the input graphs held then is not a derived fact, so the
/// facts of the inferred graph are probed against the input graphs in one sorted pass.
fn reconstruct(
    snap: &Arc<Snapshot>,
    terms: Terms,
    ch: &Changes,
    generalized: &FxHashSet<Triple>,
    inputs: &[Id],
    budget: &Arc<Budget>,
) -> anyhow::Result<Closure> {
    let base = Arc::new(Base::new(snap.clone(), inputs)?);
    let inferred = snap.lookup_iri(INFERRED_GRAPH);
    let m = match inferred {
        Some(g) => snap.count(Perm::Gspo, &[g.0])? as usize,
        None => 0,
    };
    let fact = size_of::<Triple>();
    let n = m + ch.inferred_removed.len();
    // the facts read, the result of their probe, and the sets of changed facts
    let held = Held::try_new(
        budget,
        n * (fact + 1)
            + table_bytes(table_capacity(ch.inferred_added.len()), fact)
            + table_bytes(table_capacity(ch.base_added.len()), fact)
            + table_bytes(table_capacity(ch.base_removed.len()), fact),
    )?;
    let mut derived: Vec<Triple> = Vec::with_capacity(n);
    if let Some(g) = inferred {
        let skip: FxHashSet<Triple> = ch.inferred_added.iter().copied().collect();
        snap.scan(Perm::Gspo, &[g.0], |c| {
            match c {
                Chunk::Block(b, s, e) => derived.extend(
                    (s..e)
                        .map(|i| [b.cols[1][i], b.cols[2][i], b.cols[3][i]])
                        .filter(|t| !skip.contains(t)),
                ),
                Chunk::Row(k) => {
                    let t = [k[1], k[2], k[3]];
                    if !skip.contains(&t) {
                        derived.push(t);
                    }
                }
            }
            Ok(true)
        })?;
    }
    derived.extend(&ch.inferred_removed);
    derived.sort_unstable();
    derived.dedup();
    // a fact the input graphs held at the previous run is explicit there: one they hold
    // now that was not added since, or one removed since
    let now = base.contains_sorted(&derived)?;
    let added: FxHashSet<Triple> = ch.base_added.iter().copied().collect();
    let removed: FxHashSet<Triple> = ch.base_removed.iter().copied().collect();
    let mut i = 0;
    derived.retain(|t| {
        let then = (now[i] && !added.contains(t)) || removed.contains(t);
        i += 1;
        !then
    });
    drop((now, added, removed));
    let mut graph =
        Graph::with_base(base).try_like(derived.len() + generalized.len(), budget, true)?;
    graph.try_add_batch(derived)?;
    graph.try_add_batch(generalized.iter().copied())?;
    drop(held);
    Ok(Closure::new(graph, terms))
}

/// Changes as the commit diff reports them, or as ids of one generation.
pub(crate) enum RawChanges {
    Quads {
        /// the changes of each input graph
        base: Vec<(DiffOp, oxrdf::Quad)>,
        inferred: Vec<(DiffOp, oxrdf::Quad)>,
        /// whether there is more than one input graph: a triple then joins or leaves the
        /// union only when no other input graph holds it
        union: bool,
    },
    Ids(Changes),
}

impl RawChanges {
    /// Triples removed from the default graph.
    pub(crate) fn base_removed(&self) -> usize {
        match self {
            RawChanges::Ids(c) => c.base_removed.len(),
            RawChanges::Quads { base, .. } => {
                base.iter().filter(|(op, _)| *op == DiffOp::Remove).count()
            }
        }
    }

    /// The changes as ids of `terms`, whose snapshot is `snap`; `inputs` are the ids of
    /// the input graphs in it.
    fn resolve(self, terms: &Terms, snap: &Snapshot, inputs: &[Id]) -> anyhow::Result<Changes> {
        Ok(match self {
            RawChanges::Ids(c) => c,
            RawChanges::Quads {
                base,
                inferred,
                union,
            } => {
                let id = |t: Term| terms.id_for(&t);
                let triple = |q: oxrdf::Quad| -> Triple {
                    [id(q.subject.into()), id(q.predicate.into()), id(q.object)]
                };
                let mut c = Changes::default();
                if union {
                    // per triple, the graphs whose change the diff reports, and whether
                    // the triple was added there
                    let mut by_triple: FxHashMap<Triple, Vec<(Option<Id>, bool)>> =
                        FxHashMap::default();
                    for (op, q) in base {
                        let g = match &q.graph_name {
                            GraphName::NamedNode(n) => snap.lookup_iri(n.as_str()),
                            _ => Some(Id::DEFAULT_GRAPH),
                        };
                        by_triple
                            .entry(triple(q))
                            .or_default()
                            .push((g, op == DiffOp::Add));
                    }
                    for (t, changed) in by_triple {
                        let stored = t
                            .iter()
                            .all(|&x| Id(x).tag() != sparkles_core::id::Tag::Local);
                        let (mut before, mut after) = (false, false);
                        for &g in inputs {
                            let now =
                                stored && snap.contains(&[Id(t[0]), Id(t[1]), Id(t[2]), g])?;
                            after |= now;
                            before |= match changed.iter().find(|(cg, _)| *cg == Some(g)) {
                                Some((_, added)) => !added,
                                None => now,
                            };
                        }
                        // a graph the snapshot no longer names held the triple before when
                        // the diff removed it there
                        before |= changed.iter().any(|(cg, added)| cg.is_none() && !added);
                        match (before, after) {
                            (false, true) => c.base_added.push(t),
                            (true, false) => c.base_removed.push(t),
                            _ => {}
                        }
                    }
                } else {
                    for (op, q) in base {
                        match op {
                            DiffOp::Add => c.base_added.push(triple(q)),
                            DiffOp::Remove => c.base_removed.push(triple(q)),
                        }
                    }
                }
                for (op, q) in inferred {
                    match op {
                        DiffOp::Add => c.inferred_added.push(triple(q)),
                        DiffOp::Remove => c.inferred_removed.push(triple(q)),
                    }
                }
                c
            }
        })
    }
}

/// The ids of the input graphs in `snap` (or in `other`, for a graph only it names).
pub(crate) fn input_ids(snap: &Snapshot, other: Option<&Snapshot>, inputs: &[GraphRef]) -> Vec<Id> {
    let mut ids: Vec<Id> = inputs
        .iter()
        .filter_map(|g| match g {
            GraphRef::Default => Some(Id::DEFAULT_GRAPH),
            GraphRef::Named(n) => snap
                .lookup_iri(n)
                .or_else(|| other.and_then(|o| o.lookup_iri(n))),
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The changes between two commits from the store's commit diff.
fn logged_changes(
    store: &Store,
    since: u64,
    head: u64,
    inputs: &[GraphRef],
) -> anyhow::Result<RawChanges> {
    if since == head {
        return Ok(RawChanges::Ids(Changes::default()));
    }
    let diff = |g: GraphName| -> anyhow::Result<Vec<(DiffOp, oxrdf::Quad)>> {
        let o = DiffOptions {
            graph: Some(g),
            ..Default::default()
        };
        let d = store.diff(&At::Commit(since), &At::Commit(head), &o)?;
        Ok(d.iter().collect())
    };
    let mut base = Vec::new();
    for g in inputs {
        base.extend(diff(match g {
            GraphRef::Default => GraphName::DefaultGraph,
            GraphRef::Named(n) => GraphName::NamedNode(NamedNode::new_unchecked(n.as_str())),
        })?);
    }
    Ok(RawChanges::Quads {
        base,
        inferred: diff(GraphName::NamedNode(NamedNode::new_unchecked(
            INFERRED_GRAPH,
        )))?,
        union: inputs.len() > 1,
    })
}

/// The changes between two snapshots of one generation, from their deltas.
fn delta_changes(
    old: &Snapshot,
    new: &Snapshot,
    inputs: &[GraphRef],
) -> anyhow::Result<RawChanges> {
    let i = Perm::Gspo.index();
    let inferred = new
        .lookup_iri(INFERRED_GRAPH)
        .or_else(|| old.lookup_iri(INFERRED_GRAPH));
    let ids = input_ids(new, Some(old), inputs);
    // whether a triple is in one of the input graphs of a snapshot
    let in_union = |s: &Snapshot, t: &Triple| -> anyhow::Result<bool> {
        for g in &ids {
            if s.contains(&[Id(t[0]), Id(t[1]), Id(t[2]), *g])? {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let mut c = Changes::default();
    let mut seen: FxHashSet<[u64; 4]> = FxHashSet::default();
    let mut seen_base: FxHashSet<Triple> = FxHashSet::default();
    for set in [
        &old.delta.ins[i],
        &old.delta.del[i],
        &new.delta.ins[i],
        &new.delta.del[i],
    ] {
        for k in set.iter() {
            let base = ids.binary_search(&Id(k[0])).is_ok();
            if !(base || inferred.is_some_and(|g| g.0 == k[0])) || !seen.insert(*k) {
                continue;
            }
            let q = Perm::Gspo.to_quad(k);
            let t = [q[0].0, q[1].0, q[2].0];
            if base {
                if !seen_base.insert(t) {
                    continue;
                }
                match (in_union(old, &t)?, in_union(new, &t)?) {
                    (false, true) => c.base_added.push(t),
                    (true, false) => c.base_removed.push(t),
                    _ => {}
                }
                continue;
            }
            match (old.contains(&q)?, new.contains(&q)?) {
                (false, true) => c.inferred_added.push(t),
                (true, false) => c.inferred_removed.push(t),
                _ => {}
            }
        }
    }
    Ok(RawChanges::Ids(c))
}

/// Keep the closure after the run's commit: in the cache, while the commit kept the
/// generation (a commit that rebuilt it changed every id), with the local ids the commit
/// stored replaced by their store ids.
#[allow(clippy::too_many_arguments)]
pub(crate) fn keep(
    store: &Store,
    cache: Option<&Cache>,
    receipt: &sparkles_core::commit::Receipt,
    before: &Arc<Snapshot>,
    digest: u64,
    inputs: &[GraphRef],
    mut closure: Closure,
    mut generalized: FxHashSet<Triple>,
    stored: &FxHashMap<u64, Id>,
    saved: Option<SavedMeta>,
) -> anyhow::Result<bool> {
    let Some(cache) = cache else {
        return Ok(false);
    };
    if closure.graph.live() > cache.max_triples.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let snap = if receipt.committed {
        store.snapshot()
    } else {
        before.clone()
    };
    if snap.commit != receipt.commit.seq || !Arc::ptr_eq(&snap.generation, &before.generation) {
        return Ok(false);
    }
    // the work after the commit is charged to the run, strictly: a closure that does
    // not fit is not kept
    let budget = closure.graph.budget().clone();
    let mut moved: FxHashMap<u64, u64> = stored.iter().map(|(l, id)| (*l, id.0)).collect();
    moved.extend(closure.terms.rebase(snap.clone()));
    closure.try_remap(&moved)?;
    // the input graphs of the commit, which are those the run read
    let ids = match closure.graph.base() {
        Some(b) => b.graph_ids(),
        None => return Ok(false),
    };
    closure.set_inputs(snap.clone(), &ids)?;
    let mut held = Held::new(&budget);
    remap_set(&mut generalized, &moved, Some(&mut held))?;
    drop(held);
    closure.try_settle()?;
    budget.check_peak()?;
    closure.compact_if_needed(true)?;
    // the searches for other proofs look facts up by subject and by object: build those
    // indexes now, after the commit, rather than in the next run
    closure.graph.ensure_index(true, true)?;
    closure.graph.ensure_index(false, true)?;
    budget.check_peak()?;
    cache.put(Kept {
        dataset_id: receipt.dataset_id,
        commit: receipt.commit.seq,
        digest,
        inputs: inputs.to_vec(),
        snap,
        closure,
        generalized,
        saved,
    });
    Ok(true)
}

// ------------------------------------------------------------ saved state ----
//
// `reasoning-generalized.bin` holds the generalized facts of one run, and
// `reasoning-generalized.log` the facts later incremental runs added (`+`) and removed
// (`-`), so that a run that changes a few of them appends a few records. The `.json`
// binds both to the commit and the rules of the last run: the checksum of the `.bin`,
// and the length and checksum of the part of the log that belongs to it. The `.bin` is
// written again once the log is longer than the `.bin`.

const SAVED_BIN: &str = "reasoning-generalized.bin";
const SAVED_LOG: &str = "reasoning-generalized.log";
const SAVED_META: &str = "reasoning-generalized.json";
const MAGIC: &[u8] = b"SPKGEN1\n";

/// What `reasoning-generalized.json` records.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedMeta {
    dataset_id: String,
    commit: u64,
    /// the rules digest, hex
    rules: String,
    /// facts in the `.bin`
    triples: u64,
    /// size and FNV-1a of the `.bin`, hex
    bytes: u64,
    checksum: String,
    /// the length and FNV-1a of the log's records that belong to the `.bin`
    #[serde(default)]
    log_bytes: u64,
    #[serde(default)]
    log_checksum: String,
    /// the input graphs, sorted (absent: the default graph alone)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inputs: Option<Vec<GraphRef>>,
}

impl SavedMeta {
    fn input_graphs(&self) -> Vec<GraphRef> {
        self.inputs
            .clone()
            .unwrap_or_else(|| vec![GraphRef::Default])
    }
}

/// The generalized facts a run added and removed.
#[derive(Default)]
pub(crate) struct GeneralizedChanges {
    pub added: Vec<Triple>,
    pub removed: Vec<Triple>,
}

pub(crate) fn fnv(bytes: &[u8], mut h: u64) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub(crate) const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

type Keys = [Vec<u8>; 3];

enum Saved {
    Found(SavedMeta, Vec<Keys>),
    /// saved for the commit, with other rules
    OtherRules,
    Missing,
}

fn put_keys(out: &mut Vec<u8>, k: &Keys) {
    for x in k {
        out.extend((x.len() as u32).to_le_bytes());
        out.extend(x);
    }
}

/// The metadata of the generalized facts saved for the materialization at `commit` of
/// the dataset `dataset_id`, whatever its rules.
fn saved_meta(root: &Path, dataset_id: uuid::Uuid, commit: u64) -> Option<SavedMeta> {
    let m = std::fs::read(root.join(SAVED_META)).ok()?;
    let meta = serde_json::from_slice::<SavedMeta>(&m).ok()?;
    (meta.dataset_id == dataset_id.to_string() && meta.commit == commit).then_some(meta)
}

/// A reader that hashes what it reads and counts it.
struct Hashing<R> {
    r: R,
    h: u64,
    n: u64,
}

impl<R: std::io::Read> std::io::Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf)?;
        self.h = fnv(&buf[..n], self.h);
        self.n += n as u64;
        Ok(n)
    }
}

impl<R: std::io::Read> Hashing<R> {
    fn new(r: R) -> Self {
        Hashing {
            r,
            h: FNV_START,
            n: 0,
        }
    }

    /// The next three length-prefixed keys, each at most what is left before `end`.
    fn keys(&mut self, end: u64) -> Option<Keys> {
        use std::io::Read as _;
        let mut t: Keys = Default::default();
        for k in &mut t {
            let mut len = [0u8; 4];
            self.read_exact(&mut len).ok()?;
            let len = u32::from_le_bytes(len) as u64;
            if len > end.saturating_sub(self.n) {
                return None;
            }
            k.resize(len as usize, 0);
            self.read_exact(k).ok()?;
        }
        Some(t)
    }

    fn checksum(&self) -> String {
        format!("{:016x}", self.h)
    }
}

/// Read the generalized facts that `meta` describes in one pass over the saved files,
/// without holding them: `f(true, keys)` for each fact of the `.bin`, then `f(added,
/// keys)` for each record of the log that belongs to it. Returns whether the files are
/// consistent with `meta`. The checksums are known only at the end, so on `false` the
/// caller drops what it collected.
fn scan_saved(root: &Path, meta: &SavedMeta, mut f: impl FnMut(bool, Keys)) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(root.join(SAVED_BIN)) else {
        return false;
    };
    if file.metadata().map(|m| m.len()).ok() != Some(meta.bytes) {
        return false;
    }
    let mut r = Hashing::new(std::io::BufReader::new(file.take(meta.bytes)));
    let mut head = [0u8; 16];
    if r.read_exact(&mut head).is_err() || &head[..8] != MAGIC {
        return false;
    }
    if u64::from_le_bytes(head[8..16].try_into().expect("8 bytes")) != meta.triples {
        return false;
    }
    for _ in 0..meta.triples {
        let Some(k) = r.keys(meta.bytes) else {
            return false;
        };
        f(true, k);
    }
    if std::io::copy(&mut r, &mut std::io::sink()).is_err()
        || r.n != meta.bytes
        || r.checksum() != meta.checksum
    {
        return false;
    }
    if meta.log_bytes == 0 {
        return true;
    }
    let Ok(log) = std::fs::File::open(root.join(SAVED_LOG)) else {
        return false;
    };
    let mut r = Hashing::new(std::io::BufReader::new(log.take(meta.log_bytes)));
    while r.n < meta.log_bytes {
        let mut op = [0u8; 1];
        if r.read_exact(&mut op).is_err() {
            return false;
        }
        let Some(k) = r.keys(meta.log_bytes) else {
            return false;
        };
        match op[0] {
            b'+' => f(true, k),
            b'-' => f(false, k),
            _ => return false,
        }
    }
    r.n == meta.log_bytes && r.checksum() == meta.log_checksum
}

/// The generalized facts saved for the materialization at `commit`, as term keys.
fn read_saved(
    root: &Path,
    dataset_id: uuid::Uuid,
    commit: u64,
    digest: u64,
) -> anyhow::Result<Saved> {
    let Some(meta) = saved_meta(root, dataset_id, commit) else {
        return Ok(Saved::Missing);
    };
    if meta.rules != format!("{digest:016x}") {
        return Ok(Saved::OtherRules);
    }
    let mut set: FxHashSet<Keys> = FxHashSet::default();
    let consistent = scan_saved(root, &meta, |added, k| {
        if added {
            set.insert(k);
        } else {
            set.remove(&k);
        }
    });
    Ok(if consistent {
        Saved::Found(meta, set.into_iter().collect())
    } else {
        Saved::Missing
    })
}

/// The literal typings among the generalized facts of the materialization at `commit` of
/// `store`'s dataset (see [`LiteralTypings`]): from the closure that `cache` keeps for
/// that commit, or else from the generalized facts a persistent dataset saved for it.
/// `None` when neither has them. The typings are read whatever the rules of the
/// materialization were, so a caller that reports them checks that its rules are sound
/// for OWL 2.
///
/// The cache's lock is held only for a pass over the kept generalized facts that compares
/// ids, which keeps the `rdf:type` facts with a literal subject and an object the check
/// decides. Their subjects are decoded after the lock is released, so a run that waits
/// for the cache, and the writers behind it, do not wait for the decoding. A typing whose
/// subject is a term the rules made is decoded under the lock, since only the closure
/// knows it. Nothing is kept between calls, and an in-memory dataset has no typings while
/// a run has its closure out of the cache.
pub fn literal_typings(
    store: &Store,
    commit: u64,
    cache: Option<&Cache>,
) -> anyhow::Result<Option<LiteralTypings>> {
    if let Some(c) = cache {
        let ty_term = Term::NamedNode(rdf::TYPE.into_owned());
        // under the lock: typings decided at once, the others as (subject, datatype)
        let mut found = Vec::new();
        let mut pending: Vec<(u64, u64)> = Vec::new();
        let mut datatypes: FxHashMap<u64, Option<NamedNode>> = FxHashMap::default();
        let snap = 'kept: {
            let k = c.kept.lock();
            let Some(k) = k
                .as_ref()
                .filter(|k| k.dataset_id == store.dataset_id() && k.commit == commit)
            else {
                break 'kept None;
            };
            let terms = &k.closure.terms;
            let ty = terms.snap.lookup_iri(rdf::TYPE.as_str()).map(|i| i.0);
            let mut predicates: FxHashMap<u64, bool> = FxHashMap::default();
            for t in &k.generalized {
                let is_type = match ty {
                    Some(ty) => t[1] == ty,
                    None => *predicates
                        .entry(t[1])
                        .or_insert_with(|| terms.term(t[1]).as_ref() == Some(&ty_term)),
                };
                if !is_type || terms.kind(t[0]) != crate::terms::Kind::Literal {
                    continue;
                }
                let dt = datatypes
                    .entry(t[2])
                    .or_insert_with(|| match terms.term(t[2]) {
                        Some(Term::NamedNode(n))
                            if crate::diagnostics::decidable_range(n.as_str()) =>
                        {
                            Some(n)
                        }
                        _ => None,
                    });
                let Some(dt) = dt else {
                    continue;
                };
                if Id(t[0]).tag() == sparkles_core::id::Tag::Local {
                    let Some(s) = terms.term(t[0]) else {
                        continue;
                    };
                    found.extend(LiteralTypings::violation(
                        &s,
                        &ty_term,
                        &Term::NamedNode(dt.clone()),
                    ));
                } else {
                    pending.push((t[0], t[2]));
                }
            }
            Some(terms.snap.clone())
        };
        // the saved files are read without the lock
        let Some(snap) = snap else {
            return saved(store, commit);
        };
        pending.sort_unstable();
        for (s, dt) in pending {
            let (Some(s), Some(Some(dt))) = (snap.term(Id(s)), datatypes.get(&dt)) else {
                continue;
            };
            found.extend(LiteralTypings::violation(
                &s,
                &ty_term,
                &Term::NamedNode(dt.clone()),
            ));
        }
        return Ok(Some(LiteralTypings {
            commit,
            facts: found,
        }));
    }
    saved(store, commit)
}

/// The literal typings that a persistent dataset saved for `commit`.
fn saved(store: &Store, commit: u64) -> anyhow::Result<Option<LiteralTypings>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    Ok(saved_typings(root, store.dataset_id(), commit)
        .map(|facts| LiteralTypings { commit, facts }))
}

/// The literal typings among the generalized facts a persistent dataset saved for the
/// materialization at `commit`, whatever its rules, read in one pass over the saved
/// files that keeps only the typings. `None` when no consistent state is saved for that
/// commit.
fn saved_typings(
    root: &Path,
    dataset_id: uuid::Uuid,
    commit: u64,
) -> Option<Vec<(Literal, NamedNode)>> {
    let meta = saved_meta(root, dataset_id, commit)?;
    let ty_key = sparkles_core::id::iri_key(rdf::TYPE.as_str());
    let ty_term = Term::NamedNode(rdf::TYPE.into_owned());
    let mut found: FxHashMap<Keys, (Literal, NamedNode)> = FxHashMap::default();
    let consistent = scan_saved(root, &meta, |added, k| {
        if !added {
            found.remove(&k);
        } else if k[1] == ty_key && k[0].first() == Some(&b'"') {
            let (s, o) = (
                sparkles_core::id::key_to_term(&k[0]),
                sparkles_core::id::key_to_term(&k[2]),
            );
            if let Some(x) = LiteralTypings::violation(&s, &ty_term, &o) {
                found.insert(k, x);
            }
        }
    });
    consistent.then(|| found.into_values().collect())
}

/// Replace a file by renaming a new one over it. Nothing is synced: after a crash, a
/// state file that is missing, torn or of another commit only makes the next run a full
/// one (the commit and the checksums are checked on reading).
fn write_atomic(root: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = root.join(format!("{name}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(&tmp, root.join(name))
}

/// Save the generalized facts of the materialization at the run's commit. With `prev`,
/// the saved state the run started from, and the run's changes, only those changes are
/// appended to the log.
pub(crate) fn save(
    store: &Store,
    receipt: &sparkles_core::commit::Receipt,
    digest: u64,
    inputs: &[GraphRef],
    terms: &Terms,
    generalized: &FxHashSet<Triple>,
    prev: Option<(&SavedMeta, &GeneralizedChanges)>,
) -> anyhow::Result<Option<SavedMeta>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let keys = |t: &Triple| -> Option<Keys> {
        Some([
            terms.key_of(t[0])?,
            terms.key_of(t[1])?,
            terms.key_of(t[2])?,
        ])
    };
    let mut meta = match prev {
        Some((m, ch)) if m.log_bytes < m.bytes.max(1 << 20) => {
            let mut rec = Vec::new();
            for (op, ts) in [(b'-', &ch.removed), (b'+', &ch.added)] {
                for k in ts.iter().filter_map(keys) {
                    rec.push(op);
                    put_keys(&mut rec, &k);
                }
            }
            let mut m = m.clone();
            if !rec.is_empty() {
                // drop what a run that never saved its metadata appended
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(false)
                    .open(root.join(SAVED_LOG))?;
                f.set_len(m.log_bytes)?;
                use std::io::Seek as _;
                f.seek(std::io::SeekFrom::End(0))?;
                f.write_all(&rec)?;
                let h = match m.log_bytes {
                    0 => FNV_START,
                    _ => u64::from_str_radix(&m.log_checksum, 16)?,
                };
                m.log_bytes += rec.len() as u64;
                m.log_checksum = format!("{:016x}", fnv(&rec, h));
            }
            m
        }
        _ => {
            let mut bin = MAGIC.to_vec();
            let n_at = bin.len();
            bin.extend(0u64.to_le_bytes());
            let mut n = 0u64;
            for k in generalized.iter().filter_map(keys) {
                put_keys(&mut bin, &k);
                n += 1;
            }
            bin[n_at..n_at + 8].copy_from_slice(&n.to_le_bytes());
            write_atomic(root, SAVED_BIN, &bin)?;
            let _ = std::fs::remove_file(root.join(SAVED_LOG));
            SavedMeta {
                dataset_id: String::new(),
                commit: 0,
                rules: String::new(),
                triples: n,
                bytes: bin.len() as u64,
                checksum: format!("{:016x}", fnv(&bin, FNV_START)),
                log_bytes: 0,
                log_checksum: String::new(),
                inputs: None,
            }
        }
    };
    meta.inputs = (inputs != [GraphRef::Default]).then(|| inputs.to_vec());
    meta.dataset_id = receipt.dataset_id.to_string();
    meta.commit = receipt.commit.seq;
    meta.rules = format!("{digest:016x}");
    write_atomic(root, SAVED_META, &serde_json::to_vec_pretty(&meta)?)?;
    Ok(Some(meta))
}

/// Remove the saved state (the inferred graph was cleared).
pub(crate) fn remove_saved(store: &Store) {
    if let Some(root) = store.root() {
        for f in [SAVED_META, SAVED_BIN, SAVED_LOG] {
            let _ = std::fs::remove_file(root.join(f));
        }
    }
}
