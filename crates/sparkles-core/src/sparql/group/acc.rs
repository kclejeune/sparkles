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
//! that can, which the incremental view operator in `incr/ops/group.rs` calls with signed
//! weights.

use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::incr::persist::{Reader, Writer};
use crate::sparql::ctx::Ctx;
use crate::sparql::exec::AggState;
use crate::sparql::expr::Expr;
use crate::sparql::plan::Agg;
use crate::sparql::table::Table;
use crate::sparql::value::{NumOp, Value, arith, exact_sum, order_cmp};
use oxsdatatypes::{Decimal, Double, Float};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::AggregateFunction;
use std::any::Any;
use std::cmp::Ordering;

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
    /// Signed multiplicity of each row (`None`: every row has unit weight). `add_batch`
    /// folds positive weights and `retract` takes back rows using their weight magnitude.
    pub weights: Option<&'a [i64]>,
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

    /// Positive multiplicity for [`Accumulator::add_batch`].
    #[inline]
    fn add_weight(&self, row: usize) -> u64 {
        self.weights
            .map_or(1, |w| u64::try_from(w[row]).unwrap_or(0))
    }

    /// Magnitude to take back for [`Accumulator::retract`] (accepts signed negative
    /// delta weights `-k` as well as positive retraction counts `+k`).
    #[inline]
    fn retract_weight(&self, row: usize) -> u64 {
        self.weights.map_or(1, |w| w[row].unsigned_abs())
    }

    /// Whether every row's retraction weight fits in `0..=i64::MAX` (rejects `i64::MIN`,
    /// whose magnitude `2^63` overflows signed `i64` arithmetic).
    #[inline]
    fn retract_weights_fit_i64(&self, rows: usize) -> bool {
        self.weights
            .is_none_or(|w| w[..rows].iter().all(|&v| v != i64::MIN))
    }
}

const SHRINK_MIN_CAP: usize = 64;

fn shrink_vec<T>(v: &mut Vec<T>) {
    if v.is_empty() {
        *v = Vec::new();
    } else if v.capacity() >= SHRINK_MIN_CAP && v.len() * 4 <= v.capacity() {
        v.shrink_to(v.len() * 2);
    }
}

fn shrink_map<K: Eq + std::hash::Hash, V>(m: &mut FxHashMap<K, V>) {
    if m.is_empty() {
        *m = FxHashMap::default();
    } else if m.capacity() >= SHRINK_MIN_CAP && m.len() * 4 <= m.capacity() {
        m.shrink_to(m.len() * 2);
    }
}

/// Shared finalization for `SUM` and `AVG` across both one-off query accumulators and
/// retractable view accumulators, keeping `AggregateErrors` readings identical.
pub(crate) fn finish_sum_avg(
    ctx: &Ctx,
    is_avg: bool,
    rows: u64,
    undef: u64,
    sum: Option<Value>,
) -> Id {
    // AVG is Sum over Count, and Count leaves the errors out
    if rows > 0 && undef == rows && is_avg && ctx.aggregate_errors.avg_of_errors_zero() {
        return ctx.intern_value(&Value::Integer(0.into()));
    }
    let Some(sum) = sum else {
        return Id::UNDEF;
    };
    let v = if is_avg {
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

/// Shared finalization for `MIN` and `MAX` across one-off and retractable accumulators.
pub(crate) fn finish_best(ctx: &Ctx, is_min: bool, errs: u64, best: Option<Id>) -> Id {
    if errs > 0 && ctx.aggregate_errors.best_errs(is_min) {
        Id::UNDEF
    } else {
        best.unwrap_or(Id::UNDEF)
    }
}

/// Shared finalization for `GROUP_CONCAT` across one-off and retractable accumulators.
pub(crate) fn finish_concat(ctx: &Ctx, distinct: bool, empty: bool, ok: bool, text: String) -> Id {
    let nothing = empty && distinct && ctx.aggregate_errors.distinct_concat_of_nothing_unbound();
    if ok && !nothing {
        // SPARQL 1.1: the result is a simple literal
        ctx.intern_value(&Value::Str(text.into()))
    } else {
        Id::UNDEF
    }
}

/// How one aggregate folds the rows of its groups. `merge` serves item 1066's parallel
/// grouping split, and `retract` serves the incremental view operator in
/// `incr/ops/group.rs`.
pub(crate) trait Accumulator: Send {
    /// Add `groups` groups with empty state, after those held.
    fn init(&mut self, groups: usize);

    /// Fold row `i` of the batch into group `slots[i]`, for every row, in row order. A
    /// row's slot is the number of its group among those `init` made.
    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()>;

    /// Whether [`Accumulator::merge`] gives what one accumulator over all the rows gives.
    #[allow(dead_code, reason = "merge serves item 1066's parallel grouping")]
    fn mergeable(&self) -> bool {
        false
    }

    /// Fold group `j` of `other`, which saw rows that come after the rows of this
    /// accumulator, into group `into[j]` of this one, for every group of `other`.
    /// `other` is of the same kind and aggregate. An accumulator that is not
    /// [`mergeable`](Accumulator::mergeable) leaves its groups as they are and fails.
    #[allow(dead_code, reason = "merge serves item 1066's parallel grouping")]
    fn merge(&mut self, _ctx: &Ctx, _other: &mut dyn Accumulator, _into: &[u32]) -> Result<()> {
        Err(Error::unsupported("this aggregate cannot be merged"))
    }

    /// The value of group `group`, which the call takes out of the accumulator.
    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id>;

    /// Read the current value of group `group` without destroying its state, for
    /// persistent callers such as incremental views.
    fn read(&mut self, _ctx: &Ctx, _group: usize) -> Result<Id> {
        Err(Error::unsupported(
            "this aggregate cannot be read without finishing",
        ))
    }

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

    /// Drop every group's state.
    fn clear(&mut self) {}

    /// Remove group `group` by swapping in the last group, keeping slots dense.
    fn swap_remove(&mut self, _group: usize) {}

    /// Release excess backing capacity after group deletions so view memory budgets stay
    /// accurate without reallocating on every individual removal.
    fn shrink_unused(&mut self) {}

    /// Approximate bytes retained across all live groups and backing allocations, for
    /// view memory budgeting.
    fn retained_bytes(&self) -> u64 {
        0
    }

    /// Persist group `group`'s state to a checkpoint or remap writer.
    fn save_group(&self, _group: usize, _w: &mut Writer) -> Result<()> {
        Err(Error::unsupported("this aggregate cannot be checkpointed"))
    }

    /// Restore group `group`'s state from a checkpoint or remap reader.
    fn load_group(&mut self, _group: usize, _r: &mut Reader) -> Result<()> {
        Err(Error::unsupported("this aggregate cannot be checkpointed"))
    }

    /// Clone this accumulator as a boxed trait object when supported.
    fn clone_box(&self) -> Option<Box<dyn Accumulator>> {
        None
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

/// Construct a retractable accumulator for an incremental view aggregate.
pub(crate) fn retractable(agg: &Agg, row_width: usize) -> Result<Box<dyn Accumulator>> {
    match &agg.func {
        AggregateFunction::Count => {
            if agg.distinct {
                let mut inner_agg = agg.clone();
                inner_agg.distinct = false;
                let inner: Box<dyn Accumulator> = Box::new(Running::new(&inner_agg, false));
                Ok(Box::new(RetractableDistinct::new(
                    inner,
                    agg.expr.is_none(),
                    row_width,
                )))
            } else {
                Ok(Box::new(Running::new(agg, false)))
            }
        }
        AggregateFunction::Sum | AggregateFunction::Avg => {
            let mut inner_agg = agg.clone();
            inner_agg.distinct = false;
            let inner: Box<dyn Accumulator> = Box::new(RetractableSum::new(&inner_agg));
            if agg.distinct {
                Ok(Box::new(RetractableDistinct::new(inner, false, row_width)))
            } else {
                Ok(inner)
            }
        }
        AggregateFunction::Min
        | AggregateFunction::Max
        | AggregateFunction::Sample
        | AggregateFunction::GroupConcat { .. } => Ok(Box::new(Ordered::new(agg))),
        AggregateFunction::Custom(_) => Err(Error::unsupported(
            "custom aggregates do not have a retractable accumulator",
        )),
    }
}

/// The aggregates whose state is [`AggState`]: COUNT, SUM, AVG, MIN, MAX, SAMPLE and
/// ARQ's variance and deviation aggregates.
#[derive(Clone)]
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
}

impl Accumulator for Running {
    fn init(&mut self, groups: usize) {
        for _ in 0..groups {
            self.states.push(AggState::new(&self.agg));
        }
        if self.is_best() {
            self.payload.resize(self.states.len(), 0);
        }
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        let agg = &self.agg;
        if let Some(weights) = batch.weights {
            match (&agg.func, batch.arg) {
                (AggregateFunction::Count, Arg::Star) => {
                    for (row, (&slot, &w)) in slots.iter().zip(weights).enumerate() {
                        if row.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if w > 0
                            && let AggState::Count(n) = &mut self.states[slot as usize]
                        {
                            *n += w as u64;
                        }
                    }
                }
                (AggregateFunction::Count, Arg::Column(values)) => {
                    for (row, ((&slot, id), &w)) in
                        slots.iter().zip(values).zip(weights).enumerate()
                    {
                        if row.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if w > 0
                            && !id.is_undef()
                            && let AggState::Count(n) = &mut self.states[slot as usize]
                        {
                            *n += w as u64;
                        }
                    }
                }
                (AggregateFunction::Count, Arg::Unbound) => {
                    ctx.check()?;
                }
                _ => {
                    let best = self.track && self.is_best();
                    for (row, &slot) in slots.iter().enumerate() {
                        if row.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        let w = batch.add_weight(row);
                        if w == 0 {
                            continue;
                        }
                        let id = batch.value(row);
                        if best && let Some(id) = id {
                            let decoded = ctx.decoded_bytes(id)?;
                            let held = &mut self.payload[slot as usize];
                            if decoded > *held {
                                self.growth += decoded - *held;
                                *held = decoded;
                            }
                        }
                        self.states[slot as usize].add_weighted(ctx, agg, id, w)?;
                    }
                }
            }
            return Ok(());
        }
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
                return Err(Error::unsupported("this aggregate cannot be merged"));
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

    fn read(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        Ok(self.states[group].read(ctx, &self.agg))
    }

    fn retractable(&self) -> bool {
        matches!(
            self.agg.func,
            AggregateFunction::Count | AggregateFunction::Sum | AggregateFunction::Avg
        ) && !self.agg.distinct
    }

    fn retract(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<bool> {
        if !self.retractable() {
            return Ok(false);
        }
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let w = batch.retract_weight(row);
            if w == 0 {
                continue;
            }
            if !self.states[slot as usize].retract(batch.value(row), w) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn clear(&mut self) {
        self.states = Vec::new();
        self.payload = Vec::new();
        self.growth = 0;
    }

    fn swap_remove(&mut self, group: usize) {
        self.states.swap_remove(group);
        if self.is_best() {
            self.payload.swap_remove(group);
        }
        if self.states.is_empty() {
            self.states = Vec::new();
            self.payload = Vec::new();
        }
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.states);
        shrink_vec(&mut self.payload);
    }

    fn retained_bytes(&self) -> u64 {
        self.states.capacity() as u64 * size_of::<AggState>() as u64
            + self.payload.capacity() as u64 * size_of::<u64>() as u64
    }

    fn save_group(&self, group: usize, w: &mut Writer) -> Result<()> {
        match &self.states[group] {
            AggState::Count(n) => {
                w.u64(*n);
                Ok(())
            }
            _ => Err(Error::unsupported(
                "only COUNT Running accumulators can be checkpointed",
            )),
        }
    }

    fn load_group(&mut self, group: usize, r: &mut Reader) -> Result<()> {
        match &mut self.states[group] {
            AggState::Count(n) => {
                *n = r.u64()?;
                Ok(())
            }
            _ => Err(Error::unsupported(
                "only COUNT Running accumulators can be checkpointed",
            )),
        }
    }

    fn clone_box(&self) -> Option<Box<dyn Accumulator>> {
        Some(Box::new(self.clone()))
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

/// A 256-bit two's complement integer, for exact sums of 128-bit products.
#[derive(Default)]
struct Wide {
    hi: u128,
    lo: u128,
}

impl Wide {
    fn add(&mut self, hi: u128, lo: u128) {
        let (lo, carry) = self.lo.overflowing_add(lo);
        self.lo = lo;
        self.hi = self.hi.wrapping_add(hi).wrapping_add(u128::from(carry));
    }

    /// Add `a × b`.
    fn add_product(&mut self, a: i128, b: i64) {
        let negative = (a < 0) != (b < 0);
        let (m, n) = (a.unsigned_abs(), u128::from(b.unsigned_abs()));
        // |a| × |b| as two 128-bit limbs, from the 64-bit halves of |a|
        let low = (m & u128::from(u64::MAX)) * n;
        let high = (m >> 64) * n;
        let (lo, carry) = low.overflowing_add(high << 64);
        let hi = (high >> 64) + u128::from(carry);
        if negative {
            // two's complement: invert and add one
            let (lo, carry) = (!lo).overflowing_add(1);
            self.add((!hi).wrapping_add(u128::from(carry)), lo);
        } else {
            self.add(hi, lo);
        }
    }

    /// The value when it fits in 128 bits.
    fn to_i128(&self) -> Option<i128> {
        let v = self.lo as i128;
        let sign = if v < 0 { u128::MAX } else { 0 };
        (self.hi == sign).then_some(v)
    }

    /// Approximate value as `f64`, for promoting an exact subtotal that overflows
    /// `xsd:decimal` when the group also holds float or double terms.
    fn to_f64(&self) -> f64 {
        let neg = (self.hi as i128) < 0;
        let (hi, lo) = if neg {
            let (lo, carry) = (!self.lo).overflowing_add(1);
            ((!self.hi).wrapping_add(u128::from(carry)), lo)
        } else {
            (self.hi, self.lo)
        };
        // 2^128 is exact in IEEE 754 binary64.
        const TWO_POW_128: f64 = 340_282_366_920_938_463_463_374_607_431_768_211_456.0;
        let mag = (hi as f64) * TWO_POW_128 + (lo as f64);
        if neg { -mag } else { mag }
    }
}

#[derive(Clone, Debug, Default)]
struct SumState {
    rows: i64,
    /// Arguments that were errors or not numbers, which make the sum unbound
    errs: i64,
    /// Arguments that were errors (`Id::UNDEF`), apart from non-numeric values, for
    /// the reading in which AVG over only errors is zero
    undef: i64,
    /// The integer sum, in wrapping arithmetic so that removals stay exact
    int: i128,
    /// The decimal values as counts by their 128-bit encoding
    decs: FxHashMap<i128, i64>,
    floats: FxHashMap<u32, i64>,
    doubles: FxHashMap<u64, i64>,
}

impl SumState {
    /// Add `w` copies of a value (`None`: an error or non-numeric value).
    fn add(&mut self, v: Option<&Value>, w: i64) {
        self.rows += w;
        match v {
            Some(Value::Integer(i)) => {
                self.int = self
                    .int
                    .wrapping_add((i64::from(*i) as i128).wrapping_mul(w as i128));
            }
            Some(Value::Decimal(d)) => {
                let k = i128::from_be_bytes(d.to_be_bytes());
                let n = self.decs.entry(k).or_insert(0);
                *n += w;
                if *n == 0 {
                    self.decs.remove(&k);
                    shrink_map(&mut self.decs);
                }
            }
            Some(Value::Float(f)) => {
                let b = f32::from(*f).to_bits();
                let n = self.floats.entry(b).or_insert(0);
                *n += w;
                if *n == 0 {
                    self.floats.remove(&b);
                    shrink_map(&mut self.floats);
                }
            }
            Some(Value::Double(d)) => {
                let b = f64::from(*d).to_bits();
                let n = self.doubles.entry(b).or_insert(0);
                *n += w;
                if *n == 0 {
                    self.doubles.remove(&b);
                    shrink_map(&mut self.doubles);
                }
            }
            _ => self.errs += w,
        }
    }

    fn sum(&self) -> Option<Value> {
        if self.errs != 0 {
            return None;
        }
        let mut acc = if !self.decs.is_empty() {
            // Summed exactly in 256 bits so values that cancel out do not overflow on
            // the way; the integers join the decimals as a one-off evaluation promotes
            // them. When floats or doubles are present and the exact subtotal overflows
            // xsd:decimal, promote the 256-bit subtotal directly into the target
            // floating-point type so the result stays finite and order-independent.
            let mut acc = Wide::default();
            for (k, n) in &self.decs {
                acc.add_product(*k, *n);
            }
            acc.add_product(self.int, 1_000_000_000_000_000_000);
            if let Some(raw) = acc.to_i128() {
                Value::Decimal(Decimal::from_be_bytes(raw.to_be_bytes()))
            } else if !self.doubles.is_empty() {
                Value::Double(Double::from(acc.to_f64() / 1e18))
            } else if !self.floats.is_empty() {
                Value::Float(Float::from((acc.to_f64() / 1e18) as f32))
            } else {
                return None;
            }
        } else if !self.floats.is_empty() || !self.doubles.is_empty() {
            // Promote an i64-overflowing integer sum through exact_sum when it fits a
            // decimal, and otherwise directly into the target floating-point type.
            if let Some(v) = exact_sum(self.int) {
                v
            } else if !self.doubles.is_empty() {
                Value::Double(Double::from(self.int as f64))
            } else {
                Value::Float(Float::from(self.int as f32))
            }
        } else {
            Value::Integer(i64::try_from(self.int).ok()?.into())
        };
        let has_double = !self.doubles.is_empty();
        let mut floats: Vec<(f32, i64)> = self
            .floats
            .iter()
            .map(|(b, n)| (f32::from_bits(*b), *n))
            .collect();
        floats.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (f, n) in floats {
            let term = if has_double {
                Value::Double(Double::from(f64::from(f) * n as f64))
            } else {
                Value::Float(Float::from(f * n as f32))
            };
            acc = arith(NumOp::Add, &acc, &term).ok()?;
        }
        let mut doubles: Vec<(f64, i64)> = self
            .doubles
            .iter()
            .map(|(b, n)| (f64::from_bits(*b), *n))
            .collect();
        doubles.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (d, n) in doubles {
            acc = arith(NumOp::Add, &acc, &Value::Double(Double::from(d * n as f64))).ok()?;
        }
        Some(acc)
    }

    fn map_bytes(&self) -> u64 {
        (self.floats.capacity() + self.doubles.capacity()) as u64 * 24
            + self.decs.capacity() as u64 * 40
    }

    fn save(&self, w: &mut Writer) {
        w.i64(self.rows);
        w.i64(self.errs);
        w.i64(self.undef);
        w.i128(self.int);
        w.u64(self.decs.len() as u64);
        for (k, n) in &self.decs {
            w.i128(*k);
            w.i64(*n);
        }
        w.u64(self.floats.len() as u64);
        for (b, n) in &self.floats {
            w.u64(*b as u64);
            w.i64(*n);
        }
        w.u64(self.doubles.len() as u64);
        for (b, n) in &self.doubles {
            w.u64(*b);
            w.i64(*n);
        }
    }

    fn load(r: &mut Reader) -> Result<Self> {
        let mut s = Self {
            rows: r.i64()?,
            errs: r.i64()?,
            undef: r.i64()?,
            int: r.i128()?,
            ..Default::default()
        };
        for _ in 0..r.u64()? {
            let k = r.i128()?;
            s.decs.insert(k, r.i64()?);
        }
        for _ in 0..r.u64()? {
            let b = r.u64()? as u32;
            s.floats.insert(b, r.i64()?);
        }
        for _ in 0..r.u64()? {
            let b = r.u64()?;
            s.doubles.insert(b, r.i64()?);
        }
        Ok(s)
    }
}

/// Retractable `SUM` and `AVG` accumulator with exact integer and decimal tracking and
/// refcounted float, double and error states.
#[derive(Clone)]
pub(crate) struct RetractableSum {
    is_avg: bool,
    groups: Vec<SumState>,
}

impl RetractableSum {
    pub(crate) fn new(agg: &Agg) -> Self {
        Self {
            is_avg: matches!(agg.func, AggregateFunction::Avg),
            groups: Vec::new(),
        }
    }

    fn fold(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32], retract: bool) -> Result<()> {
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let mag = if retract {
                batch.retract_weight(row)
            } else {
                batch.add_weight(row)
            };
            if mag == 0 {
                continue;
            }
            let w = if retract { -(mag as i64) } else { mag as i64 };
            let s = &mut self.groups[slot as usize];
            let id = batch.value(row).unwrap_or(Id::UNDEF);
            if id.is_undef() {
                s.add(None, w);
                s.undef += w;
            } else if id.tag() == Tag::Int {
                s.rows += w;
                s.int = s
                    .int
                    .wrapping_add((id.as_i64() as i128).wrapping_mul(w as i128));
            } else {
                let v = ctx.value(id);
                ctx.check()?;
                s.add(v.as_ref(), w);
            }
        }
        Ok(())
    }
}

impl Accumulator for RetractableSum {
    fn init(&mut self, groups: usize) {
        self.groups
            .resize_with(self.groups.len() + groups, SumState::default);
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        self.fold(ctx, batch, slots, false)
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let id = self.read(ctx, group)?;
        self.groups[group] = SumState::default();
        Ok(id)
    }

    fn read(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let s = &self.groups[group];
        Ok(finish_sum_avg(
            ctx,
            self.is_avg,
            s.rows.max(0) as u64,
            s.undef.max(0) as u64,
            s.sum(),
        ))
    }

    fn retractable(&self) -> bool {
        true
    }

    fn retract(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<bool> {
        if !batch.retract_weights_fit_i64(slots.len()) {
            return Ok(false);
        }
        self.fold(ctx, batch, slots, true)?;
        Ok(true)
    }

    fn clear(&mut self) {
        self.groups = Vec::new();
    }

    fn swap_remove(&mut self, group: usize) {
        self.groups.swap_remove(group);
        if self.groups.is_empty() {
            self.groups = Vec::new();
        }
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.groups);
    }

    fn retained_bytes(&self) -> u64 {
        self.groups.capacity() as u64 * size_of::<SumState>() as u64
            + self.groups.iter().map(SumState::map_bytes).sum::<u64>()
    }

    fn save_group(&self, group: usize, w: &mut Writer) -> Result<()> {
        self.groups[group].save(w);
        Ok(())
    }

    fn load_group(&mut self, group: usize, r: &mut Reader) -> Result<()> {
        self.groups[group] = SumState::load(r)?;
        Ok(())
    }

    fn clone_box(&self) -> Option<Box<dyn Accumulator>> {
        Some(Box::new(self.clone()))
    }

    fn group_bytes(&self) -> u64 {
        96
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The values of a group as an ordered multiset: the distinct values in the order of
/// `ORDER BY` with their counts. Values the order does not tell apart, such as
/// `"01"^^xsd:integer` and `"1"^^xsd:integer`, form a run in the order they entered the
/// group, which a checkpoint and a generation switch keep, since the vector is saved and
/// loaded in its order and the order depends on the terms and not on their ids.
#[derive(Clone, Debug, Default)]
struct Multiset {
    vals: Vec<(Id, i64)>,
    /// Count of arguments that were errors (`Id::UNDEF`), which make `GROUP_CONCAT`
    /// unbound and `MIN` or `MAX` unbound under readings where errors propagate.
    errs: i64,
    /// `SAMPLE`: the value reported, kept while the group holds it
    sample: Option<Id>,
}

impl Multiset {
    fn position(
        &self,
        ctx: &Ctx,
        v: Id,
        value: Option<&Value>,
    ) -> std::result::Result<usize, usize> {
        let cmp = |x: Id| order_cmp(ctx.value(x).as_ref(), value);
        let i = self.vals.binary_search_by(|(x, _)| cmp(*x))?;
        let mut lo = i;
        while lo > 0 && cmp(self.vals[lo - 1].0) == Ordering::Equal {
            lo -= 1;
        }
        let mut hi = i + 1;
        while hi < self.vals.len() && cmp(self.vals[hi].0) == Ordering::Equal {
            hi += 1;
        }
        match self.vals[lo..hi].iter().position(|(x, _)| *x == v) {
            Some(k) => Ok(lo + k),
            None => Err(hi),
        }
    }

    fn add(&mut self, ctx: &Ctx, v: Id, w: i64) -> Result<()> {
        let value = ctx.value(v);
        ctx.check()?;
        match self.position(ctx, v, value.as_ref()) {
            Ok(i) => {
                self.vals[i].1 += w;
                if self.vals[i].1 == 0 {
                    self.vals.remove(i);
                    shrink_vec(&mut self.vals);
                }
            }
            Err(i) => self.vals.insert(i, (v, w)),
        }
        ctx.check()
    }

    fn first_of_run(&self, ctx: &Ctx, i: usize) -> Id {
        let value = ctx.value(self.vals[i].0);
        let mut lo = i;
        while lo > 0
            && self.vals[lo - 1].1 > 0
            && order_cmp(ctx.value(self.vals[lo - 1].0).as_ref(), value.as_ref()) == Ordering::Equal
        {
            lo -= 1;
        }
        self.vals[lo].0
    }

    fn min(&self) -> Option<Id> {
        self.vals.iter().find(|(_, n)| *n > 0).map(|(v, _)| *v)
    }

    fn max(&self, ctx: &Ctx) -> Option<Id> {
        let i = self.vals.iter().rposition(|(_, n)| *n > 0)?;
        Some(self.first_of_run(ctx, i))
    }

    fn settle(&mut self, ctx: &Ctx) {
        if let Some(s) = self.sample {
            let value = ctx.value(s);
            if let Ok(i) = self.position(ctx, s, value.as_ref())
                && self.vals[i].1 > 0
            {
                return;
            }
        }
        self.sample = self.min();
    }
}

#[derive(Clone)]
enum OrderedKind {
    Min,
    Max,
    Sample,
    Concat { separator: String, distinct: bool },
}

/// Retractable accumulator for `MIN`, `MAX`, `SAMPLE` and `GROUP_CONCAT` (with or
/// without `DISTINCT`), backed by an ordered [`Multiset`] per group.
#[derive(Clone)]
pub(crate) struct Ordered {
    kind: OrderedKind,
    groups: Vec<Multiset>,
}

impl Ordered {
    pub(crate) fn new(agg: &Agg) -> Self {
        let kind = match &agg.func {
            AggregateFunction::Min => OrderedKind::Min,
            AggregateFunction::Max => OrderedKind::Max,
            AggregateFunction::Sample => OrderedKind::Sample,
            AggregateFunction::GroupConcat { separator } => OrderedKind::Concat {
                separator: separator.as_deref().unwrap_or(" ").to_string(),
                distinct: agg.distinct,
            },
            _ => unreachable!("an ordered retractable aggregate"),
        };
        Self {
            kind,
            groups: Vec::new(),
        }
    }

    fn fold(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32], retract: bool) -> Result<()> {
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let mag = if retract {
                batch.retract_weight(row)
            } else {
                batch.add_weight(row)
            };
            if mag == 0 {
                continue;
            }
            let w = if retract { -(mag as i64) } else { mag as i64 };
            let m = &mut self.groups[slot as usize];
            let id = batch.value(row).unwrap_or(Id::UNDEF);
            if id.is_undef() {
                m.errs += w;
            } else {
                m.add(ctx, id, w)?;
            }
        }
        Ok(())
    }
}

impl Accumulator for Ordered {
    fn init(&mut self, groups: usize) {
        self.groups
            .resize_with(self.groups.len() + groups, Multiset::default);
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        self.fold(ctx, batch, slots, false)
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let id = self.read(ctx, group)?;
        self.groups[group] = Multiset::default();
        Ok(id)
    }

    fn read(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let m = &mut self.groups[group];
        match &self.kind {
            OrderedKind::Min => Ok(finish_best(ctx, true, m.errs.max(0) as u64, m.min())),
            OrderedKind::Max => {
                let best = m.max(ctx);
                ctx.check()?;
                Ok(finish_best(ctx, false, m.errs.max(0) as u64, best))
            }
            OrderedKind::Sample => {
                m.settle(ctx);
                ctx.check()?;
                Ok(m.sample.unwrap_or(Id::UNDEF))
            }
            OrderedKind::Concat {
                separator,
                distinct,
            } => {
                if m.errs > 0 {
                    return Ok(Id::UNDEF);
                }
                let strings = ctx.aggregate_errors.concat_strings_only();
                let charge = ctx.charge(0)?;
                let mut charged = 0u64;
                let mut out = String::new();
                let mut empty = true;
                for (v, n) in &m.vals {
                    if *n <= 0 {
                        continue;
                    }
                    let val = ctx.value(*v);
                    ctx.check()?;
                    let Some(s) = val
                        .filter(|v| !strings || v.is_string())
                        .and_then(|v| v.lexical().ok())
                    else {
                        return Ok(Id::UNDEF);
                    };
                    let reps = if *distinct { 1u64 } else { *n as u64 };
                    if s.is_empty() && separator.is_empty() {
                        empty = false;
                        continue;
                    }
                    let seps = if empty { reps - 1 } else { reps };
                    let added = reps
                        .checked_mul(s.len() as u64)
                        .and_then(|b| {
                            seps.checked_mul(separator.len() as u64)
                                .and_then(|sb| b.checked_add(sb))
                        })
                        .and_then(|b| usize::try_from(b).ok());
                    let Some(added) = added else {
                        return Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            limit: ctx.mem_limit,
                            requested: u64::MAX,
                        }));
                    };
                    let Some(new_len) = out.len().checked_add(added) else {
                        return Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            limit: ctx.mem_limit,
                            requested: u64::MAX,
                        }));
                    };
                    let need = new_len as u64;
                    if need > charged {
                        charge.add(need - charged)?;
                        charged = need;
                    }
                    let mut rem = reps;
                    while rem > 0 {
                        ctx.check()?;
                        let chunk = rem.min(1024);
                        let chunk_seps = if empty { chunk - 1 } else { chunk };
                        let chunk_bytes =
                            (chunk as usize) * s.len() + (chunk_seps as usize) * separator.len();
                        let next_len = out.len() + chunk_bytes;
                        if next_len > out.capacity() {
                            let cap_bound = if rem == chunk {
                                let amortized = new_len.max(out.capacity().saturating_mul(2));
                                if amortized > new_len
                                    && (amortized as u64) > charged
                                    && charge.add(amortized as u64 - charged).is_ok()
                                {
                                    charged = amortized as u64;
                                    amortized
                                } else {
                                    new_len
                                }
                            } else {
                                next_len.max(out.capacity().saturating_mul(2)).min(new_len)
                            };
                            if cap_bound > out.capacity() {
                                out.reserve_exact(cap_bound - out.len());
                            }
                        }
                        for _ in 0..chunk {
                            if !empty {
                                out.push_str(separator);
                            }
                            empty = false;
                            out.push_str(&s);
                        }
                        rem -= chunk;
                    }
                }
                let id = finish_concat(ctx, *distinct, empty, true, out);
                drop(charge);
                ctx.check()?;
                Ok(id)
            }
        }
    }

    fn retractable(&self) -> bool {
        true
    }

    fn retract(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<bool> {
        if !batch.retract_weights_fit_i64(slots.len()) {
            return Ok(false);
        }
        self.fold(ctx, batch, slots, true)?;
        Ok(true)
    }

    fn clear(&mut self) {
        self.groups = Vec::new();
    }

    fn swap_remove(&mut self, group: usize) {
        self.groups.swap_remove(group);
        if self.groups.is_empty() {
            self.groups = Vec::new();
        }
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.groups);
    }

    fn retained_bytes(&self) -> u64 {
        self.groups.capacity() as u64 * size_of::<Multiset>() as u64
            + self
                .groups
                .iter()
                .map(|m| m.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64)
                .sum::<u64>()
    }

    fn save_group(&self, group: usize, w: &mut Writer) -> Result<()> {
        let m = &self.groups[group];
        w.u64(m.vals.len() as u64);
        for (v, n) in &m.vals {
            w.id(*v)?;
            w.i64(*n);
        }
        w.i64(m.errs);
        match m.sample {
            Some(v) => {
                w.u64(1);
                w.id(v)?;
            }
            None => w.u64(0),
        }
        Ok(())
    }

    fn load_group(&mut self, group: usize, r: &mut Reader) -> Result<()> {
        let mut m = Multiset::default();
        // The entries were written in their order, which the terms keep.
        for _ in 0..r.u64()? {
            let v = r.id()?;
            m.vals.push((v, r.i64()?));
        }
        m.errs = r.i64()?;
        m.sample = match r.u64()? {
            0 => None,
            _ => Some(r.id()?),
        };
        self.groups[group] = m;
        Ok(())
    }

    fn clone_box(&self) -> Option<Box<dyn Accumulator>> {
        Some(Box::new(self.clone()))
    }

    fn group_bytes(&self) -> u64 {
        40
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Clone)]
enum DistinctGroup {
    Values(FxHashMap<Id, i64>),
    Rows(FxHashMap<Box<[Id]>, i64>),
}

/// Retractable `DISTINCT` wrapper over any retractable accumulator. Each group counts
/// occurrences of each distinct value (or full row for `COUNT(DISTINCT *)`), forwarding
/// transitions across zero to `inner` and passing errors (`Id::UNDEF`) through
/// un-deduplicated with their full weight.
pub(crate) struct RetractableDistinct {
    inner: Box<dyn Accumulator>,
    groups: Vec<DistinctGroup>,
    star: bool,
    row_width: usize,
}

impl RetractableDistinct {
    pub(crate) fn new(inner: Box<dyn Accumulator>, star: bool, row_width: usize) -> Self {
        Self {
            inner,
            groups: Vec::new(),
            star,
            row_width,
        }
    }

    fn empty_group(&self) -> DistinctGroup {
        if self.star {
            DistinctGroup::Rows(FxHashMap::default())
        } else {
            DistinctGroup::Values(FxHashMap::default())
        }
    }
}

impl Accumulator for RetractableDistinct {
    fn init(&mut self, groups: usize) {
        for _ in 0..groups {
            self.groups.push(self.empty_group());
        }
        self.inner.init(groups);
    }

    fn add_batch(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<()> {
        let mut keep_slots: Vec<u32> = Vec::new();
        let mut keep_vals: Vec<Id> = Vec::new();
        let mut keep_weights: Vec<i64> = Vec::new();
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let w = batch.add_weight(row) as i64;
            if w == 0 {
                continue;
            }
            match &mut self.groups[slot as usize] {
                DistinctGroup::Rows(m) => {
                    let key: Box<[Id]> = batch
                        .table
                        .cols
                        .iter()
                        .map(|c| c[batch.base + row])
                        .collect();
                    let e = m.entry(key.clone()).or_insert(0);
                    let before = *e;
                    *e += w;
                    let after = *e;
                    if after == 0 {
                        m.remove(&key);
                        shrink_map(m);
                    }
                    if before <= 0 && after > 0 {
                        keep_slots.push(slot);
                        keep_weights.push(1);
                    }
                }
                DistinctGroup::Values(m) => {
                    let id = batch.value(row).unwrap_or(Id::UNDEF);
                    if id.is_undef() {
                        // An error is never a duplicate.
                        keep_slots.push(slot);
                        keep_vals.push(Id::UNDEF);
                        keep_weights.push(w);
                    } else {
                        let e = m.entry(id).or_insert(0);
                        let before = *e;
                        *e += w;
                        let after = *e;
                        if after == 0 {
                            m.remove(&id);
                            shrink_map(m);
                        }
                        if before <= 0 && after > 0 {
                            keep_slots.push(slot);
                            keep_vals.push(id);
                            keep_weights.push(1);
                        }
                    }
                }
            }
        }
        if keep_slots.is_empty() {
            return Ok(());
        }
        let arg = if self.star {
            Arg::Star
        } else {
            Arg::Column(&keep_vals)
        };
        let inner = Batch {
            table: batch.table,
            arg,
            rows: None,
            weights: Some(&keep_weights),
            base: batch.base,
        };
        self.inner.add_batch(ctx, &inner, &keep_slots)
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let id = self.inner.finish(ctx, group)?;
        self.groups[group] = self.empty_group();
        Ok(id)
    }

    fn read(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        self.inner.read(ctx, group)
    }

    fn retractable(&self) -> bool {
        self.inner.retractable()
    }

    fn retract(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<bool> {
        if !batch.retract_weights_fit_i64(slots.len()) {
            return Ok(false);
        }
        let mut drop_slots: Vec<u32> = Vec::new();
        let mut drop_vals: Vec<Id> = Vec::new();
        let mut drop_weights: Vec<i64> = Vec::new();
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let w = batch.retract_weight(row) as i64;
            if w == 0 {
                continue;
            }
            match &mut self.groups[slot as usize] {
                DistinctGroup::Rows(m) => {
                    let key: Box<[Id]> = batch
                        .table
                        .cols
                        .iter()
                        .map(|c| c[batch.base + row])
                        .collect();
                    let e = m.entry(key.clone()).or_insert(0);
                    let before = *e;
                    *e -= w;
                    let after = *e;
                    if after == 0 {
                        m.remove(&key);
                        shrink_map(m);
                    }
                    if before > 0 && after <= 0 {
                        drop_slots.push(slot);
                        drop_weights.push(-1);
                    }
                }
                DistinctGroup::Values(m) => {
                    let id = batch.value(row).unwrap_or(Id::UNDEF);
                    if id.is_undef() {
                        drop_slots.push(slot);
                        drop_vals.push(Id::UNDEF);
                        drop_weights.push(-w);
                    } else {
                        let e = m.entry(id).or_insert(0);
                        let before = *e;
                        *e -= w;
                        let after = *e;
                        if after == 0 {
                            m.remove(&id);
                            shrink_map(m);
                        }
                        if before > 0 && after <= 0 {
                            drop_slots.push(slot);
                            drop_vals.push(id);
                            drop_weights.push(-1);
                        }
                    }
                }
            }
        }
        if drop_slots.is_empty() {
            return Ok(true);
        }
        let arg = if self.star {
            Arg::Star
        } else {
            Arg::Column(&drop_vals)
        };
        let inner = Batch {
            table: batch.table,
            arg,
            rows: None,
            weights: Some(&drop_weights),
            base: batch.base,
        };
        self.inner.retract(ctx, &inner, &drop_slots)
    }

    fn clear(&mut self) {
        self.groups = Vec::new();
        self.inner.clear();
    }

    fn swap_remove(&mut self, group: usize) {
        self.groups.swap_remove(group);
        if self.groups.is_empty() {
            self.groups = Vec::new();
        }
        self.inner.swap_remove(group);
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.groups);
        self.inner.shrink_unused();
    }

    fn retained_bytes(&self) -> u64 {
        let outer = self.groups.capacity() as u64 * size_of::<DistinctGroup>() as u64;
        let own: u64 = self
            .groups
            .iter()
            .map(|g| match g {
                DistinctGroup::Rows(m) => {
                    m.len() as u64 * (self.row_width as u64 * 8)
                        + m.capacity() as u64 * (size_of::<(Box<[Id]>, i64)>() as u64 + 16)
                }
                DistinctGroup::Values(m) => m.capacity() as u64 * 24,
            })
            .sum();
        outer + own + self.inner.retained_bytes()
    }

    fn save_group(&self, group: usize, w: &mut Writer) -> Result<()> {
        match &self.groups[group] {
            DistinctGroup::Rows(m) => {
                w.u64(m.len() as u64);
                for (r, n) in m {
                    w.row(r)?;
                    w.i64(*n);
                }
            }
            DistinctGroup::Values(m) => {
                w.u64(m.len() as u64);
                for (v, n) in m {
                    w.id(*v)?;
                    w.i64(*n);
                }
            }
        }
        self.inner.save_group(group, w)
    }

    fn load_group(&mut self, group: usize, r: &mut Reader) -> Result<()> {
        match &mut self.groups[group] {
            DistinctGroup::Rows(m) => {
                m.clear();
                for _ in 0..r.u64()? {
                    let row = r.row()?;
                    m.insert(row, r.i64()?);
                }
            }
            DistinctGroup::Values(m) => {
                m.clear();
                for _ in 0..r.u64()? {
                    let v = r.id()?;
                    m.insert(v, r.i64()?);
                }
            }
        }
        self.inner.load_group(group, r)
    }

    fn clone_box(&self) -> Option<Box<dyn Accumulator>> {
        let inner = self.inner.clone_box()?;
        Some(Box::new(Self {
            inner,
            groups: self.groups.clone(),
            star: self.star,
            row_width: self.row_width,
        }))
    }

    fn group_bytes(&self) -> u64 {
        self.inner.group_bytes() + 96
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
        Ok(finish_concat(
            ctx,
            self.distinct,
            state.empty,
            state.ok,
            state.text,
        ))
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
            weights: None,
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
