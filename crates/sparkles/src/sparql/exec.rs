//! Physical operator execution (column-at-a-time, fully materialized results).

use super::ctx::Ctx;
use super::expr::{Expr, Row, Val, ebv, eval};
use super::exprcache::Report as ExprReport;
use super::plan::{
    Agg, GraphFilter, JoinAlgo, Kind, Node, OrderedTopK, PathEnd, PathSpec, RangeSpec, ScanSpec,
};
use super::table::{Table, VarId};
use super::value::{NumOp, Value, arith, order_cmp};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{Block, O, P, Perm, S, pad};
use crate::store::Chunk;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use spargebra::algebra::AggregateFunction;
use std::cmp::Ordering;
use std::time::Instant;

const PAR_THRESHOLD: usize = 16_384;

/// Map `f` over rows `0..n` (in parallel when `par`) in chunks, checking cancellation and
/// the deadline between chunks: one clock read per chunk rather than per row, while an
/// expensive expression still stops within one chunk of the deadline.
fn map_rows<T: Send>(
    ctx: &Ctx,
    n: usize,
    par: bool,
    f: impl Fn(usize) -> T + Sync + Send,
) -> Result<Vec<T>> {
    let chunk = if par { 1 << 16 } else { 1 << 10 };
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    while start < n {
        ctx.check()?;
        let end = (start + chunk).min(n);
        if par {
            out.par_extend((start..end).into_par_iter().map(&f));
        } else {
            out.extend((start..end).map(&f));
        }
        start = end;
    }
    Ok(out)
}

/// Executed operator tree with runtime information (QLever `RuntimeInformation`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanInfo {
    pub operator: String,
    pub description: String,
    pub columns: Vec<String>,
    pub sorted_on: Vec<String>,
    pub estimated_rows: f64,
    pub estimated_cost: f64,
    pub actual_rows: i64,
    pub time_ms: f64,
    pub cached: bool,
    pub children: Vec<PlanInfo>,
    /// operator counters (spatial operators: candidates, refined, matched, …)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counters: Option<serde_json::Map<String, serde_json::Value>>,
    /// notes about the plan (root only)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<super::ctx::PlanWarning>,
}

fn names(ctx: &Ctx, vars: &[VarId]) -> Vec<String> {
    vars.iter()
        .map(|v| ctx.var_name(*v))
        .filter(|n| !n.starts_with(' '))
        .collect()
}

/// Plan description without execution (EXPLAIN).
pub fn describe(ctx: &Ctx, n: &Node) -> PlanInfo {
    PlanInfo {
        operator: n.operator().to_string(),
        description: n.desc.clone(),
        columns: names(ctx, &n.vars),
        sorted_on: names(ctx, &n.sorted),
        estimated_rows: n.est.round(),
        estimated_cost: n.cost.round(),
        actual_rows: -1,
        time_ms: 0.0,
        cached: false,
        children: n.children.iter().map(|c| describe(ctx, c)).collect(),
        counters: None,
        warnings: Vec::new(),
    }
}

/// Execute with the result cache: subtrees that are cheap to recompute (leaf scans and
/// value tables) bypass it; everything else is looked up and stored.
pub fn execute(ctx: &Ctx, n: &Node) -> Result<(Table, PlanInfo)> {
    ctx.check()?;
    let results = &ctx.snap.results;
    let cacheable = ctx.use_cache
        && results.enabled()
        && !matches!(
            n.kind,
            Kind::Scan(_)
                | Kind::RangeScan(..)
                | Kind::Values(_)
                | Kind::Empty
                | Kind::CountScan { .. }
                | Kind::CountDistinctScan { .. }
                | Kind::GroupCountScan { .. }
                | Kind::CountJoinRuns { .. }
        );
    // (spatial operators are cached: an index search is not cheap to repeat)
    let key = if cacheable {
        super::cache::key(n, ctx)
    } else {
        None
    };
    if let Some(k) = &key {
        static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DEBUG.get_or_init(|| std::env::var_os("SPARKLES_DEBUG_CACHE").is_some()) {
            eprintln!("CACHEKEY {} :: {}", n.operator(), k.key);
        }
        let start = Instant::now();
        if let Some(t) = results.get(k, ctx)? {
            let mut info = describe(ctx, n);
            info.actual_rows = t.len() as i64;
            info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
            info.cached = true;
            info.children.clear();
            return Ok((t, info));
        }
    }
    let (t, info) = execute_uncached(ctx, n)?;
    if let Some(k) = key {
        // only worth caching when recomputation is not trivially cheap
        if info.time_ms >= results.min_ms {
            results.put(k, &t, ctx);
        }
    }
    Ok((t, info))
}

fn execute_uncached(ctx: &Ctx, n: &Node) -> Result<(Table, PlanInfo)> {
    let start = Instant::now();
    let mut infos = Vec::new();
    // the children's tables count against the memory budget until this operator is done
    let held = ctx.charge(0)?;
    let child = |i: usize, infos: &mut Vec<PlanInfo>| -> Result<Table> {
        let (t, info) = execute(ctx, &n.children[i])?;
        infos.push(info);
        held.add(t.mem_bytes())?;
        Ok(t)
    };
    // runtime detail for EXPLAIN (which variant of the operator ran)
    let mut note: Option<String> = None;
    let mut counters = None;
    // expressions evaluated per distinct value
    let mut expr_report = ExprReport::default();
    let mut table = match &n.kind {
        Kind::Empty => Table::empty(n.vars.clone()),
        Kind::Values(t) => t.clone(),
        Kind::Scan(spec) => {
            note = column_note(ctx, spec);
            scan(ctx, spec, &n.vars)?
        }
        Kind::RangeScan(spec, range) => {
            note = column_note(ctx, spec);
            range_scan(ctx, spec, range, &n.vars)?
        }
        Kind::OrderedTopK(spec) => match ordered_topk(ctx, spec, &n.vars)? {
            Some((t, read)) => {
                note = Some(format!("[read {read} rows]"));
                t
            }
            None => {
                // the values are not totally ordered: the generic plan decides
                let (t, info) = execute(ctx, &spec.fallback)?;
                infos.push(info);
                note = Some("[values not totally ordered: ran the generic plan]".into());
                t
            }
        },
        Kind::GroupCountScan {
            spec,
            key: _,
            counts,
            metadata,
        } => {
            if *metadata {
                class_counts(ctx, &n.vars, counts.len())
            } else {
                group_count_scan(ctx, spec, &n.vars, counts.len())?
            }
        }
        Kind::CountJoinRuns { var } => {
            let mut sides = Vec::with_capacity(2);
            for c in &n.children {
                let t0 = Instant::now();
                let Kind::Scan(spec) = &c.kind else {
                    unreachable!("CountJoinRuns children are scans")
                };
                let runs = key_runs(ctx, spec)?;
                let mut info = describe(ctx, c);
                info.actual_rows = runs.0.len() as i64;
                info.time_ms = t0.elapsed().as_secs_f64() * 1000.0;
                infos.push(info);
                held.add(super::ctx::table_bytes(runs.0.len(), 2))?;
                sides.push(runs);
            }
            let ((lk, lc), (rk, rc)) = (&sides[0], &sides[1]);
            let (mut i, mut j, mut total) = (0, 0, 0u64);
            while i < lk.len() && j < rk.len() {
                match lk[i].cmp(&rk[j]) {
                    Ordering::Less => i += 1,
                    Ordering::Greater => j += 1,
                    Ordering::Equal => {
                        total = total.saturating_add(lc[i].saturating_mul(rc[j]));
                        i += 1;
                        j += 1;
                    }
                }
            }
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(total.min(i64::MAX as u64) as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::CountScan { spec, var } => {
            let c = ctx.snap.count(spec.perm, &spec.prefix)?;
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::Join { algo, keys } => {
            let l = child(0, &mut infos)?;
            if l.is_empty() {
                infos.push(describe(ctx, &n.children[1]));
                Table::empty(n.vars.clone())
            } else {
                let r = child(1, &mut infos)?;
                match algo {
                    JoinAlgo::Cross => cross(ctx, &l, &r)?,
                    JoinAlgo::Merge => join_tables(ctx, &l, &r, keys, true)?,
                    JoinAlgo::Hash => join_tables(ctx, &l, &r, keys, false)?,
                }
            }
        }
        Kind::CountDistinctScan {
            spec,
            var,
            metadata,
        } => {
            let c = match metadata {
                Some(c) => *c,
                None => count_distinct_scan(ctx, spec)?,
            };
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::CountJoin { algo, var } => {
            let l = child(0, &mut infos)?;
            let r = if l.is_empty() {
                infos.push(describe(ctx, &n.children[1]));
                Table::empty(n.children[1].vars.clone())
            } else {
                child(1, &mut infos)?
            };
            let c = if l.is_empty() || r.is_empty() {
                0
            } else {
                join_count(ctx, &l, &r, *algo == JoinAlgo::Merge)?
            };
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::Unpack { t, parts } => {
            let input = child(0, &mut infos)?;
            unpack(ctx, input, *t, parts, &n.vars)?
        }
        Kind::IndexJoin(spec) => {
            let l = child(0, &mut infos)?;
            let (t, stats) = super::indexjoin::run(ctx, spec, &l, &n.vars)?;
            note = Some(stats.note());
            counters = Some(stats.counters());
            t
        }
        Kind::LeftJoin { expr } => {
            let l = child(0, &mut infos)?;
            let r = child(1, &mut infos)?;
            left_join(ctx, &l, &r, expr.as_ref())?
        }
        Kind::Minus => {
            let l = child(0, &mut infos)?;
            let r = child(1, &mut infos)?;
            minus(ctx, l, &r, &mut note)?
        }
        Kind::Union => {
            let mut out = Table::new(n.vars.clone());
            for i in 0..n.children.len() {
                out.append(child(i, &mut infos)?);
                ctx.check_output(out.len(), out.width())?;
            }
            out
        }
        Kind::Filter(es) => {
            let mut t = child(0, &mut infos)?;
            expr_report = apply_filter(ctx, &mut t, es)?;
            (note, counters) = super::exists::explain(ctx, es);
            t
        }
        Kind::Extend(v, e) => {
            let mut t = child(0, &mut infos)?;
            ctx.check_output(t.len(), 1)?;
            let col = compute_column(ctx, &t, e, &mut expr_report)?;
            t.vars.push(*v);
            t.cols.push(col);
            t
        }
        Kind::Sort(vars) => {
            let mut t = child(0, &mut infos)?;
            // sorting copies the rows
            ctx.check_output(t.len(), t.width())?;
            t.sort_by_vars(vars);
            t
        }
        Kind::OrderBy { keys, limit } => {
            let t = child(0, &mut infos)?;
            let (t, pre) = order_by(ctx, t, keys, *limit, &mut expr_report)?;
            if let Some(kept) = pre {
                note = Some(format!("[numeric prefilter kept {kept} rows]"));
            }
            t
        }
        Kind::Project(vars) => child(0, &mut infos)?.project(vars),
        Kind::Distinct => distinct(child(0, &mut infos)?),
        Kind::Slice {
            offset,
            limit: Some(limit),
        } => {
            // LIMIT without ORDER BY: any `offset + limit` solutions will do, so the
            // input is computed with early termination
            let (t, info, _) = execute_limited(ctx, &n.children[0], offset.saturating_add(*limit))?;
            infos.push(info);
            held.add(t.mem_bytes())?;
            t.slice(*offset, Some(*limit))
        }
        Kind::Slice { offset, limit } => child(0, &mut infos)?.slice(*offset, *limit),
        Kind::Group { keys, aggs } => {
            let t = child(0, &mut infos)?;
            group(ctx, &t, keys, aggs, &mut expr_report)?
        }
        Kind::Path {
            spec,
            bound_from_left,
        } => {
            let mut inputs = Vec::new();
            for i in 0..n.children.len() {
                inputs.push(child(i, &mut infos)?);
            }
            let (t, sweeps) = path(ctx, spec, *bound_from_left, inputs, &n.vars)?;
            if sweeps > 0 {
                note = Some(format!(
                    "[{sweeps} frontier levels expanded by index sweeps]"
                ));
            }
            t
        }
        Kind::TextSearch(spec) => crate::text::search(ctx, spec, &n.vars)?,
        Kind::VectorSearch(spec) => vector_search(ctx, spec, &n.vars)?,
        Kind::SpatialScan(spec) => {
            let (t, c) = spatial_scan(ctx, spec, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialPf(spec) => {
            let (t, c) = spatial_pf(ctx, spec, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialJoin(spec) => {
            let mut inputs = Vec::with_capacity(n.children.len());
            for i in 0..n.children.len() {
                inputs.push(child(i, &mut infos)?);
            }
            let (t, c) = spatial_join(ctx, spec, inputs, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialKnn(spec) => {
            // the template runs once per batch of candidates
            let (t, c, runs) = spatial_knn(ctx, spec, &n.children[0], &n.vars)?;
            infos.extend(runs);
            counters = Some(c);
            t
        }
        Kind::SpatialRelate(spec) => {
            let (t, c) = spatial_relate(ctx, spec, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::Service {
            endpoint,
            query,
            silent,
        } => match service(ctx, endpoint, query, &n.vars) {
            Ok(t) => t,
            // SILENT hides failures of the remote service, not a refusal or a spent
            // budget
            Err(e @ (Error::NotPermitted(_) | Error::BudgetExceeded(_))) => return Err(e),
            Err(_) if *silent => Table::unit(),
            Err(e) => return Err(e),
        },
    };
    if !matches!(
        n.kind,
        Kind::Scan(_)
            | Kind::RangeScan(..)
            | Kind::Sort(_)
            | Kind::Join {
                algo: JoinAlgo::Merge,
                ..
            }
    ) {
        if !table.sorted.is_empty() && table.sorted != n.sorted {
            table.sorted.clear();
        }
    } else {
        table.sorted = n.sorted.clone();
    }
    if let Some(x) = expr_report.note() {
        note = Some(note.map_or(x.clone(), |n| format!("{n} {x}")));
        counters = merge_counters(counters, expr_report.counters());
    }
    ctx.check()?;
    // the inputs are gone (or became the output): only the output is alive now
    drop(held);
    ctx.check_output(table.len(), table.width())?;
    let info = PlanInfo {
        operator: n.operator().to_string(),
        description: match note {
            Some(x) => format!("{} {x}", n.desc),
            None => n.desc.clone(),
        },
        columns: names(ctx, &n.vars),
        sorted_on: names(ctx, &n.sorted),
        estimated_rows: n.est.round(),
        estimated_cost: n.cost.round(),
        actual_rows: table.len() as i64,
        time_ms: start.elapsed().as_secs_f64() * 1000.0,
        cached: false,
        children: infos,
        counters,
        warnings: Vec::new(),
    };
    Ok((table, info))
}

/// Both operators' counters (the expression cache's next to those of EXISTS).
fn merge_counters(
    a: Option<serde_json::Map<String, serde_json::Value>>,
    b: Option<serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    match (a, b) {
        (Some(mut a), Some(b)) => {
            a.extend(b);
            Some(a)
        }
        (a, b) => a.or(b),
    }
}

// ------------------------------------------------------------------ scans ------

/// Key columns a scan reads: its variables, the graph (unless every graph passes) and
/// the repeated-variable columns. The others are never decoded for whole blocks.
fn scan_mask(ctx: &Ctx, spec: &ScanSpec) -> crate::index::ColMask {
    if !ctx.opt.selective_columns {
        return crate::index::ALL_COLS;
    }
    let mut m: crate::index::ColMask = 0;
    for &(c, _) in &spec.cols {
        m |= 1 << c;
    }
    if !matches!(spec.graph, GraphFilter::All) {
        m |= 1 << spec.graph_col;
    }
    for &(a, b) in &spec.eqs {
        m |= (1 << a) | (1 << b);
    }
    m
}

/// EXPLAIN note naming the key columns a scan decodes, when it skips any.
fn column_note(ctx: &Ctx, spec: &ScanSpec) -> Option<String> {
    let m = scan_mask(ctx, spec);
    (m != crate::index::ALL_COLS).then(|| {
        let names: Vec<&str> = (0..4)
            .filter(|c| m & (1 << c) != 0)
            .map(|c| ["S", "P", "O", "G"][spec.perm.order()[c]])
            .collect();
        format!("[decodes {}]", names.join(""))
    })
}

/// Whether every row of `b[s..e]` passes the scan's graph filter and repeated-variable
/// checks, so the slice can be taken column-wise (for a default-graph query over a store
/// with few or no named graphs, this is one pass over the graph column).
fn block_passes(spec: &ScanSpec, b: &Block, s: usize, e: usize) -> bool {
    spec.eqs.is_empty()
        && (matches!(spec.graph, GraphFilter::All)
            || b.cols[spec.graph_col][s..e]
                .iter()
                .all(|&g| spec.graph.accepts(g)))
}

/// Per-class subject counts from the index statistics (admitted by the planner only when
/// they are exact), in class id order like the index runs.
fn class_counts(ctx: &Ctx, vars: &[VarId], naggs: usize) -> Table {
    let mut classes = ctx.snap.generation.stats.classes.clone();
    classes.sort_unstable();
    let mut t = Table::new(vars.to_vec());
    t.cols[0] = classes.iter().map(|&(c, _)| Id(c)).collect();
    let cnt: Vec<Id> = classes
        .iter()
        .map(|&(_, n)| Id::from_i64(n as i64).unwrap_or(Id::UNDEF))
        .collect();
    for a in 0..naggs {
        t.cols[1 + a] = cnt.clone();
    }
    t.len = classes.len();
    t
}

/// Count runs of the first free key column of a scan (graph filter / repeated variables /
/// union-graph dedup applied row by row; plain blocks counted directly).
fn group_count_scan(ctx: &Ctx, spec: &ScanSpec, vars: &[VarId], naggs: usize) -> Result<Table> {
    let (keys, counts) = key_runs(ctx, spec)?;
    let mut t = Table::new(vars.to_vec());
    let cnt: Vec<Id> = counts
        .iter()
        .map(|&c| Id::from_i64(c as i64).unwrap_or(Id::UNDEF))
        .collect();
    t.len = keys.len();
    t.cols[0] = keys;
    for a in 0..naggs {
        t.cols[1 + a] = cnt.clone();
    }
    Ok(t)
}

/// Length of the run of `v` at the start of the sorted slice `col`: a short linear probe
/// (most runs are short), then binary search for long runs.
#[inline]
fn run_len(col: &[u64], v: u64) -> usize {
    const PROBE: usize = 16;
    let end = col.len().min(PROBE);
    match col[..end].iter().position(|x| *x != v) {
        Some(n) => n,
        None if end < col.len() => end + col[end..].partition_point(|x| *x == v),
        None => end,
    }
}

/// The distinct values of a scan's first free key column with the number of rows of each,
/// in key order.
fn key_runs(ctx: &Ctx, spec: &ScanSpec) -> Result<(Vec<Id>, Vec<u64>)> {
    let kc = spec.cols[0].0;
    let kcs: Vec<usize> = spec.cols.iter().map(|(k, _)| *k).collect();
    let mut keys: Vec<Id> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let runs = std::cell::Cell::new(0usize);
    let mut bump = |k: u64, n: u64| {
        if keys.last() == Some(&Id(k)) {
            *counts.last_mut().unwrap() += n;
        } else {
            keys.push(Id(k));
            counts.push(n);
            runs.set(keys.len());
        }
    };
    let mut last: Option<[u64; 4]> = None;
    let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, scan_mask(ctx, spec), |chunk| {
            let mut row = |k: &[u64; 4]| {
                if !spec.graph.accepts(k[spec.graph_col])
                    || spec.eqs.iter().any(|&(a, b)| k[a] != k[b])
                {
                    return;
                }
                if spec.dedup {
                    let mut proj = [0u64; 4];
                    for (i, &c) in kcs.iter().enumerate() {
                        proj[i] = k[c];
                    }
                    if last == Some(proj) {
                        return;
                    }
                    last = Some(proj);
                }
                bump(k[kc], 1);
            };
            match chunk {
                Chunk::Block(b, s, e) if !spec.dedup && block_passes(spec, b, s, e) => {
                    let col = &b.cols[kc][s..e];
                    let mut i = 0;
                    while i < col.len() {
                        let v = col[i];
                        let run = run_len(&col[i..], v);
                        bump(v, run as u64);
                        i += run;
                    }
                }
                Chunk::Block(b, s, e) => (s..e).for_each(|i| row(&b.key(i))),
                Chunk::Row(k) => row(&k),
            }
            ctx.check()?;
            // a key and a count per run
            ctx.check_output(runs.get(), 2)?;
            Ok(true)
        })?;
    Ok((keys, counts))
}

/// Number of distinct values in the first free key column of a scan sorted on it: the
/// runs of equal ids. Whole blocks are compared column-wise; rows are checked one by one
/// only when a graph filter or repeated variables apply (duplicates across graphs fall
/// into the same run, so union-graph dedup needs nothing extra).
fn count_distinct_scan(ctx: &Ctx, spec: &ScanSpec) -> Result<u64> {
    // new runs in `col` given the last value seen before it
    fn runs(col: &[u64], last: &mut Option<u64>) -> u64 {
        let Some((&first, _)) = col.split_first() else {
            return 0;
        };
        let n =
            (*last != Some(first)) as u64 + col.windows(2).filter(|w| w[0] != w[1]).count() as u64;
        *last = col.last().copied();
        n
    }
    let kc = spec.cols[0].0;
    let mut n = 0u64;
    let mut last: Option<u64> = None;
    let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, scan_mask(ctx, spec), |chunk| {
            let mut row = |k: &[u64; 4]| {
                if spec.graph.accepts(k[spec.graph_col])
                    && spec.eqs.iter().all(|&(a, b)| k[a] == k[b])
                {
                    n += runs(&[k[kc]], &mut last);
                }
            };
            match chunk {
                Chunk::Block(b, s, e) if block_passes(spec, b, s, e) => {
                    n += runs(&b.cols[kc][s..e], &mut last)
                }
                Chunk::Block(b, s, e) => (s..e).for_each(|i| row(&b.key(i))),
                Chunk::Row(k) => row(&k),
            }
            ctx.check()?;
            Ok(true)
        })?;
    Ok(n)
}

fn scan(ctx: &Ctx, spec: &ScanSpec, vars: &[VarId]) -> Result<Table> {
    Ok(scan_limited(ctx, spec, vars, None)?.0)
}

/// Apply a row-preserving / row-reducing unary operator to its input.
fn apply_unary(ctx: &Ctx, n: &Node, mut t: Table, report: &mut ExprReport) -> Result<Table> {
    Ok(match &n.kind {
        Kind::Filter(exprs) => {
            report.extend(apply_filter(ctx, &mut t, exprs)?);
            t
        }
        Kind::Extend(v, e) => {
            let col = compute_column(ctx, &t, e, report)?;
            t.vars.push(*v);
            t.cols.push(col);
            t
        }
        Kind::Project(vars) => t.project(vars),
        Kind::Unpack { t: tv, parts } => unpack(ctx, t, *tv, parts, &n.vars)?,
        Kind::Distinct => distinct(t),
        Kind::IndexJoin(spec) => super::indexjoin::run(ctx, spec, &t, &n.vars)?.0,
        _ => unreachable!("not a unary streaming operator"),
    })
}

/// Execute `n` so that it produces at least `want` solutions if it has that many; any
/// subset of the solutions is acceptable (LIMIT without ORDER BY, ASK, EXISTS). Scans stop
/// early; operators whose output over a prefix of their input is a subset of their full
/// output re-run with a geometrically growing input budget until they have enough rows.
/// Returns `(table, info, complete)`; `complete` means no further rows exist.
fn execute_limited(ctx: &Ctx, n: &Node, want: usize) -> Result<(Table, PlanInfo, bool)> {
    ctx.check()?;
    let start = Instant::now();
    let finish = |t: Table, children: Vec<PlanInfo>, complete: bool| {
        let mut info = describe(ctx, n);
        info.children = children;
        info.actual_rows = t.len() as i64;
        info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
        if !complete {
            info.description = format!("{} [stopped early]", info.description);
        }
        Ok((t, info, complete))
    };
    match &n.kind {
        Kind::Scan(spec) => {
            let (t, truncated) = scan_limited(ctx, spec, &n.vars, Some(want))?;
            finish(t, Vec::new(), !truncated)
        }
        Kind::Filter(_)
        | Kind::Extend(..)
        | Kind::Project(_)
        | Kind::Unpack { .. }
        | Kind::Distinct
        | Kind::IndexJoin(_) => {
            let mut budget = want.max(64);
            loop {
                let (input, cinfo, complete) = execute_limited(ctx, &n.children[0], budget)?;
                let mut report = ExprReport::default();
                let out = apply_unary(ctx, n, input, &mut report)?;
                if out.len() >= want || complete {
                    let (t, mut info, complete) = finish(out, vec![cinfo], complete)?;
                    if let Kind::Filter(exprs) = &n.kind {
                        super::exists::annotate(ctx, &mut info, exprs);
                    }
                    if let Some(x) = report.note() {
                        info.description = format!("{} {x}", info.description);
                        info.counters = merge_counters(info.counters.take(), report.counters());
                    }
                    return Ok((t, info, complete));
                }
                budget = budget.saturating_mul(8);
            }
        }
        Kind::Join { algo, .. } => {
            // limit the (estimated) larger side, compute the other one fully
            let lim = if n.children[0].est >= n.children[1].est {
                0
            } else {
                1
            };
            let (other, oinfo) = execute(ctx, &n.children[1 - lim])?;
            let _held = ctx.charge(other.mem_bytes())?;
            if other.is_empty() {
                let mut e = Table::empty(n.vars.clone());
                e.sorted.clear();
                return finish(e, vec![describe(ctx, &n.children[lim]), oinfo], true);
            }
            let mut budget = want.max(64);
            loop {
                let (part, pinfo, complete) = execute_limited(ctx, &n.children[lim], budget)?;
                let (l, r) = if lim == 0 {
                    (&part, &other)
                } else {
                    (&other, &part)
                };
                let out = match algo {
                    JoinAlgo::Cross => cross(ctx, l, r)?,
                    JoinAlgo::Merge => join_tables(ctx, l, r, &[], true)?,
                    JoinAlgo::Hash => join_tables(ctx, l, r, &[], false)?,
                };
                if out.len() >= want || complete {
                    let infos = if lim == 0 {
                        vec![pinfo, oinfo]
                    } else {
                        vec![oinfo, pinfo]
                    };
                    return finish(out, infos, complete);
                }
                budget = budget.saturating_mul(8);
            }
        }
        Kind::Union => {
            let mut out = Table::new(n.vars.clone());
            let mut infos = Vec::new();
            let mut complete = true;
            for (i, c) in n.children.iter().enumerate() {
                if out.len() >= want {
                    complete = false;
                    infos.extend(n.children[i..].iter().map(|c| describe(ctx, c)));
                    break;
                }
                let (t, info, c_complete) = execute_limited(ctx, c, want - out.len())?;
                complete &= c_complete;
                infos.push(info);
                out.append(t);
                ctx.check_output(out.len(), out.width())?;
            }
            finish(out, infos, complete)
        }
        Kind::Slice {
            offset,
            limit: Some(limit),
        } => {
            let (t, info, _) = execute_limited(ctx, &n.children[0], offset.saturating_add(*limit))?;
            // the slice's output does not grow with a larger budget
            finish(t.slice(*offset, Some(*limit)), vec![info], true)
        }
        _ => {
            let (t, info) = execute(ctx, n)?;
            Ok((t, info, true))
        }
    }
}

/// Scan with an optional row limit; returns `(table, truncated)`.
fn scan_limited(
    ctx: &Ctx,
    spec: &ScanSpec,
    vars: &[VarId],
    limit: Option<usize>,
) -> Result<(Table, bool)> {
    let mut t = Table::new(vars.to_vec());
    // the exact number of rows under the prefix (at most two block decodes, and those
    // blocks are read by the scan anyway) bounds the output: reserve it once instead of
    // growing every column by repeated doubling
    let bound = ctx.snap.count(spec.perm, &spec.prefix)?;
    let cap = usize::try_from(bound)
        .unwrap_or(usize::MAX)
        .min(limit.unwrap_or(usize::MAX))
        .min(ctx.max_rows.saturating_add(1))
        .min(ctx.rows_within_budget(vars.len()).saturating_add(1));
    for c in &mut t.cols {
        c.reserve_exact(cap);
    }
    let truncated = scan_into(
        ctx,
        spec,
        pad(&spec.prefix, 0),
        pad(&spec.prefix, u64::MAX),
        limit,
        &mut t,
    )?;
    Ok((t, truncated))
}

/// A scan restricted to id ranges of its first free column: exact ranges are copied,
/// the others filtered. The output keeps the scan's order.
fn range_scan(ctx: &Ctx, spec: &ScanSpec, range: &RangeSpec, vars: &[VarId]) -> Result<Table> {
    let mut t = Table::new(vars.to_vec());
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&spec.prefix, fill);
        k[spec.prefix.len()] = v;
        k
    };
    for r in &range.ranges {
        let (lo, hi) = (bound(r.lo, 0), bound(r.hi, u64::MAX));
        if r.exact {
            scan_into(ctx, spec, lo, hi, None, &mut t)?;
        } else {
            let mut part = Table::new(vars.to_vec());
            scan_into(ctx, spec, lo, hi, None, &mut part)?;
            if !part.is_empty() {
                apply_filter(ctx, &mut part, &range.filter)?;
                t.append(part);
            }
        }
        ctx.check_output(t.len(), t.width())?;
    }
    Ok(t)
}

// ---------------------------------------------------------- ordered top-k ------

/// The unread part `[lo, hi]` of a monotone piece of an [`OrderedTopK`] scan.
struct TopKRun {
    lo: u64,
    hi: u64,
    exact: bool,
    /// the best values are at the high end of the id range
    best_high: bool,
    /// the rows read so far that can still be among the first k
    kept: Table,
    /// base rows to read in the next step (grows geometrically)
    want: usize,
    /// no unread row of the piece can be among the first k
    done: bool,
}

/// Rows of an [`OrderedTopK`] scan whose order-column ids lie in `[lo, hi]`, in key order,
/// with the filters applied; also returns how many rows were scanned.
fn topk_read(
    ctx: &Ctx,
    spec: &OrderedTopK,
    lo: u64,
    hi: u64,
    exact: bool,
    vars: &[VarId],
) -> Result<(Table, usize)> {
    let scan = &spec.scan;
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[scan.prefix.len()] = v;
        k
    };
    let mut t = Table::new(vars.to_vec());
    scan_into(ctx, scan, bound(lo, 0), bound(hi, u64::MAX), None, &mut t)?;
    let scanned = t.len();
    if !exact && !spec.range_filter.is_empty() && !t.is_empty() {
        apply_filter(ctx, &mut t, &spec.range_filter)?;
    }
    if !spec.filter.is_empty() && !t.is_empty() {
        apply_filter(ctx, &mut t, &spec.filter)?;
    }
    Ok((t, scanned))
}

/// The order-column id of the base row `want` rows from the best end of the ids
/// `[lo, hi]` of an [`OrderedTopK`] scan (`lo` or `hi` when there are fewer). Only the
/// leading key columns of the blocks at that end are decoded.
fn topk_boundary(
    ctx: &Ctx,
    spec: &OrderedTopK,
    lo: u64,
    hi: u64,
    best_high: bool,
    mut want: usize,
) -> Result<u64> {
    let scan = &spec.scan;
    let c = scan.prefix.len();
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[c] = v;
        k
    };
    let (klo, khi) = (bound(lo, 0), bound(hi, u64::MAX));
    let perm = ctx.snap.perm(scan.perm);
    let (b0, b1) = perm.key_block_range(&klo, &khi);
    let mask = crate::index::bound_cols(&klo, &khi);
    let mut visit = |b: usize| -> Result<Option<u64>> {
        let blk = ctx.snap.cache.get_cols(perm, b, mask)?;
        let (s, e) = blk.key_range(&klo, &khi);
        if e - s >= want {
            let col = &blk.cols[c];
            return Ok(Some(if best_high {
                col[e - want]
            } else {
                col[s + want - 1]
            }));
        }
        want -= e - s;
        Ok(None)
    };
    if best_high {
        for b in (b0..b1).rev() {
            if let Some(id) = visit(b)? {
                return Ok(id);
            }
        }
        Ok(lo)
    } else {
        for b in b0..b1 {
            if let Some(id) = visit(b)? {
                return Ok(id);
            }
        }
        Ok(hi)
    }
}

/// Read the next rows of a monotone piece from its best end, keeping those that can
/// still be among the first k; returns the rows scanned.
fn topk_step(ctx: &Ctx, spec: &OrderedTopK, run: &mut TopKRun, vars: &[VarId]) -> Result<usize> {
    // base rows only: delta rows in between are merged by the scan
    let edge = topk_boundary(ctx, spec, run.lo, run.hi, run.best_high, run.want)?;
    let (a, b) = if run.best_high {
        (edge, run.hi)
    } else {
        (run.lo, edge)
    };
    let (mut part, scanned) = topk_read(ctx, spec, a, b, run.exact, vars)?;
    if run.best_high {
        run.done |= a == run.lo;
        run.hi = a.saturating_sub(1);
    } else {
        run.done |= b == run.hi;
        run.lo = b.saturating_add(1);
    }
    run.want = run.want.saturating_mul(4);
    // the rows come in id order, and earlier steps read only better ids: the `need`-th
    // best row of this step and its ties complete the piece's best k, and every other
    // row of the piece has k rows with strictly better values ahead of it (distinct ids
    // of a monotone piece are distinct numbers)
    let need = spec.k - run.kept.len();
    if part.len() >= need {
        let col = &part.cols[0];
        let keep: Vec<bool> = if run.best_high {
            let cut = col[part.len() - need];
            col.iter().map(|&id| id >= cut).collect()
        } else {
            let cut = col[need - 1];
            col.iter().map(|&id| id <= cut).collect()
        };
        part.filter_rows(&keep);
        run.done = true;
    }
    run.kept.append(part);
    Ok(scanned)
}

/// Whether ORDER BY's comparison of `id` with other values is a total order (it is not
/// for NaN, which compares with numbers by its lexical form, nor for partially ordered
/// dates, times and durations).
fn totally_ordered(ctx: &Ctx, id: Id) -> bool {
    use crate::id::Tag;
    match id.tag() {
        Tag::Double => !id.as_f64().is_nan(),
        Tag::DateTime | Tag::Date => false,
        Tag::Vocab | Tag::Delta => match ctx.value(id) {
            Some(Value::Double(d)) => !d.is_nan(),
            Some(Value::Float(f)) => !f.is_nan(),
            Some(
                Value::DateTime(_)
                | Value::Date(_)
                | Value::Time(_)
                | Value::Duration(_)
                | Value::YearMonth(_)
                | Value::DayTime(_)
                | Value::Triple(_),
            ) => false,
            _ => true,
        },
        _ => true,
    }
}

/// [`OrderedTopK`]: the first k rows of the ORDER BY, in its order; `None` when a value
/// outside the monotone pieces is not totally ordered (the generic plan must decide).
/// Also returns how many rows were scanned.
fn ordered_topk(ctx: &Ctx, spec: &OrderedTopK, out: &[VarId]) -> Result<Option<(Table, usize)>> {
    let vars: Vec<VarId> = spec.scan.cols.iter().map(|c| c.1).collect();
    debug_assert_eq!(vars.first(), Some(&spec.var));
    let mut scanned = 0;
    // the pieces without value order are read whole
    let mut rest = Table::new(vars.clone());
    let mut runs = Vec::new();
    for p in &spec.pieces {
        match p.mono {
            None => {
                let (t, n) = topk_read(ctx, spec, p.lo, p.hi, p.exact, &vars)?;
                scanned += n;
                rest.append(t);
                ctx.check_output(rest.len(), rest.width())?;
            }
            Some(up) => runs.push(TopKRun {
                lo: p.lo,
                hi: p.hi,
                exact: p.exact,
                best_high: up != spec.asc,
                kept: Table::new(vars.clone()),
                want: spec.k.max(16) * 2,
                done: false,
            }),
        }
    }
    let total = |id: &Id| totally_ordered(ctx, *id);
    if !(if rest.len() > PAR_THRESHOLD / 4 {
        rest.cols[0].par_iter().all(total)
    } else {
        rest.cols[0].iter().all(total)
    }) {
        return Ok(None);
    }
    let keys = [(Expr::Var(spec.var), spec.asc)];
    // only their first k can be among the first k of all rows (the order of two rows
    // does not depend on the others)
    if rest.len() > spec.k {
        rest = order_by(ctx, rest, &keys, Some(spec.k), &mut ExprReport::default())?.0;
    }
    let held = ctx.charge(rest.mem_bytes())?;
    let better = if spec.asc {
        Ordering::Less
    } else {
        Ordering::Greater
    };
    loop {
        ctx.check()?;
        for run in runs.iter_mut().filter(|r| !r.done) {
            scanned += topk_step(ctx, spec, run, &vars)?;
        }
        // the candidates in the generic plan's row order, ranked by the generic ORDER BY
        let n = rest.len() + runs.iter().map(|r| r.kept.len()).sum::<usize>();
        ctx.check_output(n, vars.len())?;
        let mut cand = rest.clone();
        for run in &runs {
            cand.append(run.kept.clone());
        }
        cand.sorted.clear();
        cand.sort_by_vars(&spec.tie_order);
        let (top, _) = order_by(ctx, cand, &keys, Some(spec.k), &mut ExprReport::default())?;
        if top.len() == spec.k {
            // a piece is finished once the k-th row is strictly better than its best
            // unread value, and so than every unread value
            let kth = ctx.value(top.cols[0][spec.k - 1]);
            for run in runs.iter_mut().filter(|r| !r.done) {
                let best = Id(if run.best_high { run.hi } else { run.lo });
                if order_cmp(kth.as_ref(), ctx.value(best).as_ref()) == better {
                    run.done = true;
                }
            }
        }
        if runs.iter().all(|r| r.done) {
            drop(held);
            return Ok(Some((top.project(out), scanned)));
        }
    }
}

/// Append the scan's rows with keys in `[lo, hi]` to `t`, stopping after `limit` rows in
/// total; returns whether rows may remain.
fn scan_into(
    ctx: &Ctx,
    spec: &ScanSpec,
    lo: crate::index::Key,
    hi: crate::index::Key,
    limit: Option<usize>,
    t: &mut Table,
) -> Result<bool> {
    let limit = limit.unwrap_or(usize::MAX);
    let mut truncated = false;
    let kcs: Vec<usize> = spec.cols.iter().map(|(k, _)| *k).collect();
    let mut last: Option<[u64; 4]> = None;
    let mut n = 0usize;
    let mut row = |k: &[u64; 4], t: &mut Table| {
        if !spec.graph.accepts(k[spec.graph_col]) {
            return;
        }
        if spec.eqs.iter().any(|&(a, b)| k[a] != k[b]) {
            return;
        }
        if spec.dedup {
            let mut proj = [0u64; 4];
            for (i, &kc) in kcs.iter().enumerate() {
                proj[i] = k[kc];
            }
            if last == Some(proj) {
                return;
            }
            last = Some(proj);
        }
        for (c, &kc) in kcs.iter().enumerate() {
            t.cols[c].push(Id(k[kc]));
        }
        t.len += 1;
    };
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, scan_mask(ctx, spec), |chunk| {
            if t.len >= limit {
                truncated = true;
                return Ok(false);
            }
            match chunk {
                Chunk::Block(b, s, e) if !spec.dedup && block_passes(spec, b, s, e) => {
                    let e = e.min(s.saturating_add(limit - t.len));
                    for (c, &kc) in kcs.iter().enumerate() {
                        t.cols[c].extend(b.cols[kc][s..e].iter().map(|&x| Id(x)));
                    }
                    t.len += e - s;
                    n += e - s;
                }
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        row(&b.key(i), t);
                    }
                    n += e - s;
                }
                Chunk::Row(k) => {
                    row(&k, t);
                    n += 1;
                }
            }
            if n > 1 << 16 {
                n = 0;
                ctx.check()?;
                ctx.check_output(t.len(), t.width())?;
            }
            if t.len >= limit {
                // there may be more matching rows after this point
                truncated = true;
                return Ok(false);
            }
            Ok(true)
        })?;
    Ok(truncated)
}

// ------------------------------------------------------------------ joins ------

/// Output layout of a join: left vars, then right-only vars.
struct JoinLayout {
    vars: Vec<VarId>,
    /// for each shared var: (left col, right col)
    shared: Vec<(usize, usize)>,
    /// right-only columns
    right_only: Vec<usize>,
}

fn layout(l: &Table, r: &Table) -> JoinLayout {
    let mut vars = l.vars.clone();
    let mut shared = Vec::new();
    let mut right_only = Vec::new();
    for (rc, v) in r.vars.iter().enumerate() {
        match l.col_of(*v) {
            Some(lc) => shared.push((lc, rc)),
            None => {
                vars.push(*v);
                right_only.push(rc);
            }
        }
    }
    JoinLayout {
        vars,
        shared,
        right_only,
    }
}

#[inline]
fn compatible(l: &Table, r: &Table, i: usize, j: usize, shared: &[(usize, usize)]) -> bool {
    shared.iter().all(|&(lc, rc)| {
        let (a, b) = (l.cols[lc][i], r.cols[rc][j]);
        a.is_undef() || b.is_undef() || a == b
    })
}

fn materialize(l: &Table, r: &Table, lay: &JoinLayout, pairs: &[(u32, u32)]) -> Table {
    let mut cols: Vec<Vec<Id>> = Vec::with_capacity(lay.vars.len());
    for (lc, col) in l.cols.iter().enumerate() {
        let fill = lay.shared.iter().find(|(x, _)| *x == lc).map(|(_, rc)| *rc);
        cols.push(
            pairs
                .iter()
                .map(|&(i, j)| {
                    let v = col[i as usize];
                    match (v.is_undef(), fill) {
                        (true, Some(rc)) => r.cols[rc][j as usize],
                        _ => v,
                    }
                })
                .collect(),
        );
    }
    for &rc in &lay.right_only {
        cols.push(pairs.iter().map(|&(_, j)| r.cols[rc][j as usize]).collect());
    }
    Table {
        vars: lay.vars.clone(),
        cols,
        len: pairs.len(),
        sorted: Vec::new(),
    }
}

fn has_undef(t: &Table, c: usize) -> bool {
    t.cols[c].iter().any(|v| v.is_undef())
}

/// Exponential + binary search for the first index `>= target` starting at `from`.
#[inline]
fn gallop(col: &[Id], from: usize, target: Id) -> usize {
    let mut step = 1;
    let mut hi = from;
    while hi < col.len() && col[hi] < target {
        hi = from + step;
        step *= 2;
    }
    let lo = from + step / 4;
    let hi = hi.min(col.len());
    lo.min(hi) + col[lo.min(hi)..hi].partition_point(|x| *x < target)
}

/// Pairs of compatible rows (inner join).
fn join_pairs(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    lay: &JoinLayout,
    merge: bool,
) -> Result<Vec<(u32, u32)>> {
    let mut pairs: Vec<(u32, u32)> = Vec::new();
    // a pair per output row now, the materialized row later
    let w = lay.vars.len() + 1;
    if lay.shared.is_empty() {
        // the product size is known up front: reject it before allocating anything
        ctx.check_output(l.len().saturating_mul(r.len()), w)?;
        for i in 0..l.len() {
            if i % 1024 == 0 {
                ctx.check()?;
            }
            for j in 0..r.len() {
                pairs.push((i as u32, j as u32));
            }
            ctx.check_output(pairs.len(), w)?;
        }
        return Ok(pairs);
    }
    // key columns without UNDEF on both sides can be matched by equality
    let exact: Vec<(usize, usize)> = lay
        .shared
        .iter()
        .copied()
        .filter(|&(lc, rc)| !has_undef(l, lc) && !has_undef(r, rc))
        .collect();
    if exact.is_empty() {
        // nested loop with compatibility check (UNDEF on join columns)
        for i in 0..l.len() {
            if i % 1024 == 0 {
                ctx.check()?;
            }
            for j in 0..r.len() {
                if compatible(l, r, i, j, &lay.shared) {
                    pairs.push((i as u32, j as u32));
                }
            }
            ctx.check_output(pairs.len(), w)?;
        }
        return Ok(pairs);
    }
    let (lk, rk) = exact[0];
    let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lk));
    let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rk));
    if merge && lsorted && rsorted {
        // zipper join with galloping (QLever)
        let (a, b) = (&l.cols[lk], &r.cols[rk]);
        let (mut i, mut j) = (0usize, 0usize);
        let mut steps = 0usize;
        while i < a.len() && j < b.len() {
            steps += 1;
            if steps.is_multiple_of(4096) {
                ctx.check()?;
                ctx.check_output(pairs.len(), w)?;
            }
            match a[i].cmp(&b[j]) {
                Ordering::Less => i = gallop(a, i, b[j]),
                Ordering::Greater => j = gallop(b, j, a[i]),
                Ordering::Equal => {
                    let v = a[i];
                    let ie = i + a[i..].partition_point(|x| *x == v);
                    let je = j + b[j..].partition_point(|x| *x == v);
                    // an equal-key run emits up to the product of its lengths: with a
                    // single key that is exact, so an oversized run fails before it is
                    // expanded; otherwise the budget is checked while expanding
                    let run = (ie - i).saturating_mul(je - j);
                    if lay.shared.len() == 1 {
                        ctx.check_output(pairs.len().saturating_add(run), w)?;
                    }
                    for ii in i..ie {
                        if (ii - i) % 1024 == 1023 {
                            ctx.check()?;
                            ctx.check_output(pairs.len(), w)?;
                        }
                        for jj in j..je {
                            if lay.shared.len() == 1 || compatible(l, r, ii, jj, &lay.shared) {
                                pairs.push((ii as u32, jj as u32));
                            }
                        }
                    }
                    i = ie;
                    j = je;
                }
            }
        }
        return Ok(pairs);
    }
    // hash join: build on the smaller side, probe preserves the larger side's order
    let build_left = l.len() < r.len();
    let (bt, pt) = if build_left { (l, r) } else { (r, l) };
    let bcols: Vec<usize> = exact
        .iter()
        .map(|&(lc, rc)| if build_left { lc } else { rc })
        .collect();
    let pcols: Vec<usize> = exact
        .iter()
        .map(|&(lc, rc)| if build_left { rc } else { lc })
        .collect();
    let others_needed = exact.len() < lay.shared.len();
    let emit = |bi: usize, pi: usize, pairs: &mut Vec<(u32, u32)>| {
        let (i, j) = if build_left { (bi, pi) } else { (pi, bi) };
        if !others_needed || compatible(l, r, i, j, &lay.shared) {
            pairs.push((i as u32, j as u32));
        }
    };
    if bcols.len() == 1 {
        let mut map: FxHashMap<Id, Vec<u32>> = FxHashMap::default();
        for (i, v) in bt.cols[bcols[0]].iter().enumerate() {
            map.entry(*v).or_default().push(i as u32);
        }
        for (pi, v) in pt.cols[pcols[0]].iter().enumerate() {
            if pi % 65536 == 0 {
                ctx.check()?;
                ctx.check_output(pairs.len(), w)?;
            }
            if let Some(m) = map.get(v) {
                if m.len() > 1024 {
                    ctx.check_output(pairs.len().saturating_add(m.len()), w)?;
                }
                for &bi in m {
                    emit(bi as usize, pi, &mut pairs);
                }
            }
        }
    } else {
        let mut map: FxHashMap<Vec<Id>, Vec<u32>> = FxHashMap::default();
        for i in 0..bt.len() {
            map.entry(bcols.iter().map(|&c| bt.cols[c][i]).collect())
                .or_default()
                .push(i as u32);
        }
        let mut key = Vec::with_capacity(pcols.len());
        for pi in 0..pt.len() {
            if pi % 65536 == 0 {
                ctx.check()?;
                ctx.check_output(pairs.len(), w)?;
            }
            key.clear();
            key.extend(pcols.iter().map(|&c| pt.cols[c][pi]));
            if let Some(m) = map.get(&key) {
                if m.len() > 1024 {
                    ctx.check_output(pairs.len().saturating_add(m.len()), w)?;
                }
                for &bi in m {
                    emit(bi as usize, pi, &mut pairs);
                }
            }
        }
    }
    Ok(pairs)
}

/// Number of join results; merge joins on a single exact key count run products.
fn join_count(ctx: &Ctx, l: &Table, r: &Table, merge: bool) -> Result<u64> {
    let lay = layout(l, r);
    if merge && lay.shared.len() == 1 {
        let (lk, rk) = lay.shared[0];
        let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lk));
        let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rk));
        if lsorted && rsorted && !has_undef(l, lk) && !has_undef(r, rk) {
            let (a, b) = (&l.cols[lk], &r.cols[rk]);
            let (mut i, mut j, mut n) = (0usize, 0usize, 0u64);
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    Ordering::Less => i = gallop(a, i, b[j]),
                    Ordering::Greater => j = gallop(b, j, a[i]),
                    Ordering::Equal => {
                        let v = a[i];
                        let ie = i + a[i..].partition_point(|x| *x == v);
                        let je = j + b[j..].partition_point(|x| *x == v);
                        n += ((ie - i) * (je - j)) as u64;
                        i = ie;
                        j = je;
                    }
                }
            }
            return Ok(n);
        }
    }
    Ok(join_pairs(ctx, l, r, &lay, merge)?.len() as u64)
}

/// Decompose RDF 1.2 triple terms: rows whose `t` is not a triple term, or whose
/// components do not match the constants / already bound variables, are dropped.
fn unpack(
    ctx: &Ctx,
    input: Table,
    t: VarId,
    parts: &[PathEnd; 3],
    vars: &[VarId],
) -> Result<Table> {
    let tc = input.col_of(t);
    let mut out = Table::new(vars.to_vec());
    let map: Vec<Option<usize>> = vars.iter().map(|v| input.col_of(*v)).collect();
    let mut row = vec![Id::UNDEF; vars.len()];
    for i in 0..input.len() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let Some(tc) = tc else { break };
        let Some(oxrdf::Term::Triple(tr)) = ctx.term(input.cols[tc][i]) else {
            continue;
        };
        let comps = [
            ctx.intern_term(&tr.subject.clone().into()),
            ctx.intern_term(&oxrdf::Term::NamedNode(tr.predicate.clone())),
            ctx.intern_term(&tr.object),
        ];
        for (j, m) in map.iter().enumerate() {
            row[j] = m.map_or(Id::UNDEF, |c| input.cols[c][i]);
        }
        let mut ok = true;
        for (p, c) in parts.iter().zip(comps) {
            match p {
                PathEnd::Const(k) => ok &= *k == c,
                PathEnd::Var(v) => {
                    let j = vars.iter().position(|x| x == v).unwrap();
                    if row[j].is_undef() {
                        row[j] = c;
                    } else {
                        ok &= row[j] == c;
                    }
                }
            }
            if !ok {
                break;
            }
        }
        if ok {
            out.push_row(&row);
        }
    }
    Ok(out)
}

fn join_tables(ctx: &Ctx, l: &Table, r: &Table, _keys: &[VarId], merge: bool) -> Result<Table> {
    let lay = layout(l, r);
    let pairs = join_pairs(ctx, l, r, &lay, merge)?;
    ctx.check_output(pairs.len(), lay.vars.len() + 1)?;
    Ok(materialize(l, r, &lay, &pairs))
}

fn cross(ctx: &Ctx, l: &Table, r: &Table) -> Result<Table> {
    let lay = layout(l, r);
    ctx.check_output(l.len().saturating_mul(r.len()), lay.vars.len() + 1)?;
    let pairs = join_pairs(ctx, l, r, &lay, false)?;
    Ok(materialize(l, r, &lay, &pairs))
}

fn left_join(ctx: &Ctx, l: &Table, r: &Table, expr: Option<&Expr>) -> Result<Table> {
    let lay = layout(l, r);
    let mut pairs = join_pairs(ctx, l, r, &lay, false)?;
    let mut joined = materialize(l, r, &lay, &pairs);
    if let Some(e) = expr {
        let map = joined.var_map(ctx.nvars());
        let keep: Vec<bool> = (0..joined.len())
            .map(|i| {
                ebv(
                    e,
                    &Row {
                        table: &joined,
                        i,
                        map: &map,
                        dec: None,
                    },
                    ctx,
                )
                .unwrap_or(false)
            })
            .collect();
        joined.filter_rows(&keep);
        let mut k = keep.iter();
        pairs.retain(|_| *k.next().unwrap());
    }
    let mut matched = vec![false; l.len()];
    for (i, _) in &pairs {
        matched[*i as usize] = true;
    }
    let missing: Vec<usize> = (0..l.len()).filter(|&i| !matched[i]).collect();
    if !missing.is_empty() {
        for (c, col) in l.cols.iter().enumerate() {
            joined.cols[c].extend(missing.iter().map(|&i| col[i]));
        }
        for c in l.width()..joined.width() {
            joined.cols[c].extend(std::iter::repeat_n(Id::UNDEF, missing.len()));
        }
        joined.len += missing.len();
    }
    Ok(joined)
}

fn minus(ctx: &Ctx, mut l: Table, r: &Table, note: &mut Option<String>) -> Result<Table> {
    let lay = layout(&l, r);
    if lay.shared.is_empty() || r.is_empty() {
        return Ok(l);
    }
    let r_undef = lay.shared.iter().any(|&(_, rc)| has_undef(r, rc));
    if ctx.opt.anti_join
        && let [(lc, rc)] = lay.shared[..]
        && !r_undef
        && !has_undef(&l, lc)
    {
        let (keep, how) = anti_join(ctx, &l, lc, r, rc)?;
        *note = Some(format!(
            "[anti-join on ?{} by {how}]",
            ctx.var_name(l.vars[lc])
        ));
        let sorted = l.sorted.clone();
        l.filter_rows(&keep);
        l.sorted = sorted;
        return Ok(l);
    }
    let set: FxHashSet<Vec<Id>> = if r_undef {
        FxHashSet::default()
    } else {
        (0..r.len())
            .map(|j| lay.shared.iter().map(|&(_, rc)| r.cols[rc][j]).collect())
            .collect()
    };
    let mut keep = vec![true; l.len()];
    for (i, k) in keep.iter_mut().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let all_defined = lay.shared.iter().all(|&(lc, _)| !l.cols[lc][i].is_undef());
        let removed = if !r_undef && all_defined {
            let key: Vec<Id> = lay.shared.iter().map(|&(lc, _)| l.cols[lc][i]).collect();
            set.contains(&key)
        } else {
            (0..r.len()).any(|j| {
                compatible(&l, r, i, j, &lay.shared)
                    && lay
                        .shared
                        .iter()
                        .any(|&(lc, rc)| !l.cols[lc][i].is_undef() && !r.cols[rc][j].is_undef())
            })
        };
        *k = !removed;
    }
    let sorted = l.sorted.clone();
    l.filter_rows(&keep);
    l.sorted = sorted;
    Ok(l)
}

/// The rows of `l` whose id in column `lc` is not in column `rc` of `r` (neither column
/// holds UNDEF, so compatibility is equality): a galloping merge when both sides are
/// sorted on the key, else a probe of the right side's ids. Also names the method.
fn anti_join(
    ctx: &Ctx,
    l: &Table,
    lc: usize,
    r: &Table,
    rc: usize,
) -> Result<(Vec<bool>, &'static str)> {
    let (a, b) = (&l.cols[lc], &r.cols[rc]);
    let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lc));
    let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rc));
    if lsorted && rsorted {
        let mut keep = vec![true; a.len()];
        let (mut i, mut j) = (0, 0);
        if b.len() > 8 * a.len() {
            // gallop over a much larger right side
            for (i, &x) in a.iter().enumerate() {
                if j < b.len() && b[j] < x {
                    j = gallop(b, j, x);
                }
                keep[i] = j == b.len() || b[j] != x;
            }
            return Ok((keep, "merge"));
        }
        // branch-free zipper: each step advances the side with the smaller id (the left
        // one on equal ids, which may repeat), and the left row's flag is final once it
        // advances
        while i < a.len() && j < b.len() {
            if (i + j) % 65536 == 0 {
                ctx.check()?;
            }
            let (x, y) = (a[i], b[j]);
            keep[i] = x != y;
            i += (x <= y) as usize;
            j += (x > y) as usize;
        }
        return Ok((keep, "merge"));
    }
    let _held = ctx.charge((b.len() * 16) as u64)?;
    let set: FxHashSet<Id> = b.iter().copied().collect();
    ctx.check()?;
    Ok((a.par_iter().map(|id| !set.contains(id)).collect(), "hash"))
}

// ------------------------------------------------------------ expressions ------

/// Decode the base-vocabulary ids of the columns the expressions read into row-aligned
/// value columns: rows are argsorted by id so every distinct term is decoded once and
/// each front-coded block is touched once (in parallel); per-row lookups are then O(1)
/// and lock-free.
fn decode_for(ctx: &Ctx, t: &Table, exprs: &[&Expr]) -> Option<super::expr::DecodedCols> {
    if t.len() < 4096 || !exprs.iter().any(|e| super::expr::needs_values(e)) {
        return None;
    }
    let mut vars = Vec::new();
    for e in exprs {
        e.vars(&mut vars);
    }
    vars.sort_unstable();
    vars.dedup();
    let vocab = &ctx.snap.generation.vocab;
    let mut out: super::expr::DecodedCols = vec![None; t.width()];
    for v in vars {
        let Some(c) = t.col_of(v) else { continue };
        let col = &t.cols[c];
        let mut idx: Vec<u32> = (0..col.len() as u32)
            .filter(|&i| col[i as usize].tag() == crate::id::Tag::Vocab)
            .collect();
        if idx.len() < 4096 {
            continue;
        }
        idx.par_sort_unstable_by_key(|&i| col[i as usize]);
        let mut uniq: Vec<u64> = idx.iter().map(|&i| col[i as usize].payload()).collect();
        uniq.dedup();
        let decoded: Vec<Value> = uniq
            .par_chunks(4096)
            .flat_map_iter(|chunk| {
                let mut part = Vec::with_capacity(chunk.len());
                vocab.get_sorted(chunk, |_, k| part.push(Value::from_key(k)));
                part
            })
            .collect();
        if decoded.len() != uniq.len() {
            continue;
        }
        let mut vals: Vec<Option<Value>> = vec![None; col.len()];
        if decoded.len() == idx.len() {
            // every row holds a different term: move the values instead of cloning
            for (&i, v) in idx.iter().zip(decoded) {
                vals[i as usize] = Some(v);
            }
        } else {
            let mut j = 0;
            for &i in &idx {
                let p = col[i as usize].payload();
                while uniq[j] != p {
                    j += 1;
                }
                vals[i as usize] = Some(decoded[j].clone());
            }
        }
        out[c] = Some(vals);
    }
    Some(out)
}

/// Keep the rows that pass every conjunct: EXISTS conjuncts answered from a key set
/// first, then conjuncts that are pure over one variable once per distinct value (see
/// [`super::exprcache`]), the others row by row on the rows that are left.
pub(super) fn apply_filter(ctx: &Ctx, t: &mut Table, exprs: &[Expr]) -> Result<ExprReport> {
    let mut report = ExprReport::default();
    let rest = super::exists::apply(ctx, t, exprs)?;
    let exprs = rest.as_deref().unwrap_or(exprs);
    if exprs.is_empty() {
        return Ok(report);
    }
    let sorted = t.sorted.clone();
    match super::exprcache::filter(ctx, t, exprs, &mut report)? {
        Some((keep, rest)) => {
            t.filter_rows(&keep);
            if !rest.is_empty() {
                let keep = filter_mask(ctx, t, &rest)?;
                t.filter_rows(&keep);
            }
        }
        None => {
            let keep = filter_mask(ctx, t, exprs)?;
            t.filter_rows(&keep);
        }
    }
    t.sorted = sorted;
    Ok(report)
}

/// Evaluate the filter row by row.
pub(super) fn filter_mask(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Result<Vec<bool>> {
    let t0 = Instant::now();
    let dec = decode_for(ctx, t, &exprs.iter().collect::<Vec<_>>());
    tracing::debug!("decoded values in {:?}", t0.elapsed());
    let t0 = Instant::now();
    let map = t.var_map(ctx.nvars());
    let test = |i: usize| {
        let row = Row {
            table: t,
            i,
            map: &map,
            dec: dec.as_ref(),
        };
        exprs.iter().all(|e| ebv(e, &row, ctx).unwrap_or(false))
    };
    let par = t.len() > PAR_THRESHOLD && !exprs.iter().any(|e| e.has_exists());
    let keep = map_rows(ctx, t.len(), par, test)?;
    tracing::debug!("filter evaluated {} rows in {:?}", t.len(), t0.elapsed());
    Ok(keep)
}

/// BIND: the value of `e` on every row (unbound on an error), once per distinct input
/// value when the expression is pure over one variable.
fn compute_column(ctx: &Ctx, t: &Table, e: &Expr, report: &mut ExprReport) -> Result<Vec<Id>> {
    if t.len() >= super::exprcache::MIN_ROWS
        && super::exprcache::eligible(&[e]).is_ok()
        && let Some(p) =
            super::exprcache::per_value(ctx, t, &[e], false, report, |v| column_rows(ctx, v, e))?
    {
        return Ok(p.rows(t.len()));
    }
    column_rows(ctx, t, e)
}

fn column_rows(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Vec<Id>> {
    let dec = decode_for(ctx, t, &[e]);
    let map = t.var_map(ctx.nvars());
    let f = |i: usize| match eval(
        e,
        &Row {
            table: t,
            i,
            map: &map,
            dec: dec.as_ref(),
        },
        ctx,
    ) {
        Ok(v) => v.into_id(ctx),
        Err(_) => Id::UNDEF,
    };
    map_rows(ctx, t.len(), t.len() > PAR_THRESHOLD && !e.has_exists(), f)
}

/// Candidate rows for `ORDER BY ?v LIMIT k` when every `?v` is a number other than NaN:
/// a rounded `f64` per row is cheap, and rows whose rounded key is worse than the k-th
/// best rounded key have at least k rows strictly ahead of them in the exact order (the
/// rounding is monotone and numbers order by value first), so they cannot be among the
/// first k. `None` when the shortcut does not apply.
fn topk_candidates(ctx: &Ctx, t: &Table, keys: &[(Expr, bool)], k: usize) -> Option<Vec<usize>> {
    let [(Expr::Var(v), asc)] = keys else {
        return None;
    };
    if k == 0 || k.saturating_mul(8) > t.len() {
        return None;
    }
    let col = &t.cols[t.col_of(*v)?];
    let approx = |id: Id| -> Option<f64> {
        use crate::id::Tag;
        match id.tag() {
            Tag::Int => Some(id.as_i64() as f64),
            Tag::Double => Some(id.as_f64()).filter(|d| !d.is_nan()),
            Tag::Decimal | Tag::Vocab | Tag::Delta => super::value::approx_f64(&ctx.value(id)?),
            _ => None,
        }
    };
    // ascending order ⇔ smallest first; negate so that "best" is always the largest
    let sign = if *asc { -1.0 } else { 1.0 };
    let f: Vec<f64> = col
        .par_iter()
        .map(|&id| approx(id).map(|d| d * sign))
        .collect::<Option<Vec<f64>>>()?;
    let mut sorted = f.clone();
    let (_, kth, _) = sorted.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
    let tau = *kth;
    Some((0..f.len()).filter(|&i| f[i] >= tau).collect())
}

/// ORDER BY (with an optional LIMIT); also returns how many rows the numeric top-k
/// prefilter kept, if it ran.
fn order_by(
    ctx: &Ctx,
    mut t: Table,
    keys: &[(Expr, bool)],
    limit: Option<usize>,
    report: &mut ExprReport,
) -> Result<(Table, Option<usize>)> {
    let mut prefiltered = None;
    if let Some(k) = limit
        && ctx.opt.topk_prefilter
        && let Some(cand) = topk_candidates(ctx, &t, keys, k)
        && cand.len() < t.len()
    {
        // the candidates keep their relative order, so ties break as in the full sort
        prefiltered = Some(cand.len());
        ctx.check_output(cand.len(), t.width())?;
        t = t.take_rows(&cand);
    }
    Ok((order_by_rows(ctx, t, keys, limit, report)?, prefiltered))
}

fn order_by_rows(
    ctx: &Ctx,
    t: Table,
    keys: &[(Expr, bool)],
    limit: Option<usize>,
    report: &mut ExprReport,
) -> Result<Table> {
    // each key's value per row (`None`: an error), per distinct input value where the
    // key is pure over one variable
    let key_rows = |t: &Table, e: &Expr, dec: Option<&super::expr::DecodedCols>| {
        let map = t.var_map(ctx.nvars());
        let f = |i: usize| {
            eval(
                e,
                &Row {
                    table: t,
                    i,
                    map: &map,
                    dec,
                },
                ctx,
            )
            .ok()
            .and_then(|v| match v {
                Val::Id(id) => ctx.value(id),
                Val::V(v) | Val::Dec(_, v) => Some(v),
            })
        };
        map_rows(ctx, t.len(), t.len() > PAR_THRESHOLD, f)
    };
    let mut cached: Vec<Option<super::exprcache::PerValue<Option<Value>>>> = Vec::new();
    for (e, _) in keys {
        cached.push(if t.len() >= super::exprcache::MIN_ROWS {
            super::exprcache::per_value(ctx, &t, &[e], false, report, |v| {
                key_rows(v, e, decode_for(ctx, v, &[e]).as_ref())
            })?
        } else {
            None
        });
    }
    let rest: Vec<&Expr> = keys
        .iter()
        .zip(&cached)
        .filter(|(_, c)| c.is_none())
        .map(|((e, _), _)| e)
        .collect();
    let dec = decode_for(ctx, &t, &rest);
    let key_vals: Vec<super::exprcache::Column<Option<Value>>> = keys
        .iter()
        .zip(cached)
        .map(|((e, _), c)| {
            Ok(match c {
                Some(p) => super::exprcache::Column::Values(p),
                None => super::exprcache::Column::Rows(key_rows(&t, e, dec.as_ref())?),
            })
        })
        .collect::<Result<_>>()?;
    ctx.check()?;
    let cmp = |a: &usize, b: &usize| {
        for (k, (_, asc)) in keys.iter().enumerate() {
            let o = order_cmp(key_vals[k].get(*a).as_ref(), key_vals[k].get(*b).as_ref());
            let o = if *asc { o } else { o.reverse() };
            if o != Ordering::Equal {
                return o;
            }
        }
        a.cmp(b)
    };
    let mut idx: Vec<usize> = (0..t.len()).collect();
    match limit {
        Some(k) if k < idx.len() => {
            if k > 0 {
                idx.select_nth_unstable_by(k - 1, cmp);
            }
            idx.truncate(k);
            idx.sort_by(cmp);
        }
        _ => idx.par_sort_by(cmp),
    }
    ctx.check()?;
    ctx.check_output(idx.len(), t.width())?;
    Ok(t.take_rows(&idx))
}

fn distinct(t: Table) -> Table {
    if t.width() == 1 {
        let mut seen = FxHashSet::default();
        let idx: Vec<usize> = (0..t.len())
            .filter(|&i| seen.insert(t.cols[0][i]))
            .collect();
        let sorted = t.sorted.clone();
        let mut out = t.take_rows(&idx);
        out.sorted = sorted;
        return out;
    }
    if t.width() == 0 {
        let mut t = t;
        t.len = t.len.min(1);
        return t;
    }
    let mut seen: FxHashSet<Vec<Id>> = FxHashSet::default();
    let idx: Vec<usize> = (0..t.len()).filter(|&i| seen.insert(t.row(i))).collect();
    let sorted = t.sorted.clone();
    let mut out = t.take_rows(&idx);
    out.sorted = sorted;
    out
}

// ------------------------------------------------------------------ group ------

fn group(
    ctx: &Ctx,
    t: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
    report: &mut ExprReport,
) -> Result<Table> {
    if ctx.opt.incremental_group
        && let Some(out) = group_incremental(ctx, t, keys, aggs)?
    {
        return Ok(out);
    }
    let kcols: Vec<Option<usize>> = keys.iter().map(|k| t.col_of(*k)).collect();
    let mut order: Vec<Vec<Id>> = Vec::new();
    let mut groups: FxHashMap<Vec<Id>, Vec<u32>> = FxHashMap::default();
    for i in 0..t.len() {
        if i % 65_536 == 0 {
            ctx.check()?;
            ctx.check_output(order.len(), keys.len() + aggs.len())?;
        }
        let key: Vec<Id> = kcols
            .iter()
            .map(|c| c.map_or(Id::UNDEF, |c| t.cols[c][i]))
            .collect();
        match groups.get_mut(&key) {
            Some(g) => g.push(i as u32),
            None => {
                order.push(key.clone());
                groups.insert(key, vec![i as u32]);
            }
        }
    }
    if t.is_empty() && keys.is_empty() {
        order.push(Vec::new());
        groups.insert(Vec::new(), Vec::new());
    }
    let mut vars = keys.to_vec();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let mut out = Table::new(vars);
    let map = t.var_map(ctx.nvars());
    // aggregate arguments that are pure over one variable, once per distinct value
    let mut args = Vec::with_capacity(aggs.len());
    for (_, agg) in aggs {
        args.push(match &agg.expr {
            Some(e)
                if t.len() >= super::exprcache::MIN_ROWS
                    && super::exprcache::eligible(&[e]).is_ok() =>
            {
                super::exprcache::per_value(ctx, t, &[e], false, report, |v| {
                    let map = v.var_map(ctx.nvars());
                    let f = |i: usize| {
                        let row = Row {
                            table: v,
                            i,
                            map: &map,
                            dec: None,
                        };
                        eval(e, &row, ctx).ok().map(|x| x.into_id(ctx))
                    };
                    map_rows(ctx, v.len(), v.len() > PAR_THRESHOLD && !e.has_exists(), f)
                })?
            }
            _ => None,
        });
    }
    for key in order {
        ctx.check()?;
        let rows = &groups[&key];
        let mut row = key.clone();
        for ((_, agg), arg) in aggs.iter().zip(&args) {
            row.push(aggregate(ctx, t, &map, rows, agg, arg.as_ref()));
        }
        out.push_row(&row);
    }
    Ok(out)
}

/// Whether `group` can run incrementally: at most one key variable, and aggregates that
/// are `COUNT(*)` or COUNT / SUM / AVG / MIN / MAX / SAMPLE of an input column, without
/// DISTINCT. The planner uses this to name the operator in EXPLAIN.
pub fn incremental_group_ok(keys: &[VarId], aggs: &[(VarId, Agg)], input: &[VarId]) -> bool {
    keys.len() <= 1
        && keys.iter().all(|k| input.contains(k))
        && aggs.iter().all(|(_, a)| {
            !a.distinct
                && match &a.expr {
                    None => matches!(a.func, AggregateFunction::Count),
                    Some(Expr::Var(v)) => {
                        input.contains(v)
                            && matches!(
                                a.func,
                                AggregateFunction::Count
                                    | AggregateFunction::Sum
                                    | AggregateFunction::Avg
                                    | AggregateFunction::Min
                                    | AggregateFunction::Max
                                    | AggregateFunction::Sample
                            )
                    }
                    _ => false,
                }
        })
}

/// Running state of one aggregate of one group (same results as [`aggregate`]).
enum AggState {
    /// COUNT(*) / COUNT(?v): rows / bound values
    Count(u64),
    /// SUM / AVG: an exact integer sum while every value is an inline integer, then the
    /// generic numeric sum; `None` once a value was unbound or not numeric
    Sum {
        int: i64,
        generic: Option<Value>,
        ok: bool,
        rows: u64,
    },
    /// MIN / MAX: the best id and its value
    Best(Option<Id>, Option<Value>),
    Sample(Option<Id>),
}

impl AggState {
    fn new(agg: &Agg) -> AggState {
        match agg.func {
            AggregateFunction::Sum | AggregateFunction::Avg => AggState::Sum {
                int: 0,
                generic: None,
                ok: true,
                rows: 0,
            },
            AggregateFunction::Min | AggregateFunction::Max => AggState::Best(None, None),
            AggregateFunction::Sample => AggState::Sample(None),
            _ => AggState::Count(0),
        }
    }

    /// Add one row; `id` is the aggregated column's value (`None` for COUNT(*)).
    #[inline]
    fn add(&mut self, ctx: &Ctx, agg: &Agg, id: Option<Id>) {
        match self {
            AggState::Count(n) => *n += id.is_none_or(|id| !id.is_undef()) as u64,
            AggState::Sum {
                int,
                generic,
                ok,
                rows,
            } => {
                *rows += 1;
                let id = id.unwrap_or(Id::UNDEF);
                if !*ok {
                    return;
                }
                if generic.is_none()
                    && id.tag() == crate::id::Tag::Int
                    && let Some(s) = int.checked_add(id.as_i64())
                {
                    *int = s;
                    return;
                }
                let acc = generic
                    .take()
                    .unwrap_or_else(|| Value::Integer((*int).into()));
                match (!id.is_undef()).then(|| ctx.value(id)).flatten() {
                    Some(v) => match arith(NumOp::Add, &acc, &v) {
                        Ok(v) => *generic = Some(v),
                        Err(_) => *ok = false,
                    },
                    None => *ok = false,
                }
            }
            AggState::Best(best, best_v) => {
                let Some(id) = id.filter(|id| !id.is_undef()) else {
                    return;
                };
                let v = ctx.value(id);
                let better = match best_v {
                    None => best.is_none(),
                    Some(b) => {
                        let o = order_cmp(v.as_ref(), Some(b));
                        if matches!(agg.func, AggregateFunction::Min) {
                            o == Ordering::Less
                        } else {
                            o == Ordering::Greater
                        }
                    }
                };
                if better {
                    *best = Some(id);
                    *best_v = v;
                }
            }
            AggState::Sample(x) => {
                if x.is_none() {
                    *x = id.filter(|id| !id.is_undef());
                }
            }
        }
    }

    fn finish(self, ctx: &Ctx, agg: &Agg) -> Id {
        match self {
            AggState::Count(n) => Id::from_i64(n as i64).unwrap_or(Id::UNDEF),
            AggState::Sum {
                int,
                generic,
                ok,
                rows,
            } => {
                if !ok {
                    return Id::UNDEF;
                }
                let sum = generic.unwrap_or_else(|| Value::Integer(int.into()));
                let v = if matches!(agg.func, AggregateFunction::Avg) {
                    if rows == 0 {
                        Some(Value::Integer(0.into()))
                    } else {
                        arith(NumOp::Div, &sum, &Value::Integer((rows as i64).into())).ok()
                    }
                } else {
                    Some(sum)
                };
                v.map_or(Id::UNDEF, |v| ctx.intern_value(&v))
            }
            AggState::Best(best, _) => best.unwrap_or(Id::UNDEF),
            AggState::Sample(x) => x.unwrap_or(Id::UNDEF),
        }
    }
}

/// GROUP BY in one pass: groups are found through a hash map on the single key id and
/// every aggregate keeps a running state, instead of collecting per-group row lists and
/// value vectors. `None` when the query shape is not admitted (see
/// [`incremental_group_ok`]).
fn group_incremental(
    ctx: &Ctx,
    t: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
) -> Result<Option<Table>> {
    if !incremental_group_ok(keys, aggs, &t.vars) {
        return Ok(None);
    }
    let kcol = keys.first().and_then(|k| t.col_of(*k));
    let acols: Vec<Option<usize>> = aggs
        .iter()
        .map(|(_, a)| match &a.expr {
            Some(Expr::Var(v)) => t.col_of(*v),
            _ => None,
        })
        .collect();
    let mut index: FxHashMap<Id, u32> = FxHashMap::default();
    let mut order: Vec<Id> = Vec::new();
    let mut states: Vec<AggState> = Vec::new();
    let na = aggs.len();
    if kcol.is_none() {
        // no grouping: one group, also for empty input
        order.push(Id::UNDEF);
        states.extend(aggs.iter().map(|(_, a)| AggState::new(a)));
    }
    for i in 0..t.len() {
        if i % 65_536 == 0 {
            ctx.check()?;
            ctx.check_output(order.len(), keys.len() + aggs.len())?;
        }
        let g = match kcol {
            None => 0,
            Some(c) => {
                let k = t.cols[c][i];
                *index.entry(k).or_insert_with(|| {
                    order.push(k);
                    states.extend(aggs.iter().map(|(_, a)| AggState::new(a)));
                    (order.len() - 1) as u32
                }) as usize
            }
        };
        for (a, ((_, agg), col)) in aggs.iter().zip(&acols).enumerate() {
            states[g * na + a].add(ctx, agg, col.map(|c| t.cols[c][i]));
        }
    }
    let mut vars = keys.to_vec();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let mut out = Table::new(vars);
    let mut states = states.into_iter();
    let mut row = Vec::with_capacity(1 + na);
    for k in order {
        ctx.check()?;
        row.clear();
        if kcol.is_some() {
            row.push(k);
        }
        for (_, agg) in aggs {
            row.push(states.next().unwrap().finish(ctx, agg));
        }
        out.push_row(&row);
    }
    Ok(Some(out))
}

/// One aggregate of one group; `arg` holds the argument's value per row when it was
/// evaluated per distinct value (`None` in it: an error).
fn aggregate(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    agg: &Agg,
    arg: Option<&super::exprcache::PerValue<Option<Id>>>,
) -> Id {
    let Some(e) = &agg.expr else {
        // COUNT(*)
        let n = if agg.distinct {
            rows.iter()
                .map(|&i| t.row(i as usize))
                .collect::<FxHashSet<_>>()
                .len()
        } else {
            rows.len()
        };
        return Id::from_i64(n as i64).unwrap_or(Id::UNDEF);
    };
    let mut vals: Vec<Result<Id, ()>> = rows
        .iter()
        .map(|&i| {
            if let Some(a) = arg {
                return a.get(i as usize).ok_or(());
            }
            eval(
                e,
                &Row {
                    table: t,
                    i: i as usize,
                    map,
                    dec: None,
                },
                ctx,
            )
            .map(|v| v.into_id(ctx))
            .map_err(|_| ())
        })
        .collect();
    if agg.distinct {
        let mut seen = FxHashSet::default();
        vals.retain(|v| match v {
            Ok(id) => seen.insert(*id),
            Err(_) => true,
        });
    }
    let values = || {
        vals.iter()
            .filter_map(|v| v.ok())
            .filter_map(|id| ctx.value(id))
    };
    let fold_sum = || -> Option<Value> {
        let mut acc = Value::Integer(0.into());
        for v in &vals {
            let v = ctx.value((*v).ok()?)?;
            acc = arith(NumOp::Add, &acc, &v).ok()?;
        }
        Some(acc)
    };
    let result: Option<Value> = match &agg.func {
        AggregateFunction::Count => {
            return Id::from_i64(vals.iter().filter(|v| v.is_ok()).count() as i64)
                .unwrap_or(Id::UNDEF);
        }
        AggregateFunction::Sum => fold_sum(),
        AggregateFunction::Avg => {
            if vals.is_empty() {
                Some(Value::Integer(0.into()))
            } else {
                fold_sum().and_then(|s| {
                    arith(NumOp::Div, &s, &Value::Integer((vals.len() as i64).into())).ok()
                })
            }
        }
        AggregateFunction::Min | AggregateFunction::Max => {
            let is_min = matches!(agg.func, AggregateFunction::Min);
            let mut best: Option<Id> = None;
            let mut best_v: Option<Value> = None;
            for id in vals.iter().filter_map(|v| v.ok()) {
                let v = ctx.value(id);
                let better = match &best_v {
                    None => best.is_none(),
                    Some(b) => {
                        let o = order_cmp(v.as_ref(), Some(b));
                        if is_min {
                            o == Ordering::Less
                        } else {
                            o == Ordering::Greater
                        }
                    }
                };
                if better {
                    best = Some(id);
                    best_v = v;
                }
            }
            return best.unwrap_or(Id::UNDEF);
        }
        AggregateFunction::Sample => return vals.iter().find_map(|v| v.ok()).unwrap_or(Id::UNDEF),
        AggregateFunction::GroupConcat { separator } => {
            let sep = separator.as_deref().unwrap_or(" ");
            let mut parts = Vec::new();
            for v in values() {
                let Ok(s) = v.lexical() else { return Id::UNDEF };
                parts.push(s.to_string());
            }
            if vals.iter().any(|v| v.is_err()) {
                return Id::UNDEF;
            }
            // SPARQL 1.1: the result is a simple literal (language tags are dropped)
            Some(Value::Str(parts.join(sep).into()))
        }
        AggregateFunction::Custom(iri) => {
            return super::aggext::aggregate(ctx, iri.as_str(), &vals);
        }
    };
    result.map_or(Id::UNDEF, |v| ctx.intern_value(&v))
}

// ------------------------------------------------------------------ paths ------

struct Graph<'a> {
    ctx: &'a Ctx,
    spec: &'a PathSpec,
    graph: GraphFilter,
    fwd: Option<FxHashMap<u64, Vec<u64>>>,
    bwd: Option<FxHashMap<u64, Vec<u64>>>,
    /// frontier levels expanded by one sweep over the predicate's index range
    sweeps: &'a std::cell::Cell<usize>,
}

/// A frontier at least `rows / SWEEP_RATIO` long is expanded by one merged pass over the
/// predicate's rows instead of one index seek per node (a seek costs about as much as
/// merging a few hundred rows).
const SWEEP_RATIO: u64 = 512;

impl Graph<'_> {
    /// Neighbours of every node of a sorted, duplicate-free frontier (with duplicates),
    /// for simple predicate paths: per-node seeks for a small frontier, otherwise one
    /// pass over the predicate's index rows between the first and last frontier node,
    /// merged with the frontier.
    fn expand(&self, frontier: &[u64], forward: bool, out: &mut Vec<u64>) -> Result<()> {
        let Some((p, rev)) = self.spec.simple else {
            for &x in frontier {
                out.extend(self.neighbours(x, forward)?);
            }
            return Ok(());
        };
        let perm = if forward != rev { Perm::Pso } else { Perm::Pos };
        let rows = self.ctx.snap.estimate(perm, &[p]);
        if !self.ctx.opt.batched_paths
            || frontier.len() < 64
            || (frontier.len() as u64).saturating_mul(SWEEP_RATIO) < rows
        {
            for &x in frontier {
                out.extend(self.neighbours(x, forward)?);
            }
            return Ok(());
        }
        self.sweeps.set(self.sweeps.get() + 1);
        let gc = perm.col_of(crate::index::G);
        let (first, last) = (frontier[0], frontier[frontier.len() - 1]);
        let mut j = 0;
        self.ctx.snap.scan_between_cols(
            perm,
            [p, first, 0, 0],
            [p, last, u64::MAX, u64::MAX],
            if self.ctx.opt.selective_columns {
                (1 << 1) | (1 << 2) | (1 << gc)
            } else {
                crate::index::ALL_COLS
            },
            |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        let (keys, vals, gs) =
                            (&b.cols[1][s..e], &b.cols[2][s..e], &b.cols[gc][s..e]);
                        let mut i = 0;
                        while i < keys.len() && j < frontier.len() {
                            let (k, f) = (keys[i], frontier[j]);
                            if k < f {
                                // skip to the next frontier node
                                i += keys[i..].partition_point(|&x| x < f);
                            } else if k > f {
                                j += frontier[j..].partition_point(|&x| x < k);
                            } else {
                                // the run may continue in the next chunk: `j` moves on only
                                // once a larger key is seen
                                while i < keys.len() && keys[i] == f {
                                    if self.graph.accepts(gs[i]) {
                                        out.push(vals[i]);
                                    }
                                    i += 1;
                                }
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        j += frontier[j..].partition_point(|&x| x < k[1]);
                        if j < frontier.len() && frontier[j] == k[1] && self.graph.accepts(k[gc]) {
                            out.push(k[2]);
                        }
                    }
                }
                Ok(j < frontier.len())
            },
        )?;
        self.ctx.check()?;
        Ok(())
    }

    fn neighbours(&self, x: u64, forward: bool) -> Result<Vec<u64>> {
        if let Some((p, rev)) = self.spec.simple {
            let dir = forward != rev;
            // forward: (x p ?o) via PSO; backward: (?s p x) via POS
            let perm = if dir { Perm::Pso } else { Perm::Pos };
            let gc = perm.col_of(crate::index::G);
            let mut out = Vec::new();
            let mask = if self.ctx.opt.selective_columns {
                (1 << 2) | (1 << gc)
            } else {
                crate::index::ALL_COLS
            };
            let (lo, hi) = (pad(&[p, x], 0), pad(&[p, x], u64::MAX));
            self.ctx.snap.scan_between_cols(perm, lo, hi, mask, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if self.graph.accepts(b.cols[gc][i]) {
                                out.push(b.cols[2][i]);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if self.graph.accepts(k[gc]) {
                            out.push(k[2]);
                        }
                    }
                }
                Ok(true)
            })?;
            out.dedup();
            return Ok(out);
        }
        let m = if forward { &self.fwd } else { &self.bwd };
        Ok(m.as_ref()
            .and_then(|m| m.get(&x))
            .cloned()
            .unwrap_or_default())
    }

    /// Nodes reachable from `start` according to min/max length.
    fn reach(&self, start: u64, forward: bool) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        if self.spec.min == 0 {
            out.push(start);
            seen.insert(start);
        }
        let mut frontier = vec![start];
        let mut depth = 0;
        let mut found = Vec::new();
        while !frontier.is_empty() {
            self.ctx.check()?;
            depth += 1;
            let mut next = Vec::new();
            frontier.sort_unstable();
            found.clear();
            self.expand(&frontier, forward, &mut found)?;
            for &y in &found {
                if seen.insert(y) {
                    out.push(y);
                    next.push(y);
                }
            }
            if self.spec.max_one && depth >= 1 {
                break;
            }
            frontier = next;
            self.ctx.check_output(out.len(), 1)?;
        }
        Ok(out)
    }

    /// Does the term occur as subject or object in the active graph?
    fn is_node(&self, x: u64) -> Result<bool> {
        for perm in [Perm::Spo, Perm::Osp] {
            let gc = perm.col_of(crate::index::G);
            let mut found = false;
            self.ctx.snap.scan(perm, &[x], |c| {
                found = match c {
                    Chunk::Block(b, s, e) => (s..e).any(|i| self.graph.accepts(b.cols[gc][i])),
                    Chunk::Row(k) => self.graph.accepts(k[gc]),
                };
                Ok(!found)
            })?;
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// All start nodes when both ends are unbound.
    fn all_nodes(&self) -> Result<Vec<u64>> {
        let mut set = FxHashSet::default();
        if self.spec.min > 0 {
            if let Some((p, rev)) = self.spec.simple {
                let perm = if rev { Perm::Pos } else { Perm::Pso };
                let gc = perm.col_of(crate::index::G);
                for k in self.ctx.snap.scan_keys(perm, &[p])? {
                    if self.graph.accepts(k[gc]) {
                        set.insert(k[1]);
                    }
                }
            } else if let Some(f) = &self.fwd {
                set.extend(f.keys().copied());
            }
        } else {
            // every subject and object of the active graph
            let gc = Perm::Spo.col_of(crate::index::G);
            for k in self.ctx.snap.scan_keys(Perm::Spo, &[])? {
                if self.graph.accepts(k[gc]) {
                    set.insert(k[S]);
                    set.insert(k[Perm::Spo.col_of(O)]);
                }
            }
            let _ = P;
        }
        let mut v: Vec<u64> = set.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }
}

fn path(
    ctx: &Ctx,
    spec: &PathSpec,
    bound_from_left: bool,
    mut inputs: Vec<Table>,
    vars: &[VarId],
) -> Result<(Table, usize)> {
    let left = if bound_from_left { inputs.pop() } else { None };
    let edges = inputs.pop();
    let graphs: Vec<(GraphFilter, Option<Id>)> = match spec.graph_var {
        None => vec![(spec.graph.clone(), None)],
        Some(_) => {
            let all = ctx.snap.graph_ids()?;
            all.into_iter()
                .filter(|g| spec.graph.accepts(g.0))
                .map(|g| (GraphFilter::One(g.0), Some(g)))
                .collect()
        }
    };
    let (sv, ov) = (
        match spec.subj {
            PathEnd::Var(v) => Some(v),
            _ => None,
        },
        match spec.obj {
            PathEnd::Var(v) => Some(v),
            _ => None,
        },
    );
    let mut pvars = Vec::new();
    for v in [sv, ov].into_iter().flatten() {
        if !pvars.contains(&v) {
            pvars.push(v);
        }
    }
    if let Some(g) = spec.graph_var {
        pvars.push(g);
    }
    let mut out = Table::new(pvars.clone());
    let sweeps = std::cell::Cell::new(0);
    for (gf, gid) in graphs {
        let (fwd, bwd) = match (&edges, spec.edge_vars) {
            (Some(e), Some((a, b))) => {
                let (ac, bc) = (e.col_of(a).unwrap(), e.col_of(b).unwrap());
                let gc = spec.graph_var.and_then(|g| e.col_of(g));
                let mut f: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
                let mut bw: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
                for i in 0..e.len() {
                    if let (Some(gc), Some(g)) = (gc, gid)
                        && e.cols[gc][i] != g
                    {
                        continue;
                    }
                    let (x, y) = (e.cols[ac][i].0, e.cols[bc][i].0);
                    if x == 0 || y == 0 {
                        continue;
                    }
                    f.entry(x).or_default().push(y);
                    bw.entry(y).or_default().push(x);
                }
                (Some(f), Some(bw))
            }
            _ => (None, None),
        };
        let g = Graph {
            ctx,
            spec,
            graph: gf,
            fwd,
            bwd,
            sweeps: &sweeps,
        };
        let push = |s: u64, o: u64, out: &mut Table| {
            let mut row = Vec::with_capacity(pvars.len());
            if let Some(v) = sv {
                let _ = v;
                row.push(Id(s));
            }
            if let Some(v) = ov
                && Some(v) != sv
            {
                row.push(Id(o));
            }
            if sv.is_some() && sv == ov && s != o {
                return;
            }
            if let Some(g) = gid {
                row.push(g);
            }
            out.push_row(&row);
        };
        match (&spec.subj, &spec.obj) {
            (PathEnd::Const(s), PathEnd::Const(o)) => {
                if g.reach(s.0, true)?.contains(&o.0) {
                    push(s.0, o.0, &mut out);
                }
            }
            (PathEnd::Const(s), PathEnd::Var(_)) => {
                for o in g.reach(s.0, true)? {
                    push(s.0, o, &mut out);
                }
            }
            (PathEnd::Var(_), PathEnd::Const(o)) => {
                for s in g.reach(o.0, false)? {
                    push(s, o.0, &mut out);
                }
            }
            (PathEnd::Var(a), PathEnd::Var(b)) => {
                let starts: Vec<u64> = match &left {
                    Some(l) => {
                        let (col, forward) = match (l.col_of(*a), l.col_of(*b)) {
                            (Some(c), _) => (c, true),
                            (None, Some(c)) => (c, false),
                            _ => return Err(Error::invalid("path bound variable missing")),
                        };
                        let mut s: Vec<u64> = l.cols[col]
                            .iter()
                            .map(|x| x.0)
                            .filter(|x| *x != 0)
                            .collect();
                        s.sort_unstable();
                        s.dedup();
                        for x in s {
                            // variable–variable paths range over graph nodes only: a
                            // zero-length match needs the start term to occur in the graph
                            let in_graph = spec.min > 0 || g.is_node(x)?;
                            for y in g.reach(x, forward)? {
                                if y == x && spec.min == 0 && !in_graph {
                                    continue;
                                }
                                if forward {
                                    push(x, y, &mut out)
                                } else {
                                    push(y, x, &mut out)
                                }
                            }
                        }
                        continue;
                    }
                    None => g.all_nodes()?,
                };
                for x in starts {
                    for y in g.reach(x, true)? {
                        push(x, y, &mut out);
                    }
                    ctx.check_output(out.len(), out.width())?;
                }
            }
        }
    }
    let t = match left {
        Some(l) => join_tables(ctx, &l, &out, &[], false)?.project(vars),
        None => out.project(vars),
    };
    Ok((t, sweeps.get()))
}

// ----------------------------------------------------------------- vectors ------

/// Exact top-k vector search (`spk:vectorSearch`).
#[cfg(feature = "geo")]
use crate::geo::exec::{spatial_pf, spatial_scan};
#[cfg(feature = "geo")]
use crate::geo::join::spatial_join;
#[cfg(feature = "geo")]
use crate::geo::knn::spatial_knn;
#[cfg(feature = "geo")]
use crate::geo::rewrite::spatial_relate;

#[cfg(not(feature = "geo"))]
type Counters = serde_json::Map<String, serde_json::Value>;

#[cfg(not(feature = "geo"))]
fn spatial_scan(
    _: &Ctx,
    _: &super::geopf::SpatialScanSpec,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_pf(_: &Ctx, _: &super::geopf::SpatialPfSpec, _: &[VarId]) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_join(
    _: &Ctx,
    _: &super::geojoin::SpatialJoinSpec,
    _: Vec<Table>,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_knn(
    _: &Ctx,
    _: &super::geojoin::SpatialKnnSpec,
    _: &Node,
    _: &[VarId],
) -> Result<(Table, Counters, Vec<PlanInfo>)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_relate(
    _: &Ctx,
    _: &super::georewrite::SpatialRelateSpec,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

fn vector_search(ctx: &Ctx, spec: &super::plan::VectorSpec, vars: &[VarId]) -> Result<Table> {
    use super::plan::VectorQuery;
    use crate::vector;
    let mut t = Table::new(vars.to_vec());
    let Some(pred) = spec.pred else {
        return Ok(t);
    };
    let graph = |g: u64| spec.graph.accepts(g);
    let query: Vec<f32> = match &spec.query {
        VectorQuery::Vector(v) => v.to_vec(),
        VectorQuery::Entity(e) => {
            // the entity's vectors under the predicate, in the active graph
            let mut found: Vec<Id> = Vec::new();
            for k in ctx.snap.scan_keys(Perm::Pso, &[pred.0, e.0])? {
                if graph(k[3]) && !found.contains(&Id(k[2])) {
                    found.push(Id(k[2]));
                }
            }
            match found.as_slice() {
                [] => return Ok(t),
                [o] => ctx
                    .snap
                    .key(*o)
                    .and_then(|k| vector::from_key(&k))
                    .ok_or_else(|| {
                        Error::invalid("the entity's vector is not a valid spk:vector")
                    })?,
                many => {
                    return Err(Error::invalid(format!(
                        "entity {} has {} vectors for the predicate; pass a vector literal",
                        ctx.term(*e).map_or("?".into(), |t| t.to_string()),
                        many.len()
                    )));
                }
            }
        }
    };
    let q = vector::Search {
        pred: pred.0,
        query: &query,
        k: spec.k,
        metric: spec.metric,
        graph: &graph,
        dedup: spec.dedup,
    };
    let hits = vector::search(&ctx.snap, &q, &|| ctx.check())?;
    ctx.check_output(hits.len(), vars.len())?;
    let col = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
    let cs = match spec.subject {
        PathEnd::Var(v) => col(Some(v)),
        _ => None,
    };
    let (cscore, cvec, cg) = (col(spec.score), col(spec.vector), col(spec.graph_var));
    let mut row = vec![Id::UNDEF; vars.len()];
    for h in hits {
        if let PathEnd::Const(s) = spec.subject
            && s.0 != h.s
        {
            continue;
        }
        row.fill(Id::UNDEF);
        if let Some(c) = cs {
            row[c] = Id(h.s);
        }
        if let Some(c) = cscore {
            row[c] = Id::from_f64(h.score as f64)
                .unwrap_or_else(|| ctx.intern_value(&Value::Double((h.score as f64).into())));
        }
        if let Some(c) = cvec {
            row[c] = Id(h.o);
        }
        if let Some(c) = cg {
            row[c] = Id(h.g);
        }
        t.push_row(&row);
    }
    Ok(t)
}

// ---------------------------------------------------------------- service ------

fn service(ctx: &Ctx, endpoint: &PathEnd, query: &str, vars: &[VarId]) -> Result<Table> {
    if !ctx.allow_service {
        return Err(Error::Service("SERVICE is disabled".into()));
    }
    if ctx.forbid_service {
        return Err(Error::NotPermitted(
            "SERVICE requires the federate permission".into(),
        ));
    }
    let PathEnd::Const(id) = endpoint else {
        return Err(Error::unsupported("SERVICE with a variable endpoint"));
    };
    let Some(oxrdf::Term::NamedNode(url)) = ctx.term(*id) else {
        return Err(Error::Service("invalid SERVICE endpoint".into()));
    };
    // the host of the endpoint (no user info, no port)
    let host = oxiri::Iri::parse(url.as_str())
        .ok()
        .and_then(|i| {
            let a = i.authority()?;
            let a = a.rsplit('@').next().unwrap_or(a);
            Some(match a.strip_prefix('[') {
                Some(v6) => v6.split(']').next().unwrap_or(v6).to_string(),
                None => a.split(':').next().unwrap_or(a).to_string(),
            })
        })
        .unwrap_or_default();
    // a client span: its context is what the outbound headers hook propagates
    let span = tracing::info_span!(
        "sparql.service",
        otel.kind = "client",
        http.request.method = "POST",
        server.address = %host,
        http.response.status_code = tracing::field::Empty,
    );
    let _entered = span.enter();
    // within the query's deadline too
    let timeout = ctx.deadline.map_or(ctx.outbound.timeout, |d| {
        d.saturating_duration_since(Instant::now())
    });
    let resp = ctx
        .outbound
        .send(&ctx.outbound_budget, url.as_str(), timeout, |client, u| {
            client
                .post(u)
                .header(
                    "Accept",
                    "application/sparql-results+json, application/sparql-results+xml;q=0.8",
                )
                .form(&[("query", query)])
        })
        .map_err(|f| {
            f.into_error(&format!("SERVICE <{}>", url.as_str()), |m| {
                Error::Service(format!("<{}>: {m}", url.as_str()))
            })
        })?;
    span.record("http.response.status_code", i64::from(resp.status.as_u16()));
    if !resp.status.is_success() {
        return Err(Error::Service(format!(
            "{} returned {}",
            url.as_str(),
            resp.status
        )));
    }
    let fmt = if resp.content_type.contains("xml") {
        sparesults::QueryResultsFormat::Xml
    } else {
        sparesults::QueryResultsFormat::Json
    };
    // parsed as it streams in, under the policy's byte ceiling and deadline
    let parser = sparesults::QueryResultsParser::from_format(fmt);
    let mut t = Table::new(vars.to_vec());
    let failed = |e: sparesults::QueryResultsParseError| match e {
        // a spent budget, or the body's own words (timeout, size, connection)
        sparesults::QueryResultsParseError::Io(e) => match crate::codec::io_error(e) {
            Error::Io(e) => Error::Service(format!("<{}>: {e}", url.as_str())),
            e => e,
        },
        e => Error::Service(format!("<{}>: {e}", url.as_str())),
    };
    match parser.for_reader(resp.body).map_err(failed)? {
        sparesults::ReaderQueryResultsParserOutput::Solutions(sols) => {
            for sol in sols {
                let sol = sol.map_err(failed)?;
                let row: Vec<Id> = vars
                    .iter()
                    .map(|v| {
                        sol.get(ctx.var_name(*v).as_str())
                            .map_or(Id::UNDEF, |term| ctx.intern_term(term))
                    })
                    .collect();
                t.push_row(&row);
            }
        }
        sparesults::ReaderQueryResultsParserOutput::Boolean(_) => {}
    }
    Ok(t)
}
