//! Incremental maintenance of a materialization with the backward/forward algorithm
//! (Motik, Nenov, Piro, Horrocks: *Incremental Update of Datalog Materialisation: the
//! Backward/Forward Algorithm*, AAAI 2015).
//!
//! The closure of the previous run is a [`Closure`]: the input graphs of a snapshot as
//! its base tier, and every derived fact, the generalized ones included, in memory as its
//! derived tier. An update reads the input graphs of the snapshot it brings the closure
//! to, so the inserted facts are in the base tier from its start and the deleted ones are
//! not. Given the explicit facts deleted and inserted since, an update
//!
//! 1. **deletes** what no longer follows. A deleted explicit fact is a candidate, held
//!    in the derived tier while the search decides whether it still follows. Before a
//!    candidate is deleted, a backward search looks for another proof of it among the
//!    facts not deleted, and keeps it if one exists. Only facts without a proof are
//!    deleted, and the heads of the rule instances that used them become candidates.
//!    DRed (Gupta, Mumick, Subrahmanian 1993) would delete every consequence first and
//!    derive the survivors again; with RDFS that deletes most of the closure for a single
//!    `rdf:type` triple, through `rdfs:Resource` and the class axioms;
//! 2. **inserts** the new explicit facts and runs the rules semi-naively from them, with
//!    the closure that remains as the old facts. The inserted facts are delta rows of the
//!    derived tier, flagged explicit, for the length of the run, and leave it at its
//!    end, since the base tier holds them.
//!
//! The backward search decides provability exactly. It explores the rule instances that
//! derive a fact, depth first, and counts for each instance the body facts not proved
//! yet. Explicit facts and facts proved earlier in the run are proved. When the search
//! finds no proof and has explored every instance it reached, none of the facts it
//! reached has a proof, and they are all remembered as unprovable.

use crate::budget::{Budget, BudgetFailure, Held, table_bytes, table_capacity, table_grown};
use crate::closure::{DeletionOps, ReachScratch};
use crate::engine::{self, CHead, CRule, Eval, First, Limits, Mode, Slot, Step, VISIT_BATCH};
use crate::facts::{Base, Memo};
use crate::graph::{Cands, Graph, Triple};
use crate::lookups::{self, Lookups};
use crate::terms::Terms;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::Id;
use sparkles_core::incr::fixpoint::{Change, FactStore, FixedPoint, Program, Sink};
use sparkles_core::incr::pass::Which;
use sparkles_core::incr::persist::{Reader, Writer};
use sparkles_core::incr::prov::{Ann, Provenance};
use sparkles_core::store::Snapshot;
use std::marker::PhantomData;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A run that cannot be maintained incrementally and must materialize in full.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub(crate) struct Fallback(pub String);

/// The rule instances that the deletion of an update may visit for each fact of the
/// closure before the run gives up the update and materializes in full (see [`Bound`]).
const VISITS_PER_FACT: u64 = 4;

/// The fewest rule instances that the deletion of an update may visit, so that a small
/// closure is not materialized in full for a deletion of ordinary cost.
const MIN_VISITS: u64 = 1 << 20;

/// The fewest bytes that the deletion of an update may hold beyond what the run held
/// when it started, so that a small closure is not materialized in full for a deletion
/// of ordinary cost (see [`Bound`]).
const MIN_HELD: usize = 64 << 20;

/// The memory bound of a deletion, which a test of this thread can set.
fn held_bound(n: usize) -> usize {
    #[cfg(test)]
    if let Some(m) = tests::HELD_BOUND.with(std::cell::Cell::get) {
        return m;
    }
    n
}

/// The cost of the deletion of one update and its bound, past which the update gives
/// way to a full run.
///
/// The deletion counts the rule instances it visits, in its searches for other proofs
/// and in the steps that find the consequences of deleted facts, and it compares the
/// memory the run holds with what the run held when the deletion started. A full run
/// derives every fact of the closure at least once and holds the closure, so its cost
/// grows at least with the size of the closure. The searches can cost far more. In a
/// clique of a symmetric and transitive property each fact has a derivation through
/// every member, and a search for another proof of a removed edge reaches most of the
/// closure through each of them, keeping a node for every fact it reaches and a record
/// for every instance.
///
/// The deletion may visit [`VISITS_PER_FACT`] instances for each fact of the closure, and
/// at least [`MIN_VISITS`]. The charges of the run may grow by as many bytes beyond their
/// total when the deletion started as the closure's own heap, and by at least
/// [`MIN_HELD`]. That total already holds the closure, the changes, the removed facts
/// held as candidates, the plans and what the witnesses prepared. Past either bound the
/// update stops with a [`Fallback`], its state is dropped, and the run materializes in
/// full, which gives the same closure.
///
/// The bound is a heuristic on sampled charges. Each search, and each evaluation that
/// finds consequences, compares them once every [`VISIT_BATCH`] instances it matched and
/// once more when it ends, so the charges can pass the bound by what the batches under
/// way charge before the update stops. A deletion that ends within the bound counts the
/// same visits on every run with the same number of threads. When the bound stops it, the
/// parallel work of a round has counted what it reached by then, and the memory it held
/// depends on how it interleaved, so near the bound one run can fall back where another
/// does not.
pub(crate) struct Bound {
    visits: AtomicU64,
    max_visits: u64,
    budget: Arc<Budget>,
    /// what the run held when the deletion started
    base: usize,
    max_held: usize,
    /// the most the deletion held beyond `base` when it compared
    peak_held: AtomicUsize,
}

impl Bound {
    /// The maximum rule-instance visits allowed for a deletion on `c`.
    pub(crate) fn max_visits_for(c: &Closure) -> u64 {
        let facts = c.graph.offset() as u64 + c.graph.live() as u64;
        facts.saturating_mul(VISITS_PER_FACT).max(MIN_VISITS)
    }

    /// The bound for the deletion of an update of `c`, charged to `budget`, as it starts.
    pub(crate) fn for_closure(c: &Closure, budget: &Arc<Budget>) -> Bound {
        Bound::new(
            budget,
            Bound::max_visits_for(c),
            held_bound(c.heap_bytes().max(MIN_HELD)),
        )
    }

    pub(crate) fn new(budget: &Arc<Budget>, max_visits: u64, max_held: usize) -> Bound {
        Bound {
            visits: AtomicU64::new(0),
            max_visits,
            budget: budget.clone(),
            base: budget.used(),
            max_held,
            peak_held: AtomicUsize::new(0),
        }
    }

    /// The rule instances visited so far.
    pub(crate) fn visits(&self) -> u64 {
        self.visits.load(Ordering::Relaxed)
    }

    /// The most the deletion was seen to hold beyond what the run held at its start.
    pub(crate) fn peak_held(&self) -> usize {
        self.peak_held.load(Ordering::Relaxed)
    }

    /// Count `n` more visits, and give way to a full run when the visits or the memory
    /// held pass the bound.
    fn add(&self, n: u64) -> Result<(), Fallback> {
        let done = self
            .visits
            .fetch_add(n, Ordering::Relaxed)
            .saturating_add(n);
        if done > self.max_visits {
            return Err(Fallback(format!(
                "the deletion visited more than {} rule instances, {VISITS_PER_FACT} for each \
                 fact of the closure, which costs more than a full run",
                self.max_visits
            )));
        }
        let held = self.budget.used().saturating_sub(self.base);
        self.peak_held.fetch_max(held, Ordering::Relaxed);
        if held > self.max_held {
            return Err(Fallback(format!(
                "the deletion held more than {} bytes beside the closure, more than a full \
                 run holds",
                self.max_held
            )));
        }
        Ok(())
    }
}

/// A materialization: the input graphs of a snapshot as the base tier of its graph (see
/// [`crate::facts`]), and the derived facts in memory as its derived tier.
///
/// A fact is explicit when the base tier holds it. The derived tier holds the facts the
/// base tier does not, and a flag marks those of them that count as explicit too: the
/// GeoSPARQL default geometries, and for the length of an update the inserted facts.
/// Whether a fact of the closure is explicit therefore needs no read of the store: it
/// is explicit when the derived tier does not hold it, or holds it flagged.
pub(crate) struct Closure {
    pub graph: Graph,
    pub terms: Terms,
    /// the GeoSPARQL default geometries the closure holds as explicit facts, when they
    /// are known (see [`crate::default_geometry_changes`])
    pub defaults: Option<Defaults>,
    /// the heap of the closure's own buffers, charged to the budget of the graph. The
    /// flags that mark derived facts explicit are the tier's. Returned after the
    /// default-geometry payloads have been destroyed.
    flags_held: Held,
}

/// The GeoSPARQL default geometries of a closure: each feature's default geometry, and
/// the id of `geo:hasDefaultGeometry`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Defaults {
    pub pred: u64,
    pub of: FxHashMap<u64, u64>,
}

impl Defaults {
    /// Whether `t` is one of the default geometries.
    pub fn contains(&self, t: &Triple) -> bool {
        t[1] == self.pred && self.of.get(&t[0]) == Some(&t[2])
    }

    pub fn facts(&self) -> impl Iterator<Item = Triple> + '_ {
        self.of.iter().map(|(f, g)| [*f, self.pred, *g])
    }

    /// The heap of the map.
    pub fn heap_bytes(&self) -> usize {
        table_bytes(self.of.capacity(), size_of::<(u64, u64)>())
    }

    /// Replace ids that moved. The new map is charged before it is built, beside the old
    /// one (which stays charged to the closure until it settles), failing past the limit
    /// when `strict`.
    fn remap(
        &mut self,
        moved: &FxHashMap<u64, u64>,
        budget: &Arc<Budget>,
        strict: bool,
    ) -> Result<(), BudgetFailure> {
        let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
        self.pred = m(self.pred);
        if self
            .of
            .iter()
            .any(|(f, g)| moved.contains_key(f) || moved.contains_key(g))
        {
            let mut held = Held::new(budget);
            let bytes = table_bytes(table_capacity(self.of.len()), size_of::<(u64, u64)>());
            if strict {
                held.try_grow(bytes)?;
            } else {
                held.grow(bytes)?;
            }
            self.of = self.of.iter().map(|(f, g)| (m(*f), m(*g))).collect();
        }
        Ok(())
    }
}

impl Closure {
    /// The closure of `graph`, whose base tier holds the explicit facts and whose
    /// derived tier holds the derived ones, none of them flagged.
    pub fn new(mut graph: Graph, terms: Terms) -> anyhow::Result<Closure> {
        let flags_held = Held::new(graph.budget());
        if graph.base().is_some() {
            let lookups = Lookups::new(graph.budget(), lookups::DEFAULT_MAX_BYTES);
            match graph.base_mut() {
                // the closure holds the only reference to its tier
                Some(b) => b.set_lookups(lookups),
                // a tier shared with others gets a copy of its own, whose count of the
                // input graphs reads the store and can fail
                None => {
                    let b = graph.base().expect("checked above");
                    let b = Base::new(b.snap.clone(), &b.graph_ids())?;
                    graph.set_base(Arc::new(b.with_lookups(lookups)))?;
                }
            }
        }
        Ok(Closure {
            graph,
            terms,
            flags_held,
            defaults: None,
        })
    }

    /// The heap of the closure: its derived tier, its local terms, the flags, the default
    /// geometries and the remembered lookups. The base tier is the store's.
    pub fn heap_bytes(&self) -> usize {
        self.graph.heap_bytes()
            + self.terms.heap_bytes()
            + self.own_bytes()
            + self.lookups().map_or(0, Lookups::bytes)
    }

    /// The lookups of the base tier the closure remembers.
    pub fn lookups(&self) -> Option<&Lookups> {
        self.graph.base().and_then(Base::lookups)
    }

    /// The remembered lookups, to change between runs, while no run shares the tier.
    pub fn lookups_mut(&mut self) -> Option<&mut Lookups> {
        self.graph.base_mut().and_then(Base::lookups_mut)
    }

    fn own_bytes(&self) -> usize {
        self.defaults.as_ref().map_or(0, Defaults::heap_bytes)
    }
}

/// Validated byte plan for migrating a [`Closure`] onto a target [`Budget`].
pub(crate) struct ClosureTransferPlan {
    graph: Arc<Budget>,
    terms: Arc<Budget>,
    flags: Arc<Budget>,
    lookups: Option<Arc<Budget>>,
    graph_bytes: usize,
    term_bytes: usize,
    flag_bytes: usize,
    lookup_bytes: usize,
    pub(crate) total: usize,
}

impl Closure {
    /// Charge the closure to `budget` from now on, or fail when it does not fit.
    #[cfg(test)]
    pub fn set_budget(&mut self, budget: &Arc<Budget>) -> Result<(), BudgetFailure> {
        let plan = self.plan_budget_transfer(budget)?;
        let mut prepared = Held::try_new_exact(budget, plan.total)?;
        self.apply_budget_transfer(plan, budget, &mut prepared);
        debug_assert_eq!(prepared.bytes(), 0);
        drop(prepared);
        Ok(())
    }

    /// Validate domain rules and compute the exact bytes required to move this closure
    /// onto `budget`, without mutating any existing credit owner.
    pub(crate) fn plan_budget_transfer(
        &self,
        budget: &Arc<Budget>,
    ) -> Result<ClosureTransferPlan, BudgetFailure> {
        // Validate exclusivity before reserving or mutating any existing owner.
        if self.lookups().is_some() && self.graph.base_is_shared() {
            return Err(BudgetFailure::parent(
                sparkles_budget::ParentRefused::DifferentParent,
            ));
        }
        let graph = self.graph.budget().clone();
        let terms = self.terms.budget();
        let flags = self.flags_held.budget().clone();
        let lookups = self.lookups().map(|l| l.budget().clone());
        let move_bytes = |source: &Arc<Budget>, bytes: usize| -> Result<usize, BudgetFailure> {
            if Arc::ptr_eq(source, budget) {
                return Ok(0);
            }
            if bytes != 0
                && source.parent().is_some()
                && budget.parent().is_some()
                && source.domain() != budget.domain()
            {
                return Err(BudgetFailure::parent(
                    sparkles_budget::ParentRefused::ForeignDomain,
                ));
            }
            Ok(bytes)
        };
        let graph_bytes = move_bytes(&graph, self.graph.heap_bytes())?;
        let term_bytes = move_bytes(&terms, self.terms.heap_bytes())?;
        let flag_bytes = move_bytes(&flags, self.own_bytes())?;
        let lookup_bytes = match &lookups {
            Some(source) => move_bytes(source, self.lookups().expect("captured lookups").bytes())?,
            None => 0,
        };
        let total = graph_bytes
            .checked_add(term_bytes)
            .and_then(|n| n.checked_add(flag_bytes))
            .and_then(|n| n.checked_add(lookup_bytes))
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        Ok(ClosureTransferPlan {
            graph,
            terms,
            flags,
            lookups,
            graph_bytes,
            term_bytes,
            flag_bytes,
            lookup_bytes,
            total,
        })
    }

    /// Commit a validated transfer plan using already-prepared credit on `budget`.
    pub(crate) fn apply_budget_transfer(
        &mut self,
        plan: ClosureTransferPlan,
        budget: &Arc<Budget>,
        prepared: &mut Held,
    ) {
        let old_terms = (!Arc::ptr_eq(&plan.terms, budget)).then(|| {
            self.terms
                .replace_credit(&plan.terms, prepared.split(plan.term_bytes))
        });
        let old_graph = (!Arc::ptr_eq(&plan.graph, budget))
            .then(|| self.graph.replace_credit(prepared.split(plan.graph_bytes)));
        let old_flags = (!Arc::ptr_eq(&plan.flags, budget))
            .then(|| std::mem::replace(&mut self.flags_held, prepared.split(plan.flag_bytes)));
        let old_lookups = plan
            .lookups
            .as_ref()
            .filter(|source| !Arc::ptr_eq(source, budget))
            .map(|_| {
                self.lookups_mut()
                    .expect("exclusive prepared lookups")
                    .replace_credit(prepared.split(plan.lookup_bytes))
            });
        // The entire transition is visible before returning any old contribution.
        drop(old_graph);
        drop(old_flags);
        if let Some((source, bytes, retired_cache)) = old_terms {
            drop(retired_cache);
            source.release(bytes);
        }
        if let Some((source, bytes)) = old_lookups {
            source.release(bytes);
        }
    }

    /// Charge the change of the closure's own buffers (the flags and the default
    /// geometries) since the last call.
    pub fn settle(&mut self) -> Result<(), BudgetFailure> {
        let own = self.own_bytes();
        self.flags_held.set(own)?;
        self.graph.settle()
    }

    /// [`settle`](Self::settle), failing instead of charging past the limit.
    pub fn try_settle(&mut self) -> Result<(), BudgetFailure> {
        let own = self.own_bytes();
        self.flags_held.try_set(own)?;
        self.graph.try_settle()
    }

    /// Replace the base tier by the input graphs `graphs` of `snap`, a later snapshot of
    /// the same generation, in the order the run resolved them. The new tier starts with
    /// no remembered lookups. Those of the old tier are returned frozen, and they hold
    /// for the old snapshot only. A caller that brings them up to date with the changes
    /// since gives them back with [`put_lookups`](Self::put_lookups), and any other
    /// caller drops them.
    pub fn set_inputs(
        &mut self,
        snap: Arc<Snapshot>,
        graphs: &[Id],
    ) -> anyhow::Result<Option<Lookups>> {
        let base = Base::new(snap, graphs)?;
        let fresh = Lookups::new(self.graph.budget(), lookups::DEFAULT_MAX_BYTES);
        let old = self.graph.swap_base(Arc::new(base.with_lookups(fresh)))?;
        // the lookups move over only when no one else shares the old tier, which holds
        // between runs
        let mut lookups = old
            .and_then(|b| Arc::try_unwrap(b).ok())
            .and_then(|mut b| b.take_lookups());
        if let Some(l) = &mut lookups {
            l.freeze();
        }
        Ok(lookups)
    }

    /// Give the base tier remembered lookups that hold for its snapshot, in place of
    /// those it has.
    pub fn put_lookups(&mut self, lookups: Lookups) {
        if let Some(b) = self.graph.base_mut() {
            b.set_lookups(lookups);
        }
    }

    /// Whether row `row` of the derived tier is flagged explicit.
    #[inline]
    pub fn flagged(&self, row: u32) -> bool {
        self.graph.flagged(row)
    }

    pub fn set_flag(&mut self, row: u32, on: bool) {
        let done = self.set_flag_with(row, on, false);
        debug_assert!(done.is_ok(), "charges that may pass the limit do not fail");
    }

    /// [`set_flag`](Self::set_flag), charging the growth of the flags before it is
    /// allocated, and with `strict` failing when it does not fit.
    pub fn set_flag_with(&mut self, row: u32, on: bool, strict: bool) -> Result<(), BudgetFailure> {
        self.graph.set_flag_with(row, on, strict)
    }

    /// Whether a fact of the closure is explicit: the base tier holds it when the
    /// derived tier does not, and a fact of the derived tier is explicit when it is
    /// flagged. A triple that is not in the closure counts as explicit, so the caller
    /// asks only about facts it read from the closure.
    #[inline]
    pub fn explicit_held(&self, t: &Triple) -> bool {
        self.graph.position(t).is_none_or(|r| self.flagged(r))
    }

    /// The live facts of the derived tier, in row order, with their flags.
    pub fn live(&self) -> impl Iterator<Item = (Triple, bool)> + '_ {
        let g = &self.graph;
        let rows = (g.offset()..g.len()).filter(|&r| g.alive(r));
        rows.map(|r| (g.triple(r), self.flagged(r)))
    }

    /// Replace local ids by store ids (`moved`) in every live row.
    pub fn remap(&mut self, moved: &FxHashMap<u64, u64>) -> Result<(), BudgetFailure> {
        self.remap_with(moved, false)
    }

    /// [`remap`](Self::remap), charging the work beside the closure before it is
    /// allocated and failing past the limit. The closure is unusable after an error.
    pub fn try_remap(&mut self, moved: &FxHashMap<u64, u64>) -> Result<(), BudgetFailure> {
        self.remap_with(moved, true)
    }

    fn remap_with(
        &mut self,
        moved: &FxHashMap<u64, u64>,
        strict: bool,
    ) -> Result<(), BudgetFailure> {
        if moved.is_empty() {
            return Ok(());
        }
        let budget = self.graph.budget().clone();
        if let Some(d) = &mut self.defaults {
            d.remap(moved, &budget, strict)?;
        }
        let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
        // the facts that move, in a buffer charged before each growth
        let fact = size_of::<(Triple, bool)>();
        let mut held = Held::new(&budget);
        let mut hit: Vec<(Triple, bool)> = Vec::new();
        for x in self
            .live()
            .filter(|(t, _)| t.iter().any(|x| moved.contains_key(x)))
        {
            if hit.len() == hit.capacity() {
                let cap = hit.capacity();
                let new = (cap * 2).max(4);
                if strict {
                    held.try_grow(new * fact)?;
                } else {
                    held.grow(new * fact)?;
                }
                hit.reserve_exact(new - hit.len());
                held.shrink(cap * fact);
            }
            hit.push(x);
        }
        for (t, _) in &hit {
            if let Some(r) = self.graph.position(t) {
                self.set_flag(r, false);
            }
            self.graph.kill(t)?;
        }
        for (t, flag) in hit {
            let n = [m(t[0]), m(t[1]), m(t[2])];
            match self.graph.position(&n) {
                Some(r) => {
                    if flag {
                        self.set_flag_with(r, true, strict)?;
                    }
                }
                None => {
                    if strict {
                        self.graph.try_add_batch([n])?;
                    } else {
                        self.graph.add_batch([n]);
                    }
                    let r = self.graph.len() - 1;
                    self.set_flag_with(r, flag, strict)?;
                }
            }
        }
        Ok(())
    }

    /// Rebuild the derived tier without its dead rows when they are a quarter of it or
    /// more. The new tier is built beside the old one, charged as it grows; with
    /// `strict`, failing past the limit (the closure is unusable then).
    pub fn compact_if_needed(&mut self, strict: bool) -> Result<(), BudgetFailure> {
        let g = &self.graph;
        let rows = (g.len() - g.offset()) as usize;
        if (g.dead_rows() as usize) * 4 < rows {
            return Ok(());
        }
        let budget = g.budget().clone();
        let n = g.live();
        let mut held = Held::new(&budget);
        let list = n * size_of::<(Triple, bool)>() + n.div_ceil(64) * size_of::<u64>();
        if strict {
            held.try_grow(list)?;
        } else {
            held.grow(list)?;
        }
        let live: Vec<(Triple, bool)> = self.live().collect();
        let mut graph = g.try_like(live.len(), &budget, strict)?;
        if strict {
            graph.try_add_batch(live.iter().map(|x| x.0))?;
        } else {
            graph.add_batch(live.iter().map(|x| x.0));
        }
        let first = graph.offset();
        for (r, (_, b)) in live.iter().enumerate() {
            if *b {
                graph.set_flag_with(first + r as u32, true, strict)?;
            }
        }
        self.graph = graph;
        drop(live);
        drop(held);
        self.settle()?;
        Ok(())
    }
}

/// What an update changed.
#[derive(Debug, Default)]
pub(crate) struct Update {
    /// facts removed from the closure (an insertion may have derived some of them again)
    pub deleted: Vec<Triple>,
    /// rows from this one on were added
    pub added_from: u32,
    /// facts whose provability was searched for
    pub checked: u64,
    /// rule instances the deletion visited (see [`Bound`])
    pub visited: u64,
    /// the most the deletion was seen to hold beside what the run held when it started
    pub held_beside: usize,
    /// semi-naive iterations of the insertion
    pub iterations: usize,
    /// the wall time of the deletion and of the insertion, in microseconds
    pub delete_micros: u64,
    pub insert_micros: u64,
    /// the charge of the update's buffers that remain, `deleted` among them
    pub held: Option<Held>,
}

impl Update {
    /// The facts added to the closure.
    pub fn added<'a>(&self, c: &'a Closure) -> impl Iterator<Item = Triple> + 'a {
        let g = &c.graph;
        (self.added_from..g.len())
            .filter(|&r| g.alive(r))
            .map(|r| g.triple(r))
    }
}

fn cancelled(limits: &Limits) -> bool {
    limits
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
}

/// A call that an update makes before it deletes anything (see [`update`]).
pub(crate) type BeforeDelete<'a> = &'a mut dyn FnMut(&mut Closure) -> anyhow::Result<()>;

/// Bring `c` from the closure of the old explicit facts to that of the new ones:
/// `removed` explicit facts are no longer explicit, `inserted` ones are. The base tier of
/// `c` holds the new explicit facts already: the inserted ones and not the removed ones,
/// except for explicit facts the store does not hold, such as default geometries, which
/// the derived tier holds flagged.
///
/// The update is one pass of the fixed-point operator of F13 §4.3 in exact mode, with
/// [`TierProgram`] as its program and the closure's derived tier as its store
/// ([`TierStore`]).
///
/// Returns [`Fallback`] (as the error) when the rules read RDF lists and a list fact is
/// derivation. The closure is unusable after any error.
///
/// `before_delete` is called once the closure holds every fact of the previous closure
/// and the removed explicit facts as candidates, before anything is deleted: the
/// witnesses of [`crate::witness`] read there which facts the removed ones reach.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn update<'a>(
    c: &mut Closure,
    rules: &'a [CRule],
    limits: &'a Limits,
    inserted: &'a [Triple],
    removed: &'a [Triple],
    before_delete: Option<BeforeDelete<'a>>,
) -> anyhow::Result<Update> {
    update_with(c, rules, limits, inserted, removed, before_delete, None)
}

/// [`update`] of the closure of a lowered program with relations (item 839's plan,
/// §12.2). `rules` are the program's lowered rules, and `rebuild` is its relation part
/// when `c` lacks its relation facts, as a closure read back from the dataset does.
///
/// The relation facts are rebuilt once the removed facts are candidates, over the
/// previous closure and the inserted facts, which the base tier already holds. Every
/// rebuilt fact that depends on a removed fact is then a consequence of it, so the
/// deletion stays exact, and the insertion evaluates from the first rebuilt row, so every
/// rule instance with a relation premise is evaluated and the triples that relation facts
/// support through inserted facts are derived. A part whose rules make terms falls back,
/// since a rebuild would make other blank nodes than the stored ones.
pub(crate) fn update_with<'a>(
    c: &mut Closure,
    rules: &'a [CRule],
    limits: &'a Limits,
    inserted: &'a [Triple],
    removed: &'a [Triple],
    before_delete: Option<BeforeDelete<'a>>,
    rebuild: Option<&'a crate::ir::Program>,
) -> anyhow::Result<Update> {
    update_with_moved(
        c,
        rules,
        limits,
        inserted,
        removed,
        &[],
        false,
        before_delete,
        rebuild,
    )
}

/// [`update_with`], also carrying `moved` (triples whose graph membership changed across
/// multiple input graphs without changing the triple union) and `multi_graph` (whether
/// the run reads more than one configured input graph, even if some are now empty).
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_with_moved<'a>(
    c: &mut Closure,
    rules: &'a [CRule],
    limits: &'a Limits,
    inserted: &'a [Triple],
    removed: &'a [Triple],
    moved: &'a [Triple],
    multi_graph: bool,
    before_delete: Option<BeforeDelete<'a>>,
    rebuild: Option<&'a crate::ir::Program>,
) -> anyhow::Result<Update> {
    let budget = &limits.budget;
    let mut cx = TierCx {
        rules,
        limits,
        inserted,
        removed,
        moved,
        multi_graph,
        before_delete,
        rebuild,
        out: Update::default(),
        started: std::time::Instant::now(),
    };
    let changes = {
        let store = TierStore::new(c, budget);
        let mut fp = FixedPoint::with_store(TierProgram::default(), store);
        fp.advance(&mut cx, None)?
    };
    let mut out = cx.out;
    // the operator released its charge of the changes with the pass, and they stay
    // allocated until the removed facts are copied out of them
    let change_bytes = changes.capacity() * size_of::<Change<Triple>>();
    let mut held = Held::try_new(budget, change_bytes)?;
    // the removed facts stay with the update, charged before their buffer is allocated;
    // a fact over-deleted during operator maintenance and re-derived in saturation stays
    // alive in `c` and is not reported as deleted
    let n = changes
        .iter()
        .filter(|ch| ch.weight < 0 && c.graph.position(&ch.fact).is_none())
        .count();
    held.try_grow(n * size_of::<Triple>())?;
    out.deleted.reserve_exact(n);
    out.deleted.extend(
        changes
            .iter()
            .filter(|ch| ch.weight < 0 && c.graph.position(&ch.fact).is_none())
            .map(|ch| ch.fact),
    );
    drop(changes);
    held.shrink(change_bytes);
    out.held = Some(held);
    Ok(out)
}

/// What one update of a closure reads, and what it reports, as the context of a
/// [`TierProgram`].
pub(crate) struct TierCx<'a> {
    rules: &'a [CRule],
    limits: &'a Limits,
    inserted: &'a [Triple],
    removed: &'a [Triple],
    /// triples that moved between multiple input graphs while staying in the union
    moved: &'a [Triple],
    /// whether the run reads more than one configured input graph
    multi_graph: bool,
    before_delete: Option<BeforeDelete<'a>>,
    /// the relation part of a program whose closure lacks its relation facts (item 839),
    /// which the update rebuilds first
    rebuild: Option<&'a crate::ir::Program>,
    /// what the update reports, filled as its stages end
    out: Update,
    /// when the current stage started
    started: std::time::Instant,
}

/// The derived tier of a closure as the store of the fixed-point operator.
///
/// The facts it holds are the live rows of the derived tier: the derived facts, the
/// removed explicit facts held as candidates, and the flagged rows. The base tier's
/// facts are not held here, and the program reads them in its own evaluation. The
/// operator's reservations are charged to the run's budget through one [`Held`], which
/// returns what it kept unused when the update ends.
pub(crate) struct TierStore<'a> {
    pub c: &'a mut Closure,
    held: Mutex<Held>,
}

impl<'a> TierStore<'a> {
    pub fn new(c: &'a mut Closure, budget: &Arc<Budget>) -> TierStore<'a> {
        TierStore {
            c,
            held: Mutex::new(Held::new(budget)),
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        // a panic while the lock was held leaves a count that is still the charge
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl FactStore<Triple, ()> for TierStore<'_> {
    type Error = anyhow::Error;

    fn contains(&self, f: &Triple) -> bool {
        self.c.graph.position(f).is_some()
    }

    fn record(&self, f: &Triple) -> Option<()> {
        self.contains(f).then_some(())
    }

    fn len(&self) -> usize {
        self.c.graph.live()
    }

    fn for_each(&self, visit: &mut dyn FnMut(&Triple, &())) {
        let g = &self.c.graph;
        for r in g.offset()..g.len() {
            if g.alive(r) {
                visit(&g.triple(r), &());
            }
        }
    }

    fn bytes(&self) -> u64 {
        self.c.heap_bytes() as u64
    }

    fn insert(&mut self, f: Triple, _r: ()) -> anyhow::Result<bool> {
        if self.contains(&f) {
            return Ok(false);
        }
        Ok(self.c.graph.try_add_batch([f])? > 0)
    }

    fn set_record(&mut self, _f: Triple, _r: ()) -> anyhow::Result<()> {
        Ok(())
    }

    fn remove(&mut self, f: &Triple) -> anyhow::Result<bool> {
        Ok(self.c.graph.kill(f)?)
    }

    fn clear(&mut self) -> anyhow::Result<()> {
        let mut live = Held::try_new(self.c.graph.budget(), self.len() * size_of::<Triple>())?;
        let mut all = Vec::with_capacity(self.len());
        self.for_each(&mut |t, _| all.push(*t));
        for t in &all {
            self.c.graph.kill(t)?;
        }
        drop(all);
        live.shrink(live.bytes());
        Ok(())
    }

    fn reserve(&self, bytes: u64) -> anyhow::Result<()> {
        let n = usize::try_from(bytes).map_err(|_| anyhow::anyhow!("a reservation past usize"))?;
        Ok(self.held().try_grow(n)?)
    }

    fn release(&self, bytes: u64) {
        let mut h = self.held();
        let n = usize::try_from(bytes).unwrap_or(usize::MAX).min(h.bytes());
        h.shrink(n);
    }
}

/// The reasoner's rules over a closure as a program of the fixed-point operator in exact
/// mode: the backward/forward algorithm of the module's documentation, with the
/// operator running its rounds.
///
/// [`Program::prepare`] flags the inserted facts the derived tier holds, moves the
/// removed explicit facts into the derived tier as candidates, and calls the witness
/// hook. The first frontier is the removed explicit facts. [`Program::prove_batch`] is
/// one round of [`Search::prove`], in parallel through [`prove_all`], under the bound of
/// [`Bound`], [`Program::consequences_batch`] at the old snapshot is [`consequences`] of
/// the dead facts, and a removal kills the row. [`Program::saturate`] appends the inserted
/// facts that no row holds as flagged rows, runs the rules semi-naively from them, and
/// drops the inserted facts the base tier holds from the derived tier, and
/// [`Program::saturated`] lists the live rows it added.
///
/// The operator's changes are those of the derived tier's live rows, as the update
/// reported them before: a removed fact that no longer follows is a removal, and a row
/// that the insertion added and that is live at its end is an addition. A derived fact
/// that becomes explicit leaves the derived tier for the base tier without a change.
#[derive(Default)]
pub(crate) struct TierProgram<'a> {
    /// the deletion's state, made by `prepare` when facts were removed and dropped when
    /// the pass ends
    del: Option<Deletion>,
    /// the first row the insertion added
    start: u32,
    /// the first row of the rebuilt relation facts, from which the insertion evaluates,
    /// and the facts the rebuild derived, which the insertion's limit of inferred facts
    /// counts
    rebuilt: Option<(u32, usize)>,
    /// whether the rules read RDF lists
    lists: bool,
    /// whether the deletion removed an RDF list fact
    list_deleted: bool,
    /// explicit RDF lists whose members changed in this update (F16 §7.3)
    changed_lists: Option<ChangedLists>,
    /// live derived facts produced by list-reading rules over old list members before a
    /// list change, added to the first deletion frontier
    list_frontier: Vec<Triple>,
    /// budget charge for `list_frontier`
    list_frontier_held: Option<Held>,
    /// facts deleted during Phase 1 whose proof search encountered an unproved live fact
    /// of an operator-governed predicate, checked for 1-step revival after saturation
    overdeleted_touching_op: Vec<Triple>,
    /// budget charge for `overdeleted_touching_op`
    overdeleted_held: Option<Held>,
    _store: PhantomData<TierStore<'a>>,
}

/// What the deletion of one update keeps across its rounds.
struct Deletion {
    plans: Plans,
    /// the plans' charge
    _plans_held: Held,
    /// operator-maintained transitive and equivalence relations for this deletion
    ops: Option<DeletionOps>,
    /// made by the first round, once the first frontier is held
    bound: Option<Bound>,
    search: Search,
    abort: AtomicBool,
}

/// Explicit RDF list heads whose member sequences changed between the old and new
/// snapshots, with their old member lists (F16 §7.3).
struct ChangedLists {
    heads: Vec<u64>,
    old_members: FxHashMap<u64, Option<std::rc::Rc<[u64]>>>,
    _held: Held,
}

impl TierProgram<'_> {
    fn deletion(&mut self) -> anyhow::Result<&mut Deletion> {
        self.del
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("a deletion round of an update that removed nothing"))
    }
}

impl<'a> Program for TierProgram<'a> {
    type Fact = Triple;
    type Record = ();
    type Cx<'c> = TierCx<'c>;
    type Error = anyhow::Error;
    type Store = TierStore<'a>;

    const EXACT: bool = true;

    fn batch_len(&self) -> usize {
        // a round is one batch: its searches are independent and run in parallel
        usize::MAX
    }

    fn premises(
        &self,
        _f: &Triple,
        _r: &(),
        _out: &mut Sink<'_, Triple, anyhow::Error>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn axioms(
        &mut self,
        _cx: &mut TierCx<'_>,
        _store: &TierStore<'a>,
        _out: &mut Sink<'_, (Triple, ()), anyhow::Error>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("a closure is built by a full run, not by the fixed-point operator")
    }

    fn consequences(
        &mut self,
        cx: &mut TierCx<'_>,
        store: &TierStore<'a>,
        w: Which,
        f: &Triple,
        held: &dyn Fn(&Triple) -> bool,
        out: &mut Sink<'_, (Triple, ()), anyhow::Error>,
    ) -> anyhow::Result<()> {
        self.consequences_batch(cx, store, w, std::slice::from_ref(f), held, out)
    }

    fn consequences_batch(
        &mut self,
        cx: &mut TierCx<'_>,
        store: &TierStore<'a>,
        w: Which,
        facts: &[Triple],
        _held: &dyn Fn(&Triple) -> bool,
        out: &mut Sink<'_, (Triple, ()), anyhow::Error>,
    ) -> anyhow::Result<()> {
        if !matches!(w, Which::Old) {
            anyhow::bail!("an update inserts through its own saturation");
        }
        let c: &Closure = store.c;
        let (first, rest) = (c.terms.rdf_first, c.terms.rdf_rest);
        let is_list = |t: &Triple| t[1] == first || t[1] == rest;
        if self.lists && facts.iter().any(|t| is_list(t) && !cx.removed.contains(t)) {
            self.list_deleted = true;
        }
        let d = self.deletion()?;
        let Some(bound) = d.bound.as_ref() else {
            anyhow::bail!("the consequences of facts that no round decided");
        };
        let _held = consequences_into(
            c,
            cx.rules,
            &d.plans,
            facts,
            cx.limits,
            Some(bound),
            &mut |t| out.push((t, ())),
        )?;
        if let Some(ops) = d.ops.as_mut() {
            let mut visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
            ops.consequences(
                facts,
                &c.graph,
                &c.terms,
                &cx.limits.budget,
                cx.limits.cancel.as_deref(),
                &mut visit,
                &mut |t| out.push((t, ())),
            )?;
        }
        Ok(())
    }

    fn prove(
        &mut self,
        cx: &mut TierCx<'_>,
        store: &TierStore<'a>,
        f: &Triple,
        held: &dyn Fn(&Triple) -> bool,
    ) -> anyhow::Result<Option<()>> {
        let mut v = Vec::with_capacity(1);
        self.prove_batch(cx, store, std::slice::from_ref(f), held, &mut v)?;
        Ok(v.pop().flatten())
    }

    fn prove_batch(
        &mut self,
        cx: &mut TierCx<'_>,
        store: &TierStore<'a>,
        facts: &[Triple],
        _held: &dyn Fn(&Triple) -> bool,
        out: &mut Vec<Option<()>>,
    ) -> anyhow::Result<()> {
        let c: &Closure = store.c;
        let limits = cx.limits;
        let budget = &limits.budget;
        let explicit_lists = self.lists;
        let (first, rest) = (c.terms.rdf_first, c.terms.rdf_rest);
        let is_list = |t: &Triple| t[1] == first || t[1] == rest;
        let Some(Deletion {
            plans,
            ops,
            bound,
            search,
            abort,
            ..
        }) = self.del.as_mut()
        else {
            anyhow::bail!("a deletion round of an update that removed nothing");
        };
        let overdeleted = &mut self.overdeleted_touching_op;
        let overdeleted_held = self
            .overdeleted_held
            .get_or_insert_with(|| Held::new(budget));
        // after the plans and the first frontier, as the deletion starts
        let bound = &*bound.get_or_insert_with(|| Bound::for_closure(c, budget));
        // Materialize confirmed explicit-edge reachability graphs for any operator
        // predicates in this round before parallel search starts.
        if let Some(ops) = ops.as_mut() {
            let mut preds_held = Held::new(budget);
            let mut preds: Vec<u64> = Vec::new();
            for t in facts {
                if ops.is_governed(t[1], &c.graph) && !preds.contains(&t[1]) {
                    push_charged(&mut preds, t[1], &mut preds_held)?;
                }
            }
            let mut visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
            for q in preds {
                ops.ensure_pred(
                    q,
                    &c.graph,
                    &c.terms,
                    budget,
                    limits.cancel.as_deref(),
                    &mut visit,
                )?;
            }
            drop(preds_held);
        }
        let out_start = out.len();
        let ops_ref = ops.as_ref();
        if rayon::current_num_threads() > 1 && facts.len() >= PAR_SEARCH {
            // the searches of a round are independent: see `prove_all`
            prove_all(
                c,
                cx.rules,
                plans,
                ops_ref,
                explicit_lists,
                abort,
                limits,
                bound,
                search,
                facts,
                out,
            )?;
        } else {
            let scx = Cx {
                c,
                rules: cx.rules,
                plans,
                ops: ops_ref,
                explicit_lists,
                abort,
                cancel: limits.cancel.as_deref(),
                budget,
                bound,
                known: None,
            };
            for &t in facts {
                let proved = search.prove(&scx, t);
                if let Some(e) = search.err.take() {
                    return Err(e);
                }
                out.push(proved.then_some(()));
            }
            // the round's blocks are not kept until the next round
            search.memo = Memo::default();
        }
        for (&t, v) in facts.iter().zip(&out[out_start..]) {
            if v.is_none() && search.unprovable_touched_op.contains(&t) {
                push_charged(overdeleted, t, overdeleted_held)?;
                bound.add(0)?;
            }
            if explicit_lists && v.is_some() && is_list(&t) && !c.explicit_held(&t) {
                return Err(Fallback("the rules read RDF lists that are derived".into()).into());
            }
        }
        Ok(())
    }

    fn broken(
        &mut self,
        _cx: &mut TierCx<'_>,
        _store: &TierStore<'a>,
        _recorded: &dyn Fn(&Triple) -> Option<()>,
        _out: &mut Sink<'_, Triple, anyhow::Error>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn added(
        &mut self,
        _cx: &mut TierCx<'_>,
        _store: &TierStore<'a>,
        _held: &dyn Fn(&Triple) -> bool,
        _out: &mut Sink<'_, (Triple, ()), anyhow::Error>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn prepare(&mut self, cx: &mut TierCx<'_>, store: &mut TierStore<'a>) -> anyhow::Result<()> {
        let c = &mut *store.c;
        let (rules, budget) = (cx.rules, &cx.limits.budget);
        self.lists = rules.iter().any(engine::reads_lists);
        let (first, rest) = (c.terms.rdf_first, c.terms.rdf_rest);
        let is_list = |t: &Triple| t[1] == first || t[1] == rest;
        if self.lists {
            let g = &c.graph;
            for p in [first, rest] {
                let rows = g.cands(None, Some(p), None, 0, g.len());
                if rows
                    .iter()
                    .any(|r| g.alive(r) && g.triple(r)[1] == p && !c.flagged(r))
                {
                    return Err(Fallback("the rules read RDF lists that are derived".into()).into());
                }
            }
        }
        // the closure, the run's changes and the maintenance state are charged to the run
        // before they are used, also when nothing is inserted
        budget.check()?;
        // the inserted facts the derived tier holds are explicit from the start, as the
        // base tier's copies of them are
        for t in cx.inserted {
            if let Some(r) = c.graph.position(t) {
                c.set_flag_with(r, true, true)?;
            }
        }
        // ---- deletion
        cx.started = std::time::Instant::now();
        // the removed facts that were in the base tier: they join the derived tier as
        // candidates, and stay there when they still follow
        let back_held = Held::try_new(budget, std::mem::size_of_val(cx.removed))?;
        let mut back: Vec<Triple> = Vec::with_capacity(cx.removed.len());
        for t in cx.removed {
            match c.graph.position(t) {
                Some(r) => c.set_flag(r, false),
                None => back.push(*t),
            }
        }
        c.graph.try_add_batch(back)?;
        drop(back_held);
        if let Some(part) = cx.rebuild {
            let lowered = part
                .lower(budget)
                .map_err(crate::ir::lower::LowerError::into_anyhow)?;
            if lowered
                .strata
                .iter()
                .flatten()
                .any(|r| crate::ir::rebuild::makes_terms(r, &c.terms))
            {
                return Err(
                    Fallback("the relation facts make new terms and are not kept".into()).into(),
                );
            }
            drop(lowered);
            let lo = c.graph.len();
            crate::ir::rebuild::rebuild_relations(&mut c.graph, part, &c.terms, cx.limits)?;
            self.rebuilt = Some((lo, (c.graph.len() - lo) as usize));
        }
        if let Some(f) = cx.before_delete.as_mut() {
            (*f)(c)?;
        }
        // Per-list tracking (F16 §7.3): when explicit RDF list triples changed, find every
        // list head whose member sequence changed and seed the first deletion frontier
        // with the live derived heads that list-reading rules produced over the old lists.
        let mut bound: Option<Bound> = None;
        if self.lists && cx.moved.iter().any(is_list) {
            return Err(Fallback("an RDF list moved across input graphs".into()).into());
        }
        if self.lists && cx.inserted.iter().chain(cx.removed).any(is_list) {
            let b = bound.get_or_insert_with(|| Bound::for_closure(c, budget));
            let changed = changed_lists(c, cx.inserted, cx.removed, cx.multi_graph, cx.limits, b)?;
            if !changed.heads.is_empty() {
                let (frontier, frontier_held) =
                    eval_list_rules_old(c, rules, &changed, cx.limits, b)?;
                self.list_frontier = frontier;
                self.list_frontier_held = Some(frontier_held);
                self.changed_lists = Some(changed);
            }
        }
        if !cx.removed.is_empty() || !self.list_frontier.is_empty() {
            let b = bound.get_or_insert_with(|| Bound::for_closure(c, budget));
            let ops = if cx.limits.closures {
                let mut visit = |n: u64| b.add(n).map_err(anyhow::Error::from);
                DeletionOps::detect(
                    rules,
                    &c.graph,
                    &c.terms,
                    budget,
                    cx.limits.cancel.as_deref(),
                    &mut visit,
                )?
            } else {
                None
            };
            let plans = Plans::new_without(&mut c.graph, rules, |ri| {
                ops.as_ref().is_some_and(|o| o.replaces(ri))
            })?;
            let plans_held = Held::try_new(budget, plans.bytes)?;
            // F16 §7.3 cost estimate against item 798's visit bound (replaces
            // LARGE_DELETION): estimate non-operator join visits from removed predicates
            // and closure fan-out before running backward searches, while `plans_held`
            // remains charged for the live `Plans` allocation.
            if !cx.removed.is_empty() {
                let max_visits = Bound::max_visits_for(c);
                let est = estimate_deletion_visits(c, rules, &plans, cx.removed, cx.limits)?;
                if est > max_visits {
                    return Err(Fallback(format!(
                        "the deletion is estimated to visit {est} rule instances, past the \
                         bound of {max_visits} for the closure, which costs more than a full run"
                    ))
                    .into());
                }
            }
            self.del = Some(Deletion {
                plans,
                _plans_held: plans_held,
                ops,
                bound,
                search: Search::new(budget),
                abort: AtomicBool::new(false),
            });
        }
        Ok(())
    }

    fn frontier(
        &mut self,
        cx: &mut TierCx<'_>,
        _store: &TierStore<'a>,
        out: &mut Sink<'_, Triple, anyhow::Error>,
    ) -> anyhow::Result<()> {
        for t in cx.removed {
            out.push(*t)?;
        }
        for t in &self.list_frontier {
            out.push(*t)?;
        }
        Ok(())
    }

    fn saturate(&mut self, cx: &mut TierCx<'_>, store: &mut TierStore<'a>) -> anyhow::Result<bool> {
        let c = &mut *store.c;
        let limits = cx.limits;
        let (rules, budget) = (cx.rules, &limits.budget);
        let mut dirty_preds: Option<(FxHashSet<u64>, Held)> = None;
        let mut saved_plans: Option<(Plans, Held)> = None;
        if let Some(d) = self.del.take() {
            cx.out.checked = d.search.checked;
            if let Some(b) = &d.bound {
                cx.out.visited = b.visits();
                cx.out.held_beside = b.peak_held();
            }
            if let Some(ops) = d.ops {
                dirty_preds = Some(ops.take_dirty_preds());
            }
            // Only keep plans for `revive_overdeleted` if at least one unproved
            // operator-governed fact touched during Phase 1 actually survived deletion at
            // a row `< start` (otherwise `engine::run_from_with_dirty` handles everything).
            let needs_revive = !self.overdeleted_touching_op.is_empty()
                && d.search
                    .op_facts_touched
                    .iter()
                    .any(|w| c.graph.position(w).is_some());
            if needs_revive {
                saved_plans = Some((d.plans, d._plans_held));
            } else {
                self.overdeleted_touching_op = Vec::new();
                self.overdeleted_held = None;
            }
            if cancelled(limits) {
                anyhow::bail!("reasoning cancelled");
            }
            if self.list_deleted {
                return Err(Fallback("a derived RDF list fact was removed".into()).into());
            }
        }
        let (first, rest) = (c.terms.rdf_first, c.terms.rdf_rest);
        let is_list = |t: &Triple| t[1] == first || t[1] == rest;
        // ---- insertion
        cx.out.delete_micros = cx.started.elapsed().as_micros() as u64;
        cx.started = std::time::Instant::now();
        let inserted = cx.inserted;
        let start = c.graph.len();
        self.start = start;
        cx.out.added_from = start;
        let mut held = Held::try_new(budget, std::mem::size_of_val(inserted))?;
        let fresh: Vec<Triple> = inserted
            .iter()
            .filter(|t| c.graph.position(t).is_none())
            .copied()
            .collect();
        c.graph.try_add_batch(fresh)?;
        for r in start..c.graph.len() {
            c.set_flag_with(r, true, true)?;
        }
        // Every derived fact admitted after the explicit inserted rows (list replay,
        // semi-naive saturation, and over-deleted revival) counts against one shared
        // `max_inferred` allowance from `inferred_origin`.
        let inferred_origin = c.graph.len();
        let narrowed;
        let eff_limits = match self.rebuilt {
            None => limits,
            Some((_, facts)) => {
                narrowed = Limits {
                    max_iterations: limits.max_iterations,
                    max_inferred: limits.max_inferred.saturating_sub(facts),
                    cancel: limits.cancel.clone(),
                    deadline: limits.deadline,
                    progress: limits.progress.clone(),
                    budget: limits.budget.clone(),
                    closures: limits.closures,
                    magic: None,
                    sampled_planning: limits.sampled_planning,
                };
                &narrowed
            }
        };
        // Per-list tracking (F16 §7.3): evaluate list-reading rules for changed explicit
        // lists using their new member sequences so newly enabled instances enter `start..`.
        if let Some(changed) = self.changed_lists.take() {
            eval_list_rules_new(c, rules, &changed, eff_limits, inferred_origin)?;
        }
        budget.check()?;
        // after a rebuild, the relation facts are new to the rules too, and the rebuilt
        // facts count against the limit of inferred facts with the insertion's
        let from = self.rebuilt.map_or(start, |(lo, ..)| lo.min(start));
        let has_dirty = dirty_preds.as_ref().is_some_and(|(s, _)| !s.is_empty());
        if c.graph.len() > from || has_dirty {
            let run_res = engine::run_from_with_dirty(
                &mut c.graph,
                rules,
                &c.terms,
                eff_limits,
                Some(from),
                None,
                engine::Origin {
                    base_len: Some(inferred_origin),
                    iterations: 0,
                    lists: None,
                },
                dirty_preds.take(),
            );
            let o = match run_res {
                Ok(o) => o,
                Err(e) if e.is::<crate::closure::Restart>() => {
                    return Err(Fallback("an RDF list fact was derived".into()).into());
                }
                Err(e) => return Err(e),
            };
            cx.out.iterations = o.iterations;
            // If any Phase-1 deleted fact failed its proof after encountering a surviving
            // unproved operator-governed fact (mixed R_other + operator derivation), check
            // 1-step revival over the now-reclosed closure and saturate until quiescent.
            if let Some((plans, _plans_held)) = saved_plans.take() {
                revive_overdeleted(
                    c,
                    rules,
                    &plans,
                    eff_limits,
                    inferred_origin,
                    &mut self.overdeleted_touching_op,
                    &mut cx.out.iterations,
                )?;
                self.overdeleted_touching_op = Vec::new();
                self.overdeleted_held = None;
            }
            if self.lists && cx.out.added(c).any(|t| is_list(&t) && !c.explicit_held(&t)) {
                return Err(Fallback("an RDF list fact was derived".into()).into());
            }
        } else if let Some((plans, _plans_held)) = saved_plans.take() {
            if let Some((dirty, dirty_held)) = dirty_preds.take() {
                drop(dirty);
                drop(dirty_held);
            }
            revive_overdeleted(
                c,
                rules,
                &plans,
                eff_limits,
                inferred_origin,
                &mut self.overdeleted_touching_op,
                &mut cx.out.iterations,
            )?;
            if self.lists && cx.out.added(c).any(|t| is_list(&t) && !c.explicit_held(&t)) {
                return Err(Fallback("an RDF list fact was derived".into()).into());
            }
        } else if let Some((dirty, dirty_held)) = dirty_preds.take() {
            drop(dirty);
            drop(dirty_held);
        }
        // the inserted facts that the base tier holds leave the derived tier, found with
        // one sorted pass over the base tier
        held.try_grow(std::mem::size_of_val(inserted) + inserted.len())?;
        let mut ins: Vec<Triple> = inserted.to_vec();
        ins.sort_unstable();
        ins.dedup();
        let in_base = match c.graph.base() {
            Some(b) => b.contains_sorted(&ins)?,
            None => vec![false; ins.len()],
        };
        for (t, _) in ins.iter().zip(in_base).filter(|x| x.1) {
            if let Some(r) = c.graph.position(t) {
                c.set_flag(r, false);
                c.graph.kill(t)?;
            }
        }
        drop(ins);
        drop(held);
        cx.out.insert_micros = cx.started.elapsed().as_micros() as u64;
        Ok(true)
    }

    fn saturated(
        &mut self,
        _cx: &mut TierCx<'_>,
        store: &TierStore<'a>,
        out: &mut Sink<'_, Triple, anyhow::Error>,
    ) -> anyhow::Result<()> {
        let g = &store.c.graph;
        for r in self.start..g.len() {
            if g.alive(r) {
                out.push(g.triple(r))?;
            }
        }
        Ok(())
    }

    fn pass_end(&mut self, _cx: &mut TierCx<'_>) -> anyhow::Result<()> {
        self.del = None;
        self.changed_lists = None;
        self.list_frontier.clear();
        self.list_frontier_held = None;
        self.overdeleted_touching_op.clear();
        self.overdeleted_held = None;
        Ok(())
    }

    fn annotate(
        &mut self,
        _cx: &mut TierCx<'_>,
        _store: &TierStore<'a>,
        _mode: Provenance,
        _f: &Triple,
        _r: &(),
        _premises: &[&Ann],
    ) -> anyhow::Result<Ann> {
        anyhow::bail!("the reasoner's maintenance keeps no annotations")
    }

    fn tick(&mut self, cx: &mut TierCx<'_>) -> anyhow::Result<()> {
        if cancelled(cx.limits) {
            anyhow::bail!("reasoning cancelled");
        }
        Ok(())
    }

    fn save(&self, w: &mut Writer, f: &Triple, _r: &()) -> sparkles_core::Result<()> {
        for x in f {
            w.u64(*x);
        }
        Ok(())
    }

    fn load(&self, r: &mut Reader) -> sparkles_core::Result<(Triple, ())> {
        Ok(([r.u64()?, r.u64()?, r.u64()?], ()))
    }
}

/// How many facts DRed would delete before deriving the survivors again: every fact with
/// a derivation that uses a removed explicit fact, transitively. For measurements; the
/// closure is unusable afterwards.
pub(crate) fn dred_overdeletion(
    c: &mut Closure,
    rules: &[CRule],
    limits: &Limits,
    removed: &[Triple],
) -> anyhow::Result<usize> {
    // the removed facts of the base tier, as derived facts that are deleted first
    let back: Vec<Triple> = removed
        .iter()
        .filter(|t| c.graph.position(t).is_none())
        .copied()
        .collect();
    c.graph.add_batch(back);
    let plans = Plans::new(&mut c.graph, rules)?;
    let mut gone: FxHashSet<Triple> = FxHashSet::default();
    let mut frontier: Vec<Triple> = removed
        .iter()
        .filter(|t| c.graph.position(t).is_some() && gone.insert(**t))
        .copied()
        .collect();
    while !frontier.is_empty() {
        let (next, _held) = consequences(c, rules, &plans, &frontier, limits, None)?;
        for t in &frontier {
            c.graph.kill(t)?;
        }
        frontier = next
            .into_iter()
            .filter(|t| c.graph.position(t).is_some() && gone.insert(*t))
            .collect();
    }
    Ok(gone.len())
}

// ------------------------------------------------------------------ plans ----

/// A plan that starts with the variables of a rule head bound, and those variables.
pub(crate) type BackPlan = (Vec<Step>, Vec<usize>);

/// The plans an update evaluates with, made once per update.
pub(crate) struct Plans {
    /// per rule and head: the plan with the head's variables bound (those that some atom
    /// binds), and those variables
    pub(crate) back: Vec<Vec<Option<BackPlan>>>,
    /// heads by constant predicate: (rule, head)
    pub(crate) by_pred: FxHashMap<u64, Vec<(usize, usize)>>,
    /// heads with a variable predicate
    pub(crate) any_pred: Vec<(usize, usize)>,
    /// per rule and atom: the plan that starts with that atom
    pub(crate) forward: Vec<Vec<Option<Vec<Step>>>>,
    /// an estimate of the plans' heap
    pub(crate) bytes: usize,
}

impl Plans {
    pub(crate) fn new(g: &mut Graph, rules: &[CRule]) -> Result<Plans, BudgetFailure> {
        Self::new_without(g, rules, |_| false)
    }

    /// Build forward and backward plans for every rule for which `skip(ri)` is false.
    pub(crate) fn new_without(
        g: &mut Graph,
        rules: &[CRule],
        skip: impl Fn(usize) -> bool,
    ) -> Result<Plans, BudgetFailure> {
        let mut p = Plans {
            back: Vec::with_capacity(rules.len()),
            by_pred: FxHashMap::default(),
            any_pred: Vec::new(),
            forward: Vec::with_capacity(rules.len()),
            bytes: 0,
        };
        let all = |r: &CRule| vec![(0, g.len()); r.atoms.len()];
        for (ri, r) in rules.iter().enumerate() {
            if skip(ri) {
                p.back.push(Vec::new());
                p.forward.push(Vec::new());
                continue;
            }
            let ranges = all(r);
            let mut in_atoms = vec![false; r.nvars];
            for a in &r.atoms {
                for s in [a.s, a.p, a.o] {
                    if let Slot::Var(v) = s {
                        in_atoms[v] = true;
                    }
                }
            }
            let mut heads = Vec::new();
            for (hi, h) in r.head.iter().enumerate() {
                let CHead::Triple(s, pr, o) = h else {
                    heads.push(None);
                    continue;
                };
                let mut pre: Vec<usize> = [s, pr, o]
                    .into_iter()
                    .filter_map(|x| match x {
                        Slot::Var(v) if in_atoms[*v] => Some(*v),
                        _ => None,
                    })
                    .collect();
                pre.sort_unstable();
                pre.dedup();
                let plan = engine::plan_with(r, &ranges, Some(g), None, &pre);
                if plan.is_some() {
                    match pr {
                        Slot::Const(c) => p.by_pred.entry(*c).or_default().push((ri, hi)),
                        _ => p.any_pred.push((ri, hi)),
                    }
                }
                heads.push(plan.map(|s| (s, pre)));
            }
            p.back.push(heads);
            p.forward.push(
                (0..r.atoms.len())
                    .map(|d| engine::plan_with(r, &ranges, Some(g), Some(d), &[]))
                    .collect(),
            );
        }
        // the indexes the plans use
        let (mut need_s, mut need_o) = (false, false);
        for (ri, r) in rules.iter().enumerate() {
            let plans = p.back[ri]
                .iter()
                .flatten()
                .map(|(s, pre)| (s, pre.as_slice()))
                .chain(p.forward[ri].iter().flatten().map(|s| (s, &[][..])));
            for (steps, pre) in plans {
                let (s, o) = engine::index_needs_with(r, steps, pre);
                need_s |= s;
                need_o |= o;
            }
        }
        // the plans' own heap: their steps and bound variables, a small part of a run
        p.bytes = p
            .back
            .iter()
            .flatten()
            .flatten()
            .map(|(s, pre)| s.len() * size_of::<Step>() + pre.len() * 8 + 64)
            .chain(
                p.forward
                    .iter()
                    .flatten()
                    .flatten()
                    .map(|s| s.len() * size_of::<Step>() + 32),
            )
            .sum::<usize>()
            + table_bytes(p.by_pred.capacity(), 32);
        if need_s {
            g.ensure_index(true, true)?;
        }
        if need_o {
            g.ensure_index(false, true)?;
        }
        Ok(p)
    }

    pub(crate) fn heads(&self, p: u64) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.by_pred
            .get(&p)
            .into_iter()
            .flatten()
            .chain(&self.any_pred)
            .copied()
    }
}

/// The heads (in the closure) of the rule instances with a body atom in `dead`, whose
/// rows are still live. Each instance the evaluations match counts as a visit of
/// `bound`.
fn consequences(
    c: &Closure,
    rules: &[CRule],
    plans: &Plans,
    dead: &[Triple],
    limits: &Limits,
    bound: Option<&Bound>,
) -> anyhow::Result<(Vec<Triple>, Held)> {
    let mut out = Vec::new();
    let mut held = consequences_into(c, rules, plans, dead, limits, bound, &mut |t| {
        out.push(t);
        Ok(())
    })?;
    held.try_grow(out.capacity() * size_of::<Triple>())?;
    Ok((out, held))
}

/// [`consequences`], giving each distinct head to `push` once. The returned charge is
/// what the heads' evaluation still holds, nothing once the heads are given.
#[allow(clippy::too_many_arguments)]
fn consequences_into(
    c: &Closure,
    rules: &[CRule],
    plans: &Plans,
    dead: &[Triple],
    limits: &Limits,
    bound: Option<&Bound>,
    push: &mut dyn FnMut(Triple) -> anyhow::Result<()>,
) -> anyhow::Result<Held> {
    let g = &c.graph;
    let rows: Vec<u32> = dead.iter().filter_map(|t| g.position(t)).collect();
    let mut work: Vec<(usize, usize, Vec<u32>)> = Vec::new();
    for (ri, r) in rules.iter().enumerate() {
        if plans.forward[ri].is_empty() {
            continue;
        }
        for (d, a) in r.atoms.iter().enumerate() {
            if plans.forward[ri][d].is_none() {
                continue;
            }
            let fits = |x: Slot, v: u64| !matches!(x, Slot::Const(k) if k != v);
            let mine: Vec<u32> = rows
                .iter()
                .copied()
                .filter(|&row| {
                    let t = g.triple(row);
                    fits(a.s, t[0]) && fits(a.p, t[1]) && fits(a.o, t[2])
                })
                .collect();
            for ch in mine.chunks(256) {
                work.push((ri, d, ch.to_vec()));
            }
        }
    }
    let abort = AtomicBool::new(false);
    let mut held = Held::try_new(
        &limits.budget,
        work.iter()
            .map(|w| w.2.len() * 4 + size_of::<(usize, usize, Vec<u32>)>())
            .sum(),
    )?;
    type Heads = (Vec<Triple>, Held, Option<anyhow::Error>);
    // each instance the evaluations match counts as a visit of the deletion
    let visit = |n: u64| -> anyhow::Result<()> {
        match bound {
            Some(b) => b.add(n).map_err(anyhow::Error::from),
            None => Ok(()),
        }
    };
    let visit: &engine::Visit<'_> = &visit;
    let mut heads: Vec<Heads> = work
        .par_iter()
        .map(|(ri, d, rows)| {
            let r = &rules[*ri];
            let steps = plans.forward[*ri][*d].as_ref().unwrap();
            let mut ev = Eval::new(
                g,
                &c.terms,
                r,
                steps,
                g.len(),
                &abort,
                limits.cancel.as_deref(),
                usize::MAX,
                Mode::Existing { visit: Some(visit) },
                &limits.budget,
            );
            let mut b = vec![0u64; r.nvars];
            ev.run(0, &mut b, Some(First::Rows(Cands::ids(rows))));
            if ev.err.is_none()
                && let Err(e) = visit(std::mem::take(&mut ev.visits))
            {
                ev.err = Some(e);
                abort.store(true, Ordering::Relaxed);
            }
            let (out, h) = ev.take_out();
            (out, h, ev.err.take())
        })
        .collect();
    if let Some(e) = preferred_error(heads.iter_mut().filter_map(|h| h.2.take())) {
        return Err(e);
    }
    let n: usize = heads.iter().map(|h| h.0.len()).sum();
    if let Some(b) = bound {
        // the heads are held now
        b.add(0)?;
    }
    // the heads, deduplicated
    held.try_grow(table_bytes(table_grown(0, 0, n), size_of::<Triple>()))?;
    let mut seen: FxHashSet<Triple> = FxHashSet::with_capacity_and_hasher(n, Default::default());
    // the errors were taken above
    for (h, hh, _) in heads {
        for t in h {
            if seen.insert(t) {
                push(t)?;
            }
        }
        drop(hh);
    }
    drop(seen);
    Ok(held.split(0))
}

/// The error that parallel work of the deletion returns: the first [`Fallback`], since a
/// part past the bound gives way to a full run, which may also avoid the other parts'
/// errors, or else the first error.
fn preferred_error(errs: impl Iterator<Item = anyhow::Error>) -> Option<anyhow::Error> {
    let mut first = None;
    for e in errs {
        if e.is::<Fallback>() {
            return Some(e);
        }
        first.get_or_insert(e);
    }
    first
}

// ------------------------------------------------------------------ proofs ----

/// The fewest facts a round of the deletion searches in parallel.
const PAR_SEARCH: usize = 2;

/// Search for other proofs of `facts`, the distinct facts of one round of the deletion,
/// in parallel, and push whether each has one to `out`, in the order of `facts`. `out`
/// has room for them. Each chunk's answers are charged to its search until they are
/// pushed.
///
/// The searches of one round see the same live facts, since the round deletes its
/// unprovable facts only after every search, and a search's answer does not depend on
/// what the others learned: a fact is proved from explicit facts only, and a fact is
/// unprovable when a search explored everything it reached without a proof. The
/// searches therefore split into chunks, each searched in order by a [`Search`] of its
/// own that also reads what `search` learned in earlier rounds, and what the chunks
/// learn joins `search` afterwards. The chunks repeat some work that one search would
/// share, and they read the base tier on every thread, which matters most when its
/// blocks are not decoded yet.
#[allow(clippy::too_many_arguments)]
fn prove_all(
    c: &Closure,
    rules: &[CRule],
    plans: &Plans,
    ops: Option<&DeletionOps>,
    explicit_lists: bool,
    abort: &AtomicBool,
    limits: &Limits,
    bound: &Bound,
    search: &mut Search,
    facts: &[Triple],
    out: &mut Vec<Option<()>>,
) -> anyhow::Result<()> {
    let budget = &limits.budget;
    let chunk = facts
        .len()
        .div_ceil(rayon::current_num_threads() * 4)
        .max(1);
    let mut parts: Vec<(Vec<bool>, Search)> = {
        let cx = Cx {
            c,
            rules,
            plans,
            ops,
            explicit_lists,
            abort,
            cancel: limits.cancel.as_deref(),
            budget,
            bound,
            known: Some(&*search),
        };
        facts
            .par_chunks(chunk)
            .map(|ch| {
                let mut s = Search::new(budget);
                // the chunk's answers, charged before they are allocated
                let mut found = Vec::new();
                match s.held.try_grow(ch.len() * size_of::<bool>()) {
                    Ok(()) => found.reserve_exact(ch.len()),
                    Err(e) => s.err = Some(e.into()),
                }
                for &t in ch {
                    if s.err.is_some() || abort.load(Ordering::Relaxed) {
                        break;
                    }
                    found.push(s.prove(&cx, t));
                }
                if s.err.is_some() {
                    abort.store(true, Ordering::Relaxed);
                }
                // the chunk's blocks go now, not when the whole round ends
                s.memo = Memo::default();
                (found, s)
            })
            .collect()
    };
    // the errors first, before anything is absorbed, whose growth could fail too
    if let Some(e) = preferred_error(parts.iter_mut().filter_map(|p| p.1.err.take())) {
        return Err(e);
    }
    for (found, s) in parts {
        out.extend(found.iter().map(|&p| p.then_some(())));
        // the answers' charge goes with the chunk's search
        drop(found);
        search.absorb(s)?;
    }
    if cancelled(limits) {
        anyhow::bail!("reasoning cancelled");
    }
    Ok(())
}

/// How deep the search recurses before it queues the facts it reaches instead.
const MAX_DEPTH: u32 = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// reached, rule instances not explored yet
    Fresh,
    /// its instances are being explored
    Open,
    /// every instance explored, no proof found (yet)
    Explored,
    Proved,
}

struct Node {
    t: Triple,
    state: State,
    /// the instances with this fact in the body
    uses: Vec<u32>,
}

struct Inst {
    head: u32,
    /// body facts not proved yet
    open: u32,
}

/// What a search reads.
struct Cx<'a> {
    c: &'a Closure,
    rules: &'a [CRule],
    plans: &'a Plans,
    /// operator-maintained closure relations for fast explicit-edge reachability proofs
    ops: Option<&'a DeletionOps>,
    /// read RDF lists only from explicit (`base` and flagged) rows
    explicit_lists: bool,
    abort: &'a AtomicBool,
    cancel: Option<&'a AtomicBool>,
    budget: &'a Arc<Budget>,
    /// the deletion's cost and its bound
    bound: &'a Bound,
    /// what the searches of earlier rounds learned, when this search is one of a round's
    /// parallel ones
    known: Option<&'a Search>,
}

/// Provability of facts among the live ones: what is known across the searches of one
/// update, and the state of the current search. Its buffers are charged to the run's
/// budget before they grow, and the first refusal stops the search.
struct Search {
    proved: FxHashSet<Triple>,
    unprovable: FxHashSet<Triple>,
    /// subset of `unprovable` whose failed proof encountered a live unproved fact of an
    /// operator-governed predicate (eligible for 1-step revival after saturation)
    unprovable_touched_op: FxHashSet<Triple>,
    /// live unproved operator-governed facts actually touched by failed proofs (used at
    /// saturation to skip `revive_overdeleted` when none survived Phase 1)
    op_facts_touched: FxHashSet<Triple>,
    /// operator-governed facts touched during the current search
    op_touched_now: Vec<Triple>,
    /// whether the search currently running encountered a live unproved operator fact
    touched_op_now: bool,
    /// reusable charged scratch workspace for directed explicit-edge reachability probes
    reach_scratch: ReachScratch,
    checked: u64,
    nodes: Vec<Node>,
    node_of: FxHashMap<Triple, u32>,
    insts: Vec<Inst>,
    queue: Vec<u32>,
    /// the buffers above and the nodes' lists of uses
    held: Held,
    /// of `held`: the nodes' lists of uses, freed when a search starts
    uses: usize,
    /// the first refusal, the first error of an evaluation, or the [`Fallback`] of a
    /// deletion past its bound
    err: Option<anyhow::Error>,
    /// the rule instances visited and not yet added to [`Cx::bound`]
    pending: u64,
    /// the base-tier blocks the search's evaluations read last, lent to each in turn:
    /// the blocks of a snapshot do not change, and the evaluations of one search read
    /// near each other
    memo: Memo,
}

/// The bytes of the allocation a vector of capacity `cap` with `len` elements of `elem`
/// bytes grows to when `add` more do not fit, or 0 when they fit (the standard
/// library's doubling rule).
fn vec_grown(cap: usize, len: usize, add: usize, elem: usize) -> usize {
    let want = len.saturating_add(add);
    if want <= cap {
        return 0;
    }
    let min = if elem == 1 {
        8
    } else if elem <= 1024 {
        4
    } else {
        1
    };
    want.max(cap.saturating_mul(2))
        .max(min)
        .saturating_mul(elem)
}

impl Search {
    fn new(budget: &Arc<Budget>) -> Search {
        Search {
            proved: FxHashSet::default(),
            unprovable: FxHashSet::default(),
            unprovable_touched_op: FxHashSet::default(),
            op_facts_touched: FxHashSet::default(),
            op_touched_now: Vec::new(),
            touched_op_now: false,
            reach_scratch: ReachScratch::new(budget),
            checked: 0,
            nodes: Vec::new(),
            node_of: FxHashMap::default(),
            insts: Vec::new(),
            queue: Vec::new(),
            held: Held::new(budget),
            uses: 0,
            err: None,
            pending: 0,
            memo: Memo::default(),
        }
    }

    /// Add the visits counted on their own to the deletion's; false when they pass its
    /// bound, and the search stops with the [`Fallback`].
    fn flush(&mut self, cx: &Cx<'_>) -> bool {
        match cx.bound.add(std::mem::take(&mut self.pending)) {
            Ok(()) => true,
            Err(f) => {
                self.err.get_or_insert(f.into());
                false
            }
        }
    }

    /// Whether `t` is proved by an operator's confirmed explicit edges; records any
    /// budget, bound, or cancellation error into `self.err`.
    fn proved_by_base(&mut self, cx: &Cx<'_>, t: &Triple) -> bool {
        let Some(ops) = cx.ops else {
            return false;
        };
        let bound = cx.bound;
        let mut visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
        match ops.proved_by_base(t, &mut self.reach_scratch, cx.cancel, &mut visit) {
            Ok(v) => v,
            Err(e) => {
                self.err.get_or_insert(e);
                false
            }
        }
    }

    /// Record an operator-governed fact touched during the current search, charging
    /// growth before pushing.
    fn record_op_touched(&mut self, t: Triple) -> bool {
        let (cap, len) = (self.op_touched_now.capacity(), self.op_touched_now.len());
        let Some(old) = self.vec_growth(cap, len, 1, size_of::<Triple>()) else {
            return false;
        };
        self.op_touched_now.push(t);
        self.held.shrink(old);
        true
    }

    /// Whether this search, or the searches of earlier rounds, proved `t`.
    #[inline]
    fn is_proved(&self, cx: &Cx<'_>, t: &Triple) -> bool {
        self.proved.contains(t) || cx.known.is_some_and(|k| k.proved.contains(t))
    }

    /// Whether this search, or the searches of earlier rounds, found `t` unprovable.
    #[inline]
    fn is_unprovable(&self, cx: &Cx<'_>, t: &Triple) -> bool {
        self.unprovable.contains(t) || cx.known.is_some_and(|k| k.unprovable.contains(t))
    }

    #[inline]
    fn is_unprovable_touched_op(&self, cx: &Cx<'_>, t: &Triple) -> bool {
        self.unprovable_touched_op.contains(t)
            || cx
                .known
                .is_some_and(|k| k.unprovable_touched_op.contains(t))
    }

    /// Take in what another search of the same round learned, charging the growth of
    /// the sets before it is allocated.
    fn absorb(&mut self, other: Search) -> Result<(), BudgetFailure> {
        let fact = size_of::<Triple>();
        for (mine, theirs) in [
            (&mut self.proved, &other.proved),
            (&mut self.unprovable, &other.unprovable),
            (
                &mut self.unprovable_touched_op,
                &other.unprovable_touched_op,
            ),
            (&mut self.op_facts_touched, &other.op_facts_touched),
        ] {
            let (cap, len) = (mine.capacity(), mine.len());
            let to = table_grown(cap, len, theirs.len());
            if to != cap {
                self.held.try_grow(table_bytes(to, fact))?;
                mine.reserve(theirs.len());
                self.held.shrink(table_bytes(cap, fact));
            }
            mine.extend(theirs.iter().copied());
        }
        self.checked += other.checked;
        Ok(())
    }

    /// Charge `n` bytes; false when they do not fit, and the search stops.
    fn charge(&mut self, n: usize) -> bool {
        if self.err.is_some() {
            return false;
        }
        match self.held.try_grow(n) {
            Ok(()) => true,
            Err(e) => {
                self.err = Some(e.into());
                false
            }
        }
    }

    /// The new allocation (charged) and the old one (to release once the elements are
    /// in) of a vector growing for `add` more elements: `Some((0, 0))` when they fit,
    /// `None` when the growth is refused.
    fn vec_growth(&mut self, cap: usize, len: usize, add: usize, elem: usize) -> Option<usize> {
        match vec_grown(cap, len, add, elem) {
            0 => Some(0),
            n if self.charge(n) => Some(cap * elem),
            _ => None,
        }
    }

    /// The same for a hash table with elements of `elem` bytes.
    fn table_growth(&mut self, cap: usize, len: usize, add: usize, elem: usize) -> Option<usize> {
        let to = table_grown(cap, len, add);
        if to == cap {
            return Some(0);
        }
        self.charge(table_bytes(to, elem))
            .then(|| table_bytes(cap, elem))
    }

    /// Whether a live fact follows from the explicit facts. A refusal leaves the answer
    /// false with `err` set, and the caller stops.
    fn prove(&mut self, cx: &Cx<'_>, t: Triple) -> bool {
        if self.err.is_some() {
            return false;
        }
        if self.is_proved(cx, &t) {
            return true;
        }
        if self.is_unprovable(cx, &t) {
            return false;
        }
        let Some(row) = cx.c.graph.position(&t) else {
            return false;
        };
        let fact = size_of::<Triple>();
        let fast = cx.c.flagged(row) || self.proved_by_base(cx, &t);
        if self.err.is_some() {
            return false;
        }
        if fast {
            let (cap, len) = (self.proved.capacity(), self.proved.len());
            let Some(old) = self.table_growth(cap, len, 1, fact) else {
                return false;
            };
            self.proved.insert(t);
            self.held.shrink(old);
            return true;
        }
        self.checked += 1;
        self.touched_op_now = false;
        self.op_touched_now.clear();
        self.nodes.clear();
        self.node_of.clear();
        self.insts.clear();
        self.queue.clear();
        self.held.shrink(std::mem::take(&mut self.uses));
        let Some(root) = self.node(t) else {
            return false;
        };
        self.expand(cx, root, root, 0);
        while self.err.is_none()
            && self.nodes[root as usize].state != State::Proved
            && let Some(n) = self.queue.pop()
        {
            if self.nodes[n as usize].state == State::Fresh {
                self.expand(cx, n, root, 0);
            }
        }
        if self.err.is_none() {
            self.flush(cx);
        }
        if self.err.is_some() {
            return false;
        }
        let proved = self.nodes[root as usize].state == State::Proved;
        // what the search learned, charged before it is kept
        let gained = self
            .nodes
            .iter()
            .filter(|n| n.state == State::Proved)
            .count();
        let lost = if proved { 0 } else { self.nodes.len() - gained };
        let touched = !proved && self.touched_op_now;
        let op_touched_n = if touched {
            self.op_touched_now.len()
        } else {
            0
        };
        let (pc, pl) = (self.proved.capacity(), self.proved.len());
        let (uc, ul) = (self.unprovable.capacity(), self.unprovable.len());
        let (tc, tl) = (
            self.unprovable_touched_op.capacity(),
            self.unprovable_touched_op.len(),
        );
        let (oc, ol) = (
            self.op_facts_touched.capacity(),
            self.op_facts_touched.len(),
        );
        let (pt, ut, tt, ot) = (
            table_grown(pc, pl, gained),
            table_grown(uc, ul, lost),
            if touched {
                table_grown(tc, tl, lost)
            } else {
                tc
            },
            if op_touched_n > 0 {
                table_grown(oc, ol, op_touched_n)
            } else {
                oc
            },
        );
        let new = if pt == pc { 0 } else { table_bytes(pt, fact) }
            + if ut == uc { 0 } else { table_bytes(ut, fact) }
            + if tt == tc { 0 } else { table_bytes(tt, fact) }
            + if ot == oc { 0 } else { table_bytes(ot, fact) };
        let old = if pt == pc { 0 } else { table_bytes(pc, fact) }
            + if ut == uc { 0 } else { table_bytes(uc, fact) }
            + if tt == tc { 0 } else { table_bytes(tc, fact) }
            + if ot == oc { 0 } else { table_bytes(oc, fact) };
        if !self.charge(new) {
            return false;
        }
        self.proved.reserve(gained);
        self.unprovable.reserve(lost);
        if touched {
            self.unprovable_touched_op.reserve(lost);
            if op_touched_n > 0 {
                self.op_facts_touched.reserve(op_touched_n);
                self.op_facts_touched
                    .extend(self.op_touched_now.iter().copied());
            }
        }
        self.held.shrink(old);
        for n in &self.nodes {
            match n.state {
                State::Proved => {
                    self.proved.insert(n.t);
                }
                // with no proof of the root, the search explored everything it reached
                _ if !proved => {
                    self.unprovable.insert(n.t);
                    if touched {
                        self.unprovable_touched_op.insert(n.t);
                    }
                }
                _ => {}
            }
        }
        proved
    }

    /// The node of `t`, made (and queued) when it is new; `None` when its room is
    /// refused.
    fn node(&mut self, t: Triple) -> Option<u32> {
        if let Some(&n) = self.node_of.get(&t) {
            return Some(n);
        }
        let n = self.nodes.len() as u32;
        let (nc, nl) = (self.nodes.capacity(), self.nodes.len());
        let a = self.vec_growth(nc, nl, 1, size_of::<Node>())?;
        let (qc, ql) = (self.queue.capacity(), self.queue.len());
        let b = self.vec_growth(qc, ql, 1, 4)?;
        let (mc, ml) = (self.node_of.capacity(), self.node_of.len());
        let c = self.table_growth(mc, ml, 1, size_of::<(Triple, u32)>())?;
        self.nodes.push(Node {
            t,
            state: State::Fresh,
            uses: Vec::new(),
        });
        self.node_of.insert(t, n);
        self.queue.push(n);
        self.held.shrink(a + b + c);
        Some(n)
    }

    /// Record that instance `inst` uses node `m`; false when the room is refused.
    fn used_by(&mut self, m: u32, inst: u32) -> bool {
        let u = &self.nodes[m as usize].uses;
        let (cap, len) = (u.capacity(), u.len());
        let grown = vec_grown(cap, len, 1, 4);
        if grown > 0 {
            if !self.charge(grown) {
                return false;
            }
            self.held.shrink(cap * 4);
            self.uses += grown - cap * 4;
        }
        self.nodes[m as usize].uses.push(inst);
        true
    }

    /// Room for one more instance; false when it is refused.
    fn room_for_instance(&mut self) -> bool {
        let (cap, len) = (self.insts.capacity(), self.insts.len());
        match self.vec_growth(cap, len, 1, size_of::<Inst>()) {
            Some(old) => {
                // the push follows at once
                self.held.shrink(old);
                true
            }
            None => false,
        }
    }

    fn mark_proved(&mut self, n: u32) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            let node = &mut self.nodes[x as usize];
            if node.state == State::Proved {
                continue;
            }
            node.state = State::Proved;
            for i in std::mem::take(&mut node.uses) {
                let inst = &mut self.insts[i as usize];
                inst.open -= 1;
                if inst.open == 0 {
                    stack.push(inst.head);
                }
            }
        }
    }

    fn done(&self, n: u32, root: u32) -> bool {
        self.nodes[n as usize].state == State::Proved
            || self.nodes[root as usize].state == State::Proved
    }

    /// Explore the rule instances that derive node `n`, depth first.
    fn expand(&mut self, cx: &Cx<'_>, n: u32, root: u32, depth: u32) {
        self.nodes[n as usize].state = State::Open;
        let t = self.nodes[n as usize].t;
        let g = &cx.c.graph;
        if cx.ops.is_some_and(|ops| ops.is_governed(t[1], g)) {
            self.touched_op_now = true;
            if !self.record_op_touched(t) {
                return;
            }
        }
        for (ri, hi) in cx.plans.heads(t[1]) {
            let Some((steps, pre)) = &cx.plans.back[ri][hi] else {
                continue;
            };
            let rule = &cx.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            // a nested expansion takes a memo of its own meanwhile
            let memo = std::mem::take(&mut self.memo);
            let (failed, memo) = {
                let mut sink = |body: &[Triple], implicit: &[Triple], _: &[u64]| -> bool {
                    self.instance(cx, n, root, depth, body, implicit)
                };
                let mut ev = Eval::new(
                    g,
                    &cx.c.terms,
                    rule,
                    steps,
                    g.len(),
                    cx.abort,
                    cx.cancel,
                    usize::MAX,
                    Mode::Instances {
                        head: hi,
                        target: t,
                        sink: &mut sink,
                    },
                    cx.budget,
                );
                ev.set_explicit_lists(cx.explicit_lists);
                ev.lend_memo(memo);
                ev.run(0, &mut b, None);
                (ev.err.take(), ev.take_memo())
            };
            self.memo = memo;
            if let Some(e) = failed {
                self.err.get_or_insert(e);
            }
            if self.err.is_some() {
                break;
            }
            if self.done(n, root) || cx.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                break;
            }
        }
        let node = &mut self.nodes[n as usize];
        if node.state == State::Open {
            node.state = State::Explored;
        }
    }

    /// One instance deriving node `n`; returns false to stop exploring `n`.
    fn instance(
        &mut self,
        cx: &Cx<'_>,
        n: u32,
        root: u32,
        depth: u32,
        matched: &[Triple],
        implicit: &[Triple],
    ) -> bool {
        if self.err.is_some() {
            return false;
        }
        self.pending += 1;
        if self.pending >= VISIT_BATCH && !self.flush(cx) {
            return false;
        }
        let t = self.nodes[n as usize].t;
        let body = matched.iter().chain(implicit).copied();
        let mut facts: smallvec::SmallVec<[Triple; 4]> = smallvec::SmallVec::new();
        for u in body {
            // an instance that uses its own head or an unprovable fact proves nothing
            if u == t {
                return true;
            }
            if self.is_unprovable(cx, &u) {
                if self.is_unprovable_touched_op(cx, &u) {
                    self.touched_op_now = true;
                }
                if cx.ops.is_some_and(|ops| ops.is_governed(u[1], &cx.c.graph)) {
                    self.touched_op_now = true;
                    if !self.record_op_touched(u) {
                        return false;
                    }
                }
                return true;
            }
            // a body fact the derived tier does not hold is from the base tier
            if self.is_proved(cx, &u) || cx.c.explicit_held(&u) {
                continue;
            }
            let by_base = self.proved_by_base(cx, &u);
            if self.err.is_some() {
                return false;
            }
            if by_base {
                continue;
            }
            facts.push(u);
        }
        if !self.room_for_instance() {
            return false;
        }
        let inst = self.insts.len() as u32;
        self.insts.push(Inst { head: n, open: 0 });
        let mut open = 0;
        let mut fresh: smallvec::SmallVec<[u32; 4]> = smallvec::SmallVec::new();
        for u in facts {
            let known = self.node_of.contains_key(&u);
            let Some(m) = self.node(u) else {
                return false;
            };
            if self.nodes[m as usize].state == State::Proved {
                continue;
            }
            if !known {
                fresh.push(m);
            }
            open += 1;
            if !self.used_by(m, inst) {
                return false;
            }
        }
        self.insts[inst as usize].open = open;
        if open == 0 {
            self.mark_proved(n);
            return false;
        }
        if depth < MAX_DEPTH {
            for m in fresh {
                if self.nodes[m as usize].state == State::Fresh {
                    self.expand(cx, m, root, depth + 1);
                }
                if self.done(n, root) {
                    return false;
                }
            }
        }
        true
    }
}

// ---------------------------------------------------- per-list & cost helpers ----

/// Push `x` onto `v`, charging any reallocation to `held` before it occurs.
fn push_charged<T>(v: &mut Vec<T>, x: T, held: &mut Held) -> Result<(), BudgetFailure> {
    let (cap, len) = (v.capacity(), v.len());
    let grown = vec_grown(cap, len, 1, size_of::<T>());
    if grown > 0 {
        held.try_grow(grown)?;
        v.reserve_exact((grown / size_of::<T>()) - len);
        held.shrink(cap * size_of::<T>());
    }
    v.push(x);
    Ok(())
}

/// Insert `x` into `set`, charging any table growth to `held` before it occurs.
fn insert_set_charged<T: Eq + std::hash::Hash>(
    set: &mut FxHashSet<T>,
    x: T,
    held: &mut Held,
) -> Result<bool, BudgetFailure> {
    if set.contains(&x) {
        return Ok(false);
    }
    let (cap, len) = (set.capacity(), set.len());
    let to = table_grown(cap, len, 1);
    if to != cap {
        held.try_grow(table_bytes(to, size_of::<T>()))?;
        set.reserve(1);
        held.shrink(table_bytes(cap, size_of::<T>()));
    }
    Ok(set.insert(x))
}

/// Reserve room in `map` for key `k` if absent, charging any table growth before it occurs.
fn reserve_map_charged<K: Eq + std::hash::Hash, V>(
    map: &mut FxHashMap<K, V>,
    k: &K,
    held: &mut Held,
) -> Result<(), BudgetFailure> {
    if map.contains_key(k) {
        return Ok(());
    }
    let (cap, len) = (map.capacity(), map.len());
    let to = table_grown(cap, len, 1);
    if to != cap {
        held.try_grow(table_bytes(to, size_of::<(K, V)>()))?;
        map.reserve(1);
        held.shrink(table_bytes(cap, size_of::<(K, V)>()));
    }
    Ok(())
}

/// List-head slots (`args[0]`) read by list builtins in `r`.
fn list_head_slots(r: &CRule) -> Vec<Slot> {
    use crate::builtins::BuiltinKind as B;
    let mut out = Vec::new();
    for b in &r.builtins {
        if matches!(
            b.kind,
            B::ListMember
                | B::ListContains
                | B::ListNotContains
                | B::ListLength
                | B::ListEntry
                | B::ListForAll
        ) && let Some(&s) = b.args.first()
            && !out.contains(&s)
        {
            out.push(s);
        }
    }
    out
}

/// Summary of removed `rdf:first` or `rdf:rest` objects for one list cell: the lowest
/// removed object id and whether more than one distinct object was removed.
#[derive(Clone, Copy)]
struct RemovedObj {
    min: u64,
    multi: bool,
}

fn record_removed_obj(
    map: &mut FxHashMap<u64, RemovedObj>,
    cell: u64,
    obj: u64,
    held: &mut Held,
) -> Result<(), BudgetFailure> {
    reserve_map_charged(map, &cell, held)?;
    map.entry(cell)
        .and_modify(|e| {
            if obj != e.min {
                e.multi = true;
                if obj < e.min {
                    e.min = obj;
                }
            }
        })
        .or_insert(RemovedObj {
            min: obj,
            multi: false,
        });
    Ok(())
}

/// Per-cell lookup caches shared across affected heads during `changed_lists` so a
/// shared list tail (including one whose `rdf:first`/`rdf:rest` values were all removed
/// and now sit as unflagged rows in `c.graph`) is scanned at most once per snapshot.
#[derive(Default)]
struct ListCellCache {
    new_first: FxHashMap<u64, Option<u64>>,
    new_rest: FxHashMap<u64, Option<u64>>,
    old_first: FxHashMap<u64, Option<u64>>,
    old_rest: FxHashMap<u64, Option<u64>>,
}

/// Walk an explicit RDF list at `head` in the current snapshot with memory charging,
/// cancellation polling, per-cell caching, and visit counting against `visit`.
#[allow(clippy::too_many_arguments)]
fn new_explicit_list(
    g: &Graph,
    memo: &mut Memo,
    head: u64,
    first: u64,
    rest: u64,
    nil: u64,
    cache: &mut ListCellCache,
    cache_held: &mut Held,
    limits: &Limits,
    visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
) -> anyhow::Result<Option<Vec<u64>>> {
    const MAX_LEN: usize = 1 << 20;
    let mut held = Held::new(&limits.budget);
    let mut out: Vec<u64> = Vec::new();
    let mut seen: FxHashSet<u64> = FxHashSet::default();
    let mut cur = head;
    let mut pending: u64 = 0;
    let first_obj = |memo: &mut Memo,
                     s: u64,
                     p: u64,
                     cell_cache: &mut FxHashMap<u64, Option<u64>>,
                     cache_held: &mut Held,
                     pending: &mut u64,
                     visit: &mut dyn FnMut(u64) -> anyhow::Result<()>|
     -> anyhow::Result<Option<u64>> {
        if let Some(&cached) = cell_cache.get(&s) {
            return Ok(cached);
        }
        if let Some(b) = g.base() {
            let mut found = None;
            let mut base_meter = |n: u64| -> anyhow::Result<()> {
                visit(n)?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
                Ok(())
            };
            b.for_each_with_metered(memo, Some(s), Some(p), None, &mut base_meter, |t| {
                found = Some(t[2]);
                false
            })?;
            if found.is_some() {
                reserve_map_charged(cell_cache, &s, cache_held)?;
                cell_cache.insert(s, found);
                return Ok(found);
            }
        }
        let mut best: Option<(u32, u64)> = None;
        for i in g.cands(Some(s), Some(p), None, g.offset(), g.len()).iter() {
            *pending += 1;
            if *pending >= VISIT_BATCH {
                visit(std::mem::take(pending))?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
            }
            if g.alive(i) && g.flagged(i) {
                let t = g.triple(i);
                if t[0] == s && t[1] == p && best.is_none_or(|(bi, _)| i < bi) {
                    best = Some((i, t[2]));
                }
            }
        }
        let res = best.map(|(_, o)| o);
        reserve_map_charged(cell_cache, &s, cache_held)?;
        cell_cache.insert(s, res);
        Ok(res)
    };
    while cur != nil {
        pending += 1;
        if pending >= VISIT_BATCH {
            visit(std::mem::take(&mut pending))?;
            if cancelled(limits) {
                anyhow::bail!("reasoning cancelled");
            }
        }
        if !insert_set_charged(&mut seen, cur, &mut held)? || out.len() > MAX_LEN {
            if pending > 0 {
                visit(pending)?;
            }
            return Ok(None);
        }
        let Some(f) = first_obj(
            memo,
            cur,
            first,
            &mut cache.new_first,
            cache_held,
            &mut pending,
            visit,
        )?
        else {
            if pending > 0 {
                visit(pending)?;
            }
            return Ok(None);
        };
        push_charged(&mut out, f, &mut held)?;
        let Some(r) = first_obj(
            memo,
            cur,
            rest,
            &mut cache.new_rest,
            cache_held,
            &mut pending,
            visit,
        )?
        else {
            if pending > 0 {
                visit(pending)?;
            }
            return Ok(None);
        };
        cur = r;
    }
    if pending > 0 {
        visit(pending)?;
    }
    Ok(Some(out))
}

/// Read an explicit RDF list at `head` as it existed BEFORE the update: `base` + flagged
/// rows, minus `inserted_list`, plus `removed_first`/`removed_rest`. Returns `Err(Fallback)`
/// if a branching list cell changed across multiple input graphs where the old graph
/// rank of competing objects cannot be determined from union deltas alone.
#[allow(clippy::too_many_arguments)]
fn old_explicit_list(
    g: &Graph,
    memo: &mut Memo,
    head: u64,
    first: u64,
    rest: u64,
    nil: u64,
    inserted_list: &FxHashSet<Triple>,
    removed_first: &FxHashMap<u64, RemovedObj>,
    removed_rest: &FxHashMap<u64, RemovedObj>,
    multi_graph: bool,
    cache: &mut ListCellCache,
    cache_held: &mut Held,
    limits: &Limits,
    visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
) -> anyhow::Result<Option<Vec<u64>>> {
    const MAX_LEN: usize = 1 << 20;
    let find_obj = |memo: &mut Memo,
                    s: u64,
                    p: u64,
                    rem: &FxHashMap<u64, RemovedObj>,
                    cell_cache: &mut FxHashMap<u64, Option<u64>>,
                    cache_held: &mut Held,
                    pending: &mut u64,
                    visit: &mut dyn FnMut(u64) -> anyhow::Result<()>|
     -> anyhow::Result<Option<u64>> {
        if let Some(&cached) = cell_cache.get(&s) {
            return Ok(cached);
        }
        let rem_entry = rem.get(&s).copied();
        let rem_o = rem_entry.map(|r| r.min);
        if let Some(b) = g.base() {
            let is_multi = multi_graph || b.passes() > 1;
            if is_multi && rem_entry.is_some_and(|r| r.multi) {
                return Err(Fallback(
                    "a branching RDF list changed across multiple input graphs".into(),
                )
                .into());
            }
            let mut base_o = None;
            let mut base_meter = |n: u64| -> anyhow::Result<()> {
                visit(n)?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
                Ok(())
            };
            b.for_each_with_metered(memo, Some(s), Some(p), None, &mut base_meter, |t| {
                if !inserted_list.contains(&t) {
                    base_o = Some(t[2]);
                    return false;
                }
                true
            })?;
            let resolved = match (base_o, rem_o) {
                (Some(bo), Some(ro)) => {
                    if is_multi {
                        return Err(Fallback(
                            "a branching RDF list changed across multiple input graphs".into(),
                        )
                        .into());
                    }
                    // For a single input graph, `Base::for_each_with(Some(s), Some(p), None)`
                    // scans `Perm::Gspo`, yielding objects in ascending id order.
                    Some(bo.min(ro))
                }
                (Some(bo), None) => Some(bo),
                (None, Some(ro)) => Some(ro),
                (None, None) => None,
            };
            if resolved.is_some() {
                reserve_map_charged(cell_cache, &s, cache_held)?;
                cell_cache.insert(s, resolved);
                return Ok(resolved);
            }
        }
        let mut best: Option<(u32, u64)> = None;
        for i in g.cands(Some(s), Some(p), None, g.offset(), g.len()).iter() {
            *pending += 1;
            if *pending >= VISIT_BATCH {
                visit(std::mem::take(pending))?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
            }
            if g.alive(i) && g.flagged(i) {
                let t = g.triple(i);
                if t[0] == s
                    && t[1] == p
                    && !inserted_list.contains(&t)
                    && best.is_none_or(|(bi, _)| i < bi)
                {
                    best = Some((i, t[2]));
                }
            }
        }
        let res = best.map(|(_, o)| o).or(rem_o);
        reserve_map_charged(cell_cache, &s, cache_held)?;
        cell_cache.insert(s, res);
        Ok(res)
    };
    let mut held = Held::new(&limits.budget);
    let mut out: Vec<u64> = Vec::new();
    let mut cur = head;
    let mut seen: FxHashSet<u64> = FxHashSet::default();
    let mut pending: u64 = 0;
    while cur != nil {
        pending += 1;
        if pending >= VISIT_BATCH {
            visit(std::mem::take(&mut pending))?;
            if cancelled(limits) {
                anyhow::bail!("reasoning cancelled");
            }
        }
        if !insert_set_charged(&mut seen, cur, &mut held)? || out.len() > MAX_LEN {
            if pending > 0 {
                visit(pending)?;
            }
            return Ok(None);
        }
        let Some(f) = find_obj(
            memo,
            cur,
            first,
            removed_first,
            &mut cache.old_first,
            cache_held,
            &mut pending,
            visit,
        )?
        else {
            if pending > 0 {
                visit(pending)?;
            }
            return Ok(None);
        };
        push_charged(&mut out, f, &mut held)?;
        let Some(r) = find_obj(
            memo,
            cur,
            rest,
            removed_rest,
            &mut cache.old_rest,
            cache_held,
            &mut pending,
            visit,
        )?
        else {
            if pending > 0 {
                visit(pending)?;
            }
            return Ok(None);
        };
        cur = r;
    }
    if pending > 0 {
        visit(pending)?;
    }
    Ok(Some(out))
}

/// Find every explicit RDF list head whose member sequence changed between the old and
/// new snapshots, walking backward along `rdf:rest` in both snapshots (F16 §7.3).
fn changed_lists(
    c: &Closure,
    inserted: &[Triple],
    removed: &[Triple],
    multi_graph: bool,
    limits: &Limits,
    bound: &Bound,
) -> anyhow::Result<ChangedLists> {
    let mut visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
    changed_lists_metered(c, inserted, removed, multi_graph, limits, &mut visit)
}

fn changed_lists_metered(
    c: &Closure,
    inserted: &[Triple],
    removed: &[Triple],
    multi_graph: bool,
    limits: &Limits,
    visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
) -> anyhow::Result<ChangedLists> {
    if cancelled(limits) {
        anyhow::bail!("reasoning cancelled");
    }
    let budget = &limits.budget;
    let (first, rest, nil) = (c.terms.rdf_first, c.terms.rdf_rest, c.terms.rdf_nil);
    let is_list = |t: &Triple| t[1] == first || t[1] == rest;
    let mut work_held = Held::new(budget);
    let mut kept_held = Held::new(budget);
    let mut heads: Vec<u64> = Vec::new();
    let mut old_members: FxHashMap<u64, Option<std::rc::Rc<[u64]>>> = FxHashMap::default();
    let g = &c.graph;
    let mut memo = Memo::default();
    {
        let mut inserted_list: FxHashSet<Triple> = FxHashSet::default();
        let mut removed_first: FxHashMap<u64, RemovedObj> = FxHashMap::default();
        let mut removed_rest: FxHashMap<u64, RemovedObj> = FxHashMap::default();
        let mut removed_rev_rest: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
        let mut affected: FxHashSet<u64> = FxHashSet::default();
        let mut queue: Vec<u64> = Vec::new();
        let mut cell_cache = ListCellCache::default();
        let mut pending: u64 = 0;
        for &t in inserted {
            pending += 1;
            if pending >= VISIT_BATCH {
                visit(std::mem::take(&mut pending))?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
            }
            if !is_list(&t) {
                continue;
            }
            insert_set_charged(&mut inserted_list, t, &mut work_held)?;
            if insert_set_charged(&mut affected, t[0], &mut work_held)? {
                push_charged(&mut queue, t[0], &mut work_held)?;
            }
        }
        for &t in removed {
            pending += 1;
            if pending >= VISIT_BATCH {
                visit(std::mem::take(&mut pending))?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
            }
            if !is_list(&t) {
                continue;
            }
            if t[1] == first {
                record_removed_obj(&mut removed_first, t[0], t[2], &mut work_held)?;
            } else {
                record_removed_obj(&mut removed_rest, t[0], t[2], &mut work_held)?;
                reserve_map_charged(&mut removed_rev_rest, &t[2], &mut work_held)?;
                push_charged(
                    removed_rev_rest.entry(t[2]).or_default(),
                    t[0],
                    &mut work_held,
                )?;
            }
            if insert_set_charged(&mut affected, t[0], &mut work_held)? {
                push_charged(&mut queue, t[0], &mut work_held)?;
            }
        }
        visit(std::mem::take(&mut pending))?;
        if cancelled(limits) {
            anyhow::bail!("reasoning cancelled");
        }
        while let Some(cell) = queue.pop() {
            pending += 1;
            if pending >= VISIT_BATCH {
                visit(std::mem::take(&mut pending))?;
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
            }
            if let Some(base) = g.base() {
                let mut err: Option<anyhow::Error> = None;
                let mut base_meter = |n: u64| -> anyhow::Result<()> {
                    visit(n)?;
                    if cancelled(limits) {
                        anyhow::bail!("reasoning cancelled");
                    }
                    Ok(())
                };
                base.for_each_with_metered(
                    &mut memo,
                    None,
                    Some(rest),
                    Some(cell),
                    &mut base_meter,
                    |t| match insert_set_charged(&mut affected, t[0], &mut work_held) {
                        Ok(true) => match push_charged(&mut queue, t[0], &mut work_held) {
                            Ok(()) => true,
                            Err(e) => {
                                err = Some(e.into());
                                false
                            }
                        },
                        Ok(false) => true,
                        Err(e) => {
                            err = Some(e.into());
                            false
                        }
                    },
                )?;
                if let Some(e) = err {
                    return Err(e);
                }
            }
            for r in g
                .cands(None, Some(rest), Some(cell), g.offset(), g.len())
                .iter()
            {
                pending += 1;
                if pending >= VISIT_BATCH {
                    visit(std::mem::take(&mut pending))?;
                    if cancelled(limits) {
                        anyhow::bail!("reasoning cancelled");
                    }
                }
                if g.alive(r) && g.flagged(r) {
                    let t = g.triple(r);
                    if t[1] == rest
                        && t[2] == cell
                        && insert_set_charged(&mut affected, t[0], &mut work_held)?
                    {
                        push_charged(&mut queue, t[0], &mut work_held)?;
                    }
                }
            }
            if let Some(prevs) = removed_rev_rest.get(&cell) {
                for &p in prevs {
                    pending += 1;
                    if pending >= VISIT_BATCH {
                        visit(std::mem::take(&mut pending))?;
                        if cancelled(limits) {
                            anyhow::bail!("reasoning cancelled");
                        }
                    }
                    if insert_set_charged(&mut affected, p, &mut work_held)? {
                        push_charged(&mut queue, p, &mut work_held)?;
                    }
                }
            }
        }
        if pending > 0 {
            visit(std::mem::take(&mut pending))?;
        }
        for &h in &affected {
            if cancelled(limits) {
                anyhow::bail!("reasoning cancelled");
            }
            let new_m = new_explicit_list(
                g,
                &mut memo,
                h,
                first,
                rest,
                nil,
                &mut cell_cache,
                &mut work_held,
                limits,
                visit,
            )?;
            let old_m = old_explicit_list(
                g,
                &mut memo,
                h,
                first,
                rest,
                nil,
                &inserted_list,
                &removed_first,
                &removed_rest,
                multi_graph,
                &mut cell_cache,
                &mut work_held,
                limits,
                visit,
            )?;
            if old_m != new_m {
                push_charged(&mut heads, h, &mut kept_held)?;
            }
            let m_bytes = old_m
                .as_ref()
                .map_or(0, |v| v.len() * size_of::<u64>() + 16);
            kept_held.try_grow(m_bytes)?;
            reserve_map_charged(&mut old_members, &h, &mut kept_held)?;
            old_members.insert(h, old_m.map(std::rc::Rc::<[u64]>::from));
            visit(0)?;
        }
    }
    if let Some(base) = g.base() {
        base.add_scans(memo.scans);
    }
    drop(work_held);
    heads.sort_unstable();
    heads.dedup();
    Ok(ChangedLists {
        heads,
        old_members,
        _held: kept_held,
    })
}

/// Evaluate list-reading rules over `changed.old_members` to find live derived heads that
/// depended on a changed explicit RDF list and must enter the first deletion frontier.
fn eval_list_rules_old(
    c: &Closure,
    rules: &[CRule],
    changed: &ChangedLists,
    limits: &Limits,
    bound: &Bound,
) -> anyhow::Result<(Vec<Triple>, Held)> {
    let g = &c.graph;
    let abort = AtomicBool::new(false);
    let mut out_held = Held::new(&limits.budget);
    let mut seen_held = Held::new(&limits.budget);
    let mut out: Vec<Triple> = Vec::new();
    let mut seen: FxHashSet<Triple> = FxHashSet::default();
    let visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
    let visit: &engine::Visit<'_> = &visit;
    for r in rules {
        if !engine::reads_lists(r) {
            continue;
        }
        let ranges = vec![(0, g.len()); r.atoms.len()];
        for slot in list_head_slots(r) {
            match slot {
                Slot::Var(lv) => {
                    let Some(steps) = engine::plan_with(r, &ranges, Some(g), None, &[lv]) else {
                        continue;
                    };
                    let mut ev = Eval::new(
                        g,
                        &c.terms,
                        r,
                        &steps,
                        g.len(),
                        &abort,
                        limits.cancel.as_deref(),
                        usize::MAX,
                        Mode::Existing { visit: Some(visit) },
                        &limits.budget,
                    );
                    ev.set_explicit_lists(true);
                    ev.set_list_override(Some(&changed.old_members));
                    let mut b = vec![0u64; r.nvars];
                    for &h in &changed.heads {
                        if cancelled(limits) {
                            anyhow::bail!("reasoning cancelled");
                        }
                        b[lv] = h;
                        ev.run(0, &mut b, None);
                        if let Some(e) = ev.err.take() {
                            return Err(e);
                        }
                        if ev.visits > 0 {
                            bound.add(std::mem::take(&mut ev.visits))?;
                        }
                    }
                    let (found, _h) = ev.take_out();
                    for t in found {
                        if g.position(&t).is_some_and(|row| !c.flagged(row))
                            && insert_set_charged(&mut seen, t, &mut seen_held)?
                        {
                            push_charged(&mut out, t, &mut out_held)?;
                        }
                    }
                    bound.add(0)?;
                }
                Slot::Const(h) if changed.heads.binary_search(&h).is_ok() => {
                    let Some(steps) = engine::plan_with(r, &ranges, Some(g), None, &[]) else {
                        continue;
                    };
                    let mut ev = Eval::new(
                        g,
                        &c.terms,
                        r,
                        &steps,
                        g.len(),
                        &abort,
                        limits.cancel.as_deref(),
                        usize::MAX,
                        Mode::Existing { visit: Some(visit) },
                        &limits.budget,
                    );
                    ev.set_explicit_lists(true);
                    ev.set_list_override(Some(&changed.old_members));
                    let mut b = vec![0u64; r.nvars];
                    ev.run(0, &mut b, None);
                    if let Some(e) = ev.err.take() {
                        return Err(e);
                    }
                    if ev.visits > 0 {
                        bound.add(std::mem::take(&mut ev.visits))?;
                    }
                    let (found, _h) = ev.take_out();
                    for t in found {
                        if g.position(&t).is_some_and(|row| !c.flagged(row))
                            && insert_set_charged(&mut seen, t, &mut seen_held)?
                        {
                            push_charged(&mut out, t, &mut out_held)?;
                        }
                    }
                    bound.add(0)?;
                }
                _ => {}
            }
        }
    }
    drop(seen);
    drop(seen_held);
    Ok((out, out_held))
}

/// Evaluate list-reading rules for `changed.heads` using the new explicit RDF lists and
/// merge any newly derived facts into `c.graph`, deduplicating across rules before
/// counting distinct new rows against `limits.max_inferred` from `inferred_origin`.
fn eval_list_rules_new(
    c: &mut Closure,
    rules: &[CRule],
    changed: &ChangedLists,
    limits: &Limits,
    inferred_origin: u32,
) -> anyhow::Result<()> {
    let abort = AtomicBool::new(false);
    let mut fresh_held = Held::new(&limits.budget);
    let mut fresh: Vec<Triple> = Vec::new();
    {
        let g = &c.graph;
        let already = g.inferred_since(inferred_origin);
        let mut seen_held = Held::new(&limits.budget);
        let mut seen: FxHashSet<Triple> = FxHashSet::default();
        let mut memo = Memo::default();
        for r in rules {
            if !engine::reads_lists(r) {
                continue;
            }
            let ranges = vec![(0, g.len()); r.atoms.len()];
            for slot in list_head_slots(r) {
                let rule_name = &r.name;
                let mut admit = |t: Triple| -> anyhow::Result<()> {
                    if g.position(&t).is_some() {
                        return Ok(());
                    }
                    if let Some(base) = g.base()
                        && base.contains_with(&mut memo, &t)?
                    {
                        return Ok(());
                    }
                    if !insert_set_charged(&mut seen, t, &mut seen_held)? {
                        return Ok(());
                    }
                    if already.saturating_add(fresh.len()) >= limits.max_inferred {
                        return Err(crate::InferredLimitExceeded {
                            limit: limits.max_inferred,
                            context: format!(" (in rule '{rule_name}')"),
                        }
                        .into());
                    }
                    push_charged(&mut fresh, t, &mut fresh_held)?;
                    Ok(())
                };
                match slot {
                    Slot::Var(lv) => {
                        let Some(steps) = engine::plan_with(r, &ranges, Some(g), None, &[lv])
                        else {
                            continue;
                        };
                        let mut ev = Eval::new(
                            g,
                            &c.terms,
                            r,
                            &steps,
                            g.len(),
                            &abort,
                            limits.cancel.as_deref(),
                            usize::MAX,
                            Mode::NewSink { sink: &mut admit },
                            &limits.budget,
                        );
                        ev.set_explicit_lists(true);
                        ev.set_deadline(limits.deadline);
                        let mut b = vec![0u64; r.nvars];
                        for &h in &changed.heads {
                            if limits.stopped() {
                                return Err(limits.stop_error());
                            }
                            b[lv] = h;
                            ev.run(0, &mut b, None);
                            if let Some(e) = ev.err.take() {
                                return Err(e);
                            }
                            if limits.stopped() {
                                return Err(limits.stop_error());
                            }
                        }
                    }
                    Slot::Const(h) if changed.heads.binary_search(&h).is_ok() => {
                        let Some(steps) = engine::plan_with(r, &ranges, Some(g), None, &[]) else {
                            continue;
                        };
                        let mut ev = Eval::new(
                            g,
                            &c.terms,
                            r,
                            &steps,
                            g.len(),
                            &abort,
                            limits.cancel.as_deref(),
                            usize::MAX,
                            Mode::NewSink { sink: &mut admit },
                            &limits.budget,
                        );
                        ev.set_explicit_lists(true);
                        ev.set_deadline(limits.deadline);
                        let mut b = vec![0u64; r.nvars];
                        ev.run(0, &mut b, None);
                        if let Some(e) = ev.err.take() {
                            return Err(e);
                        }
                        if limits.stopped() {
                            return Err(limits.stop_error());
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(base) = g.base() {
            base.add_scans(memo.scans);
        }
    }
    if !fresh.is_empty() {
        c.graph.try_add_batch(fresh)?;
    }
    drop(fresh_held);
    Ok(())
}

/// Estimate rule-instance visits for a deletion from the removed triples matching each
/// non-operator rule's constant `(s, p, o)` slots and the closure's fan-out (`plans`),
/// to compare against item 798's visit bound (F16 §7.3, replacing `LARGE_DELETION`).
/// Only keys required by active compiled patterns are indexed, and all projection tables
/// are charged to `limits.budget` before growth.
fn estimate_deletion_visits(
    c: &Closure,
    rules: &[CRule],
    plans: &Plans,
    removed: &[Triple],
    limits: &Limits,
) -> anyhow::Result<u64> {
    let g = &c.graph;
    let hi = g.len();
    let total_facts = g.offset() as u64 + g.live() as u64;
    if total_facts == 0 || removed.is_empty() {
        return Ok(0);
    }
    let mut held = Held::new(&limits.budget);
    let mut by_p: FxHashMap<u64, u64> = FxHashMap::default();
    let mut by_ps: FxHashMap<(u64, u64), u64> = FxHashMap::default();
    let mut by_po: FxHashMap<(u64, u64), u64> = FxHashMap::default();
    let mut by_pso: FxHashMap<Triple, u64> = FxHashMap::default();
    let mut by_s: FxHashMap<u64, u64> = FxHashMap::default();
    let mut by_o: FxHashMap<u64, u64> = FxHashMap::default();
    let mut by_so: FxHashMap<(u64, u64), u64> = FxHashMap::default();
    let mut register_pattern = |s: Slot, p: Slot, o: Slot| -> Result<(), BudgetFailure> {
        match (s, p, o) {
            (Slot::Const(ks), Slot::Const(kp), Slot::Const(ko)) => {
                let k = [ks, kp, ko];
                reserve_map_charged(&mut by_pso, &k, &mut held)?;
                by_pso.entry(k).or_insert(0);
            }
            (Slot::Const(ks), Slot::Const(kp), _) => {
                let k = (kp, ks);
                reserve_map_charged(&mut by_ps, &k, &mut held)?;
                by_ps.entry(k).or_insert(0);
            }
            (_, Slot::Const(kp), Slot::Const(ko)) => {
                let k = (kp, ko);
                reserve_map_charged(&mut by_po, &k, &mut held)?;
                by_po.entry(k).or_insert(0);
            }
            (_, Slot::Const(kp), _) => {
                reserve_map_charged(&mut by_p, &kp, &mut held)?;
                by_p.entry(kp).or_insert(0);
            }
            (Slot::Const(ks), _, Slot::Const(ko)) => {
                let k = (ks, ko);
                reserve_map_charged(&mut by_so, &k, &mut held)?;
                by_so.entry(k).or_insert(0);
            }
            (Slot::Const(ks), _, _) => {
                reserve_map_charged(&mut by_s, &ks, &mut held)?;
                by_s.entry(ks).or_insert(0);
            }
            (_, _, Slot::Const(ko)) => {
                reserve_map_charged(&mut by_o, &ko, &mut held)?;
                by_o.entry(ko).or_insert(0);
            }
            _ => {}
        }
        Ok(())
    };
    for (ri, r) in rules.iter().enumerate() {
        if !plans.forward[ri].is_empty() {
            for (d, a) in r.atoms.iter().enumerate() {
                if plans.forward[ri][d].is_some() {
                    register_pattern(a.s, a.p, a.o)?;
                }
            }
        }
        for (hi_head, h) in r.head.iter().enumerate() {
            if plans.back[ri].get(hi_head).is_some_and(Option::is_some)
                && let CHead::Triple(s, p, o) = *h
            {
                register_pattern(s, p, o)?;
            }
        }
    }
    for (idx, &t) in removed.iter().enumerate() {
        if (idx & (VISIT_BATCH as usize - 1)) == 0 && cancelled(limits) {
            anyhow::bail!("reasoning cancelled");
        }
        let [s, p, o] = t;
        if !by_p.is_empty()
            && let Some(cnt) = by_p.get_mut(&p)
        {
            *cnt += 1;
        }
        if !by_ps.is_empty()
            && let Some(cnt) = by_ps.get_mut(&(p, s))
        {
            *cnt += 1;
        }
        if !by_po.is_empty()
            && let Some(cnt) = by_po.get_mut(&(p, o))
        {
            *cnt += 1;
        }
        if !by_pso.is_empty()
            && let Some(cnt) = by_pso.get_mut(&t)
        {
            *cnt += 1;
        }
        if !by_s.is_empty()
            && let Some(cnt) = by_s.get_mut(&s)
        {
            *cnt += 1;
        }
        if !by_o.is_empty()
            && let Some(cnt) = by_o.get_mut(&o)
        {
            *cnt += 1;
        }
        if !by_so.is_empty()
            && let Some(cnt) = by_so.get_mut(&(s, o))
        {
            *cnt += 1;
        }
    }
    let total_removed = removed.len() as u64;
    let count_matching = |s: Slot, p: Slot, o: Slot| -> u64 {
        match (s, p, o) {
            (Slot::Const(ks), Slot::Const(kp), Slot::Const(ko)) => {
                by_pso.get(&[ks, kp, ko]).copied().unwrap_or(0)
            }
            (Slot::Const(ks), Slot::Const(kp), _) => by_ps.get(&(kp, ks)).copied().unwrap_or(0),
            (_, Slot::Const(kp), Slot::Const(ko)) => by_po.get(&(kp, ko)).copied().unwrap_or(0),
            (_, Slot::Const(kp), _) => by_p.get(&kp).copied().unwrap_or(0),
            (Slot::Const(ks), _, Slot::Const(ko)) => by_so.get(&(ks, ko)).copied().unwrap_or(0),
            (Slot::Const(ks), _, _) => by_s.get(&ks).copied().unwrap_or(0),
            (_, _, Slot::Const(ko)) => by_o.get(&ko).copied().unwrap_or(0),
            _ => total_removed,
        }
    };
    let dist_p = g.distinct_predicates().max(1) as u64;
    let dist_s = g.distinct_subjects().max(1) as u64;
    let dist_o = g.distinct_objects().max(1) as u64;
    let avg_p = total_facts.div_ceil(dist_p).max(1);

    let max_nvars = rules.iter().map(|r| r.nvars).max().unwrap_or(0);
    if max_nvars > 16 {
        held.try_grow(max_nvars * size_of::<bool>())?;
    }
    let mut var_bound: smallvec::SmallVec<[bool; 16]> = smallvec::smallvec![false; max_nvars];
    if var_bound.spilled() && var_bound.capacity() > max_nvars {
        held.try_grow((var_bound.capacity() - max_nvars) * size_of::<bool>())?;
    }

    let plan_fanout = |var_bound: &mut [bool],
                       r: &CRule,
                       steps: &[Step],
                       skip_atom: Option<usize>,
                       pre: &[usize]|
     -> u64 {
        let bound = &mut var_bound[..r.nvars];
        bound.fill(false);
        let bind = |s: Slot, b: &mut [bool]| {
            if let Slot::Var(v) = s {
                b[v] = true;
            }
        };
        if let Some(d) = skip_atom {
            let a = &r.atoms[d];
            bind(a.s, bound);
            bind(a.p, bound);
            bind(a.o, bound);
        }
        for &v in pre {
            bound[v] = true;
        }
        let is_bound = |s: Slot, b: &[bool]| match s {
            Slot::Const(_) => true,
            Slot::Var(v) => b[v],
            Slot::Any => false,
        };
        let mut fanout = 1u64;
        for step in steps {
            let atom = match *step {
                Step::Atom { atom, .. } => atom,
                Step::Builtin(bi) => {
                    for &v in &r.builtins[bi].outputs {
                        bound[v] = true;
                    }
                    continue;
                }
            };
            if skip_atom == Some(atom) {
                continue;
            }
            let a = &r.atoms[atom];
            let sb = is_bound(a.s, bound);
            let ob = is_bound(a.o, bound);
            let est = match a.p {
                Slot::Const(p) => {
                    let n = g.count_p(p, 0, hi) as u64;
                    if n == 0 {
                        return 0;
                    }
                    let (ds, do_) = g.pstats(p);
                    match (sb, ob) {
                        (true, true) => 1,
                        (true, false) => n.div_ceil(ds.max(1) as u64).max(1),
                        (false, true) => n.div_ceil(do_.max(1) as u64).max(1),
                        (false, false) => n,
                    }
                }
                Slot::Var(vp) if bound[vp] => match (sb, ob) {
                    (true, true) => 1,
                    (true, false) => avg_p.div_ceil(dist_s).max(1),
                    (false, true) => avg_p.div_ceil(dist_o).max(1),
                    (false, false) => avg_p,
                },
                _ => match (sb, ob) {
                    (true, true) => 1,
                    (true, false) => total_facts.div_ceil(dist_s).max(1),
                    (false, true) => total_facts.div_ceil(dist_o).max(1),
                    (false, false) => total_facts,
                },
            };
            fanout = fanout.saturating_mul(est);
            bind(a.s, bound);
            bind(a.p, bound);
            bind(a.o, bound);
        }
        fanout
    };

    let mut total = 0u64;
    // 1. Forward consequence fan-out for non-operator rules matching removed triples.
    for (ri, r) in rules.iter().enumerate() {
        if plans.forward[ri].is_empty() {
            continue;
        }
        for (d, a) in r.atoms.iter().enumerate() {
            let Some(steps) = plans.forward[ri][d].as_deref() else {
                continue;
            };
            let count = count_matching(a.s, a.p, a.o);
            if count == 0 {
                continue;
            }
            let f = plan_fanout(&mut var_bound, r, steps, Some(d), &[]);
            total = total.saturating_add(count.saturating_mul(f));
        }
    }
    // 2. Backward proof fan-out for non-operator rule heads matching removed triples.
    let mut add_head = |ri: usize, hi_head: usize| {
        let Some((steps, pre)) = plans.back[ri][hi_head].as_ref() else {
            return;
        };
        let CHead::Triple(hs, hp, ho) = rules[ri].head[hi_head] else {
            return;
        };
        let count = count_matching(hs, hp, ho);
        if count == 0 {
            return;
        }
        let f = plan_fanout(&mut var_bound, &rules[ri], steps, None, pre);
        total = total.saturating_add(count.saturating_mul(f));
    };
    for list in plans.by_pred.values() {
        for &(ri, hi_head) in list {
            add_head(ri, hi_head);
        }
    }
    for &(ri, hi_head) in &plans.any_pred {
        add_head(ri, hi_head);
    }
    Ok(total)
}

/// Re-derive any fact deleted during Phase 1 after its proof search touched a surviving
/// unproved fact of an operator-governed predicate, once saturation has re-closed the
/// operator relations. Repeats until quiescent, respecting `limits.max_inferred` from
/// `inferred_origin` and cumulative `limits.max_iterations`.
#[allow(clippy::too_many_arguments)]
fn revive_overdeleted(
    c: &mut Closure,
    rules: &[CRule],
    plans: &Plans,
    limits: &Limits,
    inferred_origin: u32,
    candidates: &mut Vec<Triple>,
    iterations: &mut usize,
) -> anyhow::Result<()> {
    candidates.sort_unstable();
    candidates.dedup();
    let abort = AtomicBool::new(false);
    loop {
        candidates.retain(|t| {
            c.graph.position(t).is_none()
                && !c
                    .graph
                    .base()
                    .is_some_and(|b| b.contains(t).unwrap_or(false))
        });
        if candidates.is_empty() {
            break;
        }
        let mut revived_held = Held::new(&limits.budget);
        let mut revived: Vec<Triple> = Vec::new();
        {
            let g = &c.graph;
            let already = g.inferred_since(inferred_origin);
            for &t in candidates.iter() {
                if cancelled(limits) {
                    anyhow::bail!("reasoning cancelled");
                }
                let mut found = false;
                for (ri, hi) in plans.heads(t[1]) {
                    let Some((steps, pre)) = &plans.back[ri][hi] else {
                        continue;
                    };
                    let rule = &rules[ri];
                    let Some(mut b) = bind_head(rule, hi, pre, t) else {
                        continue;
                    };
                    let failed = {
                        let mut sink = |_: &[Triple], _: &[Triple], _: &[u64]| -> bool {
                            found = true;
                            false
                        };
                        let mut ev = Eval::new(
                            g,
                            &c.terms,
                            rule,
                            steps,
                            g.len(),
                            &abort,
                            limits.cancel.as_deref(),
                            usize::MAX,
                            Mode::Instances {
                                head: hi,
                                target: t,
                                sink: &mut sink,
                            },
                            &limits.budget,
                        );
                        ev.run(0, &mut b, None);
                        ev.err.take()
                    };
                    if let Some(e) = failed {
                        return Err(e);
                    }
                    if found {
                        break;
                    }
                }
                if found {
                    if already.saturating_add(revived.len()) >= limits.max_inferred {
                        return Err(crate::InferredLimitExceeded {
                            limit: limits.max_inferred,
                            context: " (reviving over-deleted facts)".into(),
                        }
                        .into());
                    }
                    push_charged(&mut revived, t, &mut revived_held)?;
                }
            }
        }
        if revived.is_empty() {
            break;
        }
        let start = c.graph.len();
        c.graph.try_add_batch(revived)?;
        drop(revived_held);
        let o = match engine::run_from(
            &mut c.graph,
            rules,
            &c.terms,
            limits,
            Some(start),
            None,
            engine::Origin {
                base_len: Some(inferred_origin),
                iterations: *iterations,
                lists: None,
            },
        ) {
            Ok(o) => o,
            Err(e) if e.is::<crate::closure::Restart>() => {
                return Err(Fallback("an RDF list fact was derived".into()).into());
            }
            Err(e) => return Err(e),
        };
        *iterations = o.iterations;
    }
    Ok(())
}

/// Bindings of rule `ri`'s head `hi` unified with `t` (only the variables in `pre`), or
/// `None` when a constant differs.
pub(crate) fn bind_head(
    rule: &CRule,
    hi: usize,
    pre: &[usize],
    t: Triple,
) -> Option<smallvec::SmallVec<[u64; 16]>> {
    let CHead::Triple(s, p, o) = &rule.head[hi] else {
        return None;
    };
    let mut b = smallvec::smallvec![0u64; rule.nvars];
    for (slot, v) in [(s, t[0]), (p, t[1]), (o, t[2])] {
        match *slot {
            Slot::Const(c) if c != v => return None,
            Slot::Var(x) if pre.contains(&x) => {
                if b[x] != 0 && b[x] != v {
                    return None;
                }
                b[x] = v;
            }
            _ => {}
        }
    }
    Some(b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Snapshot, Store, StoreOptions};
    use std::sync::Arc;

    pub(crate) const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    pub(crate) const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
    pub(crate) const OWL: &str = "http://www.w3.org/2002/07/owl#";
    pub(crate) const EX: &str = "http://ex.org/";

    thread_local! {
        /// The memory bound of the deletions this thread starts, in place of the closure's.
        pub(crate) static HELD_BOUND: std::cell::Cell<Option<usize>> =
            const { std::cell::Cell::new(None) };
    }

    /// A removal whose search holds more than the deletion's memory bound stops the
    /// operator's pass with the bound's [`Fallback`], which a run answers with a full
    /// run, and the pass releases what it reserved.
    #[test]
    fn the_memory_bound_stops_a_pass_of_the_operator() {
        let v = Vocab::new();
        let id = |s: &str| v.iri[s];
        let (sub, ty) = (id(&format!("{RDFS}subClassOf")), id(&format!("{RDF}type")));
        let class = |i: usize| id(&format!("{EX}C{i}"));
        let ind = |i: usize| id(&format!("{EX}i{i}"));
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        for i in 0..5 {
            base.insert([class(i), sub, class(i + 1)]);
        }
        for i in 0..8 {
            base.insert([ind(i), ty, class(0)]);
        }
        let text = crate::RDFS_RULES;
        for (bound, fails) in [(None, false), (Some(0), true)] {
            let mut c = full(&v, text, &base);
            let mut now = base.clone();
            let removed = vec![[ind(0), ty, class(0)]];
            now.remove(&removed[0]);
            let snap = set_default_graph(&v, &now);
            let moved = c.terms.rebase(snap.clone()).unwrap();
            c.remap(&moved).unwrap();
            if let Some(mut l) = c.set_inputs(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap() {
                l.update(&[], &removed, crate::facts::rank);
                c.put_lookups(l);
            }
            let rules = compiled(text, &c.terms);
            let lim = limits();
            let used = lim.budget.used();
            HELD_BOUND.with(|b| b.set(bound));
            let r = update(&mut c, &rules, &lim, &[], &removed, None);
            HELD_BOUND.with(|b| b.set(None));
            match r {
                Err(e) => {
                    assert!(fails, "{e:#}");
                    let f = e.downcast_ref::<Fallback>().expect("a fallback");
                    assert!(f.0.contains("held more than 0 bytes"), "{}", f.0);
                    // the pass's reservations are back, beside what the closure grew by
                    drop(c);
                    assert!(lim.budget.used() <= used, "{} > {used}", lim.budget.used());
                }
                Ok(up) => {
                    assert!(!fails, "the deletion stayed within no memory");
                    assert!(up.deleted.contains(&removed[0]), "{:?}", up.deleted);
                    assert_eq!(render(&c), render(&full(&v, text, &now)));
                }
            }
        }
    }

    /// Directed closure reachability (`BaseReach::Directed`) and shared-tail changed-list
    /// preparation both respect the deletion memory bound and release all charges on
    /// fallback.
    #[test]
    fn directed_closure_and_changed_lists_respect_memory_bound() {
        let v = Vocab::new();
        let id = |s: &str| v.iri[s];
        let sub = id(&format!("{RDFS}subClassOf"));
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        for i in 0..5 {
            base.insert([v.classes[i], sub, v.classes[i + 1]]);
        }
        let text = crate::RDFS_RULES;
        let mut c = full(&v, text, &base);
        let mut now = base.clone();
        let removed = vec![[v.classes[2], sub, v.classes[3]]];
        now.remove(&removed[0]);
        let snap = set_default_graph(&v, &now);
        let moved = c.terms.rebase(snap.clone()).unwrap();
        c.remap(&moved).unwrap();
        if let Some(mut l) = c.set_inputs(snap, &[Id::DEFAULT_GRAPH]).unwrap() {
            l.update(&[], &removed, crate::facts::rank);
            c.put_lookups(l);
        }
        let rules = compiled(text, &c.terms);
        let lim = limits();
        let used = lim.budget.used();
        HELD_BOUND.with(|b| b.set(Some(0)));
        let r = update(&mut c, &rules, &lim, &[], &removed, None);
        HELD_BOUND.with(|b| b.set(None));
        let e = r.expect_err("directed closure build must respect held bound");
        assert!(e.downcast_ref::<Fallback>().is_some(), "{e:#}");
        drop(c);
        assert!(lim.budget.used() <= used, "{} > {used}", lim.budget.used());

        // Shared-tail explicit list preparation under HELD_BOUND = 0
        let first = id(&format!("{RDF}first"));
        let rest = id(&format!("{RDF}rest"));
        let nil = id(&format!("{RDF}nil"));
        let items = v.props[0];
        let mut lbase: FxHashSet<Triple> = FxHashSet::default();
        lbase.insert([v.inds[0], items, v.list_nodes[0]]);
        for i in 0..3 {
            lbase.insert([v.list_nodes[i], first, v.classes[i]]);
            lbase.insert([
                v.list_nodes[i],
                rest,
                if i + 1 < 3 { v.list_nodes[i + 1] } else { nil },
            ]);
        }
        let ltext = "@prefix ex: <http://ex.org/>.\n\
                     [m: (?c ex:p0 ?l), listMember(?l, ?x) -> (?c ex:p1 ?x)]";
        let mut lc = full(&v, ltext, &lbase);
        let mut lnow = lbase.clone();
        let lrem = vec![[v.list_nodes[2], first, v.classes[2]]];
        let lins = vec![[v.list_nodes[2], first, v.classes[5]]];
        lnow.remove(&lrem[0]);
        lnow.insert(lins[0]);
        let lsnap = set_default_graph(&v, &lnow);
        let lmoved = lc.terms.rebase(lsnap.clone()).unwrap();
        lc.remap(&lmoved).unwrap();
        if let Some(mut l) = lc.set_inputs(lsnap, &[Id::DEFAULT_GRAPH]).unwrap() {
            l.update(&lins, &lrem, crate::facts::rank);
            lc.put_lookups(l);
        }
        let lrules = compiled(ltext, &lc.terms);
        let llim = limits();
        let lused = llim.budget.used();
        HELD_BOUND.with(|b| b.set(Some(0)));
        let lr = update(&mut lc, &lrules, &llim, &lins, &lrem, None);
        HELD_BOUND.with(|b| b.set(None));
        let le = lr.expect_err("changed_lists preparation must respect held bound");
        assert!(le.downcast_ref::<Fallback>().is_some(), "{le:#}");
        drop(lc);
        assert!(
            llim.budget.used() <= lused,
            "{} > {lused}",
            llim.budget.used()
        );
    }

    /// `estimate_deletion_visits` only indexes constant keys appearing in compiled rules
    /// and admits its projection tables against `limits.budget` before growth.
    #[test]
    fn estimate_deletion_visits_charges_and_ignores_unmatched_removed_objects() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let terms = Terms::new(snap);
        let text = "@prefix ex: <http://ex.org/>.\n\
                    [r: (?s ex:p ex:c) -> (?s ex:q ex:ok)]";
        let rules = compiled(text, &terms);
        let p = terms
            .id_for(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                "http://ex.org/p",
            )))
            .unwrap();
        let mut g = Graph::default();
        // Seed one fact so total_facts > 0.
        g.try_add_batch(vec![[10_000, p, 20_000]]).unwrap();
        let mut c = Closure::new(g, terms).unwrap();
        let plans = Plans::new(&mut c.graph, &rules).unwrap();

        // 5,000 removed `ex:p` triples with distinct non-matching objects.
        let removed: Vec<Triple> = (0..5_000u64)
            .map(|i| [100_000 + i, p, 200_000 + i])
            .collect();

        // A tight 8 KiB budget fits the single compiled `(ex:p, ex:c)` key in `by_po`
        // (whereas indexing all 5,000 removed `(p, o)` pairs would need >120 KiB).
        let tight = Limits {
            budget: Budget::new(Some(8 * 1024)),
            ..limits()
        };
        let est = estimate_deletion_visits(&c, &rules, &plans, &removed, &tight)
            .expect("unmatched removed objects must not grow projection tables");
        assert_eq!(est, 0);
        assert_eq!(tight.budget.used(), 0);
        assert!(tight.budget.peak() > 0 && tight.budget.peak() <= 8 * 1024);

        // A 1-byte budget must refuse before allocating the compiled pattern table.
        let tiny = Limits {
            budget: Budget::new(Some(1)),
            ..limits()
        };
        let err = estimate_deletion_visits(&c, &rules, &plans, &removed, &tiny)
            .expect_err("1-byte budget must reject estimator table allocation");
        assert!(
            crate::budget::BudgetFailure::from_anyhow(&err).is_some(),
            "{err:#}"
        );
        assert_eq!(tiny.budget.used(), 0);
    }

    /// With a nonzero memory allowance, operator candidate scans (`build_undirected`,
    /// `build_directed`, and guarded symmetric `pred_has_explicit_literal_obj`) count
    /// rejected unflagged derived rows against `Bound` and poll cancellation.
    #[test]
    fn operator_candidate_scans_respect_visit_bound_and_cancel() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let terms = Terms::new(snap);
        let iri = |u: &str| {
            terms
                .id_for(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(u)))
                .unwrap()
        };
        let eq_p = iri(&format!("{OWL}sameAs"));
        let sub_p = iri(&format!("{RDFS}subClassOf"));
        let rdf_type = iri(&format!("{RDF}type"));
        let owl_transitive = iri(&format!("{OWL}TransitiveProperty"));
        let owl_symmetric = iri(&format!("{OWL}SymmetricProperty"));
        let sym_tr_p = 50_000u64;
        let rules_text = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.\n\
                          @prefix owl: <http://www.w3.org/2002/07/owl#>.\n\
                          @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>.\n\
                          [eq-sym: (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)]\n\
                          [eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)]\n\
                          [sub: (?x rdfs:subClassOf ?y), (?y rdfs:subClassOf ?z) -> (?x rdfs:subClassOf ?z)]\n\
                          [trp: (?p rdf:type owl:TransitiveProperty), (?x ?p ?y), (?y ?p ?z) -> (?x ?p ?z)]\n\
                          [symp: (?p rdf:type owl:SymmetricProperty), (?x ?p ?y), notLiteral(?y) -> (?y ?p ?x)]";
        let rules = compiled(rules_text, &terms);

        // Build a derived tier with 2,500 unflagged derived rows per predicate and only 1
        // flagged explicit edge, so scanning only flagged matches would visit 1 row while
        // scanning all candidates visits > 2,500 rows.
        let mut g = Graph::default();
        let mut rows: Vec<Triple> = Vec::new();
        rows.push([sym_tr_p, rdf_type, owl_transitive]);
        rows.push([sym_tr_p, rdf_type, owl_symmetric]);
        let guard_end = rows.len() as u32;
        for &p in &[eq_p, sub_p, sym_tr_p] {
            rows.push([100_001, p, 100_002]);
            for i in 0..2_500u64 {
                rows.push([200_000 + i, p, 300_000 + i]);
            }
        }
        g.try_add_batch(rows).unwrap();
        for r in 0..guard_end {
            g.set_flag_with(r, true, false).unwrap();
        }
        // Flag only the single explicit edge for each predicate.
        for idx in 0..3u32 {
            g.set_flag_with(guard_end + idx * 2_501, true, false)
                .unwrap();
        }

        let budget = Budget::new(Some(64 << 20));
        let mut noop_visit = |_: u64| Ok(());
        // 0. `DeletionOps::detect` itself meters `pred_has_ambiguous_literals`,
        //    `pred_has_literal_subject`, and `guarded_has_literal_subject` against `Bound`
        //    and polls cancellation mid-scan.
        let irref_rules_text = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.\n\
                                @prefix owl: <http://www.w3.org/2002/07/owl#>.\n\
                                @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>.\n\
                                [sub-irref: (?x rdfs:subClassOf ?y), (?y rdfs:subClassOf ?z), notEqual(?x, ?z) -> (?x rdfs:subClassOf ?z)]";
        let irref_guarded_text = "@prefix owl: <http://www.w3.org/2002/07/owl#>.\n\
                                  @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>.\n\
                                  [trp-irref: (?p rdf:type owl:TransitiveProperty), (?x ?p ?y), (?y ?p ?z), notEqual(?x, ?z) -> (?x ?p ?z)]";
        let irref_rules = compiled(irref_rules_text, &terms);
        let irref_guarded_rules = compiled(irref_guarded_text, &terms);
        for (label, det_rules) in [
            ("detect equivalence ambiguity", &rules),
            ("detect fixed irreflexive literal subject", &irref_rules),
            (
                "detect guarded irreflexive literal subject",
                &irref_guarded_rules,
            ),
        ] {
            let bound = Bound::new(&budget, 1_500, 64 << 20);
            let mut visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
            let err = match crate::closure::DeletionOps::detect(
                det_rules, &g, &terms, &budget, None, &mut visit,
            ) {
                Ok(_) => panic!("{label}: detection scan must hit visit bound"),
                Err(e) => e,
            };
            assert!(
                err.downcast_ref::<Fallback>()
                    .is_some_and(|f| f.0.contains("visited more than 1500")),
                "{label}: unexpected error: {err:#}"
            );
            assert!(bound.visits() > 1_500, "{label}: {}", bound.visits());

            let cancel = AtomicBool::new(false);
            let seen_batch = AtomicBool::new(false);
            let mut visit_c = |n: u64| {
                if n > 0 {
                    seen_batch.store(true, Ordering::Relaxed);
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            };
            let cerr = match crate::closure::DeletionOps::detect(
                det_rules,
                &g,
                &terms,
                &budget,
                Some(&cancel),
                &mut visit_c,
            ) {
                Ok(_) => panic!("{label}: cancellation must stop detection scan"),
                Err(e) => e,
            };
            assert!(
                seen_batch.load(Ordering::Relaxed),
                "{label}: must reach detection scan loop"
            );
            assert!(
                cerr.to_string().contains("cancelled"),
                "{label}: unexpected cancel error: {cerr:#}"
            );
        }

        for (label, pred) in [
            ("undirected equivalence", eq_p),
            ("directed transitive", sub_p),
            ("guarded symmetric literal precheck", sym_tr_p),
        ] {
            // 1. Tight visit bound with generous nonzero memory allowance triggers Fallback.
            let mut ops = crate::closure::DeletionOps::detect(
                &rules,
                &g,
                &terms,
                &budget,
                None,
                &mut noop_visit,
            )
            .unwrap()
            .expect("closure ops");
            let bound = Bound::new(&budget, 1_500, 64 << 20);
            let mut visit = |n: u64| bound.add(n).map_err(anyhow::Error::from);
            let err = ops
                .ensure_pred(pred, &g, &terms, &budget, None, &mut visit)
                .expect_err(&format!(
                    "{label}: large unflagged scan must hit visit bound"
                ));
            assert!(
                err.downcast_ref::<Fallback>()
                    .is_some_and(|f| f.0.contains("visited more than 1500")),
                "{label}: unexpected error: {err:#}"
            );
            assert!(
                bound.visits() > 1_500,
                "{label}: expected >1500 visits, got {}",
                bound.visits()
            );

            // 2. Cancellation during candidate scan with nonzero memory allowance stops cleanly.
            let mut ops_c = crate::closure::DeletionOps::detect(
                &rules,
                &g,
                &terms,
                &budget,
                None,
                &mut noop_visit,
            )
            .unwrap()
            .expect("closure ops");
            let cancel = AtomicBool::new(false);
            let seen_batch = AtomicBool::new(false);
            let mut visit_c = |n: u64| {
                if n > 0 {
                    seen_batch.store(true, Ordering::Relaxed);
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            };
            let cerr = ops_c
                .ensure_pred(pred, &g, &terms, &budget, Some(&cancel), &mut visit_c)
                .expect_err(&format!("{label}: cancellation must stop candidate scan"));
            assert!(
                seen_batch.load(Ordering::Relaxed),
                "{label}: must reach scan loop"
            );
            assert!(
                cerr.to_string().contains("cancelled"),
                "{label}: unexpected cancel error: {cerr:#}"
            );
        }
    }

    /// Base-tier scans with few or no accepted facts and a large rejected range in an
    /// unselected graph (or across multi-graph passes) meter raw rows before filtering and
    /// stop on both `Bound` and mid-scan cancellation in detection, reachability building,
    /// and ordered list lookups.
    #[test]
    fn metered_base_scans_count_rejected_rows_in_detection_reachability_and_lists() {
        let store = Store::in_memory(StoreOptions::default());
        let unselected = "http://ex.org/unselected";
        let input_g1 = "http://ex.org/g1";
        let input_g2 = "http://ex.org/g2";
        let same_as = format!("{OWL}sameAs");
        let sub_class = format!("{RDFS}subClassOf");
        let rdf_first = format!("{RDF}first");
        let rdf_rest = format!("{RDF}rest");
        let rdf_nil = format!("{RDF}nil");
        let cell = "http://ex.org/cell";

        let mut nq = String::new();
        // 2,500 rejected rows in `unselected` for each scanned pattern, and 0 accepted rows
        // in `input_g1` for `owl:sameAs` / `rdfs:subClassOf`.
        for i in 0..2_500usize {
            let s = format!("http://ex.org/s{i}");
            let o = format!("http://ex.org/o{i}");
            nq.push_str(&format!("<{s}> <{same_as}> <{o}> <{unselected}> .\n"));
            nq.push_str(&format!("<{s}> <{sub_class}> <{o}> <{unselected}> .\n"));
            nq.push_str(&format!("<{cell}> <{rdf_first}> <{o}> <{unselected}> .\n"));
            nq.push_str(&format!("<{s}> <{rdf_rest}> <{cell}> <{unselected}> .\n"));
        }
        // Seed `input_g1` and `input_g2` so their graph ids exist in the snapshot.
        nq.push_str(&format!(
            "<http://ex.org/keep_s> <http://ex.org/other> <http://ex.org/keep_o> <{input_g1}> .\n"
        ));
        nq.push_str(&format!(
            "<http://ex.org/keep_s> <http://ex.org/other> <http://ex.org/keep_o> <{input_g2}> .\n"
        ));
        nq.push_str(&format!(
            "<{cell}> <{rdf_rest}> <{rdf_nil}> <{input_g1}> .\n"
        ));
        store
            .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();

        let snap = store.snapshot();
        let terms = Terms::new(snap.clone());
        let id = |iri: &str| snap.lookup_iri(iri).unwrap().0;
        let g1_id = Id(id(input_g1));
        let g2_id = Id(id(input_g2));
        let sub_p = id(&sub_class);
        let cell_id = id(cell);
        let first_p = id(&rdf_first);

        let rules = compiled(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.\n\
             @prefix owl: <http://www.w3.org/2002/07/owl#>.\n\
             [eq-sym: (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)]\n\
             [eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)]\n\
             [sub: (?x rdfs:subClassOf ?y), (?y rdfs:subClassOf ?z) -> (?x rdfs:subClassOf ?z)]",
            &terms,
        );
        let budget = Budget::new(Some(64 << 20));
        let g_single = Graph::with_base(Arc::new(Base::new(snap.clone(), &[g1_id]).unwrap()));

        // 1. `DeletionOps::detect` scans rejected `owl:sameAs` base rows and hits Bound/cancel.
        let b_det = Bound::new(&budget, 1_500, 64 << 20);
        let mut v_det = |n: u64| b_det.add(n).map_err(anyhow::Error::from);
        let err = match crate::closure::DeletionOps::detect(
            &rules, &g_single, &terms, &budget, None, &mut v_det,
        ) {
            Ok(_) => panic!("detect over rejected base range must hit visit bound"),
            Err(e) => e,
        };
        assert!(
            err.downcast_ref::<Fallback>()
                .is_some_and(|f| f.0.contains("visited more than 1500")),
            "{err:#}"
        );
        assert!(b_det.visits() > 1_500);

        let cancel_det = AtomicBool::new(false);
        let seen_det = AtomicBool::new(false);
        let mut vc_det = |n: u64| {
            if n > 0 {
                seen_det.store(true, Ordering::Relaxed);
                cancel_det.store(true, Ordering::Relaxed);
            }
            Ok(())
        };
        let cerr = match crate::closure::DeletionOps::detect(
            &rules,
            &g_single,
            &terms,
            &budget,
            Some(&cancel_det),
            &mut vc_det,
        ) {
            Ok(_) => panic!("detect over rejected base range must cancel mid-scan"),
            Err(e) => e,
        };
        assert!(seen_det.load(Ordering::Relaxed));
        assert!(cerr.to_string().contains("cancelled"), "{cerr:#}");

        // 2. `DeletionOps::ensure_pred` on `rdfs:subClassOf` scans rejected base rows with 0
        //    accepted facts and hits Bound + mid-scan cancel.
        let sub_rules = compiled(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.\n\
             [sub: (?x rdfs:subClassOf ?y), (?y rdfs:subClassOf ?z) -> (?x rdfs:subClassOf ?z)]",
            &terms,
        );
        let mut noop = |_: u64| Ok(());
        let mut ops = crate::closure::DeletionOps::detect(
            &sub_rules, &g_single, &terms, &budget, None, &mut noop,
        )
        .unwrap()
        .expect("sub op");
        let b_reach = Bound::new(&budget, 1_500, 64 << 20);
        let mut v_reach = |n: u64| b_reach.add(n).map_err(anyhow::Error::from);
        let err = ops
            .ensure_pred(sub_p, &g_single, &terms, &budget, None, &mut v_reach)
            .expect_err("ensure_pred over rejected base range must hit visit bound");
        assert!(
            err.downcast_ref::<Fallback>()
                .is_some_and(|f| f.0.contains("visited more than 1500")),
            "{err:#}"
        );
        assert!(b_reach.visits() > 1_500);

        let cancel_r = AtomicBool::new(false);
        let seen_r = AtomicBool::new(false);
        let mut vc_r = |n: u64| {
            if n > 0 {
                seen_r.store(true, Ordering::Relaxed);
                cancel_r.store(true, Ordering::Relaxed);
            }
            Ok(())
        };
        let cerr = ops
            .ensure_pred(
                sub_p,
                &g_single,
                &terms,
                &budget,
                Some(&cancel_r),
                &mut vc_r,
            )
            .expect_err("ensure_pred over rejected base range must cancel mid-scan");
        assert!(seen_r.load(Ordering::Relaxed));
        assert!(cerr.to_string().contains("cancelled"), "{cerr:#}");

        // 3. Multi-graph ordered list lookup (`changed_lists`) scans rejected base rows
        //    across passes and hits Bound + mid-scan cancel.
        let g_multi = Graph::with_base(Arc::new(Base::new(snap.clone(), &[g1_id, g2_id]).unwrap()));
        let c_multi = Closure::new(g_multi, Terms::new(snap)).unwrap();
        let lim = Limits {
            budget: Budget::new(Some(64 << 20)),
            ..limits()
        };
        let rem = vec![[cell_id, first_p, 999_999]];
        let b_list = Bound::new(&lim.budget, 1_500, 64 << 20);
        let Err(err) = changed_lists(&c_multi, &[], &rem, true, &lim, &b_list) else {
            panic!("multi-graph changed_lists over rejected base range must hit visit bound");
        };
        assert!(
            err.downcast_ref::<Fallback>()
                .is_some_and(|f| f.0.contains("visited more than 1500")),
            "{err:#}"
        );
        assert!(b_list.visits() > 1_500);

        let cancel_l = Arc::new(AtomicBool::new(false));
        let seen_l = AtomicBool::new(false);
        let lim_l = Limits {
            cancel: Some(cancel_l.clone()),
            budget: Budget::new(Some(64 << 20)),
            ..limits()
        };
        let mut vc_l = |n: u64| {
            if n > 0 {
                seen_l.store(true, Ordering::Relaxed);
                cancel_l.store(true, Ordering::Relaxed);
            }
            Ok(())
        };
        let Err(cerr) = changed_lists_metered(&c_multi, &[], &rem, true, &lim_l, &mut vc_l) else {
            panic!("multi-graph changed_lists over rejected base range must cancel mid-scan");
        };
        assert!(seen_l.load(Ordering::Relaxed));
        assert!(cerr.to_string().contains("cancelled"), "{cerr:#}");
    }

    /// Shared-tail `changed_lists` with nonzero memory allowance scales linearly with
    /// removed objects `M` independently of head count `N` even when all `M` `rdf:first`
    /// values are removed from the tail (leaving unflagged rows in `c.graph` with no
    /// replacement), and ticks visit-bound and mid-loop cancellation before `is_list` on
    /// large mixed updates.
    #[test]
    fn shared_tail_changed_lists_scales_linearly_and_respects_bound_and_cancel() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let terms = Terms::new(snap.clone());
        let (first, rest, nil) = (terms.rdf_first, terms.rdf_rest, terms.rdf_nil);

        // Seed the `M` removed `rdf:first` rows on `tail` as unflagged derived candidates,
        // matching what `TierProgram::prepare` puts in `c.graph` before calling
        // `changed_lists`, with NO surviving `rdf:first` on `tail` (removal-only).
        let build_closure = |n_heads: u64, m_removed: u64| -> (Closure, u64, Vec<Triple>) {
            let tail = 900_000u64;
            let mut g = Graph::default();
            let mut rows: Vec<Triple> = Vec::new();
            rows.push([tail, rest, nil]);
            for i in 0..n_heads {
                let h = 100_000 + i;
                rows.push([h, first, 200_000 + i]);
                rows.push([h, rest, tail]);
            }
            let explicit_n = rows.len() as u32;
            let rem: Vec<Triple> = (0..m_removed).map(|j| [tail, first, 500_000 + j]).collect();
            rows.extend_from_slice(&rem);
            g.try_add_batch(rows).unwrap();
            for r in 0..explicit_n {
                g.set_flag_with(r, true, false).unwrap();
            }
            (
                Closure::new(g, Terms::new(snap.clone())).unwrap(),
                tail,
                rem,
            )
        };

        let n_heads = 40u64;
        let (c_small, _tail, rem_small) = build_closure(n_heads, 2);
        let lim = Limits {
            budget: Budget::new(Some(64 << 20)),
            ..limits()
        };
        let b_small = Bound::new(&lim.budget, u64::MAX, 64 << 20);
        let ch_small = changed_lists(&c_small, &[], &rem_small, false, &lim, &b_small).unwrap();
        assert_eq!(ch_small.heads.len(), (n_heads + 1) as usize);

        // Increase M from 2 to 1,500 while keeping N = 40 fixed and removing ALL `rdf:first`
        // values on `tail` (unflagged rows present in `c.graph`): visits must grow by
        // O(M) (not Θ(N * M) = 60,000).
        let (c_large, tail, rem_large) = build_closure(n_heads, 1_500);
        let b_large = Bound::new(&lim.budget, u64::MAX, 64 << 20);
        let ch_large = changed_lists(&c_large, &[], &rem_large, false, &lim, &b_large).unwrap();
        assert_eq!(ch_large.heads.len(), (n_heads + 1) as usize);
        let delta_visits = b_large.visits() - b_small.visits();
        assert!(
            (1_400..=5_000).contains(&delta_visits),
            "expected O(M) delta visits for removal-only shared tail, got {delta_visits} (small={}, large={})",
            b_small.visits(),
            b_large.visits()
        );

        // Also exercise the live `TierProgram::prepare` / `update` path for removal-only
        // shared-tail edits: verify exact equivalence with full materialization and O(M)
        // visits instead of Θ(N * M).
        {
            let live_store = Store::in_memory(StoreOptions::default());
            let items = "http://ex.org/items";
            let out_p = "http://ex.org/hasMember";
            let rdf_first = format!("{RDF}first");
            let rdf_rest = format!("{RDF}rest");
            let rdf_nil = format!("{RDF}nil");
            let tail_iri = "http://ex.org/tail";
            let mut nq = format!("<{tail_iri}> <{rdf_rest}> <{rdf_nil}> .\n");
            for i in 0..20usize {
                nq.push_str(&format!(
                    "<http://ex.org/owner{i}> <{items}> <http://ex.org/head{i}> .\n"
                ));
                nq.push_str(&format!(
                    "<http://ex.org/head{i}> <{rdf_first}> <http://ex.org/val{i}> .\n"
                ));
                nq.push_str(&format!(
                    "<http://ex.org/head{i}> <{rdf_rest}> <{tail_iri}> .\n"
                ));
            }
            for j in 0..400usize {
                nq.push_str(&format!(
                    "<{tail_iri}> <{rdf_first}> <http://ex.org/mv{j}> .\n"
                ));
            }
            live_store
                .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
                .unwrap();
            let snap0 = live_store.snapshot();
            let t0 = Terms::new(snap0.clone());
            let ltext = "@prefix ex: <http://ex.org/>.\n\
                         [m: (?c ex:items ?l), listMember(?l, ?x) -> (?c ex:hasMember ?x)]";
            let lrules0 = compiled(ltext, &t0);
            let g0 = Graph::with_base(Arc::new(
                Base::new(snap0.clone(), &[Id::DEFAULT_GRAPH]).unwrap(),
            ));
            let mut c0 = Closure::new(g0, t0).unwrap();
            engine::run(&mut c0.graph, &lrules0, &c0.terms, &limits(), None).unwrap();
            let tail_id = snap0.lookup_iri(tail_iri).unwrap().0;
            let first_id = snap0.lookup_iri(&rdf_first).unwrap().0;
            let rem_ids: Vec<Triple> = (0..400usize)
                .map(|j| {
                    [
                        tail_id,
                        first_id,
                        snap0.lookup_iri(&format!("http://ex.org/mv{j}")).unwrap().0,
                    ]
                })
                .collect();
            let mut txn = live_store.write();
            for &[s, p, o] in &rem_ids {
                txn.delete([Id(s), Id(p), Id(o), Id::DEFAULT_GRAPH])
                    .unwrap();
            }
            txn.commit().unwrap();
            let snap1 = live_store.snapshot();
            let moved = c0.terms.rebase(snap1.clone()).unwrap();
            c0.remap(&moved).unwrap();
            c0.set_inputs(snap1.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
            let lrules1 = compiled(ltext, &c0.terms);
            let up = update(&mut c0, &lrules1, &lim, &[], &rem_ids, None).unwrap();
            // Without per-cell caching, 20 heads * 400 removed unflagged rows = 8,000+ extra
            // visits; with caching, total visits stay well below 4,000.
            assert!(
                up.visited < 4_000,
                "live removal-only shared tail rescanned unflagged rows: visits={}",
                up.visited
            );
            let out_p_id = c0
                .terms
                .id_for(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                    out_p,
                )))
                .unwrap();
            assert_eq!(
                c0.graph
                    .cands(
                        None,
                        Some(out_p_id),
                        None,
                        c0.graph.offset(),
                        c0.graph.len()
                    )
                    .iter()
                    .filter(|&r| c0.graph.alive(r))
                    .count(),
                0
            );
        }

        // Large mixed update with 2,500 ordinary non-list triples before 1 list triple
        // ticks visit-bound and mid-loop cancellation BEFORE `is_list`.
        let (c_bound, _, _) = build_closure(2, 0);
        let non_list_p = 777_777u64;
        let mut mixed_ins: Vec<Triple> = (0..2_500u64)
            .map(|j| [10_000 + j, non_list_p, 20_000 + j])
            .collect();
        mixed_ins.push([tail, first, 800_000]);
        let b_tight = Bound::new(&lim.budget, 1_200, 64 << 20);
        let Err(err) = changed_lists(&c_bound, &mixed_ins, &[], false, &lim, &b_tight) else {
            panic!("large non-list prefix in inserted must hit visit bound");
        };
        assert!(
            err.downcast_ref::<Fallback>()
                .is_some_and(|f| f.0.contains("visited more than 1200")),
            "unexpected error: {err:#}"
        );
        assert!(b_tight.visits() > 1_200 && b_tight.visits() < 2_500);

        // Mid-loop cancellation: flag starts `false` and flips `true` on the first batch
        // during the non-list prefix in `removed`, proving cancellation is polled mid-loop.
        let cancel = Arc::new(AtomicBool::new(false));
        let seen_batch = AtomicBool::new(false);
        let lim_cancel = Limits {
            cancel: Some(cancel.clone()),
            budget: Budget::new(Some(64 << 20)),
            ..limits()
        };
        let mut visit_c = |n: u64| {
            if n > 0 {
                seen_batch.store(true, Ordering::Relaxed);
                cancel.store(true, Ordering::Relaxed);
            }
            Ok(())
        };
        let Err(cerr) =
            changed_lists_metered(&c_bound, &[], &mixed_ins, false, &lim_cancel, &mut visit_c)
        else {
            panic!("mid-loop cancellation must stop changed_lists");
        };
        assert!(
            seen_batch.load(Ordering::Relaxed),
            "cancellation must trigger mid-loop, not only at entry"
        );
        assert!(cerr.to_string().contains("cancelled"), "{cerr:#}");
    }

    /// `TierProgram::prepare` holds `plans_held` during `estimate_deletion_visits` so the
    /// combined live memory of `Plans` and the estimator's projection maps + `>16`-variable
    /// `var_bound` buffer cannot bypass a tight budget cap.
    #[test]
    fn deletion_memory_admission_charges_plans_during_estimate_and_var_bound_spill() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let terms = Terms::new(snap);
        // Rule with 20 variables (> 16) over only 2 predicates (`ex:p0`, `ex:out`) so
        // `by_p` stays in its initial 4-bucket table with no resize peak, isolating the
        // `var_bound` heap-spill charge directly.
        let text = "@prefix ex: <http://ex.org/>.\n\
                    [wide: (?v0 ex:p0 ?v1), (?v1 ex:p0 ?v2), (?v2 ex:p0 ?v3), (?v3 ex:p0 ?v4),\
                           (?v4 ex:p0 ?v5), (?v5 ex:p0 ?v6), (?v6 ex:p0 ?v7), (?v7 ex:p0 ?v8),\
                           (?v8 ex:p0 ?v9), (?v9 ex:p0 ?v10), (?v10 ex:p0 ?v11), (?v11 ex:p0 ?v12),\
                           (?v12 ex:p0 ?v13), (?v13 ex:p0 ?v14), (?v14 ex:p0 ?v15),\
                           (?v15 ex:p0 ?v16), (?v16 ex:p0 ?v17), (?v17 ex:p0 ?v18),\
                           (?v18 ex:p0 ?v19) -> (?v0 ex:out ?v19)]";
        let rules = compiled(text, &terms);
        assert!(rules[0].nvars > 16);
        let p0 = terms
            .id_for(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                "http://ex.org/p0",
            )))
            .unwrap();
        let mut g = Graph::default();
        g.try_add_batch(vec![[10_000, p0, 20_000]]).unwrap();
        let mut c = Closure::new(g, terms).unwrap();
        let plans = Plans::new(&mut c.graph, &rules).unwrap();
        let removed = vec![[10_000, p0, 20_000]];

        // Measure exact un-stepped estimator peak alone (`limit < 16 KiB` keeps
        // `Budget::step == 0`, giving 1-byte granularity). Includes `by_p` (2 keys,
        // 4 buckets) + `var_bound` spill of `nvars` bytes.
        let probe_lim = Limits {
            budget: Budget::new(Some(16_000)),
            ..limits()
        };
        estimate_deletion_visits(&c, &rules, &plans, &removed, &probe_lim).unwrap();
        let est_peak = probe_lim.budget.peak();
        let by_p_bytes = table_bytes(table_capacity(2), size_of::<(u64, u64)>());
        assert_eq!(
            est_peak,
            by_p_bytes + rules[0].nvars,
            "estimator must charge exact by_p + >16-var SmallVec spill"
        );
        assert_eq!(probe_lim.budget.used(), 0);

        // Estimator alone fails when budget is 1 byte short of `est_peak`.
        let short_est = Limits {
            budget: Budget::new(Some(est_peak - 1)),
            ..limits()
        };
        let err = estimate_deletion_visits(&c, &rules, &plans, &removed, &short_est)
            .expect_err("1 byte short of estimator peak (including var_bound spill) must fail");
        assert!(
            crate::budget::BudgetFailure::from_anyhow(&err).is_some(),
            "{err:#}"
        );
        assert_eq!(short_est.budget.used(), 0);

        // Combined admission in `TierProgram::prepare`: pick a cap larger than `plans.bytes`
        // and larger than `est_peak` individually (plus the single removed triple), yet below
        // `plans.bytes + est_peak`. Because `plans_held` stays live during the estimate,
        // `prepare` must reject with `BudgetFailure` and release all charges.
        assert!(plans.bytes > 0);
        let combined_cap = (plans.bytes + est_peak) - 1;
        assert!(combined_cap > plans.bytes && combined_cap > est_peak);
        let lim_overlap = Limits {
            budget: Budget::new(Some(combined_cap)),
            ..limits()
        };
        let mut prog = TierProgram::default();
        let mut cx = TierCx {
            rules: &rules,
            limits: &lim_overlap,
            inserted: &[],
            removed: &removed,
            moved: &[],
            multi_graph: false,
            before_delete: None,
            rebuild: None,
            out: Update::default(),
            started: std::time::Instant::now(),
        };
        let mut tstore = TierStore::new(&mut c, &lim_overlap.budget);
        let err = prog
            .prepare(&mut cx, &mut tstore)
            .expect_err("prepare must hold plans_held during estimate_deletion_visits");
        assert!(
            crate::budget::BudgetFailure::from_anyhow(&err).is_some(),
            "{err:#}"
        );
        assert_eq!(lim_overlap.budget.used(), 0);
    }

    /// The no-revival branch in `TierProgram::saturate` drops the backing allocation of
    /// `overdeleted_touching_op` before releasing `overdeleted_held`.
    #[test]
    fn overdeleted_no_revival_drops_buffer_before_releasing_charge() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let terms = Terms::new(snap);
        let rules = compiled(crate::RDFS_RULES, &terms);
        let g = Graph::default();
        let mut c = Closure::new(g, terms).unwrap();
        let lim = Limits {
            budget: Budget::new(Some(64 << 20)),
            ..limits()
        };
        let mut prog = TierProgram::default();
        let mut held = Held::new(&lim.budget);
        for i in 0..256u64 {
            push_charged(&mut prog.overdeleted_touching_op, [i, 1, 2], &mut held).unwrap();
        }
        assert!(prog.overdeleted_touching_op.capacity() >= 256);
        assert!(held.bytes() >= 256 * size_of::<Triple>());
        prog.overdeleted_held = Some(held);
        let plans = Plans::new(&mut c.graph, &rules).unwrap();
        let plans_held = Held::try_new(&lim.budget, plans.bytes).unwrap();
        prog.del = Some(Deletion {
            plans,
            _plans_held: plans_held,
            ops: None,
            bound: None,
            search: Search::new(&lim.budget),
            abort: AtomicBool::new(false),
        });
        let mut cx = TierCx {
            rules: &rules,
            limits: &lim,
            inserted: &[],
            removed: &[],
            moved: &[],
            multi_graph: false,
            before_delete: None,
            rebuild: None,
            out: Update::default(),
            started: std::time::Instant::now(),
        };
        let mut tstore = TierStore::new(&mut c, &lim.budget);
        prog.saturate(&mut cx, &mut tstore).unwrap();
        assert_eq!(prog.overdeleted_touching_op.capacity(), 0);
        assert!(prog.overdeleted_held.is_none());
        assert_eq!(lim.budget.used(), 0);
    }

    #[test]
    fn the_bound_counts_visits() {
        let budget = Budget::unlimited();
        let b = Bound::new(&budget, 10, usize::MAX);
        b.add(4).unwrap();
        b.add(6).unwrap();
        assert_eq!(b.visits(), 10);
        let f = b.add(1).unwrap_err();
        assert!(
            f.0.contains("visited more than 10 rule instances"),
            "{}",
            f.0
        );
    }

    /// The consequences of a deleted fact stop at the bound also when their instances
    /// come from a builtin that binds many values, `listMember` here: 2,000 members
    /// twice give 4 million instances of one head.
    #[test]
    fn consequences_from_a_generator_stop_at_the_bound() {
        let mut ttl = String::from("<http://ex.org/a> <http://ex.org/p> (");
        for m in 0..2_000 {
            ttl.push_str(&format!(" <http://ex.org/m{m}>"));
        }
        ttl.push_str(" ) .\n");
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                ttl.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let terms = Terms::new(snap.clone());
        let text = "@prefix ex: <http://ex.org/>.
            [a: (?x ex:p ?l), listMember(?l, ?m), listMember(?l, ?n) -> (ex:s ex:r ex:o)]";
        let rules = compiled(text, &terms);
        let b = Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
        let mut g = Graph::with_base(Arc::new(b));
        // the fact the deletion removes, held in the derived tier as an update holds it
        let id = |l: &str| snap.lookup_iri(&format!("{EX}{l}")).unwrap().0;
        let l = snap
            .scan_keys(
                sparkles_core::index::Perm::Gspo,
                &[Id::DEFAULT_GRAPH.0, id("a"), id("p")],
            )
            .unwrap()[0][3];
        let dead = [id("a"), id("p"), l];
        g.try_add_batch(vec![dead]).unwrap();
        let mut c = Closure::new(g, terms).unwrap();
        let plans = Plans::new(&mut c.graph, &rules).unwrap();
        let lim = limits();
        let bound = Bound::new(&lim.budget, 2 * VISIT_BATCH, usize::MAX);
        let e = consequences(&c, &rules, &plans, &[dead], &lim, Some(&bound)).unwrap_err();
        assert!(e.is::<Fallback>(), "{e:#}");
        // a batch past the bound, and not the rest of the product
        assert!(
            bound.visits() <= 3 * VISIT_BATCH,
            "{} visits",
            bound.visits()
        );
    }

    #[test]
    fn the_bound_counts_memory_from_its_start() {
        let budget = Budget::new(Some(1 << 20));
        // held before the deletion starts, which does not count
        let _before = Held::try_new(&budget, 1_000).unwrap();
        let b = Bound::new(&budget, u64::MAX, 500);
        let mut h = Held::try_new(&budget, 400).unwrap();
        b.add(1).unwrap();
        h.try_grow(200).unwrap();
        let f = b.add(1).unwrap_err();
        assert!(f.0.contains("held more than 500 bytes"), "{}", f.0);
        assert!(b.peak_held() > 500, "{}", b.peak_held());
        h.shrink(600);
        b.add(1).unwrap();
    }

    /// xorshift64*
    pub(crate) struct Rng(pub(crate) u64);
    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        pub(crate) fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        pub(crate) fn pick<T: Copy>(&mut self, v: &[T]) -> T {
            v[self.below(v.len())]
        }
    }

    pub(crate) struct Vocab {
        /// the vocabulary in a named graph, and the explicit facts of a test in the
        /// default graph
        pub(crate) store: Store,
        pub(crate) snap: Arc<Snapshot>,
        pub(crate) classes: Vec<u64>,
        pub(crate) props: Vec<u64>,
        pub(crate) inds: Vec<u64>,
        pub(crate) lits: Vec<u64>,
        pub(crate) list_nodes: Vec<u64>,
        pub(crate) iri: FxHashMap<String, u64>,
    }

    impl Vocab {
        pub(crate) fn new() -> Vocab {
            let mut ttl = String::new();
            let mut names = Vec::new();
            for i in 0..6 {
                names.push(format!("{EX}C{i}"));
            }
            for i in 0..5 {
                names.push(format!("{EX}p{i}"));
            }
            for i in 0..8 {
                names.push(format!("{EX}i{i}"));
            }
            for i in 0..3 {
                names.push(format!("{EX}l{i}"));
            }
            for l in ["type", "first", "rest", "nil", "Property", "List"] {
                names.push(format!("{RDF}{l}"));
            }
            for l in [
                "subClassOf",
                "subPropertyOf",
                "domain",
                "range",
                "Class",
                "Resource",
                "Literal",
            ] {
                names.push(format!("{RDFS}{l}"));
            }
            for l in [
                "sameAs",
                "inverseOf",
                "TransitiveProperty",
                "SymmetricProperty",
                "FunctionalProperty",
                "InverseFunctionalProperty",
                "equivalentClass",
                "equivalentProperty",
                "intersectionOf",
                "unionOf",
                "hasValue",
                "onProperty",
                "someValuesFrom",
                "allValuesFrom",
                "Thing",
                "Class",
                "Restriction",
            ] {
                names.push(format!("{OWL}{l}"));
            }
            for n in &names {
                ttl.push_str(&format!("<urn:v> <urn:has> <{n}> <urn:vocab> .\n"));
            }
            for l in [
                "\"a\"",
                "\"b\"",
                "\"7\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            ] {
                ttl.push_str(&format!("<urn:v> <urn:has> {l} <urn:vocab> .\n"));
            }
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[Source::from_bytes(
                    ttl.into_bytes(),
                    RdfFormat::NQuads,
                    None,
                )])
                .unwrap();
            let snap = store.snapshot();
            let iri: FxHashMap<String, u64> = names
                .iter()
                .map(|n| (n.clone(), snap.lookup_iri(n).unwrap().0))
                .collect();
            let get = |p: &str| -> Vec<u64> {
                let mut v: Vec<(String, u64)> = iri
                    .iter()
                    .filter(|(k, _)| k.starts_with(&format!("{EX}{p}")))
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                v.sort();
                v.into_iter().map(|x| x.1).collect()
            };
            let terms = Terms::new(snap.clone());
            let lits = ["\"a\"", "\"b\""]
                .iter()
                .map(|l| {
                    terms
                        .id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                            &l[1..2],
                        )))
                        .unwrap()
                })
                .chain([sparkles_core::id::Id::from_i64(7).unwrap().0])
                .collect();
            Vocab {
                store,
                classes: get("C"),
                props: get("p"),
                inds: get("i"),
                list_nodes: get("l"),
                lits,
                snap,
                iri,
            }
        }

        pub(crate) fn id(&self, ns: &str, l: &str) -> u64 {
            self.iri[&format!("{ns}{l}")]
        }

        /// A random triple: schema statements, typing, property assertions.
        pub(crate) fn triple(&self, r: &mut Rng, owl: bool) -> Triple {
            let rdf = |l| self.id(RDF, l);
            let rdfs = |l| self.id(RDFS, l);
            let o = |l| self.id(OWL, l);
            let (c, p, i) = (&self.classes, &self.props, &self.inds);
            let k = r.below(if owl { 20 } else { 10 });
            match k {
                0 => [r.pick(c), rdfs("subClassOf"), r.pick(c)],
                1 => [r.pick(p), rdfs("subPropertyOf"), r.pick(p)],
                2 => [r.pick(p), rdfs("domain"), r.pick(c)],
                3 => [r.pick(p), rdfs("range"), r.pick(c)],
                4 | 5 => [r.pick(i), rdf("type"), r.pick(c)],
                6..=8 => [r.pick(i), r.pick(p), r.pick(i)],
                9 => [r.pick(i), r.pick(p), r.pick(&self.lits)],
                10 => [r.pick(i), o("sameAs"), r.pick(i)],
                11 => [r.pick(p), o("inverseOf"), r.pick(p)],
                12 => [
                    r.pick(p),
                    rdf("type"),
                    r.pick(&[
                        o("TransitiveProperty"),
                        o("SymmetricProperty"),
                        o("FunctionalProperty"),
                        o("InverseFunctionalProperty"),
                    ]),
                ],
                13 => [r.pick(c), o("equivalentClass"), r.pick(c)],
                14 => [r.pick(c), o("hasValue"), r.pick(i)],
                15 => [r.pick(c), o("onProperty"), r.pick(p)],
                16 => [r.pick(c), o("someValuesFrom"), r.pick(c)],
                17 => [r.pick(c), o("allValuesFrom"), r.pick(c)],
                18 => [r.pick(p), o("equivalentProperty"), r.pick(p)],
                _ => [r.pick(c), rdf("type"), o("Class")],
            }
        }

        /// The triples of a two-member list for `owl:intersectionOf` or `owl:unionOf`.
        pub(crate) fn list(&self, r: &mut Rng) -> Vec<Triple> {
            let (first, rest, nil) = (
                self.id(RDF, "first"),
                self.id(RDF, "rest"),
                self.id(RDF, "nil"),
            );
            let (l0, l1) = (r.pick(&self.list_nodes), r.pick(&self.list_nodes));
            if l0 == l1 {
                return Vec::new();
            }
            let op = if r.below(2) == 0 {
                "intersectionOf"
            } else {
                "unionOf"
            };
            vec![
                [r.pick(&self.classes), self.id(OWL, op), l0],
                [l0, first, r.pick(&self.classes)],
                [l0, rest, l1],
                [l1, first, r.pick(&self.classes)],
                [l1, rest, nil],
            ]
        }
    }

    pub(crate) fn limits() -> Limits {
        Limits {
            max_iterations: 10_000,
            max_inferred: 10_000_000,
            budget: Budget::unlimited(),
            closures: false,
            magic: None,
            sampled_planning: false,
            cancel: None,
            deadline: None,
            progress: None,
        }
    }

    pub(crate) fn compiled(text: &str, terms: &Terms) -> engine::CompiledRules {
        let rules = crate::parse_rules(text).unwrap();
        let mut w = crate::budget::WorkingWarnings::new(&terms.budget());
        let c = engine::compile(&rules, terms, &mut w).unwrap();
        assert!(
            c.iter()
                .all(|r| engine::incremental_blocker(r, terms).is_none())
        );
        c
    }

    /// Make the default graph hold exactly `base`, and return its snapshot.
    pub(crate) fn set_default_graph(v: &Vocab, base: &FxHashSet<Triple>) -> Arc<Snapshot> {
        let now = v.store.snapshot();
        let g = Id::DEFAULT_GRAPH;
        let mut txn = v.store.write();
        let have: Vec<[u64; 4]> = now
            .scan_keys(sparkles_core::index::Perm::Gspo, &[g.0])
            .unwrap()
            .iter()
            .map(|k| [k[1], k[2], k[3], k[0]])
            .collect();
        for q in &have {
            if !base.contains(&[q[0], q[1], q[2]]) {
                txn.delete([Id(q[0]), Id(q[1]), Id(q[2]), g]).unwrap();
            }
        }
        for t in base {
            assert!(
                t.iter()
                    .all(|&x| Id(x).tag() != sparkles_core::id::Tag::Local)
            );
            txn.insert([Id(t[0]), Id(t[1]), Id(t[2]), g]).unwrap();
        }
        txn.commit().unwrap();
        let snap = v.store.snapshot();
        assert!(Arc::ptr_eq(&snap.generation, &v.snap.generation));
        snap
    }

    pub(crate) fn full(v: &Vocab, text: &str, base: &FxHashSet<Triple>) -> Closure {
        let snap = set_default_graph(v, base);
        let terms = Terms::new(snap.clone());
        let rules = compiled(text, &terms);
        let b = Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap();
        let mut g = Graph::with_base(Arc::new(b));
        engine::run(&mut g, &rules, &terms, &limits(), None).unwrap();
        Closure::new(g, terms).unwrap()
    }

    /// The facts of both tiers as text, explicit ones marked. A fact both tiers hold
    /// appears twice when the derived tier does not flag it.
    fn render(c: &Closure) -> std::collections::BTreeSet<String> {
        let s = |x: u64| c.terms.term(x).map_or(format!("?{x:x}"), |t| t.to_string());
        let line = |t: Triple, b: bool| {
            format!(
                "{} {} {}{}",
                s(t[0]),
                s(t[1]),
                s(t[2]),
                if b { " ." } else { "" }
            )
        };
        let mut out: std::collections::BTreeSet<String> =
            c.live().map(|(t, b)| line(t, b)).collect();
        c.graph
            .base()
            .unwrap()
            .for_each(None, None, None, |t| {
                out.insert(line(t, true));
                true
            })
            .unwrap();
        out
    }

    /// Check every lookup the closure remembers against a scan of the input graphs of
    /// `snap`, and return the number of answers checked.
    fn check_lookups(c: &Closure, snap: &Arc<Snapshot>, what: &str) -> usize {
        let Some(l) = c.lookups() else {
            return 0;
        };
        let plain = Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
        let answers = l.answers();
        for (k, facts, complete) in &answers {
            let at = |x: u64| (x != 0).then_some(x);
            let mut all = Vec::new();
            plain
                .for_each(at(k[0]), at(k[1]), at(k[2]), |t| {
                    all.push(t);
                    true
                })
                .unwrap();
            let ok = if *complete {
                *facts == all
            } else {
                all.starts_with(facts)
            };
            assert!(
                ok,
                "{what}: lookup {k:?} kept {facts:?} ({complete}), scan gives {all:?}"
            );
        }
        answers.len()
    }

    /// Random changes, each followed by an incremental run checked against a full run.
    /// Some steps undo the previous one, and some reach their state through an earlier
    /// commit, so the remembered lookups take in several commits at once.
    /// The change an update reports against the closures around it: a derived fact
    /// that was not in the old closure is an addition, a fact of the old closure that no
    /// longer holds is a removal, an addition is a live derived fact, and a removed fact
    /// either no longer holds or was derived again.
    #[allow(clippy::too_many_arguments)]
    fn check_change(
        c: &Closure,
        up: &Update,
        before: &FxHashSet<Triple>,
        base: &FxHashSet<Triple>,
        inserted: &[Triple],
        removed: &[Triple],
        what: &str,
    ) {
        let after: FxHashSet<Triple> = c.live().filter(|x| !x.1).map(|x| x.0).collect();
        let holds = |t: &Triple| after.contains(t) || base.contains(t);
        // the explicit facts before the update
        let old_base =
            |t: &Triple| (base.contains(t) && !inserted.contains(t)) || removed.contains(t);
        let added: FxHashSet<Triple> = up.added(c).collect();
        let deleted: FxHashSet<Triple> = up.deleted.iter().copied().collect();
        assert_eq!(deleted.len(), up.deleted.len(), "{what}: a removal twice");
        for t in after.difference(before) {
            if !old_base(t) {
                assert!(
                    added.contains(t),
                    "{what}: {t:?} appeared without an addition"
                );
            }
        }
        for t in before.iter().chain(removed) {
            if !holds(t) {
                assert!(deleted.contains(t), "{what}: {t:?} went without a removal");
            }
        }
        for t in &added {
            assert!(after.contains(t), "{what}: {t:?} added but not derived");
        }
        for t in &deleted {
            // a run's changes are net, and a fact this generator removes and inserts again
            // is reported removed while the base tier holds it again, as before the operator
            if inserted.contains(t) && removed.contains(t) {
                continue;
            }
            assert!(
                !holds(t) || added.contains(t),
                "{what}: {t:?} removed but still held (derived {}, explicit {}, inserted {}, \
                 removed {}, before {})",
                after.contains(t),
                base.contains(t),
                inserted.contains(t),
                removed.contains(t),
                before.contains(t)
            );
        }
    }

    fn differential(text: &str, owl: bool, lists: bool, seed: u64, steps: usize) -> (usize, usize) {
        let v = Vocab::new();
        let mut r = Rng(seed);
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        for _ in 0..25 {
            base.insert(v.triple(&mut r, owl));
        }
        let mut c = full(&v, text, &base);
        let (mut incremental, mut fallbacks) = (0, 0);
        let mut checked = 0;
        let mut last: Option<(Vec<Triple>, Vec<Triple>)> = None;
        for step in 0..steps {
            let mut removed = Vec::new();
            let mut inserted = Vec::new();
            match last.take() {
                Some((ins, rem)) if r.below(4) == 0 => {
                    // undo the previous step
                    for t in ins {
                        if base.remove(&t) {
                            removed.push(t);
                        }
                    }
                    for t in rem {
                        if base.insert(t) {
                            inserted.push(t);
                        }
                    }
                }
                _ => {
                    let current: Vec<Triple> = {
                        let mut x: Vec<Triple> = base.iter().copied().collect();
                        x.sort_unstable();
                        x
                    };
                    for _ in 0..r.below(4) {
                        if !current.is_empty() {
                            let t = r.pick(&current);
                            if base.remove(&t) {
                                removed.push(t);
                            }
                        }
                    }
                    for _ in 0..r.below(5) {
                        let t = v.triple(&mut r, owl);
                        if base.insert(t) {
                            inserted.push(t);
                        }
                    }
                    if lists && r.below(6) == 0 {
                        for t in v.list(&mut r) {
                            if base.insert(t) {
                                inserted.push(t);
                            }
                        }
                    }
                }
            }
            last = Some((inserted.clone(), removed.clone()));
            if r.below(3) == 0 {
                // an earlier commit with triples that the next one takes back
                let mut between = base.clone();
                for _ in 0..3 {
                    between.insert(v.triple(&mut r, owl));
                }
                set_default_graph(&v, &between);
            }
            // the closure reads the input graphs of the new state, as an update does
            let snap = set_default_graph(&v, &base);
            let moved = c.terms.rebase(snap.clone()).unwrap();
            c.remap(&moved).unwrap();
            // the remembered lookups take the changes in, as `maintain::previous` does
            if let Some(mut l) = c.set_inputs(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap() {
                l.update(&inserted, &removed, crate::facts::rank);
                c.put_lookups(l);
            }
            let what = format!("seed {seed} step {step}");
            checked += check_lookups(&c, &snap, &what);
            let rules = compiled(text, &c.terms);
            let derived = |c: &Closure| -> FxHashSet<Triple> {
                c.live().filter(|x| !x.1).map(|x| x.0).collect()
            };
            let before = derived(&c);
            match update(&mut c, &rules, &limits(), &inserted, &removed, None) {
                Ok(up) => {
                    incremental += 1;
                    checked += check_lookups(&c, &snap, &what);
                    check_change(&c, &up, &before, &base, &inserted, &removed, &what);
                }
                Err(e) => {
                    assert!(e.downcast_ref::<Fallback>().is_some(), "{e:#}");
                    fallbacks += 1;
                    c = full(&v, text, &base);
                    continue;
                }
            }
            let want = full(&v, text, &base);
            let (a, b) = (render(&c), render(&want));
            if a != b {
                let extra: Vec<_> = a.difference(&b).collect();
                let missing: Vec<_> = b.difference(&a).collect();
                panic!(
                    "seed {seed} step {step}: removed {removed:?} inserted {inserted:?}\nextra {extra:#?}\nmissing {missing:#?}"
                );
            }
            c.compact_if_needed(false).unwrap();
        }
        assert!(checked > 0, "seed {seed}: no lookup was remembered");
        (incremental, fallbacks)
    }

    /// A scan that probes its candidates' heads in groups derives the same facts with
    /// the same ids as the evaluation one candidate at a time, for random
    /// OWL RL and RDFS programs, with and without a limit of inferred facts that stops a
    /// run partway through a group.
    #[test]
    fn grouped_head_probes_match_one_at_a_time() {
        use std::sync::atomic::Ordering;
        let run = |text: &str, base: &FxHashSet<Triple>, v: &Vocab, cap: usize, off: bool| {
            let snap = set_default_graph(v, base);
            let terms = Terms::new(snap.clone());
            let rules = compiled(text, &terms);
            let b = Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap();
            let mut g = Graph::with_base(Arc::new(b));
            let mut lim = limits();
            lim.max_inferred = cap;
            engine::AHEAD_OFF.store(off, Ordering::Relaxed);
            let res = engine::run(&mut g, &rules, &terms, &lim, None);
            engine::AHEAD_OFF.store(false, Ordering::Relaxed);
            let facts: Vec<(Triple, bool)> = (g.offset()..g.len())
                .map(|i| (g.triple(i), g.alive(i)))
                .collect();
            (
                res.map(|o| o.iterations).map_err(|e| format!("{e:#}")),
                facts,
            )
        };
        let mut groups = 0;
        for (text, owl) in [(crate::OWL_RL_RULES, true), (crate::RDFS_RULES, false)] {
            for seed in 1..40u64 {
                let v = Vocab::new();
                let mut r = Rng(seed * 104_729 + 7);
                let mut base: FxHashSet<Triple> = FxHashSet::default();
                for _ in 0..60 {
                    base.insert(v.triple(&mut r, owl));
                }
                for cap in [10_000_000, 1 + r.0 as usize % 200] {
                    let before = engine::AHEAD_GROUPS.load(Ordering::Relaxed);
                    let grouped = run(text, &base, &v, cap, false);
                    groups += engine::AHEAD_GROUPS.load(Ordering::Relaxed) - before;
                    let single = run(text, &base, &v, cap, true);
                    assert_eq!(grouped, single, "seed {seed}, limit {cap}, owl {owl}");
                }
            }
        }
        assert!(groups > 1_000, "{groups}");
    }

    #[test]
    fn rdfs_matches_full() {
        for seed in 1..25 {
            differential(crate::RDFS_RULES, false, false, seed, 25);
        }
    }

    #[test]
    fn rdfs_simple_matches_full() {
        for seed in 1..25 {
            differential(crate::RDFS_SIMPLE_RULES, false, false, seed, 25);
        }
    }

    #[test]
    fn owl_rl_matches_full() {
        let mut n = (0, 0);
        for seed in 1..25 {
            let (i, f) = differential(crate::OWL_RL_RULES, true, true, seed, 25);
            n = (n.0 + i, n.1 + f);
        }
        assert!(n.0 > n.1, "{n:?}");
    }

    /// A closure remembers lookups whether or not its base tier is shared: a tier it
    /// holds alone takes them in place, and a shared one is copied, so the other holder
    /// never sees them.
    #[test]
    fn closures_get_lookups_of_their_own() {
        let v = Vocab::new();
        let base: FxHashSet<Triple> = FxHashSet::default();
        let snap = set_default_graph(&v, &base);
        let terms = Terms::new(snap.clone());
        let tier = Arc::new(Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap());
        let alone = Closure::new(Graph::with_base(tier.clone()), terms).unwrap();
        // the graph and `tier` share it, so the closure copies it
        assert!(tier.lookups().is_none());
        assert!(alone.lookups().is_some());
        assert!(!Arc::ptr_eq(&alone.graph.base_arc().unwrap(), &tier));
        drop(alone);
        let g = Graph::with_base(tier);
        let before = Arc::as_ptr(&g.base_arc().unwrap());
        let c = Closure::new(g, Terms::new(snap)).unwrap();
        assert!(c.lookups().is_some());
        assert_eq!(Arc::as_ptr(&c.graph.base_arc().unwrap()), before);
    }

    /// The searches of a deletion round run in parallel when the run has threads, and one
    /// after the other when it has one: both match a full run.
    #[test]
    fn serial_and_parallel_searches_match_full() {
        for threads in [1, 6] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                for seed in 30..38 {
                    differential(crate::RDFS_RULES, false, false, seed, 15);
                    differential(crate::OWL_RL_RULES, true, true, seed, 15);
                }
            });
        }
    }

    #[test]
    fn jena_rules_match_full() {
        let rules = "@prefix ex: <http://ex.org/>.
            [chain: (?a ex:p0 ?b), (?b ex:p0 ?c), notEqual(?a, ?c) -> (?a ex:p1 ?c)]
            [lit: (?a ex:p2 ?v), isLiteral(?v), strConcat(?v, '!', ?w) -> (?a ex:p3 ?w)]
            [back: (?a ex:p3 ?w), regex(?w, '(.*)!', ?x) -> (?a ex:p4 ?x)]
            [num: (?a ex:p4 ?v), lessThan(?v, 10) -> (?a rdf:type ex:C0)]
            [sym: (?a ex:p1 ?b) -> (?b ex:p1 ?a)]
            [ty: (?a rdf:type ex:C0), (?a ?p ?b), notEqual(?p, rdf:type) -> (?b rdf:type ex:C1)]
            [any: (?a ?p ?b), (?p rdf:type owl:TransitiveProperty), (?b ?p ?c) -> (?a ?p ?c)]";
        for seed in 1..25 {
            differential(rules, true, false, seed, 25);
        }
    }
}
