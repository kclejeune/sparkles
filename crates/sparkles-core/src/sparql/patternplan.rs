//! Plans answered from the predicate patterns ([`crate::patterns`]): `PatternCount`, the
//! predicates of a set of subjects with the number of subjects having each, and
//! `PatternScan`, the pairs of `spk:hasPredicate`.
//!
//! The planner recognizes, at a GROUP BY on `?p` alone, a basic graph pattern with one
//! triple `?s ?p ?o` whose `?p` and `?o` appear in no other triple, and aggregates that
//! count distinct `?s`. The other triples are the input that binds `?s` (none: every
//! subject). The same shape with `?s spk:hasPredicate ?p` also takes `COUNT(?s)` and
//! `COUNT(*)`, which count the input's rows per predicate. The table records which
//! predicates a subject has, not how many triples, so `COUNT(*)` over `?s ?p ?o` is not
//! answered from it.
//!
//! A plan uses the patterns only when the answer is the same as the scan's: the active
//! graph is the default graph (the default table) or the union of the named graphs under
//! the union default graph (the named table, and only when the request reads every
//! graph), the snapshot hides no triples (C12b masks run as before), RDFS on read is off,
//! and the snapshot's delta touches few enough subjects to correct the tables per subject
//! ([`crate::patterns::Correction`]). Otherwise the query runs as before.

use super::ctx::Ctx;
use super::plan::{GraphFilter, PathEnd};
use super::table::{Table, VarId};
use crate::error::Result;
use crate::id::Id;
use crate::patterns::{Correction, PatternFile, Scope};
use spargebra::algebra::{AggregateExpression, AggregateFunction, Expression, GraphPattern};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern, Variable};
use std::sync::Arc;

/// `spk:hasPredicate`: `?s spk:hasPredicate ?p` holds when `?s` is the subject of a
/// triple with predicate `?p` in the active graph.
pub const HAS_PREDICATE: &str = "urn:x-sparkles:hasPredicate";
/// QLever's name for the same property.
pub const QL_HAS_PREDICATE: &str =
    "http://qlever.cs.uni-freiburg.de/builtin-functions/has-predicate";

/// Whether `p` names `spk:hasPredicate` or its QLever synonym.
pub fn is_has_predicate(p: &NamedNodePattern) -> bool {
    matches!(p, NamedNodePattern::NamedNode(n) if n.as_str() == HAS_PREDICATE || n.as_str() == QL_HAS_PREDICATE)
}

/// The patterns a plan reads, with the correction for the snapshot's delta.
#[derive(Clone)]
pub struct PatternUse {
    pub file: Arc<PatternFile>,
    pub scope: Scope,
    pub corr: Arc<Correction>,
}

impl PatternUse {
    fn note(&self) -> String {
        let mut s = format!("[predicate patterns, {} table", self.scope.name());
        if !self.corr.is_empty() {
            s.push_str(&format!(
                ", corrected for {} subjects of the delta",
                self.corr.touched.len()
            ));
        }
        s.push(']');
        s
    }
}

/// The patterns for a plan over the graphs `gf` selects, when they give the scan's
/// answer on this snapshot and view.
pub fn usable(ctx: &Ctx, gf: &GraphFilter) -> Option<PatternUse> {
    if !ctx.opt.predicate_patterns || ctx.rdfs.is_some() || ctx.snap.mask.is_some() {
        return None;
    }
    let scope = match gf {
        GraphFilter::Default => Scope::Default,
        // the union of the named graphs: only when the request reads every graph
        GraphFilter::Named if ctx.graphs.is_none() => Scope::Named,
        _ => return None,
    };
    let file = ctx.snap.generation.patterns.file.clone()?;
    let table = file.table(scope)?;
    let corr = ctx
        .snap
        .counts
        .pattern_correction(scope, &ctx.snap, |snap| {
            crate::patterns::correction(
                snap,
                table,
                scope,
                crate::patterns::touched_limit(snap),
                &|| ctx.check(),
            )
        })?;
    Some(PatternUse { file, scope, corr })
}

/// What a count of a [`PatternCountSpec`] counts per predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// distinct subjects
    Subjects,
    /// rows of the input whose subject has the predicate
    Rows,
}

/// `GROUP BY ?p` with counts over an input binding `?s` and `?s ?p ?o`.
#[derive(Clone)]
pub struct PatternCountSpec {
    pub uses: PatternUse,
    /// the input's subject variable (child 0), or `None` for every subject
    pub subject: Option<VarId>,
    pub pred: VarId,
    pub counts: Vec<(VarId, Measure)>,
    /// the query's `?s ?p ?o`, for the plan's `patterns`
    pub src: Option<std::sync::Arc<super::exec::PlanPattern>>,
}

/// `spk:hasPredicate` read from the patterns.
#[derive(Clone)]
pub struct PatternScanSpec {
    pub uses: PatternUse,
    pub s: PathEnd,
    pub p: PathEnd,
    /// the query's `?s spk:hasPredicate ?p`, for the plan's `patterns`
    pub src: Option<std::sync::Arc<super::exec::PlanPattern>>,
}

/// The shape of a GROUP BY that the patterns can answer.
pub(super) struct CountShape {
    /// the other triples of the pattern: the input
    pub rest: Vec<TriplePattern>,
    pub s: Variable,
    /// the object variable the rewrite drops, which must not be substituted
    pub o: Option<Variable>,
    pub counts: Vec<(Variable, Measure)>,
    /// the triple the patterns answer, for the plan's `patterns`
    pub triple: TriplePattern,
}

/// Recognize `GROUP BY ?p` over `inner` with `aggregates` as a pattern count.
pub(super) fn count_shape(
    inner: &GraphPattern,
    variables: &[Variable],
    aggregates: &[(Variable, AggregateExpression)],
) -> Option<CountShape> {
    let [p] = variables else { return None };
    let GraphPattern::Bgp { patterns } = inner else {
        return None;
    };
    let names = |t: &TermPattern| -> Option<String> {
        match t {
            TermPattern::Variable(v) => Some(format!("?{}", v.as_str())),
            TermPattern::BlankNode(b) => Some(format!("_:{}", b.as_str())),
            _ => None,
        }
    };
    // a variable or blank node anywhere in a triple, inside triple terms too
    fn in_term(t: &TermPattern, name: &str) -> bool {
        match t {
            TermPattern::Variable(v) => format!("?{}", v.as_str()) == name,
            TermPattern::BlankNode(b) => format!("_:{}", b.as_str()) == name,
            TermPattern::Triple(tp) => in_triple(tp, name),
            _ => false,
        }
    }
    fn in_triple(t: &TriplePattern, name: &str) -> bool {
        in_term(&t.subject, name)
            || in_term(&t.object, name)
            || matches!(&t.predicate, NamedNodePattern::Variable(v) if format!("?{}", v.as_str()) == name)
    }
    let mentions = |t: &TriplePattern, name: &str| in_triple(t, name);
    let pname = format!("?{}", p.as_str());
    let mut found: Option<(usize, Variable, bool)> = None;
    for (i, t) in patterns.iter().enumerate() {
        let TermPattern::Variable(s) = &t.subject else {
            continue;
        };
        let sname = format!("?{}", s.as_str());
        let candidate = match &t.predicate {
            // ?s ?p ?o with ?o fresh, and ?s not ?p
            NamedNodePattern::Variable(v) if v == p && *s != *p => match names(&t.object) {
                Some(o) if o != sname && o != pname => Some((o, false)),
                _ => None,
            },
            pred if is_has_predicate(pred) => match &t.object {
                TermPattern::Variable(v) if v == p && *s != *p => Some((String::new(), true)),
                _ => None,
            },
            _ => None,
        };
        let Some((o, has)) = candidate else { continue };
        if found.is_some() {
            return None;
        }
        // ?p and ?o occur in no other triple
        let elsewhere = patterns
            .iter()
            .enumerate()
            .any(|(j, u)| j != i && (mentions(u, &pname) || (!o.is_empty() && mentions(u, &o))));
        if elsewhere {
            return None;
        }
        found = Some((i, s.clone(), has));
    }
    let (i, s, has) = found?;
    let o = match &patterns[i].object {
        TermPattern::Variable(v) if !has => Some(v.clone()),
        _ => None,
    };
    let mut counts = Vec::new();
    for (out, agg) in aggregates {
        let m = match agg {
            AggregateExpression::FunctionCall {
                name: AggregateFunction::Count,
                expr: Expression::Variable(v),
                distinct,
            } if *v == s => match (distinct, has) {
                (true, _) => Measure::Subjects,
                (false, true) => Measure::Rows,
                (false, false) => return None,
            },
            AggregateExpression::CountSolutions { distinct: false } if has => Measure::Rows,
            _ => return None,
        };
        counts.push((out.clone(), m));
    }
    let rest: Vec<TriplePattern> = patterns
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i)
        .map(|(_, t)| t.clone())
        .collect();
    // an input that does not name ?s would be a cross product
    if !rest.is_empty() {
        let sname = format!("?{}", s.as_str());
        if !rest.iter().any(|t| mentions(t, &sname)) {
            return None;
        }
    }
    Some(CountShape {
        rest,
        s,
        o,
        counts,
        triple: patterns[i].clone(),
    })
}

/// (predicate, count), in predicate order.
type PerPredicate = Vec<(u64, u64)>;

fn count_id(n: u64) -> Id {
    Id::from_i64(n.min(i64::MAX as u64) as i64).unwrap_or(Id::UNDEF)
}

/// Run a [`PatternCountSpec`] over `input` (its child's table, when it has one). The
/// table's columns follow `vars`: the predicate, then the counts. Also returns the
/// EXPLAIN note.
pub(super) fn run_count(
    ctx: &Ctx,
    spec: &PatternCountSpec,
    input: Option<&Table>,
    vars: &[VarId],
) -> Result<(Table, String)> {
    let u = &spec.uses;
    let table = u
        .file
        .table(u.scope)
        .ok_or_else(|| crate::error::Error::Corrupt("pattern table missing".into()))?;
    let check = || ctx.check();
    // per predicate: (distinct subjects, rows)
    let (subjects, rows): (PerPredicate, Option<PerPredicate>) = match (input, spec.subject) {
        (Some(t), Some(s)) => {
            let col = t.col_of(s).map(|c| &t.cols[c]);
            let mut ids: Vec<u64> = col
                .map(|c| {
                    c.iter()
                        .filter(|id| **id != Id::UNDEF)
                        .map(|id| id.0)
                        .collect()
                })
                .unwrap_or_default();
            ids.sort_unstable();
            let mut weighted: Vec<(u64, u64)> = Vec::new();
            for id in ids {
                match weighted.last_mut() {
                    Some((x, n)) if *x == id => *n += 1,
                    _ => weighted.push((id, 1)),
                }
            }
            let need_rows = spec.counts.iter().any(|c| c.1 == Measure::Rows);
            let distinct: Vec<(u64, u64)> = weighted.iter().map(|&(s, _)| (s, 1)).collect();
            let subj = crate::patterns::count_subjects(table, &u.corr, &distinct, &check)?;
            let rows = if need_rows {
                Some(crate::patterns::count_subjects(
                    table, &u.corr, &weighted, &check,
                )?)
            } else {
                None
            };
            (subj, rows)
        }
        _ => {
            let all = crate::patterns::count_all(table, &u.corr)?;
            (all.clone(), Some(all))
        }
    };
    let mut out = Table::new(vars.to_vec());
    let rows_of: rustc_hash::FxHashMap<u64, u64> =
        rows.map(|r| r.into_iter().collect()).unwrap_or_default();
    let mut row = vec![Id::UNDEF; vars.len()];
    for (p, n) in &subjects {
        row[0] = Id(*p);
        for (i, (_, m)) in spec.counts.iter().enumerate() {
            row[1 + i] = count_id(match m {
                Measure::Subjects => *n,
                Measure::Rows => rows_of.get(p).copied().unwrap_or(0),
            });
        }
        out.push_row(&row);
    }
    Ok((out, u.note()))
}

/// Run a [`PatternScanSpec`]: its variables' columns follow `vars`. With `limit`, it
/// stops at that many rows. Also returns the EXPLAIN note and whether it stopped early.
pub(super) fn run_scan(
    ctx: &Ctx,
    spec: &PatternScanSpec,
    vars: &[VarId],
    limit: Option<usize>,
) -> Result<(Table, String, bool)> {
    let u = &spec.uses;
    let table = u
        .file
        .table(u.scope)
        .ok_or_else(|| crate::error::Error::Corrupt("pattern table missing".into()))?;
    let mut out = Table::new(vars.to_vec());
    let sc = match spec.s {
        PathEnd::Var(v) => out.col_of(v),
        PathEnd::Const(_) => None,
    };
    let pc = match spec.p {
        PathEnd::Var(v) => out.col_of(v),
        PathEnd::Const(_) => None,
    };
    let want = limit.unwrap_or(usize::MAX);
    let width = out.width();
    let mut row = vec![Id::UNDEF; vars.len()];
    let mut stopped = false;
    // false once the limit is met
    let mut emit = |s: u64, p: u64, out: &mut Table| -> Result<bool> {
        if out.len() >= want {
            stopped = true;
            return Ok(false);
        }
        // the columns double as they fill: admit the next size's bytes before they
        // grow, and the rows there are against the row limits
        let n = out.len();
        if n >= 4096 && n.is_power_of_two() {
            ctx.check()?;
            ctx.check_output_rows(n)?;
            ctx.check_output_bytes(n.saturating_mul(2), width)?;
        }
        if let Some(c) = sc {
            row[c] = Id(s);
        }
        if let Some(c) = pc {
            row[c] = Id(p);
        }
        out.push_row(&row);
        Ok(true)
    };
    match (spec.s.clone(), spec.p.clone()) {
        (PathEnd::Const(s), p) => {
            let mut preds = Vec::new();
            crate::patterns::predicates_of(table, &u.corr, s.0, &mut preds)?;
            for q in preds {
                if let PathEnd::Const(c) = p
                    && c.0 != q
                {
                    continue;
                }
                if !emit(s.0, q, &mut out)? {
                    break;
                }
            }
        }
        (PathEnd::Var(_), p) => {
            let only = match p {
                PathEnd::Const(c) => Some(c.0),
                PathEnd::Var(_) => None,
            };
            crate::patterns::for_each_pair(table, &u.corr, only, &|| ctx.check(), |s, q| {
                emit(s, q, &mut out)
            })?;
        }
    }
    // the rows between the last power of two and the end, which a limited run returns
    // without the eager executor's output check
    ctx.check_output(out.len(), out.width())?;
    Ok((out, u.note(), stopped))
}
