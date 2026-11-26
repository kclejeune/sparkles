//! The grouping operator: it assigns each input row the slot of its group through a hash
//! table on the keys, in order of first appearance, and hands the rows to one
//! accumulator per aggregate a batch at a time.
//!
//! The operator has two strategies. When every aggregate keeps running state, rows may
//! arrive in any number of batches, and a grouping that may spill does so in passes
//! (X08 §6.5). Once the groups of a pass hold half the memory the query had when the
//! grouping started, or a new group does not fit, the pass takes no new groups. Rows of
//! groups it already holds are still aggregated, and rows of other groups are written in
//! input order to one of [`PARTITIONS`] spill files chosen by a hash of their keys.
//! After the pass's groups are output and freed, each file is read by a pass of its own,
//! which partitions what it passes on with another hash. Every row of a group lands in
//! the same pass, in input order, so each aggregate sees exactly the rows it would see
//! in memory, and each pass takes at least one group. If a subtotal runs out of
//! space, that accumulator keeps its prefix and spills its remaining evaluated
//! arguments. Each prefix is resumed in arrival order before its result is emitted.
//!
//! When an aggregate needs its group's rows, such as FOLD, a registered aggregate or a
//! custom aggregate other than ARQ's statistics, the operator is built over whole groups
//! and given one batch that holds every row of each of its groups. The eager executor
//! gives it its whole input, and the cursor the finished groups of an input ordered on
//! the keys. The operator then indexes the rows of each slot once for the aggregates
//! that read them.

use super::acc::{Accumulator, Arg, Batch, Concat, Distinct, RowIndex, Rows, Running};
use super::deferred::{Deferred, subtotal};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::bignum::AGG_SUM_PAYLOAD_BYTES;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::exec::{compute_column, stat_aggregate};
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report as ExprReport;
use crate::sparql::plan::Agg;
use crate::sparql::table::{Table, VarId};
use crate::spill::{QuerySpill, SpillKind, SpillReader, SpillRun, SpillWriter};
use rustc_hash::FxHashMap;
use spargebra::algebra::AggregateFunction;
use std::sync::Arc;

/// Whether an aggregate keeps running state: COUNT(*), COUNT(DISTINCT *), or COUNT,
/// SUM, AVG, MIN, MAX, SAMPLE, GROUP_CONCAT or an ARQ statistics aggregate of an
/// expression, with or without DISTINCT. FOLD orders its group's rows and registered
/// aggregates run one group's accumulator at a time, so both need their group's rows,
/// and so do the other custom aggregates. With the `incremental_group` optimization off,
/// every aggregate reads its group's rows, which keeps the per-group kernel as an
/// independent check of the running states.
pub(crate) fn running(ctx: &Ctx, agg: &Agg) -> bool {
    ctx.opt.incremental_group && admitted(agg)
}

fn admitted(agg: &Agg) -> bool {
    agg.registered.is_none()
        && agg.fold.is_none()
        && match &agg.expr {
            None => matches!(agg.func, AggregateFunction::Count),
            Some(_) => {
                matches!(
                    agg.func,
                    AggregateFunction::Count
                        | AggregateFunction::Sum
                        | AggregateFunction::Avg
                        | AggregateFunction::Min
                        | AggregateFunction::Max
                        | AggregateFunction::Sample
                        | AggregateFunction::GroupConcat { .. }
                ) || stat_aggregate(&agg.func).is_some()
            }
        }
}

/// Whether every aggregate keeps running state, so that the grouping takes its rows in
/// any batches.
pub(crate) fn hashable(ctx: &Ctx, aggs: &[(VarId, Agg)]) -> bool {
    aggs.iter().all(|(_, agg)| running(ctx, agg))
}

/// Whether an aggregate's result is the same whatever the order of its group's rows, as
/// SPARQL defines it: COUNT, SUM, AVG, MIN and MAX, with or without DISTINCT, when they
/// keep running state. GROUP_CONCAT joins its values in row order and SAMPLE keeps the
/// first value, so they depend on it, and the ARQ statistics aggregates are left with
/// them. A sum of doubles or floats in another order may round differently in its last
/// bits, and MIN or MAX keeps the first of values that compare equal, which SPARQL leaves
/// open as well.
pub(crate) fn order_free(ctx: &Ctx, agg: &Agg) -> bool {
    running(ctx, agg)
        && matches!(
            agg.func,
            AggregateFunction::Count
                | AggregateFunction::Sum
                | AggregateFunction::Avg
                | AggregateFunction::Min
                | AggregateFunction::Max
        )
}

/// Where an aggregate's argument comes from in an input batch.
enum Source {
    Star,
    Column(Option<usize>),
    Expr(Expr),
    /// An argument that the aggregate evaluates itself from its group's rows
    Rows,
}

/// Groups found by their keys, in order of first appearance.
enum Index {
    One(FxHashMap<Id, u32>),
    Many(FxHashMap<Box<[Id]>, u32>),
}

impl Index {
    fn new(keys: usize) -> Self {
        if keys == 1 {
            Index::One(Default::default())
        } else {
            Index::Many(Default::default())
        }
    }
}

/// The slot of a row whose group this pass does not hold.
const PASSED: u32 = u32::MAX;
/// The spill files one pass divides the rows it passes on among.
pub(crate) const PARTITIONS: usize = 16;
/// The least state a pass holds before it takes no new groups.
const MIN_PASS_BYTES: u64 = 256 << 10;
/// The most rows an accumulator that keeps running state folds before its growth is
/// charged.
const CHUNK_ROWS: usize = 65_536;
/// The least step by which the retained charge of the group state grows.
const RETAIN_STEP: u64 = 64 << 10;

pub(crate) struct Grouping {
    keys: Vec<Option<usize>>,
    sources: Vec<Source>,
    aggs: Vec<Agg>,
    accs: Vec<Box<dyn Accumulator>>,
    /// Whether an accumulator reads its group's rows, so that each call of
    /// [`Grouping::add`] holds every row of its groups.
    whole: bool,
    index: Index,
    /// The keys of every group, `keys.len()` per group.
    order: Vec<Id>,
    groups: usize,
    /// The groups of the pass that were output.
    at: usize,
    /// The bytes each new group takes in the index, the keys and the accumulators.
    per_group: u64,
    memory: GroupMemory,
    /// Evaluated arguments of subtotals that have reached the pass's memory limit.
    deferred: Option<Box<Deferred>>,
    /// Whether the operator checks the output size of an eager grouping as it grows.
    eager: bool,
    /// Where rows of groups that do not fit go, when the query may spill.
    query: Option<Arc<QuerySpill>>,
    /// The state's bytes at which a pass takes no new groups, half the room the query
    /// had when the grouping or, for a later pass, the pass started.
    threshold: u64,
    /// Whether this pass takes no new groups.
    frozen: bool,
    /// The rows this pass passes on, by partition.
    writers: Vec<Option<SpillWriter>>,
    /// The partitions later passes read, each with how many times its rows were
    /// partitioned, which seeds the hash of its own partitions.
    pending: Vec<(Arc<SpillRun>, u64)>,
    /// How many times the rows of this pass were partitioned.
    depth: u64,
    /// The bytes written to spill files, and the passes after the first.
    pub(crate) spilled: u64,
    pub(crate) passes: usize,
}

struct GroupMemory {
    charge: Option<RetainedCharge>,
    bytes: u64,
    reserved: u64,
    base: u64,
}

impl GroupMemory {
    /// Count `bytes` more of retained state. The charge grows ahead in steps of at
    /// least 64 KiB, still before the state does, so that adding many small groups does
    /// not update the memory ledger for each of them. It overstates the state by less
    /// than one step.
    fn retain(&mut self, bytes: u64) -> Result<()> {
        let retained = self.bytes.saturating_add(bytes);
        if retained > self.reserved {
            let reserved = retained.saturating_add((retained / 8).max(RETAIN_STEP));
            if let Some(charge) = &mut self.charge {
                if charge.resize(reserved).is_err() {
                    // Near the budget, reserve only what the state needs.
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

    fn reset(&mut self) -> Result<()> {
        self.bytes = self.base;
        self.reserved = self.base;
        if let Some(charge) = &mut self.charge {
            charge.resize(self.base)?;
        }
        Ok(())
    }
}

/// Access to the pass's memory and deferred arguments during a batch fold.
pub(crate) enum BatchBudget<'a> {
    Retain(&'a mut dyn FnMut(u64) -> Result<()>),
    SpillSum(SumBudget<'a>),
}

impl<'a> BatchBudget<'a> {
    pub(super) fn retain(&mut self, bytes: u64) -> Result<()> {
        match self {
            Self::Retain(f) => f(bytes),
            Self::SpillSum(b) => b.memory.retain(bytes),
        }
    }

    pub(super) fn sum_budget(&mut self) -> Option<&mut SumBudget<'a>> {
        match self {
            Self::Retain(_) => None,
            Self::SpillSum(b) => Some(b),
        }
    }
}

/// Access to the pass's memory and deferred arguments during a spilling sum fold.
pub(crate) struct SumBudget<'a> {
    memory: &'a mut GroupMemory,
    threshold: u64,
    deferred: &'a mut Option<Box<Deferred>>,
    aggregate: usize,
}

impl SumBudget<'_> {
    pub(super) fn replace(&mut self, old: u64, new: u64) -> Result<bool> {
        if let Some(growth) = new.checked_sub(old) {
            if self.memory.bytes.saturating_add(growth) > self.threshold {
                return Ok(false);
            }
            match self.memory.retain(growth) {
                Ok(()) => Ok(true),
                Err(error) if memory_refused(&error) => Ok(false),
                Err(error) => Err(error),
            }
        } else {
            self.memory.bytes -= old - new;
            Ok(true)
        }
    }

    pub(super) fn defer(&mut self, ctx: &Ctx, slot: u32, id: Option<Id>) -> Result<()> {
        if self.deferred.is_none() {
            *self.deferred = Some(Box::new(Deferred::new(ctx)?));
        }
        self.deferred
            .as_mut()
            .expect("open deferred argument stream")
            .push(
                subtotal(slot as usize, self.aggregate),
                id.unwrap_or(Id::UNDEF),
            )
    }
}

fn memory_refused(error: &Error) -> bool {
    matches!(error, Error::BudgetExceeded(b) if matches!(
        b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
    ))
}

fn sum_or_avg(agg: &Agg) -> bool {
    matches!(agg.func, AggregateFunction::Sum | AggregateFunction::Avg)
}

fn accumulators(
    ctx: &Ctx,
    aggs: &[Agg],
    charged: bool,
    spilling: bool,
) -> Vec<Box<dyn Accumulator>> {
    aggs.iter()
        .map(|agg| -> Box<dyn Accumulator> {
            if !running(ctx, agg) {
                return Box::new(Rows::new(agg));
            }
            let inner: Box<dyn Accumulator> = match agg.func {
                AggregateFunction::GroupConcat { .. } => Box::new(Concat::new(agg)),
                _ => Box::new(Running::new(agg, charged && !(spilling && sum_or_avg(agg)))),
            };
            if agg.distinct {
                Box::new(Distinct::new(inner, agg.expr.is_none()))
            } else {
                inner
            }
        })
        .collect()
}

impl Grouping {
    /// A grouping on `keys` with `aggs` over rows of the variables `input`. `charged`
    /// counts its state against the query's memory, and `spill` lets a grouping whose
    /// aggregates keep running state pass rows to disk when its groups do not fit.
    pub(crate) fn new(
        ctx: &Ctx,
        keys: &[VarId],
        aggs: &[(VarId, Agg)],
        input: &[VarId],
        charged: bool,
        spill: bool,
    ) -> Result<Self> {
        let whole = !hashable(ctx, aggs);
        let query = if spill && !whole && !keys.is_empty() {
            ctx.spill.clone()
        } else {
            None
        };
        let agg_list: Vec<Agg> = aggs.iter().map(|(_, a)| a.clone()).collect();
        // One fold at a time can align both operands, produce a result and
        // normalize it. Replay can also hold a subtotal of its own in this space.
        let sum_scratch = if query.is_some() && agg_list.iter().any(sum_or_avg) {
            6 * AGG_SUM_PAYLOAD_BYTES
        } else {
            0
        };
        let base_bytes = if spill {
            aggs.len() as u64 * 256 + keys.len() as u64 * 64 + 1024
        } else {
            aggs.len() as u64 * 64 + keys.len() as u64 * 64 + 64
        };
        let bytes = base_bytes + sum_scratch;
        let charge = if charged {
            ctx.retained_charge(bytes)?
        } else {
            None
        };
        let accs = accumulators(ctx, &agg_list, charge.is_some(), query.is_some());
        let sources = agg_list
            .iter()
            .map(|agg| match &agg.expr {
                None => Source::Star,
                Some(_) if agg.fold.is_some() || agg.registered.is_some() => Source::Rows,
                Some(Expr::Var(v)) => Source::Column(input.iter().position(|x| x == v)),
                Some(expr) => Source::Expr(expr.clone()),
            })
            .collect();
        let nk = keys.len() as u64;
        // The index entry, twice while the index grows, a boxed key for several key
        // columns, the key in `order`, and each accumulator's state for the group.
        // Text and values that states own are charged as they grow.
        let entry = if nk == 1 { 13 } else { 8 * nk + 16 + 21 };
        let per_group = entry * 2 + nk * 8 + accs.iter().map(|a| a.group_bytes()).sum::<u64>() + 16;
        let mut grouping = Self {
            keys: keys
                .iter()
                .map(|k| input.iter().position(|v| v == k))
                .collect(),
            sources,
            aggs: agg_list,
            accs,
            whole,
            index: Index::new(keys.len()),
            order: Vec::new(),
            groups: 0,
            at: 0,
            per_group,
            memory: GroupMemory {
                charge,
                bytes,
                reserved: bytes,
                base: bytes,
            },
            deferred: None,
            eager: false,
            query,
            threshold: 0,
            frozen: false,
            writers: Vec::new(),
            pending: Vec::new(),
            depth: 0,
            spilled: 0,
            passes: 0,
        };
        if keys.is_empty() {
            grouping.insert(ctx, &[])?;
        }
        Ok(grouping)
    }

    /// Whether every aggregate counts rows, so that a count of the input answers them.
    pub(crate) fn only_counts_rows(&self) -> bool {
        self.keys.is_empty()
            && !self.whole
            && self
                .aggs
                .iter()
                .all(|agg| agg.expr.is_none() && !agg.distinct)
    }

    /// Count `rows` more rows in every aggregate of an ungrouped grouping of which
    /// [`Grouping::only_counts_rows`] holds.
    pub(crate) fn count_rows(&mut self, rows: u64) {
        for acc in &mut self.accs {
            acc.as_any()
                .downcast_mut::<Running>()
                .expect("a running count")
                .add_rows(rows);
        }
    }

    /// Add a group with these keys and return its number.
    fn insert(&mut self, ctx: &Ctx, key: &[Id]) -> Result<u32> {
        let at = self.groups as u32;
        ctx.check_rows(self.groups.saturating_add(1))?;
        self.memory.retain(self.per_group)?;
        match &mut self.index {
            Index::One(index) => {
                index.insert(key[0], at);
            }
            Index::Many(index) => {
                index.insert(key.into(), at);
            }
        }
        self.order.extend_from_slice(key);
        for acc in &mut self.accs {
            acc.init(1);
        }
        self.groups += 1;
        Ok(at)
    }

    /// Add a group with these keys, or pass its rows on when the pass is full.
    fn place(&mut self, ctx: &Ctx, key: &[Id]) -> Result<u32> {
        // A pass also stops taking groups when what else the query holds leaves less
        // than a quarter of the room it had, so that the batches in flight still fit.
        if self.query.is_some()
            && self.groups > 0
            && (self.memory.bytes >= self.threshold || ctx.memory_room() < self.threshold / 2)
        {
            self.frozen = true;
        }
        if self.frozen {
            return Ok(PASSED);
        }
        match self.insert(ctx, key) {
            Ok(group) => Ok(group),
            // A pass that holds a group passes on the groups that do not fit.
            Err(error) if self.query.is_some() && self.groups > 0 && memory_refused(&error) => {
                self.frozen = true;
                Ok(PASSED)
            }
            Err(error) => Err(error),
        }
    }

    /// Size a pass from the room the query has when it starts, once the input it read
    /// and the pass before it have given their memory back.
    pub(crate) fn start_pass(&mut self, ctx: &Ctx) {
        if self.query.is_some() {
            self.threshold = (ctx.memory_room() / 2).max(MIN_PASS_BYTES);
        }
    }

    /// The slot of every row of `table`, adding groups as they appear.
    fn slots(&mut self, ctx: &Ctx, table: &Table) -> Result<Vec<u32>> {
        let mut slots: Vec<u32> = Vec::with_capacity(table.len);
        let width = self.keys.len() + self.aggs.len();
        if self.keys.is_empty() {
            slots.resize(table.len, 0);
        } else if let [Some(column)] = self.keys.as_slice() {
            let column = &table.cols[*column];
            for (row, &id) in column.iter().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                    if self.eager && row.is_multiple_of(65_536) {
                        ctx.check_output(self.groups, width)?;
                    }
                }
                let found = match &self.index {
                    Index::One(index) => index.get(&id).copied(),
                    Index::Many(_) => unreachable!("a one-key index"),
                };
                slots.push(match found {
                    Some(group) => group,
                    None => self.place(ctx, &[id])?,
                });
            }
        } else {
            let mut key = vec![Id::UNDEF; self.keys.len()];
            for row in 0..table.len {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                    if self.eager && row.is_multiple_of(65_536) {
                        ctx.check_output(self.groups, width)?;
                    }
                }
                for (k, column) in key.iter_mut().zip(&self.keys) {
                    *k = column.map_or(Id::UNDEF, |c| table.cols[c][row]);
                }
                let found = match &self.index {
                    Index::One(index) => index.get(&key[0]).copied(),
                    Index::Many(index) => index.get(key.as_slice()).copied(),
                };
                slots.push(match found {
                    Some(group) => group,
                    None => self.place(ctx, &key)?,
                });
            }
        }
        Ok(slots)
    }

    /// Fold the rows of `table`, whose variables are the grouping's input, into their
    /// groups. A grouping over whole groups takes one call that holds every row of each
    /// of its groups.
    pub(crate) fn add(&mut self, ctx: &Ctx, table: &Table, report: &mut ExprReport) -> Result<()> {
        // The group of every row of the batch, then each aggregate over the whole
        // batch, so that the kind of an aggregate is matched once per batch.
        let _slots = ctx.charge(table.len as u64 * 4 + 64)?;
        let mut slots = self.slots(ctx, table)?;
        // Rows of groups this pass does not hold go to the next pass.
        let passed = if self.frozen {
            slots.iter().filter(|&&slot| slot == PASSED).count()
        } else {
            0
        };
        let kept;
        let table = if passed > 0 {
            self.pass_on(ctx, table, &slots, passed)?;
            let charge =
                ctx.charge((table.len - passed) as u64 * table.cols.len() as u64 * 8 + 256)?;
            let mut rows = Table::new(table.vars.clone());
            for (to, from) in rows.cols.iter_mut().zip(&table.cols) {
                to.extend(
                    from.iter()
                        .zip(&slots)
                        .filter(|&(_, &slot)| slot != PASSED)
                        .map(|(&id, _)| id),
                );
            }
            rows.len = table.len - passed;
            slots.retain(|&slot| slot != PASSED);
            kept = (rows, charge);
            &kept.0
        } else {
            table
        };
        // Expression arguments are evaluated once per batch into charged columns.
        let _scratch = ctx.charge(
            self.sources
                .iter()
                .filter(|source| matches!(source, Source::Expr(_)))
                .count() as u64
                * (table.len as u64 * 8 + 64),
        )?;
        let computed = self
            .sources
            .iter()
            .map(|source| match source {
                Source::Expr(expr) => compute_column(ctx, table, expr, report).map(Some),
                _ => Ok(None),
            })
            .collect::<Result<Vec<Option<Vec<Id>>>>>()?;
        // The rows of each group, for the aggregates that read them.
        let index = self
            .whole
            .then(|| -> Result<RowIndex> {
                let _index = ctx.charge(table.len as u64 * 4 + self.groups as u64 * 8 + 64)?;
                Ok(RowIndex::new(&slots, self.groups))
            })
            .transpose()?;
        for (a, values) in computed.iter().enumerate() {
            ctx.check()?;
            let arg = match (&self.sources[a], values) {
                (_, Some(values)) => Arg::Column(values),
                (Source::Star | Source::Rows, _) => Arg::Star,
                (Source::Column(Some(c)), _) => Arg::Column(&table.cols[*c]),
                (Source::Column(None), _) => Arg::Unbound,
                (Source::Expr(_), None) => unreachable!("a computed argument"),
            };
            // Rows of groups that keep running state go in chunks, so that what their
            // state grows by is charged as it grows.
            let chunk = if self.whole {
                slots.len().max(1)
            } else {
                CHUNK_ROWS
            };
            let mut base = 0;
            loop {
                let end = slots.len().min(base + chunk);
                let batch = Batch {
                    table,
                    arg: match arg {
                        Arg::Column(values) => Arg::Column(&values[base..end]),
                        other => other,
                    },
                    rows: index.as_ref(),
                    base,
                };
                if self.query.is_some() {
                    let mut budget = BatchBudget::SpillSum(SumBudget {
                        memory: &mut self.memory,
                        threshold: self.threshold,
                        deferred: &mut self.deferred,
                        aggregate: a,
                    });
                    self.accs[a].add_batch(ctx, &batch, &slots[base..end], &mut budget)?;
                } else {
                    let memory = &mut self.memory;
                    let mut retain = |delta: u64| memory.retain(delta);
                    let mut budget = BatchBudget::Retain(&mut retain);
                    self.accs[a].add_batch(ctx, &batch, &slots[base..end], &mut budget)?;
                }
                ctx.check()?;
                let _ = self.accs[a].take_growth();
                base = end;
                if base >= slots.len() {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Write the rows of `table` whose slot is [`PASSED`] to the partitions of later
    /// passes.
    fn pass_on(&mut self, ctx: &Ctx, table: &Table, slots: &[u32], rows: usize) -> Result<()> {
        let query = self.query.clone().expect("a spilling grouping");
        let _gather = ctx.charge(rows as u64 * (table.cols.len() as u64 * 8 + 8) + 256)?;
        let mut parts: Vec<Vec<usize>> = vec![Vec::new(); PARTITIONS];
        for (row, _) in slots
            .iter()
            .enumerate()
            .filter(|&(_, &slot)| slot == PASSED)
        {
            let hash = crate::sparql::cursor::row_hash(
                self.depth,
                self.keys
                    .iter()
                    .map(|key| key.map_or(Id::UNDEF, |c| table.cols[c][row])),
            );
            parts[(hash >> 60) as usize % PARTITIONS].push(row);
        }
        if self.writers.is_empty() {
            self.writers.resize_with(PARTITIONS, || None);
        }
        for (part, rows) in parts.iter().enumerate() {
            if rows.is_empty() {
                continue;
            }
            let columns: Vec<Vec<Id>> = table
                .cols
                .iter()
                .map(|column| rows.iter().map(|&row| column[row]).collect())
                .collect();
            let writer = &mut self.writers[part];
            if writer.is_none() {
                *writer = Some(query.create(SpillKind::GroupPartition, table.cols.len())?);
            }
            let slices: Vec<&[Id]> = columns.iter().map(Vec::as_slice).collect();
            writer
                .as_mut()
                .expect("an open spill file")
                .write(&slices, rows.len())?;
        }
        Ok(())
    }

    /// Close the partitions this pass passed rows to, once its input has ended.
    pub(crate) fn end_input(&mut self) -> Result<()> {
        for writer in std::mem::take(&mut self.writers).into_iter().flatten() {
            let run = writer.finish()?;
            self.spilled = self.spilled.saturating_add(run.bytes());
            self.pending.push((Arc::new(run), self.depth + 1));
        }
        if let Some(deferred) = &mut self.deferred {
            deferred.seal()?;
            self.spilled += deferred.take_written();
        }
        Ok(())
    }

    /// Up to `cap` groups of the pass not yet output, as rows of the keys and then the
    /// aggregates' values, or `None` once the pass is output.
    pub(crate) fn output(
        &mut self,
        ctx: &Ctx,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Table>> {
        let n = cap.min(self.groups - self.at);
        if n == 0 {
            return Ok(None);
        }
        let nk = self.keys.len();
        let mut out = Table::new(vars.to_vec());
        for col in &mut out.cols {
            col.reserve_exact(n);
        }
        for k in 0..nk {
            out.cols[k].extend((self.at..self.at + n).map(|g| self.order[g * nk + k]));
        }
        if let Some(deferred) = &mut self.deferred {
            // Replay the shared spill stream in its slot and aggregate order.
            for g in self.at..self.at + n {
                ctx.check()?;
                for (a, acc) in self.accs.iter_mut().enumerate() {
                    let id = acc.finish_replay(ctx, g, a, deferred)?;
                    ctx.check()?;
                    out.cols[nk + a].push(id);
                }
            }
            self.spilled += deferred.take_written();
            if deferred.is_empty() {
                self.deferred = None;
            }
        } else {
            for (a, acc) in self.accs.iter_mut().enumerate() {
                ctx.check()?;
                for g in self.at..self.at + n {
                    let id = acc.finish(ctx, g)?;
                    ctx.check()?;
                    out.cols[nk + a].push(id);
                }
            }
        }
        out.len = n;
        self.at += n;
        Ok(Some(out))
    }

    /// Free the output pass and start the next one over the rows it passed on, which
    /// the returned reader gives. `None` when no rows were passed on.
    pub(crate) fn next_pass(&mut self, ctx: &Ctx) -> Result<Option<SpillReader>> {
        let Some((run, depth)) = self.pending.pop() else {
            return Ok(None);
        };
        self.index = Index::new(self.keys.len());
        self.order = Vec::new();
        self.accs = accumulators(ctx, &self.aggs, self.memory.charge.is_some(), true);
        self.groups = 0;
        self.at = 0;
        self.frozen = false;
        self.memory.reset()?;
        self.depth = depth;
        self.passes += 1;
        Ok(Some(run.reader()?))
    }
}

/// GROUP BY over a table that holds the whole input, for the eager executor and for
/// the finished groups of an input ordered on the keys. Returns the keys and then the
/// aggregates' values, one row per group in order of first appearance, and one row for
/// an ungrouped aggregate over no rows. `eager` checks the output's size against the
/// query's limits as the groups grow, as an eager table is checked.
pub(crate) fn whole(
    ctx: &Ctx,
    table: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
    charged: bool,
    eager: bool,
    report: &mut ExprReport,
) -> Result<Table> {
    let mut vars = keys.to_vec();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let mut grouping = Grouping::new(ctx, keys, aggs, &table.vars, charged, false)?;
    grouping.eager = eager;
    if grouping.only_counts_rows() {
        grouping.count_rows(table.len as u64);
    } else {
        grouping.add(ctx, table, report)?;
    }
    Ok(grouping
        .output(ctx, &vars, usize::MAX)?
        .unwrap_or_else(|| Table::new(vars)))
}
