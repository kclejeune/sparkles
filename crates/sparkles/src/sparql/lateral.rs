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
    let ctx = p.ctx;
    let keys: Vec<VarId> = mentioned(right)
        .iter()
        .map(|n| ctx.var(n))
        .filter(|v| left.vars.contains(v))
        .collect();
    // the right side planned without substitution, for its variables and estimates
    let r = p.plan(right, g, Vec::new())?;
    if keys.is_empty() {
        return Ok(super::plan::join(left, r, ctx));
    }
    let mut vars = left.vars.clone();
    let mut certain = left.certain.clone();
    for v in &r.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    for v in &r.certain {
        if !certain.contains(v) {
            certain.push(*v);
        }
    }
    let est = join_est(&left, &r, &keys).max(1.0);
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
        "per row on {}",
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
    for i in 0..l.len() {
        let key: Vec<Id> = cols
            .iter()
            .map(|c| c.map_or(Id::UNDEF, |c| l.cols[c][i]))
            .collect();
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
        for (k, id) in spec.keys.iter().zip(key) {
            if !id.is_undef() {
                p.subst.insert(*k, *id);
                p.scoped.insert(*k);
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
        if r.is_empty() {
            continue;
        }
        let rows = l.take_rows(&groups[key]);
        let joined = join_tables(ctx, &rows, &r, &[], false)?;
        ctx.check_output(out.len() + joined.len(), vars.len())?;
        out.append(joined.project(vars));
    }
    Ok((out, order.len(), solutions))
}
