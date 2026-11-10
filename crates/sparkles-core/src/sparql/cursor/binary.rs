//! The cursor's binary join operator: build one input, then resume compatible probe rows
//! without materializing the join's output or a Cartesian vector of matching row pairs.
//!
//! The hash kernel of [`crate::sparql::join::hash`] groups the build rows by key and
//! finds the pairs of each probe batch, up to the output's room, and the operator
//! gathers the output columns from the pairs. An OPTIONAL condition is evaluated once
//! over the gathered candidate rows of a chunk, as eager left joins do.
//!
//! A query that may spill partitions both inputs to disk once the build input outgrows
//! its share of memory, and joins the partitions one pair at a time ([`super::joinspill`],
//! X08 §6.3).
use super::joinspill::JoinSpill;
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::{Error, Result};
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::expr::Expr;
use crate::sparql::join::hash::{Join, Probe};
use crate::sparql::plan::{JoinAlgo, Kind, Node};
use crate::sparql::table::{Table, VarId};
use std::sync::Arc;

pub(super) use crate::sparql::join::hash::{
    EqualityIndex, HashIndex, Mode, NO_ROW, gather, map_bytes,
};

pub(super) fn eligible(node: &Node) -> bool {
    node.children.len() == 2
        && matches!(
            node.kind,
            Kind::Join { .. }
                | Kind::HalfJoin { .. }
                | Kind::Minus
                | Kind::LeftJoin { .. }
                | Kind::KeyedLeftJoin { .. }
                | Kind::KeyedJoin { .. }
        )
}

/// The input that is streamed. It matches the planner's hash probe order: the larger
/// input of an inner hash join, and child zero of every other join.
pub(super) fn probe_side(node: &Node) -> usize {
    usize::from(
        matches!(
            node.kind,
            Kind::Join {
                algo: JoinAlgo::Hash,
                ..
            }
        ) && node.children[0].est < node.children[1].est,
    )
}

pub(super) struct Binary {
    pub(super) mode: Mode,
    pub(super) probe_side: usize,
    pub(super) columns: Vec<(Option<usize>, Option<usize>)>,
    pub(super) shared: Vec<(usize, usize)>,
    /// Probe columns in order of preference for the hash key: certain on the probe
    /// side first. The build data decides which one is used.
    pub(super) preferred: Vec<(usize, usize)>,
    pub(super) index: Option<HashIndex>,
    /// A keyed OPTIONAL's equality ([`Kind::KeyedLeftJoin`]): its variable's column on
    /// the probe side and on the build side, and once the build input is read, its index.
    pub(super) equality: Option<(usize, usize)>,
    pub(super) equality_index: Option<EqualityIndex>,
    pub(super) build: Option<Buffer>,
    pub(super) probe: Option<Buffer>,
    /// The probe row being joined.
    pub(super) row: usize,
    /// How many of the row's candidates have been visited.
    pub(super) offset: usize,
    /// Whether the row has met a matching build row so far.
    pub(super) matched: bool,
    pub(super) expression: Option<Expr>,
    /// Whether a semi, anti or MINUS join lists the probe rows that a build row
    /// matches, whatever the mode keeps, because the build input is joined in several
    /// chunks and a probe row's verdict waits for the last one.
    pub(super) deferred: bool,
    /// Whether the output must keep the probe input's order if the join spills,
    /// because an operator above relies on it.
    pub(super) ordered: bool,
    /// The variables of the probe and build inputs.
    pub(super) probe_vars: Vec<VarId>,
    pub(super) build_vars: Vec<VarId>,
    /// The planner's estimate of the build input's rows.
    pub(super) build_est: f64,
    /// The partitioned join, once the build input has spilled.
    pub(super) spill: Option<Box<JoinSpill>>,
    _charge: Option<RetainedCharge>,
}

impl Binary {
    /// `ordered` tells that an operator above relies on the probe input's order, which
    /// a spilled join then restores.
    pub(super) fn new(ctx: &Ctx, node: &Node, ordered: bool) -> Result<Self> {
        let mode = match &node.kind {
            Kind::LeftJoin { .. } | Kind::KeyedLeftJoin { .. } => Mode::Optional,
            Kind::HalfJoin { anti: false } => Mode::Semi,
            Kind::HalfJoin { anti: true } => Mode::Anti,
            Kind::Minus => Mode::Minus,
            _ => Mode::Inner,
        };
        let probe_side = probe_side(node);
        let (probe, build) = (&node.children[probe_side], &node.children[1 - probe_side]);
        let charge = ctx.retained_charge(node.vars.len() as u64 * 96 + 1024)?;
        let columns = node
            .vars
            .iter()
            .map(|v| {
                (
                    probe.vars.iter().position(|x| x == v),
                    build.vars.iter().position(|x| x == v),
                )
            })
            .collect();
        let shared: Vec<_> = probe
            .vars
            .iter()
            .enumerate()
            .filter_map(|(p, v)| build.vars.iter().position(|x| x == v).map(|b| (p, b)))
            .collect();
        let mut preferred = shared.clone();
        preferred.sort_by_key(|&(p, _)| !probe.certain.contains(&probe.vars[p]));
        let expression = match &node.kind {
            Kind::LeftJoin { expr } => expr.clone(),
            Kind::KeyedLeftJoin { expr, .. } | Kind::KeyedJoin { expr, .. } => Some(expr.clone()),
            _ => None,
        };
        let equality = match &node.kind {
            Kind::KeyedLeftJoin { left, right, .. } | Kind::KeyedJoin { left, right, .. } => probe
                .vars
                .iter()
                .position(|v| v == left)
                .zip(build.vars.iter().position(|v| v == right)),
            _ => None,
        };
        Ok(Self {
            mode,
            probe_side,
            columns,
            shared,
            preferred,
            index: None,
            equality,
            equality_index: None,
            build: None,
            probe: None,
            row: 0,
            offset: 0,
            matched: false,
            expression,
            deferred: false,
            ordered,
            probe_vars: probe.vars.clone(),
            build_vars: build.vars.clone(),
            build_est: build.est,
            spill: None,
            _charge: charge,
        })
    }

    fn ensure_probe(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        cap: usize,
    ) -> Result<bool> {
        if self.probe.as_ref().is_some_and(|b| self.row < b.table.len) {
            return Ok(true);
        }
        self.probe = None;
        self.advance();
        self.row = 0;
        self.probe = children[self.probe_side].next(ctx, options, cap)?;
        Ok(self.probe.is_some())
    }

    fn build(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
    ) -> Result<()> {
        let child = &mut children[1 - self.probe_side];
        let mut build = Buffer::new(ctx, &child.vars, 0)?;
        // A query that may spill keeps the build input to half the room it had, so that
        // the probe batches and the output still fit when the join must partition.
        let threshold = ctx
            .spill
            .is_some()
            .then(|| (ctx.memory_room() / 2).max(super::joinspill::MIN_BUDGET));
        // The most the input's operators took above what the query held before, while
        // making one batch, as the ledger's peak shows when it rises.
        let mut demand = 0u64;
        // A synthetic threshold must not reject builds that have no key to
        // partition. Inspect each appended batch only until a usable key appears.
        let mut partitionable = false;
        loop {
            let (live, peak) = (ctx.live_bytes(), ctx.mem_peak());
            let Some(batch) = child.next(ctx, options, options.batch_rows)? else {
                break;
            };
            let raised = ctx.mem_peak();
            if raised > peak {
                demand = demand.max(raised - live);
            }
            let n = batch.table.len;
            let held = batch.charge.bytes();
            // The next batch, and what the input's operators hold to make it.
            let next_batch = super::capacity_bytes(&batch.table)
                .saturating_mul(4)
                .max(demand.saturating_sub(held));
            let grown = threshold.filter(|&threshold| {
                partitionable
                    && build
                        .charge
                        .bytes()
                        .saturating_add(super::capacity_bytes(&batch.table))
                        > threshold
                    && build.table.len > 0
            });
            let appended = match grown {
                Some(threshold) => Err(ctx.memory_exceeded(threshold)),
                None => merge::append(ctx, &mut build, &batch.table, 0..n),
            };
            match appended {
                // The build also spills when what else the query holds leaves less than a
                // quarter of the room it had, or less than a few of the input's batches, so
                // that the input can still give its next batch.
                Ok(()) => {
                    if threshold.is_some() && !partitionable {
                        partitionable = self.preferred.iter().any(|&(_, b)| {
                            batch.table.cols[b].iter().any(|value| !value.is_undef())
                        });
                    }
                    if partitionable
                        && let Some(threshold) = threshold
                        && ctx.memory_room() < (threshold / 2).max(next_batch)
                    {
                        let error = ctx.memory_exceeded(threshold);
                        return self.start_spill(ctx, children, options, build, None, error);
                    }
                }
                Err(error) if spills(ctx, &error) => {
                    return self.start_spill(ctx, children, options, build, Some(batch), error);
                }
                Err(error) => return Err(error),
            }
        }
        if let Some((p, b)) = self.equality {
            self.equality_index = Some(EqualityIndex::new(ctx, &build.table, b, p)?);
            self.build = Some(build);
            return Ok(());
        }
        // Key on the shared column that the build data leaves unbound least often.
        // A column that is unbound in every build row selects nothing.
        let Some((p, b)) = self.key_column(ctx, &build.table)? else {
            self.build = Some(build);
            return Ok(());
        };
        if build.table.len > 1 {
            match HashIndex::new(ctx, &build.table, b, p) {
                Ok(index) => self.index = Some(index),
                Err(error) if spills(ctx, &error) => {
                    return self.start_spill(ctx, children, options, build, None, error);
                }
                Err(error) => return Err(error),
            }
        }
        self.build = Some(build);
        Ok(())
    }

    /// The shared column to key on: the one that `table`, a build table, leaves unbound
    /// least often, as (probe column, build column), unless every row leaves each
    /// shared column unbound.
    pub(super) fn key_column(&self, ctx: &Ctx, table: &Table) -> Result<Option<(usize, usize)>> {
        let unbound = |column: usize| table.cols[column].iter().filter(|x| x.is_undef()).count();
        let mut best: Option<((usize, usize), usize)> = None;
        for &(p, b) in &self.preferred {
            ctx.check()?;
            let n = unbound(b);
            if n < table.len && best.is_none_or(|(_, m)| n < m) {
                best = Some(((p, b), n));
            }
            if n == 0 {
                break;
            }
        }
        Ok(best.map(|(key, _)| key))
    }

    /// Partition the build input to disk: the rows read so far in `build`, the batch
    /// `pending` that did not fit, and the rest of the input. A join without a key to
    /// partition on fails with `error`, the budget error that started the spill.
    fn start_spill(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        build: Buffer,
        pending: Option<Buffer>,
        error: Error,
    ) -> Result<()> {
        self.index = None;
        // The rows read so far choose the key. A first batch that did not fit has no
        // rows before it, and build rows that leave every shared column unbound select
        // nothing, so the pending batch chooses the key then.
        let key = match self.key_column(ctx, &build.table)? {
            Some(key) => Some(key),
            None => match &pending {
                Some(pending) => self.key_column(ctx, &pending.table)?,
                None => None,
            },
        };
        let Some(key) = key else {
            return Err(error);
        };
        let child = &mut children[1 - self.probe_side];
        let spill = JoinSpill::start(ctx, self, key, build, pending, child, options, error)?;
        self.spill = Some(Box::new(spill));
        Ok(())
    }

    fn advance(&mut self) {
        self.row += 1;
        self.offset = 0;
        self.matched = false;
    }

    /// The kernel's view of the current probe batch against the build table.
    fn join(&self) -> Join<'_> {
        Join {
            mode: self.mode,
            probe: &self.probe.as_ref().expect("available probe").table,
            build: &self.build.as_ref().expect("captured build").table,
            shared: &self.shared,
            index: self.index.as_ref(),
            equality: self
                .equality
                .map(|(p, _)| p)
                .zip(self.equality_index.as_ref()),
        }
    }

    /// Where the probe stands in the current batch, for the kernel to resume from.
    fn at(&self) -> Probe {
        Probe {
            row: self.row,
            end: self.probe.as_ref().expect("available probe").table.len,
            offset: self.offset,
            matched: self.matched,
        }
    }

    fn resume(&mut self, at: Probe) {
        self.row = at.row;
        self.offset = at.offset;
        self.matched = at.matched;
    }

    /// Inner and OPTIONAL joins without a condition: the pairs of the current probe
    /// batch from the current row, at most `room` of them. An unmatched OPTIONAL row
    /// pairs with [`NO_ROW`].
    pub(super) fn pairs(
        &mut self,
        ctx: &Ctx,
        room: usize,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<()> {
        let mut at = self.at();
        self.join().pairs(ctx, &mut at, room, lp, rp)?;
        self.resume(at);
        Ok(())
    }

    /// OPTIONAL with a condition: collect the compatible pairs of a chunk of probe rows,
    /// test the condition on their merged rows at once, then keep the pairs that pass and
    /// extend each finished row that none of its pairs passed.
    pub(super) fn condition(
        &mut self,
        ctx: &Ctx,
        vars: &[VarId],
        room: usize,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<()> {
        let mut at = self.at();
        let expression = self.expression.as_ref().expect("a join condition");
        self.join()
            .condition(ctx, &mut at, &self.columns, vars, expression, room, lp, rp)?;
        self.resume(at);
        Ok(())
    }

    /// Semi, anti and MINUS joins: the probe rows that pass, at most `room` of them. A
    /// row passes a semi join when a build row matches it, and the others when none does.
    /// A deferred join lists the rows that a build row matches.
    pub(super) fn filter(&mut self, ctx: &Ctx, room: usize, lp: &mut Vec<u32>) -> Result<()> {
        let mut at = self.at();
        self.join().filter(ctx, &mut at, room, self.deferred, lp)?;
        self.resume(at);
        Ok(())
    }

    /// Release exhausted input and join state while keeping the operator's metadata.
    /// Parents that spill can use this room for their remaining passes.
    pub(super) fn release_working_sets(&mut self) {
        self.index = None;
        self.equality_index = None;
        self.build = None;
        self.probe = None;
        self.spill = None;
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if let Some(mut spill) = self.spill.take() {
            let batch = spill.next(self, ctx, children, options, vars, cap);
            self.spill = Some(spill);
            return batch;
        }
        if !self.ensure_probe(ctx, children, options, cap)? {
            return Ok(None);
        }
        if self.build.is_none() && self.spill.is_none() {
            self.build(ctx, children, options)?;
            if self.spill.is_some() {
                return self.next(ctx, children, options, vars, cap);
            }
        }
        let mut output = Buffer::new(ctx, vars, cap)?;
        let _pairs = ctx.charge(cap as u64 * 8 + ctx.nvars() as u64 * 16 + 256)?;
        let mut lp: Vec<u32> = Vec::with_capacity(cap);
        let mut rp: Vec<u32> = Vec::with_capacity(cap);
        while output.table.len < cap {
            ctx.check()?;
            if !self.ensure_probe(ctx, children, options, cap)? {
                break;
            }
            let room = cap - output.table.len;
            lp.clear();
            rp.clear();
            match self.mode {
                Mode::Semi | Mode::Anti | Mode::Minus => self.filter(ctx, room, &mut lp)?,
                Mode::Optional | Mode::Inner if self.expression.is_some() => {
                    self.condition(ctx, vars, room, &mut lp, &mut rp)?
                }
                Mode::Inner | Mode::Optional => self.pairs(ctx, room, &mut lp, &mut rp)?,
            }
            let probe = &self.probe.as_ref().expect("available probe").table;
            let build = &self.build.as_ref().expect("captured build").table;
            if rp.is_empty() && !lp.is_empty() {
                // Rows of the probe input alone.
                rp.resize(lp.len(), NO_ROW);
            }
            gather(&mut output.table, &self.columns, probe, build, &lp, &rp);
        }
        output.reconcile()?;
        Ok((!output.table.is_empty()).then_some(output))
    }
}

/// Whether `error` is a memory budget that a query with spill answers by spilling.
pub(super) fn spills(ctx: &Ctx, error: &Error) -> bool {
    ctx.spill.is_some()
        && matches!(
            error,
            Error::BudgetExceeded(b)
                if matches!(b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory)
        )
}
