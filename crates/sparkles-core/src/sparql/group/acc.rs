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

use super::deferred::{Deferred, subtotal};
use super::op::BatchBudget;
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::incr::persist::{Reader, Writer};
use crate::sparql::ctx::{Charge, Ctx};
use crate::sparql::exec::{AggState, ScratchAdmit, SumAdmit};
use crate::sparql::expr::Expr;
use crate::sparql::plan::Agg;
use crate::sparql::table::Table;
use crate::sparql::value::{NumOp, Value, arith, exact_sum, order_cmp};
use oxsdatatypes::{Decimal, Double, Float};
use rustc_hash::{FxBuildHasher, FxHashSet};
use spargebra::algebra::AggregateFunction;
use std::any::Any;
use std::cmp::Ordering;

type HbMap<K, V> = hashbrown::HashMap<K, V, FxBuildHasher>;

/// Largest byte length a Rust `Vec<u8>` or `String` allocation can represent.
const MAX_ALLOC_BYTES: usize = isize::MAX as usize;

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

fn shrink_map<K: Eq + std::hash::Hash, V>(m: &mut HbMap<K, V>) {
    let entry_bytes = size_of::<(K, V)>() + 1;
    if m.is_empty() {
        *m = HbMap::default();
    } else {
        let alloc = m.allocation_size();
        if alloc >= SHRINK_MIN_CAP * entry_bytes
            && m.len().saturating_mul(entry_bytes).saturating_mul(4) <= alloc
        {
            m.shrink_to(m.len() + m.len() / 2);
        }
    }
}

#[inline]
fn bucket_capacity(buckets: usize) -> usize {
    if buckets == 0 {
        0
    } else if buckets < 8 {
        buckets - 1
    } else {
        buckets / 8 * 7
    }
}

#[inline]
pub(crate) fn hbmap_insert_growth<K: Eq + std::hash::Hash, V>(
    map: &HbMap<K, V>,
    key_extra: u64,
) -> u64 {
    let table_delta = if map.len() < map.capacity() {
        0
    } else {
        let elem = size_of::<(K, V)>();
        let alloc = map.allocation_size();
        if alloc == 0 {
            let first_cap = sparkles_budget::sizes::table_grown(0, 0, 1);
            sparkles_budget::sizes::table_bytes(first_cap, elem) as u64
        } else {
            // `HashMap::capacity()` is `items + growth_left`, so tombstones can exhaust
            // `growth_left` while `len() < full_capacity`. Derive the current bucket count
            // from the live allocation so tombstone-triggered resizes are admitted before
            // hashbrown doubles the table.
            let mut buckets = sparkles_budget::sizes::buckets(map.capacity()).max(4);
            while sparkles_budget::sizes::table_bytes(bucket_capacity(buckets), elem) < alloc {
                let Some(doubled) = buckets.checked_mul(2) else {
                    break;
                };
                buckets = doubled;
            }
            let full_cap = bucket_capacity(buckets);
            if map.len().saturating_add(1) <= full_cap / 2 {
                0
            } else {
                let next_buckets = buckets.saturating_mul(2);
                let next_bytes =
                    sparkles_budget::sizes::table_bytes(bucket_capacity(next_buckets), elem) as u64;
                next_bytes.saturating_sub(alloc as u64)
            }
        }
    };
    table_delta.saturating_add(key_extra)
}

#[inline]
fn admit_with_headroom(
    delta: u64,
    headroom: &mut u64,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<()> {
    if delta > *headroom {
        admit(delta - *headroom)?;
        *headroom = delta;
    }
    Ok(())
}

fn hbmap_load_insert<K: Eq + std::hash::Hash, V>(
    r: &Reader,
    map: &mut HbMap<K, V>,
    key: K,
    val: V,
    key_extra: u64,
) -> Result<()> {
    if map.contains_key(&key) {
        map.insert(key, val);
        return Ok(());
    }
    let before = map.allocation_size() as u64;
    let est = hbmap_insert_growth(map, key_extra);
    r.charge_state(est)?;
    map.insert(key, val);
    let actual = (map.allocation_size() as u64)
        .saturating_sub(before)
        .saturating_add(key_extra);
    if est > actual {
        r.release_state(est - actual);
    } else if actual > est {
        r.charge_state(actual - est)?;
    }
    Ok(())
}

#[inline]
fn boxed_hbmap_init_growth<K, V>(key_extra: u64) -> u64 {
    let first_cap = sparkles_budget::sizes::table_grown(0, 0, 1);
    let table = sparkles_budget::sizes::table_bytes(first_cap, size_of::<(K, V)>()) as u64;
    (size_of::<HbMap<K, V>>() as u64)
        .saturating_add(table)
        .saturating_add(key_extra)
}

#[inline]
pub(crate) fn vec_growth<T>(v: &Vec<T>, add: usize) -> u64 {
    if add == 0 || v.len().saturating_add(add) <= v.capacity() {
        0
    } else {
        let next_cap =
            sparkles_budget::sizes::vec_grown(v.capacity(), v.len(), add, size_of::<T>());
        (next_cap.saturating_sub(v.capacity()) as u64).saturating_mul(size_of::<T>() as u64)
    }
}

#[inline]
pub(crate) fn vec_insert_growth<T>(v: &Vec<T>) -> u64 {
    vec_growth(v, 1)
}

/// Shared finalization for `SUM` and `AVG` over a pre-evaluated result value, keeping
/// `AggregateErrors` readings and big-number interning scratch admission consistent.
pub(crate) fn finish_sum_avg(
    ctx: &Ctx,
    is_avg: bool,
    rows: u64,
    undef: u64,
    value: Option<Value>,
) -> Result<Id> {
    // AVG is Sum over Count, and Count leaves the errors out
    if rows > 0 && undef == rows && is_avg && ctx.aggregate_errors.avg_of_errors_zero() {
        return ctx.try_intern_value(&Value::Integer(0.into()));
    }
    let _scratch = match value.as_ref() {
        Some(Value::BigInteger(i)) => {
            Some(ctx.charge(crate::sparql::bignum::intern_scratch_bytes(i.lexical_len()))?)
        }
        Some(Value::BigDecimal(d)) => {
            Some(ctx.charge(crate::sparql::bignum::intern_scratch_bytes(d.lexical_len()))?)
        }
        _ => None,
    };
    let id = match value.as_ref() {
        Some(v) => ctx.try_intern_value(v)?,
        None => Id::UNDEF,
    };
    drop(value);
    drop(_scratch);
    ctx.check()?;
    Ok(id)
}

/// Shared finalization for `MIN` and `MAX` across one-off and retractable accumulators.
pub(crate) fn finish_best(ctx: &Ctx, is_min: bool, errs: u64, best: Option<Id>) -> Id {
    if errs > 0 && ctx.aggregate_errors.best_errs(is_min) {
        Id::UNDEF
    } else {
        best.unwrap_or(Id::UNDEF)
    }
}

fn concat_is_undef(ctx: &Ctx, distinct: bool, empty: bool, ok: bool) -> bool {
    let nothing = empty && distinct && ctx.aggregate_errors.distinct_concat_of_nothing_unbound();
    !ok || nothing
}

/// Shared finalization for `GROUP_CONCAT` across one-off and retractable accumulators.
/// Encodes directly into a simple-literal vocabulary key without intermediate `Arc<str>`
/// or `oxrdf::Literal` copies, admitting any scratch growth before reserving it.
pub(crate) fn finish_concat(
    ctx: &Ctx,
    distinct: bool,
    empty: bool,
    ok: bool,
    text: String,
) -> Result<Id> {
    if concat_is_undef(ctx, distinct, empty, ok) {
        return Ok(Id::UNDEF);
    }
    if text.is_empty() {
        let id = ctx.try_intern_key(&[b'"', crate::id::KEY_SEP])?;
        ctx.check()?;
        return Ok(id);
    }
    let len = text.len();
    let Some(key_len) = len.checked_add(2).filter(|&n| n <= MAX_ALLOC_BYTES) else {
        return Err(Error::BudgetExceeded(crate::error::Budget {
            kind: crate::error::BudgetKind::Memory,
            limit: ctx.mem_limit,
            requested: (len as u64).saturating_add(2),
        }));
    };
    let mut key = text.into_bytes();
    let _scratch = if key.capacity() < key_len {
        let charge = ctx.charge(key_len as u64)?;
        key.try_reserve_exact(key_len - len).map_err(|_| {
            Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                limit: ctx.mem_limit,
                requested: key_len as u64,
            })
        })?;
        Some(charge)
    } else {
        None
    };
    key.push(0);
    key.copy_within(0..len, 1);
    key[0] = b'"';
    key.push(crate::id::KEY_SEP);
    let id = ctx.try_intern_key(&key)?;
    drop(key);
    drop(_scratch);
    ctx.check()?;
    Ok(id)
}

/// How one aggregate folds the rows of its groups. `merge` serves item 1066's parallel
/// grouping split, and `retract` serves the incremental view operator in
/// `incr/ops/group.rs`.
pub(crate) trait Accumulator: Send {
    /// Add `groups` groups with empty state, after those held.
    fn init(&mut self, groups: usize);

    /// Fold row `i` of the batch into group `slots[i]`, for every row, in row order. A
    /// row's slot is the number of its group among those `init` made. `budget` admits any
    /// retained state growth before allocation and handles spilling sum subtotals.
    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()>;

    /// Whether [`Accumulator::merge`] gives what one accumulator over all the rows gives.
    #[allow(dead_code, reason = "merge serves item 1066's parallel grouping")]
    fn mergeable(&self) -> bool {
        false
    }

    /// Fold group `j` of `other`, which saw rows that come after the rows of this
    /// accumulator, into group `into[j]` of this one, for every group of `other`.
    /// `other` is of the same kind and aggregate. `into` must have one entry per group
    /// of `other`, every destination must be in bounds of `self`, and all destinations
    /// must be distinct; violating that precondition or attempting an inexact merge
    /// leaves both accumulators unchanged and returns `Err(Error::Unsupported(_))`.
    fn merge(
        &mut self,
        _ctx: &Ctx,
        _other: &mut dyn Accumulator,
        _into: &[u32],
        _retain: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        Err(Error::unsupported("this aggregate cannot be merged"))
    }

    /// The value of group `group`, which the call takes out of the accumulator.
    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id>;

    /// Like [`Accumulator::finish`], admitting any finalization scratch through `scratch`.
    fn finish_with(
        &mut self,
        ctx: &Ctx,
        group: usize,
        _scratch: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        self.finish(ctx, group)
    }

    /// Finish in slot and aggregate order when some subtotals have deferred arguments.
    fn finish_replay(
        &mut self,
        ctx: &Ctx,
        group: usize,
        _aggregate: usize,
        _deferred: &mut Deferred,
        scratch: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        self.finish_with(ctx, group, scratch)
    }

    /// Read the current value of group `group` without destroying its state, for
    /// persistent callers such as incremental views.
    fn read(&mut self, _ctx: &Ctx, _group: usize) -> Result<Id> {
        Err(Error::unsupported(
            "this aggregate cannot be read without finishing",
        ))
    }

    /// Upper bound on bytes that calling [`Accumulator::finish`] for every group can
    /// charge to `ctx.retain_bytes()` when interning results. Polls `ctx.check()` in
    /// bounded chunks so cancellation and deadlines remain responsive.
    fn finish_intern_bytes(&self, _ctx: &Ctx) -> Result<u64> {
        Ok(0)
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

    /// Backing-vector growth in bytes that [`Accumulator::init`] will allocate for
    /// `groups` new slots, used to pre-admit checkpoint loads before pushing groups.
    fn init_growth(&self, _groups: usize) -> u64 {
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

/// Whether `into` contains an out-of-bounds (`>= limit`) or repeated destination.
fn has_invalid_or_duplicate_dest(into: &[u32], limit: usize) -> bool {
    if into.len() <= 1 {
        return into.first().is_some_and(|&to| to as usize >= limit);
    }
    if limit <= 4_096 {
        let mut seen = [0u64; 64];
        for &to in into {
            let idx = to as usize;
            if idx >= limit {
                return true;
            }
            let bit = 1u64 << (idx & 63);
            let word = &mut seen[idx >> 6];
            if (*word & bit) != 0 {
                return true;
            }
            *word |= bit;
        }
        false
    } else {
        let mut seen = FxHashSet::with_capacity_and_hasher(into.len(), Default::default());
        into.iter()
            .any(|&to| to as usize >= limit || !seen.insert(to))
    }
}

/// The aggregates whose state is [`AggState`]: COUNT, SUM, AVG, MIN, MAX, SAMPLE and
/// ARQ's variance and deviation aggregates.
#[derive(Clone)]
pub(crate) struct Running {
    agg: Agg,
    /// Whether heap-owned state (decoded best values, `Box<DecimalSum>`) is tracked, for
    /// a charged grouping
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
        if self.track && self.is_best() {
            self.payload.resize(self.states.len(), 0);
        }
    }

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
        struct TrackAdmit<'a, 'b> {
            budget: &'a mut BatchBudget<'b>,
            track_payload: bool,
            n_states: usize,
            slot_idx: usize,
            payload: &'a mut Vec<u64>,
            growth: &'a mut u64,
        }
        impl ScratchAdmit for TrackAdmit<'_, '_> {
            fn charge_scratch<'c>(&mut self, ctx: &'c Ctx, bytes: u64) -> Result<Charge<'c>> {
                self.budget.charge_scratch(ctx, bytes)
            }
        }
        impl SumAdmit for TrackAdmit<'_, '_> {
            fn admit(&mut self, _old: u64, new_bytes: u64) -> Result<bool> {
                if self.track_payload && new_bytes > 0 {
                    if self.payload.len() <= self.slot_idx {
                        self.payload.resize(self.n_states, 0);
                    }
                    let held = &mut self.payload[self.slot_idx];
                    if new_bytes > *held {
                        let delta = new_bytes - *held;
                        self.budget.retain(delta)?;
                        *self.growth += delta;
                        *held = new_bytes;
                    }
                }
                Ok(true)
            }
        }

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
                    let track_payload = self.track && (self.is_best() || self.is_sum());
                    let states = &mut self.states;
                    let n_states = states.len();
                    let payload = &mut self.payload;
                    let growth = &mut self.growth;
                    for (row, &slot) in slots.iter().enumerate() {
                        if row.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        let w = batch.add_weight(row);
                        if w == 0 {
                            continue;
                        }
                        let id = batch.value(row);
                        let slot_idx = slot as usize;
                        let mut admit = TrackAdmit {
                            budget,
                            track_payload,
                            n_states,
                            slot_idx,
                            payload,
                            growth,
                        };
                        states[slot_idx].add_weighted(ctx, agg, id, w, &mut admit)?;
                    }
                }
            }
            return Ok(());
        }
        if self.is_sum()
            && let Some(sum_budget) = budget.sum_budget()
        {
            for (row, &slot) in slots.iter().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let id = batch.value(row);
                let state = &mut self.states[slot as usize];
                if state.sum_deferred() || !state.add_admitted(ctx, agg, id, sum_budget)? {
                    sum_budget.defer(ctx, slot, id)?;
                    state.defer_sum();
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
                let track_payload = self.track && (self.is_best() || self.is_sum());
                let states = &mut self.states;
                let n_states = states.len();
                let payload = &mut self.payload;
                let growth = &mut self.growth;
                for (row, &slot) in slots.iter().enumerate() {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let id = batch.value(row);
                    let slot_idx = slot as usize;
                    let mut admit = TrackAdmit {
                        budget,
                        track_payload,
                        n_states,
                        slot_idx,
                        payload,
                        growth,
                    };
                    states[slot_idx].add_admitted(ctx, agg, id, &mut admit)?;
                }
                ctx.check()?;
            }
        }
        Ok(())
    }

    /// `COUNT`, `SUM`, `AVG`, `MIN`, `MAX` and `SAMPLE` support merging; `SUM`, `AVG`,
    /// `MIN` and `MAX` verify exactness against single-pass semantics per group and
    /// refuse cleanly when rounding, overflow or non-transitive ordering could differ.
    fn mergeable(&self) -> bool {
        crate::sparql::exec::stat_aggregate(&self.agg.func).is_none()
    }

    fn merge(
        &mut self,
        ctx: &Ctx,
        other: &mut dyn Accumulator,
        into: &[u32],
        retain: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let other = other
            .as_any()
            .downcast_mut::<Running>()
            .expect("an accumulator of the same kind");
        if other.states.len() != into.len()
            || has_invalid_or_duplicate_dest(into, self.states.len())
            || other
                .states
                .iter()
                .zip(into)
                .any(|(state, &to)| !self.states[to as usize].can_merge(state))
        {
            return Err(Error::unsupported("this aggregate cannot be merged"));
        }
        if self.is_best() {
            for (from, &to) in other.payload.iter().zip(into) {
                let held = &mut self.payload[to as usize];
                if *from > *held {
                    let delta = *from - *held;
                    if self.track {
                        retain(delta)?;
                    }
                    self.growth += delta;
                    *held = *from;
                }
            }
            other.payload.clear();
        }
        if self.track && self.is_sum() {
            let mut need_total = 0u64;
            for (state, &to) in other.states.iter().zip(into) {
                let held_now = self.states[to as usize].merged_sum_heap_bytes(state);
                let held = self.payload.get(to as usize).copied().unwrap_or(0);
                if held_now > held {
                    need_total = need_total.saturating_add(held_now - held);
                }
            }
            if need_total > 0 {
                retain(need_total)?;
                if self.payload.len() < self.states.len() {
                    self.payload.resize(self.states.len(), 0);
                }
                for (state, &to) in other.states.iter().zip(into) {
                    let held_now = self.states[to as usize].merged_sum_heap_bytes(state);
                    let held = &mut self.payload[to as usize];
                    if held_now > *held {
                        *held = held_now;
                    }
                }
                self.growth = self.growth.saturating_add(need_total);
            }
        }
        for (state, &to) in std::mem::take(&mut other.states).into_iter().zip(into) {
            let merged = self.states[to as usize].merge(ctx, &self.agg, state);
            debug_assert!(merged, "validated by can_merge");
            if self.track && self.is_sum() {
                let held_now = self.states[to as usize].sum_heap_bytes();
                let held = self.payload.get(to as usize).copied().unwrap_or(0);
                debug_assert!(
                    held_now <= held,
                    "pre-admitted sum merge credit ({held}) must cover merged heap ({held_now})"
                );
            }
        }
        Ok(())
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let state = std::mem::replace(&mut self.states[group], AggState::Count(0));
        let id = state.finish(ctx, &self.agg)?;
        ctx.check()?;
        Ok(id)
    }

    fn finish_with(
        &mut self,
        ctx: &Ctx,
        group: usize,
        scratch: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        let state = std::mem::replace(&mut self.states[group], AggState::Count(0));
        let id = state.finish_with(ctx, &self.agg, scratch)?;
        ctx.check()?;
        Ok(id)
    }

    fn finish_replay(
        &mut self,
        ctx: &Ctx,
        group: usize,
        aggregate: usize,
        deferred: &mut Deferred,
        scratch: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        let state = &mut self.states[group];
        let _subtotal = if state.sum_deferred() {
            let charge = ctx.charge(crate::sparql::bignum::AGG_SUM_PAYLOAD_BYTES)?;
            deferred.replay(ctx, subtotal(group, aggregate), |id| {
                state.add(ctx, &self.agg, Some(id), &mut |_| Ok(()))
            })?;
            Some(charge)
        } else {
            None
        };
        self.finish_with(ctx, group, scratch)
    }

    fn read(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        self.states[group].read(ctx, &self.agg)
    }

    fn finish_intern_bytes(&self, ctx: &Ctx) -> Result<u64> {
        if matches!(
            self.agg.func,
            AggregateFunction::Count
                | AggregateFunction::Min
                | AggregateFunction::Max
                | AggregateFunction::Sample
        ) {
            return Ok(0);
        }
        let mut total = 0u64;
        for (i, state) in self.states.iter().enumerate() {
            if i.is_multiple_of(1024) {
                ctx.check()?;
            }
            total = total.saturating_add(state.finish_intern_bytes(&self.agg));
        }
        Ok(total)
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

    fn init_growth(&self, groups: usize) -> u64 {
        let payload = if self.is_best() {
            vec_growth(&self.payload, groups)
        } else {
            0
        };
        vec_growth(&self.states, groups).saturating_add(payload)
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
}

#[derive(Debug, Default)]
struct SumState {
    rows: i64,
    /// Arguments that were errors or not numbers, which make the sum unbound
    errs: i64,
    /// Arguments that were errors (`Id::UNDEF`), apart from non-numeric values, for
    /// the reading in which AVG over only errors is zero
    undef: i64,
    /// The integer sum, in wrapping arithmetic so that removals stay exact
    int: i128,
    /// Arbitrary-precision integers and decimals as counts, allocated only when seen
    big_ints: Option<Box<HbMap<crate::sparql::bignum::BigInt, i64>>>,
    big_decs: Option<Box<HbMap<crate::sparql::bignum::BigDec, i64>>>,
    /// Running sum of `heap_bytes()` across retained keys in `big_ints` and `big_decs`
    /// so `map_bytes()` stays O(1) without scanning big-number maps.
    big_key_bytes: u64,
    /// The decimal values as counts by their 128-bit encoding
    decs: HbMap<i128, i64>,
    floats: HbMap<u32, i64>,
    doubles: HbMap<u64, i64>,
}

impl Clone for SumState {
    fn clone(&self) -> Self {
        let big_ints = self.big_ints.clone();
        let big_decs = self.big_decs.clone();
        let big_key_bytes = big_ints
            .as_ref()
            .map_or(0, |m| m.keys().map(|k| k.heap_bytes()).sum::<u64>())
            + big_decs
                .as_ref()
                .map_or(0, |m| m.keys().map(|k| k.heap_bytes()).sum::<u64>());
        Self {
            rows: self.rows,
            errs: self.errs,
            undef: self.undef,
            int: self.int,
            big_ints,
            big_decs,
            big_key_bytes,
            decs: self.decs.clone(),
            floats: self.floats.clone(),
            doubles: self.doubles.clone(),
        }
    }
}

impl SumState {
    /// Add `w` copies of a value (`None`: an error or non-numeric value), pre-admitting
    /// any new map allocation, map resize, or cloned big-number payload via `admit`
    /// before allocating.
    fn add_admitted(
        &mut self,
        v: Option<&Value>,
        w: i64,
        headroom: &mut u64,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        match v {
            Some(Value::Integer(i)) => {
                self.int = self
                    .int
                    .wrapping_add((i64::from(*i) as i128).wrapping_mul(w as i128));
            }
            Some(Value::BigInteger(i)) => {
                if let Some(map) = self.big_ints.as_mut()
                    && let Some(n) = map.get_mut(i.as_ref())
                {
                    *n += w;
                    if *n == 0 {
                        if let Some((removed, _)) = map.remove_entry(i.as_ref()) {
                            self.big_key_bytes =
                                self.big_key_bytes.saturating_sub(removed.heap_bytes());
                        }
                        if map.is_empty() {
                            self.big_ints = None;
                        } else {
                            shrink_map(map);
                        }
                    }
                } else {
                    let est_key_bytes = i.heap_bytes();
                    let delta = match self.big_ints.as_ref() {
                        Some(map) => hbmap_insert_growth(map, est_key_bytes),
                        None => boxed_hbmap_init_growth::<crate::sparql::bignum::BigInt, i64>(
                            est_key_bytes,
                        ),
                    };
                    admit_with_headroom(delta, headroom, admit)?;
                    let before = self.big_ints.as_ref().map_or(0, |m| {
                        size_of::<HbMap<crate::sparql::bignum::BigInt, i64>>() as u64
                            + m.allocation_size() as u64
                    });
                    let key = (**i).clone();
                    let key_bytes = key.heap_bytes();
                    let map = self.big_ints.get_or_insert_with(Default::default);
                    map.insert(key, w);
                    self.big_key_bytes = self.big_key_bytes.saturating_add(key_bytes);
                    let after = size_of::<HbMap<crate::sparql::bignum::BigInt, i64>>() as u64
                        + map.allocation_size() as u64;
                    let actual = after.saturating_sub(before).saturating_add(key_bytes);
                    *headroom = headroom.saturating_sub(actual);
                }
            }
            Some(Value::Decimal(d)) => {
                let k = i128::from_be_bytes(d.to_be_bytes());
                if let Some(n) = self.decs.get_mut(&k) {
                    *n += w;
                    if *n == 0 {
                        self.decs.remove(&k);
                        shrink_map(&mut self.decs);
                    }
                } else {
                    let delta = hbmap_insert_growth(&self.decs, 0);
                    admit_with_headroom(delta, headroom, admit)?;
                    let before = self.decs.allocation_size() as u64;
                    self.decs.insert(k, w);
                    let actual = (self.decs.allocation_size() as u64).saturating_sub(before);
                    *headroom = headroom.saturating_sub(actual);
                }
            }
            Some(Value::BigDecimal(d)) => {
                if let Some(map) = self.big_decs.as_mut()
                    && let Some(n) = map.get_mut(d.as_ref())
                {
                    *n += w;
                    if *n == 0 {
                        if let Some((removed, _)) = map.remove_entry(d.as_ref()) {
                            self.big_key_bytes =
                                self.big_key_bytes.saturating_sub(removed.heap_bytes());
                        }
                        if map.is_empty() {
                            self.big_decs = None;
                        } else {
                            shrink_map(map);
                        }
                    }
                } else {
                    let est_key_bytes = d.heap_bytes();
                    let delta = match self.big_decs.as_ref() {
                        Some(map) => hbmap_insert_growth(map, est_key_bytes),
                        None => boxed_hbmap_init_growth::<crate::sparql::bignum::BigDec, i64>(
                            est_key_bytes,
                        ),
                    };
                    admit_with_headroom(delta, headroom, admit)?;
                    let before = self.big_decs.as_ref().map_or(0, |m| {
                        size_of::<HbMap<crate::sparql::bignum::BigDec, i64>>() as u64
                            + m.allocation_size() as u64
                    });
                    let key = (**d).clone();
                    let key_bytes = key.heap_bytes();
                    let map = self.big_decs.get_or_insert_with(Default::default);
                    map.insert(key, w);
                    self.big_key_bytes = self.big_key_bytes.saturating_add(key_bytes);
                    let after = size_of::<HbMap<crate::sparql::bignum::BigDec, i64>>() as u64
                        + map.allocation_size() as u64;
                    let actual = after.saturating_sub(before).saturating_add(key_bytes);
                    *headroom = headroom.saturating_sub(actual);
                }
            }
            Some(Value::Float(f)) => {
                let b = f32::from(*f).to_bits();
                if let Some(n) = self.floats.get_mut(&b) {
                    *n += w;
                    if *n == 0 {
                        self.floats.remove(&b);
                        shrink_map(&mut self.floats);
                    }
                } else {
                    let delta = hbmap_insert_growth(&self.floats, 0);
                    admit_with_headroom(delta, headroom, admit)?;
                    let before = self.floats.allocation_size() as u64;
                    self.floats.insert(b, w);
                    let actual = (self.floats.allocation_size() as u64).saturating_sub(before);
                    *headroom = headroom.saturating_sub(actual);
                }
            }
            Some(Value::Double(d)) => {
                let b = f64::from(*d).to_bits();
                if let Some(n) = self.doubles.get_mut(&b) {
                    *n += w;
                    if *n == 0 {
                        self.doubles.remove(&b);
                        shrink_map(&mut self.doubles);
                    }
                } else {
                    let delta = hbmap_insert_growth(&self.doubles, 0);
                    admit_with_headroom(delta, headroom, admit)?;
                    let before = self.doubles.allocation_size() as u64;
                    self.doubles.insert(b, w);
                    let actual = (self.doubles.allocation_size() as u64).saturating_sub(before);
                    *headroom = headroom.saturating_sub(actual);
                }
            }
            _ => self.errs += w,
        }
        self.rows += w;
        Ok(())
    }

    fn sum_or_avg(&self, avg: bool) -> Option<Value> {
        if self.errs != 0 {
            return None;
        }
        if avg && self.rows == 0 {
            return Some(Value::Integer(0.into()));
        }
        let has_big_int = self.big_ints.as_ref().is_some_and(|m| !m.is_empty());
        let has_big_dec = self.big_decs.as_ref().is_some_and(|m| !m.is_empty());
        let has_approx = !self.floats.is_empty() || !self.doubles.is_empty();

        // Accumulate exact subtotals with bounded headroom so cancellation and averaging at
        // the digit cap are evaluated on the final result rather than intermediate terms.
        enum ExactAcc {
            Int(crate::sparql::bignum::BigInt),
            Dec(crate::sparql::bignum::BigDec),
            Fixed(Value),
        }

        let exact = if has_big_dec || (has_big_int && !self.decs.is_empty()) {
            use crate::sparql::bignum::{AGG_MAX_DEC_DIGITS, BigDec, BigInt};
            let mut total = BigDec::from_bigint(BigInt::from_i128(self.int));
            if let Some(map) = &self.big_ints {
                for (bi, &n) in map.iter() {
                    let term = BigDec::from_bigint(bi.clone())
                        .checked_mul_i64_with_limit(n, AGG_MAX_DEC_DIGITS)?;
                    total = total.checked_add_with_limit(&term, AGG_MAX_DEC_DIGITS)?;
                }
            }
            for (&k, &n) in &self.decs {
                let d = BigDec::from_oxs_decimal(Decimal::from_be_bytes(k.to_be_bytes()));
                let term = d.checked_mul_i64_with_limit(n, AGG_MAX_DEC_DIGITS)?;
                total = total.checked_add_with_limit(&term, AGG_MAX_DEC_DIGITS)?;
            }
            if let Some(map) = &self.big_decs {
                for (bd, &n) in map.iter() {
                    let term = bd.checked_mul_i64_with_limit(n, AGG_MAX_DEC_DIGITS)?;
                    total = total.checked_add_with_limit(&term, AGG_MAX_DEC_DIGITS)?;
                }
            }
            ExactAcc::Dec(total)
        } else if !self.decs.is_empty() {
            // Summed exactly in 256 bits so values that cancel out do not overflow on
            // the way; the integers join the decimals as a one-off evaluation promotes
            // them.
            let mut acc = Wide::default();
            for (k, n) in &self.decs {
                acc.add_product(*k, *n);
            }
            acc.add_product(self.int, 1_000_000_000_000_000_000);
            if let Some(raw) = acc.to_i128() {
                ExactAcc::Fixed(Value::Decimal(Decimal::from_be_bytes(raw.to_be_bytes())))
            } else {
                use crate::sparql::bignum::{AGG_MAX_DEC_DIGITS, BigDec, BigInt};
                let mut total = BigDec::from_bigint(BigInt::from_i128(self.int));
                for (&k, &n) in &self.decs {
                    let d = BigDec::from_oxs_decimal(Decimal::from_be_bytes(k.to_be_bytes()));
                    let term = d.checked_mul_i64_with_limit(n, AGG_MAX_DEC_DIGITS)?;
                    total = total.checked_add_with_limit(&term, AGG_MAX_DEC_DIGITS)?;
                }
                ExactAcc::Dec(total)
            }
        } else if has_big_int {
            use crate::sparql::bignum::{AGG_MAX_INT_DIGITS, BigInt};
            let mut total = BigInt::from_i128(self.int);
            if let Some(map) = &self.big_ints {
                for (bi, &n) in map.iter() {
                    let term = bi.checked_mul_i64_with_limit(n, AGG_MAX_INT_DIGITS)?;
                    total = total.checked_add_with_limit(&term, AGG_MAX_INT_DIGITS)?;
                }
            }
            ExactAcc::Int(total)
        } else {
            ExactAcc::Fixed(exact_sum(self.int))
        };

        if !has_approx {
            return match exact {
                ExactAcc::Fixed(v) => {
                    if avg {
                        arith(NumOp::Div, &v, &Value::Integer(self.rows.into())).ok()
                    } else {
                        Some(v)
                    }
                }
                ExactAcc::Int(bi) => {
                    if avg {
                        crate::sparql::value::avg_big_int(bi, self.rows.try_into().ok()?)
                    } else {
                        bi.fits_limits().then(|| Value::from_bigint(bi))
                    }
                }
                ExactAcc::Dec(bd) => {
                    if avg {
                        crate::sparql::value::avg_big_decimal(&bd, self.rows.try_into().ok()?)
                    } else {
                        bd.fits_limits().then(|| Value::from_bigdec(bd))
                    }
                }
            };
        }

        // Fold floats at float precision before the first double promotes the subtotal.
        let mut acc = match exact {
            ExactAcc::Fixed(v) => v,
            ExactAcc::Dec(bd) => match bd.to_oxs_decimal() {
                Some(decimal) => Value::Decimal(decimal),
                None if self.floats.is_empty() => Value::Double(Double::from(bd.to_f64())),
                None => Value::Float(Float::from(bd.to_f32())),
            },
            ExactAcc::Int(bi) if self.floats.is_empty() => Value::Double(Double::from(bi.to_f64())),
            ExactAcc::Int(bi) => Value::Float(Float::from(bi.to_f32())),
        };
        let mut floats: Vec<(f32, i64)> = self
            .floats
            .iter()
            .map(|(b, n)| (f32::from_bits(*b), *n))
            .collect();
        floats.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (f, n) in floats {
            acc = arith(NumOp::Add, &acc, &Value::Float(Float::from(f * n as f32))).ok()?;
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
        if avg {
            arith(NumOp::Div, &acc, &Value::Integer(self.rows.into())).ok()
        } else {
            Some(acc)
        }
    }

    fn map_bytes(&self) -> u64 {
        let big = self.big_ints.as_ref().map_or(0, |m| {
            size_of::<HbMap<crate::sparql::bignum::BigInt, i64>>() as u64
                + m.allocation_size() as u64
        }) + self.big_decs.as_ref().map_or(0, |m| {
            size_of::<HbMap<crate::sparql::bignum::BigDec, i64>>() as u64
                + m.allocation_size() as u64
        }) + self.big_key_bytes;
        (self.floats.allocation_size()
            + self.doubles.allocation_size()
            + self.decs.allocation_size()) as u64
            + big
    }

    #[cfg(test)]
    fn exact_map_bytes(&self) -> u64 {
        let big = self.big_ints.as_ref().map_or(0, |m| {
            size_of::<HbMap<crate::sparql::bignum::BigInt, i64>>() as u64
                + m.allocation_size() as u64
                + m.keys().map(|k| k.heap_bytes()).sum::<u64>()
        }) + self.big_decs.as_ref().map_or(0, |m| {
            size_of::<HbMap<crate::sparql::bignum::BigDec, i64>>() as u64
                + m.allocation_size() as u64
                + m.keys().map(|k| k.heap_bytes()).sum::<u64>()
        });
        (self.floats.allocation_size()
            + self.doubles.allocation_size()
            + self.decs.allocation_size()) as u64
            + big
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
        let big_ints_len = self.big_ints.as_ref().map_or(0, |m| m.len()) as u64;
        w.u64(big_ints_len);
        if let Some(m) = &self.big_ints {
            for (bi, n) in m.iter() {
                w.bytes(bi.to_string().as_bytes());
                w.i64(*n);
            }
        }
        let big_decs_len = self.big_decs.as_ref().map_or(0, |m| m.len()) as u64;
        w.u64(big_decs_len);
        if let Some(m) = &self.big_decs {
            for (bd, n) in m.iter() {
                w.bytes(bd.to_string().as_bytes());
                w.i64(*n);
            }
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
            let c = r.i64()?;
            hbmap_load_insert(r, &mut s.decs, k, c, 0)?;
        }
        for _ in 0..r.u64()? {
            let b = r.u64()? as u32;
            let c = r.i64()?;
            hbmap_load_insert(r, &mut s.floats, b, c, 0)?;
        }
        for _ in 0..r.u64()? {
            let b = r.u64()?;
            let c = r.i64()?;
            hbmap_load_insert(r, &mut s.doubles, b, c, 0)?;
        }
        let n_bi = r.u64()?;
        if n_bi > 0 {
            r.charge_state(size_of::<HbMap<crate::sparql::bignum::BigInt, i64>>() as u64)?;
            let mut map = HbMap::default();
            for _ in 0..n_bi {
                let text = std::str::from_utf8(r.bytes()?)
                    .map_err(|_| Error::Corrupt("view state holds non-UTF-8 big integer".into()))?;
                let bi = crate::sparql::bignum::BigInt::parse(text).ok_or_else(|| {
                    Error::Corrupt("view state holds malformed big integer".into())
                })?;
                let c = r.i64()?;
                let key_bytes = bi.heap_bytes();
                let is_new = !map.contains_key(&bi);
                hbmap_load_insert(r, &mut map, bi, c, key_bytes)?;
                if is_new {
                    s.big_key_bytes = s.big_key_bytes.saturating_add(key_bytes);
                }
            }
            s.big_ints = Some(Box::new(map));
        }
        let n_bd = r.u64()?;
        if n_bd > 0 {
            r.charge_state(size_of::<HbMap<crate::sparql::bignum::BigDec, i64>>() as u64)?;
            let mut map = HbMap::default();
            for _ in 0..n_bd {
                let text = std::str::from_utf8(r.bytes()?)
                    .map_err(|_| Error::Corrupt("view state holds non-UTF-8 big decimal".into()))?;
                let bd = crate::sparql::bignum::BigDec::parse(text).ok_or_else(|| {
                    Error::Corrupt("view state holds malformed big decimal".into())
                })?;
                let c = r.i64()?;
                let key_bytes = bd.heap_bytes();
                let is_new = !map.contains_key(&bd);
                hbmap_load_insert(r, &mut map, bd, c, key_bytes)?;
                if is_new {
                    s.big_key_bytes = s.big_key_bytes.saturating_add(key_bytes);
                }
            }
            s.big_decs = Some(Box::new(map));
        }
        Ok(s)
    }
}

/// Retractable `SUM` and `AVG` accumulator with exact integer and decimal tracking and
/// refcounted float, double and error states.
pub(crate) struct RetractableSum {
    is_avg: bool,
    groups: Vec<SumState>,
    maps_bytes: u64,
}

impl Clone for RetractableSum {
    fn clone(&self) -> Self {
        let groups = self.groups.clone();
        let maps_bytes = groups.iter().map(SumState::map_bytes).sum();
        Self {
            is_avg: self.is_avg,
            groups,
            maps_bytes,
        }
    }
}

impl RetractableSum {
    pub(crate) fn new(agg: &Agg) -> Self {
        Self {
            is_avg: matches!(agg.func, AggregateFunction::Avg),
            groups: Vec::new(),
            maps_bytes: 0,
        }
    }

    fn fold(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        retract: bool,
        mut budget: Option<&mut BatchBudget<'_>>,
    ) -> Result<()> {
        let decode = ctx.charge(0)?;
        let mut headroom = 0u64;
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
                s.add_admitted(None, w, &mut headroom, &mut |_| Ok(()))?;
                s.undef += w;
            } else if id.tag() == Tag::Int {
                s.rows += w;
                s.int = s
                    .int
                    .wrapping_add((id.as_i64() as i128).wrapping_mul(w as i128));
            } else {
                let v = ctx.value_checked_into(id, &decode)?;
                let mut admit = |delta: u64| match budget.as_deref_mut() {
                    Some(b) => b.retain(delta),
                    None => Ok(()),
                };
                let before_map = s.map_bytes();
                let res = s.add_admitted(v.as_ref(), w, &mut headroom, &mut admit);
                let after_map = s.map_bytes();
                self.maps_bytes = self
                    .maps_bytes
                    .saturating_sub(before_map)
                    .saturating_add(after_map);
                drop(v);
                decode.resize(0)?;
                res?;
            }
        }
        drop(decode);
        Ok(())
    }
}

impl Accumulator for RetractableSum {
    fn init(&mut self, groups: usize) {
        self.groups
            .resize_with(self.groups.len() + groups, SumState::default);
    }

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
        self.fold(ctx, batch, slots, false, Some(budget))
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let id = self.read(ctx, group)?;
        let old = self.groups[group].map_bytes();
        self.maps_bytes = self.maps_bytes.saturating_sub(old);
        self.groups[group] = SumState::default();
        Ok(id)
    }

    fn read(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let s = &self.groups[group];
        finish_sum_avg(
            ctx,
            self.is_avg,
            s.rows.max(0) as u64,
            s.undef.max(0) as u64,
            s.sum_or_avg(self.is_avg),
        )
    }

    fn retractable(&self) -> bool {
        true
    }

    fn retract(&mut self, ctx: &Ctx, batch: &Batch<'_>, slots: &[u32]) -> Result<bool> {
        if !batch.retract_weights_fit_i64(slots.len()) {
            return Ok(false);
        }
        self.fold(ctx, batch, slots, true, None)?;
        Ok(true)
    }

    fn clear(&mut self) {
        self.groups = Vec::new();
        self.maps_bytes = 0;
    }

    fn swap_remove(&mut self, group: usize) {
        let removed = self.groups.swap_remove(group);
        self.maps_bytes = self.maps_bytes.saturating_sub(removed.map_bytes());
        if self.groups.is_empty() {
            self.groups = Vec::new();
            self.maps_bytes = 0;
        }
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.groups);
    }

    fn retained_bytes(&self) -> u64 {
        self.groups.capacity() as u64 * size_of::<SumState>() as u64 + self.maps_bytes
    }

    fn init_growth(&self, groups: usize) -> u64 {
        vec_growth(&self.groups, groups)
    }

    fn save_group(&self, group: usize, w: &mut Writer) -> Result<()> {
        self.groups[group].save(w);
        Ok(())
    }

    fn load_group(&mut self, group: usize, r: &mut Reader) -> Result<()> {
        let old = self.groups[group].map_bytes();
        let loaded = SumState::load(r)?;
        let new = loaded.map_bytes();
        self.maps_bytes = self.maps_bytes.saturating_sub(old).saturating_add(new);
        self.groups[group] = loaded;
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

    fn add_admitted(
        &mut self,
        ctx: &Ctx,
        v: Id,
        w: i64,
        decode: &Charge<'_>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let value = ctx.value_checked_into(v, decode)?;
        let pos = self.position(ctx, v, value.as_ref());
        drop(value);
        decode.resize(0)?;
        match pos {
            Ok(i) => {
                self.vals[i].1 += w;
                if self.vals[i].1 == 0 {
                    self.vals.remove(i);
                    shrink_vec(&mut self.vals);
                }
            }
            Err(i) => {
                let delta = vec_insert_growth(&self.vals);
                if delta > 0 {
                    admit(delta)?;
                }
                self.vals.insert(i, (v, w));
            }
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
pub(crate) struct Ordered {
    kind: OrderedKind,
    groups: Vec<Multiset>,
    vals_bytes: u64,
}

impl Clone for Ordered {
    fn clone(&self) -> Self {
        let groups = self.groups.clone();
        let vals_bytes = groups
            .iter()
            .map(|m| m.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64)
            .sum();
        Self {
            kind: self.kind.clone(),
            groups,
            vals_bytes,
        }
    }
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
            vals_bytes: 0,
        }
    }

    fn fold(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        retract: bool,
        mut budget: Option<&mut BatchBudget<'_>>,
    ) -> Result<()> {
        let decode = ctx.charge(0)?;
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
                let mut admit = |delta: u64| match budget.as_deref_mut() {
                    Some(b) => b.retain(delta),
                    None => Ok(()),
                };
                let before = m.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64;
                let res = m.add_admitted(ctx, id, w, &decode, &mut admit);
                let after = m.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64;
                self.vals_bytes = self.vals_bytes.saturating_sub(before).saturating_add(after);
                res?;
            }
        }
        drop(decode);
        Ok(())
    }
}

impl Accumulator for Ordered {
    fn init(&mut self, groups: usize) {
        self.groups
            .resize_with(self.groups.len() + groups, Multiset::default);
    }

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
        self.fold(ctx, batch, slots, false, Some(budget))
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let id = self.read(ctx, group)?;
        let old = self.groups[group].vals.capacity() as u64 * size_of::<(Id, i64)>() as u64;
        self.vals_bytes = self.vals_bytes.saturating_sub(old);
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
                let mut key: Vec<u8> = Vec::new();
                let mut empty = true;
                let total_vals = m.vals.len();
                for (idx, (v, n)) in m.vals.iter().enumerate() {
                    if *n <= 0 {
                        continue;
                    }
                    let val = ctx.value_checked(*v)?;
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
                    let added_u64 = reps.checked_mul(s.len() as u64).and_then(|b| {
                        seps.checked_mul(separator.len() as u64)
                            .and_then(|sb| b.checked_add(sb))
                    });
                    if added_u64 == Some(0) {
                        empty = false;
                        continue;
                    }
                    let prefix_len = if key.is_empty() {
                        1u64
                    } else {
                        key.len() as u64
                    };
                    let total_u64 = added_u64
                        .and_then(|a| prefix_len.checked_add(a))
                        .and_then(|n| n.checked_add(1));
                    let Some(final_key_len) = total_u64
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|&n| n <= MAX_ALLOC_BYTES)
                    else {
                        return Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            limit: ctx.mem_limit,
                            requested: total_u64.unwrap_or(u64::MAX),
                        }));
                    };
                    let need = final_key_len as u64;
                    if need > charged {
                        charge.add(need - charged)?;
                        charged = need;
                    }
                    let has_more_vals = idx + 1 < total_vals;
                    let mut rem = reps;
                    while rem > 0 {
                        ctx.check()?;
                        let chunk = rem.min(1024);
                        let chunk_seps = if empty { chunk - 1 } else { chunk };
                        let chunk_bytes =
                            (chunk as usize) * s.len() + (chunk_seps as usize) * separator.len();
                        let next_len =
                            (if key.is_empty() { 1 } else { key.len() }) + chunk_bytes + 1;
                        if next_len > key.capacity() {
                            let doubled = key.capacity().saturating_mul(2).min(MAX_ALLOC_BYTES);
                            let cap_bound = if rem == chunk {
                                if has_more_vals {
                                    let amortized = final_key_len.max(doubled);
                                    if amortized > final_key_len {
                                        let extra = (amortized as u64).saturating_sub(charged);
                                        if extra == 0 || charge.add(extra).is_ok() {
                                            charged = charged.max(amortized as u64);
                                            amortized
                                        } else {
                                            final_key_len
                                        }
                                    } else {
                                        final_key_len
                                    }
                                } else {
                                    final_key_len
                                }
                            } else {
                                next_len.max(doubled).min(final_key_len)
                            };
                            if cap_bound > key.capacity() {
                                key.try_reserve_exact(cap_bound - key.len()).map_err(|_| {
                                    Error::BudgetExceeded(crate::error::Budget {
                                        kind: crate::error::BudgetKind::Memory,
                                        limit: ctx.mem_limit,
                                        requested: cap_bound as u64,
                                    })
                                })?;
                            }
                        }
                        if key.is_empty() {
                            key.push(b'"');
                        }
                        for _ in 0..chunk {
                            if !empty {
                                key.extend_from_slice(separator.as_bytes());
                            }
                            empty = false;
                            key.extend_from_slice(s.as_bytes());
                        }
                        rem -= chunk;
                    }
                }
                if concat_is_undef(ctx, *distinct, empty, true) {
                    return Ok(Id::UNDEF);
                }
                let id = if key.is_empty() {
                    ctx.intern_key(&[b'"', crate::id::KEY_SEP])
                } else {
                    key.push(crate::id::KEY_SEP);
                    ctx.intern_key(&key)
                };
                drop(key);
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
        self.fold(ctx, batch, slots, true, None)?;
        Ok(true)
    }

    fn clear(&mut self) {
        self.groups = Vec::new();
        self.vals_bytes = 0;
    }

    fn swap_remove(&mut self, group: usize) {
        let removed = self.groups.swap_remove(group);
        let old = removed.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64;
        self.vals_bytes = self.vals_bytes.saturating_sub(old);
        if self.groups.is_empty() {
            self.groups = Vec::new();
            self.vals_bytes = 0;
        }
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.groups);
    }

    fn retained_bytes(&self) -> u64 {
        self.groups.capacity() as u64 * size_of::<Multiset>() as u64 + self.vals_bytes
    }

    fn init_growth(&self, groups: usize) -> u64 {
        vec_growth(&self.groups, groups)
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
        let old = self.groups[group].vals.capacity() as u64 * size_of::<(Id, i64)>() as u64;
        let mut m = Multiset::default();
        // The entries were written in their order, which the terms keep.
        for _ in 0..r.u64()? {
            let v = r.id()?;
            let c = r.i64()?;
            r.charge_state(vec_insert_growth(&m.vals))?;
            m.vals.push((v, c));
        }
        m.errs = r.i64()?;
        m.sample = match r.u64()? {
            0 => None,
            _ => Some(r.id()?),
        };
        let new = m.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64;
        self.vals_bytes = self.vals_bytes.saturating_sub(old).saturating_add(new);
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
    Values(HbMap<Id, i64>),
    Rows(HbMap<Box<[Id]>, i64>),
}

#[inline]
fn distinct_group_bytes(group: &DistinctGroup, row_width: usize) -> u64 {
    match group {
        DistinctGroup::Rows(m) => {
            m.len() as u64 * (row_width as u64 * 8) + m.allocation_size() as u64
        }
        DistinctGroup::Values(m) => m.allocation_size() as u64,
    }
}

/// Retractable `DISTINCT` wrapper over any retractable accumulator. Each group counts
/// occurrences of each distinct value (or full row for `COUNT(DISTINCT *)`), forwarding
/// transitions across zero to `inner` and passing errors (`Id::UNDEF`) through
/// un-deduplicated with their full weight.
pub(crate) struct RetractableDistinct {
    inner: Box<dyn Accumulator>,
    groups: Vec<DistinctGroup>,
    groups_own_bytes: u64,
    star: bool,
    row_width: usize,
}

impl RetractableDistinct {
    pub(crate) fn new(inner: Box<dyn Accumulator>, star: bool, row_width: usize) -> Self {
        Self {
            inner,
            groups: Vec::new(),
            groups_own_bytes: 0,
            star,
            row_width,
        }
    }

    fn empty_group(&self) -> DistinctGroup {
        if self.star {
            DistinctGroup::Rows(HbMap::default())
        } else {
            DistinctGroup::Values(HbMap::default())
        }
    }
}

impl Accumulator for RetractableDistinct {
    fn init(&mut self, groups: usize) {
        let star = self.star;
        self.groups.resize_with(self.groups.len() + groups, || {
            if star {
                DistinctGroup::Rows(HbMap::default())
            } else {
                DistinctGroup::Values(HbMap::default())
            }
        });
        self.inner.init(groups);
    }

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
        let mut keep_slots: Vec<u32> = Vec::new();
        let mut keep_vals: Vec<Id> = Vec::new();
        let mut keep_weights: Vec<i64> = Vec::new();
        let mut row_buf: Vec<Id> = Vec::new();
        let mut headroom = 0u64;
        for (row, &slot) in slots.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let w = batch.add_weight(row) as i64;
            if w == 0 {
                continue;
            }
            let before_own = distinct_group_bytes(&self.groups[slot as usize], self.row_width);
            let mut admit = |delta: u64| budget.retain(delta);
            let step_res: Result<()> = match &mut self.groups[slot as usize] {
                DistinctGroup::Rows(m) => {
                    row_buf.clear();
                    row_buf.extend(batch.table.cols.iter().map(|c| c[batch.base + row]));
                    let res = if let Some(e) = m.get_mut(row_buf.as_slice()) {
                        let before = *e;
                        *e += w;
                        let after = *e;
                        if after == 0 {
                            m.remove(row_buf.as_slice());
                            shrink_map(m);
                        }
                        Ok((before, after))
                    } else {
                        let key_extra = (row_buf.len() * size_of::<Id>()) as u64;
                        let delta = hbmap_insert_growth(m, key_extra);
                        admit_with_headroom(delta, &mut headroom, &mut admit).map(|()| {
                            let before_alloc = m.allocation_size() as u64;
                            let key: Box<[Id]> = row_buf.as_slice().into();
                            m.insert(key, w);
                            let actual = (m.allocation_size() as u64)
                                .saturating_sub(before_alloc)
                                .saturating_add(key_extra);
                            headroom = headroom.saturating_sub(actual);
                            (0, w)
                        })
                    };
                    res.map(|(before, after)| {
                        if before <= 0 && after > 0 {
                            keep_slots.push(slot);
                            keep_weights.push(1);
                        }
                    })
                }
                DistinctGroup::Values(m) => {
                    let id = batch.value(row).unwrap_or(Id::UNDEF);
                    if id.is_undef() {
                        // An error is never a duplicate.
                        keep_slots.push(slot);
                        keep_vals.push(Id::UNDEF);
                        keep_weights.push(w);
                        Ok(())
                    } else {
                        let res = if let Some(e) = m.get_mut(&id) {
                            let before = *e;
                            *e += w;
                            let after = *e;
                            if after == 0 {
                                m.remove(&id);
                                shrink_map(m);
                            }
                            Ok((before, after))
                        } else {
                            let delta = hbmap_insert_growth(m, 0);
                            admit_with_headroom(delta, &mut headroom, &mut admit).map(|()| {
                                let before_alloc = m.allocation_size() as u64;
                                m.insert(id, w);
                                let actual =
                                    (m.allocation_size() as u64).saturating_sub(before_alloc);
                                headroom = headroom.saturating_sub(actual);
                                (0, w)
                            })
                        };
                        res.map(|(before, after)| {
                            if before <= 0 && after > 0 {
                                keep_slots.push(slot);
                                keep_vals.push(id);
                                keep_weights.push(1);
                            }
                        })
                    }
                }
            };
            let after_own = distinct_group_bytes(&self.groups[slot as usize], self.row_width);
            self.groups_own_bytes = self
                .groups_own_bytes
                .saturating_sub(before_own)
                .saturating_add(after_own);
            step_res?;
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
        self.inner.add_batch(ctx, &inner, &keep_slots, budget)
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        let id = self.inner.finish(ctx, group)?;
        let old = distinct_group_bytes(&self.groups[group], self.row_width);
        self.groups_own_bytes = self.groups_own_bytes.saturating_sub(old);
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
            let before_own = distinct_group_bytes(&self.groups[slot as usize], self.row_width);
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
            let after_own = distinct_group_bytes(&self.groups[slot as usize], self.row_width);
            self.groups_own_bytes = self
                .groups_own_bytes
                .saturating_sub(before_own)
                .saturating_add(after_own);
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
        self.groups_own_bytes = 0;
        self.inner.clear();
    }

    fn swap_remove(&mut self, group: usize) {
        let removed = self.groups.swap_remove(group);
        let old = distinct_group_bytes(&removed, self.row_width);
        self.groups_own_bytes = self.groups_own_bytes.saturating_sub(old);
        if self.groups.is_empty() {
            self.groups = Vec::new();
            self.groups_own_bytes = 0;
        }
        self.inner.swap_remove(group);
    }

    fn shrink_unused(&mut self) {
        shrink_vec(&mut self.groups);
        self.inner.shrink_unused();
    }

    fn retained_bytes(&self) -> u64 {
        let outer = self.groups.capacity() as u64 * size_of::<DistinctGroup>() as u64;
        outer + self.groups_own_bytes + self.inner.retained_bytes()
    }

    fn init_growth(&self, groups: usize) -> u64 {
        vec_growth(&self.groups, groups).saturating_add(self.inner.init_growth(groups))
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
        let old = distinct_group_bytes(&self.groups[group], self.row_width);
        let row_width = self.row_width;
        let load_res: Result<()> = match &mut self.groups[group] {
            DistinctGroup::Rows(m) => {
                m.clear();
                let row_extra = row_width as u64 * 8;
                (0..r.u64()?).try_for_each(|_| {
                    let row = r.row()?;
                    let c = r.i64()?;
                    hbmap_load_insert(r, m, row, c, row_extra)
                })
            }
            DistinctGroup::Values(m) => {
                m.clear();
                (0..r.u64()?).try_for_each(|_| {
                    let v = r.id()?;
                    let c = r.i64()?;
                    hbmap_load_insert(r, m, v, c, 0)
                })
            }
        };
        let new = distinct_group_bytes(&self.groups[group], self.row_width);
        self.groups_own_bytes = self
            .groups_own_bytes
            .saturating_sub(old)
            .saturating_add(new);
        load_res?;
        self.inner.load_group(group, r)
    }

    fn clone_box(&self) -> Option<Box<dyn Accumulator>> {
        let inner = self.inner.clone_box()?;
        let groups = self.groups.clone();
        let groups_own_bytes = groups
            .iter()
            .map(|g| distinct_group_bytes(g, self.row_width))
            .sum();
        Some(Box::new(Self {
            inner,
            groups,
            groups_own_bytes,
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

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
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
            let decode = ctx.charge(0)?;
            let Some(value) = ctx.value_checked_into(id, &decode)? else {
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
            let add_len =
                lexical
                    .len()
                    .saturating_add(if state.empty { 0 } else { self.separator.len() });
            if state.text.len().saturating_add(add_len) > state.text.capacity() {
                let before = state.text.capacity();
                let needed = state.text.len().saturating_add(add_len);
                let next_cap = needed.max(before.saturating_mul(2));
                // the string and its reallocation coexist while it grows
                let delta = (next_cap.saturating_sub(before) as u64).saturating_mul(2);
                if budget.retain(delta).is_err() {
                    let exact = (needed.saturating_sub(before) as u64).saturating_mul(2);
                    budget.retain(exact)?;
                    state.text.reserve_exact(add_len);
                    let actual =
                        (state.text.capacity().saturating_sub(before) as u64).saturating_mul(2);
                    if actual > exact {
                        budget.retain(actual - exact)?;
                    }
                    self.growth += actual.max(exact);
                } else {
                    state.text.reserve(next_cap - state.text.len());
                    let actual =
                        (state.text.capacity().saturating_sub(before) as u64).saturating_mul(2);
                    if actual > delta {
                        budget.retain(actual - delta)?;
                    }
                    self.growth += actual.max(delta);
                }
            }
            if !state.empty {
                state.text.push_str(&self.separator);
            }
            state.text.push_str(&lexical);
            state.empty = false;
            drop(lexical);
            drop(value);
            drop(decode);
        }
        ctx.check()?;
        Ok(())
    }

    fn mergeable(&self) -> bool {
        true
    }

    fn merge(
        &mut self,
        ctx: &Ctx,
        other: &mut dyn Accumulator,
        into: &[u32],
        retain: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let other = other
            .as_any()
            .downcast_mut::<Concat>()
            .expect("an accumulator of the same kind");
        if other.groups.len() != into.len()
            || has_invalid_or_duplicate_dest(into, self.groups.len())
        {
            return Err(Error::unsupported("this aggregate cannot be merged"));
        }
        for (i, (from, &to)) in std::mem::take(&mut other.groups)
            .into_iter()
            .zip(into)
            .enumerate()
        {
            if i.is_multiple_of(1024) {
                ctx.check()?;
            }
            let state = &mut self.groups[to as usize];
            state.ok &= from.ok;
            if !state.ok || from.empty {
                continue;
            }
            let add_len =
                from.text
                    .len()
                    .saturating_add(if state.empty { 0 } else { self.separator.len() });
            if state.text.len().saturating_add(add_len) > state.text.capacity() {
                let before = state.text.capacity();
                let needed = state.text.len().saturating_add(add_len);
                let next_cap = needed.max(before.saturating_mul(2));
                let delta = (next_cap.saturating_sub(before) as u64).saturating_mul(2);
                if retain(delta).is_err() {
                    let exact = (needed.saturating_sub(before) as u64).saturating_mul(2);
                    retain(exact)?;
                    state.text.reserve_exact(add_len);
                    let actual =
                        (state.text.capacity().saturating_sub(before) as u64).saturating_mul(2);
                    if actual > exact {
                        retain(actual - exact)?;
                    }
                    self.growth += actual.max(exact);
                } else {
                    state.text.reserve(next_cap - state.text.len());
                    let actual =
                        (state.text.capacity().saturating_sub(before) as u64).saturating_mul(2);
                    if actual > delta {
                        retain(actual - delta)?;
                    }
                    self.growth += actual.max(delta);
                }
            }
            if !state.empty {
                state.text.push_str(&self.separator);
            }
            state.text.push_str(&from.text);
            state.empty = false;
        }
        ctx.check()?;
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
        finish_concat(ctx, self.distinct, state.empty, state.ok, state.text)
    }

    fn finish_intern_bytes(&self, ctx: &Ctx) -> Result<u64> {
        let mut total = 0u64;
        for (i, state) in self.groups.iter().enumerate() {
            if i.is_multiple_of(1024) {
                ctx.check()?;
            }
            if state.ok {
                total = total.saturating_add((state.text.len() as u64).saturating_add(128));
            }
        }
        Ok(total)
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

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        slots: &[u32],
        budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
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
                let entry = (slot, key);
                if self.rows.contains(&entry) {
                    false
                } else {
                    // the boxed row, the set's entry and room for its table to grow
                    let bytes = batch.table.cols.len() as u64 * 8 + 96;
                    budget.retain(bytes)?;
                    self.growth += bytes;
                    self.rows.insert(entry)
                }
            } else {
                match id {
                    // an error is never a duplicate
                    Some(id) if !id.is_undef() => {
                        if self.values.contains(&(slot, id)) {
                            false
                        } else {
                            // the set's entry, with room for its table to grow
                            budget.retain(64)?;
                            self.growth += 64;
                            self.values.insert((slot, id))
                        }
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
        self.inner.add_batch(ctx, &inner, &keep, budget)
    }

    fn group_bytes(&self) -> u64 {
        self.inner.group_bytes()
    }

    fn finish(&mut self, ctx: &Ctx, group: usize) -> Result<Id> {
        self.inner.finish(ctx, group)
    }

    fn finish_with(
        &mut self,
        ctx: &Ctx,
        group: usize,
        scratch: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        self.inner.finish_with(ctx, group, scratch)
    }

    fn finish_replay(
        &mut self,
        ctx: &Ctx,
        group: usize,
        aggregate: usize,
        deferred: &mut Deferred,
        scratch: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        self.inner
            .finish_replay(ctx, group, aggregate, deferred, scratch)
    }

    fn finish_intern_bytes(&self, ctx: &Ctx) -> Result<u64> {
        self.inner.finish_intern_bytes(ctx)
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

    fn add_batch(
        &mut self,
        ctx: &Ctx,
        batch: &Batch<'_>,
        _slots: &[u32],
        _budget: &mut BatchBudget<'_>,
    ) -> Result<()> {
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
                crate::sparql::exec::aggregate(
                    ctx,
                    batch.table,
                    &map,
                    rows,
                    &self.agg,
                    arg,
                    exists,
                )?
            };
            ctx.check()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incr::persist::{Reader, Writer};
    use crate::incr::terms::Terms;
    use crate::sparql::bignum::{BigDec, BigInt};
    use crate::sparql::ctx::Ctx;
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, Term};

    fn make_ctx() -> (Store, Ctx) {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let ctx = Ctx::new(snap);
        (store, ctx)
    }

    fn sum_agg() -> Agg {
        Agg {
            func: AggregateFunction::Sum,
            expr: Some(Expr::Var(0)),
            distinct: false,
            fold: None,
            registered: None,
        }
    }

    /// Regression for P2-1: when a full 128-bucket hashbrown map loses one entry to a
    /// deletion leaving a tombstone (`len == 111, capacity == 111, growth_left == 0`),
    /// the next insert can double the table to 256 buckets. Both `RetractableSum` and
    /// `RetractableDistinct` must pre-admit that resize before allocating so a tight
    /// budget refuses with `allocation_size()` unchanged.
    #[test]
    fn tombstone_in_full_retractable_map_refuses_before_resize() {
        let (_store, ctx) = make_ctx();
        let empty_table = Table::new(Vec::new());

        // 1. RetractableSum decimal map: fill to 112 distinct decimals (128 buckets,
        // growth_left == 0), retract one entry that leaves a tombstone, then verify a
        // tight budget refuses before `decs.allocation_size()` increases.
        let mut sum = RetractableSum::new(&sum_agg());
        sum.init(1);
        let dec_ids: Vec<Id> = (0..130i64)
            .map(|i| ctx.intern_value(&Value::Decimal(Decimal::from(i + 1))))
            .collect();
        let init_slots = vec![0u32; 112];
        let mut allow_all = |_delta: u64| Ok(());
        {
            let mut budget = BatchBudget::Retain(&mut allow_all);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&dec_ids[..112]),
                rows: None,
                weights: None,
                base: 0,
            };
            sum.add_batch(&ctx, &batch, &init_slots, &mut budget)
                .unwrap();
        }
        assert_eq!(sum.groups[0].decs.len(), 112);
        assert_eq!(sum.groups[0].decs.capacity(), 112);
        let alloc_128 = sum.groups[0].decs.allocation_size();

        // Find a key among the 112 whose retraction leaves a tombstone (`capacity == len`).
        let mut left_tombstone = false;
        for &victim in &dec_ids[..112] {
            let ret_batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&[victim]),
                rows: None,
                weights: Some(&[-1]),
                base: 0,
            };
            assert!(sum.retract(&ctx, &ret_batch, &[0]).unwrap());
            if sum.groups[0].decs.capacity() == sum.groups[0].decs.len() {
                left_tombstone = true;
                break;
            }
            let add_batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&[victim]),
                rows: None,
                weights: Some(&[1]),
                base: 0,
            };
            let mut budget = BatchBudget::Retain(&mut allow_all);
            sum.add_batch(&ctx, &add_batch, &[0], &mut budget).unwrap();
        }
        assert!(
            left_tombstone,
            "expected at least one deletion in full 128-bucket decs map to leave a tombstone"
        );
        assert_eq!(sum.groups[0].decs.len(), 111);
        assert_eq!(sum.groups[0].decs.capacity(), 111);
        assert_eq!(sum.groups[0].decs.allocation_size(), alloc_128);
        let predicted = hbmap_insert_growth(&sum.groups[0].decs, 0);
        assert!(
            predicted > 0,
            "hbmap_insert_growth must predict 256-bucket resize on exhausted tombstone map"
        );

        let mut refused_delta = None;
        let mut tight = |delta: u64| {
            refused_delta = Some(delta);
            Err(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                limit: alloc_128 as u64 + 64,
                requested: alloc_128 as u64 + delta,
            }))
        };
        {
            let mut budget = BatchBudget::Retain(&mut tight);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&[dec_ids[120]]),
                rows: None,
                weights: Some(&[1]),
                base: 0,
            };
            let err = sum
                .add_batch(&ctx, &batch, &[0], &mut budget)
                .expect_err("tight budget must refuse tombstone-triggered decimal resize");
            assert!(matches!(err, Error::BudgetExceeded(_)), "got {err:?}");
        }
        assert_eq!(refused_delta, Some(predicted));
        assert_eq!(
            sum.groups[0].decs.allocation_size(),
            alloc_128,
            "decs table must not resize before budget admission"
        );

        // 2. RetractableDistinct Values map: same tombstone-at-capacity scenario.
        let mut distinct =
            RetractableDistinct::new(Box::new(RetractableSum::new(&sum_agg())), false, 1);
        distinct.init(1);
        let int_ids: Vec<Id> = (0..130i64)
            .map(|i| Id::from_i64((i * 1_000_003) ^ 0x55aa).unwrap())
            .collect();
        {
            let mut budget = BatchBudget::Retain(&mut allow_all);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&int_ids[..112]),
                rows: None,
                weights: None,
                base: 0,
            };
            distinct
                .add_batch(&ctx, &batch, &init_slots, &mut budget)
                .unwrap();
        }
        let distinct_alloc_128 = match &distinct.groups[0] {
            DistinctGroup::Values(m) => {
                assert_eq!(m.len(), 112);
                assert_eq!(m.capacity(), 112);
                m.allocation_size()
            }
            DistinctGroup::Rows(_) => unreachable!(),
        };

        let mut distinct_tombstone = false;
        for &victim in &int_ids[..112] {
            let ret_batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&[victim]),
                rows: None,
                weights: Some(&[-1]),
                base: 0,
            };
            assert!(distinct.retract(&ctx, &ret_batch, &[0]).unwrap());
            let exhausted = match &distinct.groups[0] {
                DistinctGroup::Values(m) => m.capacity() == m.len(),
                DistinctGroup::Rows(_) => unreachable!(),
            };
            if exhausted {
                distinct_tombstone = true;
                break;
            }
            let add_batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&[victim]),
                rows: None,
                weights: Some(&[1]),
                base: 0,
            };
            let mut budget = BatchBudget::Retain(&mut allow_all);
            distinct
                .add_batch(&ctx, &add_batch, &[0], &mut budget)
                .unwrap();
        }
        assert!(
            distinct_tombstone,
            "expected at least one deletion in full 128-bucket distinct map to leave a tombstone"
        );
        let distinct_predicted = match &distinct.groups[0] {
            DistinctGroup::Values(m) => hbmap_insert_growth(m, 0),
            DistinctGroup::Rows(_) => unreachable!(),
        };
        assert!(distinct_predicted > 0);

        let mut refused_distinct = None;
        let mut tight_distinct = |delta: u64| {
            refused_distinct = Some(delta);
            Err(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                limit: distinct_alloc_128 as u64 + 64,
                requested: distinct_alloc_128 as u64 + delta,
            }))
        };
        {
            let mut budget = BatchBudget::Retain(&mut tight_distinct);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&[int_ids[125]]),
                rows: None,
                weights: Some(&[1]),
                base: 0,
            };
            let err = distinct
                .add_batch(&ctx, &batch, &[0], &mut budget)
                .expect_err("tight budget must refuse tombstone-triggered distinct resize");
            assert!(matches!(err, Error::BudgetExceeded(_)), "got {err:?}");
        }
        assert_eq!(refused_distinct, Some(distinct_predicted));
        match &distinct.groups[0] {
            DistinctGroup::Values(m) => assert_eq!(
                m.allocation_size(),
                distinct_alloc_128,
                "distinct Values table must not resize before budget admission"
            ),
            DistinctGroup::Rows(_) => unreachable!(),
        }
    }

    /// Regression for P2-2: O(1) incremental `retained_bytes()` on `RetractableSum`,
    /// `Ordered`, and `RetractableDistinct` must match a direct full scan across big-number
    /// inserts, retractions, `swap_remove`, `shrink_unused`, `finish`, `clear`, and
    /// checkpoint round-trips.
    #[test]
    fn retractable_retained_bytes_matches_exact_scan() {
        let (store, ctx) = make_ctx();
        let snap = store.snapshot();
        let empty_table = Table::new(Vec::new());
        let mut allow_all = |_delta: u64| Ok(());

        let exact_sum_bytes = |acc: &RetractableSum| -> u64 {
            acc.groups.capacity() as u64 * size_of::<SumState>() as u64
                + acc
                    .groups
                    .iter()
                    .map(SumState::exact_map_bytes)
                    .sum::<u64>()
        };
        let exact_ordered_bytes = |acc: &Ordered| -> u64 {
            acc.groups.capacity() as u64 * size_of::<Multiset>() as u64
                + acc
                    .groups
                    .iter()
                    .map(|m| m.vals.capacity() as u64 * size_of::<(Id, i64)>() as u64)
                    .sum::<u64>()
        };
        let exact_distinct_bytes = |acc: &RetractableDistinct| -> u64 {
            acc.groups.capacity() as u64 * size_of::<DistinctGroup>() as u64
                + acc
                    .groups
                    .iter()
                    .map(|g| distinct_group_bytes(g, acc.row_width))
                    .sum::<u64>()
                + acc.inner.retained_bytes()
        };

        // 1. RetractableSum across BigInt, BigDec, Decimal, Float, Double and 3 groups.
        let mut sum = RetractableSum::new(&sum_agg());
        sum.init(3);
        assert_eq!(sum.retained_bytes(), exact_sum_bytes(&sum));

        let mut vals = Vec::new();
        let mut slots = Vec::new();
        for i in 0..300u64 {
            let bi = BigInt::parse(&format!("9999999999999999999999999999999999{i:04}")).unwrap();
            let bd = BigDec::parse(&format!("1234567890123456789012345678901234.{i:04}")).unwrap();
            vals.push(ctx.intern_value(&Value::from_bigint(bi)));
            slots.push((i % 3) as u32);
            vals.push(ctx.intern_value(&Value::from_bigdec(bd)));
            slots.push(((i + 1) % 3) as u32);
            vals.push(ctx.intern_value(&Value::Double(Double::from(i as f64 + 0.5))));
            slots.push(((i + 2) % 3) as u32);
        }
        {
            let mut budget = BatchBudget::Retain(&mut allow_all);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&vals),
                rows: None,
                weights: None,
                base: 0,
            };
            sum.add_batch(&ctx, &batch, &slots, &mut budget).unwrap();
        }
        assert_eq!(sum.retained_bytes(), exact_sum_bytes(&sum));
        assert!(sum.maps_bytes > 0);

        // Retract half the big-number values.
        let half = vals.len() / 2;
        let neg_w = vec![-1i64; half];
        let ret_batch = Batch {
            table: &empty_table,
            arg: Arg::Column(&vals[..half]),
            rows: None,
            weights: Some(&neg_w),
            base: 0,
        };
        assert!(sum.retract(&ctx, &ret_batch, &slots[..half]).unwrap());
        assert_eq!(sum.retained_bytes(), exact_sum_bytes(&sum));

        // Checkpoint save + load round-trip on group 0.
        let terms = Terms::default();
        let mut w = Writer::new(&snap, &terms);
        sum.save_group(0, &mut w).unwrap();
        let bytes = w.finish();
        let mut load_terms = Terms::default();
        let mut r = Reader::new(&bytes, &snap, &mut load_terms).unwrap();
        let mut reloaded = RetractableSum::new(&sum_agg());
        reloaded.init(1);
        reloaded.load_group(0, &mut r).unwrap();
        assert_eq!(reloaded.retained_bytes(), exact_sum_bytes(&reloaded));

        // swap_remove, finish, shrink_unused, clear.
        sum.swap_remove(1);
        assert_eq!(sum.retained_bytes(), exact_sum_bytes(&sum));
        let _ = sum.finish(&ctx, 0).unwrap();
        assert_eq!(sum.retained_bytes(), exact_sum_bytes(&sum));
        sum.shrink_unused();
        assert_eq!(sum.retained_bytes(), exact_sum_bytes(&sum));
        sum.clear();
        assert_eq!(sum.retained_bytes(), 0);

        // 2. Ordered (MIN) across adds, retracts, swap_remove, finish, clear.
        let min_agg = Agg {
            func: AggregateFunction::Min,
            expr: Some(Expr::Var(0)),
            distinct: false,
            fold: None,
            registered: None,
        };
        let mut ordered = Ordered::new(&min_agg);
        ordered.init(2);
        let str_ids: Vec<Id> = (0..64)
            .map(|i| {
                ctx.intern_term(&Term::Literal(Literal::new_simple_literal(format!(
                    "v{i:03}"
                ))))
            })
            .collect();
        let ord_slots: Vec<u32> = (0..64).map(|i| (i % 2) as u32).collect();
        {
            let mut budget = BatchBudget::Retain(&mut allow_all);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&str_ids),
                rows: None,
                weights: None,
                base: 0,
            };
            ordered
                .add_batch(&ctx, &batch, &ord_slots, &mut budget)
                .unwrap();
        }
        assert_eq!(ordered.retained_bytes(), exact_ordered_bytes(&ordered));
        let ord_neg = vec![-1i64; 32];
        assert!(
            ordered
                .retract(
                    &ctx,
                    &Batch {
                        table: &empty_table,
                        arg: Arg::Column(&str_ids[..32]),
                        rows: None,
                        weights: Some(&ord_neg),
                        base: 0,
                    },
                    &ord_slots[..32],
                )
                .unwrap()
        );
        assert_eq!(ordered.retained_bytes(), exact_ordered_bytes(&ordered));
        ordered.swap_remove(0);
        assert_eq!(ordered.retained_bytes(), exact_ordered_bytes(&ordered));
        let _ = ordered.finish(&ctx, 0).unwrap();
        assert_eq!(ordered.retained_bytes(), exact_ordered_bytes(&ordered));
        ordered.clear();
        assert_eq!(ordered.retained_bytes(), 0);

        // 3. RetractableDistinct (Values) across adds, retracts, clone_box, swap_remove, clear.
        let mut distinct =
            RetractableDistinct::new(Box::new(RetractableSum::new(&sum_agg())), false, 1);
        distinct.init(2);
        {
            let mut budget = BatchBudget::Retain(&mut allow_all);
            let batch = Batch {
                table: &empty_table,
                arg: Arg::Column(&vals[..60]),
                rows: None,
                weights: None,
                base: 0,
            };
            distinct
                .add_batch(&ctx, &batch, &ord_slots[..60], &mut budget)
                .unwrap();
        }
        assert_eq!(distinct.retained_bytes(), exact_distinct_bytes(&distinct));
        let mut cloned = distinct.clone_box().unwrap();
        let cloned_ref = cloned
            .as_any()
            .downcast_ref::<RetractableDistinct>()
            .expect("cloned RetractableDistinct");
        assert_eq!(
            cloned_ref.retained_bytes(),
            exact_distinct_bytes(cloned_ref)
        );
        assert!(
            distinct
                .retract(
                    &ctx,
                    &Batch {
                        table: &empty_table,
                        arg: Arg::Column(&vals[..30]),
                        rows: None,
                        weights: Some(&ord_neg[..30]),
                        base: 0,
                    },
                    &ord_slots[..30],
                )
                .unwrap()
        );
        assert_eq!(distinct.retained_bytes(), exact_distinct_bytes(&distinct));
        distinct.swap_remove(0);
        assert_eq!(distinct.retained_bytes(), exact_distinct_bytes(&distinct));
        distinct.clear();
        assert_eq!(distinct.retained_bytes(), 0);
    }
}
