//! Parallel eager grouping over Rayon partitions.
//!
//! Two strategies share the same accumulators and exact single-pass semantics:
//!
//! 1. **Contiguous range-split with [`Accumulator::merge`]** for ungrouped queries and
//!    low-cardinality groupings whose aggregates are all mergeable. Each worker folds a
//!    contiguous slice of input rows without scattering them, then merges the local
//!    states in slice order, which preserves global first-appearance key order. If a
//!    `SUM` or `AVG` merge is refused to preserve exact rounding or overflow semantics,
//!    only that aggregate is re-folded sequentially using the already-built group index.
//! 2. **Key-hash partitioning** for medium- and high-cardinality groupings and groupings
//!    that hold non-mergeable running aggregates (`DISTINCT`, ARQ statistics). Workers
//!    bucket row indices by a hash of the group keys in one parallel pass; each partition
//!    then folds its disjoint keys in original row order with no cross-partition merge,
//!    and the finished groups are assembled in exact first-appearance order.

use super::acc::{Accumulator, Arg, Batch};
use super::op::{
    CHUNK_ROWS, Index, RETAIN_STEP, Source, accumulator, accumulators, group_bytes_per_entry,
    sources_of, whole_sequential_with_computed,
};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::exec::{PAR_MIN_LEN, PAR_THRESHOLD, compute_column};
use crate::sparql::exprcache::Report as ExprReport;
use crate::sparql::plan::Agg;
use crate::sparql::table::{Table, VarId};
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Smallest eager input that enters parallel grouping.
pub(super) const PAR_GROUP_THRESHOLD: usize = PAR_THRESHOLD;

/// Rows sampled evenly across the input to distinguish tiny-cardinality groupings
/// (range-split + `merge`) from medium/high-cardinality groupings (key-hash partitions).
const SAMPLE_ROWS: usize = 256;

/// Maximum distinct keys seen in the sample to attempt contiguous range-split + `merge`.
const RANGE_SPLIT_SAMPLE_DISTINCT: usize = 64;

/// Per-worker group cap in range-split before aborting to key-hash partitioning.
const RANGE_SPLIT_MAX_GROUPS: usize = 2_048;

/// Rows per L1-friendly folding chunk inside a hash partition.
const HASH_PART_CHUNK_ROWS: usize = 4_096;

/// Bounds on the power-of-two hash partition count.
const MIN_HASH_PARTS: usize = 4;
const MAX_HASH_PARTS: usize = 64;

#[inline]
fn is_memory_budget_error(err: &Error) -> bool {
    matches!(
        err,
        Error::BudgetExceeded(b)
            if matches!(b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory)
    )
}

#[inline]
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn sample_distinct_keys(table: &Table, key_cols: &[Option<usize>], limit: usize) -> usize {
    let step = (table.len / SAMPLE_ROWS).max(1);
    match key_cols {
        [Some(col)] => {
            let col = &table.cols[*col];
            let mut seen: FxHashSet<Id> = FxHashSet::default();
            let mut row = 0;
            while row < table.len {
                seen.insert(col[row]);
                if seen.len() > limit {
                    return seen.len();
                }
                row += step;
            }
            seen.len()
        }
        [Some(c0), Some(c1)] => {
            let (col0, col1) = (&table.cols[*c0], &table.cols[*c1]);
            let mut seen: FxHashSet<(Id, Id)> = FxHashSet::default();
            let mut row = 0;
            while row < table.len {
                seen.insert((col0[row], col1[row]));
                if seen.len() > limit {
                    return seen.len();
                }
                row += step;
            }
            seen.len()
        }
        _ => {
            let mut seen: FxHashSet<Vec<Id>> = FxHashSet::default();
            let mut key = vec![Id::UNDEF; key_cols.len()];
            let mut row = 0;
            while row < table.len {
                for (slot, col) in key.iter_mut().zip(key_cols) {
                    *slot = col.map_or(Id::UNDEF, |c| table.cols[c][row]);
                }
                if !seen.contains(&key) {
                    seen.insert(key.clone());
                    if seen.len() > limit {
                        return seen.len();
                    }
                }
                row += step;
            }
            seen.len()
        }
    }
}

enum WorkerErr {
    Real(Error),
    Memory,
    TooManyGroups,
    Aborted,
}

impl WorkerErr {
    fn from_error(err: Error, abort: &AtomicBool) -> Self {
        abort.store(true, Ordering::Relaxed);
        if is_memory_budget_error(&err) {
            Self::Memory
        } else {
            Self::Real(err)
        }
    }
}

struct LocalState {
    index: Index,
    order: Vec<Id>,
    first_row: Vec<u32>,
    accs: Vec<Box<dyn Accumulator>>,
    groups: usize,
    per_group: u64,
    charge: Option<RetainedCharge>,
    bytes: u64,
    reserved: u64,
}

impl LocalState {
    fn empty(key_count: usize, per_group: u64) -> Self {
        Self {
            index: Index::new(key_count),
            order: Vec::new(),
            first_row: Vec::new(),
            accs: Vec::new(),
            groups: 0,
            per_group,
            charge: None,
            bytes: 0,
            reserved: 0,
        }
    }

    fn new(
        ctx: &Ctx,
        key_count: usize,
        agg_list: &[Agg],
        charged: bool,
        per_group: u64,
    ) -> Result<Self> {
        let base = agg_list.len() as u64 * 256 + key_count as u64 * 64 + 1024;
        let charge = if charged {
            ctx.retained_charge(base)?
        } else {
            None
        };
        let accs = accumulators(ctx, agg_list, charge.is_some());
        Ok(Self {
            index: Index::new(key_count),
            order: Vec::new(),
            first_row: Vec::new(),
            accs,
            groups: 0,
            per_group,
            charge,
            bytes: base,
            reserved: base,
        })
    }

    fn retain(&mut self, bytes: u64) -> Result<()> {
        let retained = self.bytes.saturating_add(bytes);
        if retained > self.reserved {
            let reserved = retained.saturating_add((retained / 8).max(RETAIN_STEP));
            if let Some(charge) = &mut self.charge {
                if charge.resize(reserved).is_err() {
                    charge.resize(retained)?;
                    self.reserved = retained;
                } else {
                    self.reserved = reserved;
                }
            } else {
                self.reserved = reserved;
            }
        }
        self.bytes = retained;
        Ok(())
    }

    fn insert(&mut self, key: &[Id], row: u32) -> Result<u32> {
        let at = self.groups as u32;
        self.retain(self.per_group)?;
        match &mut self.index {
            Index::One(index) => {
                index.insert(key[0], at);
            }
            Index::Many(index) => {
                index.insert(key.into(), at);
            }
        }
        self.order.extend_from_slice(key);
        self.first_row.push(row);
        for acc in &mut self.accs {
            acc.init(1);
        }
        self.groups += 1;
        Ok(at)
    }

    fn insert_ungrouped(&mut self, row: u32) -> Result<u32> {
        let at = self.groups as u32;
        self.retain(self.per_group)?;
        self.first_row.push(row);
        for acc in &mut self.accs {
            acc.init(1);
        }
        self.groups += 1;
        Ok(at)
    }
}

enum RangeOutcome {
    Done(Table),
    TooManyGroups,
    MemoryFallback,
}

/// Parallel eager grouping entry point. Caller guarantees `eager && hashable &&
/// !only_counts_rows && table.len >= PAR_GROUP_THRESHOLD && table.len <= u32::MAX &&
/// rayon::current_num_threads() > 1`.
pub(super) fn whole_parallel(
    ctx: &Ctx,
    table: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
    charged: bool,
    eager: bool,
    report: &mut ExprReport,
) -> Result<Table> {
    ctx.check()?;
    let mut vars = keys.to_vec();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let agg_list: Vec<Agg> = aggs.iter().map(|(_, a)| a.clone()).collect();
    let key_cols: Vec<Option<usize>> = keys
        .iter()
        .map(|k| table.vars.iter().position(|v| v == k))
        .collect();
    let sources = sources_of(&agg_list, &table.vars);

    // Evaluate expression arguments once up front so neither parallel strategy nor any
    // sequential fallback re-evaluates them or duplicates ExprReport entries.
    let expr_count = sources
        .iter()
        .filter(|source| matches!(source, Source::Expr(_)))
        .count();
    let _expr_scratch = if expr_count > 0 {
        Some(ctx.charge(expr_count as u64 * (table.len as u64 * 8 + 64))?)
    } else {
        None
    };
    let computed: Vec<Option<Vec<Id>>> = sources
        .iter()
        .map(|source| match source {
            Source::Expr(expr) => compute_column(ctx, table, expr, report).map(Some),
            _ => Ok(None),
        })
        .collect::<Result<Vec<_>>>()?;

    let probe_accs = accumulators(ctx, &agg_list, charged);
    let all_mergeable = probe_accs.iter().all(|a| a.mergeable());
    let per_group = group_bytes_per_entry(keys.len(), &probe_accs);
    drop(probe_accs);

    let threads = rayon::current_num_threads().max(1);
    let workers = threads.min(table.len / PAR_MIN_LEN).max(1);
    if workers <= 1 {
        return whole_sequential_with_computed(
            ctx, table, keys, aggs, charged, eager, &vars, &computed,
        );
    }

    if keys.is_empty() {
        if all_mergeable {
            match run_range_split(
                ctx, table, &key_cols, &agg_list, &sources, &computed, &vars, charged, eager,
                per_group, workers,
            )? {
                RangeOutcome::Done(out) => return Ok(out),
                RangeOutcome::TooManyGroups | RangeOutcome::MemoryFallback => {}
            }
        }
        return whole_sequential_with_computed(
            ctx, table, keys, aggs, charged, eager, &vars, &computed,
        );
    }

    let sample_distinct = sample_distinct_keys(table, &key_cols, RANGE_SPLIT_SAMPLE_DISTINCT);
    if all_mergeable && sample_distinct <= RANGE_SPLIT_SAMPLE_DISTINCT {
        match run_range_split(
            ctx, table, &key_cols, &agg_list, &sources, &computed, &vars, charged, eager,
            per_group, workers,
        )? {
            RangeOutcome::Done(out) => return Ok(out),
            RangeOutcome::MemoryFallback => {
                return whole_sequential_with_computed(
                    ctx, table, keys, aggs, charged, eager, &vars, &computed,
                );
            }
            RangeOutcome::TooManyGroups => {
                // Fall through to key-hash partitioning for high-cardinality keys.
            }
        }
    } else if !all_mergeable && sample_distinct <= 1 {
        // Every sampled row shares one key and aggregates cannot merge across slices.
        return whole_sequential_with_computed(
            ctx, table, keys, aggs, charged, eager, &vars, &computed,
        );
    }

    if let Some(out) = run_hash_partitioned(
        ctx, table, &key_cols, &agg_list, &sources, &computed, &vars, charged, eager, per_group,
        workers,
    )? {
        return Ok(out);
    }

    whole_sequential_with_computed(ctx, table, keys, aggs, charged, eager, &vars, &computed)
}

#[allow(clippy::too_many_arguments)]
fn run_range_split(
    ctx: &Ctx,
    table: &Table,
    key_cols: &[Option<usize>],
    agg_list: &[Agg],
    sources: &[Source],
    computed: &[Option<Vec<Id>>],
    vars: &[VarId],
    charged: bool,
    eager: bool,
    per_group: u64,
    workers: usize,
) -> Result<RangeOutcome> {
    let nk = key_cols.len();
    let width = vars.len();
    let abort = AtomicBool::new(false);
    let len = table.len;

    let results: Vec<std::result::Result<LocalState, WorkerErr>> = (0..workers)
        .into_par_iter()
        .map(|w| {
            let start = w * len / workers;
            let end = (w + 1) * len / workers;
            if abort.load(Ordering::Relaxed) {
                return Err(WorkerErr::Aborted);
            }
            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;

            let mut local = LocalState::new(ctx, nk, agg_list, charged, per_group)
                .map_err(|e| WorkerErr::from_error(e, &abort))?;
            if nk == 0 {
                local
                    .insert_ungrouped(start as u32)
                    .map_err(|e| WorkerErr::from_error(e, &abort))?;
            }

            let mut slots: Vec<u32> = Vec::new();
            let mut key_buf: Vec<Id> = vec![Id::UNDEF; nk];
            let mut base = start;
            while base < end {
                if abort.load(Ordering::Relaxed) {
                    return Err(WorkerErr::Aborted);
                }
                ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                let chunk_end = end.min(base + CHUNK_ROWS);
                let chunk_len = chunk_end - base;
                let _slot_charge = ctx
                    .charge(chunk_len as u64 * 4 + 64)
                    .map_err(|e| WorkerErr::from_error(e, &abort))?;
                slots.clear();
                slots.reserve(chunk_len);

                if nk == 0 {
                    slots.resize(chunk_len, 0);
                } else if let [Some(col)] = key_cols {
                    let column = &table.cols[*col];
                    for (offset, &id) in column[base..chunk_end].iter().enumerate() {
                        if offset.is_multiple_of(1024) {
                            if abort.load(Ordering::Relaxed) {
                                return Err(WorkerErr::Aborted);
                            }
                            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                        let found = match &local.index {
                            Index::One(index) => index.get(&id).copied(),
                            Index::Many(_) => unreachable!("a one-key index"),
                        };
                        let slot = match found {
                            Some(g) => g,
                            None => {
                                if local.groups >= RANGE_SPLIT_MAX_GROUPS {
                                    abort.store(true, Ordering::Relaxed);
                                    return Err(WorkerErr::TooManyGroups);
                                }
                                local
                                    .insert(&[id], (base + offset) as u32)
                                    .map_err(|e| WorkerErr::from_error(e, &abort))?
                            }
                        };
                        slots.push(slot);
                    }
                } else {
                    for row in base..chunk_end {
                        if (row - base).is_multiple_of(1024) {
                            if abort.load(Ordering::Relaxed) {
                                return Err(WorkerErr::Aborted);
                            }
                            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                        for (k, col) in key_buf.iter_mut().zip(key_cols) {
                            *k = col.map_or(Id::UNDEF, |c| table.cols[c][row]);
                        }
                        let found = match &local.index {
                            Index::One(index) => index.get(&key_buf[0]).copied(),
                            Index::Many(index) => index.get(key_buf.as_slice()).copied(),
                        };
                        let slot = match found {
                            Some(g) => g,
                            None => {
                                if local.groups >= RANGE_SPLIT_MAX_GROUPS {
                                    abort.store(true, Ordering::Relaxed);
                                    return Err(WorkerErr::TooManyGroups);
                                }
                                local
                                    .insert(&key_buf, row as u32)
                                    .map_err(|e| WorkerErr::from_error(e, &abort))?
                            }
                        };
                        slots.push(slot);
                    }
                }

                for (a, values) in computed.iter().enumerate() {
                    if abort.load(Ordering::Relaxed) {
                        return Err(WorkerErr::Aborted);
                    }
                    ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                    let arg = match (&sources[a], values) {
                        (_, Some(values)) => Arg::Column(&values[base..chunk_end]),
                        (Source::Star | Source::Rows, _) => Arg::Star,
                        (Source::Column(Some(c)), _) => {
                            Arg::Column(&table.cols[*c][base..chunk_end])
                        }
                        (Source::Column(None), _) => Arg::Unbound,
                        (Source::Expr(_), None) => unreachable!("a computed argument"),
                    };
                    let batch = Batch {
                        table,
                        arg,
                        rows: None,
                        base,
                    };
                    local.accs[a]
                        .add_batch(ctx, &batch, &slots)
                        .map_err(|e| WorkerErr::from_error(e, &abort))?;
                    let growth = local.accs[a].take_growth();
                    if growth > 0 {
                        local
                            .retain(growth)
                            .map_err(|e| WorkerErr::from_error(e, &abort))?;
                    }
                }
                base = chunk_end;
            }
            Ok(local)
        })
        .collect();

    let mut parts: Vec<LocalState> = Vec::with_capacity(workers);
    let mut saw_memory = false;
    let mut saw_too_many = false;
    for res in results {
        match res {
            Ok(state) => parts.push(state),
            Err(WorkerErr::Real(err)) => return Err(err),
            Err(WorkerErr::Memory) => saw_memory = true,
            Err(WorkerErr::TooManyGroups) => saw_too_many = true,
            Err(WorkerErr::Aborted) => {}
        }
    }
    if saw_memory {
        return Ok(RangeOutcome::MemoryFallback);
    }
    if saw_too_many {
        return Ok(RangeOutcome::TooManyGroups);
    }

    let mut iter = parts.into_iter();
    let mut merged = iter.next().expect("at least one worker");
    ctx.check_rows(merged.groups)?;
    if eager && let Err(e) = ctx.check_output(merged.groups, width) {
        if is_memory_budget_error(&e) {
            return Ok(RangeOutcome::MemoryFallback);
        }
        return Err(e);
    }

    let mut refused = vec![false; agg_list.len()];
    let mut into: Vec<u32> = Vec::new();
    for mut part in iter {
        ctx.check()?;
        into.clear();
        into.reserve(part.groups);
        if nk == 0 {
            into.push(0);
        } else {
            for g in 0..part.groups {
                let key = &part.order[g * nk..(g + 1) * nk];
                let found = match &merged.index {
                    Index::One(index) => index.get(&key[0]).copied(),
                    Index::Many(index) => index.get(key).copied(),
                };
                let slot = match found {
                    Some(slot) => slot,
                    None => {
                        ctx.check_rows(merged.groups.saturating_add(1))?;
                        if eager
                            && let Err(e) = ctx.check_output(merged.groups.saturating_add(1), width)
                        {
                            if is_memory_budget_error(&e) {
                                return Ok(RangeOutcome::MemoryFallback);
                            }
                            return Err(e);
                        }
                        match merged.insert(key, part.first_row[g]) {
                            Ok(slot) => slot,
                            Err(e) if is_memory_budget_error(&e) => {
                                return Ok(RangeOutcome::MemoryFallback);
                            }
                            Err(e) => return Err(e),
                        }
                    }
                };
                into.push(slot);
            }
        }
        for (a, is_refused) in refused.iter_mut().enumerate() {
            if *is_refused {
                continue;
            }
            match merged.accs[a].merge(ctx, &mut *part.accs[a], &into) {
                Ok(()) => {
                    let growth = merged.accs[a].take_growth();
                    if growth > 0
                        && let Err(e) = merged.retain(growth)
                    {
                        if is_memory_budget_error(&e) {
                            return Ok(RangeOutcome::MemoryFallback);
                        }
                        return Err(e);
                    }
                }
                Err(Error::Unsupported(_)) => {
                    *is_refused = true;
                }
                Err(e) if is_memory_budget_error(&e) => {
                    return Ok(RangeOutcome::MemoryFallback);
                }
                Err(e) => return Err(e),
            }
        }
    }

    // Re-fold only the aggregates whose merge was refused (e.g. non-associative float/double
    // SUM/AVG or right-side numeric overflow), reusing the already-built global group index.
    if refused.iter().any(|&r| r) {
        let track = merged.charge.is_some();
        for (a, &is_refused) in refused.iter().enumerate() {
            if is_refused {
                let mut acc = accumulator(ctx, &agg_list[a], track);
                acc.init(merged.groups);
                merged.accs[a] = acc;
            }
        }
        let mut slots: Vec<u32> = Vec::new();
        let mut key_buf: Vec<Id> = vec![Id::UNDEF; nk];
        let mut base = 0;
        while base < len {
            ctx.check()?;
            let chunk_end = len.min(base + CHUNK_ROWS);
            let chunk_len = chunk_end - base;
            let _slot_charge = match ctx.charge(chunk_len as u64 * 4 + 64) {
                Ok(c) => c,
                Err(e) if is_memory_budget_error(&e) => {
                    return Ok(RangeOutcome::MemoryFallback);
                }
                Err(e) => return Err(e),
            };
            slots.clear();
            slots.reserve(chunk_len);
            if nk == 0 {
                slots.resize(chunk_len, 0);
            } else if let [Some(col)] = key_cols {
                let column = &table.cols[*col];
                let Index::One(index) = &merged.index else {
                    unreachable!("a one-key index")
                };
                for (offset, &id) in column[base..chunk_end].iter().enumerate() {
                    if offset.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    slots.push(*index.get(&id).expect("group already indexed"));
                }
            } else {
                for row in base..chunk_end {
                    if (row - base).is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    for (k, col) in key_buf.iter_mut().zip(key_cols) {
                        *k = col.map_or(Id::UNDEF, |c| table.cols[c][row]);
                    }
                    let slot = match &merged.index {
                        Index::One(index) => *index.get(&key_buf[0]).expect("group indexed"),
                        Index::Many(index) => {
                            *index.get(key_buf.as_slice()).expect("group indexed")
                        }
                    };
                    slots.push(slot);
                }
            }
            for (a, &is_refused) in refused.iter().enumerate() {
                if !is_refused {
                    continue;
                }
                ctx.check()?;
                let arg = match (&sources[a], &computed[a]) {
                    (_, Some(values)) => Arg::Column(&values[base..chunk_end]),
                    (Source::Star | Source::Rows, _) => Arg::Star,
                    (Source::Column(Some(c)), _) => Arg::Column(&table.cols[*c][base..chunk_end]),
                    (Source::Column(None), _) => Arg::Unbound,
                    (Source::Expr(_), None) => unreachable!("a computed argument"),
                };
                let batch = Batch {
                    table,
                    arg,
                    rows: None,
                    base,
                };
                if let Err(e) = merged.accs[a].add_batch(ctx, &batch, &slots) {
                    if is_memory_budget_error(&e) {
                        return Ok(RangeOutcome::MemoryFallback);
                    }
                    return Err(e);
                }
                let growth = merged.accs[a].take_growth();
                if growth > 0
                    && let Err(e) = merged.retain(growth)
                {
                    if is_memory_budget_error(&e) {
                        return Ok(RangeOutcome::MemoryFallback);
                    }
                    return Err(e);
                }
            }
            base = chunk_end;
        }
    }

    ctx.check()?;
    ctx.check_rows(merged.groups)?;
    if eager && let Err(e) = ctx.check_output(merged.groups, width) {
        if is_memory_budget_error(&e) {
            return Ok(RangeOutcome::MemoryFallback);
        }
        return Err(e);
    }

    let n = merged.groups;
    if n == 0 {
        return Ok(RangeOutcome::Done(Table::new(vars.to_vec())));
    }
    let mut out = Table::new(vars.to_vec());
    for col in &mut out.cols {
        col.reserve_exact(n);
    }
    for k in 0..nk {
        out.cols[k].extend((0..n).map(|g| merged.order[g * nk + k]));
    }
    for (a, acc) in merged.accs.iter_mut().enumerate() {
        ctx.check()?;
        for g in 0..n {
            let id = acc.finish(ctx, g)?;
            out.cols[nk + a].push(id);
        }
    }
    out.len = n;
    Ok(RangeOutcome::Done(out))
}

#[allow(clippy::too_many_arguments)]
fn run_hash_partitioned(
    ctx: &Ctx,
    table: &Table,
    key_cols: &[Option<usize>],
    agg_list: &[Agg],
    sources: &[Source],
    computed: &[Option<Vec<Id>>],
    vars: &[VarId],
    charged: bool,
    eager: bool,
    per_group: u64,
    workers: usize,
) -> Result<Option<Table>> {
    let nk = key_cols.len();
    let width = vars.len();
    let len = table.len;
    let parts_count = (workers * 2)
        .next_power_of_two()
        .clamp(MIN_HASH_PARTS, MAX_HASH_PARTS);
    let part_mask = parts_count - 1;

    // Pre-admit worst-case radix bucket scratch; fall back to sequential if memory refuses.
    let radix_bytes = (len as u64)
        .saturating_mul(8)
        .saturating_add((workers * parts_count) as u64 * 64 + 4096);
    let radix_charge = match ctx.charge(radix_bytes) {
        Ok(c) => c,
        Err(e) if is_memory_budget_error(&e) => return Ok(None),
        Err(e) => return Err(e),
    };

    // Pass 1: bucket row indices into `parts_count` disjoint key-hash partitions per worker.
    let reserve_per_bucket = ((len / workers) / parts_count).clamp(16, 4_096);
    let abort = AtomicBool::new(false);
    let bucket_results: Vec<std::result::Result<Vec<Vec<u32>>, WorkerErr>> = (0..workers)
        .into_par_iter()
        .map(|w| {
            let start = w * len / workers;
            let end = (w + 1) * len / workers;
            if abort.load(Ordering::Relaxed) {
                return Err(WorkerErr::Aborted);
            }
            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
            let mut buckets: Vec<Vec<u32>> = (0..parts_count)
                .map(|_| Vec::with_capacity(reserve_per_bucket))
                .collect();
            match key_cols {
                [Some(c)] => {
                    let col = &table.cols[*c];
                    for (offset, &id) in col[start..end].iter().enumerate() {
                        if offset.is_multiple_of(4096) {
                            if abort.load(Ordering::Relaxed) {
                                return Err(WorkerErr::Aborted);
                            }
                            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                        let p = (mix64(id.0) as usize) & part_mask;
                        buckets[p].push((start + offset) as u32);
                    }
                }
                [Some(c0), Some(c1)] => {
                    let (col0, col1) = (&table.cols[*c0], &table.cols[*c1]);
                    for row in start..end {
                        if (row - start).is_multiple_of(4096) {
                            if abort.load(Ordering::Relaxed) {
                                return Err(WorkerErr::Aborted);
                            }
                            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                        let h = mix64(mix64(col0[row].0) ^ col1[row].0);
                        buckets[(h as usize) & part_mask].push(row as u32);
                    }
                }
                _ => {
                    for row in start..end {
                        if (row - start).is_multiple_of(4096) {
                            if abort.load(Ordering::Relaxed) {
                                return Err(WorkerErr::Aborted);
                            }
                            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                        let h = crate::sparql::cursor::row_hash(
                            0,
                            key_cols
                                .iter()
                                .map(|k| k.map_or(Id::UNDEF, |c| table.cols[c][row])),
                        );
                        buckets[(h as usize) & part_mask].push(row as u32);
                    }
                }
            }
            Ok(buckets)
        })
        .collect();

    let mut worker_buckets: Vec<Vec<Vec<u32>>> = Vec::with_capacity(workers);
    let mut saw_memory = false;
    for res in bucket_results {
        match res {
            Ok(b) => worker_buckets.push(b),
            Err(WorkerErr::Real(e)) => return Err(e),
            Err(WorkerErr::Memory) => saw_memory = true,
            Err(WorkerErr::TooManyGroups | WorkerErr::Aborted) => {}
        }
    }
    if saw_memory {
        return Ok(None);
    }

    // Pass 2: fold each partition independently in worker order (exact original row order).
    let has_distinct_star = agg_list.iter().any(|a| a.distinct && a.expr.is_none());
    let global_groups = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);

    let part_results: Vec<std::result::Result<LocalState, WorkerErr>> = (0..parts_count)
        .into_par_iter()
        .map(|p| {
            let total_rows: usize = worker_buckets.iter().map(|wb| wb[p].len()).sum();
            if total_rows == 0 {
                return Ok(LocalState::empty(nk, per_group));
            }
            if abort.load(Ordering::Relaxed) {
                return Err(WorkerErr::Aborted);
            }
            ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;

            let chunk_cap = total_rows.min(HASH_PART_CHUNK_ROWS);
            let chunk_scratch = (chunk_cap as u64)
                .saturating_mul(
                    4 + 8
                        + if has_distinct_star {
                            table.cols.len() as u64 * 8
                        } else {
                            0
                        },
                )
                .saturating_add(256);
            let _chunk_charge = ctx
                .charge(chunk_scratch)
                .map_err(|e| WorkerErr::from_error(e, &abort))?;

            let mut local = LocalState::new(ctx, nk, agg_list, charged, per_group)
                .map_err(|e| WorkerErr::from_error(e, &abort))?;
            let mut slots: Vec<u32> = Vec::with_capacity(chunk_cap);
            let mut vals: Vec<Id> = Vec::with_capacity(chunk_cap);
            let mut key_buf: Vec<Id> = vec![Id::UNDEF; nk];
            let mut chunk_table = if has_distinct_star {
                Some(Table::new(table.vars.clone()))
            } else {
                None
            };
            let mut uncounted_groups: usize = 0;

            for wb in &worker_buckets {
                for chunk in wb[p].chunks(HASH_PART_CHUNK_ROWS) {
                    if abort.load(Ordering::Relaxed) {
                        return Err(WorkerErr::Aborted);
                    }
                    ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                    slots.clear();

                    if let [Some(c)] = key_cols {
                        let col = &table.cols[*c];
                        for (i, &r) in chunk.iter().enumerate() {
                            if i.is_multiple_of(1024) {
                                if abort.load(Ordering::Relaxed) {
                                    return Err(WorkerErr::Aborted);
                                }
                                ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                            }
                            let id = col[r as usize];
                            let found = match &local.index {
                                Index::One(index) => index.get(&id).copied(),
                                Index::Many(_) => unreachable!("a one-key index"),
                            };
                            let slot = match found {
                                Some(g) => g,
                                None => {
                                    let g = local
                                        .insert(&[id], r)
                                        .map_err(|e| WorkerErr::from_error(e, &abort))?;
                                    uncounted_groups += 1;
                                    if uncounted_groups >= 64 {
                                        let total = global_groups
                                            .fetch_add(uncounted_groups, Ordering::Relaxed)
                                            + uncounted_groups;
                                        uncounted_groups = 0;
                                        ctx.check_rows(total)
                                            .map_err(|e| WorkerErr::from_error(e, &abort))?;
                                        if eager {
                                            ctx.check_output(total, width)
                                                .map_err(|e| WorkerErr::from_error(e, &abort))?;
                                        }
                                    }
                                    g
                                }
                            };
                            slots.push(slot);
                        }
                    } else {
                        for (i, &r) in chunk.iter().enumerate() {
                            if i.is_multiple_of(1024) {
                                if abort.load(Ordering::Relaxed) {
                                    return Err(WorkerErr::Aborted);
                                }
                                ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                            }
                            let row = r as usize;
                            for (k, col) in key_buf.iter_mut().zip(key_cols) {
                                *k = col.map_or(Id::UNDEF, |c| table.cols[c][row]);
                            }
                            let found = match &local.index {
                                Index::One(index) => index.get(&key_buf[0]).copied(),
                                Index::Many(index) => index.get(key_buf.as_slice()).copied(),
                            };
                            let slot = match found {
                                Some(g) => g,
                                None => {
                                    let g = local
                                        .insert(&key_buf, r)
                                        .map_err(|e| WorkerErr::from_error(e, &abort))?;
                                    uncounted_groups += 1;
                                    if uncounted_groups >= 64 {
                                        let total = global_groups
                                            .fetch_add(uncounted_groups, Ordering::Relaxed)
                                            + uncounted_groups;
                                        uncounted_groups = 0;
                                        ctx.check_rows(total)
                                            .map_err(|e| WorkerErr::from_error(e, &abort))?;
                                        if eager {
                                            ctx.check_output(total, width)
                                                .map_err(|e| WorkerErr::from_error(e, &abort))?;
                                        }
                                    }
                                    g
                                }
                            };
                            slots.push(slot);
                        }
                    }

                    if uncounted_groups > 0 {
                        let total = global_groups.fetch_add(uncounted_groups, Ordering::Relaxed)
                            + uncounted_groups;
                        uncounted_groups = 0;
                        ctx.check_rows(total)
                            .map_err(|e| WorkerErr::from_error(e, &abort))?;
                        if eager {
                            ctx.check_output(total, width)
                                .map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                    }

                    if let Some(ct) = &mut chunk_table {
                        for (to, from) in ct.cols.iter_mut().zip(&table.cols) {
                            to.clear();
                            to.extend(chunk.iter().map(|&r| from[r as usize]));
                        }
                        ct.len = chunk.len();
                    }

                    for (a, values) in computed.iter().enumerate() {
                        if abort.load(Ordering::Relaxed) {
                            return Err(WorkerErr::Aborted);
                        }
                        ctx.check().map_err(|e| WorkerErr::from_error(e, &abort))?;
                        let batch = match (&sources[a], values) {
                            (_, Some(values)) => {
                                vals.clear();
                                vals.extend(chunk.iter().map(|&r| values[r as usize]));
                                Batch {
                                    table,
                                    arg: Arg::Column(&vals),
                                    rows: None,
                                    base: 0,
                                }
                            }
                            (Source::Column(Some(c)), _) => {
                                let col = &table.cols[*c];
                                vals.clear();
                                vals.extend(chunk.iter().map(|&r| col[r as usize]));
                                Batch {
                                    table,
                                    arg: Arg::Column(&vals),
                                    rows: None,
                                    base: 0,
                                }
                            }
                            (Source::Column(None), _) => Batch {
                                table,
                                arg: Arg::Unbound,
                                rows: None,
                                base: 0,
                            },
                            (Source::Star | Source::Rows, _) => Batch {
                                table: if agg_list[a].distinct {
                                    chunk_table.as_ref().expect("gathered chunk table")
                                } else {
                                    table
                                },
                                arg: Arg::Star,
                                rows: None,
                                base: 0,
                            },
                            (Source::Expr(_), None) => unreachable!("a computed argument"),
                        };
                        local.accs[a]
                            .add_batch(ctx, &batch, &slots)
                            .map_err(|e| WorkerErr::from_error(e, &abort))?;
                        let growth = local.accs[a].take_growth();
                        if growth > 0 {
                            local
                                .retain(growth)
                                .map_err(|e| WorkerErr::from_error(e, &abort))?;
                        }
                    }
                }
            }
            Ok(local)
        })
        .collect();

    // Drop radix buckets and their charge before assembling the output table.
    drop(worker_buckets);
    drop(radix_charge);

    let mut parts: Vec<LocalState> = Vec::with_capacity(parts_count);
    let mut saw_memory = false;
    for res in part_results {
        match res {
            Ok(state) => parts.push(state),
            Err(WorkerErr::Real(e)) => return Err(e),
            Err(WorkerErr::Memory) => saw_memory = true,
            Err(WorkerErr::TooManyGroups | WorkerErr::Aborted) => {}
        }
    }
    if saw_memory {
        return Ok(None);
    }

    // Pass 3: deterministic assembly in exact first-appearance order.
    let total_groups: usize = parts.iter().map(|p| p.groups).sum();
    ctx.check()?;
    ctx.check_rows(total_groups)?;
    if eager && let Err(e) = ctx.check_output(total_groups, width) {
        if is_memory_budget_error(&e) {
            return Ok(None);
        }
        return Err(e);
    }
    if total_groups == 0 {
        return Ok(Some(Table::new(vars.to_vec())));
    }

    let _order_charge = match ctx.charge(total_groups as u64 * 12 + 64) {
        Ok(c) => c,
        Err(e) if is_memory_budget_error(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut order_map: Vec<(u32, u16, u32)> = Vec::with_capacity(total_groups);
    for (p_idx, part) in parts.iter().enumerate() {
        for (g, &first_row) in part.first_row.iter().enumerate() {
            order_map.push((first_row, p_idx as u16, g as u32));
        }
    }
    if total_groups >= PAR_GROUP_THRESHOLD {
        order_map.par_sort_unstable_by_key(|&(row, _, _)| row);
    } else {
        order_map.sort_unstable_by_key(|&(row, _, _)| row);
    }

    let mut out = Table::new(vars.to_vec());
    for col in &mut out.cols {
        col.reserve_exact(total_groups);
    }
    for k in 0..nk {
        out.cols[k].extend(
            order_map
                .iter()
                .map(|&(_, p, g)| parts[p as usize].order[g as usize * nk + k]),
        );
    }
    for a in 0..agg_list.len() {
        ctx.check()?;
        for &(_, p, g) in &order_map {
            let id = parts[p as usize].accs[a].finish(ctx, g as usize)?;
            out.cols[nk + a].push(id);
        }
    }
    out.len = total_groups;
    Ok(Some(out))
}
