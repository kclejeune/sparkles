//! A compiled circuit with its state: the result it keeps current, the commit it
//! reflects, and the passes that move it (spec F13 §3.3, §3.4).

use super::budget::ViewBudget;
use super::compile::{Compiled, Kind};
use super::footprint::{Footprint, Resolved};
use super::inference::{Inference, InferredFootprint};
use super::ops::{Node, Op, PlanNode};
use super::pass::{Pass, PassStats, Vis};
use super::persist::{Reader, Writer};
use super::prov::{self, AChanges, AnnMap, Annotation, FactCache, FactKey, Provenance};
use super::terms::Terms;
use super::zset::{Changes, Row, ZSet};
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::pool::QueryPool;
use crate::sparql::viewsvc::{PublishedBytes, ViewPatterns, ViewTable, ViewVersion};
use crate::store::{ChangesOptions, Snapshot, Store};
use crate::task::Control;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use sparkles_budget::{MemoryLease, ParentHandle};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};

/// The most net changes a pass applies; more are cheaper to evaluate from scratch.
pub const MAX_PASS_CHANGES: usize = 1_000_000;

/// The net changes from one commit to a later one, as ids of the later snapshot.
pub struct DeltaSet {
    pub after: u64,
    /// the snapshot the changes lead to
    pub snapshot: Arc<Snapshot>,
    pub added: Vec<[Id; 4]>,
    pub removed: Vec<[Id; 4]>,
    /// `false` when some commit's changes are not known (a large bulk commit, a gap the
    /// change feed cannot bridge, terms the generation lacks): passes evaluate again
    pub complete: bool,
    old: OnceLock<std::result::Result<Arc<Snapshot>, String>>,
}

impl DeltaSet {
    /// The net changes after commit `after` up to the store's head.
    pub fn read(store: &Store, after: u64, control: &Control) -> DeltaSet {
        let snap = store.snapshot();
        let mut d = DeltaSet {
            after,
            snapshot: snap.clone(),
            added: Vec::new(),
            removed: Vec::new(),
            complete: true,
            old: OnceLock::new(),
        };
        if after >= snap.commit {
            return d;
        }
        let mut net: FxHashMap<[Id; 4], i64> = FxHashMap::default();
        let mut at = after;
        let o = ChangesOptions {
            max_commits: 1000,
            cancel: Some(control.cancel.flag()),
            deadline: control.deadline,
            ..Default::default()
        };
        while at < snap.commit {
            let page = match store.commit_deltas(at, &o) {
                Ok(p) => p,
                Err(_) => {
                    d.complete = false;
                    break;
                }
            };
            if page.snapshot.generation.uid != snap.generation.uid || page.commits.is_empty() {
                d.complete = false;
                break;
            }
            for c in &page.commits {
                if c.seq > snap.commit {
                    break;
                }
                if !c.complete {
                    d.complete = false;
                }
                for q in &c.removed {
                    *net.entry(*q).or_insert(0) -= 1;
                }
                for q in &c.added {
                    *net.entry(*q).or_insert(0) += 1;
                }
                at = c.seq;
            }
            if !d.complete || net.len() > MAX_PASS_CHANGES {
                d.complete = false;
                break;
            }
        }
        if d.complete {
            for (q, n) in net {
                match n.cmp(&0) {
                    std::cmp::Ordering::Greater => d.added.push(q),
                    std::cmp::Ordering::Less => d.removed.push(q),
                    std::cmp::Ordering::Equal => {}
                }
            }
        }
        d
    }

    /// The changes after commit `after` up to the store's head one commit at a time,
    /// each set leading to the snapshot of its own commit (spec F13 §3.6). The run's net
    /// changes are reversed once to give the snapshot at `after`
    /// ([`Snapshot::before_changes`]), and each commit's snapshot is built forward from
    /// the previous one as [`Steps::next`] yields it, so no history is read and at most
    /// two derived snapshots are alive. The last set leads to the head's own snapshot.
    /// `None` when some commit's changes are not known, when the commits list more than
    /// [`MAX_PASS_CHANGES`] changes together, or when a generation switch interrupts
    /// the listing: a reader then evaluates again at the head.
    pub fn steps(store: &Store, after: u64, control: &Control) -> Option<Steps> {
        let snap = store.snapshot();
        let mut commits = std::collections::VecDeque::new();
        let mut net: FxHashMap<[Id; 4], i64> = FxHashMap::default();
        let o = ChangesOptions {
            max_commits: 1000,
            cancel: Some(control.cancel.flag()),
            deadline: control.deadline,
            ..Default::default()
        };
        let mut at = after;
        let mut listed = 0usize;
        while at < snap.commit {
            let page = store.commit_deltas(at, &o).ok()?;
            if page.snapshot.generation.uid != snap.generation.uid || page.commits.is_empty() {
                return None;
            }
            for c in page.commits {
                if c.seq > snap.commit {
                    break;
                }
                if !c.complete {
                    return None;
                }
                listed += c.added.len() + c.removed.len();
                for q in &c.removed {
                    *net.entry(*q).or_insert(0) -= 1;
                }
                for q in &c.added {
                    *net.entry(*q).or_insert(0) += 1;
                }
                at = c.seq;
                commits.push_back(c);
            }
            if listed > MAX_PASS_CHANGES {
                return None;
            }
        }
        let (mut added, mut removed) = (Vec::new(), Vec::new());
        for (q, n) in net {
            match n.cmp(&0) {
                std::cmp::Ordering::Greater => added.push(q),
                std::cmp::Ordering::Less => removed.push(q),
                std::cmp::Ordering::Equal => {}
            }
        }
        let start = if commits.is_empty() {
            snap.clone()
        } else {
            Arc::new(snap.before_changes(&added, &removed, after).ok()?)
        };
        Some(Steps {
            head: snap,
            commits,
            prev: start,
            prev_seq: after,
        })
    }

    /// A set without changes that leads to `snap` (a build).
    pub fn at(snap: Arc<Snapshot>) -> DeltaSet {
        DeltaSet {
            after: snap.commit,
            snapshot: snap,
            added: Vec::new(),
            removed: Vec::new(),
            complete: true,
            old: OnceLock::new(),
        }
    }

    /// The snapshot before the changes, built on first use.
    pub fn old(&self) -> Result<Arc<Snapshot>> {
        self.old
            .get_or_init(|| {
                self.snapshot
                    .before_changes(&self.added, &self.removed, self.after)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            })
            .clone()
            .map_err(Error::Corrupt)
    }
}

/// The changes of a run of commits one commit at a time ([`DeltaSet::steps`]), in
/// commit order. Each set's `after` is the previous set's commit.
pub struct Steps {
    head: Arc<Snapshot>,
    commits: std::collections::VecDeque<crate::store::CommitDelta>,
    /// the snapshot of the latest set yielded, or at the run's start
    prev: Arc<Snapshot>,
    prev_seq: u64,
}

impl Steps {
    /// The commits left to yield.
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// The next commit's changes, leading from the previous set's snapshot to its own.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Result<DeltaSet>> {
        let c = self.commits.pop_front()?;
        // a commit's additions and removals applied forward: the snapshot that the
        // reversal of the opposite changes would give
        let snapshot = if self.commits.is_empty() {
            self.head.clone()
        } else {
            match self.prev.before_changes(&c.removed, &c.added, c.seq) {
                Ok(s) => Arc::new(s),
                Err(e) => return Some(Err(e)),
            }
        };
        let old = OnceLock::new();
        let _ = old.set(Ok(self.prev.clone()));
        let d = DeltaSet {
            after: self.prev_seq,
            snapshot: snapshot.clone(),
            added: c.added,
            removed: c.removed,
            complete: true,
            old,
        };
        self.prev = snapshot;
        self.prev_seq = c.seq;
        Some(Ok(d))
    }
}

/// What a pass did to a circuit.
pub enum Advance {
    /// the changes do not touch the circuit's footprint (or there were none)
    Skipped,
    /// the circuit's result changed by these rows (possibly none)
    Changed(Changes),
    /// the circuit was evaluated again; its result changed by these rows
    Reset(Changes),
}

/// Like [`Advance`], carrying the [`ScratchLease`](super::budget::ScratchLease) that
/// covers the returned [`Changes`] vector until the caller consumes and drops it.
pub(crate) enum ChargedAdvance {
    Skipped,
    Changed(Changes, super::budget::ScratchLease),
    Reset(Changes, super::budget::ScratchLease),
}

pub struct Circuit {
    pub kind: Kind,
    pub vars: Vec<String>,
    pub(crate) template_columns: Option<Arc<[String]>>,
    pub(crate) rebuild_patterns: Option<Arc<ViewPatterns>>,
    pub root: Node,
    pub footprint: Footprint,
    resolved: Option<Resolved>,
    pub terms: Terms,
    /// the result, with multiplicities
    pub result: ZSet,
    /// the commit the result reflects
    pub applied: u64,
    /// the snapshot of the latest pass
    pub snap: Option<Arc<Snapshot>>,
    /// whose view of the dataset the circuit reads
    pub access: Option<Arc<GraphAccess>>,
    /// counters of the latest pass
    pub last: PassStats,
    pub max_memory_bytes: Option<u64>,
    /// the query's base IRI, for expressions and evaluations
    pub base: Option<oxiri::Iri<String>>,
    /// what the dataset's queries read beyond the stored quads: the circuit is then one
    /// `recompute` node evaluated with it
    pub inference: Option<Arc<Inference>>,
    /// how the circuit's aggregates treat errors (`None`: the snapshot's reading); a
    /// runtime sets the dataset's
    pub aggregate_errors: Option<crate::sparql::AggregateErrors>,
    /// the query's pattern and dataset clause
    pub pattern: spargebra::algebra::GraphPattern,
    pub dataset: Option<spargebra::algebra::QueryDataset>,
    inferred: InferredFootprint,
    /// Whether the circuit keeps its result. A sink that consumes only the changes,
    /// such as an internal circuit's accumulator, sets it to `false`: the result then
    /// stays empty, and the operators keep only the state they need.
    pub keep_result: bool,
    /// The latest immutable shared view version, replaced whenever a pass updates the
    /// result so streaming cursors read without holding the circuit's lock.
    pub(crate) version: Option<Arc<ViewVersion>>,
    pub(crate) version_pool: Option<Arc<AtomicU64>>,
    /// the annotation the circuit keeps besides the weights (spec F13 §5)
    prov: Provenance,
    /// the annotations of the result's rows
    ann: AnnMap,
    facts: FactCache,
    /// the quads the latest build's pattern matching visited, which tests read
    #[cfg(test)]
    pub(crate) build_scanned: u64,
    /// the index estimates the latest build computed, which tests read
    #[cfg(test)]
    pub(crate) build_estimated: u64,
    /// query pool backing this circuit's accounting domain, when attached
    pool: Option<Arc<QueryPool>>,
    /// hierarchical budget for operator state, result, terms, provenance, history/tally,
    /// pass scratch, and nested SPARQL evaluation. Declared last so all payloads drop
    /// strictly before parent credit returns.
    budget: ViewBudget,
}

type LiveDiffRows = (
    Arc<[super::live::LiveRow]>,
    Arc<[super::live::LiveRow]>,
    super::budget::ScratchLease,
);

pub(crate) fn reserve_vec_slot<T>(
    rows: &Vec<T>,
    lease: &mut super::budget::ScratchLease,
) -> Result<()> {
    let slot = std::mem::size_of::<T>();
    let cap = sparkles_budget::sizes::vec_grown(rows.capacity(), rows.len(), 1, slot);
    lease.try_reserve((cap.saturating_sub(rows.capacity()) * slot) as u64)
}

pub(crate) fn into_arc_rows<T>(
    rows: Vec<T>,
    lease: &mut super::budget::ScratchLease,
) -> Result<Arc<[T]>> {
    let slot = std::mem::size_of::<T>() as u64;
    let old_bytes = rows.capacity() as u64 * slot;
    lease.try_reserve(rows.len() as u64 * slot)?;
    let rows = rows.into();
    lease.release(old_bytes);
    Ok(rows)
}

/// Approximate heap bytes of one decoded term row.
pub(crate) fn decoded_row_bytes(row: &[Option<Term>]) -> u64 {
    fn term_bytes(t: &Term) -> u64 {
        match t {
            Term::NamedNode(n) => n.as_str().len() as u64,
            Term::BlankNode(b) => b.as_str().len() as u64,
            Term::Literal(l) => {
                let lang_or_datatype = match l.language() {
                    Some(lang) => lang.len(),
                    None if l.datatype() == oxrdf::vocab::xsd::STRING => 0,
                    None => l.datatype().as_str().len(),
                };
                (l.value().len() + lang_or_datatype) as u64
            }
            Term::Triple(tr) => {
                let subject = match &tr.subject {
                    oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                    oxrdf::NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
                };
                (std::mem::size_of::<oxrdf::Triple>() + subject + tr.predicate.as_str().len())
                    as u64
                    + term_bytes(&tr.object)
            }
        }
    }
    let slots = std::mem::size_of_val(row) as u64;
    slots.saturating_add(row.iter().flatten().map(term_bytes).sum())
}

/// Consolidate `c` while pre-admitting the intermediate `ZSet` and compacted output
/// buffer, and reconcile `out_lease` to the returned `Changes` vector's exact size.
pub(crate) fn consolidate_budgeted(
    c: Changes,
    width: usize,
    budget: Option<super::budget::BudgetHandle>,
    out_lease: &mut super::budget::ScratchLease,
) -> Result<Changes> {
    let slot = std::mem::size_of::<(Row, i64)>() as u64;
    let row = (width as u64) * 8;
    let mut c = c;
    if c.len() <= 1 {
        c.retain(|(_, w)| *w != 0);
        if c.is_empty() {
            drop(c);
            out_lease.release(out_lease.bytes());
            return Ok(Changes::new());
        }
        out_lease.try_set(c.capacity() as u64 * slot + row)?;
        return Ok(c);
    }

    out_lease.try_set(c.capacity() as u64 * slot + c.len() as u64 * row)?;
    let mut map_lease = super::budget::ScratchLease::new(budget);
    let mut z = ZSet::new();
    let mut map_slots = 0u64;
    for (r, w) in c {
        if w != 0 && z.get(&r) == 0 {
            let slots = (z.len() as u64 + 1) * (super::zset::row_bytes(width) - row);
            if slots > map_slots {
                map_lease.try_reserve(slots - map_slots)?;
                map_slots = slots;
            }
            out_lease.transfer_to(&mut map_lease, row);
            z.add(r, w);
        } else {
            let (old, new) = z.add(r, w);
            out_lease.release(row);
            if old != 0 && new == 0 {
                map_lease.release(row);
            }
        }
    }
    // The source vector is consumed. Only new vector slots need admission; the map
    // moves its existing row payloads into them.
    out_lease.release(out_lease.bytes());
    finish_consolidation(z, row, &mut map_lease, out_lease)
}

fn finish_consolidation(
    z: ZSet,
    row_bytes: u64,
    map_lease: &mut super::budget::ScratchLease,
    out_lease: &mut super::budget::ScratchLease,
) -> Result<Changes> {
    let n = z.len() as u64;
    out_lease.try_reserve(n * std::mem::size_of::<(Row, i64)>() as u64)?;
    map_lease.transfer_to(out_lease, n * row_bytes);
    let out = z.into_changes();
    Ok(out)
}

/// Split annotated changes into consolidated `(Changes, w_lease)` and `(AChanges, a_lease)`
/// without unbudgeted cloning or consolidation.
fn split_budgeted(
    c: AChanges,
    width: usize,
    budget: Option<super::budget::BudgetHandle>,
    a_lease: super::budget::ScratchLease,
) -> Result<(
    Changes,
    super::budget::ScratchLease,
    AChanges,
    super::budget::ScratchLease,
)> {
    let row_sz = (width as u64) * 8;
    let r_bytes = super::zset::row_bytes(width);
    let mut map_lease = super::budget::ScratchLease::new(budget.clone());
    let mut z = ZSet::new();
    for (r, w, _) in &c {
        if *w == 0 {
            continue;
        }
        if z.get(r) == 0 {
            let need = (z.len() as u64 + 1).saturating_mul(r_bytes);
            if need > map_lease.bytes() {
                map_lease.try_set(need)?;
            }
        }
        z.add_borrowed(r, *w);
    }
    let mut w_lease = super::budget::ScratchLease::new(budget);
    let out = finish_consolidation(z, row_sz, &mut map_lease, &mut w_lease)?;
    Ok((out, w_lease, c, a_lease))
}

impl Circuit {
    /// A circuit of `c` read with `access`. Guards that a request decides at its
    /// snapshot keep their graphs out, since a circuit cannot decide them again at each
    /// commit.
    pub fn new(c: Compiled, access: Option<Arc<GraphAccess>>) -> Circuit {
        let access = access.map(|a| a.settled());
        let template_columns = if let Op::Distinct(distinct) = &c.root.op
            && let Op::Template(template) = &distinct.inner.op
            && template.has_blanks()
        {
            Some(
                template
                    .inner
                    .schema
                    .iter()
                    .map(|&i| c.vars[i as usize].clone())
                    .collect(),
            )
        } else {
            None
        };
        Circuit {
            kind: c.kind,
            template_columns,
            rebuild_patterns: (!c.rebuild_patterns.native.is_empty())
                .then(|| Arc::new(c.rebuild_patterns)),
            vars: c.vars,
            root: c.root,
            footprint: c.footprint,
            resolved: None,
            terms: Terms::default(),
            result: ZSet::new(),
            applied: 0,
            snap: None,
            access,
            last: PassStats::default(),
            max_memory_bytes: None,
            base: c.base,
            inference: None,
            aggregate_errors: None,
            pattern: c.pattern,
            dataset: c.dataset,
            inferred: InferredFootprint::default(),
            keep_result: true,
            version: None,
            version_pool: None,
            prov: Provenance::None,
            ann: AnnMap::default(),
            facts: FactCache::default(),
            #[cfg(test)]
            build_scanned: 0,
            #[cfg(test)]
            build_estimated: 0,
            pool: None,
            budget: ViewBudget::new(None, None),
        }
    }

    /// Bind this circuit's accounting to `pool` and `parent` before any state is built.
    pub fn set_pool(&mut self, pool: Option<Arc<QueryPool>>, parent: Option<ParentHandle>) {
        self.pool = pool;
        self.budget.set_parent(parent);
    }

    /// Set the circuit's local byte limit (`None`: unlimited locally).
    pub(crate) fn set_local_limit(&mut self, limit: Option<u64>) {
        self.max_memory_bytes = limit;
        self.budget.set_local_limit(limit);
    }

    pub(crate) fn local_limit(&self) -> Option<u64> {
        self.budget.local_limit()
    }

    pub(crate) fn budget_handle(&self) -> super::budget::BudgetHandle {
        self.budget.handle()
    }

    pub(crate) fn scratch_lease(&self) -> super::budget::ScratchLease {
        self.budget.scratch_lease()
    }

    pub(crate) fn detach_scratch_lease(
        &self,
        lease: &mut super::budget::ScratchLease,
    ) -> Option<MemoryLease> {
        self.budget.detach_scratch_lease(lease)
    }

    pub(crate) fn set_reclaim_external(&self, hook: Option<super::budget::ReclaimHook>) {
        self.budget.set_reclaim_external(hook);
    }

    /// Drain excess [`MemoryLease`]s split during shrinks so the caller can drop them
    /// outside runtime and cell locks.
    pub(crate) fn drain_retired(&self) -> Vec<MemoryLease> {
        self.budget.drain_retired()
    }

    /// Free all operator, result, term, provenance and snapshot state and detach all
    /// persistent state credit down to zero (when no external/scratch bytes remain).
    pub(crate) fn clear_state(&mut self) {
        self.root.clear();
        self.result = ZSet::new();
        self.terms = Terms::default();
        self.ann = AnnMap::default();
        self.facts.clear();
        self.resolved = None;
        self.inferred = InferredFootprint::default();
        self.snap = None;
        self.version = None;
        self.budget.clear_state();
    }

    /// Transfer this circuit's pool and live budget to a replacement circuit during an
    /// inference switch without a transient 2× reservation peak.
    pub(crate) fn take_budget(&mut self) -> (Option<Arc<QueryPool>>, ViewBudget) {
        (self.pool.take(), std::mem::take(&mut self.budget))
    }

    /// Install a transferred pool and budget onto a fresh replacement circuit.
    pub(crate) fn install_budget(&mut self, pool: Option<Arc<QueryPool>>, budget: ViewBudget) {
        self.pool = pool;
        self.budget = budget;
        self.budget.set_local_limit(self.max_memory_bytes);
    }

    /// Reconcile a transferred budget downward after the old circuit's operators were
    /// detached (caller drops the old circuit strictly before dropping retired leases).
    pub(crate) fn shrink_budget_state(&self) {
        self.budget.shrink_state_to(self.bytes());
    }

    pub(crate) fn try_set_external(&self, bytes: u64) -> Result<()> {
        self.budget.set_local_limit(self.max_memory_bytes);
        self.budget.try_set_external(bytes)
    }

    pub(crate) fn commit_external_from_scratch(
        &self,
        bytes: u64,
        lease: &mut super::budget::ScratchLease,
    ) -> Result<()> {
        self.budget.set_local_limit(self.max_memory_bytes);
        self.budget.commit_external_from_scratch(bytes, lease)
    }

    pub(crate) fn set_external_best_effort(&self, bytes: u64) {
        self.budget.set_external_best_effort(bytes);
    }

    /// Keep annotation `mode` from the next build on (spec F13 §5). The circuit must
    /// be built after this, and a state of another setting is not loaded.
    pub fn set_provenance(&mut self, mode: Provenance) -> Result<()> {
        mode.check()?;
        self.prov = mode;
        self.ann.clear();
        self.facts.clear();
        let mut next = 0;
        self.root.enable_prov(mode, &mut next);
        Ok(())
    }

    pub fn provenance(&self) -> Provenance {
        self.prov
    }

    /// Whether the circuit has a fixed-point operator (spec F13 §4.3), whose state its
    /// checkpoints hold in format 2.
    pub fn has_fixpoint(&self) -> bool {
        self.root.has_fixpoint()
    }

    /// The annotation of a row of the result, as a reader sees it.
    pub fn annotation(&self, row: &[Id]) -> Option<Annotation> {
        if self.prov.is_none() {
            return None;
        }
        let max = self.prov.max_witnesses();
        Some(
            self.ann
                .get(row)
                .map_or_else(|| prov::Ann::zero(self.prov).report(max), |a| a.report(max)),
        )
    }

    /// The whole result as terms with each row's annotation (`None` without
    /// provenance).
    pub fn annotated_rows(&self) -> Vec<(Vec<Option<Term>>, i64, Option<Annotation>)> {
        let Some(snap) = &self.snap else {
            return Vec::new();
        };
        self.result
            .iter()
            .map(|(r, w)| {
                (
                    r.iter().map(|&i| self.terms.term(snap, i)).collect(),
                    w,
                    self.annotation(r),
                )
            })
            .collect()
    }

    /// The result's rows with their annotations as the state keeps them, for tests.
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    pub(crate) fn raw_annotations(&self) -> Vec<(Vec<Option<Term>>, i64, Option<prov::Ann>)> {
        let Some(snap) = &self.snap else {
            return Vec::new();
        };
        self.result
            .iter()
            .map(|(r, w)| {
                (
                    r.iter().map(|&i| self.terms.term(snap, i)).collect(),
                    w,
                    (!self.prov.is_none()).then(|| prov::get(&self.ann, r, self.prov)),
                )
            })
            .collect()
    }

    /// The negative conditions that witnesses name ([`prov::Absence::node`]): each
    /// number with its operator, the variables it binds and the pattern that must have
    /// no match.
    pub fn conditions(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        if !self.prov.is_none() {
            self.root.conditions(&self.vars, &mut out);
        }
        out
    }

    /// The quads that fact keys name, read through the circuit's snapshot and access:
    /// each key with its subject, predicate, object and graph (`None`: the default
    /// graph), or `None` when the snapshot holds no quad of the circuit's patterns with
    /// that key, such as a quad removed since.
    #[allow(clippy::type_complexity)]
    pub fn resolve_facts(
        &self,
        keys: &[FactKey],
        control: &Control,
    ) -> Result<Vec<(FactKey, Option<([Term; 3], Option<Term>)>)>> {
        self.resolve_scan(keys, control, &mut 0)
    }

    /// [`resolve_facts`](Self::resolve_facts) with the number of quads its scans
    /// visited, which tests read.
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    pub(crate) fn resolve_counted(
        &self,
        keys: &[FactKey],
        control: &Control,
    ) -> (
        Result<Vec<(FactKey, Option<([Term; 3], Option<Term>)>)>>,
        u64,
    ) {
        let mut scanned = 0;
        let r = self.resolve_scan(keys, control, &mut scanned);
        (r, scanned)
    }

    #[allow(clippy::type_complexity)]
    fn resolve_scan(
        &self,
        keys: &[FactKey],
        control: &Control,
        scanned: &mut u64,
    ) -> Result<Vec<(FactKey, Option<([Term; 3], Option<Term>)>)>> {
        let mut found: FxHashMap<FactKey, [Id; 4]> = FxHashMap::default();
        let Some(snap) = self.snap.clone() else {
            return Ok(keys.iter().map(|k| (*k, None)).collect());
        };
        let wanted: rustc_hash::FxHashSet<FactKey> = keys.iter().copied().collect();
        let mut terms = self.terms.clone();
        let vis = Vis::new(self.access.as_ref(), &snap);
        let p = Pass::new(
            snap.clone(),
            None,
            &[],
            &[],
            &mut terms,
            &vis,
            self.access.clone(),
            control,
            &self.vars,
        );
        // quads are hashed as the scan visits them, and the scan stops once every key
        // is found
        let mut seen = 0u64;
        for b in self.root.bgps() {
            if found.len() == wanted.len() {
                break;
            }
            let r = b.visit(&p, super::pass::Which::New, &mut |q| {
                seen += 1;
                if seen.is_multiple_of(1024) {
                    control.check()?;
                }
                let k = p.fact_key_of(&q);
                if wanted.contains(&k) {
                    found.insert(k, q);
                }
                Ok(found.len() < wanted.len())
            });
            #[cfg(test)]
            {
                *scanned = p.scanned.get();
            }
            r?;
        }
        let _ = &scanned;
        drop(p);
        let term = |i: Id| self.terms.term(&snap, i);
        Ok(keys
            .iter()
            .map(|k| {
                let q = found.get(k).and_then(|q| {
                    let g = if q[3] == Id::DEFAULT_GRAPH {
                        None
                    } else {
                        Some(term(q[3])?)
                    };
                    Some(([term(q[0])?, term(q[1])?, term(q[2])?], g))
                });
                (*k, q)
            })
            .collect())
    }

    /// The names of the result's columns.
    pub fn columns(&self) -> Vec<String> {
        self.root
            .schema
            .iter()
            .map(|&v| self.vars[v as usize].clone())
            .collect()
    }

    pub fn plan(&self) -> PlanNode {
        self.root.plan(&self.vars)
    }

    /// The approximate memory of the state and the result.
    pub fn bytes(&self) -> u64 {
        self.root.bytes()
            + self.result.bytes(self.root.schema.len())
            + self.terms.bytes()
            + self.version.as_ref().map_or(0, |v| {
                v.retained_bytes()
                    .saturating_sub(v.terms.key_storage_bytes())
            })
            + prov::map_bytes(&self.ann, self.root.schema.len())
            + self.facts.bytes()
    }

    /// Build a flat immutable [`ViewTable`] from the current result (in window order when
    /// the query orders it).
    fn build_view_table(&self) -> ViewTable {
        let width = self.root.schema.len();
        if let Some((w, cols)) = self.root.result_window()
            && w.positions
        {
            let rows = w.rows();
            let mut ids = Vec::with_capacity(rows.len() * width);
            let mut weights = Vec::with_capacity(rows.len());
            let mut total_rows = 0u64;
            for (r, n) in rows {
                if n > 0 {
                    match cols {
                        None => ids.extend_from_slice(&r),
                        Some(cols) => {
                            ids.extend(cols.iter().map(|c| c.map_or(Id::UNDEF, |i| r[i])));
                        }
                    }
                    weights.push(n);
                    total_rows = total_rows.saturating_add(n as u64);
                }
            }
            return ViewTable {
                width,
                ids,
                weights,
                total_rows,
                published: None,
            };
        }
        let mut ids = Vec::with_capacity(self.result.len() * width);
        let mut weights = Vec::with_capacity(self.result.len());
        let mut total_rows = 0u64;
        for (r, w) in self.result.iter() {
            if w > 0 {
                ids.extend_from_slice(r);
                weights.push(w);
                total_rows = total_rows.saturating_add(w as u64);
            }
        }
        ViewTable {
            width,
            ids,
            weights,
            total_rows,
            published: None,
        }
    }

    fn publish_fresh_version(&mut self) {
        self.publish_version(false);
    }

    fn publish_updated_version(&mut self, remapped: bool) {
        self.publish_version(!remapped);
    }

    fn publish_version(&mut self, reuse_terms: bool) {
        let Some(snap) = self.snap.clone().filter(|_| self.keep_result) else {
            self.version = None;
            return;
        };
        let pool = self.version_pool.as_ref();
        let (columns, published_columns) = if let Some(prev) = &self.version {
            (prev.columns.clone(), prev.published_columns.clone())
        } else {
            let columns: Arc<[String]> = self.columns().into();
            let bytes = columns.len() * std::mem::size_of::<String>()
                + columns.iter().map(String::capacity).sum::<usize>()
                + 32
                + std::mem::size_of::<PublishedBytes>();
            (
                columns,
                pool.map(|p| Arc::new(PublishedBytes::new(p, bytes as u64))),
            )
        };
        let terms = match self.version.as_ref() {
            Some(prev) if reuse_terms && prev.terms.len() == self.terms.key_count() => {
                prev.terms.clone()
            }
            _ => {
                let mut terms = self.terms.snapshot_in(pool);
                terms.published = pool.map(|p| PublishedBytes::new(p, terms.table_bytes() + 16));
                Arc::new(terms)
            }
        };
        let mut table = self.build_view_table();
        table.published = pool.map(|p| PublishedBytes::new(p, table.retained_bytes() + 16));
        let published = self.version_charge();
        self.version = Some(Arc::new(ViewVersion {
            columns,
            commit: self.applied,
            snap,
            terms,
            table: Arc::new(table),
            evaluated_under: None,
            published,
            published_columns,
        }));
    }

    fn version_charge(&self) -> Option<Arc<PublishedBytes>> {
        self.version_pool.as_ref().map(|p| {
            Arc::new(PublishedBytes::new(
                p,
                (std::mem::size_of::<ViewVersion>()
                    + 16
                    + std::mem::size_of::<PublishedBytes>()
                    + 16) as u64,
            ))
        })
    }

    pub(crate) fn set_version_pool(&mut self, pool: Option<Arc<AtomicU64>>) {
        self.version_pool = pool;
        self.version = None;
        if self.version_pool.is_some() && self.snap.is_some() {
            self.publish_fresh_version();
        }
    }

    /// Advance the version's commit and snapshot without copying rows or terms.
    fn bump_version(&mut self, commit: u64, snap: Arc<Snapshot>) {
        if !self.keep_result {
            self.version = None;
            return;
        }
        if let Some(prev) = self.version.as_ref() {
            let mut version = (**prev).clone();
            version.commit = commit;
            version.snap = snap;
            version.published = self.version_charge();
            self.version = Some(Arc::new(version));
        } else {
            self.publish_fresh_version();
        }
    }

    /// Update the published commit after checkpoint recovery.
    pub(crate) fn set_applied(&mut self, commit: u64) {
        self.applied = commit;
        if let Some(snap) = &self.snap
            && self
                .version
                .as_ref()
                .is_none_or(|version| version.commit != commit)
        {
            self.bump_version(commit, snap.clone());
        }
    }

    fn pass<'a>(
        &'a mut self,
        snap: Arc<Snapshot>,
        old: Option<Arc<Snapshot>>,
        added: &'a [[Id; 4]],
        removed: &'a [[Id; 4]],
        vis: &'a Vis,
        control: &'a Control,
    ) -> (Pass<'a>, &'a mut Node) {
        self.budget.set_local_limit(self.max_memory_bytes);
        let budget_handle = self.budget.handle();
        let pool = self.pool.clone();
        let mut p = Pass::new(
            snap,
            old,
            added,
            removed,
            &mut self.terms,
            vis,
            self.access.clone(),
            control,
            &self.vars,
        );
        p.max_memory_bytes = self.max_memory_bytes;
        p.budget = Some(budget_handle);
        p.pool = pool;
        p.base = self.base.clone();
        p.inference = self.inference.clone();
        if let Some(m) = self.aggregate_errors {
            p.aggregate_errors = m;
        }
        p.prov = self.prov;
        if !self.prov.is_none() {
            p.facts = Some(&mut self.facts);
        }
        (p, &mut self.root)
    }

    /// Run the root's delta, with annotations when the circuit keeps them, pre-admitting
    /// consolidation and returning leases reconciled to the returned buffers.
    #[allow(clippy::type_complexity)]
    fn run_delta(
        root: &mut Node,
        p: &mut Pass,
    ) -> Result<(
        Changes,
        super::budget::ScratchLease,
        Option<(AChanges, super::budget::ScratchLease)>,
    )> {
        let width = root.schema.len();
        let budget = p.budget_handle();
        if p.prov.is_none() {
            let raw = root.delta(p)?;
            let mut out_lease = p.take_output_lease();
            let out = consolidate_budgeted(raw, width, budget, &mut out_lease)?;
            return Ok((out, out_lease, None));
        }
        let raw = root.delta_a(p)?;
        let raw_lease = p.take_output_lease();
        let (w, w_lease, a, a_lease) = split_budgeted(raw, width, budget, raw_lease)?;
        Ok((w, w_lease, Some((a, a_lease))))
    }

    /// Apply `out` into `self.result` while pre-admitting persistent Z-set growth via
    /// per-owner `pass_bytes` before cloning new rows.
    fn apply_result_budgeted(&mut self, out: &Changes) -> Result<()> {
        if !self.keep_result {
            return Ok(());
        }
        let width = self.root.schema.len();
        let r_bytes = super::zset::row_bytes(width);
        let base = self.result.bytes(width);
        let mut pass_reserved = 0u64;
        let handle = self.budget.handle();
        for (r, w) in out {
            if *w == 0 {
                continue;
            }
            if self.result.get(r) == 0 {
                let target = (self.result.len() as u64 + 1).saturating_mul(r_bytes);
                let needed = target.saturating_sub(base);
                if needed > pass_reserved {
                    handle.try_reserve_pass(needed - pass_reserved)?;
                    pass_reserved = needed;
                }
            }
            self.result.add_borrowed(r, *w);
            let needed = self.result.bytes(width).saturating_sub(base);
            if needed < pass_reserved {
                handle.release_pass(pass_reserved - needed);
                pass_reserved = needed;
            }
        }
        Ok(())
    }

    /// Move build output rows into `self.result`, transferring moved row payload credit
    /// from `out_lease` into `pass_bytes` and pre-admitting hash-table slot growth.
    fn install_build_result_budgeted(
        &mut self,
        out: Changes,
        out_lease: &mut super::budget::ScratchLease,
    ) -> Result<()> {
        if !self.keep_result {
            drop(out);
            return Ok(());
        }
        let width = self.root.schema.len();
        let r_bytes = super::zset::row_bytes(width);
        let row_payload = (width as u64) * 8;
        let handle = self.budget.handle();
        let mut pass_reserved = 0u64;
        let mut z = ZSet::new();
        for (r, w) in out {
            if w == 0 {
                continue;
            }
            if z.get(&r) == 0 {
                let needed = (z.len() as u64 + 1).saturating_mul(r_bytes);
                if needed > pass_reserved {
                    let delta = needed - pass_reserved;
                    let moved = handle.move_scratch_to_pass(out_lease, delta.min(row_payload));
                    let rem = delta - moved;
                    if rem > 0 {
                        handle.try_reserve_pass(rem)?;
                    }
                    pass_reserved = needed;
                }
            }
            z.add(r, w);
            let cur = z.bytes(width);
            if cur < pass_reserved {
                handle.release_pass(pass_reserved - cur);
                pass_reserved = cur;
            }
        }
        self.result = z;
        Ok(())
    }

    /// Add the annotations' changes to the result's, pre-admitting retained annotation
    /// map growth before mutating `self.ann`.
    fn absorb_budgeted(&mut self, a: Option<AChanges>) -> Result<()> {
        let Some(a) = a else {
            return Ok(());
        };
        if !self.keep_result {
            return Ok(());
        }
        let width = self.root.schema.len();
        let r_bytes = super::zset::row_bytes(width);
        let base = prov::map_bytes(&self.ann, width);
        let mut pass_reserved = 0u64;
        let handle = self.budget.handle();
        for (r, _, d) in a {
            let extra = d.bytes().saturating_add(if self.ann.contains_key(&r) {
                0
            } else {
                r_bytes
            });
            let target = prov::map_bytes(&self.ann, width).saturating_add(extra);
            let needed = target.saturating_sub(base);
            if needed > pass_reserved {
                handle.try_reserve_pass(needed - pass_reserved)?;
                pass_reserved = needed;
            }
            prov::absorb_row_state(&mut self.ann, &r, d, self.prov);
            let needed = prov::map_bytes(&self.ann, width).saturating_sub(base);
            if needed < pass_reserved {
                handle.release_pass(pass_reserved - needed);
                pass_reserved = needed;
            }
        }
        Ok(())
    }

    /// Evaluate the circuit from scratch at `snap`.
    pub fn build(&mut self, snap: Arc<Snapshot>, control: &Control) -> Result<()> {
        self.clear_moves();
        self.root.clear();
        self.result = ZSet::new();
        self.terms = Terms::default();
        self.ann = AnnMap::default();
        self.facts.clear();
        // the schema may have changed in changes no pass saw
        self.inferred = InferredFootprint::default();
        self.budget.shrink_state_to(0);
        let vis = Vis::new(self.access.as_ref(), &snap);
        let (mut p, root) = self.pass(snap.clone(), None, &[], &[], &vis, control);
        let width = root.schema.len();
        let budget = p.budget_handle();
        let res = if p.prov.is_none() {
            root.init(&mut p).map(|out| {
                let out_lease = p.take_output_lease();
                (out, out_lease, None)
            })
        } else {
            root.init_a(&mut p).and_then(|a| {
                let raw_lease = p.take_output_lease();
                let (w, w_lease, a, a_lease) = split_budgeted(a, width, budget, raw_lease)?;
                Ok((w, w_lease, Some((a, a_lease))))
            })
        };
        let stats = p.stats;
        #[cfg(test)]
        let (scanned, estimated) = (p.scanned.get(), p.estimated.get());
        drop(p);
        let (out, mut out_lease, ann) = match res {
            Ok(v) => v,
            Err(e) => {
                self.clear_state();
                return Err(e);
            }
        };
        #[cfg(test)]
        {
            self.build_scanned = scanned;
            self.build_estimated = estimated;
        }
        if let Err(e) = self.install_build_result_budgeted(out, &mut out_lease) {
            drop(ann);
            self.clear_state();
            drop(out_lease);
            return Err(e);
        }
        self.ann.clear();
        if let Some((a, a_lease)) = ann {
            let res = self.absorb_budgeted(Some(a));
            drop(a_lease);
            if let Err(e) = res {
                self.clear_state();
                drop(out_lease);
                return Err(e);
            }
        }
        if let Err(e) = self
            .budget
            .commit_pass_with_scratch(self.bytes(), &mut out_lease)
        {
            self.clear_state();
            drop(out_lease);
            return Err(e);
        }
        self.applied = snap.commit;
        self.snap = Some(snap);
        self.last = stats;
        self.publish_fresh_version();
        Ok(())
    }

    /// Move the state to the id space of `to` (a new generation, or terms the store
    /// now holds that the circuit held as local ids), through the checkpoint encoding.
    fn remap(&mut self, from: &Arc<Snapshot>, to: &Arc<Snapshot>) -> Result<()> {
        let body = self.encode(from)?;
        self.decode(&body, to)
    }

    /// The state as checkpoint bytes, its terms read from `snap`.
    pub fn encode(&self, snap: &Snapshot) -> Result<Vec<u8>> {
        let mut w = Writer::new(snap, &self.terms);
        w.zset(&self.result)?;
        self.root.save(&mut w)?;
        if !self.prov.is_none() {
            prov::save_map(&mut w, &self.ann)?;
        }
        Ok(w.finish())
    }

    /// Load a state written by [`encode`](Self::encode) into the id space of `snap`.
    pub fn decode(&mut self, body: &[u8], snap: &Arc<Snapshot>) -> Result<()> {
        self.budget.set_local_limit(self.max_memory_bytes);
        let mut body_lease = self.budget.scratch_lease();
        body_lease.try_reserve(body.len() as u64)?;
        let res = self.decode_with_input_charged(body, snap);
        drop(body_lease);
        res
    }

    /// Like [`decode`](Self::decode), when the caller already holds a [`ScratchLease`]
    /// covering `body` for its lifetime (such as a pre-admitted checkpoint file buffer).
    pub(crate) fn decode_with_input_charged(
        &mut self,
        body: &[u8],
        snap: &Arc<Snapshot>,
    ) -> Result<()> {
        self.budget.set_local_limit(self.max_memory_bytes);
        self.root.clear();
        self.result = ZSet::new();
        self.terms = Terms::default();
        self.ann = AnnMap::default();
        self.facts.clear();
        self.budget.shrink_state_to(0);
        let budget_handle = self.budget.handle();
        let decoded = (|| {
            let mut terms = Terms::default();
            let mut r = Reader::with_budget(body, snap, &mut terms, Some(budget_handle))?;
            self.result = r.zset()?;
            self.root.load(&mut r)?;
            if !self.prov.is_none() {
                self.ann = prov::load_map(&mut r, self.prov)?;
            }
            // ids change with the id space
            self.facts.clear();
            if !r.done() {
                return Err(Error::Corrupt("a view's state has trailing bytes".into()));
            }
            self.terms = terms;
            Ok(())
        })();
        if let Err(e) = decoded {
            self.clear_state();
            return Err(e);
        }
        if let Err(e) = self.budget.commit_pass(self.bytes()) {
            self.clear_state();
            return Err(e);
        }
        self.snap = Some(snap.clone());
        self.resolved = None;
        self.inferred = InferredFootprint::default();
        self.publish_fresh_version();
        Ok(())
    }

    /// Evaluate again at `d.snapshot` and return the change of the result.
    pub fn reset(&mut self, d: &DeltaSet, control: &Control) -> Result<Changes> {
        let (diff, lease) = self.reset_charged(d, control)?;
        drop(lease);
        Ok(diff)
    }

    /// Like [`reset`](Self::reset), returning the [`ScratchLease`] covering the diff
    /// vector so the caller keeps it charged while consuming it.
    pub(crate) fn reset_charged(
        &mut self,
        d: &DeltaSet,
        control: &Control,
    ) -> Result<(Changes, super::budget::ScratchLease)> {
        let snap = d.snapshot.clone();
        if !self.keep_result {
            // no result to compare with: the change is not known
            self.build(snap, control)?;
            return Ok((Vec::new(), self.budget.scratch_lease()));
        }
        // the old result in the new id space, to compare with
        if let Some(prev) = self.snap.clone()
            && (prev.generation.uid != snap.generation.uid || !self.terms.is_empty())
        {
            self.remap(&prev, &snap)?;
        }
        self.budget.set_local_limit(self.max_memory_bytes);
        let width = self.root.schema.len();
        let before_bytes = self.result.bytes(width).saturating_add(self.terms.bytes());
        // Declare `before_lease` before `before` and `old_terms` so Rust drops payloads
        // strictly before releasing their scratch credit on every exit path.
        let before_lease = self.budget.move_state_to_scratch(before_bytes)?;
        let before = std::mem::take(&mut self.result);
        let old_terms = std::mem::take(&mut self.terms);
        self.build(snap.clone(), control)?;
        let mut diff_lease = self.budget.scratch_lease();
        let diff = if old_terms.is_empty() {
            let diff = Self::minus_budgeted(&self.result, &before, width, &mut diff_lease)?;
            drop(old_terms);
            drop(before);
            drop(before_lease);
            diff
        } else {
            // local ids of the old result must mean what the new ones mean
            let mut remapped_lease = self.budget.scratch_lease();
            let mut before_keys = ZSet::new();
            for (idx, (r, w)) in before.iter().enumerate() {
                let row: Row = r
                    .iter()
                    .map(|&i| {
                        if i.tag() == crate::id::Tag::Local {
                            old_terms
                                .term(&snap, i)
                                .map_or(Id::UNDEF, |t| self.terms.intern(&snap, &t))
                        } else {
                            i
                        }
                    })
                    .collect();
                let next_bytes =
                    (before_keys.len() as u64 + 1).saturating_mul(super::zset::row_bytes(width));
                if next_bytes > remapped_lease.bytes() {
                    remapped_lease.try_set(next_bytes)?;
                }
                before_keys.add(row, w);
                if idx % 1024 == 1023 {
                    self.budget.try_set_state(self.bytes())?;
                }
            }
            self.budget.try_set_state(self.bytes())?;
            drop(old_terms);
            drop(before);
            drop(before_lease);
            let diff = Self::minus_budgeted(&self.result, &before_keys, width, &mut diff_lease)?;
            drop(before_keys);
            drop(remapped_lease);
            diff
        };
        if let Err(e) = self.budget.try_set_state(self.bytes()) {
            drop(diff);
            drop(diff_lease);
            return Err(e);
        }
        Ok((diff, diff_lease))
    }

    fn minus_budgeted(
        now: &ZSet,
        before: &ZSet,
        width: usize,
        lease: &mut super::budget::ScratchLease,
    ) -> Result<Changes> {
        let mut out = Changes::new();
        let row_sz = (width as u64) * 8;
        let slot_sz = std::mem::size_of::<(Row, i64)>() as u64;
        let mut push = |out: &mut Changes, row: &Row, w: i64| -> Result<()> {
            let next_cap = sparkles_budget::sizes::vec_grown(
                out.capacity(),
                out.len(),
                1,
                std::mem::size_of::<(Row, i64)>(),
            );
            let need = (next_cap as u64)
                .saturating_mul(slot_sz)
                .saturating_add((out.len() as u64 + 1).saturating_mul(row_sz));
            if need > lease.bytes() {
                lease.try_set(need)?;
            }
            out.push((row.clone(), w));
            Ok(())
        };
        for (r, w) in now.iter() {
            let d = w - before.get(r);
            if d != 0 {
                push(&mut out, r, d)?;
            }
        }
        for (r, w) in before.iter() {
            if now.get(r) == 0 {
                push(&mut out, r, -w)?;
            }
        }
        Ok(out)
    }

    /// Apply the net changes `d` (whose `after` must be the circuit's commit).
    pub fn advance(&mut self, d: &DeltaSet, control: &Control) -> Result<Advance> {
        match self.advance_charged(d, control)? {
            ChargedAdvance::Skipped => Ok(Advance::Skipped),
            ChargedAdvance::Changed(out, lease) => {
                drop(lease);
                Ok(Advance::Changed(out))
            }
            ChargedAdvance::Reset(out, lease) => {
                drop(lease);
                Ok(Advance::Reset(out))
            }
        }
    }

    /// Apply the output of a delta pass to persistent result and annotation state while
    /// pre-admitting growth and keeping `out_lease` attached to the returned `Changes`.
    #[allow(clippy::type_complexity)]
    fn finish_delta_pass(
        &mut self,
        res: Result<(
            Changes,
            super::budget::ScratchLease,
            Option<(AChanges, super::budget::ScratchLease)>,
        )>,
        snap: Arc<Snapshot>,
        stats: PassStats,
        remapped: bool,
    ) -> Result<ChargedAdvance> {
        let (out, out_lease, ann) = match res {
            Ok(v) => v,
            Err(e) => {
                self.budget.abort_pass(self.bytes());
                return Err(e);
            }
        };
        if let Err(e) = self.apply_result_budgeted(&out) {
            drop(ann);
            drop(out);
            drop(out_lease);
            self.budget.abort_pass(self.bytes());
            return Err(e);
        }
        if let Some((a, a_lease)) = ann {
            let res = self.absorb_budgeted(Some(a));
            drop(a_lease);
            if let Err(e) = res {
                drop(out);
                drop(out_lease);
                self.budget.abort_pass(self.bytes());
                return Err(e);
            }
        }
        let commit = snap.commit;
        self.applied = commit;
        self.snap = Some(snap.clone());
        self.last = stats;
        if out.is_empty() && !remapped && !self.ordered() {
            self.bump_version(commit, snap);
        } else {
            self.publish_updated_version(remapped);
        }
        // Keep `out_lease` intact while `out` is returned to the caller.
        if let Err(e) = self.budget.commit_pass(self.bytes()) {
            drop(out);
            drop(out_lease);
            self.budget.abort_pass(self.bytes());
            return Err(e);
        }
        Ok(ChargedAdvance::Changed(out, out_lease))
    }

    /// Like [`advance`](Self::advance), keeping the returned [`Changes`] vector covered
    /// by its [`ScratchLease`] until the caller finishes consuming and drops it.
    pub(crate) fn advance_charged(
        &mut self,
        d: &DeltaSet,
        control: &Control,
    ) -> Result<ChargedAdvance> {
        self.clear_moves();
        let snap = d.snapshot.clone();
        if !d.complete {
            let (diff, lease) = self.reset_charged(d, control)?;
            return Ok(ChargedAdvance::Reset(diff, lease));
        }
        let mut remapped = false;
        if let Some(prev) = self.snap.clone() {
            if prev.generation.uid != snap.generation.uid {
                self.remap(&prev, &snap)?;
                remapped = true;
            } else if snap.dvocab_len > prev.dvocab_len
                && !self.terms.absorbed(&snap, prev.dvocab_len).is_empty()
            {
                // the store now holds terms the circuit kept as local ids
                self.remap(&prev, &snap)?;
                remapped = true;
            }
        }
        let vis = Vis::new(self.access.as_ref(), &snap);
        // protections that depend on the data make the whole query one `recompute`
        // node, and a change anywhere, even in a graph the principal cannot read, can
        // change what they hide: every change runs the pass. Inference makes the whole
        // query one `recompute` node too, which runs when the changes touch its
        // footprint over the inferred data.
        let whole = super::runtime::whole_reason(self.access.as_ref()).is_some();
        if whole || self.inference.is_some() {
            let skip = match &self.inference {
                Some(inf) if !whole => !self.inferred.touches(
                    inf,
                    &self.pattern,
                    self.dataset.is_some(),
                    &snap,
                    &d.added,
                    &d.removed,
                    &vis,
                )?,
                _ => d.added.is_empty() && d.removed.is_empty(),
            };
            if skip {
                let commit = snap.commit;
                self.applied = commit;
                self.snap = Some(snap.clone());
                if remapped {
                    self.set_applied(commit);
                } else {
                    self.bump_version(commit, snap);
                }
                return Ok(ChargedAdvance::Skipped);
            }
            let old = d.old()?;
            let (mut p, root) =
                self.pass(snap.clone(), Some(old), &d.added, &d.removed, &vis, control);
            p.full = true;
            let res = Self::run_delta(root, &mut p);
            let stats = p.stats;
            drop(p);
            return self.finish_delta_pass(res, snap, stats, remapped);
        }
        let (added, removed): (Vec<[Id; 4]>, Vec<[Id; 4]>) = if vis.everything() {
            (d.added.clone(), d.removed.clone())
        } else {
            (
                d.added
                    .iter()
                    .filter(|q| vis.visible(&snap, q))
                    .copied()
                    .collect(),
                d.removed
                    .iter()
                    .filter(|q| vis.visible(&snap, q))
                    .copied()
                    .collect(),
            )
        };
        if !self.resolved.as_ref().is_some_and(|r| r.current(&snap)) {
            self.resolved = Some(self.footprint.resolve(&snap));
        }
        let touched = self
            .resolved
            .as_ref()
            .expect("resolved above")
            .touches(added.iter().chain(removed.iter()));
        if !touched {
            let commit = snap.commit;
            self.applied = commit;
            self.snap = Some(snap.clone());
            if remapped {
                self.set_applied(commit);
            } else {
                self.bump_version(commit, snap);
            }
            return Ok(ChargedAdvance::Skipped);
        }
        let old = d.old()?;
        let (mut p, root) = self.pass(snap.clone(), Some(old), &added, &removed, &vis, control);
        let res = Self::run_delta(root, &mut p);
        let stats = p.stats;
        drop(p);
        self.finish_delta_pass(res, snap, stats, remapped)
    }

    /// Decode `rows` into terms while charging the decoded copy against this circuit's
    /// budget, and split out a standalone [`MemoryLease`] so the returned rows stay
    /// charged after this temporary circuit drops.
    #[allow(clippy::type_complexity)]
    pub(crate) fn rows_budgeted(
        &self,
    ) -> Result<(Vec<(Vec<Option<Term>>, i64)>, Option<MemoryLease>)> {
        let (rows, mut lease) = self.rows_charged()?;
        Ok((rows, self.budget.detach_scratch_lease(&mut lease)))
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn rows_charged(
        &self,
    ) -> Result<(Vec<(Vec<Option<Term>>, i64)>, super::budget::ScratchLease)> {
        let mut lease = self.budget.scratch_lease();
        let Some(snap) = &self.snap else {
            return Ok((Vec::new(), lease));
        };
        let mut out = Vec::new();
        let mut push_decoded = |cols, row: &Row, w: i64| -> Result<()> {
            reserve_vec_slot(&out, &mut lease)?;
            let decoded = self.decode_row_budgeted(snap, cols, row, &mut lease)?;
            out.push((decoded, w));
            Ok(())
        };
        if let Some((w, cols)) = self.root.result_window()
            && w.positions
        {
            w.for_each_row(|row, n| push_decoded(cols, row, n))?;
        } else {
            for (row, w) in self.result.iter() {
                push_decoded(None, row, w)?;
            }
        }
        Ok((out, lease))
    }

    /// Decode one row after admitting each term's owned representation.
    pub(crate) fn decode_row_budgeted(
        &self,
        snap: &Snapshot,
        cols: Option<&[Option<usize>]>,
        row: &Row,
        lease: &mut super::budget::ScratchLease,
    ) -> Result<Vec<Option<Term>>> {
        let width = cols.map_or(row.len(), <[_]>::len);
        lease.try_reserve((width * std::mem::size_of::<Option<Term>>()) as u64)?;
        let mut out = Vec::with_capacity(width);
        for col in 0..width {
            let id = cols
                .map_or(Some(col), |cols| cols[col])
                .map_or(Id::UNDEF, |i| row[i]);
            out.push(self.terms.term_budgeted(snap, id, lease)?);
        }
        Ok(out)
    }

    /// Number of rows in the ordered window, when this circuit is an ordered window.
    pub(crate) fn ordered_row_count(&self) -> Option<usize> {
        let (w, _) = self.root.result_window()?;
        w.positions.then(|| w.row_count())
    }

    /// Total number of removed + added move runs from the latest ordered-window pass.
    pub(crate) fn moves_row_count(&self) -> Option<usize> {
        let (w, _) = self.root.result_window()?;
        let m = w.moves.as_ref()?;
        Some(m.removed.len().saturating_add(m.added.len()))
    }

    /// Decode the circuit's current result into [`super::live::LiveRow`]s incrementally
    /// under a [`super::budget::ScratchLease`].
    pub(crate) fn live_rows_budgeted(
        &self,
    ) -> Result<(Arc<[super::live::LiveRow]>, super::budget::ScratchLease)> {
        let mut lease = self.budget.scratch_lease();
        let Some(snap) = &self.snap else {
            return Ok((Arc::from(Vec::new()), lease));
        };
        let mut out = Vec::new();
        let ordered = self.ordered();
        let mut at = 0u64;
        let mut push_row = |cols, row: &Row, w: i64| -> Result<()> {
            let position = ordered.then_some(at);
            let count = w.unsigned_abs();
            at += count;
            self.push_live_row(&mut out, (snap, cols, row), (count, position), &mut lease)
        };
        if let Some((w, cols)) = self.root.result_window()
            && w.positions
        {
            w.for_each_row(|row, n| push_row(cols, row, n))?;
        } else {
            for (row, w) in self.result.iter() {
                push_row(None, row, w)?;
            }
        }
        Ok((into_arc_rows(out, &mut lease)?, lease))
    }

    fn push_live_row(
        &self,
        out: &mut Vec<super::live::LiveRow>,
        (snap, cols, row): (&Snapshot, Option<&[Option<usize>]>, &Row),
        (count, position): (u64, Option<u64>),
        lease: &mut super::budget::ScratchLease,
    ) -> Result<()> {
        reserve_vec_slot(out, lease)?;
        let terms = self.decode_row_budgeted(snap, cols, row, lease)?;
        out.push(super::live::LiveRow {
            terms,
            count,
            position,
        });
        Ok(())
    }

    /// Decode the ordered window's latest moves with their positions.
    pub(crate) fn moves_live_budgeted(&self) -> Result<Option<LiveDiffRows>> {
        let Some(((window, cols), snap)) = self.root.result_window().zip(self.snap.as_ref()) else {
            return Ok(None);
        };
        let Some(moves) = &window.moves else {
            return Ok(None);
        };
        let mut lease = self.budget.scratch_lease();
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        for (out, rows) in [(&mut removed, &moves.removed), (&mut added, &moves.added)] {
            for (pos, row, count) in rows {
                self.push_live_row(out, (snap, cols, row), (*count, Some(*pos)), &mut lease)?;
            }
        }
        Self::finish_live_diff(removed, added, lease)
    }

    /// Decode compact unordered changes into the live event's removed and added rows.
    pub(crate) fn diff_live_budgeted(&self, c: &Changes) -> Result<Option<LiveDiffRows>> {
        let Some(snap) = &self.snap else {
            return Ok(None);
        };
        let mut lease = self.budget.scratch_lease();
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        for (row, w) in c {
            let out = match w.cmp(&0) {
                std::cmp::Ordering::Less => &mut removed,
                std::cmp::Ordering::Greater => &mut added,
                std::cmp::Ordering::Equal => continue,
            };
            self.push_live_row(out, (snap, None, row), (w.unsigned_abs(), None), &mut lease)?;
        }
        Self::finish_live_diff(removed, added, lease)
    }

    fn finish_live_diff(
        removed: Vec<super::live::LiveRow>,
        added: Vec<super::live::LiveRow>,
        mut lease: super::budget::ScratchLease,
    ) -> Result<Option<LiveDiffRows>> {
        if removed.is_empty() && added.is_empty() {
            return Ok(None);
        }
        let removed = into_arc_rows(removed, &mut lease)?;
        let added = into_arc_rows(added, &mut lease)?;
        Ok(Some((removed, added, lease)))
    }

    /// The terms of rows of the result.
    pub fn decode_rows(&self, rows: &[(Row, i64)]) -> Vec<(Vec<Option<Term>>, i64)> {
        let Some(snap) = &self.snap else {
            return Vec::new();
        };
        rows.iter()
            .map(|(r, w)| (r.iter().map(|&i| self.terms.term(snap, i)).collect(), *w))
            .collect()
    }

    /// The whole result as terms, in the window's order when the query orders it.
    pub fn rows(&self) -> Vec<(Vec<Option<Term>>, i64)> {
        if let Some(rows) = self.ordered_rows() {
            return rows;
        }
        let rows: Vec<(Row, i64)> = self.result.iter().map(|(r, w)| (r.clone(), w)).collect();
        self.decode_rows(&rows)
    }

    /// Whether the result is an ordered window whose rows have positions (spec F13
    /// §4.4).
    pub fn ordered(&self) -> bool {
        self.root.result_window().is_some_and(|(w, _)| w.positions)
    }

    fn window_row(cols: Option<&[Option<usize>]>, r: &Row) -> Row {
        match cols {
            None => r.clone(),
            Some(cols) => cols.iter().map(|c| c.map_or(Id::UNDEF, |i| r[i])).collect(),
        }
    }

    /// The rows of an ordered window in order, each with its copies.
    pub fn ordered_rows(&self) -> Option<Vec<(Vec<Option<Term>>, i64)>> {
        let (w, cols) = self.root.result_window()?;
        if !w.positions {
            return None;
        }
        let rows: Vec<(Row, i64)> = w
            .rows()
            .iter()
            .map(|(r, n)| (Self::window_row(cols, r), *n))
            .collect();
        Some(self.decode_rows(&rows))
    }

    /// The rows that entered and left an ordered window in the latest pass, each with
    /// its position and copies: `None` when the pass did not reach the window.
    #[allow(clippy::type_complexity)]
    pub fn moves(
        &self,
    ) -> Option<(
        Vec<(u64, Vec<Option<Term>>, u64)>,
        Vec<(u64, Vec<Option<Term>>, u64)>,
    )> {
        let (w, cols) = self.root.result_window()?;
        let m = w.moves.as_ref()?;
        let side = |v: &[(u64, Row, u64)]| {
            let rows: Vec<(Row, i64)> = v
                .iter()
                .map(|(_, r, n)| (Self::window_row(cols, r), *n as i64))
                .collect();
            self.decode_rows(&rows)
                .into_iter()
                .zip(v)
                .map(|((t, _), (pos, _, n))| (*pos, t, *n))
                .collect()
        };
        Some((side(&m.removed), side(&m.added)))
    }

    fn clear_moves(&mut self) {
        let had = self
            .root
            .result_window_mut()
            .and_then(|w| w.moves.take())
            .is_some();
        if had {
            self.budget.shrink_state_to(self.bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql::{QueryOptions, parse_query, update};

    #[test]
    fn root_absorbs_long_property_path_witness_without_an_effective_copy() {
        let store = Store::in_memory(crate::store::StoreOptions::default());
        let mut seed = String::from("INSERT DATA { ");
        for i in 0..128 {
            seed.push_str(&format!("<urn:n{i}> <urn:step> <urn:n{}> . ", i + 1));
        }
        seed.push('}');
        update::update(&store, &seed, &QueryOptions::default()).unwrap();
        let query = parse_query(
            "SELECT * WHERE { <urn:n0> <urn:step>+ <urn:n128> }",
            None,
            &[],
        )
        .unwrap();
        let compiled = || super::super::compile::compile(&query, None).unwrap();
        let mode = Provenance::why();
        let mut source = Circuit::new(compiled(), None);
        source.set_provenance(mode).unwrap();
        source.build(store.snapshot(), &Control::none()).unwrap();
        let (_, _, Some(ann)) = source.raw_annotations().pop().unwrap() else {
            panic!("the property path must produce a WHY annotation");
        };
        let witness_bytes = 128 * std::mem::size_of::<prov::Atom>();
        assert_eq!(ann.report(1).witnesses[0].len(), 128);

        let mut target = Circuit::new(compiled(), None);
        target.set_provenance(mode).unwrap();
        let input_bytes = ann.bytes() + std::mem::size_of::<(Row, i64, prov::Ann)>() as u64;
        target.set_local_limit(Some(
            input_bytes + ann.bytes() + super::super::zset::row_bytes(0),
        ));
        let mut input = target.budget.scratch_lease();
        input.try_set(input_bytes).unwrap();
        let row: Row = Box::default();
        let change = Some(vec![(row.clone(), 1, ann)]);
        let largest = crate::test_alloc::record_largest(|| {
            target.absorb_budgeted(change).unwrap();
        });
        assert_eq!(target.annotation(&row).unwrap().witnesses[0].len(), 128);
        assert!(
            largest < witness_bytes,
            "absorption allocated a {largest}-byte buffer"
        );
    }
}
