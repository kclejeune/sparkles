//! The accumulator trait of the grouping operator and the accumulators of the aggregates
//! that keep running state.
//!
//! An accumulator holds the state of one aggregate for every group of a pass, in one
//! column of states, and folds a whole batch of rows into it per call. The operator
//! assigns each row a slot, the number of its group, so a call costs one dynamic
//! dispatch per aggregate per batch and none per row. The trait has the five methods of
//! the design. `init` makes room for new groups, `add_batch` folds rows into their
//! groups, `merge` folds the groups of another accumulator that saw later rows into
//! this one, `finish` gives a group's value, and `retract` takes rows back for a state
//! that can, which item 418's view operator needs.

use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::exec::AggState;
use crate::sparql::expr::Expr;
use crate::sparql::plan::Agg;
use crate::sparql::table::Table;
use crate::sparql::value::Value;
use rustc_hash::FxHashSet;
use spargebra::algebra::AggregateFunction;
use std::any::Any;

/// Where an aggregate's argument comes from in a batch.
#[derive(Clone, Copy)]
pub(crate) enum Arg<'a> {
    /// `COUNT(*)`, which counts rows
    Star,
    /// A variable that the input does not bind, unbound in every row
    Unbound,
    /// The argument's id in every row of the batch, unbound for an error
    Column(&'a [Id]),
}

/// A batch of rows for an accumulator to fold, and the whole table it comes from for the
/// aggregates that read other columns of a row.
pub(crate) struct Batch<'a> {
    pub table: &'a Table,
    pub arg: Arg<'a>,
    /// The rows of each slot, when the batch holds every row of each of its groups
    pub rows: Option<&'a RowIndex>,
    /// The row of `table` that the batch's first row is
    pub base: usize,
}

/// The rows of each slot of a batch, in row order: slot `g`'s rows are
/// `rows[start[g]..start[g + 1]]`.
pub(crate) struct RowIndex {
    start: Vec<u32>,
    rows: Vec<u32>,
}

impl RowIndex {
    /// Index `slots` by a counting sort, for `groups` slots.
    pub(crate) fn new(slots: &[u32], groups: usize) -> Self {
        let mut start = vec![0u32; groups + 1];
        for &g in slots {
            start[g as usize + 1] += 1;
        }
        for g in 0..groups {
            start[g + 1] += start[g];
        }
        let mut rows = vec![0u32; slots.len()];
        let mut next = start.clone();
        for (i, &g) in slots.iter().enumerate() {
            rows[next[g as usize] as usize] = i as u32;
            next[g as usize] += 1;
        }
        Self { start, rows }
    }

    pub(crate) fn groups(&self) -> usize {
        self.start.len() - 1
    }

    pub(crate) fn of(&self, group: usize) -> &[u32] {
        &self.rows[self.start[group] as usize..self.start[group + 1] as usize]
    }
}

impl Batch<'_> {
    /// The argument of row `row`, `None` for `COUNT(*)`.
    #[inline]
    fn value(&self, row: usize) -> Option<Id> {
        match self.arg {
            Arg::Star => None,
            Arg::Unbound => Some(Id::UNDEF),
            Arg::Column(values) => Some(values[row]),
        }
    }
}

/// How one aggregate folds the rows of its groups. `merge` and `retract` have no caller
/// in the operator yet: a parallel split of a grouping merges, and item 418's view
/// operator retracts.
#[allow(dead_code, reason = "merge and retract serve later callers")]
pub(crate) trait Accumulator: Send {
    /// Add `groups` groups with empty state, after those held.
    fn init(&mut self, groups: usize);

    /// Fold row `i` of the batch into group `slots[i]`, for every row, in row order. A
    /// row's slot is the number of its group among those `init` made.
    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()>;

    /// Whether [`Accumulator::merge`] gives what one accumulator over all the rows gives.
    fn mergeable(&self) -> bool {
        false
    }

    /// Fold group `j` of `other`, which saw rows that come after the rows of this
    /// accumulator, into group `into[j]` of this one, for every group of `other`.
    /// `other` is of the same kind and aggregate. An accumulator that is not
    /// [`mergeable`](Accumulator::mergeable) leaves its groups as they are and fails.
    fn merge(&mut self, _ctx: &Ctx, _other: &mut dyn Accumulator, _into: &[u32]) -> Result<()> {
        Err(crate::error::Error::unsupported(
            "this aggregate cannot be merged",
        ))
    }

    /// The value of group `group`, which the call takes out of the accumulator.
    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id>;

    /// Whether [`Accumulator::retract`] can take back every row.
    fn retractable(&self) -> bool {
        false
    }

    /// Take back row `i` of the batch from group `slots[i]`, for every row, which were
    /// added before. `Ok(false)` when a row cannot be taken back, such as one of a sum
    /// that left the exact integers, and the groups of the batch are then in no state
    /// to use, so the caller recomputes them from their rows.
    fn retract(&mut self, _ctx: &Ctx, _batch: &Batch<'_>, _slots: &[u32]) -> Result<bool> {
        Ok(false)
    }

    /// The bytes of state each new group takes, which the operator charges.
    fn group_bytes(&self) -> u64;

    /// The bytes the state has grown by, apart from the groups' own, since the last
    /// call, which the operator charges.
    fn take_growth(&mut self) -> u64 {
        0
    }

    fn as_any(&mut self) -> &mut dyn Any;
}

/// The aggregates whose state is [`AggState`]: COUNT, SUM, AVG, MIN, MAX, SAMPLE and
/// ARQ's variance and deviation aggregates.
pub(crate) struct Running {
    agg: Agg,
    /// Whether the decoded bytes of the best values are tracked, for a charged grouping
    track: bool,
    states: Vec<AggState>,
    /// The decoded bytes of the best value of each group of a MIN or MAX, which the
    /// state keeps alive.
    payload: Vec<u64>,
    growth: u64,
}

impl Running {
    pub(crate) fn new(agg: &Agg, track: bool) -> Self {
        Self {
            agg: agg.clone(),
            track,
            states: Vec::new(),
            payload: Vec::new(),
            growth: 0,
        }
    }

    /// Count `rows` more rows in a `COUNT(*)` of an ungrouped grouping.
    pub(crate) fn add_rows(&mut self, rows: u64) {
        for state in &mut self.states {
            if let AggState::Count(n) = state {
                *n += rows;
            }
        }
    }

    fn is_best(&self) -> bool {
        matches!(
            self.agg.func,
            AggregateFunction::Min | AggregateFunction::Max
        )
    }

    fn is_sum(&self) -> bool {
        matches!(
            self.agg.func,
            AggregateFunction::Sum | AggregateFunction::Avg
        )
    }
}

impl Accumulator for Running {
    fn init(&mut self, groups: usize) {
        for _ in 0..groups {
            self.states.push(AggState::new(&self.agg));
        }
        if self.is_best() || (self.track && self.is_sum()) {
            self.payload.resize(self.states.len(), 0);
        }
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        let agg = &self.agg;
        match (&agg.func, batch.arg) {
            // running counts need no per-row dispatch
            (AggregateFunction::Count, Arg::Star) => {
                for &slot in slots {
                    if let AggState::Count(n) = &mut self.states[slot as usize] {
                        *n += 1;
                    }
                }
            }
            (AggregateFunction::Count, Arg::Column(values)) => {
                for (&slot, id) in slots.iter().zip(values) {
                    if let AggState::Count(n) = &mut self.states[slot as usize] {
                        *n += u64::from(!id.is_undef());
                    }
                }
            }
            _ => {
                let best = self.track && self.is_best();
                let sum = self.track && self.is_sum();
                for (row, &slot) in slots.iter().enumerate() {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let id = batch.value(row);
                    if best && let Some(id) = id {
                        // the best value stays decoded while its group lives
                        let decoded = ctx.decoded_bytes(id)?;
                        let held = &mut self.payload[slot as usize];
                        if decoded > *held {
                            self.growth += decoded - *held;
                            *held = decoded;
                        }
                    }
                    self.states[slot as usize].add(ctx, agg, id);
                    if sum {
                        let held_now = self.states[slot as usize].sum_heap_bytes();
                        if held_now > 0 {
                            let held = &mut self.payload[slot as usize];
                            if held_now > *held {
                                self.growth += held_now - *held;
                                *held = held_now;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// A merged SUM or AVG can differ from one pass in three ways. A float or double
    /// subtotal can round differently. An integer subtotal beyond the decimal range
    /// leaves the result unbound where one pass would have promoted it to a double. A
    /// decimal sum that overflows partway through one pass, and so stays unbound, can
    /// fit when its parts are summed apart. No caller merges these yet. Roadmap item
    /// 1066, the parallel split that merges partitions, settles them before it does.
    fn mergeable(&self) -> bool {
        crate::sparql::exec::stat_aggregate(&self.agg.func).is_none()
    }

    fn merge(&mut self, ctx: &Ctx, other: &mut dyn Accumulator, into: &[u32]) -> Result<()> {
        let other = other
            .as_any()
            .downcast_mut::<Running>()
            .expect("an accumulator of the same kind");
        for (state, &to) in std::mem::take(&mut other.states).into_iter().zip(into) {
            if !self.states[to as usize].merge(ctx, &self.agg, state) {
                return Err(crate::error::Error::unsupported(
                    "this aggregate cannot be merged",
                ));
            }
            if self.track && self.is_sum() {
                let held_now = self.states[to as usize].sum_heap_bytes();
                let held = &mut self.payload[to as usize];
                if held_now > *held {
                    self.growth += held_now - *held;
                    *held = held_now;
                }
            }
        }
        if self.is_best() {
            for (from, &to) in std::mem::take(&mut other.payload).iter().zip(into) {
                let held = &mut self.payload[to as usize];
                if *from > *held {
                    self.growth += *from - *held;
                    *held = *from;
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let state = std::mem::replace(&mut self.states[group], AggState::Count(0));
        Ok(state.finish(ctx, &self.agg))
    }

    fn retractable(&self) -> bool {
        matches!(
            self.agg.func,
            AggregateFunction::Count | AggregateFunction::Sum | AggregateFunction::Avg
        ) && !self.agg.distinct
    }

    fn retract(&mut self, _ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<bool> {
        if !self.retractable() {
            return Ok(false);
        }
        for (row, &slot) in slots.iter().enumerate() {
            if !self.states[slot as usize].retract(batch.value(row)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn group_bytes(&self) -> u64 {
        size_of::<AggState>() as u64 * 2 + 8
    }

    fn take_growth(&mut self) -> u64 {
        std::mem::take(&mut self.growth)
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// GROUP_CONCAT: the text of each group so far, and whether every value had a lexical
/// form.
pub(crate) struct Concat {
    separator: String,
    /// GROUP_CONCAT(DISTINCT …), which some readings make unbound over no rows
    distinct: bool,
    groups: Vec<ConcatState>,
    growth: u64,
}

struct ConcatState {
    text: String,
    ok: bool,
    empty: bool,
}

impl Concat {
    pub(crate) fn new(agg: &Agg) -> Self {
        let separator = match &agg.func {
            AggregateFunction::GroupConcat { separator } => {
                separator.as_deref().unwrap_or(" ").to_string()
            }
            _ => unreachable!("a GROUP_CONCAT aggregate"),
        };
        Self {
            separator,
            distinct: agg.distinct,
            groups: Vec::new(),
            growth: 0,
        }
    }
}

impl Accumulator for Concat {
    fn init(&mut self, groups: usize) {
        for _ in 0..groups {
            self.groups.push(ConcatState {
                text: String::new(),
                ok: true,
                empty: true,
            });
        }
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let state = &mut self.groups[slot as usize];
            if !state.ok {
                continue;
            }
            let id = batch.value(row).unwrap_or(Id::UNDEF);
            if id.is_undef() {
                state.ok = false;
                continue;
            }
            let Some(value) = ctx.value(id) else {
                continue;
            };
            // CONCAT takes only strings
            if ctx.aggregate_errors.concat_strings_only() && !value.is_string() {
                state.ok = false;
                continue;
            }
            let Ok(lexical) = value.lexical() else {
                state.ok = false;
                continue;
            };
            let before = state.text.capacity();
            if !state.empty {
                state.text.push_str(&self.separator);
            }
            state.text.push_str(&lexical);
            state.empty = false;
            // the string and its reallocation coexist while it grows
            self.growth += (state.text.capacity().saturating_sub(before) as u64).saturating_mul(2);
        }
        Ok(())
    }

    fn mergeable(&self) -> bool {
        true
    }

    fn merge(&mut self, _ctx: &Ctx, other: &mut dyn Accumulator, into: &[u32]) -> Result<()> {
        let other = other
            .as_any()
            .downcast_mut::<Concat>()
            .expect("an accumulator of the same kind");
        for (from, &to) in std::mem::take(&mut other.groups).into_iter().zip(into) {
            let state = &mut self.groups[to as usize];
            state.ok &= from.ok;
            if !state.ok || from.empty {
                continue;
            }
            let before = state.text.capacity();
            if !state.empty {
                state.text.push_str(&self.separator);
            }
            state.text.push_str(&from.text);
            state.empty = false;
            self.growth += (state.text.capacity().saturating_sub(before) as u64).saturating_mul(2);
        }
        Ok(())
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let state = std::mem::replace(
            &mut self.groups[group],
            ConcatState {
                text: String::new(),
                ok: false,
                empty: true,
            },
        );
        let nothing = state.empty
            && self.distinct
            && ctx.aggregate_errors.distinct_concat_of_nothing_unbound();
        Ok(if state.ok && !nothing {
            // SPARQL 1.1: the result is a simple literal
            ctx.intern_value(&Value::Str(state.text.into()))
        } else {
            Id::UNDEF
        })
    }

    fn group_bytes(&self) -> u64 {
        size_of::<ConcatState>() as u64 * 2 + 8
    }

    fn take_growth(&mut self) -> u64 {
        std::mem::take(&mut self.growth)
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// An aggregate with DISTINCT: each group passes a value to the aggregate underneath
/// once, and an error is never a duplicate of another value. COUNT(DISTINCT *) tells
/// rows apart by all of their input values.
pub(crate) struct Distinct {
    inner: Box<dyn Accumulator>,
    values: FxHashSet<(u32, Id)>,
    rows: FxHashSet<(u32, Box<[Id]>)>,
    growth: u64,
    star: bool,
}

impl Distinct {
    pub(crate) fn new(inner: Box<dyn Accumulator>, star: bool) -> Self {
        Self {
            inner,
            values: FxHashSet::default(),
            rows: FxHashSet::default(),
            growth: 0,
            star,
        }
    }
}

impl Accumulator for Distinct {
    fn init(&mut self, groups: usize) {
        self.inner.init(groups);
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        // The slots and values of the rows that are new, which the aggregate underneath
        // reads, are charged until it has read them.
        let _scratch = ctx.charge(slots.len() as u64 * 12 + 64)?;
        let mut keep: Vec<u32> = Vec::with_capacity(slots.len());
        let mut kept: Vec<Id> = Vec::with_capacity(slots.len());
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let id = batch.value(row);
            let fresh = if self.star {
                let key: Box<[Id]> = batch
                    .table
                    .cols
                    .iter()
                    .map(|c| c[batch.base + row])
                    .collect();
                // the boxed row, the set's entry and room for its table to grow
                let bytes = batch.table.cols.len() as u64 * 8 + 96;
                let fresh = self.rows.insert((slot, key));
                if fresh {
                    self.growth += bytes;
                }
                fresh
            } else {
                match id {
                    // an error is never a duplicate
                    Some(id) if !id.is_undef() => {
                        let fresh = self.values.insert((slot, id));
                        if fresh {
                            // the set's entry, with room for its table to grow
                            self.growth += 64;
                        }
                        fresh
                    }
                    _ => true,
                }
            };
            if fresh {
                keep.push(slot);
                kept.push(id.unwrap_or(Id::UNDEF));
            }
        }
        let arg = match batch.arg {
            Arg::Column(_) => Arg::Column(&kept),
            other => other,
        };
        // COUNT(DISTINCT *) counts the rows that were new
        let inner = Batch {
            table: batch.table,
            arg,
            rows: None,
            base: batch.base,
        };
        self.inner.add_batch(ctx, &inner, &keep)
    }

    fn group_bytes(&self) -> u64 {
        self.inner.group_bytes()
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        self.inner.finish(ctx, group)
    }

    fn take_growth(&mut self) -> u64 {
        std::mem::take(&mut self.growth) + self.inner.take_growth()
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// An aggregate that reads its group's rows: FOLD, a registered aggregate, a custom
/// aggregate other than ARQ's statistics, and every aggregate when the
/// `incremental_group` optimization is off. Its batch holds every row of each of its
/// groups, and the aggregate's kernel runs once per group over them.
pub(crate) struct Rows {
    agg: Agg,
    results: Vec<Id>,
}

impl Rows {
    pub(crate) fn new(agg: &Agg) -> Self {
        Self {
            agg: agg.clone(),
            results: Vec::new(),
        }
    }
}

impl Accumulator for Rows {
    fn init(&mut self, groups: usize) {
        self.results.extend(std::iter::repeat_n(Id::UNDEF, groups));
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, _slots: &[u32]) -> Result<()> {
        let index = batch.rows.expect("a batch of whole groups");
        let map = batch.table.var_map(ctx.nvars());
        let arg = match batch.arg {
            Arg::Column(values) => Some(values),
            Arg::Star | Arg::Unbound => None,
        };
        // EXISTS patterns that the aggregate evaluates itself, such as in a FOLD's value
        // or ORDER BY, answered from their key sets for the batch's rows
        let evaluated: Vec<&Expr> = self
            .agg
            .expr
            .iter()
            .filter(|_| arg.is_none())
            .chain(self.agg.fold.iter().flat_map(|fold| {
                fold.value
                    .iter()
                    .chain(fold.order.iter().map(|(key, _)| key))
            }))
            .collect();
        let nested = crate::sparql::exists::nested(ctx, batch.table, &evaluated)?;
        let exists = nested.as_ref().map(|n| &n.answers);
        for g in 0..index.groups() {
            ctx.check()?;
            let rows = index.of(g);
            self.results[g] = if let Some(descriptor) = &self.agg.registered {
                crate::sparql::registeredagg::aggregate(
                    ctx,
                    batch.table,
                    &map,
                    rows,
                    &self.agg,
                    descriptor,
                )?
            } else {
                crate::sparql::exec::aggregate(ctx, batch.table, &map, rows, &self.agg, arg, exists)
            };
            if ctx.calls_extensions {
                ctx.check()?;
            }
        }
        Ok(())
    }

    fn finish(&mut self, _ctx: &Ctx, group: usize) -> Result<Id> {
        Ok(self.results[group])
    }

    fn group_bytes(&self) -> u64 {
        16
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
