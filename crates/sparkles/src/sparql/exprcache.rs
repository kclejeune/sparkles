//! Pure expressions evaluated once per distinct input value.
//!
//! An expression that reads one variable and is pure within a query has one result per
//! term of that variable, so it is evaluated once per distinct id of the variable's
//! column, and every row reads its result from there. FILTER conjuncts, BIND, ORDER BY
//! keys and aggregate arguments use it (GROUP BY on an expression is a BIND first).
//!
//! * Pure within a query: no RAND, UUID, STRUUID, BNODE (a fresh node per call, or one
//!   per solution) or EXISTS (correlated with the whole row). NOW and the base IRI are
//!   fixed for a query, and the rest of the context (snapshot, dataset, decoded values,
//!   the geometry memo of `geof:` functions) is too.
//! * Ids identify terms: an id is decoded the same way on every row (inline ids are
//!   canonical numerals, vocabulary, delta and query-local ids each name one stored key,
//!   with its datatype, language and direction), so the result of a distinct id — an
//!   error included — is the result of every row holding it. Errors keep their meaning
//!   for the operator: a failed FILTER test, an unbound BIND, an error ORDER BY key.
//!
//! Admission bounds the cost when values hardly repeat: rows in runs of equal ids are
//! counted exactly, and other repeats are estimated from a sample (Chao1). Below half
//! the rows in expected repeats, the expression runs row by row as before; after the
//! exact distinct count, more than three quarters distinct does too. The distinct values
//! and each row's index into them are the cache: at most one entry per input row, charged
//! to the query's memory budget while they are built.

use super::ctx::Ctx;
use super::expr::{Expr, Func, needs_values};
use super::table::{Table, VarId};
use crate::error::Result;
use crate::id::{Id, Tag};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::Function;

/// Smallest input that is worth the distinct pass.
pub const MIN_ROWS: usize = 1024;
/// Rows sampled to estimate how many distinct values a column holds.
const SAMPLE: usize = 4096;
/// Most distinct values (estimated) that are collected by hashing instead of sorting a
/// copy of the column.
const HASH_DISTINCT: usize = 1 << 14;

/// What one operator's expressions did, for EXPLAIN.
#[derive(Default, Debug)]
pub struct Report(Vec<Entry>);

#[derive(Debug)]
struct Entry {
    expr: String,
    rows: usize,
    /// distinct values evaluated, or why the expression ran row by row
    outcome: std::result::Result<usize, String>,
}

impl Report {
    pub fn extend(&mut self, other: Report) {
        self.0.extend(other.0);
    }

    /// `[expr cache: …]` for the operator's description.
    pub fn note(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|e| match &e.outcome {
                Ok(d) => format!("{} once per {d} distinct of {} rows", e.expr, e.rows),
                Err(why) => format!("{} row by row: {why}", e.expr),
            })
            .collect();
        Some(format!("[expr cache: {}]", parts.join("; ")))
    }

    /// Reuse counters: rows answered from an earlier evaluation (`exprCacheHits`),
    /// evaluations (`exprCacheMisses`) and expressions left to row-by-row evaluation.
    pub fn counters(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        if self.0.is_empty() {
            return None;
        }
        let (mut hits, mut misses, mut skipped) = (0u64, 0u64, 0u64);
        for e in &self.0 {
            match e.outcome {
                Ok(d) => {
                    hits += (e.rows - d) as u64;
                    misses += d as u64;
                }
                Err(_) => skipped += 1,
            }
        }
        let mut m = serde_json::Map::new();
        m.insert("exprCacheHits".into(), hits.into());
        m.insert("exprCacheMisses".into(), misses.into());
        m.insert("exprCacheSkipped".into(), skipped.into());
        Some(m)
    }

    fn push(
        &mut self,
        ctx: &Ctx,
        exprs: &[&Expr],
        rows: usize,
        outcome: std::result::Result<usize, String>,
    ) {
        let expr = exprs
            .iter()
            .map(|e| e.display(ctx))
            .collect::<Vec<_>>()
            .join(" && ");
        self.0.push(Entry {
            expr,
            rows,
            outcome,
        });
    }
}

/// Why `e` may differ between two evaluations on the same input, if it may.
pub fn impurity(e: &Expr) -> Option<&'static str> {
    match e {
        Expr::Exists(_) => Some("EXISTS"),
        Expr::Call(Func::Builtin(f), args) => match f {
            Function::Rand => Some("RAND"),
            Function::Uuid => Some("UUID"),
            Function::StrUuid => Some("STRUUID"),
            // a fresh blank node per call, or one per solution (all of its columns)
            Function::BNode => Some("BNODE"),
            _ => args.iter().find_map(impurity),
        },
        Expr::Call(_, l) | Expr::Coalesce(l) => l.iter().find_map(impurity),
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => impurity(a).or_else(|| impurity(b)),
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => impurity(a),
        Expr::In(a, l) => impurity(a).or_else(|| l.iter().find_map(impurity)),
        Expr::If(a, b, c) => impurity(a).or_else(|| impurity(b)).or_else(|| impurity(c)),
        Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) => None,
    }
}

/// The input of expressions that can be evaluated per value: `Ok(Some(v))` for one
/// variable, `Ok(None)` for none; otherwise why not.
fn input(exprs: &[&Expr]) -> std::result::Result<Option<VarId>, String> {
    if let Some(why) = exprs.iter().find_map(|e| impurity(e)) {
        return Err(format!("{why} is evaluated per row"));
    }
    let mut vars = Vec::new();
    for e in exprs {
        e.vars(&mut vars);
    }
    vars.sort_unstable();
    vars.dedup();
    match vars.as_slice() {
        [] => Ok(None),
        [v] => Ok(Some(*v)),
        vs => Err(format!("reads {} variables", vs.len())),
    }
}

/// Results per distinct input value and each row's index into them.
pub struct PerValue<T> {
    pub vals: Vec<T>,
    /// `None`: one value for every row
    slot: Option<Vec<u32>>,
}

impl<T> PerValue<T> {
    #[inline]
    pub fn get(&self, row: usize) -> &T {
        match &self.slot {
            Some(s) => &self.vals[s[row] as usize],
            None => &self.vals[0],
        }
    }

    /// The index into `vals` of a row's result.
    #[inline]
    pub fn index(&self, row: usize) -> usize {
        self.slot.as_ref().map_or(0, |s| s[row] as usize)
    }

    /// The per-row results.
    pub fn rows(&self, n: usize) -> Vec<T>
    where
        T: Clone + Send + Sync,
    {
        match &self.slot {
            Some(s) => s
                .par_iter()
                .map(|&j| self.vals[j as usize].clone())
                .collect(),
            None => vec![self.vals[0].clone(); n],
        }
    }
}

/// Per-row results, or results per distinct value.
pub enum Column<T> {
    Rows(Vec<T>),
    Values(PerValue<T>),
}

impl<T> Column<T> {
    #[inline]
    pub fn get(&self, row: usize) -> &T {
        match self {
            Column::Rows(v) => &v[row],
            Column::Values(p) => p.get(row),
        }
    }
}

/// The distinct inputs of a column: a one-column table of sorted distinct ids (one row
/// for an expression without an input column) and each row's index into it.
struct Distinct {
    values: Table,
    slot: Option<Vec<u32>>,
}

/// Evaluate `exprs` (which must all be admissible together) once per distinct value of
/// their input: `eval` gets the table of distinct values and returns one result per
/// value. `None` when the expressions are not admitted (the caller evaluates the rows);
/// the outcome is recorded in `report` when the input was large enough to try.
///
/// `always` skips the reuse estimate (key tests over sorted distinct ids beat row-by-row
/// tests even when every value differs).
pub fn per_value<T: Send>(
    ctx: &Ctx,
    t: &Table,
    exprs: &[&Expr],
    always: bool,
    report: &mut Report,
    eval: impl FnOnce(&Table) -> Result<Vec<T>>,
) -> Result<Option<PerValue<T>>> {
    let Some(d) = distinct(ctx, t, exprs, always, report)? else {
        return Ok(None);
    };
    let vals = eval(&d.values)?;
    debug_assert_eq!(vals.len(), d.values.len());
    ctx.check()?;
    Ok(Some(PerValue { vals, slot: d.slot }))
}

/// Whether the expressions are worth evaluating per value at all (pure, one input,
/// not trivial): `Ok(v)` with the input variable, else the reason.
pub fn eligible(exprs: &[&Expr]) -> std::result::Result<Option<VarId>, String> {
    let v = input(exprs)?;
    if !exprs.iter().any(|e| needs_values(e)) {
        return Err("reads no values".into());
    }
    if exprs.iter().all(|e| matches!(e, Expr::Var(_))) && v.is_some() {
        // a column copy is cheaper than the distinct pass, except where the value is
        // decoded (ORDER BY): the caller asks for those separately
        return Err("copies a column".into());
    }
    Ok(v)
}

fn distinct(
    ctx: &Ctx,
    t: &Table,
    exprs: &[&Expr],
    always: bool,
    report: &mut Report,
) -> Result<Option<Distinct>> {
    let n = t.len();
    if !ctx.opt.expr_cache || n < MIN_ROWS {
        return Ok(None);
    }
    let v = match input(exprs) {
        Ok(v) => v,
        Err(why) => {
            tracing::debug!("expression cache: {why}");
            return Ok(None);
        }
    };
    let Some(c) = v.and_then(|v| t.col_of(v)) else {
        // no input column: the same result on every row
        let values = match v {
            Some(v) => {
                let mut one = Table::new(vec![v]);
                one.push_row(&[Id::UNDEF]);
                one
            }
            None => Table::unit(),
        };
        report.push(ctx, exprs, n, Ok(1));
        return Ok(Some(Distinct { values, slot: None }));
    };
    let v = t.vars[c];
    let col = &t.cols[c];
    let runs = t.sorted.first() == Some(&v);
    let reject = |report: &mut Report, why: String| {
        tracing::debug!("expression cache: {why}");
        report.push(ctx, exprs, n, Err(why));
        Ok(None)
    };
    let est = if runs { 0 } else { estimate_distinct(col) };
    if !always && est > n / 2 {
        return reject(report, format!("about {est} distinct of {n} rows"));
    }
    // the slots, and a sorted copy of the column while it is deduplicated
    let Ok(_held) = ctx.charge((n * 12) as u64) else {
        return reject(report, "memory budget".into());
    };
    // few distinct values: collected by hashing, and each row looks up its slot
    let hashed = !runs && est <= HASH_DISTINCT;
    let mut uniq = if hashed {
        let set = col
            .par_chunks(1 << 16)
            .map(|c| c.iter().copied().collect::<FxHashSet<Id>>())
            .reduce(FxHashSet::default, |mut a, b| {
                a.extend(b);
                a
            });
        let mut u: Vec<Id> = set.into_iter().collect();
        u.sort_unstable();
        u
    } else {
        let mut u = col.clone();
        if !runs {
            u.par_sort_unstable();
        }
        u.dedup();
        u
    };
    uniq.shrink_to_fit();
    if !always && uniq.len() > n / 4 * 3 {
        return reject(report, format!("{} distinct of {n} rows", uniq.len()));
    }
    ctx.check()?;
    let slot: Vec<u32> = if hashed {
        let at: FxHashMap<Id, u32> = uniq
            .iter()
            .enumerate()
            .map(|(j, id)| (*id, j as u32))
            .collect();
        col.par_iter().map(|id| at[id]).collect()
    } else if runs {
        // `uniq` lists the runs in column order
        let mut j = 0u32;
        col.iter()
            .enumerate()
            .map(|(i, id)| {
                if i > 0 && *id != col[i - 1] {
                    j += 1;
                }
                j
            })
            .collect()
    } else {
        col.par_iter()
            .map(|id| uniq.binary_search(id).unwrap() as u32)
            .collect()
    };
    report.push(ctx, exprs, n, Ok(uniq.len()));
    let mut values = Table::new(vec![v]);
    values.len = uniq.len();
    values.cols[0] = uniq;
    values.sorted = vec![v];
    Ok(Some(Distinct {
        values,
        slot: Some(slot),
    }))
}

/// Estimated number of distinct ids in `col`: the runs of equal ids bound it exactly,
/// and a sample of one row per stratum estimates repeats anywhere (bias-corrected Chao1
/// from the values seen once and twice; a sample without any repeat is no evidence of
/// repeats).
fn estimate_distinct(col: &[Id]) -> usize {
    let n = col.len();
    let runs = 1 + col.windows(2).filter(|w| w[0] != w[1]).count();
    if runs <= n / 2 || n <= SAMPLE {
        if n <= SAMPLE {
            let mut seen: Vec<Id> = col.to_vec();
            seen.sort_unstable();
            seen.dedup();
            return seen.len();
        }
        return runs;
    }
    let mut counts: FxHashMap<Id, u32> = FxHashMap::default();
    counts.reserve(SAMPLE);
    for k in 0..SAMPLE {
        let (lo, hi) = (k * n / SAMPLE, (k + 1) * n / SAMPLE);
        let r = (k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
        let i = lo + (r % (hi - lo) as u64) as usize;
        *counts.entry(col[i]).or_default() += 1;
    }
    let d = counts.len();
    let f1 = counts.values().filter(|&&c| c == 1).count();
    let f2 = counts.values().filter(|&&c| c == 2).count();
    let est = if f1 == d {
        // every sampled value seen once: no evidence of repeats
        n
    } else {
        d + f1 * f1.saturating_sub(1) / (2 * (f2 + 1))
    };
    est.min(runs)
}

/// The FILTER conjuncts that run per distinct value: pure conjuncts over one variable,
/// grouped by it. Returns the mask of those groups and the conjuncts left for
/// row-by-row evaluation (on the rows that pass), or `None` when no group ran per value.
pub fn filter(
    ctx: &Ctx,
    t: &Table,
    exprs: &[Expr],
    report: &mut Report,
) -> Result<Option<(Vec<bool>, Vec<Expr>)>> {
    if !ctx.opt.expr_cache || t.len() < MIN_ROWS {
        return Ok(None);
    }
    let mut groups: Vec<(Option<VarId>, Vec<&Expr>)> = Vec::new();
    let mut rest: Vec<&Expr> = Vec::new();
    for e in exprs {
        match eligible(&[e]) {
            Ok(v) => match groups.iter_mut().find(|(g, _)| *g == v) {
                Some((_, es)) => es.push(e),
                None => groups.push((v, vec![e])),
            },
            Err(why) => {
                tracing::debug!("expression cache: {why}");
                rest.push(e);
            }
        }
    }
    let mut keep: Option<Vec<bool>> = None;
    for (v, es) in groups {
        let owned: Vec<Expr> = es.iter().map(|e| (*e).clone()).collect();
        let kf = v.and_then(|v| super::keyfilter::KeyFilter::new(&owned, v));
        let hit = per_value(ctx, t, &es, kf.is_some(), report, |values| match (&kf, v) {
            (Some(kf), Some(v)) => key_filter_mask(ctx, kf, &values.cols[0], v, &owned),
            _ => super::exec::filter_mask(ctx, values, &owned),
        })?;
        let Some(hit) = hit else {
            rest.extend(es);
            continue;
        };
        keep = Some(match keep {
            None => hit.rows(t.len()),
            Some(mut k) => {
                k.par_iter_mut()
                    .enumerate()
                    .for_each(|(i, k)| *k = *k && *hit.get(i));
                k
            }
        });
    }
    Ok(keep.map(|k| (k, rest.into_iter().cloned().collect())))
}

/// Outcome of a key filter for sorted distinct ids: base-vocabulary terms are tested on
/// their keys straight from the front-coded blocks (in parallel, each block visited once),
/// update-added terms on their delta keys, and everything else (inline literals, blank
/// nodes, unbound) by the general evaluator.
fn key_filter_mask(
    ctx: &Ctx,
    kf: &super::keyfilter::KeyFilter,
    uniq: &[Id],
    v: VarId,
    exprs: &[Expr],
) -> Result<Vec<bool>> {
    let vocab = &ctx.snap.generation.vocab;
    let mut hit = vec![false; uniq.len()];
    // raw ids sort by tag first, so the vocabulary ids are one contiguous range
    let lo = uniq.partition_point(|id| id.tag() < Tag::Vocab);
    let hi = lo + uniq[lo..].partition_point(|id| id.tag() == Tag::Vocab);
    ctx.check()?;
    hit[lo..hi]
        .par_chunks_mut(4096)
        .zip(uniq[lo..hi].par_chunks(4096))
        .for_each(|(h, ids)| {
            let payloads: Vec<u64> = ids.iter().map(|id| id.payload()).collect();
            let mut j = 0;
            vocab.get_sorted(&payloads, |p, k| {
                while payloads[j] != p {
                    j += 1;
                }
                h[j] = kf.test(k);
            });
        });
    let mut rest = Table::new(vec![v]);
    let mut rest_at = Vec::new();
    for (i, id) in uniq.iter().enumerate().filter(|(i, _)| *i < lo || *i >= hi) {
        match ctx.snap.key(*id) {
            Some(k) => hit[i] = kf.test(&k),
            None => {
                rest.push_row(&[*id]);
                rest_at.push(i);
            }
        }
    }
    if !rest_at.is_empty() {
        for (i, h) in rest_at
            .into_iter()
            .zip(super::exec::filter_mask(ctx, &rest, exprs)?)
        {
            hit[i] = h;
        }
    }
    Ok(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[i64]) -> Vec<Id> {
        v.iter().map(|&i| Id::from_i64(i).unwrap()).collect()
    }

    #[test]
    fn estimates_follow_repeats() {
        // small columns are counted exactly
        assert_eq!(estimate_distinct(&ids(&[1, 2, 2, 3])), 3);
        // runs bound the count
        let runs: Vec<i64> = (0..100_000).map(|i| i / 10).collect();
        assert!(estimate_distinct(&ids(&runs)) <= 10_000);
        // every value different: no evidence of repeats
        let all: Vec<i64> = (0..100_000).map(|i| (i * 7919) % 100_003).collect();
        assert_eq!(estimate_distinct(&ids(&all)), 100_000);
        // scattered repeats of a few hundred values
        let few: Vec<i64> = (0..100_000).map(|i| (i * 7919) % 300).collect();
        assert!(estimate_distinct(&ids(&few)) < 1000);
        // scattered repeats, five of each
        let five: Vec<i64> = (0..100_000).map(|i| (i * 7919) % 20_000).collect();
        let e = estimate_distinct(&ids(&five));
        assert!(e < 50_000, "{e}");
    }

    #[test]
    fn impure_calls_are_found_anywhere() {
        let call = |f: Function, args: Vec<Expr>| Expr::Call(Func::Builtin(f), args);
        let rand = call(Function::Rand, vec![]);
        assert_eq!(impurity(&rand), Some("RAND"));
        let nested = Expr::Arith(
            Box::new(Expr::Var(0)),
            Box::new(call(Function::Abs, vec![rand])),
            super::super::expr::ArithOp::Add,
        );
        assert_eq!(impurity(&nested), Some("RAND"));
        let bnode = call(Function::BNode, vec![Expr::Var(0)]);
        assert_eq!(impurity(&bnode), Some("BNODE"));
        // NOW is fixed for a query
        assert_eq!(impurity(&call(Function::Now, vec![])), None);
        assert_eq!(impurity(&call(Function::UCase, vec![Expr::Var(1)])), None);
    }
}
