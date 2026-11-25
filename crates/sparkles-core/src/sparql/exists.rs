//! Decorrelated `FILTER EXISTS` / `FILTER NOT EXISTS`: the pattern is evaluated once,
//! its solutions are reduced to a set of keys over the variables the outer solutions can
//! bind, and every outer row is answered by a probe of that set instead of evaluating
//! the substituted pattern for the row.
//!
//! # Semantics
//!
//! SPARQL evaluates `EXISTS { P }` for an outer solution μ by substitution: every
//! variable of `P` bound in μ is replaced by its value, and the substituted pattern is
//! tested for a solution (SPARQL 1.1 §18.6, `substitute`). The executor does exactly
//! that per row (with a memo per distinct key, see [`super::plan::eval_exists`]); this
//! module answers the same question from the unsubstituted solutions of `P`, for the
//! patterns where the two agree.
//!
//! **Admitted algebra.** `P` is built from basic graph patterns, property paths that
//! cannot match a zero-length path (no `*` or `?` anywhere in the path), joins,
//! `GRAPH` (constant or variable name) and `FILTER`s free of RAND / UUID / STRUUID /
//! BNODE (also inside nested EXISTS). Triple terms with variables (RDF 1.2), property
//! functions (`text:query`, `spk:vectorSearch`, `spatial:`) and everything else
//! (OPTIONAL, UNION, MINUS, BIND, VALUES, sub-selects, SERVICE) are rejected, and the
//! EXISTS is evaluated per row as before. An EXISTS that is a top-level conjunct of a
//! FILTER (`EXISTS` or `NOT EXISTS`, possibly beside other conjuncts), the whole
//! expression of a BIND or the whole condition of an OPTIONAL is answered by a probe of
//! each row, and the OPTIONAL's candidate rows are answered as a filter's rows are. An
//! EXISTS inside a larger expression, under OR, IF or another function, in an aggregate
//! argument or in an ORDER BY key, is answered through [`Answers`], which probes every
//! row of the table before the expression is evaluated, and the expression reads each
//! row's answer from it. A row that the key set cannot answer is evaluated by
//! substitution in every position. The active graph of the EXISTS
//! must be the default graph, the union graph or a constant graph; `GRAPH ?g { … FILTER
//! EXISTS … }` qualifies because the planner evaluates such a group per named graph,
//! each with its own constant active graph and its own key set.
//!
//! **Certain variables.** `cert(P)` is the set of variables bound in every solution
//! of `P`: the variables of the triple patterns of a BGP, the variable ends of a path,
//! the union of both sides of a join, a `GRAPH` name variable plus `cert` of its
//! pattern, and `cert` of a FILTER's pattern. Blank nodes of `P` are not variables
//! here: the executor never substitutes them, so they are ordinary existential
//! variables in both evaluations.
//!
//! **Risky variables.** For every `FILTER(e)` over a pattern `A` inside `P`, a
//! variable of `e` (including the variables of nested EXISTS patterns in `e`) that is
//! not in `cert(A)` is *risky*. Substitution replaces it in `e` with the outer value,
//! while the unsubstituted evaluation sees it unbound (`{ { ?x :p ?y FILTER(?v > 3) }
//! ?x :q ?v }`), so an outer row binding a risky variable is evaluated per row.
//!
//! **Claim.** Let `B` be the variables of `P` bound in μ (by a column of the outer
//! table, or by a constant substituted before the filter: initial bindings, filter
//! equalities, an enclosing EXISTS row). If `B` contains no risky variable, then
//! `substitute(P, μ)` has a solution iff some solution ν of `P` agrees with μ on `B`.
//! By induction over the admitted algebra, the solutions of the substituted pattern are
//! exactly the solutions of `P` that agree with μ on `B`, restricted to the other
//! variables:
//! * BGP: a constant in place of a variable matches the same triples as the variable
//!   restricted to that constant (term identity, which is id equality here).
//! * Path without zero-length matches: the path denotes a relation between terms of the
//!   active graph whichever end is fixed. (A zero-length path with a constant end
//!   matches that constant even when it is not in the graph, while two variable ends
//!   only enumerate graph terms: such paths are rejected.)
//! * Join: both sides are restricted the same way and agree with each other on `B`.
//! * GRAPH: the name is a variable like any other, the pattern is evaluated in the same
//!   graph by both. Both plan `P` with the same planner, active graph and dataset
//!   (FROM / FROM NAMED, the union default graph), so graph selection agrees.
//! * FILTER(e) over `A`: every variable of `B` that `e` mentions is in `cert(A)` (it is
//!   not risky), so the restricted solution of `A` binds it to μ's value and `e` sees
//!   the same values as `e` with the constants substituted. A nested EXISTS in `e` is
//!   substituted with the same values either way.
//!
//! Every non-risky variable of `P` that substitution can bind is in `cert(P)`. The keys
//! are those the filter's input can bind (a column, or a substituted constant); a row
//! that binds another variable of `P` (a filter pushed into UNION branches with other
//! columns shares the EXISTS) is evaluated per row. The key set holds the distinct
//! projections of the solutions of `P` on the keys; no key column of a solution is
//! unbound.
//!
//! **Partial bindings.** An outer row can leave key variables unbound (OPTIONAL,
//! UNION, a missing column). An unbound variable is not substituted, so it matches
//! anything: the row probes the set of projections on its bound keys only (one set per
//! mask of bound keys, built from the full key set when first needed). With no key
//! bound the answer is whether `P` has any solution at all. An UNDEF is never used as
//! an equality key.
//!
//! **Multiplicity.** The filter keeps or drops each outer row, so outer duplicates
//! stay; the key set is a set, so duplicate inner solutions are irrelevant (as they are
//! to EXISTS).
//!
//! # Cost and memory
//!
//! The pattern is planned when the filter first runs and built at most once per query
//! execution (the state lives in the EXISTS of the plan, so a filter re-run with a
//! larger input, as under LIMIT, reuses it). It is built only when its estimated cost is
//! below [`ROW_EVAL_COST`] per distinct key of the outer rows, and never inside the
//! per-row evaluation of another EXISTS (the pattern there is substituted, so a key set
//! would serve one row). The key sets count against the query's memory budget while
//! the rows are probed. A build that would exceed the budget, or fails for another
//! reason than cancellation or a timeout, leaves the EXISTS to per-row evaluation.

use super::ctx::{Charge, Ctx, RetainedCharge};
use super::exec::PAR_MIN_LEN;
use super::expr::{ExistsSpec, Expr, Row, ebv};
use super::plan::{ActiveGraph, Node, Planner, expr_vars};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::{
    AggregateExpression, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// Planned cost of the pattern that evaluating it for one distinct outer key is assumed
/// to be worth (planning and running a substituted pattern takes tens of microseconds;
/// hashing a solution of the whole pattern well under one).
const ROW_EVAL_COST: f64 = 512.0;

thread_local! {
    /// depth of per-row EXISTS evaluations on this thread
    static PER_ROW: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
type AfterCachePlanHook = Box<dyn FnMut(u64)>;

#[cfg(test)]
thread_local! {
    /// tests: decorrelate whenever the pattern is admitted, whatever it costs
    pub(crate) static FORCE: Cell<bool> = const { Cell::new(false) };
    /// tests: number of unsubstituted EXISTS plans built on this thread
    pub(super) static EXISTS_PLANS_BUILT: Cell<u64> = const { Cell::new(0) };
    /// tests: optional deterministic hook invoked right after caching an unsubstituted
    /// EXISTS plan, with the cumulative `EXISTS_PLANS_BUILT` count on this thread
    pub(super) static AFTER_CACHE_PLAN_HOOK: std::cell::RefCell<Option<AfterCachePlanHook>> =
        const { std::cell::RefCell::new(None) };
}

fn forced() -> bool {
    #[cfg(test)]
    return FORCE.with(Cell::get);
    #[cfg(not(test))]
    false
}

/// Run `f`, the evaluation of an EXISTS pattern substituted for one outer row: the
/// EXISTS filters inside it are not decorrelated.
pub(super) fn per_row<T>(f: impl FnOnce() -> T) -> T {
    struct Exit;
    impl Drop for Exit {
        fn drop(&mut self) {
            PER_ROW.with(|d| d.set(d.get() - 1));
        }
    }
    PER_ROW.with(|d| d.set(d.get() + 1));
    let _exit = Exit;
    f()
}

/// Cached unsubstituted plan and streaming eligibility verdicts for one [`ExistsSpec`].
///
/// Kept strictly separate from [`Decor`] so automatic admission never mutates `EXPLAIN`
/// notes or runtime decorrelation state.
#[derive(Default)]
pub(super) struct PlanCache {
    /// Cached unsubstituted plan of `(spec.pattern(), spec.graph)`.
    /// Populated at most once per `ExistsSpec`; dropped once `Decor` reaches
    /// `State::Built` or `State::Rejected`, or when automatic admission keeps the query eager.
    plan: Mutex<Option<CachedPlan>>,
    /// Cached structural admission (keys and risky variables) or rejection reason, with
    /// its retained memory charge. Cleared once `Decor` consumes or rejects it, or when
    /// automatic admission keeps the query eager.
    admit: Mutex<Option<CachedAdmit>>,
    /// Cached result of `cursor::streams_natively` on this spec's unsubstituted plan:
    /// 0 = unknown, 1 = false, 2 = true.
    native: AtomicU8,
    /// Cached result of `cursor::exists_streams` (strict `Operator::build`) on this spec:
    /// 0 = unknown, 1 = false, 2 = true.
    strict: AtomicU8,
}

struct CachedPlan {
    node: Arc<Node>,
    bytes: u64,
    charge: Option<RetainedCharge>,
}

struct CachedAdmit {
    result: std::result::Result<AdmitMeta, String>,
    bytes: u64,
    charge: Option<RetainedCharge>,
}

#[derive(Clone)]
struct AdmitMeta {
    keys: Vec<VarId>,
    risky: Vec<VarId>,
}

fn admit_bytes(result: &std::result::Result<AdmitMeta, String>) -> u64 {
    match result {
        Ok(meta) => (meta
            .keys
            .capacity()
            .saturating_mul(std::mem::size_of::<VarId>()))
        .saturating_add(
            meta.risky
                .capacity()
                .saturating_mul(std::mem::size_of::<VarId>()),
        ) as u64,
        Err(reason) => reason.capacity() as u64,
    }
}

impl PlanCache {
    pub(super) fn native(&self) -> Option<bool> {
        match self.native.load(Ordering::Acquire) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }

    pub(super) fn set_native(&self, value: bool) {
        self.native
            .store(if value { 2 } else { 1 }, Ordering::Release);
    }

    pub(super) fn strict(&self) -> Option<bool> {
        match self.strict.load(Ordering::Acquire) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }

    pub(super) fn set_strict(&self, value: bool) {
        self.strict
            .store(if value { 2 } else { 1 }, Ordering::Release);
    }

    pub(super) fn release_plan(&self) {
        *self.plan.lock() = None;
        *self.admit.lock() = None;
    }

    pub(super) fn uncharged_admit_bytes(&self) -> u64 {
        self.admit
            .lock()
            .as_ref()
            .filter(|cached| cached.charge.is_none())
            .map_or(0, |cached| cached.bytes)
    }

    #[cfg(test)]
    pub(super) fn has_cached_plan(&self) -> bool {
        self.plan.lock().is_some()
    }

    #[cfg(test)]
    pub(super) fn has_cached_admit(&self) -> bool {
        self.admit.lock().is_some()
    }
}

/// The decorrelation state of one EXISTS of a plan.
#[derive(Default)]
pub struct Decor {
    state: Mutex<State>,
    /// outer rows answered by a probe
    probed: AtomicU64,
    /// outer rows evaluated per row while the key set was in use
    per_row: AtomicU64,
    /// why the EXISTS was last left to per-row evaluation (EXPLAIN)
    reason: Mutex<Option<String>>,
    /// distinct outer keys of the batches a cursor filtered before the key set was built
    seen: Mutex<Seen>,
}

/// The distinct outer keys of a cursor's earlier batches. A cursor filters one batch at
/// a time, so the choice between the key set and per-row evaluation counts the keys of
/// every batch so far rather than those of one batch. Otherwise small batches would
/// never reach the number of keys that pays for the key set.
#[derive(Default)]
struct Seen {
    keys: FxHashSet<Vec<Id>>,
    charge: Option<RetainedCharge>,
}

#[derive(Default)]
enum State {
    #[default]
    Untried,
    /// admitted and planned; built once an outer table makes it worthwhile
    Planned(Box<Plan>),
    Built(Arc<Build>),
    Rejected,
}

struct Plan {
    /// variables of the pattern an outer row may bind and the key set holds (sorted)
    keys: Vec<VarId>,
    /// variables an outer row must leave unbound for a probe
    risky: Vec<VarId>,
    node: Arc<Node>,
}

struct Build {
    keys: Vec<VarId>,
    risky: Vec<VarId>,
    /// rows of the pattern
    solutions: usize,
    /// distinct projections on all keys
    full: KeySet,
    /// distinct projections on the keys of a mask (bit i: `keys[i]`), built on demand
    partial: Mutex<FxHashMap<u64, Arc<Partial>>>,
    _charge: Option<RetainedCharge>,
}

struct Partial {
    set: KeySet,
    _charge: Option<RetainedCharge>,
}

enum KeySet {
    One(FxHashSet<Id>),
    Many(FxHashSet<Box<[Id]>>),
}

impl KeySet {
    fn len(&self) -> usize {
        match self {
            KeySet::One(s) => s.len(),
            KeySet::Many(s) => s.len(),
        }
    }

    fn contains(&self, key: &[Id]) -> bool {
        match self {
            KeySet::One(s) => s.contains(&key[0]),
            KeySet::Many(s) => s.contains(key),
        }
    }

    /// Estimated bytes (ids plus hash table overhead).
    fn bytes(&self, width: usize) -> u64 {
        self.len() as u64 * (width as u64 * 8 + 16)
    }

    /// The distinct projections of `self` (over `width` keys) on the keys of `mask`.
    fn project(&self, width: usize, mask: u64) -> KeySet {
        let cols: Vec<usize> = (0..width).filter(|i| mask >> i & 1 == 1).collect();
        let mut keys: Vec<Box<[Id]>> = Vec::new();
        match self {
            KeySet::One(s) => keys.extend(s.iter().map(|id| Box::from([*id]))),
            KeySet::Many(s) => keys.extend(
                s.iter()
                    .map(|k| cols.iter().map(|&c| k[c]).collect::<Box<[Id]>>()),
            ),
        }
        if cols.len() == 1 {
            KeySet::One(keys.into_iter().map(|k| k[0]).collect())
        } else {
            KeySet::Many(keys.into_iter().collect())
        }
    }
}

impl Build {
    fn bytes(&self) -> u64 {
        let width = self.keys.len();
        self.full.bytes(width)
            + self
                .partial
                .lock()
                .iter()
                .map(|(m, s)| s.set.bytes(m.count_ones() as usize))
                .sum::<u64>()
    }
}

/// The EXISTS of a filter conjunct, and whether it is negated.
fn exists_of(e: &Expr) -> Option<(&ExistsSpec, bool)> {
    match e {
        Expr::Exists(s) => Some((s, false)),
        Expr::Not(a) => match &**a {
            Expr::Exists(s) => Some((s, true)),
            _ => None,
        },
        _ => None,
    }
}

/// Conservative pre-planning reservation for one EXISTS level, walking only the
/// immediate `GraphPattern` nodes without recursing into nested `EXISTS` patterns (which
/// reserve their own syntax/spec bytes inside `ExistsSpec::new_ordinary_charged_with_admission`,
/// and `VALUES` blocks reserve their exact table backing before allocating in `Planner`).
fn shallow_plan_estimate(spec: &ExistsSpec) -> u64 {
    use GraphPattern as GP;
    let mut stack = vec![spec.pattern()];
    let mut nodes = 0u64;
    while let Some(gp) = stack.pop() {
        nodes = nodes.saturating_add(1);
        match gp {
            GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => {}
            GP::Join { left, right }
            | GP::LeftJoin { left, right, .. }
            | GP::Lateral { left, right }
            | GP::Union { left, right }
            | GP::Minus { left, right }
            | GP::SemiJoin { left, right }
            | GP::AntiJoin { left, right } => {
                stack.push(left);
                stack.push(right);
            }
            GP::Filter { inner, .. }
            | GP::Graph { inner, .. }
            | GP::Extend { inner, .. }
            | GP::Assign { inner, .. }
            | GP::Unfold { inner, .. }
            | GP::OrderBy { inner, .. }
            | GP::Project { inner, .. }
            | GP::Distinct { inner }
            | GP::Reduced { inner }
            | GP::Slice { inner, .. }
            | GP::Group { inner, .. }
            | GP::Service { inner, .. } => {
                stack.push(inner);
            }
        }
    }
    nodes
        .saturating_mul(1024)
        .saturating_add((spec.vars.len() as u64).saturating_mul(128))
        .saturating_add(2048)
}

/// Plan `(spec.pattern(), spec.graph)` unsubstituted at most once per [`ExistsSpec`],
/// returning a shared `Arc<Node>` cached in [`ExistsSpec::plan_cache`].
///
/// Errors (`Cancelled`, `Timeout`, `BudgetExceeded`, or planning errors) are never cached.
/// Before invoking the planner, a conservative reservation covers immediate plan nodes
/// while ordinary `VALUES` tables and unattached nested `EXISTS` specs reserve their
/// backing before allocating. Once `ctx.attach_owner()` has run, the live planning charge
/// is resized and converted directly into a [`RetainedCharge`] with no release gap;
/// before attachment (during automatic admission), it is adopted into `admission` and
/// carried across `enable_cursor_accounting` via a [`super::query_memory::QueryMemoryLease`].
pub(super) fn unsubstituted_plan(ctx: &Ctx, spec: &ExistsSpec) -> Result<Arc<Node>> {
    unsubstituted_plan_with_admission(ctx, spec, None)
}

pub(super) fn unsubstituted_plan_with_admission(
    ctx: &Ctx,
    spec: &ExistsSpec,
    admission: Option<&Charge<'_>>,
) -> Result<Arc<Node>> {
    ctx.check()?;
    {
        let mut slot = spec.plan_cache.plan.lock();
        if let Some(cached) = slot.as_mut() {
            if cached.charge.is_none() && ctx.shared().is_some() {
                cached.charge = ctx.retained_charge(cached.bytes)?;
            }
            return Ok(cached.node.clone());
        }
    }
    let pre_bytes = shallow_plan_estimate(spec);
    let plan_guard = match admission {
        Some(held) if ctx.shared().is_none() => {
            let part = held.part();
            part.add(pre_bytes)?;
            part
        }
        _ => ctx.charge(pre_bytes)?,
    };
    #[cfg(test)]
    let built_count = EXISTS_PLANS_BUILT.with(|n| {
        let next = n.get() + 1;
        n.set(next);
        next
    });
    let node = Planner::for_exists_with_charge(ctx, spec, Some(&plan_guard)).plan(
        spec.pattern(),
        &spec.graph,
        Vec::new(),
    )?;
    super::depth::check_plan(node.depth())?;
    let bytes = super::cursor::plan_bytes(&node);
    let node = Arc::new(node);
    let mut slot = spec.plan_cache.plan.lock();
    if let Some(existing) = slot.as_mut() {
        if existing.charge.is_none() && ctx.shared().is_some() {
            existing.charge = ctx.retained_charge(existing.bytes)?;
        }
        return Ok(existing.node.clone());
    }
    let charge = if ctx.shared().is_some() {
        plan_guard.into_retained(bytes)?
    } else {
        plan_guard.resize(bytes)?;
        if let Some(held) = admission {
            let mut transfer = Some(plan_guard);
            held.adopt_charge(&mut transfer)?;
        }
        None
    };
    *slot = Some(CachedPlan {
        node: node.clone(),
        bytes,
        charge,
    });
    drop(slot);
    #[cfg(test)]
    AFTER_CACHE_PLAN_HOOK.with(|hook| {
        if let Some(f) = hook.borrow_mut().as_mut() {
            f(built_count);
        }
    });
    Ok(node)
}

fn visit_exists_specs(
    node: &Node,
    seen: &mut FxHashSet<usize>,
    f: &mut impl FnMut(&Arc<ExistsSpec>),
) {
    let mut specs = Vec::new();
    for expr in super::cursor::own_exprs(&node.kind) {
        expr.exists_specs(&mut specs);
    }
    for spec in specs {
        if seen.insert(Arc::as_ptr(spec) as usize) {
            f(spec);
        }
    }
    match &node.kind {
        super::plan::Kind::GraphBranches(branches) => {
            for branch in &branches.inputs {
                visit_exists_specs(branch, seen, f);
            }
        }
        super::plan::Kind::OrderedTopK(top) => {
            let mut extra = Vec::new();
            for expr in top.filter.iter().chain(&top.range_filter) {
                expr.exists_specs(&mut extra);
            }
            for spec in extra {
                if seen.insert(Arc::as_ptr(spec) as usize) {
                    f(spec);
                }
            }
            visit_exists_specs(&top.fallback, seen, f);
        }
        _ => {}
    }
    for child in &node.children {
        visit_exists_specs(child, seen, f);
    }
}

/// Drop uncharged admission-cached EXISTS plans and structural admission metadata when
/// automatic admission keeps a query eager or aborts on error, so unreached EXISTS
/// branches retain zero overhead and preserve eager peaks.
pub(super) fn clear_cached_plans(node: &Node) {
    fn clear(node: &Node, seen: &mut FxHashSet<usize>) {
        let mut specs = Vec::new();
        visit_exists_specs(node, seen, &mut |spec| specs.push(spec.clone()));
        for spec in specs {
            *spec.plan_cache.admit.lock() = None;
            if let Some(cached) = spec.plan_cache.plan.lock().take() {
                super::depth::guard_exists(|| clear(&cached.node, seen));
            }
        }
    }
    let mut seen = FxHashSet::default();
    clear(node, &mut seen);
}

/// Upgrade every admission-cached EXISTS plan and structural admission metadata under
/// `node` to a [`RetainedCharge`] once `ctx.attach_owner()` has run, splitting directly
/// out of `admission_lease` when present so no pool bytes are released in between.
/// Clears all cached plans and metadata on error.
pub(super) fn charge_cached_plans(
    ctx: &Ctx,
    node: &Node,
    mut admission_lease: Option<&mut super::query_memory::QueryMemoryLease>,
) -> Result<()> {
    fn charge_bytes(
        ctx: &Ctx,
        lease: &mut Option<&mut super::query_memory::QueryMemoryLease>,
        bytes: u64,
    ) -> Result<Option<RetainedCharge>> {
        match lease.as_deref_mut() {
            Some(lease) => ctx.retained_charge_from_lease(lease, bytes),
            None => ctx.retained_charge(bytes),
        }
    }
    fn charge(
        ctx: &Ctx,
        node: &Node,
        seen: &mut FxHashSet<usize>,
        lease: &mut Option<&mut super::query_memory::QueryMemoryLease>,
    ) -> Result<()> {
        let mut specs = Vec::new();
        visit_exists_specs(node, seen, &mut |spec| specs.push(spec.clone()));
        for spec in specs {
            ctx.check()?;
            {
                let mut admit_slot = spec.plan_cache.admit.lock();
                if let Some(admit) = admit_slot.as_mut()
                    && admit.charge.is_none()
                {
                    admit.charge = charge_bytes(ctx, lease, admit.bytes)?;
                }
            }
            let inner = {
                let mut slot = spec.plan_cache.plan.lock();
                if let Some(cached) = slot.as_mut() {
                    if cached.charge.is_none() {
                        cached.charge = charge_bytes(ctx, lease, cached.bytes)?;
                    }
                    Some(cached.node.clone())
                } else {
                    None
                }
            };
            if let Some(inner) = inner {
                super::depth::guard_exists(|| charge(ctx, &inner, seen, lease))?;
            }
        }
        Ok(())
    }
    let mut seen = FxHashSet::default();
    if let Err(err) = charge(ctx, node, &mut seen, &mut admission_lease) {
        clear_cached_plans(node);
        return Err(err);
    }
    Ok(())
}

/// The unsubstituted plan of the key set that would answer the EXISTS of `spec`, when
/// its pattern `P` is in the admitted algebra, wherever the EXISTS appears in an
/// expression. Any other EXISTS is evaluated once per distinct outer key.
///
/// `vars` are the variables of the rows `e` tests and `certain` those that every such
/// row binds. The key set cannot answer a row that binds a risky variable, one that a
/// FILTER inside `P` reads, so a risky variable that a constant or every row binds
/// forces evaluation per key, as do rows that bind none of the key variables. Both are
/// rejected here. A risky variable that only some rows bind stays admitted, since the
/// rows that leave it unbound use the key set.
pub(super) fn keyed_plan(
    ctx: &Ctx,
    spec: &ExistsSpec,
    vars: &[VarId],
    certain: &[VarId],
    admission: Option<&Charge<'_>>,
) -> Result<Option<Arc<Node>>> {
    if !ctx.opt.decorrelate_exists {
        return Ok(None);
    }
    let meta = match admit_meta(ctx, spec, admission, true) {
        Ok(Ok(meta)) => meta,
        Ok(Err(_)) | Err(Error::BudgetExceeded(_)) => return Ok(None),
        Err(e @ (Error::Cancelled | Error::Timeout | Error::Io(_) | Error::Corrupt(_))) => {
            return Err(e);
        }
        Err(_) => return Ok(None),
    };
    let constant = |v: &VarId| spec.bound.iter().any(|(b, _)| b == v);
    if meta
        .risky
        .iter()
        .any(|v| constant(v) || certain.contains(v))
    {
        return Ok(None);
    }
    if !meta.keys.iter().any(|v| constant(v) || vars.contains(v)) {
        return Ok(None);
    }
    let node = match unsubstituted_plan_with_admission(ctx, spec, admission) {
        Ok(node) => node,
        Err(e @ (Error::Cancelled | Error::Timeout | Error::Io(_) | Error::Corrupt(_))) => {
            return Err(e);
        }
        Err(_) => return Ok(None),
    };
    if meta.keys.iter().any(|v| !node.vars.contains(v)) {
        return Ok(None);
    }
    Ok(Some(node))
}

/// Apply the filter conjuncts `EXISTS { P }` / `NOT EXISTS { P }` that can be answered
/// from a key set to `t`. Returns the conjuncts left to evaluate, or `None` when no
/// conjunct was applied.
pub(super) fn apply(ctx: &Ctx, t: &mut Table, exprs: &[Expr]) -> Result<Option<Vec<Expr>>> {
    if !ctx.opt.decorrelate_exists
        || t.is_empty()
        || PER_ROW.with(Cell::get) > 0
        || !exprs.iter().any(|e| exists_of(e).is_some())
    {
        return Ok(None);
    }
    let mut rest = Vec::with_capacity(exprs.len());
    let mut applied = false;
    for e in exprs {
        match exists_of(e) {
            Some((spec, negated)) if !t.is_empty() && probe(ctx, t, e, spec, negated)? => {
                applied = true
            }
            _ => rest.push(e.clone()),
        }
    }
    Ok(applied.then_some(rest))
}

/// Where a variable's value comes from in the rows of a table: its column, else the
/// constant substituted for it before the filter.
#[derive(Clone, Copy)]
struct Slot {
    col: Option<usize>,
    constant: Option<Id>,
}

impl Slot {
    fn of(spec: &ExistsSpec, t: &Table, v: VarId) -> Slot {
        Slot {
            col: t.col_of(v),
            constant: spec.bound.iter().find(|b| b.0 == v).map(|b| b.1),
        }
    }

    fn unbound(self) -> bool {
        self.col.is_none() && self.constant.is_none()
    }

    #[inline]
    fn get(self, t: &Table, i: usize) -> Id {
        match self.col.map(|c| t.cols[c][i]) {
            Some(id) if !id.is_undef() => id,
            _ => self.constant.unwrap_or(Id::UNDEF),
        }
    }
}

fn slots(spec: &ExistsSpec, t: &Table, vars: &[VarId]) -> Vec<Slot> {
    vars.iter().map(|&v| Slot::of(spec, t, v)).collect()
}

/// Filter `t` by the EXISTS conjunct `e` from its key set, if it has (or gets) one.
fn probe(ctx: &Ctx, t: &mut Table, e: &Expr, spec: &ExistsSpec, negated: bool) -> Result<bool> {
    let Some(keep) = answers(ctx, t, e, spec, negated)? else {
        return Ok(false);
    };
    let sorted = t.sorted.clone();
    t.filter_rows(&keep);
    t.sorted = sorted;
    Ok(true)
}

/// The values of `EXISTS { P }` or `NOT EXISTS { P }` for the rows of `t`, as a BIND
/// computes them, from the key set of `P` that a FILTER would use. The same admission
/// and cost rule decides whether the key set is built, and a row that the key set cannot
/// answer is evaluated by substitution, so the values are those of per-row evaluation.
/// `None` leaves every row to per-row evaluation.
pub(super) fn column(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Option<Vec<Id>>> {
    Ok(mask(ctx, t, e)?.map(|values| values.into_iter().map(Id::from_bool).collect()))
}

/// Whether each row of `t` satisfies `e`, when `e` is `EXISTS { P }` or
/// `NOT EXISTS { P }` that the key set of `P` answers, as [`column`] decides. An
/// OPTIONAL condition tests its candidate rows this way.
pub(super) fn mask(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Option<Vec<bool>>> {
    if !ctx.opt.decorrelate_exists || t.is_empty() || PER_ROW.with(Cell::get) > 0 {
        return Ok(None);
    }
    let Some((spec, negated)) = exists_of(e) else {
        return Ok(None);
    };
    answers(ctx, t, e, spec, negated)
}

/// Whether each row of `t` satisfies the EXISTS conjunct `e`, from its key set, if it
/// has (or gets) one. A row that the key set cannot answer is evaluated by
/// substitution.
fn answers(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    spec: &ExistsSpec,
    negated: bool,
) -> Result<Option<Vec<bool>>> {
    // The probe's charge covers `found` and the answers, and it stays held while the
    // rows the key set cannot answer are evaluated.
    let Some((found, held, complete)) = probe_rows(ctx, t, spec, t.len() as u64, true)? else {
        return Ok(None);
    };
    if complete {
        // Every row has its answer from the key set, so one branch-free pass maps the
        // answers in place. The loop below, with its per-row match and evaluation, cost
        // a plain FILTER NOT EXISTS about 6% more instructions per request.
        let keep = found.into_iter().map(|f| (f == FOUND) != negated).collect();
        drop(held);
        return Ok(Some(keep));
    }
    let map = t.var_map(ctx.nvars());
    let mut keep = Vec::with_capacity(found.len());
    for (i, f) in found.into_iter().enumerate() {
        keep.push(match f {
            FOUND => !negated,
            ABSENT => negated,
            _ => {
                if i % 4096 == 0 {
                    ctx.check()?;
                }
                let row = Row {
                    table: t,
                    i,
                    map: &map,
                    dec: None,
                    exists: None,
                };
                ebv(e, &row, ctx).unwrap_or(false)
            }
        });
    }
    drop(held);
    Ok(Some(keep))
}

/// A row whose pattern has no solution that agrees with it.
const ABSENT: u8 = 0;
/// A row whose pattern has a solution that agrees with it.
const FOUND: u8 = 1;
/// A row that the key set cannot answer, which is evaluated by substitution.
const UNKNOWN: u8 = 2;

/// Whether the pattern of `spec` has a solution that agrees with each row of `t`, from
/// its key set, if it has (or gets) one, as [`ABSENT`], [`FOUND`] or [`UNKNOWN`], with
/// the charge that covers the answers and `extra` bytes more for the caller's use of
/// them, and whether every row has its answer, so that none is [`UNKNOWN`]. The caller
/// holds the charge while it uses them. With
/// `count_unknown`, the rows left to substitution are counted for EXPLAIN here, because
/// the caller evaluates every one of them. Otherwise the evaluation counts those it
/// reaches.
fn probe_rows<'a>(
    ctx: &'a Ctx,
    t: &Table,
    spec: &ExistsSpec,
    extra: u64,
    count_unknown: bool,
) -> Result<Option<(Vec<u8>, Charge<'a>, bool)>> {
    let Some(build) = prepare(ctx, spec, t)? else {
        return Ok(None);
    };
    let decor = &spec.decor;
    let keys = slots(spec, t, &build.keys);
    let risky = slots(spec, t, &build.risky);
    if risky.iter().any(|s| s.constant.is_some()) {
        decor.note(RISKY_BOUND);
        return Ok(None);
    }
    // The probe vectors are transient. Retained key sets carry their own charges, and
    // those of a context that was never shared are held while the rows are probed.
    let probe = t.len() as u64 + extra + (build.keys.len() + build.risky.len()) as u64 * 64 + 1024;
    let held = match ctx.charge(if build._charge.is_some() {
        probe
    } else {
        probe.saturating_add(build.bytes())
    }) {
        Ok(h) => h,
        Err(Error::BudgetExceeded(_)) => {
            decor.note("its key set does not fit in the memory budget");
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    // one key read from a column that binds it on every row with a plain term: each row
    // is one lookup (what the loop below does, without its per-row bookkeeping)
    if let ([key], [], KeySet::One(set)) = (keys.as_slice(), risky.as_slice(), &build.full)
        && let Some(c) = key.col
        && !t.cols[c]
            .par_iter()
            .with_min_len(PAR_MIN_LEN)
            .any(|id| id.is_undef() || id.tag() == Tag::Special)
    {
        ctx.check()?;
        let found: Vec<u8> = t.cols[c]
            .par_iter()
            .with_min_len(PAR_MIN_LEN)
            .map(|id| set.contains(id) as u8)
            .collect();
        decor
            .probed
            .fetch_add(found.len() as u64, Ordering::Relaxed);
        return Ok(Some((found, held, true)));
    }
    let width = keys.len();
    let all = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let mut sets: FxHashMap<u64, Option<Arc<Partial>>> = FxHashMap::default();
    let mut key: Vec<Id> = Vec::with_capacity(width);
    let mut found = vec![UNKNOWN; t.len()];
    let (mut probed, mut per_row) = (0u64, 0u64);
    for (i, f) in found.iter_mut().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let mut answer = None;
        if risky.iter().all(|s| s.get(t, i).is_undef()) {
            let (mut mask, mut plain) = (0u64, true);
            key.clear();
            for (j, s) in keys.iter().enumerate() {
                let id = s.get(t, i);
                if !id.is_undef() {
                    // never the value of a pattern variable: left to substitution
                    plain &= id.tag() != Tag::Special;
                    mask |= 1 << j;
                    key.push(id);
                }
            }
            if plain {
                answer = if mask == 0 {
                    Some(build.full.len() > 0)
                } else if mask == all {
                    Some(build.full.contains(&key))
                } else {
                    if let std::collections::hash_map::Entry::Vacant(entry) = sets.entry(mask) {
                        held.add(128)?;
                        entry.insert(partial(ctx, &build, mask, &held));
                    }
                    sets.get(&mask)
                        .and_then(Option::as_ref)
                        .map(|s| s.set.contains(&key))
                };
            }
        }
        match answer {
            Some(a) => {
                probed += 1;
                *f = a as u8;
            }
            None => per_row += 1,
        }
    }
    decor.probed.fetch_add(probed, Ordering::Relaxed);
    if count_unknown {
        decor.per_row.fetch_add(per_row, Ordering::Relaxed);
    }
    Ok(Some((found, held, per_row == 0)))
}

/// The answers of the EXISTS patterns inside larger expressions, such as IF or OR, an
/// aggregate argument or an ORDER BY key, for the rows of one table, from the key sets
/// of their patterns. [`super::expr::eval`] reads a row's answer from here, and
/// evaluates the pattern for the row by substitution when the key set cannot answer it
/// or the pattern has no key set. The same admission and cost rule as for a FILTER
/// decides whether a key set is built. Every row is probed, also those for which the
/// expression never reaches the EXISTS, because a probe costs a hash lookup.
pub struct Answers<'a> {
    table: &'a Table,
    /// the address of each pattern's spec, which is only compared, with its answers
    specs: Vec<(usize, Vec<u8>)>,
}

/// [`Answers`] with the charges of their memory, which the caller holds while it
/// evaluates the rows.
pub struct Nested<'a> {
    pub answers: Answers<'a>,
    _held: Vec<Charge<'a>>,
}

impl Answers<'_> {
    /// The answer of `spec` for row `i` of `t`, when its key set gave one. A row that
    /// the key set of `spec` could not answer is counted as evaluated per row, since the
    /// caller evaluates it by substitution next.
    #[inline]
    pub fn get(&self, spec: &ExistsSpec, t: &Table, i: usize) -> Option<bool> {
        if !std::ptr::eq(self.table, t) {
            return None;
        }
        let (_, found) = self
            .specs
            .iter()
            .find(|(s, _)| *s == std::ptr::from_ref(spec) as usize)?;
        match found.get(i).copied()? {
            FOUND => Some(true),
            ABSENT => Some(false),
            _ => {
                spec.decor.per_row.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}

/// The key-set answers of the EXISTS patterns that `exprs` evaluate for the rows of
/// `t`, or `None` when none of them has a key set.
pub(super) fn nested<'a>(
    ctx: &'a Ctx,
    t: &'a Table,
    exprs: &[&Expr],
) -> Result<Option<Nested<'a>>> {
    if !ctx.opt.decorrelate_exists
        || ctx.calls_extensions
        || t.is_empty()
        || PER_ROW.with(Cell::get) > 0
        || !exprs.iter().any(|e| e.has_exists())
    {
        return Ok(None);
    }
    let mut all = Vec::new();
    for e in exprs {
        e.exists_specs(&mut all);
    }
    let mut specs: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut held = Vec::new();
    for spec in all {
        let ptr = Arc::as_ptr(spec) as usize;
        if specs.iter().any(|(s, _)| *s == ptr) {
            continue;
        }
        if let Some((found, charge, _)) = probe_rows(ctx, t, spec, 0, false)? {
            specs.push((ptr, found));
            held.push(charge);
        }
    }
    Ok((!specs.is_empty()).then_some(Nested {
        answers: Answers { table: t, specs },
        _held: held,
    }))
}

const RISKY_BOUND: &str = "the outer solutions bind a variable that only a FILTER inside it uses";

/// The key set of the keys in `mask`, held by `held` (`None`: it does not fit in the
/// memory budget, and its rows are evaluated per row).
fn partial(ctx: &Ctx, build: &Build, mask: u64, held: &Charge<'_>) -> Option<Arc<Partial>> {
    let mut partial = build.partial.lock();
    if let Some(s) = partial.get(&mask) {
        return Some(s.clone());
    }
    let width = mask.count_ones() as usize;
    // reserve the estimate before building it (a projection has at most as many keys)
    let charge =
        match ctx.retained_charge(build.full.len() as u64 * (width as u64 * 8 + 128) + 1024) {
            Ok(Some(charge)) => Some(charge),
            // A context that was never shared holds the set while its rows are probed.
            Ok(None) => {
                held.add(build.full.len() as u64 * (width as u64 * 8 + 16))
                    .ok()?;
                None
            }
            Err(_) => return None,
        };
    let s = Arc::new(Partial {
        set: build.full.project(build.keys.len(), mask),
        _charge: charge,
    });
    partial.insert(mask, s.clone());
    Some(s)
}

impl Decor {
    fn note(&self, reason: &str) {
        tracing::debug!(target: "sparkles::sparql::exists", "EXISTS evaluated per row: {reason}");
        *self.reason.lock() = Some(reason.to_string());
    }
}

/// The key set of `spec`, built now if the outer table `t` makes it worthwhile.
fn prepare(ctx: &Ctx, spec: &ExistsSpec, t: &Table) -> Result<Option<Arc<Build>>> {
    let decor = &spec.decor;
    let mut st = decor.state.lock();
    match &*st {
        State::Rejected => return Ok(None),
        State::Built(b) => return Ok(Some(b.clone())),
        State::Untried => match admit(ctx, spec)? {
            Ok(plan) => *st = State::Planned(Box::new(plan)),
            Err(reason) => {
                decor.note(&reason);
                *st = State::Rejected;
                spec.plan_cache.release_plan();
                return Ok(None);
            }
        },
        State::Planned(_) => {}
    }
    let State::Planned(plan) = &*st else {
        unreachable!("planned above")
    };
    let keys = slots(spec, t, &plan.keys);
    let risky = slots(spec, t, &plan.risky);
    if keys.iter().all(|s| s.unbound()) {
        decor.note("the outer solutions bind none of its variables");
        return Ok(None);
    }
    if risky.iter().any(|s| s.constant.is_some()) {
        decor.note(RISKY_BOUND);
        return Ok(None);
    }
    // worth it when the pattern costs less than evaluating it for every distinct key of
    // the rows a probe can answer
    let cost = plan.node.cost.max(plan.node.est);
    let need = if forced() {
        1
    } else {
        ((cost / ROW_EVAL_COST).ceil() as usize).max(1)
    };
    // A cursor keeps the keys of its earlier batches, charged as retained state.
    let mut local: FxHashSet<Vec<Id>> = FxHashSet::default();
    let mut cumulative = ctx.streams().then(|| decor.seen.lock());
    let entry = plan.keys.len() as u64 * 8 + 128;
    if let Some(seen) = cumulative.as_deref_mut() {
        let room = need.saturating_sub(seen.keys.len()).min(t.len()) as u64;
        let bytes = (seen.keys.len() as u64 + room) * entry + 1024;
        let reserved = match seen.charge.as_mut() {
            Some(charge) => charge.resize(bytes).map(|()| None),
            None => ctx.retained_charge(bytes),
        };
        match reserved {
            Ok(Some(charge)) => seen.charge = Some(charge),
            Ok(None) => {}
            Err(Error::BudgetExceeded(_)) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    let seen = match cumulative.as_deref_mut() {
        Some(seen) => &mut seen.keys,
        None => &mut local,
    };
    for i in 0..t.len() {
        if i % 1024 == 0 {
            ctx.check()?;
        }
        if seen.len() >= need {
            break;
        }
        if risky.iter().all(|s| s.get(t, i).is_undef()) {
            seen.insert(keys.iter().map(|s| s.get(t, i)).collect());
        }
    }
    let distinct = seen.len();
    if let Some(seen) = cumulative.as_deref_mut()
        && let Some(charge) = seen.charge.as_mut()
    {
        charge.resize(distinct as u64 * entry + 1024)?;
    }
    if distinct == 0 {
        decor.note(RISKY_BOUND);
        return Ok(None);
    }
    if distinct < need {
        decor.note(&format!(
            "evaluating it for the {distinct} distinct outer keys costs less than the whole pattern (estimated cost {cost:.0})"
        ));
        return Ok(None);
    }
    // The key set answers every later batch, so the keys seen so far are not needed.
    if let Some(mut seen) = cumulative.take() {
        *seen = Seen::default();
    }
    if (ctx.rows_within_budget(plan.keys.len()) as f64) < plan.node.est {
        decor.note("its estimated solutions exceed the memory budget");
        *st = State::Rejected;
        spec.plan_cache.release_plan();
        return Ok(None);
    }
    // the keys are the variables these rows can bind (the rows of this filter all have
    // the same columns); a row binding another one is evaluated per row
    let mut keyed = Vec::new();
    let mut risky = plan.risky.clone();
    for (&v, s) in plan.keys.iter().zip(&keys) {
        if s.unbound() {
            risky.push(v);
        } else {
            keyed.push(v);
        }
    }
    let node = plan.node.clone();
    let built = match ctx.cursor_owner() {
        Some(owner) => build_streaming(&owner, &node, keyed, risky),
        None => build(ctx, &node, keyed, risky),
    };
    match built {
        Ok(b) => {
            let b = Arc::new(b);
            *st = State::Built(b.clone());
            *decor.reason.lock() = None;
            spec.plan_cache.release_plan();
            Ok(Some(b))
        }
        Err(e @ (Error::Timeout | Error::Cancelled | Error::Io(_) | Error::Corrupt(_))) => Err(e),
        Err(Error::BudgetExceeded(_)) => {
            decor.note("its key set does not fit in the memory budget");
            *st = State::Rejected;
            spec.plan_cache.release_plan();
            Ok(None)
        }
        Err(e) => {
            decor.note(&format!("building its key set failed ({e})"));
            *st = State::Rejected;
            spec.plan_cache.release_plan();
            Ok(None)
        }
    }
}

/// Evaluate the pattern and collect its distinct keys.
fn build(ctx: &Ctx, node: &Node, keys: Vec<VarId>, risky: Vec<VarId>) -> Result<Build> {
    // Compatibility estimates drop after the input backing. An actual paid
    // materialization keeps its own owner and must not be charged a second time.
    let held = ctx.charge(0)?;
    let (input, _) = super::depth::guard_exists(|| super::exec::execute_materialized(ctx, node))?;
    let t = &input.table;
    if !input.has_credit() {
        held.add(t.mem_bytes())?;
    }
    let cols: Vec<&[Id]> = keys
        .iter()
        .map(|&v| {
            t.col_of(v)
                .map(|c| t.cols[c].as_slice())
                .ok_or_else(|| Error::invalid("a key variable is not bound by the pattern"))
        })
        .collect::<Result<_>>()?;
    if cols.iter().any(|c| c.iter().any(|id| id.is_undef())) {
        return Err(Error::invalid("a key variable is unbound in a solution"));
    }
    let width = cols.len();
    // The hash table may reserve almost twice its cardinality, and multi-column
    // keys allocate individually. Reserve their construction before allocating.
    let charge = ctx.retained_charge(t.len() as u64 * (width as u64 * 8 + 128) + 1024)?;
    let full = if width == 1 {
        KeySet::One(cols[0].iter().copied().collect())
    } else {
        let mut s: FxHashSet<Box<[Id]>> = FxHashSet::default();
        for i in 0..t.len() {
            if i % 4096 == 0 {
                ctx.check()?;
            }
            s.insert(cols.iter().map(|c| c[i]).collect());
        }
        KeySet::Many(s)
    };
    // A context that was never shared charges the set while the solutions are held, so
    // that it fails over budget (and so falls back) before they are dropped.
    if charge.is_none() {
        held.add(full.bytes(width))?;
    }
    Ok(Build {
        keys,
        risky,
        solutions: t.len(),
        full,
        partial: Mutex::new(FxHashMap::default()),
        _charge: charge,
    })
}

/// Collect the distinct keys of the pattern from a cursor over its plan. Only the key
/// set is kept, never the pattern's solutions, and it is charged before every batch
/// grows it.
fn build_streaming(
    ctx: &Arc<Ctx>,
    node: &Node,
    keys: Vec<VarId>,
    risky: Vec<VarId>,
) -> Result<Build> {
    let width = keys.len();
    // A key's slot, control byte and the old table while the set doubles, plus the
    // boxed ids of a key over several variables.
    let entry = if width == 1 {
        32
    } else {
        width as u64 * 8 + 80
    };
    let mut charge = ctx.retained_charge(1024)?;
    let mut one: FxHashSet<Id> = FxHashSet::default();
    let mut many: FxHashSet<Box<[Id]>> = FxHashSet::default();
    // Size the set for the pattern's estimated solutions when a quarter of the
    // remaining budget covers them, so that it does not rehash as it fills. Growing
    // from empty spent a fifth of a NOT EXISTS query in rehashing.
    let expected = node.est.clamp(0.0, u32::MAX as f64) as usize;
    let mut floor = 1024;
    let presized = expected as u64 * entry + 1024;
    if let Some(retained) = charge.as_mut()
        && expected > 0
        && presized < ctx.memory_remaining() / 4
    {
        retained.resize(presized)?;
        floor = presized;
        if width == 1 {
            one.reserve(expected);
        } else {
            many.reserve(expected);
        }
    }
    let mut solutions = 0usize;
    super::depth::guard_exists(|| {
        super::cursor::drain(ctx, node, &mut |t: &Table| {
            let cols: Vec<&[Id]> = keys
                .iter()
                .map(|&v| {
                    t.col_of(v)
                        .map(|c| t.cols[c].as_slice())
                        .ok_or_else(|| Error::invalid("a key variable is not bound by the pattern"))
                })
                .collect::<Result<_>>()?;
            if cols.iter().any(|c| c.iter().any(|id| id.is_undef())) {
                return Err(Error::invalid("a key variable is unbound in a solution"));
            }
            let held = one.len() + many.len();
            // Reserve for every row of the batch being new before the set grows.
            if let Some(charge) = charge.as_mut() {
                charge.resize(((held + t.len()) as u64 * entry + 1024).max(floor))?;
            }
            if width == 1 {
                one.extend(cols[0].iter().copied());
            } else {
                for i in 0..t.len() {
                    many.insert(cols.iter().map(|c| c[i]).collect());
                }
            }
            solutions += t.len();
            if let Some(charge) = charge.as_mut() {
                charge.resize(((one.len() + many.len()) as u64 * entry + 1024).max(floor))?;
            }
            Ok(())
        })
    })?;
    let full = if width == 1 {
        KeySet::One(one)
    } else {
        KeySet::Many(many)
    };
    Ok(Build {
        keys,
        risky,
        solutions,
        full,
        partial: Mutex::new(FxHashMap::default()),
        _charge: charge,
    })
}

// ------------------------------------------------------------------ admission ------

/// Structural admission of `spec`: classify its key and risky variables once per
/// `ExistsSpec` without mutating `Decor::reason` or `Decor::state`. When `cache` is
/// true, retains the result charged to `ctx` (or `admission` before attachment); when
/// `cache` is false (`admit` in `prepare`), consumes any previously cached entry and
/// drops its charge immediately.
fn admit_meta(
    ctx: &Ctx,
    spec: &ExistsSpec,
    admission: Option<&Charge<'_>>,
    cache: bool,
) -> Result<std::result::Result<AdmitMeta, String>> {
    ctx.check()?;
    {
        let mut guard = spec.plan_cache.admit.lock();
        if cache {
            if let Some(cached) = guard.as_mut() {
                if cached.charge.is_none() && ctx.shared().is_some() {
                    cached.charge = ctx.retained_charge(cached.bytes)?;
                }
                return Ok(cached.result.clone());
            }
        } else if let Some(cached) = guard.take() {
            return Ok(cached.result);
        }
    }
    let computed = (|| -> std::result::Result<AdmitMeta, String> {
        // Evaluating a key set can reach a callback under an untaken expression arm.
        if ctx
            .extensions
            .as_ref()
            .is_some_and(|r| !r.is_empty() && r.references(spec.pattern()))
        {
            return Err("it calls a registered extension".into());
        }
        if let ActiveGraph::Var(_) = spec.graph {
            return Err("its active graph is a variable".into());
        }
        let mut risky = FxHashSet::default();
        let cert = certain(ctx, spec.pattern(), &mut risky)?;
        let keys: Vec<VarId> = spec
            .vars
            .iter()
            .copied()
            .filter(|v| !risky.contains(v))
            .collect();
        if let Some(v) = keys.iter().find(|v| !cert.contains(v)) {
            return Err(format!(
                "?{} is not bound by every solution",
                ctx.var_name(*v)
            ));
        }
        if keys.is_empty() {
            return Err("it has no variable to correlate on".into());
        }
        if keys.len() > 64 {
            return Err("it has more than 64 variables".into());
        }
        let mut risky: Vec<VarId> = risky
            .into_iter()
            .filter(|v| spec.vars.contains(v))
            .collect();
        risky.sort_unstable();
        Ok(AdmitMeta { keys, risky })
    })();
    if !cache {
        return Ok(computed);
    }
    let bytes = admit_bytes(&computed);
    let mut guard = spec.plan_cache.admit.lock();
    if let Some(existing) = guard.as_mut() {
        if existing.charge.is_none() && ctx.shared().is_some() {
            existing.charge = ctx.retained_charge(existing.bytes)?;
        }
        return Ok(existing.result.clone());
    }
    let charge = if ctx.shared().is_some() {
        ctx.retained_charge(bytes)?
    } else if let Some(held) = admission {
        held.add(bytes)?;
        None
    } else {
        // Unattached context without an admission guard: do not retain uncharged backing.
        return Ok(computed);
    };
    *guard = Some(CachedAdmit {
        result: computed.clone(),
        bytes,
        charge,
    });
    Ok(computed)
}

/// Check the pattern against the admitted algebra and reuse its unsubstituted plan.
fn admit(ctx: &Ctx, spec: &ExistsSpec) -> Result<std::result::Result<Plan, String>> {
    let meta = match admit_meta(ctx, spec, None, false) {
        Ok(Ok(meta)) => meta,
        Ok(Err(reason)) => return Ok(Err(reason)),
        Err(e @ (Error::Timeout | Error::Cancelled | Error::Io(_) | Error::Corrupt(_))) => {
            return Err(e);
        }
        Err(Error::BudgetExceeded(budget)) => {
            return Ok(Err(Error::BudgetExceeded(budget).to_string()));
        }
        Err(e) => return Ok(Err(format!("admitting it failed ({e})"))),
    };
    let node = match unsubstituted_plan(ctx, spec) {
        Ok(node) => node,
        Err(e @ (Error::Timeout | Error::Cancelled | Error::Io(_) | Error::Corrupt(_))) => {
            return Err(e);
        }
        Err(Error::BudgetExceeded(budget)) => {
            return Ok(Err(Error::BudgetExceeded(budget).to_string()));
        }
        Err(e) => return Ok(Err(format!("planning it failed ({e})"))),
    };
    if let Some(v) = meta.keys.iter().find(|v| !node.vars.contains(v)) {
        return Ok(Err(format!("its plan does not bind ?{}", ctx.var_name(*v))));
    }
    Ok(Ok(Plan {
        keys: meta.keys,
        risky: meta.risky,
        node,
    }))
}

/// `cert(gp)` for an admitted pattern (see the module documentation); adds the risky
/// variables of its filters to `risky`.
pub(super) fn certain(
    ctx: &Ctx,
    gp: &GraphPattern,
    risky: &mut FxHashSet<VarId>,
) -> std::result::Result<FxHashSet<VarId>, String> {
    use GraphPattern as GP;
    let mut out = FxHashSet::default();
    match gp {
        GP::Bgp { patterns } => {
            property_functions(patterns)?;
            for t in patterns {
                for x in [&t.subject, &t.object] {
                    match x {
                        TermPattern::Variable(v) => {
                            out.insert(ctx.var(v.as_str()));
                        }
                        TermPattern::Triple(_) => {
                            return Err("it has a triple term pattern".into());
                        }
                        _ => {}
                    }
                }
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    out.insert(ctx.var(v.as_str()));
                }
            }
        }
        GP::Path {
            subject,
            path,
            object,
        } => {
            if zero_length(path) {
                return Err("it has a path that can match zero-length paths".into());
            }
            for x in [subject, object] {
                match x {
                    TermPattern::Variable(v) => {
                        out.insert(ctx.var(v.as_str()));
                    }
                    TermPattern::Triple(_) => {
                        return Err("it has a triple term pattern".into());
                    }
                    _ => {}
                }
            }
        }
        GP::Join { left, right } => {
            out = certain(ctx, left, risky)?;
            out.extend(certain(ctx, right, risky)?);
        }
        GP::Graph { name, inner } => {
            out = certain(ctx, inner, risky)?;
            if let NamedNodePattern::Variable(v) = name {
                out.insert(ctx.var(v.as_str()));
            }
        }
        GP::Filter { expr, inner } => {
            out = certain(ctx, inner, risky)?;
            if !pure(expr) {
                return Err("a FILTER inside it is not deterministic".into());
            }
            let mut names = Vec::new();
            expr_vars(expr, &mut names);
            for n in names {
                let v = ctx.var(&n);
                if !out.contains(&v) {
                    risky.insert(v);
                }
            }
        }
        other => return Err(format!("it has {}", construct(other))),
    }
    Ok(out)
}

fn construct(gp: &GraphPattern) -> &'static str {
    use GraphPattern as GP;
    match gp {
        GP::LeftJoin { .. } => "an OPTIONAL",
        GP::Union { .. } => "a UNION",
        GP::Minus { .. } => "a MINUS",
        GP::Extend { .. } => "a BIND",
        GP::Assign { .. } => "a LET",
        GP::Unfold { .. } => "an UNFOLD",
        GP::Values { .. } => "a VALUES block",
        GP::Service { .. } => "a SERVICE call",
        GP::Group { .. } | GP::Project { .. } => "a sub-select",
        GP::Lateral { .. } => "a LATERAL",
        _ => "an unsupported construct",
    }
}

/// Property functions compute their solutions instead of matching triples (a text
/// search with a result limit and a bound subject is not the unbound search restricted).
fn property_functions(patterns: &[TriplePattern]) -> std::result::Result<(), String> {
    let call = |found: crate::error::Result<bool>| match found {
        Ok(false) => Ok(()),
        _ => Err("it calls a property function".to_string()),
    };
    call(Ok(super::arqpf::has_calls(patterns)))?;
    call(super::textpf::extract(patterns).map(|(c, _)| !c.is_empty()))?;
    call(
        super::textpf::take_calls(patterns, crate::vector::VECTOR_SEARCH, "spk:vectorSearch")
            .map(|(c, _)| !c.is_empty()),
    )?;
    call(
        super::textpf::take_calls(patterns, super::hybrid::HYBRID_SEARCH, "spk:hybridSearch")
            .map(|(c, _)| !c.is_empty()),
    )?;
    call(super::geopf::take_spatial_calls(patterns).map(|(c, _)| !c.is_empty()))
}

/// Can the path match a zero-length path (`*` or `?` anywhere in it)?
fn zero_length(p: &PropertyPathExpression) -> bool {
    use PropertyPathExpression as PP;
    match p {
        PP::NamedNode(_) | PP::NegatedPropertySet(_) => false,
        PP::Reverse(a) | PP::OneOrMore(a) => zero_length(a),
        PP::Sequence(a, b) | PP::Alternative(a, b) => zero_length(a) || zero_length(b),
        PP::ZeroOrMore(_) | PP::ZeroOrOne(_) => true,
        PP::Range { path, min, .. } => *min == 0 || zero_length(path),
        PP::Distinct(a) | PP::Multi(a) | PP::Shortest(a) => zero_length(a),
    }
}

/// Free of functions whose value differs between calls with the same arguments (also in
/// nested EXISTS patterns).
fn pure(e: &Expression) -> bool {
    use Expression as E;
    match e {
        E::FunctionCall(
            Function::Rand | Function::Uuid | Function::StrUuid | Function::BNode,
            _,
        ) => false,
        E::FunctionCall(_, l) | E::Coalesce(l) => l.iter().all(pure),
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => pure(a) && pure(b),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => pure(a),
        E::In(a, l) => pure(a) && l.iter().all(pure),
        E::If(a, b, c) => pure(a) && pure(b) && pure(c),
        E::Exists(p) => pure_pattern(p),
        E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => true,
    }
}

pub(super) fn pure_pattern(gp: &GraphPattern) -> bool {
    use GraphPattern as GP;
    match gp {
        GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => true,
        GP::Filter { expr, inner } => pure(expr) && pure_pattern(inner),
        GP::Extend {
            inner, expression, ..
        }
        | GP::Assign {
            inner, expression, ..
        }
        | GP::Unfold {
            inner, expression, ..
        } => pure(expression) && pure_pattern(inner),
        GP::LeftJoin {
            left,
            right,
            expression,
        } => expression.as_ref().is_none_or(pure) && pure_pattern(left) && pure_pattern(right),
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right } => pure_pattern(left) && pure_pattern(right),
        GP::Graph { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::Project { inner, .. } => pure_pattern(inner),
        GP::OrderBy { inner, expression } => {
            expression.iter().all(|o| match o {
                OrderExpression::Asc(e) | OrderExpression::Desc(e) => pure(e),
            }) && pure_pattern(inner)
        }
        GP::Group {
            inner, aggregates, ..
        } => {
            aggregates.iter().all(|(_, a)| match a {
                AggregateExpression::CountSolutions { .. } => true,
                AggregateExpression::FunctionCall { expr, .. } => pure(expr),
                AggregateExpression::Fold {
                    expr, value, order, ..
                } => {
                    pure(expr)
                        && value.as_ref().is_none_or(pure)
                        && order.iter().all(|o| match o {
                            OrderExpression::Asc(e) | OrderExpression::Desc(e) => pure(e),
                        })
                }
            }) && pure_pattern(inner)
        }
        // a remote endpoint answers as it likes
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

// --------------------------------------------------------------------- explain ------

/// The EXPLAIN note and counters of the EXISTS conjuncts of a filter that were
/// decorrelated, or left to per-row evaluation for a reason.
pub(super) fn explain(
    ctx: &Ctx,
    exprs: &[Expr],
) -> (
    Option<String>,
    Option<serde_json::Map<String, serde_json::Value>>,
) {
    let mut notes = Vec::new();
    let mut counters = serde_json::Map::new();
    let mut add = |k: &str, n: u64| {
        let v = counters.get(k).and_then(|v| v.as_u64()).unwrap_or(0) + n;
        counters.insert(k.into(), v.into());
    };
    let mut specs: Vec<(&ExistsSpec, bool)> = Vec::new();
    for e in exprs {
        match exists_of(e) {
            Some(top) => specs.push(top),
            None => {
                let mut nested = Vec::new();
                e.exists_specs(&mut nested);
                specs.extend(nested.into_iter().map(|s| (&**s, false)));
            }
        }
    }
    for (spec, negated) in specs {
        let d = &spec.decor;
        let what = if negated { "NOT EXISTS" } else { "EXISTS" };
        match &*d.state.lock() {
            State::Built(b) => {
                let vars: Vec<String> = b
                    .keys
                    .iter()
                    .map(|v| ctx.var_name(*v))
                    .filter(|n| !n.starts_with(' '))
                    .map(|n| format!("?{n}"))
                    .collect();
                let (probed, per_row) = (
                    d.probed.load(Ordering::Relaxed),
                    d.per_row.load(Ordering::Relaxed),
                );
                notes.push(format!(
                    "[{what} decorrelated on {}: {} keys from {} solutions, {probed} rows probed, {per_row} per row]",
                    vars.join(" "),
                    b.full.len(),
                    b.solutions
                ));
                add("existsKeys", b.full.len() as u64);
                add("existsSolutions", b.solutions as u64);
                add("existsProbed", probed);
                add("existsPerRow", per_row);
            }
            _ => {
                if let Some(r) = &*d.reason.lock() {
                    notes.push(format!("[{what} per row: {r}]"));
                }
            }
        }
    }
    (
        (!notes.is_empty()).then(|| notes.join(" ")),
        (!counters.is_empty()).then_some(counters),
    )
}

#[cfg(test)]
mod borrowed_input_tests {
    use super::*;

    #[test]
    fn reviewed_exists_build_keeps_borrowed_table_without_forced_ledger_or_duplicate_input() {
        let store = crate::store::Store::in_memory(Default::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.use_cache = false;
        ctx.var("value");
        let value = Id::from_i64(1).unwrap();
        let table = Table {
            vars: vec![0],
            cols: vec![vec![value]],
            len: 1,
            sorted: Vec::new(),
        };
        let node = Node::leaf(
            super::super::plan::Kind::Values(table),
            vec![0],
            1.0,
            "exists input".into(),
        );
        let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        let built = build(&ctx, &node, vec![0], Vec::new()).unwrap();
        assert_eq!(built.solutions, 1);
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before
        );
        drop(built);
        assert_eq!(ctx.live_bytes(), 0);
    }
}
