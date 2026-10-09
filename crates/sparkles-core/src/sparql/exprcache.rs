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
use super::exec::PAR_MIN_LEN;
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
        Expr::Call(Func::Registered(_), _) => Some("application function"),
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
pub(super) fn input(exprs: &[&Expr]) -> std::result::Result<Option<VarId>, String> {
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
    _charge: Option<super::ctx::RetainedCharge>,
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
                .with_min_len(PAR_MIN_LEN)
                .map(|&j| self.vals[j as usize].clone())
                .collect(),
            None => vec![self.vals[0].clone(); n],
        }
    }
}

/// Per-row results, or results per distinct value.
pub enum Column<T> {
    Rows {
        vals: Vec<T>,
        _charge: Option<super::ctx::RetainedCharge>,
    },
    Values(PerValue<T>),
}

impl<T> Column<T> {
    #[inline]
    pub fn get(&self, row: usize) -> &T {
        match self {
            Column::Rows { vals: v, .. } => &v[row],
            Column::Values(p) => p.get(row),
        }
    }

    /// The same column with each stored result converted, keeping its charge.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Column<U> {
        match self {
            Column::Rows { vals, _charge } => Column::Rows {
                vals: vals.into_iter().map(f).collect(),
                _charge,
            },
            Column::Values(p) => Column::Values(PerValue {
                vals: p.vals.into_iter().map(f).collect(),
                slot: p.slot,
                _charge: p._charge,
            }),
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
    let Some(d) = distinct(ctx, t, exprs, always, report, false)? else {
        return Ok(None);
    };
    let vals = eval(&d.values)?;
    debug_assert_eq!(vals.len(), d.values.len());
    ctx.check()?;
    Ok(Some(PerValue {
        vals,
        slot: d.slot,
        _charge: None,
    }))
}

/// Fixed-size numeric ORDER keys can reuse input IDs without retaining decoded
/// strings or populating the eager value cache. Own the reservation with the
/// returned values/slots; it must survive until the sort has finished.
pub(super) fn cursor_numeric_values(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut Report,
    eval: impl FnOnce(&Table) -> Result<Vec<Option<super::value::Value>>>,
) -> Result<Option<PerValue<Option<super::value::Value>>>> {
    fn numeric(e: &Expr) -> bool {
        match e {
            Expr::Var(_) => true,
            Expr::Const(id) => matches!(id.tag(), Tag::Int | Tag::Decimal | Tag::Double),
            Expr::Lit(_, value) => matches!(
                value,
                super::value::Value::Integer(_)
                    | super::value::Value::Decimal(_)
                    | super::value::Value::Float(_)
                    | super::value::Value::Double(_)
            ),
            Expr::Arith(a, b, _) => numeric(a) && numeric(b),
            Expr::Neg(e) | Expr::Pos(e) => numeric(e),
            Expr::Call(
                Func::Builtin(Function::Abs | Function::Floor | Function::Ceil | Function::Round),
                args,
            ) => args.iter().all(numeric),
            _ => false,
        }
    }
    if !ctx.opt.expr_cache || t.len() < MIN_ROWS || !numeric(e) {
        return Ok(None);
    }
    let Ok(Some(v)) = input(&[e]) else {
        return Ok(None);
    };
    let Some(column) = t.col_of(v) else {
        return Ok(None);
    };
    if !t.cols[column]
        .iter()
        .all(|id| matches!(id.tag(), Tag::Int | Tag::Decimal | Tag::Double | Tag::Undef))
    {
        return Ok(None);
    }
    let bytes = (t.len() as u64).saturating_mul(256).saturating_add(4096);
    let Ok(mut charge) = ctx.retained_charge(bytes) else {
        return Ok(None);
    };
    let d = match cursor_integer_distinct(ctx, t, &[e], v, report)? {
        Some(d) => Some(d),
        None => cursor_key_distinct(ctx, t, &[e], v, false, report)?,
    };
    let Some(d) = d else {
        return Ok(None);
    };
    let vals = eval(&d.values)?;
    ctx.check()?;
    let retained = vals.capacity() as u64
        * std::mem::size_of::<Option<super::value::Value>>() as u64
        + d.slot.as_ref().map_or(0, |s| s.capacity() as u64 * 4)
        + 4096;
    if let Some(charge) = &mut charge {
        charge.resize(retained)?;
    }
    Ok(Some(PerValue {
        vals,
        slot: d.slot,
        _charge: charge,
    }))
}

/// Cursor BIND reuse keeps only IDs and slots, with a conservative reservation
/// covering the temporary distinct set/map, table, slots and expanded output.
/// The reservation lives until the output is expanded; no entries cross batches.
/// Unlike general value-key caching, this does not retain decoded heap values.
pub(super) fn cursor_column(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut Report,
    eval: impl FnOnce(&Table) -> Result<Vec<Id>>,
) -> Result<Option<Vec<Id>>> {
    let Ok(v) = eligible(&[e]) else {
        return Ok(None);
    };
    if !ctx.opt.expr_cache || t.len() < MIN_ROWS {
        return Ok(None);
    }
    let bytes = (t.len() as u64).saturating_mul(128).saturating_add(1024);
    let Ok(_held) = ctx.charge(bytes) else {
        // Optional reuse may decline without consuming the remaining allowance.
        return Ok(None);
    };
    let d = match v {
        Some(v) => match cursor_integer_distinct(ctx, t, &[e], v, report)? {
            Some(d) => Some(d),
            None => cursor_key_distinct(ctx, t, &[e], v, false, report)?,
        },
        None => distinct(ctx, t, &[e], false, report, true)?,
    };
    let Some(d) = d else {
        return Ok(None);
    };
    let vals = eval(&d.values)?;
    debug_assert_eq!(vals.len(), d.values.len());
    ctx.check()?;
    // A cursor batch is small enough that scheduling the expansion on Rayon
    // costs more than the ID copies. Keep the eager large-table path separate.
    let rows = match d.slot {
        Some(slot) => slot.iter().map(|&i| vals[i as usize]).collect(),
        None => vec![vals[0]; t.len()],
    };
    Ok(Some(rows))
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
    cursor_reserved: bool,
) -> Result<Option<Distinct>> {
    let n = t.len();
    if !ctx.opt.expr_cache || (ctx.is_cursor() && !cursor_reserved) || n < MIN_ROWS {
        return Ok(None);
    }
    let v = match input(exprs) {
        Ok(v) => v,
        Err(why) => {
            tracing::debug!(target: "sparkles::sparql::exprcache", "expression cache: {why}");
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
        tracing::debug!(target: "sparkles::sparql::exprcache", "expression cache: {why}");
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
        col.par_iter()
            .with_min_len(PAR_MIN_LEN)
            .map(|id| at[id])
            .collect()
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
            .with_min_len(PAR_MIN_LEN)
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
    if ctx.is_cursor() {
        return cursor_filter(ctx, t, exprs, report);
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
                tracing::debug!(target: "sparkles::sparql::exprcache", "expression cache: {why}");
                rest.push(e);
            }
        }
    }
    let mut keep: Option<Vec<bool>> = None;
    for (v, es) in groups {
        let owned: Vec<Expr> = es.iter().map(|e| (*e).clone()).collect();
        let kf = v.and_then(|v| super::keyfilter::KeyFilter::new_for(ctx, &owned, v));
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
                    .with_min_len(PAR_MIN_LEN)
                    .enumerate()
                    .for_each(|(i, k)| *k = *k && *hit.get(i));
                k
            }
        });
    }
    Ok(keep.map(|k| (k, rest.into_iter().cloned().collect())))
}

/// Batch-local FILTER reuse stores only IDs, slots and booleans. A conservative
/// reservation covers distinct construction and key-test scratch until expansion.
/// Mixed/impure conjuncts stay row-by-row; no decoded-value cache survives a batch.
#[inline(never)]
fn cursor_filter(
    ctx: &Ctx,
    t: &Table,
    exprs: &[Expr],
    report: &mut Report,
) -> Result<Option<(Vec<bool>, Vec<Expr>)>> {
    let Ok(_refs) = ctx.charge((exprs.len() as u64).saturating_mul(8)) else {
        return Ok(None);
    };
    let refs: Vec<&Expr> = exprs.iter().collect();
    let Ok(v) = eligible(&refs) else {
        return Ok(None);
    };
    let key_bytes =
        v.and_then(|v| super::keyfilter::KeyFilter::cursor_scratch_bytes(exprs, v, t.len()));
    let bytes = (t.len() as u64)
        .saturating_mul(128)
        .saturating_add(1024)
        .saturating_add(key_bytes.unwrap_or(0));
    let Ok(_held) = ctx.charge(bytes) else {
        return Ok(None);
    };
    let d = match (key_bytes, v) {
        (Some(_), Some(v)) => cursor_key_distinct(ctx, t, &refs, v, true, report)?,
        _ => distinct(ctx, t, &refs, false, report, true)?,
    };
    let Some(d) = d else {
        drop(_held);
        return if key_bytes.is_none() {
            cursor_numeric_filter(ctx, t, exprs).map(|keep| keep.map(|keep| (keep, Vec::new())))
        } else {
            Ok(None)
        };
    };
    let vals = match (key_bytes, v) {
        (Some(_), Some(v)) => {
            let kf = super::keyfilter::KeyFilter::new_for(ctx, exprs, v)
                .expect("cursor key-test admission matches compilation");
            key_filter_mask(ctx, &kf, &d.values.cols[0], v, exprs)?
        }
        _ => super::exec::filter_mask(ctx, &d.values, exprs)?,
    };
    ctx.check()?;
    Ok(Some((
        PerValue {
            vals,
            slot: d.slot,
            _charge: None,
        }
        .rows(t.len()),
        Vec::new(),
    )))
}

/// Batch-local numeric dictionary evaluation after ordinary distinct reuse was
/// declined. Shared eager row evaluation stays unchanged; optional memory failure
/// returns to scalar filtering and no decoded dictionary crosses a batch.
#[inline(never)]
fn cursor_numeric_filter(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Result<Option<Vec<bool>>> {
    fn numeric(e: &Expr) -> bool {
        match e {
            Expr::Var(_) => true,
            Expr::Const(id) => matches!(
                id.tag(),
                crate::id::Tag::Int | crate::id::Tag::Decimal | crate::id::Tag::Double
            ),
            Expr::Lit(_, value) => matches!(
                value,
                super::value::Value::Integer(_)
                    | super::value::Value::Decimal(_)
                    | super::value::Value::Float(_)
                    | super::value::Value::Double(_)
            ),
            Expr::Arith(a, b, _)
            | Expr::Cmp(a, b, _)
            | Expr::Eq(a, b)
            | Expr::And(a, b)
            | Expr::Or(a, b) => numeric(a) && numeric(b),
            Expr::Neg(e) | Expr::Pos(e) | Expr::Not(e) => numeric(e),
            Expr::Call(
                super::expr::Func::Builtin(
                    spargebra::algebra::Function::Abs
                    | spargebra::algebra::Function::Floor
                    | spargebra::algebra::Function::Ceil
                    | spargebra::algebra::Function::Round,
                ),
                args,
            ) => args.iter().all(numeric),
            _ => false,
        }
    }
    if !ctx.is_cursor() || !ctx.opt.expr_cache || t.len() < 4096 || !exprs.iter().all(numeric) {
        return Ok(None);
    }
    let bytes = t.len() as u64 * (std::mem::size_of::<Option<super::value::Value>>() as u64 + 2)
        + ctx.nvars() as u64 * 16
        + exprs.len() as u64 * 8
        + t.width() as u64 * std::mem::size_of::<Option<Vec<Option<super::value::Value>>>>() as u64
        + 4096;
    let Ok(_charge) = ctx.retained_charge(bytes) else {
        return Ok(None);
    };
    let Ok(Some(var)) = eligible(&exprs.iter().collect::<Vec<_>>()) else {
        return Ok(None);
    };
    let Some(column) = t.col_of(var) else {
        return Ok(None);
    };
    let values = match cursor_dictionary_values(ctx, t, &Expr::Var(var)) {
        Ok(Some(values)) => values,
        Ok(None) => return Ok(None),
        Err(crate::Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if values.vals.iter().flatten().any(|v| {
        !matches!(
            v,
            super::value::Value::Integer(_)
                | super::value::Value::Decimal(_)
                | super::value::Value::Float(_)
                | super::value::Value::Double(_)
                | super::value::Value::Bool(_)
        )
    }) {
        return Ok(None);
    }
    let mut decoded = Vec::with_capacity(t.len());
    for row in 0..t.len() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        decoded.push(values.get(row).clone());
    }
    let mut cols = vec![None; t.width()];
    cols[column] = Some(decoded);
    let map = t.var_map(ctx.nvars());
    let keep = super::exec::map_rows(
        ctx,
        t.len(),
        t.len() > super::exec::PAR_THRESHOLD && exprs.iter().all(Expr::parallel),
        |i| {
            let row = super::expr::Row {
                table: t,
                i,
                map: &map,
                dec: Some(&cols),
            };
            exprs
                .iter()
                .all(|e| super::expr::ebv(e, &row, ctx).unwrap_or(false))
        },
    )?;
    ctx.check()?;
    Ok(Some(keep))
}

/// The caller holds the cursor reservation through expansion. Keep this batch
/// algorithm separate from eager distinct admission and its reuse estimator.
fn cursor_integer_distinct(
    ctx: &Ctx,
    t: &Table,
    exprs: &[&Expr],
    v: VarId,
    report: &mut Report,
) -> Result<Option<Distinct>> {
    let Some(column) = t.col_of(v) else {
        return Ok(None);
    };
    let mut lo = u64::MAX;
    let mut hi = 0;
    for (row, id) in t.cols[column].iter().enumerate() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        if id.tag() != Tag::Int {
            return Ok(None);
        }
        lo = lo.min(id.payload());
        hi = hi.max(id.payload());
    }
    let span = hi.saturating_sub(lo).saturating_add(1);
    if span > (t.len() / 2).min(1 << 16) as u64 {
        return Ok(None);
    }
    let mut positions = vec![u32::MAX; span as usize];
    for (row, id) in t.cols[column].iter().enumerate() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        positions[(id.payload() - lo) as usize] = 0;
    }
    let mut values = Table::new(vec![v]);
    for (offset, slot) in positions.iter_mut().enumerate() {
        if offset.is_multiple_of(1024) {
            ctx.check()?;
        }
        if *slot != u32::MAX {
            *slot = values.len as u32;
            values.push_row(&[Id::new(Tag::Int, lo + offset as u64)]);
        }
    }
    // Evaluate only observed IDs, never hypothetical holes in the integer range.
    // Direct slot lookup avoids hashing or sorting thousands of repeated numerals.
    let mut slot = Vec::with_capacity(t.len());
    for (row, id) in t.cols[column].iter().enumerate() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        slot.push(positions[(id.payload() - lo) as usize]);
    }
    values.sorted = vec![v];
    report.push(ctx, exprs, t.len(), Ok(values.len));
    Ok(Some(Distinct {
        values,
        slot: Some(slot),
    }))
}

fn cursor_key_distinct(
    ctx: &Ctx,
    t: &Table,
    exprs: &[&Expr],
    v: VarId,
    always: bool,
    report: &mut Report,
) -> Result<Option<Distinct>> {
    let Some(c) = t.col_of(v) else {
        return distinct(ctx, t, exprs, always, report, true);
    };
    if t.sorted.first() == Some(&v) {
        return distinct(ctx, t, exprs, always, report, true);
    }
    let n = t.len();
    if n > u32::MAX as usize {
        report.push(ctx, exprs, n, Err("too many row positions".into()));
        return Ok(None);
    }
    // Key tests admit unique inputs too: sort IDs with row positions once,
    // avoiding a reuse estimate and two temporary hash tables.
    let mut pairs: Vec<(Id, u32)> = t.cols[c]
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as u32))
        .collect();
    if n < 16_384 {
        pairs.sort_unstable_by_key(|p| p.0);
    } else {
        pairs.par_sort_unstable_by_key(|p| p.0);
    }
    let mut uniq = Vec::with_capacity(n);
    let mut slot = vec![0; n];
    for (at, (id, row)) in pairs.into_iter().enumerate() {
        if at.is_multiple_of(1024) {
            ctx.check()?;
        }
        if uniq.last() != Some(&id) {
            uniq.push(id);
        }
        slot[row as usize] = (uniq.len() - 1) as u32;
    }
    ctx.check()?;
    if !always && uniq.len() > n / 4 * 3 {
        report.push(
            ctx,
            exprs,
            n,
            Err(format!("{} distinct of {n} rows", uniq.len())),
        );
        return Ok(None);
    }
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

/// Outcome of FILTER conjuncts over `v` (and no other variable) for each of the sorted
/// distinct ids `uniq`, an error failing the filter; also whether they were tested on
/// vocabulary keys.
pub(super) fn filter_values(
    ctx: &Ctx,
    uniq: &[Id],
    v: VarId,
    exprs: &[Expr],
) -> Result<(Vec<bool>, bool)> {
    if let Some(kf) = super::keyfilter::KeyFilter::new_for(ctx, exprs, v) {
        return Ok((key_filter_mask(ctx, &kf, uniq, v, exprs)?, true));
    }
    let mut values = Table::new(vec![v]);
    values.len = uniq.len();
    values.cols[0] = uniq.to_vec();
    values.sorted = vec![v];
    Ok((super::exec::filter_mask(ctx, &values, exprs)?, false))
}

/// Outcome of a key filter for sorted distinct ids: base-vocabulary terms are tested on
/// their keys straight from the front-coded blocks (in parallel, each block visited once),
/// update-added terms on their delta keys, and everything else (inline literals, blank
/// nodes, unbound) by the general evaluator.
pub(super) fn key_filter_mask(
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
    // the vocabulary ids whose keys are read: with a fixed start, only those of the keys
    // with that start (the vocabulary is sorted by key), the others fail unread
    let spans: Vec<(usize, usize)> = match kf.key_prefixes() {
        Some(prefixes) if ctx.opt.filter_id_ranges => {
            let mut spans: Vec<(usize, usize)> = prefixes
                .iter()
                .map(|p| {
                    let (a, b) = vocab.prefix_range(p);
                    let at = |x: u64| lo + uniq[lo..hi].partition_point(|id| id.payload() < x);
                    (at(a), at(b))
                })
                .filter(|(s, e)| s < e)
                .collect();
            spans.sort_unstable();
            spans
        }
        _ => vec![(lo, hi)],
    };
    // a copy of the filter per task: threads sharing a regular expression contend for
    // its match caches
    let mut read: Vec<(&mut [bool], &[Id])> = Vec::with_capacity(spans.len());
    let mut rest_hit = &mut hit[..];
    let mut at = 0;
    for &(s, e) in &spans {
        let (_, tail) = rest_hit.split_at_mut(s - at);
        let (span, tail) = tail.split_at_mut(e - s);
        read.push((span, &uniq[s..e]));
        rest_hit = tail;
        at = e;
    }
    if ctx.is_cursor() && uniq.len() <= 4096 {
        // Ordinary cursor batches do not amortize task scheduling or a fresh
        // regex worker cache. Search with the already admitted query cache.
        for (h, ids) in read {
            let _payload_charge = ctx.charge(ids.len() as u64 * 8 + 64)?;
            let payloads: Vec<u64> = ids.iter().map(|id| id.payload()).collect();
            let scratch = ctx.charge(0)?;
            let mut reserved = 0;
            let mut j = 0;
            vocab.get_sorted_checked(
                &payloads,
                |need| {
                    let bytes = (need as u64).saturating_mul(2);
                    scratch.add(bytes.saturating_sub(reserved))?;
                    reserved = bytes;
                    ctx.check()
                },
                |p, key| {
                    while payloads[j] != p {
                        j += 1;
                    }
                    if j.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    h[j] = kf.test(key);
                    Ok::<_, crate::error::Error>(())
                },
            )?;
        }
    } else {
        read.into_par_iter()
            .flat_map(|(h, ids)| h.par_chunks_mut(4096).zip(ids.par_chunks(4096)))
            .try_for_each_init(
                || kf.clone(),
                |kf, (h, ids)| {
                    let payloads: Vec<u64> = ids.iter().map(|id| id.payload()).collect();
                    let mut j = 0;
                    let mut test = |p, k: &[u8]| {
                        while payloads[j] != p {
                            j += 1;
                        }
                        h[j] = kf.test(k);
                    };
                    if ctx.is_cursor() {
                        let scratch = ctx.charge(0)?;
                        let mut reserved = 0;
                        let mut examined = 0usize;
                        vocab.get_sorted_checked(
                            &payloads,
                            |need| {
                                let bytes = (need as u64).saturating_mul(2);
                                scratch.add(bytes.saturating_sub(reserved))?;
                                reserved = bytes;
                                Ok::<_, crate::error::Error>(())
                            },
                            |p, k| {
                                if examined.is_multiple_of(1024) {
                                    ctx.check()?;
                                }
                                examined += 1;
                                test(p, k);
                                Ok::<_, crate::error::Error>(())
                            },
                        )?;
                    } else {
                        vocab.get_sorted(&payloads, test);
                    }
                    Ok::<_, crate::error::Error>(())
                },
            )?;
    }
    let mut rest = Table::new(vec![v]);
    let mut rest_at = Vec::new();
    for (i, id) in uniq.iter().enumerate().filter(|(i, _)| *i < lo || *i >= hi) {
        if ctx.is_cursor() {
            let test = match id.tag() {
                Tag::Delta => ctx
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| vocab.get(id.payload()).map(|key| kf.test(key))),
                Tag::Local => ctx.with_local_key(id.payload(), |key| kf.test(key)),
                _ => None,
            };
            if let Some(test) = test {
                hit[i] = test;
                continue;
            }
            rest.push_row(&[*id]);
            rest_at.push(i);
            continue;
        }
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

/// Charge-owning ORDER variable values, decoded once per sorted ID. Unlike the
/// scalar cache, this operation-local bulk decoder never populates Ctx.values.
pub(super) fn cursor_variable_values(
    ctx: &Ctx,
    table: &Table,
    expr: &Expr,
) -> Result<Option<PerValue<Option<super::value::Value>>>> {
    cursor_values(ctx, table, expr, false)
}

/// Decode only dictionary-backed values. Inline numeric IDs keep their cheap
/// direct path; their slots are the unbound sentinel and must not be read.
pub(super) fn cursor_dictionary_values(
    ctx: &Ctx,
    table: &Table,
    expr: &Expr,
) -> Result<Option<PerValue<Option<super::value::Value>>>> {
    cursor_values(ctx, table, expr, true)
}

fn cursor_values(
    ctx: &Ctx,
    table: &Table,
    expr: &Expr,
    dictionary_only: bool,
) -> Result<Option<PerValue<Option<super::value::Value>>>> {
    let Expr::Var(var) = expr else {
        return Ok(None);
    };
    if !ctx.opt.expr_cache || table.len < MIN_ROWS {
        return Ok(None);
    }
    let Some(column) = table.col_of(*var) else {
        return Ok(None);
    };
    if table.len > u32::MAX as usize {
        return Ok(None);
    }
    let ids = &table.cols[column];
    let dictionary = |id: Id| matches!(id.tag(), Tag::Vocab | Tag::Delta | Tag::Local);
    let count = if dictionary_only {
        ids.iter().filter(|&&id| dictionary(id)).count()
    } else {
        table.len
    };
    if dictionary_only && count < 256 {
        return Ok(None);
    }
    if !dictionary_only
        && ids.iter().all(|id| {
            matches!(
                id.tag(),
                Tag::Undef | Tag::Special | Tag::Int | Tag::Double | Tag::Decimal | Tag::Bool
            )
        })
    {
        let bytes = table.len as u64
            * (std::mem::size_of::<Option<super::value::Value>>() as u64 * 2 + 8)
            + 4096;
        let Ok(charge) = ctx.retained_charge(bytes) else {
            return Ok(None);
        };
        let vals = ids
            .par_chunks(4096)
            .map(|chunk| {
                ctx.check()?;
                Ok(chunk.iter().map(|&id| ctx.value(id)).collect::<Vec<_>>())
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        ctx.check()?;
        return Ok(Some(PerValue {
            vals,
            slot: Some((0..table.len as u32).collect()),
            _charge: charge,
        }));
    }
    let base = count as u64 * 128 + table.len as u64 * 8 + 4096;
    let Ok(mut charge) = ctx.retained_charge(base) else {
        return Ok(None);
    };
    let mut pairs = table.cols[column]
        .iter()
        .copied()
        .enumerate()
        .filter(|&(_, id)| !dictionary_only || dictionary(id))
        .map(|(row, id)| (id, row as u32))
        .collect::<Vec<_>>();
    if pairs.len() >= 16_384 {
        pairs.par_sort_unstable();
    } else {
        pairs.sort_unstable();
    }
    ctx.check()?;
    let mut unique = Vec::with_capacity(count + usize::from(dictionary_only));
    if dictionary_only {
        unique.push(Id::UNDEF);
    }
    let mut slots = vec![0u32; table.len];
    for (row, &(id, position)) in pairs.iter().enumerate() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        if unique.last() != Some(&id) {
            unique.push(id);
        }
        slots[position as usize] = (unique.len() - 1) as u32;
    }
    let mut vals = vec![None; unique.len()];
    let lo = unique.partition_point(|id| id.tag() < Tag::Vocab);
    let hi = lo + unique[lo..].partition_point(|id| id.tag() == Tag::Vocab);
    let payloads = unique[lo..hi]
        .iter()
        .map(|id| id.payload())
        .collect::<Vec<_>>();
    let scratch = ctx.charge(0)?;
    let mut reserved = 0;
    let mut bytes = base;
    let mut at = lo;
    let parallel_bytes = (payloads.len() as u64)
        .saturating_mul(std::mem::size_of::<Option<super::value::Value>>() as u64 * 2)
        .saturating_add(rayon::current_num_threads() as u64 * 2048);
    let parallel = (payloads.len() >= 4096)
        .then(|| ctx.charge(parallel_bytes).ok())
        .flatten();
    if let Some(_parallel) = parallel {
        // First measure retained payload without constructing values. Admit the
        // combined payload once, before parallel decoding, instead of contending
        // on charge ownership and counters for every dictionary value.
        let payload = payloads
            .par_chunks(4096)
            .map(|chunk| {
                let scratch = ctx.charge(0)?;
                let mut reserved = 0;
                let mut bytes = 0u64;
                ctx.snap.generation.vocab.get_sorted_checked(
                    chunk,
                    |need| {
                        let need = need as u64 * 2;
                        scratch.add(need.saturating_sub(reserved))?;
                        reserved = need;
                        ctx.check()
                    },
                    |_, key| {
                        bytes = bytes.saturating_add(key.len() as u64 * 8 + 128);
                        Ok::<_, crate::error::Error>(())
                    },
                )?;
                ctx.check()?;
                Ok(bytes)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .fold(0u64, u64::saturating_add);
        bytes = bytes.saturating_add(payload);
        if let Some(charge) = &mut charge
            && charge.resize(bytes).is_err()
        {
            return Ok(None);
        }
        let decoded = payloads
            .par_chunks(4096)
            .map(|chunk| {
                let scratch = ctx.charge(0)?;
                let mut reserved = 0;
                let mut values = Vec::with_capacity(chunk.len());
                ctx.snap.generation.vocab.get_sorted_checked(
                    chunk,
                    |need| {
                        let need = need as u64 * 2;
                        scratch.add(need.saturating_sub(reserved))?;
                        reserved = need;
                        ctx.check()
                    },
                    |_, key| {
                        if values.len().is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        values.push(Some(super::value::Value::from_key(key)));
                        Ok::<_, crate::error::Error>(())
                    },
                )?;
                ctx.check()?;
                Ok(values)
            })
            .collect::<Result<Vec<_>>>()?;
        for value in decoded.into_iter().flatten() {
            vals[at] = value;
            at += 1;
        }
    } else {
        let decoded = ctx.snap.generation.vocab.get_sorted_checked(
            &payloads,
            |need| {
                let need = need as u64 * 2;
                scratch.add(need.saturating_sub(reserved))?;
                reserved = need;
                ctx.check()
            },
            |_, key| {
                if at.is_multiple_of(1024) {
                    ctx.check()?;
                }
                bytes = bytes.saturating_add(key.len() as u64 * 8 + 128);
                if let Some(charge) = &mut charge {
                    charge.resize(bytes)?;
                }
                vals[at] = Some(super::value::Value::from_key(key));
                at += 1;
                Ok::<_, crate::error::Error>(())
            },
        );
        match decoded {
            Err(crate::Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory => {
                return Ok(None);
            }
            result => result?,
        }
    }
    for row in (0..lo).chain(hi..unique.len()) {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        if !matches!(
            unique[row].tag(),
            Tag::Undef | Tag::Special | Tag::Int | Tag::Double | Tag::Decimal | Tag::Bool
        ) {
            bytes = bytes.saturating_add(ctx.decoded_bytes(unique[row])?);
            if let Some(charge) = &mut charge {
                charge.resize(bytes)?;
            }
        }
        vals[row] = ctx.value(unique[row]);
    }
    ctx.check()?;
    drop(pairs);
    drop(unique);
    drop(payloads);
    drop(scratch);
    Ok(Some(PerValue {
        vals,
        slot: Some(slots),
        _charge: charge,
    }))
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
