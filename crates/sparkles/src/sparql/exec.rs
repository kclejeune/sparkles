//! Physical operator execution (column-at-a-time, fully materialized results).

use super::ctx::Ctx;
use super::expr::{Expr, Row, Val, ebv, eval};
use super::plan::{Agg, GraphFilter, JoinAlgo, Kind, Node, PathEnd, PathSpec, ScanSpec};
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
                | Kind::Values(_)
                | Kind::Empty
                | Kind::CountScan { .. }
                | Kind::CountDistinctScan { .. }
                | Kind::GroupCountScan { .. }
        );
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
        if let Some(t) = results.get(k, ctx) {
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
    let child = |i: usize, infos: &mut Vec<PlanInfo>| -> Result<Table> {
        let (t, info) = execute(ctx, &n.children[i])?;
        infos.push(info);
        Ok(t)
    };
    let mut table = match &n.kind {
        Kind::Empty => Table::empty(n.vars.clone()),
        Kind::Values(t) => t.clone(),
        Kind::Scan(spec) => scan(ctx, spec, &n.vars)?,
        Kind::GroupCountScan {
            spec,
            key: _,
            counts,
        } => group_count_scan(ctx, spec, &n.vars, counts.len())?,
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
        Kind::CountDistinctScan { spec, var } => {
            let c = count_distinct_scan(ctx, spec)?;
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
        Kind::LeftJoin { expr } => {
            let l = child(0, &mut infos)?;
            let r = child(1, &mut infos)?;
            left_join(ctx, &l, &r, expr.as_ref())?
        }
        Kind::Minus => {
            let l = child(0, &mut infos)?;
            let r = child(1, &mut infos)?;
            minus(ctx, l, &r)?
        }
        Kind::Union => {
            let mut out = Table::new(n.vars.clone());
            for i in 0..n.children.len() {
                out.append(child(i, &mut infos)?);
                ctx.check_rows(out.len())?;
            }
            out
        }
        Kind::Filter(exprs) => {
            let mut t = child(0, &mut infos)?;
            apply_filter(ctx, &mut t, exprs);
            t
        }
        Kind::Extend(v, e) => {
            let mut t = child(0, &mut infos)?;
            let col = compute_column(ctx, &t, e);
            t.vars.push(*v);
            t.cols.push(col);
            t
        }
        Kind::Sort(vars) => {
            let mut t = child(0, &mut infos)?;
            t.sort_by_vars(vars);
            t
        }
        Kind::OrderBy { keys, limit } => {
            let t = child(0, &mut infos)?;
            order_by(ctx, t, keys, *limit)
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
            t.slice(*offset, Some(*limit))
        }
        Kind::Slice { offset, limit } => child(0, &mut infos)?.slice(*offset, *limit),
        Kind::Group { keys, aggs } => {
            let t = child(0, &mut infos)?;
            group(ctx, &t, keys, aggs)?
        }
        Kind::Path {
            spec,
            bound_from_left,
        } => {
            let mut inputs = Vec::new();
            for i in 0..n.children.len() {
                inputs.push(child(i, &mut infos)?);
            }
            path(ctx, spec, *bound_from_left, inputs, &n.vars)?
        }
        Kind::Service {
            endpoint,
            query,
            silent,
        } => match service(ctx, endpoint, query, &n.vars) {
            Ok(t) => t,
            Err(_) if *silent => Table::unit(),
            Err(e) => return Err(e),
        },
    };
    if !matches!(
        n.kind,
        Kind::Scan(_)
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
    ctx.check_rows(table.len())?;
    let info = PlanInfo {
        operator: n.operator().to_string(),
        description: n.desc.clone(),
        columns: names(ctx, &n.vars),
        sorted_on: names(ctx, &n.sorted),
        estimated_rows: n.est.round(),
        estimated_cost: n.cost.round(),
        actual_rows: table.len() as i64,
        time_ms: start.elapsed().as_secs_f64() * 1000.0,
        cached: false,
        children: infos,
    };
    Ok((table, info))
}

// ------------------------------------------------------------------ scans ------

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

/// Count runs of the first free key column of a scan (graph filter / repeated variables /
/// union-graph dedup applied row by row; plain blocks counted directly).
fn group_count_scan(ctx: &Ctx, spec: &ScanSpec, vars: &[VarId], naggs: usize) -> Result<Table> {
    let kc = spec.cols[0].0;
    let kcs: Vec<usize> = spec.cols.iter().map(|(k, _)| *k).collect();
    let mut keys: Vec<Id> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let mut bump = |k: u64, n: u64| {
        if keys.last() == Some(&Id(k)) {
            *counts.last_mut().unwrap() += n;
        } else {
            keys.push(Id(k));
            counts.push(n);
        }
    };
    let mut last: Option<[u64; 4]> = None;
    ctx.snap.scan(spec.perm, &spec.prefix, |chunk| {
        let mut row = |k: &[u64; 4]| {
            if !spec.graph.accepts(k[spec.graph_col]) || spec.eqs.iter().any(|&(a, b)| k[a] != k[b])
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
                    let run = col[i..].partition_point(|x| *x == v);
                    bump(v, run as u64);
                    i += run;
                }
            }
            Chunk::Block(b, s, e) => (s..e).for_each(|i| row(&b.key(i))),
            Chunk::Row(k) => row(&k),
        }
        ctx.check()?;
        Ok(true)
    })?;
    let mut t = Table::new(vars.to_vec());
    t.cols[0] = keys;
    let cnt: Vec<Id> = counts
        .iter()
        .map(|&c| Id::from_i64(c as i64).unwrap_or(Id::UNDEF))
        .collect();
    for a in 0..naggs {
        t.cols[1 + a] = cnt.clone();
    }
    t.len = t.cols[0].len();
    Ok(t)
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
    ctx.snap.scan(spec.perm, &spec.prefix, |chunk| {
        let mut row = |k: &[u64; 4]| {
            if spec.graph.accepts(k[spec.graph_col]) && spec.eqs.iter().all(|&(a, b)| k[a] == k[b])
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
fn apply_unary(ctx: &Ctx, n: &Node, mut t: Table) -> Result<Table> {
    Ok(match &n.kind {
        Kind::Filter(exprs) => {
            apply_filter(ctx, &mut t, exprs);
            t
        }
        Kind::Extend(v, e) => {
            let col = compute_column(ctx, &t, e);
            t.vars.push(*v);
            t.cols.push(col);
            t
        }
        Kind::Project(vars) => t.project(vars),
        Kind::Unpack { t: tv, parts } => unpack(ctx, t, *tv, parts, &n.vars)?,
        Kind::Distinct => distinct(t),
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
        | Kind::Distinct => {
            let mut budget = want.max(64);
            loop {
                let (input, cinfo, complete) = execute_limited(ctx, &n.children[0], budget)?;
                let out = apply_unary(ctx, n, input)?;
                if out.len() >= want || complete {
                    return finish(out, vec![cinfo], complete);
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
    let limit = limit.unwrap_or(usize::MAX);
    let mut truncated = false;
    let mut t = Table::new(vars.to_vec());
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
    ctx.snap.scan(spec.perm, &spec.prefix, |chunk| {
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
                    row(&b.key(i), &mut t);
                }
                n += e - s;
            }
            Chunk::Row(k) => {
                row(&k, &mut t);
                n += 1;
            }
        }
        if n > 1 << 16 {
            n = 0;
            ctx.check()?;
            ctx.check_rows(t.len())?;
        }
        if t.len >= limit {
            // there may be more matching rows after this point
            truncated = true;
            return Ok(false);
        }
        Ok(true)
    })?;
    Ok((t, truncated))
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
    if lay.shared.is_empty() {
        for i in 0..l.len() {
            for j in 0..r.len() {
                pairs.push((i as u32, j as u32));
            }
            ctx.check_rows(pairs.len())?;
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
            ctx.check_rows(pairs.len())?;
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
                ctx.check_rows(pairs.len())?;
            }
            match a[i].cmp(&b[j]) {
                Ordering::Less => i = gallop(a, i, b[j]),
                Ordering::Greater => j = gallop(b, j, a[i]),
                Ordering::Equal => {
                    let v = a[i];
                    let ie = i + a[i..].partition_point(|x| *x == v);
                    let je = j + b[j..].partition_point(|x| *x == v);
                    for ii in i..ie {
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
                ctx.check_rows(pairs.len())?;
            }
            if let Some(m) = map.get(v) {
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
                ctx.check_rows(pairs.len())?;
            }
            key.clear();
            key.extend(pcols.iter().map(|&c| pt.cols[c][pi]));
            if let Some(m) = map.get(&key) {
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
    ctx.check_rows(pairs.len())?;
    Ok(materialize(l, r, &lay, &pairs))
}

fn cross(ctx: &Ctx, l: &Table, r: &Table) -> Result<Table> {
    ctx.check_rows(l.len().saturating_mul(r.len()))?;
    let lay = layout(l, r);
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

fn minus(ctx: &Ctx, mut l: Table, r: &Table) -> Result<Table> {
    let lay = layout(&l, r);
    if lay.shared.is_empty() || r.is_empty() {
        return Ok(l);
    }
    let r_undef = lay.shared.iter().any(|&(_, rc)| has_undef(r, rc));
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

fn apply_filter(ctx: &Ctx, t: &mut Table, exprs: &[Expr]) {
    let t0 = Instant::now();
    let keep = match distinct_filter_mask(ctx, t, exprs) {
        Some(keep) => {
            tracing::debug!("filter evaluated per distinct value in {:?}", t0.elapsed());
            keep
        }
        None => filter_mask(ctx, t, exprs),
    };
    let sorted = t.sorted.clone();
    t.filter_rows(&keep);
    t.sorted = sorted;
}

/// Evaluate the filter row by row.
fn filter_mask(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Vec<bool> {
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
    let keep = if t.len() > PAR_THRESHOLD && !exprs.iter().any(|e| e.has_exists()) {
        (0..t.len()).into_par_iter().map(test).collect()
    } else {
        (0..t.len()).map(test).collect()
    };
    tracing::debug!("filter evaluated {} rows in {:?}", t.len(), t0.elapsed());
    keep
}

/// A deterministic filter that reads a single variable has the same outcome for every
/// row holding the same id: evaluate it once per distinct id and look the outcome up per
/// row, instead of decoding and testing every row. Values of a column often repeat
/// (names, labels, categories), and a column sorted on the variable yields its distinct
/// ids as runs without sorting.
fn distinct_filter_mask(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Option<Vec<bool>> {
    if t.len() < 4096
        || !exprs.iter().any(super::expr::needs_values)
        || !exprs.iter().all(super::cache::deterministic)
    {
        return None;
    }
    let mut vars = Vec::new();
    for e in exprs {
        e.vars(&mut vars);
    }
    vars.sort_unstable();
    vars.dedup();
    let &[v] = vars.as_slice() else {
        return None;
    };
    let col = &t.cols[t.col_of(v)?];
    let runs = t.sorted.first() == Some(&v);
    let mut uniq = col.clone();
    if !runs {
        uniq.par_sort_unstable();
    }
    uniq.dedup();
    let (hit, uniq) = match super::keyfilter::KeyFilter::new(exprs, v) {
        Some(kf) => (key_filter_mask(ctx, &kf, &uniq, v, exprs), uniq),
        // the general evaluator gains nothing when (nearly) every value is different
        None if uniq.len() > col.len() / 4 * 3 => return None,
        None => {
            let mut values = Table::new(vec![v]);
            values.len = uniq.len();
            values.cols[0] = uniq;
            let hit = filter_mask(ctx, &values, exprs);
            (hit, std::mem::take(&mut values.cols[0]))
        }
    };
    Some(if runs {
        // `uniq` lists the runs in column order
        let mut j = 0;
        col.iter()
            .map(|id| {
                if *id != uniq[j] {
                    j += 1;
                }
                hit[j]
            })
            .collect()
    } else {
        col.par_iter()
            .map(|id| hit[uniq.binary_search(id).unwrap()])
            .collect()
    })
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
) -> Vec<bool> {
    use crate::id::Tag;
    let vocab = &ctx.snap.generation.vocab;
    let mut hit = vec![false; uniq.len()];
    // raw ids sort by tag first, so the vocabulary ids are one contiguous range
    let lo = uniq.partition_point(|id| id.tag() < Tag::Vocab);
    let hi = lo + uniq[lo..].partition_point(|id| id.tag() == Tag::Vocab);
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
        for (i, h) in rest_at.into_iter().zip(filter_mask(ctx, &rest, exprs)) {
            hit[i] = h;
        }
    }
    hit
}

fn compute_column(ctx: &Ctx, t: &Table, e: &Expr) -> Vec<Id> {
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
    if t.len() > PAR_THRESHOLD && !e.has_exists() {
        (0..t.len()).into_par_iter().map(f).collect()
    } else {
        (0..t.len()).map(f).collect()
    }
}

fn order_by(ctx: &Ctx, t: Table, keys: &[(Expr, bool)], limit: Option<usize>) -> Table {
    let dec = decode_for(ctx, &t, &keys.iter().map(|(e, _)| e).collect::<Vec<_>>());
    let map = t.var_map(ctx.nvars());
    let key_vals: Vec<Vec<Option<Value>>> = keys
        .iter()
        .map(|(e, _)| {
            let f = |i: usize| {
                eval(
                    e,
                    &Row {
                        table: &t,
                        i,
                        map: &map,
                        dec: dec.as_ref(),
                    },
                    ctx,
                )
                .ok()
                .and_then(|v| match v {
                    Val::Id(id) => ctx.value(id),
                    Val::V(v) | Val::Dec(_, v) => Some(v),
                })
            };
            if t.len() > PAR_THRESHOLD {
                (0..t.len()).into_par_iter().map(f).collect()
            } else {
                (0..t.len()).map(f).collect()
            }
        })
        .collect();
    let cmp = |a: &usize, b: &usize| {
        for (k, (_, asc)) in keys.iter().enumerate() {
            let o = order_cmp(key_vals[k][*a].as_ref(), key_vals[k][*b].as_ref());
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
    t.take_rows(&idx)
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

fn group(ctx: &Ctx, t: &Table, keys: &[VarId], aggs: &[(VarId, Agg)]) -> Result<Table> {
    let kcols: Vec<Option<usize>> = keys.iter().map(|k| t.col_of(*k)).collect();
    let mut order: Vec<Vec<Id>> = Vec::new();
    let mut groups: FxHashMap<Vec<Id>, Vec<u32>> = FxHashMap::default();
    for i in 0..t.len() {
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
    for key in order {
        ctx.check()?;
        let rows = &groups[&key];
        let mut row = key.clone();
        for (_, agg) in aggs {
            row.push(aggregate(ctx, t, &map, rows, agg));
        }
        out.push_row(&row);
    }
    Ok(out)
}

fn aggregate(ctx: &Ctx, t: &Table, map: &[Option<usize>], rows: &[u32], agg: &Agg) -> Id {
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
        AggregateFunction::Custom(_) => None,
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
}

impl Graph<'_> {
    fn neighbours(&self, x: u64, forward: bool) -> Result<Vec<u64>> {
        if let Some((p, rev)) = self.spec.simple {
            let dir = forward != rev;
            // forward: (x p ?o) via PSO; backward: (?s p x) via POS
            let perm = if dir { Perm::Pso } else { Perm::Pos };
            let gc = perm.col_of(crate::index::G);
            let mut out = Vec::new();
            self.ctx.snap.scan(perm, &[p, x], |c| {
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
        while !frontier.is_empty() {
            self.ctx.check()?;
            depth += 1;
            let mut next = Vec::new();
            for x in frontier {
                for y in self.neighbours(x, forward)? {
                    if seen.insert(y) {
                        out.push(y);
                        next.push(y);
                    }
                }
            }
            if self.spec.max_one && depth >= 1 {
                break;
            }
            frontier = next;
            self.ctx.check_rows(out.len())?;
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
) -> Result<Table> {
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
                    ctx.check_rows(out.len())?;
                }
            }
        }
    }
    let _ = pad;
    match left {
        Some(l) => {
            let mut t = join_tables(ctx, &l, &out, &[], false)?;
            t = t.project(vars);
            Ok(t)
        }
        None => Ok(out.project(vars)),
    }
}

// ---------------------------------------------------------------- service ------

fn service(ctx: &Ctx, endpoint: &PathEnd, query: &str, vars: &[VarId]) -> Result<Table> {
    if !ctx.allow_service {
        return Err(Error::Service("SERVICE is disabled".into()));
    }
    let PathEnd::Const(id) = endpoint else {
        return Err(Error::unsupported("SERVICE with a variable endpoint"));
    };
    let Some(oxrdf::Term::NamedNode(url)) = ctx.term(*id) else {
        return Err(Error::Service("invalid SERVICE endpoint".into()));
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Service(e.to_string()))?;
    let resp = client
        .post(url.as_str())
        .header(
            "Accept",
            "application/sparql-results+json, application/sparql-results+xml;q=0.8",
        )
        .form(&[("query", query)])
        .send()
        .map_err(|e| Error::Service(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(Error::Service(format!(
            "{} returned {}",
            url.as_str(),
            resp.status()
        )));
    }
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.bytes().map_err(|e| Error::Service(e.to_string()))?;
    let fmt = if ct.contains("xml") {
        sparesults::QueryResultsFormat::Xml
    } else {
        sparesults::QueryResultsFormat::Json
    };
    let parser = sparesults::QueryResultsParser::from_format(fmt);
    let mut t = Table::new(vars.to_vec());
    match parser
        .for_slice(&body)
        .map_err(|e| Error::Service(e.to_string()))?
    {
        sparesults::SliceQueryResultsParserOutput::Solutions(sols) => {
            for sol in sols {
                let sol = sol.map_err(|e| Error::Service(e.to_string()))?;
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
        sparesults::SliceQueryResultsParserOutput::Boolean(_) => {}
    }
    Ok(t)
}
