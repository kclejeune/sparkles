//! Pure expressions evaluated once per distinct input value.
//!
//! An expression that reads one variable and is pure within a query has one result per
//! term of that variable, so it is evaluated once per distinct id of the variable's
//! column, and every row reads its result from there. FILTER conjuncts, BIND, ORDER BY
//! keys and aggregate arguments use it (GROUP BY on an expression is a BIND first).
//!
//! * Pure within a query: no RAND, UUID, STRUUID, BNODE (a fresh node per call, or one
//!   per solution), their volatile ARQ aliases, dynamic calls or EXISTS (correlated
//!   with the whole row). NOW and the base IRI are
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
/// Sorted distinct dictionary ids that one parallel task of [`decode_in_pieces`] locates in
/// the numeric column and decodes, and the fewest for which the decoding is parallel.
const DECODE_PIECE: usize = 4096;

/// What one operator's expressions did, for EXPLAIN.
#[derive(Default, Debug)]
pub struct Report(Vec<Entry>, Vec<Decided>);

/// Comparisons with constants that the ids of a column decided (see `rangemask`).
#[derive(Debug)]
struct Decided {
    expr: String,
    rows: usize,
    /// the rows whose values had to be evaluated
    tested: usize,
}

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
        self.1.extend(other.1);
    }

    /// Record that the ids of a column decided the comparisons `exprs` on `rows` rows,
    /// except for `tested` rows that were evaluated.
    pub(super) fn decided(&mut self, ctx: &Ctx, exprs: &[Expr], rows: usize, tested: usize) {
        let expr = exprs
            .iter()
            .map(|e| e.display(ctx))
            .collect::<Vec<_>>()
            .join(" && ");
        self.1.push(Decided { expr, rows, tested });
    }

    /// `[decided on ids: …]` and `[expr cache: …]` for the operator's description.
    pub fn note(&self) -> Option<String> {
        let ids = (!self.1.is_empty()).then(|| {
            let parts: Vec<String> = self
                .1
                .iter()
                .map(|d| format!("{} for {} of {} rows", d.expr, d.rows - d.tested, d.rows))
                .collect();
            format!("[decided on ids: {}]", parts.join("; "))
        });
        let cache = self.cache_note();
        match (ids, cache) {
            (Some(a), Some(b)) => Some(format!("{a} {b}")),
            (a, b) => a.or(b),
        }
    }

    fn cache_note(&self) -> Option<String> {
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
        Expr::Call(Func::Ext(iri), args) => {
            let afn = iri.strip_prefix(super::expr::AFN);
            match afn {
                Some("uuid") => Some("UUID"),
                Some("struuid") => Some("STRUUID"),
                Some("wait" | "print" | "execTime") => Some("side effects"),
                Some("eval") => Some("dynamic function"),
                _ if iri.strip_prefix(super::expr::FN) == Some("apply") => Some("dynamic function"),
                _ => args.iter().find_map(impurity),
            }
        }
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
    /// Charge `bytes` more to these values until they are dropped, such as the heap
    /// payloads of their results.
    pub(super) fn retain(&mut self, ctx: &Ctx, bytes: u64) -> Result<()> {
        match &mut self._charge {
            Some(charge) => charge.resize(charge.bytes().saturating_add(bytes)),
            None => {
                self._charge = ctx.retained_charge(bytes)?;
                Ok(())
            }
        }
    }

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

    /// The number of distinct results when the column stores one per distinct input
    /// value, which [`Column::index`] then numbers.
    pub fn distinct(&self) -> Option<usize> {
        match self {
            Column::Rows { .. } => None,
            Column::Values(p) => Some(p.vals.len()),
        }
    }

    /// The index of a row's result among the distinct results (see [`Column::distinct`]).
    #[inline]
    pub fn index(&self, row: usize) -> usize {
        match self {
            Column::Rows { .. } => row,
            Column::Values(p) => p.index(row),
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

impl<T: Send> Column<T> {
    /// Drop the column, freeing the stored results of a large one on the pool. Freeing
    /// the 102,000 decoded names of `order-by-full` on the query's thread took about 2 ms
    /// of its 13 ms on forge with 12 threads. The results go before the charge that
    /// holds them, as an ordinary drop orders them.
    pub fn drop_in_parallel(self) {
        let (vals, charge) = match self {
            Column::Rows { vals, _charge } => (vals, _charge),
            Column::Values(p) => (p.vals, p._charge),
        };
        if vals.len() > super::exec::PAR_THRESHOLD && std::mem::needs_drop::<T>() {
            vals.into_par_iter()
                .with_min_len(PAR_MIN_LEN)
                .for_each(drop);
        } else {
            drop(vals);
        }
        drop(charge);
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
    // The slots, the distinct values and their results live until the caller has
    // expanded or read them, so they stay charged until then in both accounting modes.
    let held = (d.slot.as_ref().map_or(0, Vec::capacity) as u64)
        .saturating_mul(4)
        .saturating_add(d.values.mem_bytes());
    let size = std::mem::size_of::<T>() as u64;
    // Reuse is optional, so a charge that does not fit, before or after the evaluation,
    // leaves the rows to the caller instead of failing the query.
    let Ok(mut charge) =
        ctx.retained_charge(held.saturating_add((d.values.len() as u64).saturating_mul(size)))
    else {
        return Ok(None);
    };
    let vals = eval(&d.values)?;
    debug_assert_eq!(vals.len(), d.values.len());
    if let Some(charge) = &mut charge
        && charge
            .resize(held.saturating_add((vals.capacity() as u64).saturating_mul(size)))
            .is_err()
    {
        return Ok(None);
    }
    ctx.check()?;
    Ok(Some(PerValue {
        vals,
        slot: d.slot,
        _charge: charge,
    }))
}

/// Fixed-size numeric ORDER keys can reuse input IDs without retaining decoded
/// strings or populating the eager value cache. Own the reservation with the
/// returned values/slots; it must survive until the sort has finished.
///
/// An input of more than [`super::exec::PAR_THRESHOLD`] rows takes this path only when
/// its integers span a dense range, which [`cursor_integer_distinct`] reads in parallel,
/// and leaves other inputs to [`per_value`].
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
    let large = t.len() > super::exec::PAR_THRESHOLD;
    // A large input holds its slots and at most one position of each value in the range.
    let bytes = if large {
        (t.len() as u64 * 4).saturating_add(DENSE_SPAN as u64 * 64 + 4096)
    } else {
        (t.len() as u64).saturating_mul(256).saturating_add(4096)
    };
    let Ok(mut charge) = ctx.retained_charge(bytes) else {
        return Ok(None);
    };
    let d = match cursor_integer_distinct(ctx, t, &[e], v, report)? {
        Some(d) => Some(d),
        None if large => None,
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

/// BIND reuse in both accounting modes: the value of `e` on every row, evaluated once per
/// distinct input value, or `None` when reuse is not admitted.
///
/// The input's size chooses the algorithm. A batch of at most
/// [`super::exec::PAR_THRESHOLD`] rows, which holds a cursor's batches, keeps only ids and
/// slots under a conservative reservation that covers the distinct set, the table, the
/// slots and the expanded output until the output is expanded, finds its distinct inputs
/// from a dense integer range or one sort, and expands on the calling thread. A larger
/// table estimates its distinct inputs before it collects them and expands in parallel.
/// Neither retains decoded heap values.
pub(super) fn column(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut Report,
    eval: impl FnOnce(&Table) -> Result<Vec<Id>>,
) -> Result<Option<Vec<Id>>> {
    // The size test comes first: `eligible` walks the expression and formats its
    // reason, which cost a one-row BIND, such as an aggregate's, about 400 instructions.
    if !ctx.opt.expr_cache || t.len() < MIN_ROWS {
        return Ok(None);
    }
    let Ok(v) = eligible(&[e]) else {
        return Ok(None);
    };
    if t.len() > super::exec::PAR_THRESHOLD {
        return Ok(per_value(ctx, t, &[e], false, report, eval)?.map(|p| p.rows(t.len())));
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
        None => distinct(ctx, t, &[e], false, report)?,
    };
    let Some(d) = d else {
        return Ok(None);
    };
    let vals = eval(&d.values)?;
    debug_assert_eq!(vals.len(), d.values.len());
    ctx.check()?;
    // A batch is small enough that scheduling the expansion on Rayon costs more than
    // the id copies.
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
) -> Result<Option<Distinct>> {
    let n = t.len();
    if !ctx.opt.expr_cache || n < MIN_ROWS {
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
///
/// A batch of at most [`super::exec::PAR_THRESHOLD`] rows, which holds a cursor's
/// batches, whose conjuncts all read one variable runs them together under one
/// reservation (see [`batch_filter`]). Other inputs group their conjuncts by variable.
pub fn filter(
    ctx: &Ctx,
    t: &Table,
    exprs: &[Expr],
    report: &mut Report,
) -> Result<Option<(Vec<bool>, Vec<Expr>)>> {
    if !ctx.opt.expr_cache || t.len() < MIN_ROWS {
        return Ok(None);
    }
    if t.len() <= super::exec::PAR_THRESHOLD && eligible(&exprs.iter().collect::<Vec<_>>()).is_ok()
    {
        return batch_filter(ctx, t, exprs, report);
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

/// Batch-local FILTER reuse stores only ids, slots and booleans. A conservative
/// reservation covers distinct construction and key-test scratch until expansion, and no
/// decoded value outlives the batch.
#[inline(never)]
fn batch_filter(
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
        _ => distinct(ctx, t, &refs, false, report)?,
    };
    let Some(d) = d else {
        drop(_held);
        return if key_bytes.is_none() {
            numeric_filter(ctx, t, exprs).map(|keep| keep.map(|keep| (keep, Vec::new())))
        } else {
            Ok(None)
        };
    };
    let vals = match (key_bytes, v) {
        // The shape admits key tests, but a pattern that does not compile or a charge
        // that does not fit leaves the values to the general evaluator, where an
        // invalid pattern is an error and so fails the filter.
        (Some(_), Some(v)) => match super::keyfilter::KeyFilter::new_for(ctx, exprs, v) {
            Some(kf) => key_filter_mask(ctx, &kf, &d.values.cols[0], v, exprs)?,
            None => super::exec::filter_mask(ctx, &d.values, exprs)?,
        },
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
/// declined. An optional memory failure returns to scalar filtering, and no decoded
/// dictionary outlives the batch.
#[inline(never)]
fn numeric_filter(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Result<Option<Vec<bool>>> {
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
    if !ctx.opt.expr_cache || t.len() < 4096 || !exprs.iter().all(numeric) {
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
        Err(crate::Error::BudgetExceeded(b))
            if matches!(
                b.kind,
                crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
            ) =>
        {
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
/// The widest range of integers that [`cursor_integer_distinct`] maps directly.
const DENSE_SPAN: usize = 1 << 16;
/// The rows above which [`cursor_integer_distinct`] maps in parallel. Mapping takes about
/// a nanosecond a row, which a cursor's batches of a few tens of thousands of rows do not
/// amortize on the pool.
const DENSE_PAR_ROWS: usize = 1 << 18;

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
    if t.len() > DENSE_PAR_ROWS {
        return par_integer_distinct(ctx, t, exprs, v, column, report);
    }
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
    if span > (t.len() / 2).min(DENSE_SPAN) as u64 {
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

/// [`cursor_integer_distinct`] for an input of more than [`super::exec::PAR_THRESHOLD`]
/// rows, whose range, observed values and slots are found in parallel pieces.
fn par_integer_distinct(
    ctx: &Ctx,
    t: &Table,
    exprs: &[&Expr],
    v: VarId,
    column: usize,
    report: &mut Report,
) -> Result<Option<Distinct>> {
    const PIECE: usize = 1 << 16;
    let col = &t.cols[column];
    ctx.check()?;
    let range = col
        .par_chunks(PIECE)
        .map(|piece| {
            piece.iter().try_fold((u64::MAX, 0), |(lo, hi), id| {
                (id.tag() == Tag::Int).then(|| (lo.min(id.payload()), hi.max(id.payload())))
            })
        })
        .try_reduce(|| (u64::MAX, 0), |a, b| Some((a.0.min(b.0), a.1.max(b.1))));
    let Some((lo, hi)) = range else {
        return Ok(None);
    };
    let span = hi.saturating_sub(lo).saturating_add(1);
    if span > (t.len() / 2).min(DENSE_SPAN) as u64 {
        return Ok(None);
    }
    let span = span as usize;
    ctx.check()?;
    let seen = col
        .par_chunks(PIECE)
        .map(|piece| {
            let mut seen = vec![false; span];
            for id in piece {
                seen[(id.payload() - lo) as usize] = true;
            }
            seen
        })
        .reduce(
            || vec![false; span],
            |mut a, b| {
                a.iter_mut().zip(b).for_each(|(a, b)| *a |= b);
                a
            },
        );
    ctx.check()?;
    let mut values = Table::new(vec![v]);
    let mut positions = vec![u32::MAX; span];
    for (offset, seen) in seen.into_iter().enumerate() {
        if seen {
            positions[offset] = values.len as u32;
            values.push_row(&[Id::new(Tag::Int, lo + offset as u64)]);
        }
    }
    let slot: Vec<u32> = col
        .par_iter()
        .with_min_len(super::exec::PAR_MIN_LEN)
        .map(|id| positions[(id.payload() - lo) as usize])
        .collect();
    ctx.check()?;
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
        return distinct(ctx, t, exprs, always, report);
    };
    if t.sorted.first() == Some(&v) {
        return distinct(ctx, t, exprs, always, report);
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
        Some(prefixes) => {
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
    if uniq.len() <= 4096 {
        // A few thousand values, such as those of a cursor's batch, do not amortize
        // scheduling tasks on Rayon.
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
                    // Decoding a block reserves its keys in both accounting modes.
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
                    Ok::<_, crate::error::Error>(())
                },
            )?;
    }
    let mut rest = Table::new(vec![v]);
    let mut rest_at = Vec::new();
    for (i, id) in uniq.iter().enumerate().filter(|(i, _)| *i < lo || *i >= hi) {
        // Terms added by updates and query-local terms are tested on their keys without
        // copying them, and the rest by the general evaluator.
        let test = match id.tag() {
            Tag::Delta => ctx
                .snap
                .generation
                .dvocab
                .with(|vocab| vocab.get(id.payload()).map(|key| kf.test(key))),
            Tag::Local => ctx.with_local_key(id.payload(), |key| kf.test(key)),
            _ => None,
        };
        match test {
            Some(test) => hit[i] = test,
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
        // A large inline column keeps a value per row here, where per-value reuse keeps one
        // per distinct value.
        if table.len > super::exec::PAR_THRESHOLD {
            return Ok(None);
        }
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
    let mut payloads = unique[lo..hi]
        .iter()
        .map(|id| id.payload())
        .collect::<Vec<_>>();
    let numeric = ctx.snap.generation.vocab.numeric();
    let scratch = ctx.charge(0)?;
    let mut reserved = 0;
    let mut bytes = base;
    let step = ctx.reservation_step();
    let parallel_bytes = (payloads.len() as u64)
        .saturating_mul(std::mem::size_of::<Option<super::value::Value>>() as u64 * 2)
        .saturating_add(rayon::current_num_threads() as u64 * 2048);
    let parallel = (payloads.len() >= DECODE_PIECE)
        .then(|| ctx.charge(parallel_bytes).ok())
        .flatten();
    if let Some(_parallel) = parallel {
        match decode_in_pieces(ctx, &payloads, &mut vals[lo..hi], numeric, &mut charge)? {
            Some(payload) => bytes = bytes.saturating_add(payload),
            None => return Ok(None),
        }
    } else {
        // where each payload's value goes in `vals`
        let mut positions: Vec<usize> = (lo..hi).collect();
        // numbers from the numeric column: only the other terms are decoded
        if let Some(num) = numeric
            && !payloads.is_empty()
        {
            let found = num.get_many(&payloads);
            ctx.check()?;
            let (mut rest, mut rest_at) = (Vec::new(), Vec::new());
            for ((&p, &pos), n) in payloads.iter().zip(&positions).zip(found) {
                match n.value() {
                    Some(v) => vals[pos] = Some(v),
                    None => {
                        rest.push(p);
                        rest_at.push(pos);
                    }
                }
            }
            payloads = rest;
            positions = rest_at;
        }
        let mut at = 0usize;
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
                    charge.cover(bytes, step)?;
                }
                vals[positions[at]] = Some(super::value::Value::from_key(key));
                at += 1;
                Ok::<_, crate::error::Error>(())
            },
        );
        match decoded {
            Err(crate::Error::BudgetExceeded(b))
                if matches!(
                    b.kind,
                    crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                ) =>
            {
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
            if let Some(charge) = &mut charge
                && let Err(error) = charge.cover(bytes, step)
            {
                // A large input's caller falls back to per-value reuse, which keeps one value
                // per distinct term, so the refusal declines this optional decoding there. A
                // smaller input's caller retries with a smaller trial and needs the refusal.
                if table.len > super::exec::PAR_THRESHOLD {
                    return Ok(None);
                }
                return Err(error);
            }
        }
        vals[row] = ctx.cursor_value_checked(unique[row])?;
    }
    ctx.check()?;
    drop(pairs);
    drop(unique);
    drop(payloads);
    drop(scratch);
    // The sort keeps the slots, one value per distinct term and the values' payload, and
    // the construction's pairs and distinct ids are gone.
    if let Some(charge) = &mut charge {
        let kept = slots.capacity() as u64 * 4
            + vals.capacity() as u64 * std::mem::size_of::<Option<super::value::Value>>() as u64
            + bytes.saturating_sub(base)
            + 4096;
        // The steps' slack goes too.
        if kept < charge.bytes() {
            charge.resize(kept)?;
        }
    }
    Ok(Some(PerValue {
        vals,
        slot: Some(slots),
        _charge: charge,
    }))
}

/// Decode the sorted distinct base-vocabulary ids `payloads` into `out`, which holds a
/// slot for each, in one parallel pass over pieces of [`DECODE_PIECE`] ids. Each piece
/// reads its numbers from the numeric column and decodes the rest in key order, admitting
/// each value's payload before it builds the value, on a charge of its own on `charge`'s
/// owner that grows a step ahead and exactly when the step is refused. The pieces' charges
/// join `charge` once the pass ends, and their steps' slack is given back.
///
/// The payload bytes, or `None` when the budget refused a payload, which declines the
/// optional decoding. A context without an owner has no charge, and its values are kept
/// uncharged, as eager decoding always kept them.
///
/// It replaces a pass that measured every payload before a second pass decoded it, and
/// the serial placing of the numbers and the decoded values that followed each pass, which
/// made eager `order-by-full` about 3% slower on forge with 12 threads.
fn decode_in_pieces(
    ctx: &Ctx,
    payloads: &[u64],
    out: &mut [Option<super::value::Value>],
    numeric: Option<&crate::vocab::numeric::NumColumn>,
    charge: &mut Option<super::ctx::RetainedCharge>,
) -> Result<Option<u64>> {
    debug_assert_eq!(payloads.len(), out.len());
    let step = ctx.reservation_step();
    let refused = std::sync::atomic::AtomicBool::new(false);
    let owner = charge.as_ref();
    // Every piece runs to its end or its first error and returns its charge with it, so
    // that no charge is released while values it admitted are still in `out`.
    let mut pieces: Vec<_> = out
        .par_chunks_mut(DECODE_PIECE)
        .zip(payloads.par_chunks(DECODE_PIECE))
        .map(|(out, piece)| {
            let part = owner.map(|c| c.part(ctx));
            let mut covered = 0u64;
            let mut bytes = 0u64;
            let mut decode = || -> Result<()> {
                ctx.check()?;
                // the payloads to decode and where their values go in `out`
                let (rest, rest_at): (std::borrow::Cow<'_, [u64]>, Option<Vec<usize>>) =
                    match numeric {
                        Some(num) => {
                            let mut rest = Vec::new();
                            let mut rest_at = Vec::new();
                            for (i, (&p, n)) in piece.iter().zip(num.get_many(piece)).enumerate() {
                                match n.value() {
                                    Some(v) => out[i] = Some(v),
                                    None => {
                                        rest.push(p);
                                        rest_at.push(i);
                                    }
                                }
                            }
                            (rest.into(), Some(rest_at))
                        }
                        None => (piece.into(), None),
                    };
                let scratch = ctx.charge(0)?;
                let mut reserved = 0;
                let mut at = 0usize;
                ctx.snap.generation.vocab.get_sorted_checked(
                    &rest,
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
                        if let Some(part) = &part
                            && bytes > covered
                        {
                            let ahead = bytes.saturating_add(step);
                            match part.add(ahead - covered) {
                                Ok(()) => covered = ahead,
                                Err(_) => {
                                    part.add(bytes - covered).inspect_err(|_| {
                                        refused.store(true, std::sync::atomic::Ordering::Relaxed)
                                    })?;
                                    covered = bytes;
                                }
                            }
                        }
                        let i = rest_at.as_ref().map_or(at, |r| r[at]);
                        out[i] = Some(super::value::Value::from_key(key));
                        at += 1;
                        Ok::<_, crate::error::Error>(())
                    },
                )?;
                ctx.check()
            };
            let result = decode();
            (result, bytes, covered, part)
        })
        .collect();
    if let Some(at) = pieces.iter().position(|(result, ..)| result.is_err()) {
        let (result, _, _, part) = pieces.swap_remove(at);
        // The values go before the charges that admitted them.
        out.par_iter_mut()
            .with_min_len(PAR_MIN_LEN)
            .for_each(|value| *value = None);
        drop((part, pieces));
        return match result {
            Err(crate::Error::BudgetExceeded(_))
                if refused.load(std::sync::atomic::Ordering::Relaxed) =>
            {
                Ok(None)
            }
            result => result.map(|()| None),
        };
    }
    let (mut payload, mut slack) = (0u64, 0u64);
    for (_, bytes, covered, part) in pieces {
        payload = payload.saturating_add(bytes);
        // only a charged piece covers its payload, a step ahead at most
        if let Some(part) = part {
            slack = slack.saturating_add(covered - bytes);
            if let Some(charge) = charge.as_mut() {
                charge.absorb(part);
            }
        }
    }
    if let Some(charge) = charge.as_mut()
        && slack > 0
    {
        charge.resize(charge.bytes() - slack)?;
    }
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[i64]) -> Vec<Id> {
        v.iter().map(|&i| Id::from_i64(i).unwrap()).collect()
    }

    /// The parallel mapping of a dense range of integers, over several pieces, finds the
    /// distinct values in order and a slot for every row, and declines a range wider than
    /// it maps or a column with another kind of id.
    #[test]
    fn the_parallel_dense_mapping_finds_every_value_and_slot() {
        let store = crate::store::Store::in_memory(Default::default());
        let ctx = Ctx::new(store.snapshot());
        let v = ctx.var("k");
        let n = DENSE_PAR_ROWS + 2 * (1 << 16) + 123;
        let vals: Vec<i64> = (0..n).map(|i| 1000 + ((i * 7919) % 97) as i64).collect();
        let mut t = Table::new(vec![v]);
        t.cols[0] = ids(&vals);
        t.len = n;
        let e = Expr::Var(v);
        let mut report = Report::default();
        let d = cursor_integer_distinct(&ctx, &t, &[&e], v, &mut report)
            .unwrap()
            .expect("a dense range maps directly");
        let mut uniq = vals.clone();
        uniq.sort_unstable();
        uniq.dedup();
        let found: Vec<i64> = d.values.cols[0].iter().map(|id| id.as_i64()).collect();
        assert_eq!(found, uniq);
        let slot = d.slot.expect("every row has a slot");
        assert_eq!(slot.len(), n);
        for (row, &x) in vals.iter().enumerate() {
            assert_eq!(uniq[slot[row] as usize], x, "row {row}");
        }
        t.cols[0][n - 1] = Id::from_i64(1000 + DENSE_SPAN as i64).unwrap();
        assert!(
            cursor_integer_distinct(&ctx, &t, &[&e], v, &mut report)
                .unwrap()
                .is_none()
        );
        t.cols[0][n - 1] = Id::UNDEF;
        assert!(
            cursor_integer_distinct(&ctx, &t, &[&e], v, &mut report)
                .unwrap()
                .is_none()
        );
    }

    fn owned(
        store: &crate::store::Store,
        limit: u64,
        values: &[&str],
    ) -> (std::sync::Arc<Ctx>, Vec<Id>) {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = limit;
        let ids = values
            .iter()
            .map(|v| ctx.intern_value(&super::super::value::Value::Str((*v).into())))
            .collect();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        (ctx, ids)
    }

    fn column(ctx: &Ctx, ids: Vec<Id>) -> (Table, VarId) {
        let v = ctx.var("k");
        let mut t = Table::new(vec![v]);
        t.len = ids.len();
        t.cols[0] = ids;
        (t, v)
    }

    /// The decoded values of a large column keep a charge for their slots, one value per
    /// distinct term and its payload, not the reservation that building them took.
    #[test]
    fn decoded_keys_keep_only_what_the_sort_holds() {
        let store = crate::store::Store::in_memory(Default::default());
        let (ctx, terms) = owned(&store, 1 << 30, &["a", "b"]);
        let n = 4 * super::super::exec::PAR_THRESHOLD;
        let (t, v) = column(&ctx, (0..n).map(|i| terms[i % 2]).collect());
        let before = ctx.memory_remaining();
        let values = cursor_variable_values(&ctx, &t, &Expr::Var(v))
            .unwrap()
            .expect("a column of local terms decodes per term");
        assert_eq!(values.vals.len(), 2);
        let held = before - ctx.memory_remaining();
        assert!(
            held <= n as u64 * 4 + (64 << 10),
            "{held} bytes kept for {n} rows"
        );
    }

    /// A large column of inline values is left to per-value reuse, which keeps one value
    /// per distinct value rather than one per row.
    #[test]
    fn a_large_inline_column_is_left_to_per_value_reuse() {
        let store = crate::store::Store::in_memory(Default::default());
        let (ctx, _) = owned(&store, 1 << 30, &[]);
        let n = 2 * super::super::exec::PAR_THRESHOLD;
        let (t, v) = column(
            &ctx,
            ids(&(0..n).map(|i| [-1, 1][i % 2]).collect::<Vec<_>>()),
        );
        assert!(
            cursor_variable_values(&ctx, &t, &Expr::Var(v))
                .unwrap()
                .is_none()
        );
        let small = super::super::exec::PAR_THRESHOLD / 2;
        let (t, v) = column(&ctx, t.cols[0][..small].to_vec());
        assert!(
            cursor_variable_values(&ctx, &t, &Expr::Var(v))
                .unwrap()
                .is_some()
        );
    }

    /// The least memory room in which `cursor_variable_values` decodes the column, which is
    /// the peak of its construction, since the built column shrinks its charge.
    fn least_room(ctx: &Ctx, table: &Table) -> u64 {
        let decodes_in = |room: u64| {
            let storage = ctx.charge(ctx.memory_room() - room).unwrap();
            let column = cursor_variable_values(ctx, table, &Expr::Var(table.vars[0]));
            drop(storage);
            matches!(column, Ok(Some(_)))
        };
        let (mut low, mut high) = (0, ctx.memory_room());
        assert!(decodes_in(high));
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            if decodes_in(mid) {
                high = mid;
            } else {
                low = mid;
            }
        }
        high
    }

    /// A store whose base vocabulary holds `n` distinct simple literals and a few numbers
    /// too large to be inline, with the ids of a column that reads each literal twice and
    /// each number once, in a scrambled order.
    fn vocabulary_column(n: usize) -> (crate::store::Store, Vec<Id>) {
        use crate::io::{RdfFormat, Source};
        use oxrdf::{Literal, NamedNode, Term};
        const INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
        let store = crate::store::Store::in_memory(Default::default());
        let literal = |i: usize| format!("value {i:06} {}", "x".repeat(i % 37));
        let number = |i: usize| format!("{}{i:04}", "9".repeat(30));
        let mut nt = String::new();
        for i in 0..n {
            let o = literal(i);
            nt.push_str(&format!(
                "<http://ex.org/s{i}> <http://ex.org/v> \"{o}\" .\n"
            ));
        }
        for i in 0..n / 8 {
            let o = number(i);
            nt.push_str(&format!(
                "<http://ex.org/n{i}> <http://ex.org/v> \"{o}\"^^<{INTEGER}> .\n"
            ));
        }
        store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let id = |term: Term| {
            let id = snap.lookup_term(&term).expect("a loaded term");
            assert_eq!(id.tag(), Tag::Vocab, "{term}");
            id
        };
        let mut col: Vec<Id> = (0..2 * n)
            .map(|i| id(Literal::new_simple_literal(literal(i % n)).into()))
            .collect();
        col.extend((0..n / 8).map(|i| {
            id(Literal::new_typed_literal(number(i), NamedNode::new_unchecked(INTEGER)).into())
        }));
        let mut next = 0x9e37_79b9_7f4a_7c15u64;
        for i in (1..col.len()).rev() {
            next ^= next << 13;
            next ^= next >> 7;
            next ^= next << 17;
            col.swap(i, (next % (i as u64 + 1)) as usize);
        }
        (store, col)
    }

    /// A column with more distinct base-vocabulary terms than one piece decodes them in
    /// parallel pieces, in place and in key order, to the values that decoding each term
    /// alone gives, and keeps their payload charged without the steps' slack.
    #[test]
    fn vocabulary_keys_decode_in_pieces_with_their_payload_charged() {
        let n = 3 * DECODE_PIECE + 17;
        let (store, col) = vocabulary_column(n);
        let (ctx, _) = owned(&store, 1 << 30, &[]);
        let (t, v) = column(&ctx, col);
        let before = ctx.live_bytes();
        let values = cursor_variable_values(&ctx, &t, &Expr::Var(v))
            .unwrap()
            .expect("a large column decodes per term");
        assert_eq!(values.vals.len(), n + n / 8);
        let held = ctx.live_bytes() - before;
        // each simple literal's key is its text between a quote and a 0xFF byte
        let payload: u64 = values
            .vals
            .iter()
            .flatten()
            .filter_map(|value| match value {
                super::super::value::Value::Str(s) => Some((s.len() as u64 + 2) * 8 + 128),
                _ => None,
            })
            .sum();
        let slots = t.len() as u64 * 4
            + values.vals.capacity() as u64
                * std::mem::size_of::<Option<super::super::value::Value>>() as u64;
        assert!(
            held >= payload,
            "{held} bytes held for {payload} of payload"
        );
        assert!(
            held <= slots + payload + (n as u64 / 8) * 1024 + 4096,
            "{held} bytes held for {payload} of payload and {slots} of slots"
        );
        // decoding each term alone, which leaves the value cache as it is
        for (row, &id) in t.cols[0].iter().enumerate() {
            let alone = ctx.cursor_value_checked(id).unwrap();
            assert_eq!(
                format!("{:?}", values.get(row)),
                format!("{alone:?}"),
                "row {row}"
            );
        }
        drop(values);
        ctx.release_work();
        assert_eq!(ctx.live_bytes(), before);
    }

    /// Below the room its pieces need, the parallel decoding of a large column of
    /// base-vocabulary terms declines or fails with the budget, keeps no charge and
    /// leaves no failure behind, and the column decodes once the room is back.
    #[test]
    fn a_refused_vocabulary_payload_keeps_no_charge() {
        let (store, col) = vocabulary_column(2 * DECODE_PIECE);
        let (ctx, _) = owned(&store, 1 << 30, &[]);
        let (t, _) = column(&ctx, col);
        // The room the pieces take depends on how their steps interleave, by up to a
        // step per piece, so half of the least room seen is below what any order needs.
        let peak = least_room(&ctx, &t);
        let storage = ctx.charge(ctx.memory_room() - peak / 2).unwrap();
        let before = ctx.live_bytes();
        let refused = cursor_variable_values(&ctx, &t, &Expr::Var(t.vars[0]));
        assert!(
            matches!(refused, Ok(None) | Err(crate::Error::BudgetExceeded(_))),
            "{:?}",
            refused.map(|v| v.map(|v| v.vals.len()))
        );
        assert_eq!(ctx.live_bytes(), before, "the refusal kept its charge");
        assert!(ctx.check_memory_failure().is_ok());
        drop(storage);
        assert!(
            cursor_variable_values(&ctx, &t, &Expr::Var(t.vars[0]))
                .unwrap()
                .is_some()
        );
    }

    /// A piece whose payload the budget refuses after pieces wrote values clears every
    /// value before any piece's charge goes, declines the decoding and keeps no charge,
    /// and a context without an owner decodes the same column uncharged.
    #[test]
    fn a_refused_piece_clears_the_values_before_their_charges() {
        let (store, col) = vocabulary_column(4 * DECODE_PIECE);
        let (ctx, _) = owned(&store, 1 << 30, &[]);
        let mut payloads: Vec<u64> = col.iter().map(|id| id.payload()).collect();
        payloads.sort_unstable();
        payloads.dedup();
        let numeric = ctx.snap.generation.vocab.numeric();
        let mut out = vec![None; payloads.len()];
        let mut charge = ctx.retained_charge(4096).unwrap();
        let storage = ctx.charge(ctx.memory_room() - 300_000).unwrap();
        let before = ctx.live_bytes();
        assert!(matches!(
            decode_in_pieces(&ctx, &payloads, &mut out, numeric, &mut charge),
            Ok(None)
        ));
        assert!(out.iter().all(Option::is_none));
        assert_eq!(ctx.live_bytes(), before);
        assert_eq!(charge.as_ref().map(|c| c.bytes()), Some(4096));
        drop((storage, charge));
        let unowned = Ctx::new(store.snapshot());
        let mut none = None;
        let payload = decode_in_pieces(&unowned, &payloads, &mut out, numeric, &mut none)
            .unwrap()
            .expect("an uncharged decoding");
        assert!(payload > 0);
        assert!(out.iter().all(Option::is_some));
        assert_eq!(unowned.live_bytes(), 0);
    }

    /// A refused payload charge of query-local terms in a large input declines the
    /// decoding, which is optional, instead of failing the query.
    #[test]
    fn a_refused_local_payload_declines_the_decoding() {
        let store = crate::store::Store::in_memory(Default::default());
        let n = 2 * super::super::exec::PAR_THRESHOLD;
        let fixture = || {
            let (ctx, terms) = owned(&store, 1 << 30, &["local"]);
            // the cursor's value cache holds the term, so the payload is the last charge
            assert!(ctx.value(terms[0]).is_some());
            let (t, _) = column(&ctx, vec![terms[0]; n]);
            (ctx, t)
        };
        let (measure, measured) = fixture();
        let peak = least_room(&measure, &measured);
        drop(measure);

        let (ctx, t) = fixture();
        let payload = ctx.decoded_bytes(t.cols[0][0]).unwrap();
        assert!(payload > 0);
        // the reservation for building the values fits, and the payload does not
        let storage = ctx.charge(ctx.memory_room() - (peak - 1)).unwrap();
        let before = ctx.live_bytes();
        assert!(
            cursor_variable_values(&ctx, &t, &Expr::Var(t.vars[0]))
                .unwrap()
                .is_none()
        );
        assert_eq!(ctx.live_bytes(), before, "the refusal kept its charge");
        assert!(ctx.check_memory_failure().is_ok());
        drop(storage);
        assert!(
            cursor_variable_values(&ctx, &t, &Expr::Var(t.vars[0]))
                .unwrap()
                .is_some()
        );
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

    #[test]
    fn volatile_extension_aliases_and_dynamic_calls_are_not_pure() {
        for (base, local, reason) in [
            (super::super::expr::AFN, "uuid", "UUID"),
            (super::super::expr::AFN, "struuid", "STRUUID"),
            (super::super::expr::AFN, "eval", "dynamic function"),
            (super::super::expr::FN, "apply", "dynamic function"),
            (super::super::expr::AFN, "wait", "side effects"),
            (super::super::expr::AFN, "print", "side effects"),
            (super::super::expr::AFN, "execTime", "side effects"),
        ] {
            let e = Expr::Call(Func::Ext(format!("{base}{local}")), vec![Expr::Var(1)]);
            assert_eq!(impurity(&e), Some(reason));
            assert!(eligible(&[&e]).is_err());
        }
        for (base, local) in [
            (super::super::expr::FN, "upper-case"),
            (super::super::expr::AFN, "strlen"),
        ] {
            let e = Expr::Call(Func::Ext(format!("{base}{local}")), vec![Expr::Var(1)]);
            assert_eq!(impurity(&e), None);
            assert_eq!(eligible(&[&e]).unwrap(), Some(1));
        }
    }

    fn cursor_with_late_longer_local_key() -> (std::sync::Arc<Ctx>, Table, [Id; 2]) {
        let store = crate::store::Store::in_memory(Default::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 16 << 20;
        let short = ctx.intern_value(&super::super::value::Value::Str("s".into()));
        let long = ctx.intern_value(&super::super::value::Value::Str("long".into()));
        assert_eq!((short.tag(), long.tag()), (Tag::Local, Tag::Local));
        let var = ctx.var("key");
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        let mut table = Table::new(vec![var]);
        table.cols[0] = vec![short; MIN_ROWS];
        table.cols[0][MIN_ROWS - 1] = long;
        table.len = MIN_ROWS;
        (ctx, table, [short, long])
    }

    #[test]
    fn a_refused_order_decode_leaves_the_cursor_healthy_for_a_smaller_trial() {
        // Measure actual admitted column/decode ownership in a separate real
        // cursor; do not duplicate the bulk decoder's estimate in the fixture.
        let (measure, measured_table, [short, long]) = cursor_with_late_longer_local_key();
        assert!(measure.value(short).is_some());
        let before_long = measure.live_bytes();
        assert!(measure.value(long).is_some());
        let increment = measure.live_bytes() - before_long;
        assert_eq!(increment, 24);
        let column_bytes = least_room(&measure, &measured_table);
        drop(measure);

        let (ctx, table, [short, _]) = cursor_with_late_longer_local_key();
        let input = ctx.charge(table.mem_bytes()).unwrap();
        assert!(ctx.value(short).is_some());
        let trial_storage = ctx.charge(ctx.memory_room() - column_bytes).unwrap();
        let before_trial = ctx.live_bytes();
        let result = cursor_variable_values(&ctx, &table, &Expr::Var(table.vars[0]));
        assert!(matches!(result, Err(crate::Error::BudgetExceeded(b))
            if b.kind == crate::BudgetKind::Memory && b.limit == 16 << 20 && b.requested == (16 << 20) + increment));
        assert_eq!(
            ctx.live_bytes(),
            before_trial,
            "refused column leaked trial ownership"
        );
        assert!(
            ctx.check_memory_failure().is_ok(),
            "recoverable ORDER decoding poisoned the cursor before its smaller trial"
        );
        drop(trial_storage);
        let values = cursor_variable_values(&ctx, &table, &Expr::Var(table.vars[0]))
            .unwrap()
            .unwrap();
        for row in 0..table.len {
            let expected = if row + 1 == table.len { "long" } else { "s" };
            assert_eq!(
                values.get(row).as_ref().unwrap().to_term(),
                oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(expected))
            );
        }
        drop(values);
        drop(input);
        assert!(ctx.check_memory_failure().is_ok());
        assert!(ctx.mem_peak() <= 16 << 20);
    }

    #[test]
    fn infallible_decode_failure_stays_sticky_after_other_storage_releases() {
        let (ctx, _, [short, long]) = cursor_with_late_longer_local_key();
        assert!(ctx.value(short).is_some());
        let storage = ctx.charge(ctx.memory_room() - 8).unwrap();
        assert!(ctx.value(long).is_none());
        assert!(
            matches!(ctx.check_memory_failure(), Err(crate::Error::BudgetExceeded(b))
            if b.kind == crate::BudgetKind::Memory)
        );
        drop(storage);
        assert!(
            matches!(ctx.check_memory_failure(), Err(crate::Error::BudgetExceeded(b))
            if b.kind == crate::BudgetKind::Memory)
        );
    }
}
