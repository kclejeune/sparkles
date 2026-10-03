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
//! EXISTS is evaluated per row as before. The EXISTS must be a top-level conjunct of a
//! FILTER (`EXISTS` or `NOT EXISTS`, possibly beside other conjuncts); one under OR, IF,
//! BIND or an OPTIONAL's condition is evaluated per row. The active graph of the EXISTS
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

use super::ctx::{Charge, Ctx};
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
use std::sync::atomic::{AtomicU64, Ordering};

/// Planned cost of the pattern that evaluating it for one distinct outer key is assumed
/// to be worth (planning and running a substituted pattern takes tens of microseconds;
/// hashing a solution of the whole pattern well under one).
const ROW_EVAL_COST: f64 = 512.0;

thread_local! {
    /// depth of per-row EXISTS evaluations on this thread
    static PER_ROW: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// tests: decorrelate whenever the pattern is admitted, whatever it costs
    pub(crate) static FORCE: Cell<bool> = const { Cell::new(false) };
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
    node: Node,
}

struct Build {
    keys: Vec<VarId>,
    risky: Vec<VarId>,
    /// rows of the pattern
    solutions: usize,
    /// distinct projections on all keys
    full: KeySet,
    /// distinct projections on the keys of a mask (bit i: `keys[i]`), built on demand
    partial: Mutex<FxHashMap<u64, Arc<KeySet>>>,
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
                .map(|(m, s)| s.bytes(m.count_ones() as usize))
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
    let Some(build) = prepare(ctx, spec, t)? else {
        return Ok(false);
    };
    let decor = &spec.decor;
    let keys = slots(spec, t, &build.keys);
    let risky = slots(spec, t, &build.risky);
    if risky.iter().any(|s| s.constant.is_some()) {
        decor.note(RISKY_BOUND);
        return Ok(false);
    }
    // the key sets are alive while the rows are probed
    let held = match ctx.charge(build.bytes()) {
        Ok(h) => h,
        Err(Error::BudgetExceeded(_)) => {
            decor.note("its key set does not fit in the memory budget");
            return Ok(false);
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
        let keep: Vec<bool> = t.cols[c]
            .par_iter()
            .with_min_len(PAR_MIN_LEN)
            .map(|id| set.contains(id) != negated)
            .collect();
        drop(held);
        decor.probed.fetch_add(keep.len() as u64, Ordering::Relaxed);
        let sorted = t.sorted.clone();
        t.filter_rows(&keep);
        t.sorted = sorted;
        return Ok(true);
    }
    let width = keys.len();
    let all = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let map = t.var_map(ctx.nvars());
    let mut sets: FxHashMap<u64, Option<Arc<KeySet>>> = FxHashMap::default();
    let mut key: Vec<Id> = Vec::with_capacity(width);
    let mut keep = vec![false; t.len()];
    let (mut probed, mut per_row) = (0u64, 0u64);
    for (i, k) in keep.iter_mut().enumerate() {
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
                    sets.entry(mask)
                        .or_insert_with(|| partial(&build, mask, &held))
                        .as_ref()
                        .map(|s| s.contains(&key))
                };
            }
        }
        *k = match answer {
            Some(found) => {
                probed += 1;
                found != negated
            }
            None => {
                per_row += 1;
                let row = Row {
                    table: t,
                    i,
                    map: &map,
                    dec: None,
                };
                ebv(e, &row, ctx).unwrap_or(false)
            }
        };
    }
    drop(held);
    decor.probed.fetch_add(probed, Ordering::Relaxed);
    decor.per_row.fetch_add(per_row, Ordering::Relaxed);
    let sorted = t.sorted.clone();
    t.filter_rows(&keep);
    t.sorted = sorted;
    Ok(true)
}

const RISKY_BOUND: &str = "the outer solutions bind a variable that only a FILTER inside it uses";

/// The key set of the keys in `mask`, held by `held` (`None`: it does not fit in the
/// memory budget, and its rows are evaluated per row).
fn partial(build: &Build, mask: u64, held: &Charge<'_>) -> Option<Arc<KeySet>> {
    if let Some(s) = build.partial.lock().get(&mask) {
        return Some(s.clone());
    }
    let width = mask.count_ones() as usize;
    // reserve the estimate before building it (a projection has at most as many keys)
    held.add(build.full.len() as u64 * (width as u64 * 8 + 16))
        .ok()?;
    let s = Arc::new(build.full.project(build.keys.len(), mask));
    build.partial.lock().insert(mask, s.clone());
    Some(s)
}

impl Decor {
    fn note(&self, reason: &str) {
        tracing::debug!("EXISTS evaluated per row: {reason}");
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
        State::Untried => match admit(ctx, spec) {
            Ok(plan) => *st = State::Planned(Box::new(plan)),
            Err(reason) => {
                decor.note(&reason);
                *st = State::Rejected;
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
    let mut seen: FxHashSet<Vec<Id>> = FxHashSet::default();
    for i in 0..t.len() {
        if seen.len() >= need {
            break;
        }
        if risky.iter().all(|s| s.get(t, i).is_undef()) {
            seen.insert(keys.iter().map(|s| s.get(t, i)).collect());
        }
    }
    if seen.is_empty() {
        decor.note(RISKY_BOUND);
        return Ok(None);
    }
    if seen.len() < need {
        decor.note(&format!(
            "evaluating it for the {} distinct outer keys costs less than the whole pattern (estimated cost {cost:.0})",
            seen.len()
        ));
        return Ok(None);
    }
    if (ctx.rows_within_budget(plan.keys.len()) as f64) < plan.node.est {
        decor.note("its estimated solutions exceed the memory budget");
        *st = State::Rejected;
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
    match build(ctx, &plan.node, keyed, risky) {
        Ok(b) => {
            let b = Arc::new(b);
            *st = State::Built(b.clone());
            *decor.reason.lock() = None;
            Ok(Some(b))
        }
        Err(e @ (Error::Timeout | Error::Cancelled | Error::Io(_) | Error::Corrupt(_))) => Err(e),
        Err(Error::BudgetExceeded(_)) => {
            decor.note("its key set does not fit in the memory budget");
            *st = State::Rejected;
            Ok(None)
        }
        Err(e) => {
            decor.note(&format!("building its key set failed ({e})"));
            *st = State::Rejected;
            Ok(None)
        }
    }
}

/// Evaluate the pattern and collect its distinct keys.
fn build(ctx: &Ctx, node: &Node, keys: Vec<VarId>, risky: Vec<VarId>) -> Result<Build> {
    let (t, _) = super::exec::execute(ctx, node)?;
    let held = ctx.charge(t.mem_bytes())?;
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
    // fails over budget (and so falls back) before the solutions are dropped
    held.add(full.bytes(width))?;
    Ok(Build {
        keys,
        risky,
        solutions: t.len(),
        full,
        partial: Mutex::new(FxHashMap::default()),
    })
}

// ------------------------------------------------------------------ admission ------

/// Check the pattern against the admitted algebra and plan it unsubstituted.
fn admit(ctx: &Ctx, spec: &ExistsSpec) -> std::result::Result<Plan, String> {
    if let ActiveGraph::Var(_) = spec.graph {
        return Err("its active graph is a variable".into());
    }
    let mut risky = FxHashSet::default();
    let cert = certain(ctx, &spec.pattern, &mut risky)?;
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
    let node = Planner::new(ctx)
        .plan(&spec.pattern, &spec.graph, Vec::new())
        .map_err(|e| format!("planning it failed ({e})"))?;
    if let Some(v) = keys.iter().find(|v| !node.vars.contains(v)) {
        return Err(format!("its plan does not bind ?{}", ctx.var_name(*v)));
    }
    let mut risky: Vec<VarId> = risky
        .into_iter()
        .filter(|v| spec.vars.contains(v))
        .collect();
    risky.sort_unstable();
    Ok(Plan { keys, risky, node })
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
        | GP::Minus { left, right } => pure_pattern(left) && pure_pattern(right),
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
    for (spec, negated) in exprs.iter().filter_map(exists_of) {
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

/// Add the EXPLAIN note and counters of [`explain`] to the runtime information of a
/// filter.
pub(super) fn annotate(ctx: &Ctx, info: &mut super::exec::PlanInfo, exprs: &[Expr]) {
    let (note, counters) = explain(ctx, exprs);
    if let Some(n) = note {
        info.description = format!("{} {n}", info.description);
    }
    if counters.is_some() {
        info.counters = counters;
    }
}
