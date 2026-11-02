//! Pure scan predicates can amortize dictionary work over a larger, charged
//! input buffer without increasing the caller's output batch or demand prefix.
//!
//! When the predicate reads only the scan's first free key column, the filter reads its
//! scan itself in morsels of whole index blocks. The tasks of a morsel each take a piece
//! of a block, test each run of the column's values once and copy out the rows that pass,
//! in parallel, as eager execution filters a whole scan.
use super::{Buffer, CursorOptions, Operator, State, copy_rows, exec};
use crate::error::{BudgetKind, Error, Result};
use crate::index::Key;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report;
use crate::sparql::keyfilter::KeyFilter;
use crate::sparql::plan::{Kind, Node, ScanSpec};
use crate::sparql::table::VarId;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

const INPUT_ROWS: usize = 32_768;
/// The largest input the filter reads at once. Its input doubles from [`INPUT_ROWS`]
/// with each pull up to this size, so a short answer still starts after one small read
/// while a long scan pays for fewer parallel key tests. With 32,768 rows per read, waking
/// the pool and copying the filter for each read took about a sixth of the time of a
/// substring filter over a million names on 12 cores.
const MAX_INPUT_ROWS: usize = 1 << 18;

pub(super) fn eligible(node: &Node, children: &[Operator]) -> bool {
    let Kind::Filter(exprs) = &node.kind else {
        return false;
    };
    // Never evaluate an application callback or impure input beyond demand.
    if !matches!(children, [Operator { state: State::Scan(scan), .. }] if scan.filter.is_empty()) {
        return false;
    }
    let refs = exprs.iter().collect::<Vec<_>>();
    let Ok(Some(variable)) = crate::sparql::exprcache::input(&refs) else {
        return false;
    };
    KeyFilter::cursor_scratch_bytes(exprs, variable, INPUT_ROWS).is_some()
}

pub(super) struct Filter {
    node: Node,
    pending: Option<Buffer>,
    at: usize,
    /// the rows of the next enlarged read
    input: usize,
    /// Input demand after consecutive batches without a qualifying row. The first
    /// pull still honors a short LIMIT or ASK prefix.
    empty_demand: usize,
    /// Whether the first read has chosen between morsels and the scan's own batches.
    chosen: bool,
    morsels: Option<Box<Morsels>>,
}

impl Filter {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            pending: None,
            at: 0,
            input: INPUT_ROWS,
            empty_demand: 0,
            chosen: false,
            morsels: None,
        }
    }

    pub(super) fn done(&self, child_done: bool) -> bool {
        child_done && self.pending.is_none() && self.morsels.as_ref().is_none_or(|m| m.rows == 0)
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        loop {
            ctx.check()?;
            if let Some(buffer) = &self.pending {
                if self.at == 0 && buffer.table.len <= cap {
                    return Ok(self.pending.take());
                }
                let batch = copy_rows(ctx, &buffer.table, self.at, cap)?;
                self.at += batch.as_ref().map_or(0, |b| b.table.len);
                if self.at == buffer.table.len {
                    self.pending = None;
                    self.at = 0;
                }
                return Ok(batch);
            }
            if let Some(morsels) = &mut self.morsels
                && morsels.rows > 0
            {
                let batch = morsels.emit(ctx, child, cap)?;
                // The pool of filter copies and their regex caches go with the last rows.
                if morsels.rows == 0 && child.done {
                    self.morsels = None;
                }
                return Ok(Some(batch));
            }
            if child.done {
                self.morsels = None;
                return Ok(None);
            }
            let row_bytes = (child.vars.len() as u64 * 16).saturating_add(192);
            // Include simultaneous input, pending/output copies, key-test reuse,
            // and the decoded block. Every actual allocation is also charged.
            let headroom = |rows: usize| {
                (rows as u64)
                    .saturating_mul(row_bytes)
                    .saturating_add(crate::index::BLOCK_ROWS as u64 * 32 + 8192)
            };
            let fits = |rows: usize| {
                ctx.max_rows.saturating_sub(child.rows) >= rows
                    && ctx.memory_remaining() >= headroom(rows)
            };
            // A read larger than the first is taken only when its headroom fits too.
            let mut input = self.input;
            while input > INPUT_ROWS && (!ctx.opt.grow_input || !fits(input)) {
                input /= 2;
            }
            let enlarge = (cap >= 4096 || self.empty_demand >= 4096)
                && ctx.max_rows_produced == u64::MAX
                && fits(input);
            if enlarge {
                self.input = (input * 2).min(MAX_INPUT_ROWS);
            }
            // The first read decides whether the filter reads its scan in morsels. A
            // first demand under 4096 rows, such as a small LIMIT, keeps the scan's own
            // batches, which stop at the demand.
            if !self.chosen {
                self.chosen = true;
                if enlarge {
                    self.morsels = Morsels::new(ctx, &self.node, child).map(Box::new);
                }
            }
            if let Some(morsels) = &mut self.morsels {
                // A morsel holds at least one whole block. Without room for one, the
                // scan's own batches continue from the next unread block.
                if !fits(INPUT_ROWS) || !morsels.read(ctx, child, input)? {
                    morsels.resume(ctx, child);
                    self.morsels = None;
                }
                continue;
            }
            let mut input_options = options.clone();
            let demand = if enlarge {
                input.max(cap)
            } else if self.empty_demand > cap && fits(self.empty_demand) {
                self.empty_demand
            } else {
                cap
            };
            input_options.batch_rows = demand.max(options.batch_rows);
            let Some(mut buffer) = child.next(ctx, &input_options, demand)? else {
                return Ok(None);
            };
            let scratch = OwnedCharge::new(
                ctx,
                buffer.table.len as u64 * 16 + ctx.nvars() as u64 * 16 + 128,
            )?;
            buffer.table =
                exec::apply_unary(ctx, &self.node, buffer.table, &mut Report::default())?;
            buffer.reconcile()?;
            drop(scratch);
            if !buffer.table.is_empty() {
                self.empty_demand = 0;
                self.pending = Some(buffer);
            } else if ctx.opt.grow_input && ctx.max_rows_produced == u64::MAX {
                let next = demand.saturating_mul(4).min(INPUT_ROWS);
                if ctx.max_rows.saturating_sub(child.rows) >= next
                    && ctx.memory_remaining() >= headroom(next)
                {
                    self.empty_demand = next;
                }
            }
        }
    }
}

/// A filter's own reads of its scan, a morsel of whole index blocks at a time, filtered
/// by [`exec::filter_scan_morsel`]. The cursor holds one morsel's output at a time, in
/// the parts its tasks copied, and writes its batches from them.
struct Morsels {
    spec: ScanSpec,
    key: VarId,
    on_key: Vec<Expr>,
    filter: KeyFilter,
    /// copies of the filter that the parallel tasks reuse, with warm regex caches
    filters: parking_lot::Mutex<Vec<KeyFilter>>,
    /// the key column of each output variable
    cols: Vec<usize>,
    /// the scan's key ranges
    ranges: Vec<(Key, Key)>,
    /// the key ranges whose values can pass, from the filter's fixed start if it has one
    passing: Vec<(Key, Key)>,
    /// the current range, and its next unread block once the range has been started
    at: usize,
    block: Option<usize>,
    /// the rows of the last morsel not yet written, from row `offset` of the first part
    parts: VecDeque<exec::MorselPart>,
    offset: usize,
    rows: usize,
    held: Option<OwnedCharge>,
}

impl Morsels {
    /// Morsels for a filter over a scan that has not been read, when the scan's rows are
    /// sorted on the one variable the filter reads, every range is exact, no delta key
    /// can interleave with the blocks and no union-graph dedup compares neighbours.
    fn new(ctx: &Ctx, node: &Node, child: &Operator) -> Option<Self> {
        if !ctx.opt.filter_scan_runs || !ctx.snap.delta.is_empty() || child.rows != 0 {
            return None;
        }
        let (State::Scan(scan), Kind::Filter(exprs)) = (&child.state, &node.kind) else {
            return None;
        };
        let spec = &scan.spec;
        if !scan.filter.is_empty()
            || scan.at != 0
            || scan.position.is_some()
            || scan.base_checked
            || scan.ranges.iter().any(|r| !r.2)
        {
            return None;
        }
        // Every conjunct is a key test on the key variable, which `eligible` requires.
        let key = exec::run_filter_split(spec, exprs)?;
        let on_key = exprs.clone();
        let filter = KeyFilter::new_for(ctx, &on_key, key)?;
        let cols = child
            .vars
            .iter()
            .map(|v| spec.cols.iter().find(|(_, x)| x == v).map(|c| c.0))
            .collect::<Option<Vec<_>>>()?;
        let passing = exec::key_ranges(ctx, spec, Some(&filter));
        Some(Self {
            spec: spec.clone(),
            key,
            on_key,
            filter,
            filters: parking_lot::Mutex::new(Vec::new()),
            cols,
            ranges: scan.ranges.iter().map(|&(lo, hi, _)| (lo, hi)).collect(),
            passing,
            at: 0,
            block: None,
            parts: VecDeque::new(),
            offset: 0,
            rows: 0,
            held: None,
        })
    }

    /// The next morsel's key range and the rows of its blocks: whole blocks of the
    /// current range from its next unread one, at least one and at least `want` rows
    /// when the range has them. `None` when every range has been read.
    fn take(&mut self, ctx: &Ctx, want: usize) -> Option<(Key, Key, usize)> {
        let index = ctx.snap.perm(self.spec.perm);
        while let Some(&(lo, hi)) = self.ranges.get(self.at) {
            let (b0, b1) = index.key_block_range(&lo, &hi);
            let start = self.block.unwrap_or(b0);
            if start >= b1 {
                self.at += 1;
                self.block = None;
                continue;
            }
            let mut end = start;
            let mut rows = 0;
            while end < b1 && (end == start || rows < want) {
                rows += index.blocks[end].rows as usize;
                end += 1;
            }
            // Keys are unique and blocks disjoint, so the morsel's bounds are its first
            // and last blocks' keys, clipped to the range.
            let first = if start == b0 {
                lo
            } else {
                lo.max(index.blocks[start].first)
            };
            let last = if end == b1 {
                hi
            } else {
                hi.min(index.blocks[end - 1].last)
            };
            if end == b1 {
                self.at += 1;
                self.block = None;
            } else {
                self.block = Some(end);
            }
            return Some((first, last, rows));
        }
        None
    }

    /// Read and filter the next morsel of about `want` rows into the parts. `false` when
    /// the memory budget cannot hold the morsel, which leaves the morsel unread for the
    /// scan's own batches.
    fn read(&mut self, ctx: &Arc<Ctx>, child: &mut Operator, want: usize) -> Result<bool> {
        debug_assert_eq!(self.rows, 0);
        let began = Instant::now();
        let (at, block) = (self.at, self.block);
        let Some((lo, hi, rows)) = self.take(ctx, want) else {
            child.done = true;
            return Ok(true);
        };
        // A charge the budget refuses gives the morsel back. It is not a sticky failure,
        // so the scan's own smaller batches may still fit.
        let refused = |e: &Error| {
            matches!(e, Error::BudgetExceeded(b)
                if matches!(b.kind, BudgetKind::Memory | BudgetKind::ServerMemory))
        };
        let ranges: Vec<(Key, Key)> = self
            .passing
            .iter()
            .filter_map(|&(a, b)| {
                let (a, b) = (a.max(lo), b.min(hi));
                (a <= b).then_some((a, b))
            })
            .collect();
        // The parts the tasks copy and the decoded blocks the tasks hold. A worker holds
        // the block it reads, and one that waits for a piece another worker took can
        // start a further block meanwhile, so up to two blocks per worker, and no more
        // than the morsel has. Each task also charges its own runs and key scratch.
        let width = self.cols.len().max(1) as u64;
        let blocks = rows.div_ceil(crate::index::BLOCK_ROWS).max(1) as u64;
        let live = blocks.min(2 * rayon::current_num_threads().max(1) as u64);
        let scratch = match OwnedCharge::new(
            ctx,
            (rows as u64)
                .saturating_mul(width * 8)
                .saturating_add(live.saturating_mul(crate::index::BLOCK_ROWS as u64 * 32))
                .saturating_add(4096),
        ) {
            Ok(scratch) => scratch,
            Err(e) if refused(&e) => {
                (self.at, self.block) = (at, block);
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        let (parts, read) = if ranges.is_empty() {
            (Vec::new(), 0)
        } else {
            exec::filter_scan_morsel(
                ctx,
                &self.spec,
                &self.cols,
                self.key,
                &self.on_key,
                &self.filter,
                &self.filters,
                &ranges,
            )?
            .ok_or_else(|| Error::invalid("a cursor morsel overlaps the delta"))?
        };
        let held = parts
            .iter()
            .flat_map(|p| p.iter().map(|c| c.capacity() as u64 * 8))
            .sum::<u64>()
            .saturating_add(parts.len() as u64 * width * 24 + 128);
        let mut charge = match OwnedCharge::new(ctx, held) {
            Ok(charge) => charge,
            Err(e) if refused(&e) => {
                (self.at, self.block) = (at, block);
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        drop(scratch);
        if self.at == self.ranges.len() {
            child.done = true;
        }
        child.rows = child
            .rows
            .checked_add(read as usize)
            .ok_or_else(|| Error::invalid("cursor operator row count overflow"))?;
        child.info.actual_rows = child.rows.min(i64::MAX as usize) as i64;
        child.info.time_ms += began.elapsed().as_secs_f64() * 1000.0;
        // the rows the filter read in morsels, which the scan's own batches may continue
        child
            .info
            .counters
            .get_or_insert_with(Default::default)
            .insert("morselRows".into(), (child.rows as u64).into());
        ctx.check_rows(child.rows)?;
        ctx.produced(read as usize)?;
        self.rows = parts.iter().map(|p| p.first().map_or(0, Vec::len)).sum();
        self.parts = parts.into();
        self.offset = 0;
        if self.rows == 0 {
            charge.resize(0)?;
        }
        self.held = Some(charge);
        Ok(true)
    }

    /// The next batch of at most `cap` rows of the parts.
    fn emit(&mut self, ctx: &Arc<Ctx>, child: &Operator, cap: usize) -> Result<Buffer> {
        let n = cap.min(self.rows);
        let mut buffer = Buffer::new(ctx, &child.vars, n)?;
        let mut left = n;
        while left > 0 {
            let part = self.parts.front().expect("parts hold the rows left");
            let len = part.first().map_or(0, Vec::len);
            let take = left.min(len - self.offset);
            for (to, from) in buffer.table.cols.iter_mut().zip(part) {
                to.extend_from_slice(&from[self.offset..self.offset + take]);
            }
            self.offset += take;
            left -= take;
            if self.offset == len {
                self.parts.pop_front();
                self.offset = 0;
            }
        }
        buffer.table.len = n;
        self.rows -= n;
        if self.rows == 0
            && let Some(held) = &mut self.held
        {
            held.resize(0)?;
        }
        buffer.set_order(&child.order)?;
        Ok(buffer)
    }

    /// Hand the rest of the scan back to its own batches, from the next unread block.
    fn resume(&self, ctx: &Ctx, child: &mut Operator) {
        let State::Scan(scan) = &mut child.state else {
            unreachable!("morsels read a scan");
        };
        scan.at = self.at;
        scan.position = self.block.map(|b| {
            let first = ctx.snap.perm(self.spec.perm).blocks[b].first;
            self.ranges[self.at].0.max(first)
        });
        // The block-wise batches would restart the range, so the scan resumes from its
        // key position.
        scan.base_checked = true;
        scan.base = None;
    }
}
