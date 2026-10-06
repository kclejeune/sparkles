//! `LATERAL` (Jena ARQ, SPARQL proposal SEP-0006): `Lateral(L, R)` evaluates `R` once
//! per solution μ of `L`, with the variables μ binds replaced by their values, and
//! merges each solution of the substituted `R` with μ. A variable μ leaves unbound is not
//! replaced. Inside a sub-select of `R`, a variable the sub-select does not project is a
//! different variable and is not replaced either (see [`Planner::scoped`]).
//!
//! # Plans
//!
//! When substitution cannot change `R`'s solutions, `Lateral(L, R)` is the join of `L`
//! and `R` ([`join_equivalent`]), and the planner plans it as one, inside its group's
//! join order. That holds when `R` mentions no variable in scope in `L`, or when `R` is
//! in the algebra whose substituted solutions are its solutions restricted to μ: basic
//! graph patterns, paths that cannot match a zero-length path, joins, `GRAPH` and
//! deterministic FILTERs, with no variable of `L` *risky* in `R` (used by a FILTER over a
//! pattern that does not always bind it). This is the analysis, and the argument, of the
//! EXISTS decorrelation in [`super::exists`].
//!
//! Any other lateral is a [`Kind::Lateral`] operator over `L`'s plan. It groups `L`'s
//! rows by their values of the variables `R` mentions, plans `R` for each group with
//! those values as constants of the planner (as `EXISTS` is evaluated per row), runs it
//! and joins its solutions with the group's rows.

use super::ctx::Ctx;
use super::exec::{execute, join_tables};
use super::plan::{ActiveGraph, Kind, Node, Planner, collect_pattern_vars, join_est};
use super::table::{Table, VarId};
use crate::error::Result;
use crate::id::Id;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::GraphPattern;

/// A `LATERAL` evaluated per group of the left side's rows.
#[derive(Clone)]
pub struct LateralSpec {
    /// the right side
    pub pattern: GraphPattern,
    /// the active graph of the right side
    pub graph: ActiveGraph,
    /// the variables of the left side that the right side mentions: substituted per
    /// group when bound
    pub keys: Vec<VarId>,
    /// the constants in force where the LATERAL was planned (initial bindings, the row
    /// of an enclosing EXISTS or LATERAL)
    pub outer: Vec<(VarId, Id)>,
    /// which of `outer` a sub-select hides (see [`Planner::scoped`])
    pub outer_scoped: Vec<VarId>,
    /// an OPTIONAL evaluated per left row (ARQ's property functions read the left
    /// side's values): a left row without solutions on the right is kept, and the
    /// expression is the OPTIONAL's filter
    pub optional: Option<Option<spargebra::algebra::Expression>>,
    /// Jena's `SERVICE <loop:>`: the substitution also reaches the variables of
    /// sub-selects that do not project them, and the keys are every variable of the
    /// left side that the right side mentions anywhere
    pub unscoped: bool,
    /// `SERVICE <loop:…>` over a remote endpoint: the right side is sent there, for
    /// several groups at once with `bulk` (see [`super::enhancer`])
    pub service: Option<Box<super::enhancer::RemoteLoop>>,
}

/// The names of the variables `R` mentions where substitution reaches them: in scope,
/// in FILTERs, BIND and OPTIONAL expressions and EXISTS patterns, and projected by its
/// sub-selects.
fn mentioned(right: &GraphPattern) -> Vec<String> {
    let mut names = Vec::new();
    collect_pattern_vars(right, &mut names);
    names.sort_unstable();
    names.dedup();
    names
}

/// The variables that can be bound in `L`'s solutions and are not constants already.
fn left_names(p: &Planner<'_>, left: &GraphPattern) -> FxHashSet<String> {
    let mut out = FxHashSet::default();
    left.on_in_scope_variable(|v| {
        if !p.subst.contains_key(&p.ctx.var(v.as_str())) {
            out.insert(v.as_str().to_string());
        }
    });
    out
}

/// Whether `Lateral(left, right)` has the solutions of the join of `left` and `right`
/// (see the module documentation).
pub(super) fn join_equivalent(p: &Planner<'_>, left: &GraphPattern, right: &GraphPattern) -> bool {
    if p.ctx
        .extensions
        .as_ref()
        .is_some_and(|r| r.references(right))
    {
        return false;
    }
    let lv = left_names(p, left);
    if mentioned(right).iter().all(|n| !lv.contains(n)) {
        return true;
    }
    let mut risky = FxHashSet::default();
    match super::exists::certain(p.ctx, right, &mut risky) {
        Ok(_) => risky.iter().all(|v| !lv.contains(&p.ctx.var_name(*v))),
        Err(_) => false,
    }
}

/// The plan of `Lateral(left, right)` that evaluates `right` per group of `left`'s rows.
pub(super) fn plan(
    p: &mut Planner<'_>,
    left: Node,
    right: &GraphPattern,
    g: &ActiveGraph,
) -> Result<Node> {
    plan_with(p, left, right, g, None, false)
}

/// The plan of `LeftJoin(left, right, expr)` that evaluates `right` per group of
/// `left`'s rows, as ARQ evaluates an OPTIONAL with substitution.
pub(super) fn plan_optional(
    p: &mut Planner<'_>,
    left: Node,
    right: &GraphPattern,
    expr: Option<&spargebra::algebra::Expression>,
    g: &ActiveGraph,
) -> Result<Node> {
    plan_with(p, left, right, g, Some(expr.cloned()), false)
}

/// The plan of `left` joined (or, with `optional`, left-joined) with
/// `SERVICE <loop:> { right }` over the dataset itself: a `LATERAL` whose substitution
/// reaches into sub-selects, evaluated against the query's default graph as a separate
/// query would be.
pub(super) fn plan_loop_self(
    p: &mut Planner<'_>,
    left: Node,
    right: &GraphPattern,
    optional: Option<Option<spargebra::algebra::Expression>>,
) -> Result<Node> {
    plan_with(p, left, right, &ActiveGraph::Default, optional, true)
}

fn plan_with(
    p: &mut Planner<'_>,
    left: Node,
    right: &GraphPattern,
    g: &ActiveGraph,
    optional: Option<Option<spargebra::algebra::Expression>>,
    unscoped: bool,
) -> Result<Node> {
    let ctx = p.ctx;
    let names = if unscoped {
        super::enhancer::all_vars(right)
    } else {
        mentioned(right)
    };
    let keys: Vec<VarId> = names
        .iter()
        .map(|n| ctx.var(n))
        .filter(|v| left.vars.contains(v))
        .collect();
    // Schema-only outer inputs allow planning required property arguments without
    // executing callbacks. Actual bound substitutions are installed per left row.
    let calls_extensions = ctx.extensions.as_ref().is_some_and(|r| r.references(right));
    let old_inputs = p.property_inputs.clone();
    let old_scoped = if calls_extensions {
        Some(p.scoped.clone())
    } else {
        None
    };
    if calls_extensions {
        p.property_inputs.extend(keys.iter().copied());
        if !unscoped {
            p.scoped.extend(keys.iter().copied());
        }
    }
    let planned = p.plan(right, g, Vec::new());
    p.property_inputs = old_inputs;
    if let Some(scoped) = old_scoped {
        p.scoped = scoped;
    }
    let r = planned?;
    if keys.is_empty() && optional.is_none() && !calls_extensions {
        return Ok(super::plan::join(left, r, ctx));
    }
    let mut vars = left.vars.clone();
    let mut certain = left.certain.clone();
    for v in &r.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    if optional.is_none() {
        for v in &r.certain {
            if !certain.contains(v) {
                certain.push(*v);
            }
        }
    }
    let est = match optional {
        Some(_) => join_est(&left, &r, &keys).max(left.est).max(1.0),
        None => join_est(&left, &r, &keys).max(1.0),
    };
    // a group plans and runs the right side, a fraction of its unsubstituted cost
    let groups = keys
        .iter()
        .map(|k| left.dist.get(k).copied().unwrap_or(left.est))
        .fold(1.0f64, |a, b| a * b.max(1.0))
        .min(left.est.max(1.0));
    let cost = left.cost + groups * (64.0 + r.cost / groups.max(1.0)) + est;
    let dist = vars
        .iter()
        .map(|v| {
            let d = left
                .dist
                .get(v)
                .or_else(|| r.dist.get(v))
                .copied()
                .unwrap_or(est);
            (*v, d.min(est))
        })
        .collect();
    let desc = format!(
        "{}per row on {}",
        if optional.is_some() { "OPTIONAL " } else { "" },
        keys.iter()
            .map(|k| format!("?{}", ctx.var_name(*k)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut outer: Vec<(VarId, Id)> = p.subst.iter().map(|(v, id)| (*v, *id)).collect();
    outer.sort_unstable_by_key(|(v, _)| *v);
    let mut outer_scoped: Vec<VarId> = p.scoped.iter().copied().collect();
    outer_scoped.sort_unstable();
    let spec = LateralSpec {
        pattern: right.clone(),
        graph: g.clone(),
        keys,
        outer,
        outer_scoped,
        optional,
        unscoped,
        service: None,
    };
    Ok(Node {
        kind: Kind::Lateral(Box::new(spec)),
        children: vec![left],
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost,
        dist,
        desc,
    })
}

/// Run a [`LateralSpec`] over the left side's rows `l`: the rows of `vars`, the number
/// of groups evaluated and the right side's solutions summed over them.
pub(super) fn run(
    ctx: &Ctx,
    spec: &LateralSpec,
    l: &Table,
    vars: &[VarId],
) -> Result<(Table, usize, usize)> {
    let cols: Vec<Option<usize>> = spec.keys.iter().map(|k| l.col_of(*k)).collect();
    let mut groups: FxHashMap<Vec<Id>, Vec<usize>> = FxHashMap::default();
    let mut order: Vec<Vec<Id>> = Vec::new();
    let calls_extensions = ctx
        .extensions
        .as_ref()
        .is_some_and(|r| r.references(&spec.pattern));
    for i in 0..l.len() {
        let mut key: Vec<Id> = cols
            .iter()
            .map(|c| c.map_or(Id::UNDEF, |c| l.cols[c][i]))
            .collect();
        // Callback observations belong to each solution, including duplicate rows.
        // The private suffix is not substituted into any SPARQL variable.
        if calls_extensions {
            key.push(Id(i as u64));
        }
        groups
            .entry(key)
            .or_insert_with_key(|k| {
                order.push(k.clone());
                Vec::new()
            })
            .push(i);
    }
    let mut out = Table::new(vars.to_vec());
    let mut solutions = 0;
    for key in &order {
        ctx.check()?;
        let mut p = Planner::new(ctx);
        p.subst.extend(spec.outer.iter().copied());
        p.scoped.extend(spec.outer_scoped.iter().copied());
        if calls_extensions {
            p.property_inputs.extend(spec.keys.iter().copied());
            if !spec.unscoped {
                p.scoped.extend(spec.keys.iter().copied());
            }
        }
        for (k, id) in spec.keys.iter().zip(key) {
            if !id.is_undef() {
                p.subst.insert(*k, *id);
                if !spec.unscoped {
                    p.scoped.insert(*k);
                }
            }
        }
        let graph = match &spec.graph {
            ActiveGraph::Var(v) => match p.subst.get(v) {
                Some(id) => ActiveGraph::Named(*id),
                None => spec.graph.clone(),
            },
            g => g.clone(),
        };
        let node = p.plan(&spec.pattern, &graph, Vec::new())?;
        let (r, _) = execute(ctx, &node)?;
        solutions += r.len();
        let rows = l.take_rows(&groups[key]);
        let joined = match &spec.optional {
            Some(expr) => {
                let expr = expr.as_ref().map(|e| p.compile(e, &graph));
                let mut note = None;
                super::exec::left_join(ctx, &rows, &r, expr.as_ref(), &mut note)?
            }
            None if r.is_empty() => continue,
            None => join_tables(ctx, &rows, &r, &[], false)?,
        };
        ctx.check_output(out.len() + joined.len(), vars.len())?;
        out.append(joined.project(vars));
    }
    Ok((out, order.len(), solutions))
}
