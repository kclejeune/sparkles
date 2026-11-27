//! Query-owned exact-ID reuse for pure, repeated single-input BIND expressions.
//! Dense inline integers retain their cheap admission. Other inputs need repetition
//! observed in a pilot batch, a sampled sketch across batches or each batch's exact
//! distinct count, and weak repetition of a parallel-capable expression must also pass
//! a cost guard. Map misses and the ordinary evaluation share one split controller that
//! retries the pool after a cooldown. Optional cache entries never weaken query budgets,
//! and a refusal of memory that the map's state causes yields the batch's unevaluated
//! keys to ordinary evaluation instead of failing the query, while the keys whose
//! misses finished keep their values. Expressions that can construct a geometry
//! are not admitted, because each constructed geometry stays charged to the query even
//! when a yielded batch discards it.
use crate::error::Result;
use crate::id::{Id, PAYLOAD_MASK, Tag};
use crate::index::{O, P, Perm, S};
use crate::sparql::ctx::RetainedCharge;
use crate::sparql::exec::{self, ColumnEval, OptionalColumn, Split, SplitOutcome};
use crate::sparql::expr::{Expr, Func};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use crate::sparql::{Ctx, Table};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::Function;

pub(super) struct Admission {
    input: VarId,
    expected_reuse: bool,
    direct_str_input: bool,
}

/// Entries of a general map, and of the sampled-key sketch times [`SAMPLE`].
const MAP_ENTRIES: usize = 1 << 16;
/// Entries of a map admitted by dense inline integers.
const INTEGER_ENTRIES: usize = 1 << 14;
/// One key in `1 << SAMPLE_SHIFT` is observed by the sketch, chosen by its hash, so
/// the sketch sees every occurrence of the keys it samples.
const SAMPLE_SHIFT: u32 = 4;
/// Most keys the sketch holds: it covers as many distinct keys as a full map.
const SKETCH_KEYS: usize = MAP_ENTRIES >> SAMPLE_SHIFT;
/// Sampled rows per admission decision, about 4,096 input rows.
const WINDOW: u32 = 256;
/// Rows per decision whether an admitted map still pays. The first decision waits
/// until the map has seen [`MAP_ENTRIES`] rows or filled, since a map admitted late in a
/// cyclic input misses until one period has passed.
const MAP_WINDOW: u32 = 16384;
/// Charged bytes per map entry and per sketch key, beside a fixed 512.
const ENTRY_BYTES: u64 = 96;
const SKETCH_BYTES: u64 = 32;
/// Charged bytes per row of a batch the map answers, while it runs.
const SCRATCH_BYTES: u64 = 28 + 40;
/// Consecutive clear split losses that move a bucket into serial cooldown.
const SPLIT_LOSSES: u8 = 2;
/// Eligible serial evaluations per cooldown before a split is retried.
const SPLIT_COOLDOWN: u8 = 8;
/// Eligible serial evaluations per cooldown of the stored-IRI map misses before a split
/// is retried. One split that does not pay starts it.
const IRI_SPLIT_COOLDOWN: u8 = 16;
/// Largest evaluated row count in the small split/cost bucket (`768..=2047`).
const SMALL_BUCKET_MAX: usize = 2047;
/// Minimum estimated per-row evaluation saving (ns) for unhinted weak admission on
/// parallel-capable expressions (`R * T >= COST_MIN_SAVING_NANOS * N * M`).
const COST_MIN_SAVING_NANOS: u128 = 50;
/// Minimum estimated per-row evaluation saving (ns) that lets one completed weak
/// decision admit at once when two usable same-bucket rates exist.
const COST_FAST_SAVING_NANOS: u128 = COST_MIN_SAVING_NANOS * 2;
/// Consecutive completed batch decisions that must pass the weak cost rule.
const WEAK_CONFIRMATIONS: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Waiting for the first batch of at least [`exprcache::MIN_ROWS`] rows.
    Pilot,
    /// Observing sampled keys across batches.
    Probe,
    /// Answering from the map, evaluating misses once per distinct key.
    Map,
    /// Ordinary evaluation for the rest of the query.
    Off,
}

/// Admission needs one repeated row in `ADMIT`, and an admitted map is released once
/// it answers fewer than one row in `2 * ADMIT` without an evaluation.
const ADMIT: u32 = 8;
/// The tag bits of a map entry whose value the current batch has not yet evaluated.
/// No term carries this tag, so a pending entry cannot be mistaken for a value.
const PENDING: u64 = !PAYLOAD_MASK;
/// The value of a miss that a refusal of memory left unevaluated. It carries the
/// pending tag, which no term carries, with a payload that no slot of a batch reaches.
const UNEVALUATED: Id = Id(PENDING | PAYLOAD_MASK);

/// Two evaluated-row buckets for sampled batch splits (`768..=2047` and `2048..=16384`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitBucket {
    Small,
    Large,
}

fn split_bucket(rows: usize) -> Option<SplitBucket> {
    if !exec::batch_may_split(rows) || rows > exec::PAR_THRESHOLD {
        return None;
    }
    if rows <= SMALL_BUCKET_MAX {
        Some(SplitBucket::Small)
    } else {
        Some(SplitBucket::Large)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BucketPhase {
    Active { losses: u8 },
    Cooldown { remaining: u8 },
    Retry,
}

/// Pure query-owned two-bucket split controller shared across map misses, per-batch
/// distinct columns and ordinary BIND row fallback. Survives [`Reuse::decline`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SplitController {
    /// Eligible serial evaluations per cooldown, at least 1.
    cooldown: u8,
    small: BucketPhase,
    large: BucketPhase,
}

impl SplitController {
    fn new(cooldown: u8) -> Self {
        debug_assert!(cooldown > 0);
        Self {
            cooldown,
            small: BucketPhase::Active { losses: 0 },
            large: BucketPhase::Active { losses: 0 },
        }
    }

    fn bucket(&self, b: SplitBucket) -> BucketPhase {
        match b {
            SplitBucket::Small => self.small,
            SplitBucket::Large => self.large,
        }
    }

    fn bucket_mut(&mut self, b: SplitBucket) -> &mut BucketPhase {
        match b {
            SplitBucket::Small => &mut self.small,
            SplitBucket::Large => &mut self.large,
        }
    }

    /// Cheap guard avoiding `expr.parallel()` traversal when no bucket can suppress.
    fn may_suppress(&self) -> bool {
        matches!(self.small, BucketPhase::Cooldown { .. })
            || matches!(self.large, BucketPhase::Cooldown { .. })
    }

    /// Whether splitting is permitted for an evaluation of `rows` rows.
    /// Tables above [`exec::PAR_THRESHOLD`] keep their existing large-table behavior
    /// and never consume or depend on either sampled-split bucket.
    fn allow(&self, rows: usize, expr_parallel: bool, multi_worker: bool) -> bool {
        if !expr_parallel || !multi_worker {
            return true;
        }
        match split_bucket(rows) {
            Some(b) => !matches!(self.bucket(b), BucketPhase::Cooldown { .. }),
            None => true,
        }
    }

    /// Whether the controller has observed sustained split contention in `rows`'s bucket.
    fn is_contended(&self, rows: usize) -> bool {
        split_bucket(rows).is_some_and(|b| {
            matches!(
                self.bucket(b),
                BucketPhase::Cooldown { .. } | BucketPhase::Retry
            )
        })
    }

    /// Update state after an evaluation and `ctx.check()` have succeeded, returning the
    /// phase change it made, if any.
    fn observe(
        &mut self,
        rows: usize,
        expr_parallel: bool,
        multi_worker: bool,
        parallel_allowed: bool,
        split: Option<Split>,
    ) -> Option<&'static str> {
        if !expr_parallel || !multi_worker {
            return None;
        }
        let b = split_bucket(rows)?;
        let cooldown = self.cooldown;
        let phase = self.bucket_mut(b);
        match *phase {
            BucketPhase::Cooldown { remaining } => {
                if parallel_allowed {
                    return None;
                }
                if remaining <= 1 {
                    *phase = BucketPhase::Retry;
                    Some("cooldown->retry")
                } else {
                    *phase = BucketPhase::Cooldown {
                        remaining: remaining - 1,
                    };
                    None
                }
            }
            BucketPhase::Active { losses } => match split?.classify() {
                SplitOutcome::Unknown => None,
                SplitOutcome::ClearWin | SplitOutcome::Inconclusive => {
                    *phase = BucketPhase::Active { losses: 0 };
                    None
                }
                SplitOutcome::ClearLoss => {
                    let next = losses.saturating_add(1);
                    if next >= SPLIT_LOSSES {
                        *phase = BucketPhase::Cooldown {
                            remaining: cooldown,
                        };
                        Some("active->cooldown")
                    } else {
                        *phase = BucketPhase::Active { losses: next };
                        None
                    }
                }
            },
            BucketPhase::Retry => match split?.classify() {
                SplitOutcome::Unknown | SplitOutcome::Inconclusive => None,
                SplitOutcome::ClearWin => {
                    *phase = BucketPhase::Active { losses: 0 };
                    Some("retry->active")
                }
                SplitOutcome::ClearLoss => {
                    *phase = BucketPhase::Cooldown {
                        remaining: cooldown,
                    };
                    Some("retry->cooldown")
                }
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IriBucketPhase {
    Active,
    Cooldown { remaining: u8 },
    Retry,
}

/// The split cooldown of map misses that are all stored IRIs under a direct
/// `STR(input)`, in [`Reuse::apply_map`]. Interning their strings contends across the
/// pool, so one split that does not pay starts a cooldown of [`IRI_SPLIT_COOLDOWN`]
/// eligible evaluations. Its observations stay apart from [`SplitController`], so they
/// change neither the fallback's cooldown nor the contention evidence of admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IriSplitController {
    cooldown: u8,
    small: IriBucketPhase,
    large: IriBucketPhase,
}

impl IriSplitController {
    fn new(cooldown: u8) -> Self {
        debug_assert!(cooldown > 0);
        Self {
            cooldown,
            small: IriBucketPhase::Active,
            large: IriBucketPhase::Active,
        }
    }

    fn bucket(&self, b: SplitBucket) -> IriBucketPhase {
        match b {
            SplitBucket::Small => self.small,
            SplitBucket::Large => self.large,
        }
    }

    fn bucket_mut(&mut self, b: SplitBucket) -> &mut IriBucketPhase {
        match b {
            SplitBucket::Small => &mut self.small,
            SplitBucket::Large => &mut self.large,
        }
    }

    fn allow(&self, b: SplitBucket) -> bool {
        !matches!(self.bucket(b), IriBucketPhase::Cooldown { .. })
    }

    /// Update state after an evaluation, returning the phase change it made, if any.
    fn observe(
        &mut self,
        b: SplitBucket,
        parallel_allowed: bool,
        split: Option<Split>,
    ) -> Option<&'static str> {
        let cooldown = self.cooldown;
        let phase = self.bucket_mut(b);
        match *phase {
            IriBucketPhase::Cooldown { remaining } => {
                if parallel_allowed {
                    return None;
                }
                if remaining <= 1 {
                    *phase = IriBucketPhase::Retry;
                    Some("iri:cooldown->retry")
                } else {
                    *phase = IriBucketPhase::Cooldown {
                        remaining: remaining - 1,
                    };
                    None
                }
            }
            IriBucketPhase::Active => {
                if split?.valid_paid()? {
                    None
                } else {
                    *phase = IriBucketPhase::Cooldown {
                        remaining: cooldown,
                    };
                    Some("iri:active->cooldown")
                }
            }
            IriBucketPhase::Retry => {
                if split?.valid_paid()? {
                    *phase = IriBucketPhase::Active;
                    Some("iri:retry->active")
                } else {
                    *phase = IriBucketPhase::Cooldown {
                        remaining: cooldown,
                    };
                    Some("iri:retry->cooldown")
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapSplitPolicy {
    Iri(SplitBucket),
    Default,
}

/// Evaluation-size buckets for fallback cost observations (`>= MIN_ROWS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CostBucket {
    Small,
    Large,
    Huge,
}

fn cost_bucket(rows: usize) -> Option<CostBucket> {
    if rows < crate::sparql::exprcache::MIN_ROWS {
        return None;
    }
    if rows <= SMALL_BUCKET_MAX {
        Some(CostBucket::Small)
    } else if rows <= exec::PAR_THRESHOLD {
        Some(CostBucket::Large)
    } else {
        Some(CostBucket::Huge)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EvalRate {
    nanos: u128,
    rows: usize,
}

impl EvalRate {
    fn lower(a: Self, b: Self) -> Option<Self> {
        let lhs = a.nanos.checked_mul(b.rows as u128)?;
        let rhs = b.nanos.checked_mul(a.rows as u128)?;
        Some(if lhs <= rhs { a } else { b })
    }

    /// Exact rational check `r * E >= COST_MIN_SAVING_NANOS`, i.e.,
    /// `repeats * self.nanos >= COST_MIN_SAVING_NANOS * observed_rows * self.rows`.
    fn meets_saving(&self, observed_rows: usize, repeats: usize) -> bool {
        self.meets_saving_nanos(observed_rows, repeats, COST_MIN_SAVING_NANOS)
    }

    /// Exact rational check `r * E >= min_saving_nanos`, i.e.,
    /// `repeats * self.nanos >= min_saving_nanos * observed_rows * self.rows`.
    fn meets_saving_nanos(
        &self,
        observed_rows: usize,
        repeats: usize,
        min_saving_nanos: u128,
    ) -> bool {
        if observed_rows == 0
            || repeats == 0
            || self.rows == 0
            || self.nanos == 0
            || min_saving_nanos == 0
        {
            return false;
        }
        let Some(lhs) = (repeats as u128).checked_mul(self.nanos) else {
            return false;
        };
        let Some(rhs) = min_saving_nanos
            .checked_mul(observed_rows as u128)
            .and_then(|v| v.checked_mul(self.rows as u128))
        else {
            return false;
        };
        lhs >= rhs
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeakDecision {
    Fail,
    Pass,
    FastAdmit,
}

impl WeakDecision {
    fn passed(self) -> bool {
        !matches!(self, Self::Fail)
    }
}

/// Last two usable fallback column-evaluation rates in one evaluation-size bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CostHistory {
    bucket: Option<CostBucket>,
    samples: [Option<EvalRate>; 2],
}

impl CostHistory {
    fn record(&mut self, rows: usize, nanos: u128) {
        if nanos == 0 {
            return;
        }
        let Some(bucket) = cost_bucket(rows) else {
            return;
        };
        let rate = EvalRate { nanos, rows };
        if self.bucket != Some(bucket) {
            self.bucket = Some(bucket);
            self.samples = [Some(rate), None];
            return;
        }
        if self.samples[0].is_none() {
            self.samples[0] = Some(rate);
        } else if self.samples[1].is_none() {
            self.samples[1] = Some(rate);
        } else {
            self.samples[0] = self.samples[1];
            self.samples[1] = Some(rate);
        }
    }

    fn lower_rate(&self, bucket: CostBucket) -> Option<EvalRate> {
        if self.bucket != Some(bucket) {
            return None;
        }
        EvalRate::lower(self.samples[0]?, self.samples[1]?)
    }
}

pub(super) struct Reuse {
    input: VarId,
    direct_str_input: bool,
    mode: Mode,
    values: FxHashMap<Id, Id>,
    charge: Option<RetainedCharge>,
    expected_reuse: bool,
    max_entries: usize,
    sketch: FxHashSet<Id>,
    /// The sketch filled without admitting the map. The next batch declines, so the
    /// exact count of the batch that filled it can still admit through fallback feedback.
    exhausted: bool,
    /// Sampled rows, and those whose key the sketch held, in the current window.
    window: (u32, u32),
    /// Completed probe window `(rows, repeats, batch_rows)` waiting for the batch's
    /// fallback observation so at most one weak decision is counted per batch.
    batch_probe_window: Option<(usize, usize, usize)>,
    /// Consecutive completed probe windows passing the weak cost guard. A batch that
    /// completes no window leaves the count alone.
    weak_confirmations: u8,
    /// Consecutive exact within-batch counts passing the weak cost guard. A batch's count
    /// says nothing about repetition across batches, so its failure resets only this
    /// count and never the probe windows' confirmations.
    counted_confirmations: u8,
    /// Last two usable fallback column-evaluation rates in one size bucket.
    cost: CostHistory,
    /// Rows, and those answered without an evaluation, since the map's last check.
    map_window: (u32, u32),
    /// Rows the map has seen since it was admitted.
    map_rows: u64,
    batch_index: u64,
    hits: u64,
    misses: u64,
    /// Two-bucket split controller shared across map misses and controlled BIND fallback.
    split: SplitController,
    /// Separate split cooldown for map misses that are all stored IRIs under `STR(input)`.
    iri_split: IriSplitController,
    /// The batch and reason of the last admission, for tests.
    #[cfg(test)]
    admitted: Option<(u64, &'static str)>,
    #[cfg(test)]
    evaluations: usize,
    #[cfg(test)]
    last_miss_dispatch: Option<(bool, Option<Split>)>,
    #[cfg(test)]
    last_fallback_dispatch: Option<(usize, bool, Option<Split>)>,
    /// For each batch whose misses met a refusal of memory, the misses that finished
    /// and the slots evaluated after the map was released, for tests.
    #[cfg(test)]
    refusals: Vec<(usize, usize)>,
}

impl Reuse {
    pub(super) fn prepare(ctx: &Ctx, expr: &Expr, source: Option<&Node>) -> Option<Admission> {
        if !ctx.opt.expr_cache {
            return None;
        }
        let input = crate::sparql::exprcache::eligible(&[expr]).ok().flatten()?;
        // A geometry constructor keeps its result's charge until the query ends, so
        // misses that a refusal later discards would leave that charge behind for the
        // ordinary evaluation of the batch.
        if expr.charges_geometry() {
            return None;
        }
        Some(Admission {
            input,
            direct_str_input: is_direct_str_input(expr, input),
            expected_reuse: source.is_some_and(|source| {
                source.est.is_finite()
                    && source.est > 0.0
                    && distinct_bound(ctx, source, input, &mut 64)
                        .is_some_and(|distinct| distinct as f64 <= source.est * 0.8)
            }),
        })
    }

    pub(super) fn new(ctx: &Ctx, admission: Admission) -> Option<Self> {
        // Compute the hint before building children, but retain optional memory
        // only afterward. Optional caching must not starve child construction.
        let charge = ctx.retained_charge(512).ok()?;
        Some(Self {
            input: admission.input,
            direct_str_input: admission.direct_str_input,
            mode: Mode::Pilot,
            values: Default::default(),
            charge,
            expected_reuse: admission.expected_reuse,
            max_entries: INTEGER_ENTRIES,
            sketch: Default::default(),
            exhausted: false,
            window: (0, 0),
            batch_probe_window: None,
            weak_confirmations: 0,
            counted_confirmations: 0,
            cost: CostHistory::default(),
            map_window: (0, 0),
            map_rows: 0,
            batch_index: 0,
            hits: 0,
            misses: 0,
            split: SplitController::new(SPLIT_COOLDOWN),
            iri_split: IriSplitController::new(IRI_SPLIT_COOLDOWN),
            #[cfg(test)]
            admitted: None,
            #[cfg(test)]
            evaluations: 0,
            #[cfg(test)]
            last_miss_dispatch: None,
            #[cfg(test)]
            last_fallback_dispatch: None,
            #[cfg(test)]
            refusals: Vec::new(),
        })
    }

    /// The sampled-split size bucket of a batch of map misses that are all stored IRIs
    /// under a direct `STR(input)`, which take the IRI cooldown, or `None` for others.
    fn iri_miss_bucket(
        &self,
        ctx: &Ctx,
        miss_keys: &[Id],
        multi_worker: bool,
    ) -> Option<SplitBucket> {
        if !self.direct_str_input || !multi_worker {
            return None;
        }
        let bucket = split_bucket(miss_keys.len())?;
        all_stored_iris(ctx, miss_keys).then_some(bucket)
    }

    fn map_split_policy(&self, ctx: &Ctx, miss_keys: &[Id], multi_worker: bool) -> MapSplitPolicy {
        match self.iri_miss_bucket(ctx, miss_keys, multi_worker) {
            Some(bucket) => MapSplitPolicy::Iri(bucket),
            None => MapSplitPolicy::Default,
        }
    }

    /// Whether the controlled BIND fallback may split an evaluation of `rows` rows.
    pub(super) fn allow_fallback_split(&self, rows: usize, expr: &Expr) -> bool {
        if !self.split.may_suppress() {
            return true;
        }
        self.split
            .allow(rows, expr.parallel(), rayon::current_num_threads() > 1)
    }

    /// Whether map miss evaluation may split an evaluation of `rows` rows.
    fn allow_map_split(&self, rows: usize, expr: &Expr) -> bool {
        if !self.split.may_suppress() {
            return true;
        }
        self.split
            .allow(rows, expr.parallel(), rayon::current_num_threads() > 1)
    }

    /// Whether the controlled fallback should time full column evaluation for cost-guarded
    /// admission. Restricted to `>= MIN_ROWS` evaluations of parallel-capable expressions
    /// while the map probes, since only a probing map records the rates. A batch that
    /// leaves the map in its pilot, such as sparse inline integers, takes no clock reads.
    pub(super) fn want_cost_timing(&self, evaluated_rows: usize, expr: &Expr) -> bool {
        evaluated_rows >= crate::sparql::exprcache::MIN_ROWS
            && self.mode == Mode::Probe
            && expr.parallel()
            && rayon::current_num_threads() > 1
    }

    fn observe_map_split(
        &mut self,
        rows: usize,
        expr: &Expr,
        parallel_allowed: bool,
        split: Option<Split>,
    ) {
        if split.is_none() && !self.split.may_suppress() {
            return;
        }
        let expr_parallel = split.is_some() || expr.parallel();
        let multi_worker = split.is_some() || rayon::current_num_threads() > 1;
        self.split
            .observe(rows, expr_parallel, multi_worker, parallel_allowed, split);
    }

    /// Observe a completed fallback column evaluation after `ctx.check()` has succeeded.
    /// Updates the shared split controller (even in `Mode::Off`), records fallback cost
    /// observations while probing, and evaluates at most one weak admission decision for
    /// this batch.
    pub(super) fn observe_fallback(
        &mut self,
        batch_rows: usize,
        eval: ColumnEval,
        counted: Option<(usize, usize)>,
        expr: &Expr,
    ) {
        let expr_parallel = expr.parallel();
        let multi_worker = rayon::current_num_threads() > 1;
        self.observe_fallback_with(batch_rows, eval, counted, expr_parallel, multi_worker);
    }

    fn observe_fallback_with(
        &mut self,
        batch_rows: usize,
        eval: ColumnEval,
        counted: Option<(usize, usize)>,
        expr_parallel: bool,
        multi_worker: bool,
    ) {
        if eval.evaluated_rows > 0 {
            #[cfg(test)]
            {
                self.last_fallback_dispatch =
                    Some((eval.evaluated_rows, eval.parallel_allowed, eval.split));
            }
            self.split.observe(
                eval.evaluated_rows,
                expr_parallel,
                multi_worker,
                eval.parallel_allowed,
                eval.split,
            );
        }

        let parallel_capable = expr_parallel && multi_worker;
        if self.mode == Mode::Probe
            && parallel_capable
            && let Some(nanos) = eval.elapsed_nanos
        {
            self.cost.record(eval.evaluated_rows, nanos);
        }

        let probe_win = self.batch_probe_window.take();
        if self.mode != Mode::Probe {
            return;
        }

        if !parallel_capable {
            if let Some((rows, distinct)) = counted
                && rows >= crate::sparql::exprcache::MIN_ROWS
                && rows.saturating_sub(distinct) * ADMIT as usize >= rows
            {
                self.admit("counted");
            }
            return;
        }

        // Exact within-batch repetition >= 1/2 admits immediately.
        if let Some((rows, distinct)) = counted
            && rows >= crate::sparql::exprcache::MIN_ROWS
            && rows.saturating_sub(distinct) * 2 >= rows
        {
            self.admit("counted_exact_half");
            return;
        }

        let exact_signal = counted.and_then(|(rows, distinct)| {
            (rows >= crate::sparql::exprcache::MIN_ROWS)
                .then_some((rows, rows.saturating_sub(distinct)))
        });
        if probe_win.is_none() && exact_signal.is_none() {
            return;
        }

        let bucket_rows = if eval.evaluated_rows >= crate::sparql::exprcache::MIN_ROWS {
            eval.evaluated_rows
        } else {
            batch_rows
        };

        // Each signal confirms at most once a batch and keeps its own chain, so a batch
        // that completes no probe window, whose keys may all be distinct, cannot undo a
        // window's confirmation of repetition across batches.
        let probe_dec = probe_win.map(|(n, r, _)| self.weak_decision(n, r, bucket_rows, true));
        let exact_dec = exact_signal.map(|(n, r)| self.weak_decision(n, r, bucket_rows, true));
        let confirm = |chain: &mut u8, decision: Option<WeakDecision>| match decision {
            Some(decision) if decision.passed() => *chain = chain.saturating_add(1),
            Some(_) => *chain = 0,
            None => {}
        };
        confirm(&mut self.weak_confirmations, probe_dec);
        confirm(&mut self.counted_confirmations, exact_dec);
        let fast =
            |decision: Option<WeakDecision>| matches!(decision, Some(WeakDecision::FastAdmit));
        let admit_reason = if self.counted_confirmations >= WEAK_CONFIRMATIONS {
            Some("counted_weak")
        } else if self.weak_confirmations >= WEAK_CONFIRMATIONS {
            Some("probe_weak")
        } else if fast(exact_dec) {
            Some("counted_weak_fast")
        } else if fast(probe_dec) {
            Some("probe_weak_fast")
        } else {
            None
        };
        if let Some(reason) = admit_reason {
            self.admit(reason);
        }
    }

    /// Evaluates a weak unhinted repetition signal `(repeats / observed_rows)` against the
    /// `1/8` floor and (for parallel-capable cost mode) the rational saving and contention
    /// guards. Returns [`WeakDecision::FastAdmit`] only when the lower of two usable
    /// same-bucket rates meets [`COST_FAST_SAVING_NANOS`], keeping contention-only
    /// evidence on the ordinary two-confirmation guard.
    fn weak_decision(
        &self,
        observed_rows: usize,
        repeats: usize,
        bucket_rows: usize,
        parallel_capable: bool,
    ) -> WeakDecision {
        if observed_rows == 0 || repeats.saturating_mul(ADMIT as usize) < observed_rows {
            return WeakDecision::Fail;
        }
        if !parallel_capable {
            return WeakDecision::Pass;
        }
        let rate = cost_bucket(bucket_rows).and_then(|cb| self.cost.lower_rate(cb));
        if rate.is_some_and(|rate| {
            rate.meets_saving_nanos(observed_rows, repeats, COST_FAST_SAVING_NANOS)
        }) {
            return WeakDecision::FastAdmit;
        }
        if self.split.is_contended(bucket_rows) {
            return WeakDecision::Pass;
        }
        if rate.is_some_and(|rate| rate.meets_saving(observed_rows, repeats)) {
            WeakDecision::Pass
        } else {
            WeakDecision::Fail
        }
    }

    /// Rows answered from the map and evaluations made for it, for EXPLAIN.
    pub(super) fn counters(&self) -> Option<(u64, u64)> {
        (self.hits + self.misses > 0).then_some((self.hits, self.misses))
    }

    pub(super) fn apply(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        target: VarId,
        expr: &Expr,
    ) -> Result<bool> {
        let Some(column) = table.col_of(self.input) else {
            return Ok(false);
        };
        if let Some((w_rows, w_repeats, bucket_rows)) = self.batch_probe_window.take() {
            let parallel_capable = expr.parallel() && rayon::current_num_threads() > 1;
            let decision = self.weak_decision(w_rows, w_repeats, bucket_rows, parallel_capable);
            let admit_now = self.mode == Mode::Probe && matches!(decision, WeakDecision::FastAdmit);
            if decision.passed() {
                self.weak_confirmations = self.weak_confirmations.saturating_add(1);
            } else {
                self.weak_confirmations = 0;
            }
            if admit_now {
                self.admit("probe_weak_fast");
            }
        }
        self.batch_index = self.batch_index.saturating_add(1);
        match self.mode {
            Mode::Off => return Ok(false),
            Mode::Map => {}
            Mode::Probe if self.exhausted => {
                self.decline();
                return Ok(false);
            }
            Mode::Pilot | Mode::Probe => {
                if table.len < crate::sparql::exprcache::MIN_ROWS {
                    // Small batches only feed the sketch of a probing map.
                    if self.mode == Mode::Pilot || !self.probe(ctx, &table.cols[column], expr)? {
                        return Ok(false);
                    }
                } else if let Some(dense) = dense_integers(ctx, &table.cols[column])? {
                    if !dense {
                        return Ok(false);
                    }
                    self.max_entries = INTEGER_ENTRIES;
                    self.admit("dense_int");
                } else if self.mode == Mode::Pilot {
                    if !self.pilot(ctx, &table.cols[column], expr)? {
                        return Ok(false);
                    }
                } else if !self.probe(ctx, &table.cols[column], expr)? {
                    return Ok(false);
                }
            }
        }
        self.apply_map(ctx, table, column, target, expr)
    }

    fn admit(&mut self, reason: &'static str) {
        self.mode = Mode::Map;
        self.sketch = Default::default();
        self.exhausted = false;
        self.weak_confirmations = 0;
        self.counted_confirmations = 0;
        self.batch_probe_window = None;
        self.map_window = (0, 0);
        self.map_rows = 0;
        #[cfg(test)]
        {
            self.admitted = Some((self.batch_index, reason));
        }
        #[cfg(not(test))]
        let _ = reason;
    }

    fn decline(&mut self) {
        self.mode = Mode::Off;
        self.sketch = Default::default();
        self.values = Default::default();
        self.exhausted = false;
        self.weak_confirmations = 0;
        self.counted_confirmations = 0;
        self.batch_probe_window = None;
        self.cost = CostHistory::default();
        // `Mode::Off` is final and holds nothing, so the ordinary evaluation of the rest
        // of the query has the whole retained charge.
        self.charge = None;
    }

    /// Release the map, the sketch and their retained charge once the operator produces
    /// no more rows, so that the operators above it have that memory for their remaining
    /// work. The counters stay for EXPLAIN, and a later batch evaluates the ordinary way.
    pub(super) fn release(&mut self) {
        self.decline();
    }

    /// The first batch: substantial exact repetition, or a repeated key with the
    /// snapshot hint, admits the map at once. Otherwise the sampled keys of the batch
    /// seed the sketch.
    fn pilot(&mut self, ctx: &Ctx, keys: &[Id], expr: &Expr) -> Result<bool> {
        // Cheap, mostly distinct inputs do not repay per-row map work.
        // The estimate affects profit only, never physical budget admission.
        let sample = keys.len().min(4096);
        let Ok(_pilot) = ctx.retained_charge(512 + sample as u64 * 32) else {
            self.decline();
            return Ok(false);
        };
        let mut seen = FxHashSet::with_capacity_and_hasher(sample, Default::default());
        let mut repeats = 0;
        for (row, id) in keys.iter().take(sample).enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            if !seen.insert(*id) {
                repeats += 1;
            }
        }
        let parallel_capable = expr.parallel() && rayon::current_num_threads() > 1;
        // A parallel-capable expression needs half its rows repeated to admit at once,
        // and otherwise waits for the cost guard.
        let floor = if parallel_capable { 2 } else { ADMIT as usize };
        let immediate = repeats > 0 && (repeats * floor >= sample || self.expected_reuse);
        if immediate {
            self.max_entries = MAP_ENTRIES;
            let reason = if repeats * 2 >= sample {
                "pilot_exact_half"
            } else if self.expected_reuse {
                "pilot_hint"
            } else {
                "pilot"
            };
            self.admit(reason);
            return Ok(true);
        }
        self.mode = Mode::Probe;
        self.max_entries = MAP_ENTRIES;
        drop(seen);
        // The pilot's own rows enter the sketch first and without a window, since their
        // repetition was counted exactly above and did not admit. The rest of the batch
        // then counts its repeats of pilot keys as well as of its own.
        let window = self.window;
        self.observe(ctx, &keys[..sample])?;
        self.window = window;
        self.observe(ctx, &keys[sample..])?;
        Ok(false)
    }

    /// Later batches: admit once a window of sampled rows repeats enough to pay. A sketch
    /// that fills without admitting marks itself exhausted, and the next batch declines.
    fn probe(&mut self, ctx: &Ctx, keys: &[Id], expr: &Expr) -> Result<bool> {
        let parallel_capable = expr.parallel() && rayon::current_num_threads() > 1;
        if keys.len() < crate::sparql::exprcache::MIN_ROWS && parallel_capable {
            // A small batch only feeds the sketch. Its rows stay out of the window, which
            // the cost guard judges by the size of the batch that completes it, and a
            // sketch it fills ends the probe like any other.
            let window = self.window;
            self.observe(ctx, keys)?;
            self.window = window;
            if self.sketch.len() >= SKETCH_KEYS {
                self.exhausted = true;
            }
            return Ok(false);
        }
        self.observe(ctx, keys)?;
        let (rows, repeats) = self.window;
        if rows < WINDOW {
            return Ok(false);
        }
        self.window = (0, 0);
        let rows = rows as usize;
        let repeats = repeats as usize;
        if !parallel_capable {
            if repeats * ADMIT as usize >= rows {
                self.admit("probe");
                return Ok(true);
            }
            if self.sketch.len() >= SKETCH_KEYS {
                self.exhausted = true;
            }
            return Ok(false);
        }
        let decision = self.weak_decision(rows, repeats, keys.len(), true);
        let admit_now = matches!(decision, WeakDecision::FastAdmit)
            || (matches!(decision, WeakDecision::Pass)
                && self.weak_confirmations >= WEAK_CONFIRMATIONS - 1);
        if admit_now {
            let reason = if matches!(decision, WeakDecision::FastAdmit)
                && self.weak_confirmations < WEAK_CONFIRMATIONS - 1
            {
                "probe_weak_fast"
            } else {
                "probe_weak"
            };
            self.admit(reason);
            return Ok(true);
        }
        self.batch_probe_window = Some((rows, repeats, keys.len()));
        if self.sketch.len() >= SKETCH_KEYS {
            self.exhausted = true;
        }
        Ok(false)
    }

    fn observe(&mut self, ctx: &Ctx, keys: &[Id]) -> Result<()> {
        let (mut rows, mut repeats) = self.window;
        for (row, id) in keys.iter().enumerate() {
            if row.is_multiple_of(4096) {
                ctx.check()?;
            }
            if id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SAMPLE_SHIFT) != 0 {
                continue;
            }
            rows += 1;
            if self.sketch.contains(id) {
                repeats += 1;
            } else if self.sketch.len() < SKETCH_KEYS
                && self.charge.as_mut().is_some_and(|charge| {
                    charge
                        .cover(
                            512 + (self.sketch.len() as u64 + 1) * SKETCH_BYTES,
                            ctx.reservation_step(),
                        )
                        .is_ok()
                })
            {
                self.sketch.insert(*id);
            }
        }
        self.window = (rows, repeats);
        Ok(())
    }

    /// Keep the evaluated values of a batch's pending entries while the map is below
    /// its cap and its retained charge covers them, and remove the rest. A removal
    /// rebuilds the table at the size of its survivors, without tombstones, so that its
    /// allocation stays within the retained charge once the batch's scratch is released.
    fn retain(&mut self, ctx: &Ctx, keys: &[Id], values: &[Id]) {
        let step = ctx.reservation_step();
        // Entries the map held before this batch, which the charge already covers.
        let mut held = self.values.len() - keys.len();
        let mut room = true;
        for (key, value) in keys.iter().zip(values) {
            room = room
                && held < self.max_entries
                && self.charge.as_mut().is_some_and(|charge| {
                    charge
                        .cover(512 + (held as u64 + 1) * ENTRY_BYTES, step)
                        .is_ok()
                });
            if room {
                held += 1;
                if let Some(entry) = self.values.get_mut(key) {
                    *entry = *value;
                }
            } else {
                self.values.remove(key);
            }
        }
        if !room {
            // Rebuild rather than shrink. `shrink_to_fit` keeps a table whose bucket count
            // the survivors still need, with the removed entries' tombstones, and a later
            // insertion could then grow it past the size its live entries justify.
            let old = std::mem::take(&mut self.values);
            self.values = FxHashMap::with_capacity_and_hasher(old.len(), Default::default());
            self.values.extend(old);
        }
    }

    /// The map pass of [`Reuse::apply_map`]. Hits go to `output`. A miss gets a pending
    /// slot in `output` and its row goes to `missed`. Below the entry cap the miss also
    /// enters the map with that slot, so the batch's later rows of the key need one
    /// lookup, and the batch's scratch charge covers these entries until retention is
    /// decided. A full map keeps the pending slots in a side map instead, since entries
    /// it inserted and then removed would leave tombstones that force rehashes. This
    /// returns whether the pending slots entered the map.
    fn answer(
        &mut self,
        ctx: &Ctx,
        keys: &[Id],
        output: &mut Vec<Id>,
        missed: &mut Vec<u32>,
        distinct: &mut Vec<Id>,
    ) -> Result<bool> {
        let mut slots: Option<FxHashMap<Id, Id>> =
            (self.values.len() >= self.max_entries).then(FxHashMap::default);
        for (row, key) in keys.iter().enumerate() {
            if row.is_multiple_of(4096) {
                ctx.check()?;
            }
            if let Some(value) = self.values.get(key) {
                if value.0 & !PAYLOAD_MASK == PENDING {
                    missed.push(row as u32);
                }
                output.push(*value);
                continue;
            }
            let next = Id(PENDING | distinct.len() as u64);
            let slot = match &mut slots {
                Some(slots) => *slots.entry(*key).or_insert(next),
                None => {
                    self.values.insert(*key, next);
                    next
                }
            };
            if slot == next {
                distinct.push(*key);
            }
            missed.push(row as u32);
            output.push(slot);
        }
        Ok(slots.is_none())
    }

    /// Answer the batch from the map. Misses are evaluated once per distinct key, as one
    /// column through the ordinary batch evaluation, and retained while the map has room.
    fn apply_map(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        column: usize,
        target: VarId,
        expr: &Expr,
    ) -> Result<bool> {
        let n = table.len;
        // The batch's transient memory, beside the input. The output, the missed rows,
        // the distinct misses and their values take 28 bytes a row at their exact
        // capacities. Pending slots take at most 39 bytes a row of hash table, since a
        // table of `c` entries has at most `2c * 8 / 7` buckets of 17 bytes, whether
        // they grow the map or fill the side map. The map's retained charge of
        // `ENTRY_BYTES` an entry covers its existing entries in both the old and the new
        // table while the table grows or shrinks.
        let Ok(scratch) = ctx.charge(n as u64 * SCRATCH_BYTES + 256) else {
            // Optional state yields its memory to the ordinary evaluation of the batch.
            self.decline();
            return Ok(false);
        };
        let mut output = Vec::with_capacity(n);
        let mut missed: Vec<u32> = Vec::with_capacity(n);
        let mut distinct = Vec::with_capacity(n);
        // Every pending entry is resolved before this returns, and an error releases the
        // whole map.
        let pending = match self.answer(
            ctx,
            &table.cols[column],
            &mut output,
            &mut missed,
            &mut distinct,
        ) {
            Ok(pending) => pending,
            Err(error) => {
                self.decline();
                return Err(error);
            }
        };
        let evaluated = distinct.len();
        if !distinct.is_empty() {
            #[cfg(test)]
            {
                self.evaluations += distinct.len();
            }
            let mut misses = Table::new(vec![self.input]);
            misses.len = distinct.len();
            misses.cols[0] = distinct;
            let multi_worker = rayon::current_num_threads() > 1;
            let split_policy = self.map_split_policy(ctx, &misses.cols[0], multi_worker);
            let par = match split_policy {
                MapSplitPolicy::Iri(bucket) => self.iri_split.allow(bucket),
                MapSplitPolicy::Default => self.allow_map_split(misses.len(), expr),
            };
            // A refusal of memory that the evaluation of the misses meets while the map
            // and the batch's scratch hold their charges does not fail the query. This
            // covers the result and the terms, decode envelopes and regular expressions
            // that the evaluation interns or grows on its way to it, none of which
            // records a refusal for the query. After the refusal the expression gives up
            // as it would after a latched one, so it takes no arm that the ordinary
            // evaluation skips and interns no discarded value. The misses that finished
            // keep their values, and the rest of the batch is answered by
            // `finish_refused` (retrying an unfinished row that interned an intermediate
            // term after an earlier transient allocation before refusing is tracked in
            // roadmap item 1322). Geometry constructors, whose charge lasts until the
            // query ends, never reach this pass (see `prepare`).
            let values =
                match exec::column_rows_split_optional(ctx, &misses, expr, par, UNEVALUATED)
                    .and_then(|values| ctx.check().map(|()| values))
                {
                    Ok(OptionalColumn::Refused(values)) => {
                        // The map, its pending entries and the batch's scratch go before any
                        // row is evaluated again, so that evaluation has the memory it would
                        // have without the map.
                        drop(misses);
                        drop(missed);
                        self.decline();
                        drop(scratch);
                        return self.finish_refused(ctx, table, target, expr, output, values);
                    }
                    Ok(OptionalColumn::Complete(values, split)) => {
                        #[cfg(test)]
                        {
                            self.last_miss_dispatch = Some((par, split));
                        }
                        match split_policy {
                            MapSplitPolicy::Iri(bucket) => {
                                self.iri_split.observe(bucket, par, split);
                            }
                            MapSplitPolicy::Default => {
                                self.observe_map_split(misses.len(), expr, par, split)
                            }
                        }
                        values
                    }
                    Err(error) => {
                        self.decline();
                        // Another refusal of memory that nothing latched, such as a charge of
                        // the evaluation beside the map, yields to ordinary evaluation too.
                        // Such a charge is taken before any miss is evaluated, so the
                        // ordinary evaluation of the whole batch repeats no work. After
                        // that, the evaluation fails only through the checks, which this
                        // returns.
                        ctx.check()?;
                        if crate::sparql::ctx::is_memory_refusal(&error) {
                            return Ok(false);
                        }
                        return Err(error);
                    }
                };
            for row in &missed {
                let at = &mut output[*row as usize];
                *at = values[(at.0 & PAYLOAD_MASK) as usize];
            }
            if pending {
                self.retain(ctx, &misses.cols[0], &values);
            }
            self.misses += misses.len as u64;
        }
        // Rows that needed no evaluation of their own.
        let answered = n - evaluated;
        self.hits += answered as u64;
        table.vars.push(target);
        table.cols.push(output);
        self.map_window.0 += n as u32;
        self.map_window.1 += answered as u32;
        self.map_rows += n as u64;
        if self.map_window.0 >= MAP_WINDOW
            && (self.map_rows >= MAP_ENTRIES as u64 || self.values.len() >= self.max_entries)
        {
            let (rows, answered) = std::mem::take(&mut self.map_window);
            // A map that answers fewer than one row in `2 * ADMIT` costs more than it saves.
            if answered * 2 * ADMIT < rows {
                self.decline();
            }
        }
        Ok(true)
    }

    /// Answer a batch whose misses met a refusal of memory, once the map and the batch's
    /// scratch are released. `output` holds the answers of the rows that the map held
    /// before the batch and the pending slot of every other row, and `values` holds the
    /// value of each slot whose miss finished or [`UNEVALUATED`]. A finished miss is not
    /// evaluated again, so the transient memory of its evaluation, such as a large decode,
    /// is not met a second time once later rows' results are retained. Each other slot is
    /// evaluated the ordinary way at its first row, in row order, beside the finished
    /// values and any intermediate term its refused attempt interned (roadmap item 1322),
    /// and a refusal now fails the query. On one worker the finished values are those of
    /// the earlier rows, which the evaluation without the map also retains there. On
    /// several they can include later rows. The two vectors stay within the cursor's
    /// scratch and output charges of the batch.
    fn finish_refused(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        target: VarId,
        expr: &Expr,
        mut output: Vec<Id>,
        mut values: Vec<Id>,
    ) -> Result<bool> {
        let n = table.len;
        #[cfg(test)]
        let finished = values.iter().filter(|v| **v != UNEVALUATED).count();
        #[cfg(test)]
        let mut evaluated = 0;
        exec::with_row_eval(ctx, table, expr, |eval_row| {
            for (row, at) in output.iter_mut().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                if at.0 & !PAYLOAD_MASK != PENDING {
                    continue;
                }
                let value = &mut values[(at.0 & PAYLOAD_MASK) as usize];
                if *value == UNEVALUATED {
                    *value = eval_row(row);
                    #[cfg(test)]
                    {
                        evaluated += 1;
                    }
                }
                *at = *value;
            }
            Ok(())
        })?;
        ctx.check()?;
        #[cfg(test)]
        {
            self.refusals.push((finished, evaluated));
        }
        // Each distinct miss was evaluated once, by the map or after it.
        self.misses += values.len() as u64;
        self.hits += (n - values.len()) as u64;
        drop(values);
        table.vars.push(target);
        table.cols.push(output);
        Ok(true)
    }
}

/// Whether `expr` is a direct `STR(input)` call on `input`.
fn is_direct_str_input(expr: &Expr, input: VarId) -> bool {
    matches!(
        expr,
        Expr::Call(Func::Builtin(Function::Str), args)
            if matches!(args.as_slice(), [Expr::Var(var)] if *var == input)
    )
}

/// Whether every key in `keys` is a base-vocabulary IRI (short-circuiting on the first
/// literal, triple term, inline literal, blank node, delta, local or undefined term).
fn all_stored_iris(ctx: &Ctx, keys: &[Id]) -> bool {
    if keys.is_empty() {
        return false;
    }
    let vocab = &ctx.snap.generation.vocab;
    keys.iter()
        .all(|id| id.tag() == Tag::Vocab && vocab.is_iri(id.payload()))
}

/// Whether a batch of inline integers spans few enough values for the integer map, or
/// `None` when the batch holds another kind of term.
fn dense_integers(ctx: &Ctx, keys: &[Id]) -> Result<Option<bool>> {
    let (mut lo, mut hi) = (u64::MAX, 0);
    for (row, id) in keys.iter().enumerate() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        if id.tag() != Tag::Int {
            return Ok(None);
        }
        lo = lo.min(id.payload());
        hi = hi.max(id.payload());
    }
    Ok(Some(
        hi.saturating_sub(lo) < (keys.len() / 2).min(65536) as u64,
    ))
}

/// A bounded profitability hint, not a resource or uniqueness proof. Unknown
/// shapes and changed snapshots decline the hint; dense observed reuse remains
/// eligible without statistics. Inner joins cannot add new values to a bound key.
fn distinct_bound(ctx: &Ctx, node: &Node, input: VarId, left: &mut usize) -> Option<u64> {
    if *left == 0 || !ctx.snap.delta.is_empty() {
        return None;
    }
    *left -= 1;
    match &node.kind {
        Kind::Scan(scan) if scan.cols.len() <= 4 && scan.prefix.len() <= 4 => {
            let order = scan.perm.order();
            let pcol = order.iter().position(|col| *col == P)?;
            let predicate = *scan.prefix.get(pcol)?;
            ctx.snap.predicate_count(Perm::Pso, &[predicate])?;
            let stats = ctx.snap.generation.stats.predicate(predicate)?;
            let (col, _) = scan.cols.iter().find(|(_, var)| *var == input)?;
            let distinct = match *order.get(*col)? {
                S => stats.distinct_subjects,
                O => stats.distinct_objects,
                _ => return None,
            };
            (distinct > 0 && distinct <= stats.count).then_some(distinct)
        }
        Kind::Join { .. } if node.children.len() == 2 => {
            let a = distinct_bound(ctx, &node.children[0], input, left);
            let b = distinct_bound(ctx, &node.children[1], input, left);
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Kind::Project(vars)
            if vars.len() <= 64 && vars.contains(&input) && node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        Kind::Filter(_)
        | Kind::Sort(_)
        | Kind::OrderBy { .. }
        | Kind::Slice { .. }
        | Kind::Distinct
            if node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        Kind::Extend(var, _) | Kind::Assign(var, _)
            if *var != input && node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Source;
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, Term, vocab::xsd};
    use oxrdfio::RdfFormat;

    fn new_reuse(ctx: &Ctx, expr: &Expr, source: Option<&Node>) -> Option<Reuse> {
        Reuse::new(ctx, Reuse::prepare(ctx, expr, source)?)
    }

    /// [`new_reuse`] with a split cooldown of `cooldown` evaluations.
    fn new_reuse_cooldown(ctx: &Ctx, expr: &Expr, cooldown: u8) -> Reuse {
        let mut reuse = new_reuse(ctx, expr, None).unwrap();
        reuse.split = SplitController::new(cooldown);
        reuse
    }

    impl Reuse {
        fn counted(&mut self, rows: usize, distinct: usize) {
            self.observe_fallback_with(
                rows,
                ColumnEval {
                    evaluated_rows: rows,
                    elapsed_nanos: None,
                    split: None,
                    parallel_allowed: true,
                },
                Some((rows, distinct)),
                true,
                true,
            );
        }
    }

    fn split(sample_nanos: u128, rest_nanos: u128) -> Split {
        Split {
            sample_rows: 256,
            sample_nanos,
            rest_rows: 1024,
            rest_nanos,
        }
    }

    /// A split pays only when the pool maps the rest faster than the calling thread would
    /// at the rate of its timed sample. The times are given, so no clock decides the test.
    #[test]
    fn a_split_pays_only_when_the_pool_beats_the_sampled_rate() {
        // the sample's rate projects 400 us for the rest
        assert_eq!(split(100_000, 120_000).valid_paid(), Some(true));
        assert_eq!(
            split(100_000, 400_000).valid_paid(),
            Some(false),
            "equal to the projection"
        );
        assert_eq!(
            split(100_000, 1_200_000).valid_paid(),
            Some(false),
            "contended evaluation"
        );
        assert_eq!(split(0, 10).valid_paid(), None);
    }

    #[test]
    fn one_split_loss_then_win_keeps_parallelism_available() {
        let mut ctrl = SplitController::new(SPLIT_COOLDOWN);
        for _ in 0..2 {
            assert!(ctrl.allow(4096, true, true));
            assert_eq!(
                ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000))),
                None
            );
            assert_eq!(ctrl.large, BucketPhase::Active { losses: 1 });
            assert!(
                ctrl.allow(4096, true, true),
                "one noisy split loss must not disable parallelism"
            );
            assert_eq!(
                ctrl.observe(4096, true, true, true, Some(split(100_000, 120_000))),
                None
            );
            assert_eq!(ctrl.large, BucketPhase::Active { losses: 0 });
            assert!(ctrl.allow(4096, true, true));
        }
    }

    #[test]
    fn consecutive_split_losses_pause_exactly_two_eligible_batches() {
        for cooldown in [2u8, SPLIT_COOLDOWN] {
            let mut ctrl = SplitController::new(cooldown);
            // Two consecutive clear losses enter cooldown.
            assert!(ctrl.allow(4096, true, true));
            ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
            assert!(ctrl.allow(4096, true, true));
            assert_eq!(
                ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000))),
                Some("active->cooldown")
            );
            assert_eq!(
                ctrl.large,
                BucketPhase::Cooldown {
                    remaining: cooldown
                }
            );

            // Repeat several cycles to exclude wraparound or terminal off state.
            for _ in 0..4 {
                for step in 0..cooldown {
                    assert!(
                        !ctrl.allow(4096, true, true),
                        "cooldown step {step} of {cooldown} must suppress splitting"
                    );
                    ctrl.observe(4096, true, true, false, None);
                }
                assert_eq!(ctrl.large, BucketPhase::Retry);
                assert!(
                    ctrl.allow(4096, true, true),
                    "parallelism must recover after cooldown"
                );
                // A clearly losing retry immediately restarts cooldown on one probe.
                assert_eq!(
                    ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000))),
                    Some("retry->cooldown")
                );
                assert_eq!(
                    ctrl.large,
                    BucketPhase::Cooldown {
                        remaining: cooldown
                    }
                );
            }

            // Drain cooldown once more and recover with a clear win.
            for _ in 0..cooldown {
                assert!(!ctrl.allow(4096, true, true));
                ctrl.observe(4096, true, true, false, None);
            }
            assert!(ctrl.allow(4096, true, true));
            assert_eq!(
                ctrl.observe(4096, true, true, true, Some(split(100_000, 120_000))),
                Some("retry->active")
            );
            assert_eq!(ctrl.large, BucketPhase::Active { losses: 0 });

            // After a winning retry, one isolated loss keeps splitting allowed.
            ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
            assert_eq!(ctrl.large, BucketPhase::Active { losses: 1 });
            assert!(ctrl.allow(4096, true, true));
        }
    }

    #[test]
    fn split_margin_and_missing_evidence_do_not_masquerade_as_losses() {
        // Sample rate projects 400,000 ns for 1,024 rest rows.
        // Clear win: <= 90% (360,000 ns); Clear loss: >= 120% (480,000 ns).
        assert_eq!(split(100_000, 360_000).classify(), SplitOutcome::ClearWin);
        assert_eq!(
            split(100_000, 360_001).classify(),
            SplitOutcome::Inconclusive
        );
        assert_eq!(
            split(100_000, 400_000).classify(),
            SplitOutcome::Inconclusive
        );
        assert_eq!(
            split(100_000, 479_999).classify(),
            SplitOutcome::Inconclusive
        );
        assert_eq!(split(100_000, 480_000).classify(), SplitOutcome::ClearLoss);
        assert_eq!(split(100_000, 480_001).classify(), SplitOutcome::ClearLoss);

        // Unequal row counts: 256 sample, 512 rest -> projected 200,000 ns.
        let s_win = Split {
            sample_rows: 256,
            sample_nanos: 100_000,
            rest_rows: 512,
            rest_nanos: 180_000,
        };
        let s_mid = Split {
            rest_nanos: 239_999,
            ..s_win
        };
        let s_loss = Split {
            rest_nanos: 240_000,
            ..s_win
        };
        assert_eq!(s_win.classify(), SplitOutcome::ClearWin);
        assert_eq!(s_mid.classify(), SplitOutcome::Inconclusive);
        assert_eq!(s_loss.classify(), SplitOutcome::ClearLoss);

        // Zero or overflowing inputs are unknown and never trigger cooldown.
        for invalid in [
            Split {
                sample_rows: 0,
                sample_nanos: 100_000,
                rest_rows: 1024,
                rest_nanos: 1_000_000,
            },
            Split {
                sample_rows: 256,
                sample_nanos: 0,
                rest_rows: 1024,
                rest_nanos: 1_000_000,
            },
            Split {
                sample_rows: 256,
                sample_nanos: 100_000,
                rest_rows: 0,
                rest_nanos: 1_000_000,
            },
            Split {
                sample_rows: 256,
                sample_nanos: 100_000,
                rest_rows: 1024,
                rest_nanos: 0,
            },
            Split {
                sample_rows: 256,
                sample_nanos: u128::MAX,
                rest_rows: 1024,
                rest_nanos: u128::MAX,
            },
        ] {
            assert_eq!(invalid.classify(), SplitOutcome::Unknown);
        }

        // Duration::MAX-derived reachable bounds classify without overflow.
        let max_ns = std::time::Duration::MAX.as_nanos();
        assert_eq!(
            Split {
                sample_rows: 256,
                sample_nanos: max_ns,
                rest_rows: 16_128,
                rest_nanos: max_ns,
            }
            .classify(),
            SplitOutcome::ClearWin
        );
        assert_eq!(
            Split {
                sample_rows: 256,
                sample_nanos: max_ns / 100,
                rest_rows: 16_128,
                rest_nanos: max_ns,
            }
            .classify(),
            SplitOutcome::ClearLoss
        );

        // None preserves single loss; Unknown preserves single loss; Inconclusive resets it.
        let mut ctrl = SplitController::new(2);
        ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
        assert_eq!(ctrl.large, BucketPhase::Active { losses: 1 });
        ctrl.observe(4096, true, true, true, None);
        assert_eq!(ctrl.large, BucketPhase::Active { losses: 1 });
        ctrl.observe(4096, true, true, true, Some(split(0, 1_200_000)));
        assert_eq!(ctrl.large, BucketPhase::Active { losses: 1 });
        ctrl.observe(4096, true, true, true, Some(split(100_000, 410_000)));
        assert_eq!(ctrl.large, BucketPhase::Active { losses: 0 });

        // In Retry, None and Inconclusive keep Retry pending without re-entering cooldown.
        ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
        ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
        ctrl.observe(4096, true, true, false, None);
        ctrl.observe(4096, true, true, false, None);
        assert_eq!(ctrl.large, BucketPhase::Retry);
        ctrl.observe(4096, true, true, true, None);
        assert_eq!(ctrl.large, BucketPhase::Retry);
        ctrl.observe(4096, true, true, true, Some(split(100_000, 400_000)));
        assert_eq!(ctrl.large, BucketPhase::Retry);
        assert!(ctrl.allow(4096, true, true));
    }

    #[test]
    fn ineligible_miss_batches_do_not_consume_split_cooldown() {
        let mut ctrl = SplitController::new(2);
        // Enter cooldown in the Large bucket (2048..=16384).
        ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
        ctrl.observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
        assert_eq!(ctrl.large, BucketPhase::Cooldown { remaining: 2 });

        // Ineligible sizes, one-worker, non-parallel and Small-bucket calls leave Large intact.
        for rows in [0, 1, 767, 20_000] {
            assert!(ctrl.allow(rows, true, true));
            ctrl.observe(rows, true, true, false, None);
        }
        ctrl.observe(4096, false, true, false, None);
        ctrl.observe(4096, true, false, false, None);
        assert_eq!(ctrl.large, BucketPhase::Cooldown { remaining: 2 });

        // Two-bucket isolation: Small bucket (768..=2047) remains enabled and independent.
        assert!(ctrl.allow(768, true, true));
        assert!(ctrl.allow(2047, true, true));
        ctrl.observe(768, true, true, true, Some(split(100_000, 1_200_000)));
        ctrl.observe(2047, true, true, true, Some(split(100_000, 1_200_000)));
        assert_eq!(ctrl.small, BucketPhase::Cooldown { remaining: 2 });
        assert_eq!(ctrl.large, BucketPhase::Cooldown { remaining: 2 });

        // Exact Large boundaries (2048 and 16384) consume Large cooldown without touching Small.
        assert!(!ctrl.allow(2048, true, true));
        ctrl.observe(2048, true, true, false, None);
        assert_eq!(ctrl.large, BucketPhase::Cooldown { remaining: 1 });
        assert_eq!(ctrl.small, BucketPhase::Cooldown { remaining: 2 });
        assert!(!ctrl.allow(exec::PAR_THRESHOLD, true, true));
        ctrl.observe(exec::PAR_THRESHOLD, true, true, false, None);
        assert_eq!(ctrl.large, BucketPhase::Retry);
        assert!(ctrl.allow(4096, true, true));
        assert!(!ctrl.allow(1024, true, true), "small bucket stays isolated");
    }

    #[test]
    fn cooldown_evaluates_later_map_misses_without_a_pool_split() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut reuse = new_reuse_cooldown(&ctx, &expr, 2);
            // Admit map with a dense first batch (>= 1/2 exact repeat).
            let seed_dates: Vec<Id> = (1900..1932)
                .map(|y| {
                    ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                        format!("{y}-01-01+00:00"),
                        xsd::DATE,
                    )))
                })
                .collect();
            let mut first = Table::new(vec![input]);
            first.cols[0] = (0..4096)
                .map(|i| seed_dates[i % seed_dates.len()])
                .collect();
            first.len = 4096;
            assert!(reuse.apply(&ctx, &mut first, target, &expr).unwrap());
            assert_eq!(reuse.mode, Mode::Map);

            // Seed two synthetic clear losses in the Small bucket (1,024 distinct misses).
            reuse.observe_map_split(1024, &expr, true, Some(split(100_000, 1_200_000)));
            reuse.observe_map_split(1024, &expr, true, Some(split(100_000, 1_200_000)));
            assert_eq!(reuse.split.small, BucketPhase::Cooldown { remaining: 2 });

            let make_batch = |base_year: usize| -> Table {
                let keys: Vec<Id> = (0..1024)
                    .map(|k| {
                        ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                            format!("{:04}-01-01+00:00", base_year + k),
                            xsd::DATE,
                        )))
                    })
                    .collect();
                let mut t = Table::new(vec![input]);
                t.cols[0] = (0..4096).map(|i| keys[i % 1024]).collect();
                t.len = 4096;
                t
            };

            // Cooldown step 1: 1,024 distinct misses evaluated with par = false.
            let mut b1 = make_batch(3000);
            let expected1 = exec::column_rows(&ctx, &b1, &expr).unwrap();
            let before = reuse.evaluations;
            assert!(reuse.apply(&ctx, &mut b1, target, &expr).unwrap());
            assert_eq!(b1.cols[1], expected1);
            assert_eq!(reuse.evaluations - before, 1024);
            assert_eq!(pending_entries(&reuse), 0);
            assert_eq!(reuse.last_miss_dispatch, Some((false, None)));
            assert_eq!(reuse.split.small, BucketPhase::Cooldown { remaining: 1 });

            // Cooldown step 2: consumes final step and transitions to Retry.
            let mut b2 = make_batch(4500);
            let expected2 = exec::column_rows(&ctx, &b2, &expr).unwrap();
            assert!(reuse.apply(&ctx, &mut b2, target, &expr).unwrap());
            assert_eq!(b2.cols[1], expected2);
            assert_eq!(pending_entries(&reuse), 0);
            assert_eq!(reuse.last_miss_dispatch, Some((false, None)));
            assert_eq!(reuse.split.small, BucketPhase::Retry);

            // Retry: actual production dispatch now passes par = true.
            let mut b3 = make_batch(6000);
            assert!(reuse.apply(&ctx, &mut b3, target, &expr).unwrap());
            let (par, retried) = reuse.last_miss_dispatch.unwrap();
            assert!(par, "cooldown expiry must permit a split retry");
            // The retry's real split decides by its own timing, which a loaded host can
            // make a loss.
            let expected = match retried.map(|retried| retried.classify()) {
                Some(SplitOutcome::ClearWin) => BucketPhase::Active { losses: 0 },
                Some(SplitOutcome::ClearLoss) => BucketPhase::Cooldown {
                    remaining: reuse.split.cooldown,
                },
                _ => BucketPhase::Retry,
            };
            assert_eq!(reuse.split.small, expected);

            // Inject synthetic winning observation for deterministic recovery.
            reuse.split.small = BucketPhase::Retry;
            reuse.observe_map_split(1024, &expr, true, Some(split(100_000, 120_000)));
            assert_eq!(reuse.split.small, BucketPhase::Active { losses: 0 });
        });
    }

    #[test]
    fn controlled_fallback_reports_dedup_and_row_counts_and_shares_controller_across_map_and_decline()
     {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut reuse = new_reuse_cooldown(&ctx, &expr, 2);
            let dates: Vec<Id> = (2000..6096)
                .map(|y| {
                    ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                        format!("{y}-01-01+00:00"),
                        xsd::DATE,
                    )))
                })
                .collect();

            // 1. Declined per-batch dedup: 4,096 distinct keys -> evaluated_rows == 4,096.
            let unique_table = batch(input, &dates[..4096]);
            let mut report = crate::sparql::exprcache::Report::default();
            let (col, eval) = exec::compute_column_controlled(
                &ctx,
                &unique_table,
                &expr,
                &mut report,
                |n| reuse.allow_fallback_split(n, &expr),
                |n| reuse.want_cost_timing(n, &expr),
            )
            .unwrap();
            assert_eq!(col.len(), 4096);
            assert_eq!(eval.evaluated_rows, 4096);
            assert_eq!(report.counted(), Some((4096, 4096)));

            // 2. Successful per-batch dedup: 1,024 distinct of 4,096 rows ->
            //    evaluated_rows == 1,024 while Report::counted keeps (4096, 1024).
            let dedup_keys: Vec<Id> = (0..4096).map(|i| dates[i % 1024]).collect();
            let dedup_table = batch(input, &dedup_keys);
            let mut report = crate::sparql::exprcache::Report::default();
            let (col, eval) = exec::compute_column_controlled(
                &ctx,
                &dedup_table,
                &expr,
                &mut report,
                |n| reuse.allow_fallback_split(n, &expr),
                |n| reuse.want_cost_timing(n, &expr),
            )
            .unwrap();
            assert_eq!(col, exec::column_rows(&ctx, &dedup_table, &expr).unwrap());
            assert_eq!(eval.evaluated_rows, 1024);
            assert_eq!(report.counted(), Some((4096, 1024)));

            // 3. Fallback losses enter cooldown in Large bucket and carry over into Map.
            let loss_eval = ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: Some(4096 * 1_000),
                split: Some(split(100_000, 1_200_000)),
                parallel_allowed: true,
            };
            reuse.observe_fallback(4096, loss_eval, None, &expr);
            reuse.observe_fallback(4096, loss_eval, None, &expr);
            assert_eq!(reuse.split.large, BucketPhase::Cooldown { remaining: 2 });
            assert!(!reuse.allow_fallback_split(4096, &expr));

            // Admit map: Large-bucket misses immediately inherit cooldown (no relearning).
            reuse.admit("test");
            let mut miss_batch = batch(input, &dates[..3000]);
            assert!(reuse.apply(&ctx, &mut miss_batch, target, &expr).unwrap());
            assert_eq!(reuse.last_miss_dispatch, Some((false, None)));
            assert_eq!(reuse.split.large, BucketPhase::Cooldown { remaining: 1 });

            // 4. Decline to Mode::Off preserves the shared controller for fallback.
            reuse.decline();
            assert_eq!(reuse.mode, Mode::Off);
            assert_eq!(reuse.split.large, BucketPhase::Cooldown { remaining: 1 });
            assert!(!reuse.allow_fallback_split(4096, &expr));
            let mut report = crate::sparql::exprcache::Report::default();
            let (_, eval) = exec::compute_column_controlled(
                &ctx,
                &unique_table,
                &expr,
                &mut report,
                |n| reuse.allow_fallback_split(n, &expr),
                |n| reuse.want_cost_timing(n, &expr),
            )
            .unwrap();
            assert!(!eval.parallel_allowed);
            assert!(eval.elapsed_nanos.is_none(), "Off must not time fallback");
            reuse.observe_fallback(4096, eval, report.counted(), &expr);
            assert_eq!(reuse.last_fallback_dispatch, Some((4096, false, None)));
            assert_eq!(reuse.split.large, BucketPhase::Retry);
            assert!(reuse.allow_fallback_split(4096, &expr));
        });
    }

    #[test]
    fn cost_guard_rational_threshold_and_confirmation_rules() {
        // Direct rational arithmetic checks around 50 ns/row saving.
        let mut hist = CostHistory::default();
        assert!(hist.lower_rate(CostBucket::Large).is_none());
        // Single slow outlier (2,000 ns/row) alone is not enough.
        hist.record(4096, 4096 * 2_000);
        assert!(hist.lower_rate(CostBucket::Large).is_none());
        // Second observation at 120 ns/row selects the lower rate (120 ns/row).
        hist.record(4096, 4096 * 120);
        let low = hist.lower_rate(CostBucket::Large).unwrap();
        assert!(!low.meets_saving(4096, 512), "1/8 * 120 = 15 ns < 50 ns");
        assert!(low.meets_saving(4096, 2048), "1/2 * 120 = 60 ns >= 50 ns");

        // Exact 50 ns boundary at 1/8 repeat: 399 ns fails, 400 ns and 480 ns pass.
        let mut boundary = CostHistory::default();
        boundary.record(4096, 4096 * 399);
        boundary.record(4096, 4096 * 500);
        assert!(
            !boundary
                .lower_rate(CostBucket::Large)
                .unwrap()
                .meets_saving(4096, 512)
        );
        boundary.record(4096, 4096 * 400);
        assert!(
            boundary
                .lower_rate(CostBucket::Large)
                .unwrap()
                .meets_saving(4096, 512)
        );
        boundary.record(4096, 4096 * 480);
        assert!(
            boundary
                .lower_rate(CostBucket::Large)
                .unwrap()
                .meets_saving(4096, 512)
        );

        // Changing evaluation size bucket requires two new observations.
        boundary.record(1024, 1024 * 500);
        assert!(boundary.lower_rate(CostBucket::Large).is_none());
        assert!(boundary.lower_rate(CostBucket::Small).is_none());
        boundary.record(1024, 1024 * 500);
        assert!(
            boundary
                .lower_rate(CostBucket::Small)
                .unwrap()
                .meets_saving(4096, 512)
        );

        // Reuse policy checks: counted cannot bypass cost guard or double-confirm one batch.
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        reuse.mode = Mode::Probe;

        // Cheap parallel observations (120 ns/row): counted at 1/8 (512 repeats) must not admit.
        let cheap_eval = ColumnEval {
            evaluated_rows: 4096,
            elapsed_nanos: Some(4096 * 120),
            split: None,
            parallel_allowed: true,
        };
        reuse.observe_fallback_with(4096, cheap_eval, Some((4096, 3584)), true, true);
        reuse.observe_fallback_with(4096, cheap_eval, Some((4096, 3584)), true, true);
        reuse.observe_fallback_with(4096, cheap_eval, Some((4096, 3584)), true, true);
        assert_eq!(reuse.mode, Mode::Probe);
        assert_eq!(reuse.weak_confirmations, 0);

        // Expensive parallel observations (480 ns/row): probe + counted on the SAME batch
        // must count as at most one confirmation, not two. Both signals repeat one row in
        // eight, a 60 ns/row saving, below the 100 ns that admits on one decision.
        let expensive_eval = ColumnEval {
            evaluated_rows: 4096,
            elapsed_nanos: Some(4096 * 480),
            split: None,
            parallel_allowed: true,
        };
        reuse.cost.record(4096, 4096 * 480);
        reuse.cost.record(4096, 4096 * 480);
        reuse.batch_probe_window = Some((256, 32, 4096));
        reuse.observe_fallback_with(4096, expensive_eval, Some((4096, 3584)), true, true);
        assert_eq!(
            reuse.mode,
            Mode::Probe,
            "probe and counted on the same batch must not double-confirm"
        );
        assert_eq!(reuse.weak_confirmations, 1);

        // Skipped tiny batch (no probe window, no counted) preserves weak_confirmations.
        let tiny_eval = ColumnEval {
            evaluated_rows: 128,
            elapsed_nanos: None,
            split: None,
            parallel_allowed: true,
        };
        reuse.observe_fallback_with(128, tiny_eval, None, true, true);
        assert_eq!(reuse.weak_confirmations, 1);

        // Second consecutive passing batch admits.
        reuse.observe_fallback_with(4096, expensive_eval, Some((4096, 3584)), true, true);
        assert_eq!(reuse.mode, Mode::Map);

        // Sampled historical >= 1/2 in Probe is NOT exempt from the cost guard (unknown cost
        // keeps it in Probe), whereas exact within-batch >= 1/2 IS exempt.
        let mut r2 = new_reuse(&ctx, &expr, None).unwrap();
        r2.mode = Mode::Probe;
        r2.batch_probe_window = Some((256, 200, 4096));
        r2.observe_fallback_with(
            4096,
            ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: None,
                split: None,
                parallel_allowed: true,
            },
            None,
            true,
            true,
        );
        assert_eq!(
            r2.mode,
            Mode::Probe,
            "sampled historical >= 1/2 must not bypass unknown cost"
        );
        r2.observe_fallback_with(
            4096,
            ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: None,
                split: None,
                parallel_allowed: true,
            },
            Some((4096, 2048)),
            true,
            true,
        );
        assert_eq!(
            r2.mode,
            Mode::Map,
            "exact within-batch >= 1/2 repeat admits immediately"
        );

        // Single-worker execution preserves the original 1/8 rule without cost observations.
        let mut r3 = new_reuse(&ctx, &expr, None).unwrap();
        r3.mode = Mode::Probe;
        r3.observe_fallback_with(
            4096,
            ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: None,
                split: None,
                parallel_allowed: true,
            },
            Some((4096, 3584)),
            true,
            false,
        );
        assert_eq!(r3.mode, Mode::Map);
    }

    #[test]
    fn cold_map_after_high_history_low_local_repeat_evaluates_all_uncached_keys() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        // Construct 4,096 distinct keys containing exactly WINDOW sampled keys so each
        // repeat deterministically completes one probe window.
        let pool = local_strings(&ctx, 0..16_384);
        let mut history: Vec<Id> = pool
            .iter()
            .copied()
            .filter(|id| sampled(*id))
            .take(WINDOW as usize)
            .collect();
        assert_eq!(history.len(), WINDOW as usize);
        history.extend(
            pool.iter()
                .copied()
                .filter(|id| !sampled(*id))
                .take(4096 - WINDOW as usize),
        );
        assert_eq!(history.len(), 4096);
        // Pilot seeds sketch with 4,096 unique keys.
        run(&ctx, &mut reuse, input, target, &expr, &history);
        assert_eq!(reuse.mode, Mode::Probe);
        // Two repeats of the historical keys confirm and admit the map.
        run(&ctx, &mut reuse, input, target, &expr, &history);
        run(&ctx, &mut reuse, input, target, &expr, &history);
        assert_eq!(reuse.mode, Mode::Map);
        // A cold batch of completely fresh uncached keys (no local repeats) must evaluate
        // every key and produce exact answers without phantom hits.
        let fresh = local_strings(&ctx, 50_000..54_096);
        let mut table = batch(input, &fresh);
        let expected = exec::column_rows(&ctx, &table, &expr).unwrap();
        let evals_before = reuse.evaluations;
        let hits_before = reuse.hits;
        assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(table.cols[1], expected);
        assert_eq!(reuse.evaluations - evals_before, 4096);
        assert_eq!(reuse.hits - hits_before, 0);
    }

    #[test]
    fn stored_temporal_bind_reuses_exact_ids_across_morsels() {
        let store = Store::in_memory(StoreOptions::default());
        let lexicals = (1950..1982)
            .map(|year| format!("{year}-01-01+00:00"))
            .collect::<Vec<_>>();
        let data = lexicals
            .iter()
            .enumerate()
            .map(|(i, date)| {
                format!(
                    "<urn:s{i}> <urn:p> \"{date}\"^^<http://www.w3.org/2001/XMLSchema#date> .\n"
                )
            })
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let mut ctx = Ctx::new(store.snapshot());
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let ids = lexicals
            .iter()
            .map(|date| {
                ctx.snap
                    .lookup_term(&Term::Literal(Literal::new_typed_literal(date, xsd::DATE)))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| id.tag() == Tag::Vocab));
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        for epoch in 0..3 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|i| ids[(i * 7 + epoch * 3) % ids.len()])
                .collect();
            table.len = 4096;
            assert!(
                reuse.apply(&ctx, &mut table, target, &expr).unwrap(),
                "stored temporal keys must reuse their expression results"
            );
            for row in 0..table.len {
                let index = (row * 7 + epoch * 3) % ids.len();
                assert_eq!(
                    ctx.term(table.cols[1][row]),
                    Some(Term::Literal(Literal::from(1950 + index as i64)))
                );
            }
        }
        assert_eq!(
            reuse.evaluations, 32,
            "each exact date ID is evaluated only once across three morsels"
        );
    }

    fn cursor_context(store: &Store, memory: u64) -> std::sync::Arc<Ctx> {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = memory;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        ctx
    }

    #[test]
    fn generic_reuse_keeps_optional_refusal_identity_and_effect_boundaries() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 10);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let date = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
            "2000-01-01+00:00",
            xsd::DATE,
        )));
        assert_eq!(date.tag(), Tag::Local);
        let mut table = Table::new(vec![input]);
        table.cols[0] = vec![date; 4096];
        table.len = 4096;
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
        assert_eq!(reuse.evaluations, 0);
        ctx.check().unwrap();
        assert!(
            new_reuse(&ctx, &Expr::Var(input), None).is_none(),
            "an identity copy does not need a map"
        );
        let effect = Expr::Arith(
            Box::new(Expr::Var(input)),
            Box::new(Expr::Call(Func::Builtin(Function::Rand), vec![])),
            crate::sparql::expr::ArithOp::Add,
        );
        assert!(new_reuse(&ctx, &effect, None).is_none());

        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let mut table = Table::new(vec![input]);
        table.cols[0] = (10000..14096)
            .map(|year| {
                ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                    format!("{year}-01-01+00:00"),
                    xsd::DATE,
                )))
            })
            .collect();
        table.len = 4096;
        // An all-distinct pilot does not retain results, but later skew that the
        // sketch observes admits the map on single-worker execution without extra clocks.
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        single.install(|| {
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            let mut pilot = Table::new(vec![input]);
            pilot.cols[0] = table.cols[0].clone();
            pilot.len = table.len;
            assert!(!reuse.apply(&ctx, &mut pilot, target, &expr).unwrap());
            assert!(reuse.values.is_empty());
            assert_eq!(reuse.mode, Mode::Probe);
            let first = *pilot.cols[0].iter().find(|id| sampled(**id)).unwrap();
            pilot.cols[0].fill(first);
            assert!(reuse.apply(&ctx, &mut pilot, target, &expr).unwrap());
            assert_eq!(reuse.evaluations, 1);
            assert_eq!(reuse.values.len(), 1);
        });
    }

    fn sampled(id: Id) -> bool {
        id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SAMPLE_SHIFT) == 0
    }

    fn local_strings(ctx: &Ctx, keys: impl Iterator<Item = usize>) -> Vec<Id> {
        keys.map(|i| {
            ctx.intern_term(&Term::Literal(Literal::new_simple_literal(format!(
                "value{i:09}"
            ))))
        })
        .collect()
    }

    fn batch(input: VarId, keys: &[Id]) -> Table {
        let mut table = Table::new(vec![input]);
        table.cols[0] = keys.to_vec();
        table.len = keys.len();
        table
    }

    /// Runs `keys` through `reuse` in batches of 4,096 and checks every answer against
    /// row-by-row evaluation of the same rows. Feeds deterministic fallback observations
    /// on declined batches so cross-batch admission tests do not depend on wall clocks.
    fn run(ctx: &Ctx, reuse: &mut Reuse, input: VarId, target: VarId, expr: &Expr, keys: &[Id]) {
        for chunk in keys.chunks(4096) {
            let mut table = batch(input, chunk);
            let expected = crate::sparql::exec::column_rows(ctx, &table, expr).unwrap();
            if reuse.apply(ctx, &mut table, target, expr).unwrap() {
                assert_eq!(table.cols[1], expected);
            } else {
                reuse.observe_fallback_with(
                    chunk.len(),
                    ColumnEval {
                        evaluated_rows: chunk.len(),
                        elapsed_nanos: Some(chunk.len() as u128 * 200),
                        split: None,
                        parallel_allowed: true,
                    },
                    None,
                    expr.parallel(),
                    rayon::current_num_threads() > 1,
                );
            }
            ctx.check().unwrap();
        }
    }

    #[test]
    fn sparse_reuse_across_batches_admits_after_observed_repetition() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        // Every key is distinct within the first eight batches and repeats after them,
        // the global-repeat shape that a one-shot pilot declines.
        let domain = local_strings(&ctx, 0..32768);
        let keys = (0..131072usize)
            .map(|i| domain[i.wrapping_mul(2654435761) % 32768])
            .collect::<Vec<_>>();
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.mode, Mode::Map);
        assert!(
            reuse.evaluations <= 32768 + 8192,
            "admitted soon after repetition starts: {} evaluations",
            reuse.evaluations
        );
        // A map admitted after the first period fills during the second and then
        // answers every row.
        assert!(
            reuse.hits >= 131072 - 2 * 32768 - 8192,
            "{} rows answered",
            reuse.hits
        );
    }

    #[test]
    fn hot_keys_the_sketch_does_not_sample_admit_from_the_exact_batch_count() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(
            &ctx,
            &mut reuse,
            input,
            target,
            &expr,
            &local_strings(&ctx, 0..4096),
        );
        assert_eq!(reuse.mode, Mode::Probe);
        // Eight hot keys, none of which the sketch samples.
        let hot = local_strings(&ctx, 10_000..20_000)
            .into_iter()
            .filter(|id| !sampled(*id))
            .take(8)
            .collect::<Vec<_>>();
        let keys = (0..4096).map(|i| hot[i % 8]).collect::<Vec<_>>();
        let mut table = batch(input, &keys);
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(reuse.mode, Mode::Probe, "the sketch sees no repetition");
        // The per-batch distinct pass counted 8 distinct of 4,096 rows.
        reuse.counted(4096, 8);
        assert_eq!(reuse.mode, Mode::Map);
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.evaluations, 8);
        reuse.counted(4096, 4096);
        assert_eq!(
            reuse.mode,
            Mode::Map,
            "an admitted map keeps its own release rule"
        );
    }

    fn pending_entries(reuse: &Reuse) -> usize {
        reuse
            .values
            .values()
            .filter(|value| value.0 & !PAYLOAD_MASK == PENDING)
            .count()
    }

    #[test]
    fn a_map_that_fills_during_a_batch_keeps_no_pending_entries() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        // A dense first batch admits the map with room for 100 entries.
        let dense = local_strings(&ctx, (0..4096).map(|i| i % 64));
        run(&ctx, &mut reuse, input, target, &expr, &dense);
        assert_eq!(reuse.mode, Mode::Map);
        reuse.max_entries = 100;
        // This batch repeats 200 keys, so it fills the map partway through and its
        // keys beyond the room are answered but not retained.
        let keys = local_strings(&ctx, (0..4096).map(|i| 1000 + i % 200));
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.values.len(), 100);
        assert_eq!(pending_entries(&reuse), 0);
        // A full map keeps the slots of its misses apart and still answers every row.
        let before = reuse.evaluations;
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.values.len(), 100);
        assert_eq!(pending_entries(&reuse), 0);
        // The 64 dense keys left 36 places, so 164 of the 200 keys miss again.
        assert_eq!(reuse.evaluations - before, 164);
    }

    #[test]
    fn a_refused_batch_leaves_no_tombstones_in_the_map() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(
            &ctx,
            &mut reuse,
            input,
            target,
            &expr,
            &local_strings(&ctx, (0..4096).map(|i| i % 64)),
        );
        assert_eq!(reuse.mode, Mode::Map);
        // 950 new keys against room for 936 more. The survivors need the same bucket
        // count as the table that held the 14 refused keys, so a shrink would keep that
        // table and any tombstones the removals left. Removals into groups with empty
        // slots leave none, so this checks the rebuilt table's room rather than
        // reproducing tombstones, which need colliding hashes.
        reuse.max_entries = 1000;
        let keys = local_strings(&ctx, 1000..1950);
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.values.len(), 1000);
        assert_eq!(pending_entries(&reuse), 0);
        assert_eq!(
            reuse.values.capacity(),
            FxHashMap::<Id, Id>::with_capacity_and_hasher(1000, Default::default()).capacity()
        );
    }

    #[test]
    fn unique_inputs_stop_probing_once_the_sketch_is_full() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let keys = local_strings(&ctx, 0..90000);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.mode, Mode::Off);
        assert_eq!(reuse.evaluations, 0);
        assert!(reuse.sketch.is_empty() && reuse.values.is_empty());
        assert!(reuse.charge.is_none());
    }

    #[test]
    fn the_batch_that_fills_the_sketch_can_still_admit_from_its_exact_count() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let all = local_strings(&ctx, 0..120_000);
        let (sampled_keys, unsampled): (Vec<Id>, Vec<Id>) =
            all.into_iter().partition(|id| sampled(*id));
        assert!(sampled_keys.len() > SKETCH_KEYS + WINDOW as usize);
        // A pilot of 4,095 sampled keys and one unsampled key leaves the sketch one
        // window short of full.
        let mut first = sampled_keys[..SKETCH_KEYS - 1].to_vec();
        first.push(unsampled[0]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &first), target, &expr)
                .unwrap()
        );
        assert_eq!(reuse.mode, Mode::Probe);
        assert_eq!(reuse.sketch.len(), SKETCH_KEYS - 1);
        // The next batch fills the sketch with new sampled keys, while one unsampled
        // key repeats in the rest of it.
        let hot = unsampled[1];
        let mut keys = sampled_keys[SKETCH_KEYS..SKETCH_KEYS + WINDOW as usize].to_vec();
        keys.resize(4096, hot);
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        assert!(reuse.exhausted);
        reuse.counted(4096, WINDOW as usize + 1);
        assert_eq!(reuse.mode, Mode::Map, "the exact count admits the map");
        let before = reuse.evaluations;
        run(&ctx, &mut reuse, input, target, &expr, &vec![hot; 4096]);
        assert_eq!(reuse.evaluations - before, 1);
        // Without that count, the next batch declines.
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &first), target, &expr)
                .unwrap()
        );
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        reuse.counted(4096, 4096);
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        assert_eq!(reuse.mode, Mode::Off);
    }

    #[test]
    fn an_admitted_map_that_stops_answering_rows_is_released() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        // A dense first batch admits the map, and unique keys follow.
        let mut keys = local_strings(&ctx, (0..4096).map(|i| i % 64));
        keys.extend(local_strings(&ctx, 100..100_000));
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(&ctx, &mut reuse, input, target, &expr, &keys[..4096]);
        assert_eq!(reuse.mode, Mode::Map);
        run(&ctx, &mut reuse, input, target, &expr, &keys[4096..]);
        assert_eq!(reuse.mode, Mode::Off);
        assert!(reuse.values.is_empty());
        assert!(reuse.charge.is_none());
        assert!(
            reuse.evaluations <= 64 + MAP_ENTRIES + MAP_WINDOW as usize,
            "the map is judged once it has seen as many rows as it holds"
        );
    }

    #[test]
    fn exact_id_cache_keeps_errors_and_term_identity_and_checks_all_hit_cancellation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let terms = [
            Literal::new_typed_literal("2000-01-01+00:00", xsd::DATE),
            Literal::new_typed_literal("2000-01-01Z", xsd::DATE),
            Literal::new_typed_literal("bad-date", xsd::DATE),
            Literal::new_simple_literal("2000-01-01"),
        ];
        let keys = terms
            .iter()
            .map(|t| ctx.intern_term(&Term::Literal(t.clone())))
            .chain([Id::UNDEF])
            .collect::<Vec<_>>();
        assert_ne!(
            keys[0], keys[1],
            "equal date values with different lexical forms are distinct terms"
        );
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        for epoch in 0..2 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|row| keys[(row + epoch) % keys.len()])
                .collect();
            table.len = 4096;
            assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
            for row in 0..table.len {
                if (row + epoch) % keys.len() < 2 {
                    assert_eq!(
                        ctx.term(table.cols[1][row]),
                        Some(Term::Literal(Literal::from(2000)))
                    );
                } else {
                    assert_eq!(table.cols[1][row], Id::UNDEF);
                }
            }
        }
        assert_eq!(reuse.evaluations, keys.len());
        let mut hits = Table::new(vec![input]);
        hits.cols[0] = vec![keys[0]; 4096];
        hits.len = 4096;
        ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            reuse.apply(&ctx, &mut hits, target, &expr),
            Err(crate::Error::Cancelled)
        ));
    }

    #[test]
    fn near_unique_generic_inputs_decline_optional_retention() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let keys = (0..4096)
            .map(|i| {
                ctx.intern_term(&Term::Literal(Literal::new_simple_literal(format!(
                    "value{i}"
                ))))
            })
            .collect::<Vec<_>>();
        for stride in [usize::MAX, 64] {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|i| {
                    if i == 1 || (i > 0 && i % stride == 0) {
                        keys[i - 1]
                    } else {
                        keys[i]
                    }
                })
                .collect();
            table.len = 4096;
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            assert!(
                !reuse.apply(&ctx, &mut table, target, &expr).unwrap(),
                "isolated or infrequent repetition must not retain mostly distinct results"
            );
            assert!(reuse.values.is_empty());
            assert_eq!(reuse.evaluations, 0);
            assert_eq!(table.cols.len(), 1);
            ctx.check().unwrap();
        }
        let mut table = Table::new(vec![input]);
        table.cols[0] = (0..4096).map(|i| keys[i % 2048]).collect();
        table.len = 4096;
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(
            reuse.evaluations, 2048,
            "strong within-batch reuse remains immediately eligible"
        );
        assert_eq!(
            table.cols[0], table.cols[1],
            "STR of strings keeps the exact values"
        );
    }

    #[test]
    fn domain_hint_is_snapshot_local_and_plan_walk_is_bounded() {
        use crate::sparql::plan::{GraphFilter, ScanSpec};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..4096)
            .map(|i| {
                format!(
                    "<urn:s{i:05}> <urn:p> \"value{}\" .\n",
                    if i == 1 { 0 } else { i % 3000 }
                )
            })
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let ctx = cursor_context(&store, 16 << 20);
        assert!(ctx.snap.delta.is_empty());
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let subject = ctx.var("subject");
        let predicate = ctx
            .snap
            .lookup_term(&Term::NamedNode(oxrdf::NamedNode::new("urn:p").unwrap()))
            .unwrap();
        let source = Node::leaf(
            Kind::Scan(ScanSpec {
                perm: Perm::Pso,
                prefix: vec![predicate.0],
                cols: vec![(1, subject), (2, input)],
                eqs: vec![],
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            }),
            vec![subject, input],
            4096.0,
            String::new(),
        );
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let before = ctx.live_bytes();
        let admission = Reuse::prepare(&ctx, &expr, Some(&source)).unwrap();
        assert_eq!(
            ctx.live_bytes(),
            before,
            "metadata preparation must not retain optional memory before child construction"
        );
        let mut reuse = Reuse::new(&ctx, admission).unwrap();
        assert!(reuse.expected_reuse);
        let mut bind = Node::leaf(
            Kind::Extend(target, expr.clone()),
            vec![subject, input, target],
            4096.0,
            String::new(),
        );
        bind.children.push(source.clone());
        let mut operator = super::super::Operator::build(
            &ctx,
            bind,
            super::super::FallbackPolicy::RejectMaterialization,
        )
        .unwrap();
        assert!(
            operator.bind_reuse.as_ref().unwrap().expected_reuse,
            "operator construction must inspect the input before replacing child plans with shells"
        );
        let options = super::super::CursorOptions {
            batch_rows: 1024,
            ..Default::default()
        };
        let mut rows = 0;
        let released = |operator: &super::super::Operator| {
            let reuse = operator.bind_reuse.as_ref().unwrap();
            reuse.charge.is_none() && reuse.values.is_empty() && reuse.sketch.is_empty()
        };
        while let Some(batch) = operator.next(&ctx, &options, 1024).unwrap() {
            rows += batch.table.len;
            assert_eq!(batch.table.cols[1], batch.table.cols[2]);
            assert_eq!(
                released(&operator),
                operator.done,
                "the pull that ends the input releases the optional map with its last rows"
            );
        }
        assert_eq!(rows, 4096);
        assert!(
            released(&operator),
            "an exhausted BIND keeps no optional memory for the operators above it"
        );
        assert_eq!(
            operator.bind_reuse.as_ref().unwrap().evaluations,
            3000,
            "a sparse first pilot must still reuse the bounded global domain across morsels"
        );
        let table = || {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..1024)
                .map(|i| {
                    let i = if i == 1 { 0 } else { i };
                    ctx.snap
                        .lookup_term(&Term::Literal(Literal::new_simple_literal(format!(
                            "value{i}"
                        ))))
                        .unwrap()
                })
                .collect();
            table.len = 1024;
            table
        };
        assert!(reuse.apply(&ctx, &mut table(), target, &expr).unwrap());
        let mut deep = source.clone();
        for _ in 0..64 {
            let mut parent =
                Node::leaf(Kind::Sort(vec![input]), vec![input], 4096.0, String::new());
            parent.children.push(deep);
            deep = parent;
        }
        assert!(!new_reuse(&ctx, &expr, Some(&deep)).unwrap().expected_reuse);
        let mut hidden = Node::leaf(Kind::Project(vec![]), vec![], 4096.0, String::new());
        hidden.children.push(source.clone());
        assert!(
            !new_reuse(&ctx, &expr, Some(&hidden))
                .unwrap()
                .expected_reuse
        );
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:new> <urn:p> \"new value\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let changed = cursor_context(&store, 16 << 20);
        assert!(!changed.snap.delta.is_empty());
        assert_eq!(changed.var("value"), input);
        let mut declined = new_reuse(&changed, &expr, Some(&source)).unwrap();
        assert!(!declined.expected_reuse);
        assert!(
            !declined
                .apply(&changed, &mut table(), target, &expr)
                .unwrap()
        );
        assert!(
            new_reuse(&ctx, &expr, Some(&source))
                .unwrap()
                .expected_reuse,
            "the retained old snapshot keeps its own unchanged metadata"
        );
    }

    #[test]
    fn optional_cache_does_not_starve_mandatory_child_construction() {
        use super::super::{CursorOptions, FallbackPolicy, Operator};
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 768);
        let input = ctx.var("input");
        let target = ctx.var("bound");
        let plan = || {
            let mut distinct = Node::leaf(Kind::Distinct, vec![input], 0.0, String::new());
            distinct
                .children
                .push(Node::leaf(Kind::Empty, vec![input], 0.0, String::new()));
            let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
            let mut bind = Node::leaf(
                Kind::Extend(target, expr),
                vec![input, target],
                0.0,
                String::new(),
            );
            bind.children.push(distinct);
            bind
        };
        let early = ctx.retained_charge(512).unwrap();
        assert!(
            Operator::build(&ctx, plan(), FallbackPolicy::RejectMaterialization).is_err(),
            "an early optional reservation can starve the child's mandatory264-byte state"
        );
        drop(early);
        let mut operator = Operator::build(&ctx, plan(), FallbackPolicy::RejectMaterialization)
            .expect(
                "optional BIND state must decline after mandatory children use the memory budget",
            );
        assert!(operator.bind_reuse.is_none());
        assert!(
            operator
                .next(&ctx, &CursorOptions::default(), 1024)
                .unwrap()
                .is_none()
        );
        ctx.check().unwrap();
    }

    #[test]
    fn cache_growth_refusal_retains_a_bounded_partial_map_and_normal_evaluation() {
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        single.install(|| {
            let store = Store::in_memory(StoreOptions::default());
            let ctx = cursor_context(&store, 128 << 10);
            let input = ctx.var("date");
            let target = ctx.var("year");
            let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
            let date = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                "2000-01-01+00:00",
                xsd::DATE,
            )));
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            let mut retained = None;
            for epoch in 0..4 {
                let mut table = Table::new(vec![input]);
                table.cols[0] = (0..1024)
                    .map(|row| {
                        if row < 257 {
                            date
                        } else {
                            Id::from_i64((epoch * 1024 + row) as i64).unwrap()
                        }
                    })
                    .collect();
                table.len = 1024;
                assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
                assert_eq!(
                    ctx.term(table.cols[1][0]),
                    Some(Term::Literal(Literal::from(2000)))
                );
                assert!(table.cols[1][257..].iter().all(|id| *id == Id::UNDEF));
                assert!(ctx.live_bytes() <= 128 << 10);
                ctx.check().unwrap();
                // The table the map allocated, a key and a value per slot, stays within its
                // retained charge after the batch's scratch is released.
                assert_eq!(pending_entries(&reuse), 0);
                // A rebuilt table has a fresh table's room, with no tombstones to use it up.
                assert_eq!(
                    reuse.values.capacity(),
                    FxHashMap::<Id, Id>::with_capacity_and_hasher(
                        reuse.values.len(),
                        Default::default()
                    )
                    .capacity()
                );
                assert!(
                    reuse.values.capacity() as u64 * 16 <= reuse.charge.as_ref().unwrap().bytes(),
                    "capacity {} against a charge of {}",
                    reuse.values.capacity(),
                    reuse.charge.as_ref().unwrap().bytes()
                );
                if let Some(entries) = retained {
                    assert_eq!(
                        reuse.values.len(),
                        entries,
                        "a refused optional growth keeps the existing bounded entries"
                    );
                } else {
                    assert!(reuse.values.len() < 768);
                    retained = Some(reuse.values.len());
                }
            }
            assert_eq!(
                reuse.evaluations,
                1 + 4 * 767,
                "uncached keys still evaluate normally after the cache fills"
            );
        });
    }

    #[test]
    fn iri_split_cooldown_controller() {
        // Projected serial rest time for split(100_000, rest) is 400_000 ns.
        assert_eq!(split(100_000, 120_000).valid_paid(), Some(true));
        assert_eq!(split(100_000, 399_999).valid_paid(), Some(true));
        assert_eq!(split(100_000, 400_000).valid_paid(), Some(false));
        // 1.05x is Inconclusive under classify(), but a valid nonpaying split under valid_paid().
        assert_eq!(
            split(100_000, 420_000).classify(),
            SplitOutcome::Inconclusive
        );
        assert_eq!(split(100_000, 420_000).valid_paid(), Some(false));
        assert_eq!(split(100_000, 1_200_000).valid_paid(), Some(false));
        assert_eq!(split(0, 10).valid_paid(), None);
        assert_eq!(split(100_000, 0).valid_paid(), None);
        assert_eq!(split(u128::MAX, u128::MAX).valid_paid(), None);

        let mut ctrl = IriSplitController::new(IRI_SPLIT_COOLDOWN);
        assert!(ctrl.allow(SplitBucket::Small));
        assert!(ctrl.allow(SplitBucket::Large));

        // Invalid and paying splits keep the bucket Active.
        assert_eq!(
            ctrl.observe(SplitBucket::Large, true, Some(split(0, 1_000_000))),
            None
        );
        assert_eq!(
            ctrl.observe(SplitBucket::Large, true, Some(split(100_000, 399_999))),
            None
        );
        assert_eq!(ctrl.large, IriBucketPhase::Active);

        // One valid nonpaying split (even within the ordinary Inconclusive band) enters cooldown.
        assert_eq!(
            ctrl.observe(SplitBucket::Large, true, Some(split(100_000, 420_000))),
            Some("iri:active->cooldown")
        );
        assert_eq!(
            ctrl.large,
            IriBucketPhase::Cooldown {
                remaining: IRI_SPLIT_COOLDOWN
            }
        );
        assert_eq!(ctrl.small, IriBucketPhase::Active);
        assert!(ctrl.allow(SplitBucket::Small));

        // Exactly 16 eligible suppressed batches transition to Retry.
        for step in 0..IRI_SPLIT_COOLDOWN {
            assert!(!ctrl.allow(SplitBucket::Large), "step {step} suppressed");
            let tr = ctrl.observe(SplitBucket::Large, false, None);
            if step + 1 == IRI_SPLIT_COOLDOWN {
                assert_eq!(tr, Some("iri:cooldown->retry"));
            } else {
                assert_eq!(tr, None);
            }
        }
        assert_eq!(ctrl.large, IriBucketPhase::Retry);
        assert!(ctrl.allow(SplitBucket::Large));

        // Missing or invalid retry observation keeps Retry pending.
        assert_eq!(ctrl.observe(SplitBucket::Large, true, None), None);
        assert_eq!(
            ctrl.observe(SplitBucket::Large, true, Some(split(0, 500_000))),
            None
        );
        assert_eq!(ctrl.large, IriBucketPhase::Retry);

        // Nonpaying retry re-enters cooldown immediately; paying retry recovers to Active.
        assert_eq!(
            ctrl.observe(SplitBucket::Large, true, Some(split(100_000, 400_000))),
            Some("iri:retry->cooldown")
        );
        assert_eq!(
            ctrl.large,
            IriBucketPhase::Cooldown {
                remaining: IRI_SPLIT_COOLDOWN
            }
        );
        for _ in 0..IRI_SPLIT_COOLDOWN {
            ctrl.observe(SplitBucket::Large, false, None);
        }
        assert_eq!(ctrl.large, IriBucketPhase::Retry);
        assert_eq!(
            ctrl.observe(SplitBucket::Large, true, Some(split(100_000, 120_000))),
            Some("iri:retry->active")
        );
        assert_eq!(ctrl.large, IriBucketPhase::Active);
    }

    #[test]
    fn iri_map_miss_cooldown_dispatches_homogeneous_iris_isolates_mixed_and_retries_after_16() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ttl = String::new();
        for i in 0..64 {
            ttl.push_str(&format!("<urn:seed:{i:04}> <urn:p> \"seed{i:04}\" .\n"));
        }
        for i in 0..2048 {
            ttl.push_str(&format!("<urn:iri:{i:04}> <urn:p> \"lit{i:04}\" .\n"));
        }
        store
            .load(&[Source::from_bytes(
                ttl.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();

        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("v");
        let target = ctx.var("s");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let non_str = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let nested_str = Expr::Call(Func::Builtin(Function::UCase), vec![expr.clone()]);

        let lookup_iri = |ctx: &Ctx, uri: &str| -> Id {
            let id = ctx
                .snap
                .lookup_term(&Term::NamedNode(oxrdf::NamedNode::new(uri).unwrap()))
                .unwrap();
            assert_eq!(id.tag(), Tag::Vocab);
            assert!(ctx.snap.generation.vocab.is_iri(id.payload()));
            id
        };
        let lookup_lit = |ctx: &Ctx, val: &str| -> Id {
            let id = ctx
                .snap
                .lookup_term(&Term::Literal(Literal::new_simple_literal(val)))
                .unwrap();
            assert_eq!(id.tag(), Tag::Vocab);
            assert!(!ctx.snap.generation.vocab.is_iri(id.payload()));
            id
        };

        let seed_iris: Vec<Id> = (0..64)
            .map(|i| lookup_iri(&ctx, &format!("urn:seed:{i:04}")))
            .collect();
        let stored_iris: Vec<Id> = (0..2048)
            .map(|i| lookup_iri(&ctx, &format!("urn:iri:{i:04}")))
            .collect();
        let stored_lits: Vec<Id> = (0..1024)
            .map(|i| lookup_lit(&ctx, &format!("lit{i:04}")))
            .collect();

        // Expression recognition happens once in prepare().
        assert!(!new_reuse(&ctx, &non_str, None).unwrap().direct_str_input);
        assert!(!new_reuse(&ctx, &nested_str, None).unwrap().direct_str_input);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut reuse = new_reuse_cooldown(&ctx, &expr, 2);
            assert!(reuse.direct_str_input);

            // Range, worker-count, switch and mode boundaries.
            assert_eq!(
                reuse.iri_miss_bucket(&ctx, &stored_iris[..767], true),
                None,
                "below sampled-split range uses ordinary policy"
            );
            assert_eq!(
                reuse.iri_miss_bucket(&ctx, &stored_iris[..1024], false),
                None,
                "single-worker execution uses ordinary policy"
            );
            assert_eq!(
                reuse.iri_miss_bucket(&ctx, &stored_iris[..1024], true),
                Some(SplitBucket::Small)
            );
            assert_eq!(
                reuse.iri_miss_bucket(&ctx, &stored_iris[..2048], true),
                Some(SplitBucket::Large)
            );
            assert_eq!(
                reuse.iri_miss_bucket(&ctx, &stored_lits, true),
                None,
                "Tag::Vocab literals must not match stored-IRI override"
            );

            // Admit map on 64 dense seed IRIs and cap at 64 entries so subsequent batches
            // exercise apply_map miss dispatch on full distinct miss columns.
            let seed_rows: Vec<Id> = (0..4096).map(|i| seed_iris[i % 64]).collect();
            let mut first = batch(input, &seed_rows);
            assert!(reuse.apply(&ctx, &mut first, target, &expr).unwrap());
            assert_eq!(reuse.mode, Mode::Map);
            reuse.max_entries = 64;
            assert_eq!(reuse.values.len(), 64);

            // Initial homogeneous stored-IRI miss batch dispatches with par = true and
            // produces fresh query-local string literals.
            let mut iri_small = batch(input, &stored_iris[..1024]);
            let expected_iri_small = exec::column_rows(&ctx, &iri_small, &expr).unwrap();
            assert!(reuse.apply(&ctx, &mut iri_small, target, &expr).unwrap());
            assert_eq!(iri_small.cols[1], expected_iri_small);
            assert!(
                iri_small.cols[1].iter().all(|id| id.tag() == Tag::Local),
                "STR of IRIs whose lexical strings are absent from the store interns locally"
            );
            assert!(reuse.last_miss_dispatch.unwrap().0);

            // One valid nonpaying split enters IRI Small cooldown without touching ordinary
            // SplitController, fallback cooldown, or cost contention evidence.
            reuse.iri_split.small = IriBucketPhase::Active;
            reuse
                .iri_split
                .observe(SplitBucket::Small, true, Some(split(100_000, 420_000)));
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN
                }
            );
            assert_eq!(reuse.split.small, BucketPhase::Active { losses: 0 });
            assert!(!reuse.split.is_contended(1024));
            assert!(reuse.allow_fallback_split(1024, &expr));

            // Cooldown step 1: actual dispatch on homogeneous stored-IRI misses runs par = false.
            let mut step1 = batch(input, &stored_iris[..1024]);
            assert!(reuse.apply(&ctx, &mut step1, target, &expr).unwrap());
            assert_eq!(step1.cols[1], expected_iri_small);
            assert_eq!(reuse.last_miss_dispatch, Some((false, None)));
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );
            assert_eq!(reuse.split.small, BucketPhase::Active { losses: 0 });

            // All-hit batch does not dispatch misses or consume IRI cooldown.
            reuse.last_miss_dispatch = None;
            let mut all_hits = batch(input, &seed_rows);
            assert!(reuse.apply(&ctx, &mut all_hits, target, &expr).unwrap());
            assert_eq!(reuse.last_miss_dispatch, None);
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );

            // Stored-literal batch uses ordinary policy (par = true) and leaves IRI cooldown intact.
            let mut lit_batch = batch(input, &stored_lits);
            let expected_lits = exec::column_rows(&ctx, &lit_batch, &expr).unwrap();
            assert!(reuse.apply(&ctx, &mut lit_batch, target, &expr).unwrap());
            assert_eq!(lit_batch.cols[1], expected_lits);
            assert!(reuse.last_miss_dispatch.unwrap().0);
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );

            // Mixed batch (1,023 stored IRIs + 1 stored literal at the end) uses ordinary policy.
            reuse.split.small = BucketPhase::Active { losses: 0 };
            let mut mixed_keys = stored_iris[..1023].to_vec();
            mixed_keys.push(stored_lits[0]);
            let mut mixed_batch = batch(input, &mixed_keys);
            let expected_mixed = exec::column_rows(&ctx, &mixed_batch, &expr).unwrap();
            assert!(reuse.apply(&ctx, &mut mixed_batch, target, &expr).unwrap());
            assert_eq!(mixed_batch.cols[1], expected_mixed);
            assert!(reuse.last_miss_dispatch.unwrap().0);
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );

            // Computed/query-local IRI mixed into stored IRIs uses ordinary policy.
            reuse.split.small = BucketPhase::Active { losses: 0 };
            let local_iri = ctx.intern_term(&Term::NamedNode(
                oxrdf::NamedNode::new("urn:local:computed").unwrap(),
            ));
            assert_eq!(local_iri.tag(), Tag::Local);
            let mut local_mixed = stored_iris[..1023].to_vec();
            local_mixed.push(local_iri);
            let mut local_batch = batch(input, &local_mixed);
            assert!(reuse.apply(&ctx, &mut local_batch, target, &expr).unwrap());
            assert!(reuse.last_miss_dispatch.unwrap().0);
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );

            // Large bucket (2,048 stored IRIs) is independent of Small IRI cooldown.
            let mut iri_large = batch(input, &stored_iris[..2048]);
            assert!(reuse.apply(&ctx, &mut iri_large, target, &expr).unwrap());
            assert!(reuse.last_miss_dispatch.unwrap().0);
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );

            // Fallback losses stay strictly in ordinary SplitController without touching IRI cooldown.
            let large_before_fb = reuse.iri_split.large;
            let fb_loss = ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: Some(4096 * 500),
                split: Some(split(100_000, 1_200_000)),
                parallel_allowed: true,
            };
            reuse.observe_fallback(4096, fb_loss, None, &expr);
            reuse.observe_fallback(4096, fb_loss, None, &expr);
            assert!(reuse.split.is_contended(4096));
            assert_eq!(
                reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN - 1
                }
            );
            assert_eq!(reuse.iri_split.large, large_before_fb);

            // Remaining 15 eligible Small homogeneous stored-IRI batches stay serial and unlock Retry.
            for step in 1..IRI_SPLIT_COOLDOWN {
                let mut b = batch(input, &stored_iris[..1024]);
                assert!(reuse.apply(&ctx, &mut b, target, &expr).unwrap());
                assert_eq!(b.cols[1], expected_iri_small);
                assert_eq!(
                    reuse.last_miss_dispatch,
                    Some((false, None)),
                    "eligible step {step} must stay serial"
                );
            }
            assert_eq!(reuse.iri_split.small, IriBucketPhase::Retry);

            // Retry after 16 eligible batches restores par = true on actual dispatch.
            let mut retry_batch = batch(input, &stored_iris[..1024]);
            assert!(reuse.apply(&ctx, &mut retry_batch, target, &expr).unwrap());
            assert_eq!(retry_batch.cols[1], expected_iri_small);
            assert!(
                reuse.last_miss_dispatch.unwrap().0,
                "17th eligible batch must retry with par = true"
            );
            assert_eq!(pending_entries(&reuse), 0);

            // Delta IRI mixed into stored IRIs also uses ordinary policy.
            crate::sparql::update::update(
                &store,
                "INSERT DATA { <urn:delta:iri> <urn:p> \"delta\" }",
                &crate::sparql::QueryOptions::default(),
            )
            .unwrap();
            let delta_ctx = cursor_context(&store, 64 << 20);
            let delta_input = delta_ctx.var("v");
            let delta_target = delta_ctx.var("s");
            let delta_expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(delta_input)]);
            let delta_iri = delta_ctx
                .snap
                .lookup_term(&Term::NamedNode(
                    oxrdf::NamedNode::new("urn:delta:iri").unwrap(),
                ))
                .unwrap();
            assert_eq!(delta_iri.tag(), Tag::Delta);
            let mut delta_reuse = new_reuse(&delta_ctx, &delta_expr, None).unwrap();
            let mut delta_seed = batch(delta_input, &seed_rows);
            assert!(
                delta_reuse
                    .apply(&delta_ctx, &mut delta_seed, delta_target, &delta_expr)
                    .unwrap()
            );
            delta_reuse.max_entries = 64;
            delta_reuse
                .iri_split
                .observe(SplitBucket::Small, true, Some(split(100_000, 420_000)));
            let mut delta_mixed = stored_iris[..1023].to_vec();
            delta_mixed.push(delta_iri);
            assert_eq!(
                delta_reuse.iri_miss_bucket(&delta_ctx, &delta_mixed, true),
                None,
                "delta IRIs must use ordinary policy"
            );
            let mut delta_batch = batch(delta_input, &delta_mixed);
            assert!(
                delta_reuse
                    .apply(&delta_ctx, &mut delta_batch, delta_target, &delta_expr)
                    .unwrap()
            );
            assert!(delta_reuse.last_miss_dispatch.unwrap().0);
            assert_eq!(
                delta_reuse.iri_split.small,
                IriBucketPhase::Cooldown {
                    remaining: IRI_SPLIT_COOLDOWN
                }
            );
        });
    }

    #[test]
    fn iri_map_miss_profitable_split_control_keeps_parallelism_active() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ttl = String::new();
        for i in 0..64 {
            ttl.push_str(&format!(
                "<urn:seed:{i:04}> <urn:p> \"urn:seed:{i:04}\" .\n"
            ));
        }
        // Every STR(<urn:known:i>) string literal already exists in the base vocabulary.
        for i in 0..1024 {
            ttl.push_str(&format!(
                "<urn:known:{i:04}> <urn:p> \"urn:known:{i:04}\" .\n"
            ));
        }
        store
            .load(&[Source::from_bytes(
                ttl.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();

        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("v");
        let target = ctx.var("s");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let seed_iris: Vec<Id> = (0..64)
            .map(|i| {
                ctx.snap
                    .lookup_term(&Term::NamedNode(
                        oxrdf::NamedNode::new(format!("urn:seed:{i:04}")).unwrap(),
                    ))
                    .unwrap()
            })
            .collect();
        let known_iris: Vec<Id> = (0..1024)
            .map(|i| {
                let id = ctx
                    .snap
                    .lookup_term(&Term::NamedNode(
                        oxrdf::NamedNode::new(format!("urn:known:{i:04}")).unwrap(),
                    ))
                    .unwrap();
                assert_eq!(id.tag(), Tag::Vocab);
                assert!(ctx.snap.generation.vocab.is_iri(id.payload()));
                id
            })
            .collect();

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut reuse = new_reuse_cooldown(&ctx, &expr, 2);
            let seed_rows: Vec<Id> = (0..4096).map(|i| seed_iris[i % 64]).collect();
            let mut first = batch(input, &seed_rows);
            assert!(reuse.apply(&ctx, &mut first, target, &expr).unwrap());
            reuse.max_entries = 64;

            // Actual dispatch on store-resident-output IRIs resolves outputs to Tag::Vocab
            // (no query-local interning) with par = true.
            let mut b1 = batch(input, &known_iris);
            let expected = exec::column_rows(&ctx, &b1, &expr).unwrap();
            assert!(
                expected.iter().all(|id| id.tag() == Tag::Vocab),
                "control fixture must already hold every STR(iri) literal in base vocab"
            );
            assert!(reuse.apply(&ctx, &mut b1, target, &expr).unwrap());
            assert_eq!(b1.cols[1], expected);
            assert!(reuse.last_miss_dispatch.unwrap().0);

            // A paying split on this control keeps the IRI override Active and leaves the
            // next actual dispatch parallel.
            reuse.iri_split.small = IriBucketPhase::Active;
            reuse
                .iri_split
                .observe(SplitBucket::Small, true, Some(split(100_000, 120_000)));
            assert_eq!(reuse.iri_split.small, IriBucketPhase::Active);

            let mut b2 = batch(input, &known_iris);
            assert!(reuse.apply(&ctx, &mut b2, target, &expr).unwrap());
            assert_eq!(b2.cols[1], expected);
            assert!(reuse.last_miss_dispatch.unwrap().0);

            // A paying split during Retry recovers back to Active.
            reuse.iri_split.small = IriBucketPhase::Retry;
            reuse
                .iri_split
                .observe(SplitBucket::Small, true, Some(split(100_000, 120_000)));
            assert_eq!(reuse.iri_split.small, IriBucketPhase::Active);
        });
    }

    #[test]
    fn fast_weak_admission_admits_on_single_decision_at_probe_and_fallback_sites() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);

        // 1. Exact rational boundary around 100 ns/row and 1/8 repeat floor.
        let mut boundary = new_reuse(&ctx, &expr, None).unwrap();
        boundary.mode = Mode::Probe;
        // Single rate is not usable for fast admission.
        boundary.cost.record(4096, 4096 * 3_000);
        assert_eq!(
            boundary.weak_decision(4096, 512, 4096, true),
            WeakDecision::Fail
        );
        // Lower of (3000, 799) = 799 ns/row at 1/8 repeat -> 99.875 ns/row (< 100 ns, >= 50 ns).
        boundary.cost.record(4096, 4096 * 799);
        assert_eq!(
            boundary.weak_decision(4096, 512, 4096, true),
            WeakDecision::Pass
        );
        // Lower of (799, 800) = 799 ns/row still < 100 ns.
        boundary.cost.record(4096, 4096 * 800);
        assert_eq!(
            boundary.weak_decision(4096, 512, 4096, true),
            WeakDecision::Pass
        );
        // Lower of (800, 1200) = 800 ns/row at 1/8 repeat -> exact 100 ns/row.
        boundary.cost.record(4096, 4096 * 1_200);
        assert_eq!(
            boundary.weak_decision(4096, 512, 4096, true),
            WeakDecision::FastAdmit
        );
        // Below 1/8 repeat floor (511/4096) fails even when rate is 3,000 ns/row.
        boundary.cost.record(4096, 4096 * 3_000);
        boundary.cost.record(4096, 4096 * 3_000);
        assert_eq!(
            boundary.weak_decision(4096, 511, 4096, true),
            WeakDecision::Fail
        );
        // Mismatched bucket (Small vs Large) cannot use Large rates.
        assert_eq!(
            boundary.weak_decision(1024, 512, 1024, true),
            WeakDecision::Fail
        );

        // 2. Fallback site (probe window deferred while only r0 exists, then r1 arrives):
        //    matches sha-stride-8 at batch 2 (33/257 at min(2924, 1156) -> 148.4 ns/row).
        let mut fb_probe = new_reuse(&ctx, &expr, None).unwrap();
        fb_probe.mode = Mode::Probe;
        fb_probe.batch_index = 2;
        fb_probe.cost.record(4096, 4096 * 2_924);
        fb_probe.batch_probe_window = Some((257, 33, 4096));
        fb_probe.observe_fallback_with(
            4096,
            ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: Some(4096 * 1_156),
                split: None,
                parallel_allowed: true,
            },
            Some((4096, 3585)), // exact 511/4096 fails 1/8 floor
            true,
            true,
        );
        assert_eq!(fb_probe.mode, Mode::Map);
        assert_eq!(fb_probe.admitted.map(|(batch, _)| batch), Some(2));
        assert_eq!(
            fb_probe.admitted.map(|(_, reason)| reason),
            Some("probe_weak_fast")
        );

        // 3. Fallback site (counted exact signal alone at exact 100 ns/row saving).
        let mut fb_counted = new_reuse(&ctx, &expr, None).unwrap();
        fb_counted.mode = Mode::Probe;
        fb_counted.batch_index = 3;
        fb_counted.cost.record(4096, 4096 * 900);
        fb_counted.observe_fallback_with(
            4096,
            ColumnEval {
                evaluated_rows: 4096,
                elapsed_nanos: Some(4096 * 800),
                split: None,
                parallel_allowed: true,
            },
            Some((4096, 3584)), // 512/4096 * 800 = 100 ns/row
            true,
            true,
        );
        assert_eq!(fb_counted.mode, Mode::Map);
        assert_eq!(fb_counted.admitted.map(|(batch, _)| batch), Some(3));
        assert_eq!(
            fb_counted.admitted.map(|(_, reason)| reason),
            Some("counted_weak_fast")
        );

        // 4. Sub-100 ns/row saving (e.g. 94 ns/row matching join-str-random-60-65536) still
        //    requires two confirmations, and probe + counted on the same batch count at most once.
        let mut sub_fast = new_reuse(&ctx, &expr, None).unwrap();
        sub_fast.mode = Mode::Probe;
        sub_fast.batch_index = 5;
        sub_fast.cost.record(4096, 4096 * 210);
        let str_eval = ColumnEval {
            evaluated_rows: 4096,
            elapsed_nanos: Some(4096 * 212),
            split: None,
            parallel_allowed: true,
        };
        // probe: 129/288 * 210 = 94.06 ns/row (< 100 ns); exact: 1400/4096 * 210 = 71.78 ns/row.
        sub_fast.batch_probe_window = Some((288, 129, 4096));
        sub_fast.observe_fallback_with(4096, str_eval, Some((4096, 2696)), true, true);
        assert_eq!(
            sub_fast.mode,
            Mode::Probe,
            "94 ns/row with both probe and counted on one batch must still need two confirmations"
        );
        assert_eq!(sub_fast.weak_confirmations, 1);

        // Second consecutive passing batch (< 100 ns/row) admits via ordinary two-confirmation.
        sub_fast.batch_index = 6;
        sub_fast.batch_probe_window = Some((288, 129, 4096));
        sub_fast.observe_fallback_with(4096, str_eval, None, true, true);
        assert_eq!(sub_fast.mode, Mode::Map);
        assert_eq!(sub_fast.admitted.map(|(batch, _)| batch), Some(6));
        assert_eq!(
            sub_fast.admitted.map(|(_, reason)| reason),
            Some("probe_weak")
        );

        // 5. Probe site under multi-worker pool (modeling join-sha-random-60-65536 at batch 3
        //    vs join-str-random-60-65536 at 94 ns/row).
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let strings = local_strings(&ctx, 0..16_384);
            let sampled_keys: Vec<Id> = strings
                .iter()
                .copied()
                .filter(|id| sampled(*id))
                .take(WINDOW as usize * 2)
                .collect();
            let unsampled_keys: Vec<Id> = strings
                .iter()
                .copied()
                .filter(|id| !sampled(*id))
                .take(4096)
                .collect();

            // Build a batch of 4,096 distinct keys with WINDOW sampled keys, then a second
            // batch where 129/288 (~44.8%) of sampled keys repeat from the sketch.
            let mut pilot_keys = sampled_keys[..WINDOW as usize].to_vec();
            pilot_keys.extend_from_slice(&unsampled_keys[..4096 - WINDOW as usize]);

            let mut repeat_keys = sampled_keys[..129].to_vec();
            repeat_keys.extend_from_slice(&sampled_keys[WINDOW as usize..WINDOW as usize + 159]);
            repeat_keys.extend_from_slice(&unsampled_keys[..4096 - 288]);
            assert_eq!(repeat_keys.len(), 4096);

            // High rate (1,394 ns/row -> 129/288 * 1394 = 624 ns/row >= 100 ns): admits
            // immediately at the probe site on a single completed weak decision.
            let mut fast_probe = new_reuse(&ctx, &expr, None).unwrap();
            let mut t1 = batch(input, &pilot_keys);
            assert!(!fast_probe.apply(&ctx, &mut t1, target, &expr).unwrap());
            assert_eq!(fast_probe.mode, Mode::Probe);
            fast_probe.cost.record(4096, 4096 * 1_876);
            fast_probe.cost.record(4096, 4096 * 1_394);
            assert_eq!(fast_probe.weak_confirmations, 0);

            let mut t2 = batch(input, &repeat_keys);
            let expected = exec::column_rows(&ctx, &t2, &expr).unwrap();
            assert!(
                fast_probe.apply(&ctx, &mut t2, target, &expr).unwrap(),
                "single probe weak decision >= 100 ns/row must admit directly at probe site"
            );
            assert_eq!(t2.cols[1], expected);
            assert_eq!(fast_probe.mode, Mode::Map);
            assert_eq!(fast_probe.admitted.map(|(batch, _)| batch), Some(2));
            assert_eq!(
                fast_probe.admitted.map(|(_, reason)| reason),
                Some("probe_weak_fast")
            );

            // Sub-100 ns rate (210 ns/row -> 129/288 * 210 = 94.06 ns/row < 100 ns): does NOT
            // admit at probe site on 1st window; defers to fallback for at-most-once counting.
            let mut slow_probe = new_reuse(&ctx, &expr, None).unwrap();
            let mut s1 = batch(input, &pilot_keys);
            assert!(!slow_probe.apply(&ctx, &mut s1, target, &expr).unwrap());
            slow_probe.cost.record(4096, 4096 * 210);
            slow_probe.cost.record(4096, 4096 * 212);
            let mut s2 = batch(input, &repeat_keys);
            assert!(
                !slow_probe.apply(&ctx, &mut s2, target, &expr).unwrap(),
                "94 ns/row at probe site must not admit on first completed weak decision"
            );
            assert_eq!(slow_probe.mode, Mode::Probe);
            assert_eq!(slow_probe.weak_confirmations, 0);
            assert_eq!(slow_probe.batch_probe_window, Some((288, 129, 4096)));
            // Fallback consumes that probe window as 1 confirmation, not 2.
            slow_probe.observe_fallback_with(
                4096,
                ColumnEval {
                    evaluated_rows: 4096,
                    elapsed_nanos: Some(4096 * 210),
                    split: None,
                    parallel_allowed: true,
                },
                Some((4096, 2696)),
                true,
                true,
            );
            assert_eq!(slow_probe.mode, Mode::Probe);
            assert_eq!(slow_probe.weak_confirmations, 1);
            // Next batch's probe window is the 2nd confirmation and admits at probe site.
            let mut s3 = batch(input, &repeat_keys);
            assert!(slow_probe.apply(&ctx, &mut s3, target, &expr).unwrap());
            assert_eq!(slow_probe.mode, Mode::Map);
            assert_eq!(slow_probe.admitted.map(|(batch, _)| batch), Some(3));
            assert_eq!(
                slow_probe.admitted.map(|(_, reason)| reason),
                Some("probe_weak")
            );
        });
    }

    #[test]
    fn fast_weak_admission_preserves_ordinary_guard_on_contention_only_evidence() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);

        let enter_large_contention = |reuse: &mut Reuse| {
            reuse
                .split
                .observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
            reuse
                .split
                .observe(4096, true, true, true, Some(split(100_000, 1_200_000)));
            assert!(reuse.split.is_contended(4096));
        };

        // 1. Contention with no cost history, single rate, mismatched bucket, or sub-100 ns
        //    rates all yield WeakDecision::Pass (never FastAdmit).
        let mut r = new_reuse(&ctx, &expr, None).unwrap();
        r.mode = Mode::Probe;
        enter_large_contention(&mut r);
        assert_eq!(r.weak_decision(4096, 512, 4096, true), WeakDecision::Pass);

        // Single high rate still lacks two usable same-bucket rates -> contention-only -> Pass.
        r.cost.record(4096, 4096 * 2_000);
        assert_eq!(r.weak_decision(4096, 512, 4096, true), WeakDecision::Pass);

        // Two rates in the wrong bucket (Small) -> contention-only for Large -> Pass.
        r.cost.record(1024, 1024 * 2_000);
        r.cost.record(1024, 1024 * 2_000);
        assert_eq!(r.weak_decision(4096, 512, 4096, true), WeakDecision::Pass);

        // Two cheap same-bucket rates (200 ns/row -> 25 ns/row saving) -> contention passes
        // ordinary guard only (Pass, not FastAdmit).
        r.cost.record(4096, 4096 * 200);
        r.cost.record(4096, 4096 * 200);
        assert_eq!(r.weak_decision(4096, 512, 4096, true), WeakDecision::Pass);

        // Contention in Retry phase behaves the same.
        r.split.large = BucketPhase::Retry;
        assert!(r.split.is_contended(4096));
        assert_eq!(r.weak_decision(4096, 512, 4096, true), WeakDecision::Pass);

        // 2. Fallback site on contention-only evidence requires two confirmations.
        let mut fb = new_reuse(&ctx, &expr, None).unwrap();
        fb.mode = Mode::Probe;
        enter_large_contention(&mut fb);
        let untimed = ColumnEval {
            evaluated_rows: 4096,
            elapsed_nanos: None,
            split: None,
            parallel_allowed: false,
        };
        fb.batch_index = 2;
        fb.batch_probe_window = Some((256, 64, 4096));
        fb.observe_fallback_with(4096, untimed, Some((4096, 3584)), true, true);
        assert_eq!(
            fb.mode,
            Mode::Probe,
            "contention-only fallback decision must keep two-confirmation guard"
        );
        assert_eq!(fb.weak_confirmations, 1);

        fb.batch_index = 3;
        fb.observe_fallback_with(4096, untimed, Some((4096, 3584)), true, true);
        assert_eq!(fb.mode, Mode::Map);
        assert_eq!(fb.admitted.map(|(batch, _)| batch), Some(3));
        assert_eq!(fb.admitted.map(|(_, reason)| reason), Some("counted_weak"));

        // 3. Probe site on contention-only evidence also requires two confirmations.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let strings = local_strings(&ctx, 0..16_384);
            let mut sampled_batch: Vec<Id> = strings
                .iter()
                .copied()
                .filter(|id| sampled(*id))
                .take(WINDOW as usize)
                .collect();
            sampled_batch.extend(
                strings
                    .iter()
                    .copied()
                    .filter(|id| !sampled(*id))
                    .take(4096 - WINDOW as usize),
            );
            let mut probe = new_reuse(&ctx, &expr, None).unwrap();
            let mut t1 = batch(input, &sampled_batch);
            assert!(!probe.apply(&ctx, &mut t1, target, &expr).unwrap());
            enter_large_contention(&mut probe);
            // First window on contention-only evidence must NOT fast-admit at probe site.
            let mut t2 = batch(input, &sampled_batch);
            assert!(!probe.apply(&ctx, &mut t2, target, &expr).unwrap());
            assert_eq!(probe.mode, Mode::Probe);
            assert_eq!(probe.weak_confirmations, 0);
            probe.observe_fallback_with(4096, untimed, None, true, true);
            assert_eq!(probe.mode, Mode::Probe);
            assert_eq!(probe.weak_confirmations, 1);
            // Second window admits under ordinary two-confirmation.
            let mut t3 = batch(input, &sampled_batch);
            assert!(probe.apply(&ctx, &mut t3, target, &expr).unwrap());
            assert_eq!(probe.mode, Mode::Map);
            assert_eq!(probe.admitted.map(|(_, reason)| reason), Some("probe_weak"));
        });

        // 4. Contended bucket that ALSO has two usable same-bucket rates predicting >= 100 ns/row
        //    is not penalized by contention and fast-admits on one decision.
        let mut both = new_reuse(&ctx, &expr, None).unwrap();
        both.mode = Mode::Probe;
        enter_large_contention(&mut both);
        both.cost.record(4096, 4096 * 900);
        both.cost.record(4096, 4096 * 800);
        assert_eq!(
            both.weak_decision(4096, 512, 4096, true),
            WeakDecision::FastAdmit
        );
    }

    /// Item 293 round 1: a result that the budget cannot intern while the map and its
    /// batch scratch hold their charges releases the optional state, and the ordinary
    /// evaluation of the keys left unevaluated, which fits the same budget, answers the
    /// batch.
    #[test]
    fn optional_map_yields_to_ordinary_evaluation_that_fits_the_budget() {
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        single.install(|| {
            let store = Store::in_memory(StoreOptions::default());
            let keys: Vec<Id> = (0..1024)
                .map(|row| Id::from_i64(1000 + (row % 512) as i64).unwrap())
                .collect();
            let expr_for = |ctx: &Ctx| {
                let input = ctx.var("input");
                (
                    input,
                    ctx.var("bound"),
                    Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]),
                )
            };
            let strings = |ctx: &Ctx, values: &[Id]| -> Vec<Option<Term>> {
                values.iter().map(|id| ctx.term(*id)).collect()
            };
            // Ordinary evaluation alone fits the budget.
            let ordinary = cursor_context(&store, 100 << 10);
            let (input, _, expr) = expr_for(&ordinary);
            let values = exec::column_rows(&ordinary, &batch(input, &keys), &expr).unwrap();
            ordinary.check().unwrap();
            let expected = strings(&ordinary, &values);
            assert_eq!(
                expected[0],
                Some(Term::Literal(Literal::new_simple_literal("1000")))
            );

            let ctx = cursor_context(&store, 100 << 10);
            let (input, target, expr) = expr_for(&ctx);
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            let mut table = batch(input, &keys);
            let reused = reuse
                .apply(&ctx, &mut table, target, &expr)
                .expect("the optional map must not refuse a batch that ordinary evaluation fits");
            ctx.check().unwrap();
            let values = if reused {
                table.cols[1].clone()
            } else {
                assert_eq!(reuse.mode, Mode::Off);
                assert!(reuse.values.is_empty());
                // The cursor's own fallback, with the expression cache's batch pass.
                let mut report = crate::sparql::exprcache::Report::default();
                exec::compute_column_controlled(
                    &ctx,
                    &table,
                    &expr,
                    &mut report,
                    |n| reuse.allow_fallback_split(n, &expr),
                    |n| reuse.want_cost_timing(n, &expr),
                )
                .unwrap()
                .0
            };
            ctx.check().unwrap();
            assert_eq!(strings(&ctx, &values), expected);
        });
    }

    /// Item 293 round 2: a refusal that the evaluation of the misses meets before its
    /// result, here the interning of 512 `xsd:short` literals of about 72 KiB beside
    /// the map's batch scratch, releases the optional state without failing the query.
    /// The ordinary evaluation of the batch fits the same 100 KiB budget, both for the
    /// derived literal as the result and for an integer result computed from it.
    #[test]
    fn optional_map_yields_refusals_met_before_the_result() {
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        single.install(|| {
            let store = Store::in_memory(StoreOptions::default());
            let keys: Vec<Id> = (0..1024)
                .map(|row| Id::from_i64(1000 + (row % 512) as i64).unwrap())
                .collect();
            type Build = fn(VarId) -> Expr;
            let cases: [(&str, Build); 2] = [
                ("xsd:short", |input| {
                    Expr::Call(
                        Func::Cast(xsd::SHORT.into_owned()),
                        vec![Expr::Var(input)],
                    )
                }),
                ("xsd:integer(xsd:short)", |input| {
                    Expr::Call(
                        Func::Cast(xsd::INTEGER.into_owned()),
                        vec![Expr::Call(
                            Func::Cast(xsd::SHORT.into_owned()),
                            vec![Expr::Var(input)],
                        )],
                    )
                }),
            ];
            for (name, build) in cases {
                let terms = |ctx: &Ctx, values: &[Id]| -> Vec<Option<Term>> {
                    values.iter().map(|id| ctx.term(*id)).collect()
                };
                let ordinary = cursor_context(&store, 100 << 10);
                let input = ordinary.var("input");
                let expr = build(input);
                let values = exec::column_rows(&ordinary, &batch(input, &keys), &expr).unwrap();
                ordinary.check().unwrap();
                let expected = terms(&ordinary, &values);
                assert!(expected.iter().all(Option::is_some), "{name}");

                let ctx = cursor_context(&store, 100 << 10);
                let input = ctx.var("input");
                let target = ctx.var("bound");
                let expr = build(input);
                let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
                let mut table = batch(input, &keys);
                let reused = reuse.apply(&ctx, &mut table, target, &expr).unwrap_or_else(|error| {
                    panic!("{name}: the optional map must not refuse a batch that ordinary evaluation fits: {error}")
                });
                ctx.check().unwrap_or_else(|error| {
                    panic!("{name}: the optional map latched a refusal: {error}")
                });
                assert!(reused, "{name}: the batch is answered after the refusal");
                assert_eq!(
                    reuse.refusals.len(),
                    1,
                    "{name}: the map cannot hold this batch's intermediates"
                );
                let (finished, evaluated) = reuse.refusals[0];
                assert_eq!(finished + evaluated, 512, "{name}: each key is evaluated once");
                assert!(evaluated > 0, "{name}");
                assert_eq!(reuse.mode, Mode::Off, "{name}");
                assert!(reuse.values.is_empty(), "{name}");
                assert!(reuse.charge.is_none(), "{name}");
                assert_eq!(terms(&ctx, &table.cols[1]), expected, "{name}");
            }
        });
    }

    /// Item 293 round 3: a geometry constructor keeps its result's charge until the
    /// query ends, so misses that a refusal discards would leave their charges behind
    /// for the ordinary evaluation of the batch. The map does not admit such
    /// expressions, and a batch whose ordinary evaluation fits the budget with a few
    /// KiB to spare is answered the same way with reuse enabled.
    #[cfg(feature = "geo")]
    #[test]
    fn geometry_constructors_stay_out_of_the_optional_map() {
        use crate::geo::vocab::{GEOF, SPATIALF};
        use crate::sparql::expr::ArithOp;
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        single.install(|| {
            let store = Store::in_memory(StoreOptions::default());
            let keys: Vec<Id> = (0..4096)
                .map(|row| Id::from_i64((row % 2048) as i64).unwrap())
                .collect();
            let int = |n: i64| Expr::Const(Id::from_i64(n).unwrap());
            let call = |iri: String, args: Vec<Expr>| Expr::Call(Func::Ext(iri), args);
            let builtin = |f: Function, args: Vec<Expr>| Expr::Call(Func::Builtin(f), args);
            // STRLEN(STR(spatialF:convertLatLon(?input / 40, 0))) is an inline integer, so
            // the result interns nothing and only the constructed point is charged.
            let point = |input: VarId| {
                call(
                    format!("{SPATIALF}convertLatLon"),
                    vec![
                        Expr::Arith(Box::new(Expr::Var(input)), Box::new(int(40)), ArithOp::Div),
                        int(0),
                    ],
                )
            };
            let build = |input: VarId| {
                builtin(
                    Function::StrLen,
                    vec![builtin(Function::Str, vec![point(input)])],
                )
            };
            let ordinary = |memory: u64| {
                let ctx = cursor_context(&store, memory);
                let input = ctx.var("input");
                let values = exec::column_rows(&ctx, &batch(input, &keys), &build(input));
                values.ok().filter(|_| ctx.check().is_ok())
            };
            // The smallest budget that the ordinary evaluation fits, to 256 bytes.
            let (mut refused, mut fits) = (0u64, 4u64 << 20);
            assert!(ordinary(fits).is_some());
            while fits - refused > 256 {
                let mid = refused + (fits - refused) / 2;
                if ordinary(mid).is_some() {
                    fits = mid;
                } else {
                    refused = mid;
                }
            }
            // Each row constructs a point of about 120 charged bytes.
            assert!(fits >= 256 << 10, "the batch constructs its points: {fits}");
            let budget = fits + (4 << 10);
            let expected = ordinary(budget).unwrap();
            assert!(expected.iter().all(|id| *id != Id::UNDEF));

            let ctx = cursor_context(&store, budget);
            let input = ctx.var("input");
            let target = ctx.var("bound");
            let expr = build(input);
            let mut reuse = new_reuse(&ctx, &expr, None);
            let mut table = batch(input, &keys);
            let reused = match reuse.as_mut() {
                Some(reuse) => reuse.apply(&ctx, &mut table, target, &expr).expect(
                    "the optional map must not refuse a batch that ordinary evaluation fits",
                ),
                None => false,
            };
            ctx.check()
                .expect("the optional map must not latch a refusal");
            let values = if reused {
                table.cols[1].clone()
            } else {
                let mut report = crate::sparql::exprcache::Report::default();
                let reuse = reuse.as_ref();
                exec::compute_column_controlled(
                    &ctx,
                    &table,
                    &expr,
                    &mut report,
                    |n| reuse.is_some_and(|reuse| reuse.allow_fallback_split(n, &expr)),
                    |n| reuse.is_some_and(|reuse| reuse.want_cost_timing(n, &expr)),
                )
                .expect("ordinary evaluation fits the budget")
                .0
            };
            ctx.check()
                .expect("the ordinary evaluation after the map fits the budget");
            assert_eq!(values, expected);

            // Every geometry constructor stays out, however deep, and so do the calls
            // that can select one at runtime. A geometry function that constructs
            // nothing is still admitted.
            let ctx = cursor_context(&store, 4 << 20);
            let input = ctx.var("input");
            let wkt = || call(format!("{GEOF}asWKT"), vec![point(input)]);
            for (name, expr) in [
                ("convertLatLon", point(input)),
                ("STRLEN(STR(convertLatLon))", build(input)),
                (
                    "envelope",
                    call(format!("{GEOF}envelope"), vec![Expr::Var(input)]),
                ),
                ("asWKT", wkt()),
                (
                    "IF(BOUND, transform, 0)",
                    Expr::If(
                        Box::new(Expr::Bound(input)),
                        Box::new(call(
                            format!("{GEOF}transform"),
                            vec![Expr::Var(input), int(0)],
                        )),
                        Box::new(int(0)),
                    ),
                ),
            ] {
                let eligible = crate::sparql::exprcache::eligible(&[&expr]);
                assert_eq!(eligible.ok().flatten(), Some(input), "{name}");
                assert!(Reuse::prepare(&ctx, &expr, None).is_none(), "{name}");
            }
            let dimension = call(format!("{GEOF}dimension"), vec![Expr::Var(input)]);
            assert!(Reuse::prepare(&ctx, &dimension, None).is_some());
        });
    }

    /// Runs the batch that `build` makes on its context through the map and its ordinary
    /// fallback at budgets around the smallest that its evaluation fits, and checks each
    /// run against the cursor's evaluation of the batch without the map. A run that
    /// succeeds must match its answers, query-local terms and plan warnings, so a
    /// refusal that the misses meet leaves nothing behind that the evaluation without
    /// the map would not. A run that fails must fail without the map at the same budget,
    /// since a declined map keeps no charge and does not evaluate a finished miss again.
    /// Returns how many successful runs declined the map after evaluating its misses.
    fn refused_misses_with_no_skipped_arm(
        name: &str,
        build: impl Fn(&Ctx) -> (VarId, Expr, Vec<Id>) + Sync,
    ) -> usize {
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        single.install(|| {
            let store = Store::in_memory(StoreOptions::default());
            let terms = |ctx: &Ctx, values: &[Id]| -> Vec<Option<Term>> {
                values.iter().map(|id| ctx.term(*id)).collect()
            };
            let codes = |ctx: &Ctx| -> Vec<&'static str> {
                ctx.warnings().iter().map(|w| w.code).collect()
            };
            // The cursor's evaluation of the batch without the map, or with `rows`, the
            // row-by-row evaluation that sets the lower end of the budgets.
            let ordinary = |memory: u64, rows: bool| {
                let ctx = cursor_context(&store, memory);
                let (input, expr, keys) = build(&ctx);
                let table = batch(input, &keys);
                let values = if rows {
                    exec::column_rows(&ctx, &table, &expr).ok()?
                } else {
                    let mut report = crate::sparql::exprcache::Report::default();
                    exec::compute_column_controlled(
                        &ctx,
                        &table,
                        &expr,
                        &mut report,
                        |_| false,
                        |_| false,
                    )
                    .ok()?
                    .0
                };
                ctx.check().ok()?;
                Some((ctx, values))
            };
            let smallest = |rows: bool| {
                let (mut refused, mut fits) = (0u64, 16u64 << 20);
                assert!(ordinary(fits, rows).is_some(), "{name}");
                while fits - refused > 256 {
                    let mid = refused + (fits - refused) / 2;
                    if ordinary(mid, rows).is_some() {
                        fits = mid;
                    } else {
                        refused = mid;
                    }
                }
                fits
            };
            let (plain, expected) = ordinary(16 << 20, false).unwrap();
            let expected = (terms(&plain, &expected), plain.local_len(), codes(&plain));
            let (low, high) = (smallest(true), smallest(false) + (64 << 10));
            let step = usize::try_from(((high - low) / 512).max(512)).unwrap();
            let mut declined = 0;
            for budget in (low..high).step_by(step) {
                let ctx = cursor_context(&store, budget);
                let (input, expr, keys) = build(&ctx);
                let target = ctx.var("bound");
                let mut reuse = new_reuse(&ctx, &expr, None)
                    .unwrap_or_else(|| panic!("{name}: the map admits the expression"));
                let mut table = batch(input, &keys);
                let run = reuse.apply(&ctx, &mut table, target, &expr).and_then(|reused| {
                    ctx.check()?;
                    let values = if reused {
                        table.cols[1].clone()
                    } else {
                        let mut report = crate::sparql::exprcache::Report::default();
                        exec::compute_column_controlled(
                            &ctx,
                            &table,
                            &expr,
                            &mut report,
                            |n| reuse.allow_fallback_split(n, &expr),
                            |n| reuse.want_cost_timing(n, &expr),
                        )?
                        .0
                    };
                    ctx.check()?;
                    Ok((reused, values))
                });
                let (reused, values) = match run {
                    Ok(run) => run,
                    Err(error) => {
                        assert!(
                            ordinary(budget, false).is_none(),
                            "{name} at {budget}: the map fails a batch that the evaluation without it fits: {error}"
                        );
                        continue;
                    }
                };
                if reuse.mode == Mode::Off && reuse.evaluations > 0 {
                    declined += 1;
                    assert!(reuse.charge.is_none(), "{name} at {budget}");
                }
                // A batch whose misses met a refusal evaluates each distinct key once,
                // by the map or after it.
                let distinct = keys.iter().collect::<FxHashSet<_>>().len();
                for (finished, evaluated) in &reuse.refusals {
                    assert!(reused, "{name} at {budget}");
                    assert_eq!(
                        finished + evaluated,
                        distinct,
                        "{name} at {budget}: a finished miss was evaluated again"
                    );
                }
                let found = (terms(&ctx, &values), ctx.local_len(), codes(&ctx));
                assert_eq!(found.0, expected.0, "{name} at {budget}");
                assert_eq!(
                    found.1, expected.1,
                    "{name} at {budget}: a refused miss interned a term that the evaluation without the map does not"
                );
                assert_eq!(
                    found.2, expected.2,
                    "{name} at {budget}: a refused miss raised a warning that the evaluation without the map does not"
                );
            }
            declined
        })
    }

    fn simple(ctx: &Ctx, value: &str) -> Expr {
        Expr::Const(ctx.intern_term(&Term::Literal(Literal::new_simple_literal(value))))
    }

    /// `CONCAT("fallback-", STR(?input))`, an arm that the ordinary evaluation of the
    /// tests below never takes and whose result is a new query-local term.
    fn fallback(ctx: &Ctx, input: VarId) -> Expr {
        Expr::Call(
            Func::Builtin(Function::Concat),
            vec![
                simple(ctx, "fallback-"),
                Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]),
            ],
        )
    }

    fn integer_keys() -> Vec<Id> {
        (0..1024)
            .map(|row| Id::from_i64(1000 + (row % 512) as i64).unwrap())
            .collect()
    }

    /// `sameTerm(CONCAT(STR(?input), "a"…), CONCAT(STR(?input), "b"…))` with 1 KiB of
    /// padding. Its two sides are distinct new terms, so it is false, but when the budget
    /// refuses both interns each side is `UNDEF`, and a `sameTerm` that does not give up
    /// would answer true.
    fn padded_same_term(ctx: &Ctx, input: VarId) -> Expr {
        let padded = |pad: &str| {
            Expr::Call(
                Func::Builtin(Function::Concat),
                vec![
                    Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]),
                    simple(ctx, &pad.repeat(1024)),
                ],
            )
        };
        Expr::SameTerm(Box::new(padded("a")), Box::new(padded("b")))
    }

    /// `IF(sameTerm(…), ?input / 0, 1)`, which is 1 and whose other arm fails.
    fn one_unless_same_term(ctx: &Ctx, input: VarId) -> Expr {
        Expr::If(
            Box::new(padded_same_term(ctx, input)),
            Box::new(Expr::Arith(
                Box::new(Expr::Var(input)),
                Box::new(Expr::Const(Id::from_i64(0).unwrap())),
                crate::sparql::expr::ArithOp::Div,
            )),
            Box::new(Expr::Const(Id::from_i64(1).unwrap())),
        )
    }

    /// Item 293 round 4: once the budget refuses an intern of `COALESCE`'s first arm
    /// during the misses, `COALESCE` gives up as it does after a latched refusal, and
    /// does not evaluate and intern the fallback arm that the evaluation without the map
    /// never reaches. A first arm with a large regular expression did not serve here,
    /// because no budget in the sweep both declined the map after its misses and fitted
    /// the batch.
    #[test]
    fn a_refused_coalesce_arm_does_not_evaluate_the_fallback() {
        let declined =
            refused_misses_with_no_skipped_arm("COALESCE(IF(sameTerm), CONCAT)", |ctx| {
                let input = ctx.var("input");
                let expr =
                    Expr::Coalesce(vec![one_unless_same_term(ctx, input), fallback(ctx, input)]);
                (input, expr, integer_keys())
            });
        assert!(
            declined > 0,
            "some budget declines the map after its misses"
        );
    }

    /// Item 293 round 4: `sameTerm` gives up after a refusal during the misses instead
    /// of answering true and sending `IF` to the arm that the evaluation without the map
    /// never takes.
    #[test]
    fn a_refused_same_term_does_not_choose_the_other_if_arm() {
        let declined = refused_misses_with_no_skipped_arm("IF(sameTerm, CONCAT, 1)", |ctx| {
            let input = ctx.var("input");
            let expr = Expr::If(
                Box::new(padded_same_term(ctx, input)),
                Box::new(fallback(ctx, input)),
                Box::new(Expr::Const(Id::from_i64(1).unwrap())),
            );
            (input, expr, integer_keys())
        });
        assert!(
            declined > 0,
            "some budget declines the map after its misses"
        );
    }

    /// Item 293 round 5: a refusal late in the misses does not make the batch repeat an
    /// early transient allocation after the later rows' results are retained. The first
    /// row is a query-local triple with a 16 KiB subject, whose `OBJECT` charges about
    /// 128 KiB to a decode that it drops at once. The other rows are 799 distinct
    /// integers and 224 repeats of them, each building a new string of about 1 KiB. The
    /// evaluation without the map meets the decode first and then retains the strings.
    /// A map that refused the intern of a late string and then evaluated the whole batch
    /// again would meet the decode with most of the strings retained.
    #[test]
    fn a_refused_miss_does_not_repeat_an_earlier_transient_allocation() {
        let declined = refused_misses_with_no_skipped_arm("IF(isTRIPLE, OBJECT, CONCAT)", |ctx| {
            let input = ctx.var("input");
            let triple = ctx.intern_term(&Term::Triple(Box::new(oxrdf::Triple::new(
                oxrdf::NamedNode::new_unchecked(format!(
                    "http://example.org/{}",
                    "s".repeat(16 << 10)
                )),
                oxrdf::NamedNode::new_unchecked("http://example.org/p"),
                Literal::from(7i64),
            ))));
            assert_eq!(triple.tag(), Tag::Local);
            let integers: Vec<Id> = (0..799).map(|n| Id::from_i64(1000 + n).unwrap()).collect();
            let mut keys = vec![triple];
            keys.extend(&integers);
            keys.extend(&integers[..224]);
            let builtin = |f: Function, args: Vec<Expr>| Expr::Call(Func::Builtin(f), args);
            let expr = Expr::If(
                Box::new(builtin(Function::IsTriple, vec![Expr::Var(input)])),
                Box::new(builtin(Function::Object, vec![Expr::Var(input)])),
                Box::new(builtin(
                    Function::Concat,
                    vec![
                        builtin(Function::Str, vec![Expr::Var(input)]),
                        simple(ctx, &"p".repeat(1024)),
                    ],
                )),
            );
            (input, expr, keys)
        });
        assert!(
            declined > 0,
            "some budget declines the map after its misses"
        );
    }

    /// Item 293 round 5: when the misses run on several workers and a refusal stops them,
    /// each miss that finished holds its ordinary value, and every other miss is marked
    /// unevaluated rather than holding a refused or skipped `UNDEF`.
    #[test]
    fn parallel_misses_that_meet_a_refusal_keep_only_finished_values() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        pool.install(|| {
            let store = Store::in_memory(StoreOptions::default());
            let rows = exec::PAR_THRESHOLD + 4096;
            let build = |ctx: &Ctx| {
                let input = ctx.var("input");
                let expr = Expr::Call(
                    Func::Builtin(Function::Concat),
                    vec![
                        Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]),
                        simple(ctx, &"p".repeat(256)),
                    ],
                );
                let keys: Vec<Id> = (0..rows).map(|n| Id::from_i64(n as i64).unwrap()).collect();
                (batch(input, &keys), expr)
            };
            let plain = cursor_context(&store, 64 << 20);
            let (table, expr) = build(&plain);
            let expected = exec::column_rows(&plain, &table, &expr).unwrap();
            plain.check().unwrap();

            let ctx = cursor_context(&store, 2 << 20);
            let (table, expr) = build(&ctx);
            assert!(expr.parallel() && table.len() > exec::PAR_THRESHOLD);
            let column =
                exec::column_rows_split_optional(&ctx, &table, &expr, true, UNEVALUATED).unwrap();
            ctx.check()
                .expect("a refusal of the misses is not the query's failure");
            let OptionalColumn::Refused(values) = column else {
                panic!("the budget refuses some of the misses");
            };
            let finished = values.iter().filter(|v| **v != UNEVALUATED).count();
            assert!(finished > 0 && finished < rows, "{finished} of {rows}");
            for (row, value) in values.iter().enumerate() {
                if *value != UNEVALUATED {
                    assert_ne!(*value, Id::UNDEF, "row {row}");
                    assert_eq!(ctx.term(*value), plain.term(expected[row]), "row {row}");
                }
            }
        });
    }

    /// Item 293 round 4: the geometry arm of `COALESCE` after a refusal in its first arm
    /// would parse an EPSG:27700 point and raise `geo-crs-approximate`, which the
    /// evaluation without the map never does.
    #[cfg(feature = "geo-epsg")]
    #[test]
    fn a_refused_coalesce_arm_raises_no_crs_warning() {
        use crate::geo::vocab::{GEOF, WKT_LITERAL};
        let points = |ctx: &Ctx| -> Vec<Id> {
            let points: Vec<Id> = (0..512)
                .map(|n| {
                    ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                        format!(
                            "<http://www.opengis.net/def/crs/EPSG/0/27700> POINT({} 100000)",
                            400_000 + n
                        ),
                        oxrdf::NamedNode::new_unchecked(WKT_LITERAL),
                    )))
                })
                .collect();
            (0..1024).map(|row| points[row % 512]).collect()
        };
        let dimension = |input: VarId| {
            Expr::Call(
                Func::Ext(format!("{GEOF}dimension")),
                vec![Expr::Var(input)],
            )
        };
        let declined =
            refused_misses_with_no_skipped_arm("COALESCE(IF(sameTerm), geof:dimension)", |ctx| {
                let input = ctx.var("input");
                let expr = Expr::Coalesce(vec![one_unless_same_term(ctx, input), dimension(input)]);
                (input, expr, points(ctx))
            });
        assert!(
            declined > 0,
            "some budget declines the map after its misses"
        );
    }

    /// Item 293 round 4: a derived integer cast whose literal the budget refuses inside
    /// optional evaluation fails, so two refused casts of different values do not
    /// compare equal on their `UNDEF` ids.
    #[test]
    fn refused_derived_integer_casts_do_not_compare_equal() {
        use crate::sparql::expr::{Row, eval};
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 1 << 20);
        let short = |n: i64| {
            Expr::Call(
                Func::Cast(xsd::SHORT.into_owned()),
                vec![Expr::Const(Id::from_i64(n).unwrap())],
            )
        };
        let expr = Expr::Eq(Box::new(short(1000)), Box::new(short(1001)));
        let table = Table::new(Vec::new());
        let row = Row {
            table: &table,
            i: 0,
            map: &[],
            dec: None,
            exists: None,
        };
        let held = ctx.charge(ctx.memory_room() - 16).unwrap();
        let (value, refused) =
            ctx.optional_eval(|| eval(&expr, &row, &ctx).map(|v| v.into_id(&ctx)));
        assert!(refused, "the budget refuses the casts' literals");
        assert!(
            value.is_err(),
            "refused casts must not compare equal: {value:?}"
        );
        drop(held);
        ctx.check().unwrap();
        let equal = eval(&expr, &row, &ctx).map(|v| v.into_id(&ctx));
        assert!(
            matches!(equal, Ok(id) if id == Id::from_bool(false)),
            "{equal:?}"
        );
    }

    /// Item 293 round 1: an admitted map whose batch scratch the budget refuses releases
    /// its retained entries before the batch falls back to ordinary evaluation.
    #[test]
    fn a_refused_batch_scratch_releases_the_admitted_map() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 4 << 20);
        let input = ctx.var("input");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let keys: Vec<Id> = (0..4096)
            .map(|row| Id::from_i64((row % 1024) as i64).unwrap())
            .collect();
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(
            reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        assert_eq!(reuse.mode, Mode::Map);
        assert_eq!(reuse.values.len(), 1024);
        let held = reuse.charge.as_ref().unwrap().bytes();
        assert!(held > 512);
        let before = ctx.live_bytes();
        // Leave less room than the batch's scratch of 68 bytes a row.
        let _other = ctx
            .charge(ctx.memory_remaining().saturating_sub(64 << 10))
            .unwrap();
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        assert_eq!(reuse.mode, Mode::Off);
        assert!(reuse.values.is_empty());
        assert!(reuse.charge.is_none());
        drop(_other);
        assert!(ctx.live_bytes() < before, "the map's entries are released");
        ctx.check().unwrap();
    }

    /// Item 293 round 1: with batches whose keys are all distinct, a batch that completes
    /// no probe window keeps the confirmation of the window before it, so two windows in
    /// the 50 to 100 ns a row band admit across an intermediate batch.
    #[test]
    fn a_batch_without_a_probe_window_keeps_the_previous_window_confirmation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        reuse.mode = Mode::Probe;
        reuse.cost.record(2048, 2048 * 300);
        reuse.cost.record(2048, 2048 * 300);
        let eval = ColumnEval {
            evaluated_rows: 2048,
            elapsed_nanos: None,
            split: None,
            parallel_allowed: true,
        };
        // One sampled row in four repeats: a 75 ns a row saving, which needs two windows.
        assert_eq!(reuse.weak_decision(256, 64, 2048, true), WeakDecision::Pass);
        reuse.batch_index = 2;
        reuse.batch_probe_window = Some((256, 64, 2048));
        reuse.observe_fallback_with(2048, eval, Some((2048, 2048)), true, true);
        assert_eq!(reuse.mode, Mode::Probe);
        reuse.batch_index = 3;
        reuse.observe_fallback_with(2048, eval, Some((2048, 2048)), true, true);
        assert_eq!(reuse.mode, Mode::Probe);
        reuse.batch_index = 4;
        reuse.batch_probe_window = Some((256, 64, 2048));
        reuse.observe_fallback_with(2048, eval, Some((2048, 2048)), true, true);
        assert_eq!(reuse.mode, Mode::Map);
        assert_eq!(reuse.admitted, Some((4, "probe_weak")));

        // A failed window still resets the window chain.
        let mut failed = new_reuse(&ctx, &expr, None).unwrap();
        failed.mode = Mode::Probe;
        failed.cost.record(2048, 2048 * 300);
        failed.cost.record(2048, 2048 * 300);
        failed.batch_probe_window = Some((256, 64, 2048));
        failed.observe_fallback_with(2048, eval, Some((2048, 2048)), true, true);
        failed.batch_probe_window = Some((256, 8, 2048));
        failed.observe_fallback_with(2048, eval, Some((2048, 2048)), true, true);
        failed.batch_probe_window = Some((256, 64, 2048));
        failed.observe_fallback_with(2048, eval, Some((2048, 2048)), true, true);
        assert_eq!(failed.mode, Mode::Probe);
    }

    /// Item 293 round 1: a sparse inline integer batch leaves the map in its pilot, which
    /// records no rates, so its fallback takes no clock reads.
    #[test]
    fn a_map_in_its_pilot_takes_no_cost_timing() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            let keys: Vec<Id> = (0..4096)
                .map(|row| Id::from_i64(row * 1000).unwrap())
                .collect();
            assert!(
                !reuse
                    .apply(&ctx, &mut batch(input, &keys), target, &expr)
                    .unwrap()
            );
            assert_eq!(reuse.mode, Mode::Pilot);
            assert!(!reuse.want_cost_timing(4096, &expr));
            reuse.mode = Mode::Probe;
            assert!(reuse.want_cost_timing(4096, &expr));
        });
    }

    /// Item 293 round 1: small batches of a parallel-capable expression feed the sketch
    /// without growing the window, and a sketch they fill ends the probe.
    #[test]
    fn small_parallel_batches_feed_only_the_sketch_and_can_exhaust_it() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            reuse.mode = Mode::Probe;
            reuse.max_entries = MAP_ENTRIES;
            let strings = local_strings(&ctx, 0..(SKETCH_KEYS * 24));
            let sampled_keys: Vec<Id> = strings
                .iter()
                .copied()
                .filter(|id| sampled(*id))
                .take(SKETCH_KEYS + 512)
                .collect();
            assert_eq!(sampled_keys.len(), SKETCH_KEYS + 512);
            for chunk in sampled_keys.chunks(512) {
                assert!(
                    !reuse
                        .apply(&ctx, &mut batch(input, chunk), target, &expr)
                        .unwrap()
                );
                assert_eq!(reuse.window, (0, 0));
                if reuse.exhausted {
                    break;
                }
            }
            assert!(reuse.exhausted, "a full sketch ends the probe");
            assert!(
                !reuse
                    .apply(&ctx, &mut batch(input, &sampled_keys[..512]), target, &expr)
                    .unwrap()
            );
            assert_eq!(reuse.mode, Mode::Off);
        });
    }

    /// Item 293 round 1: the pilot seeds the sketch with its sampled keys before it
    /// observes the rest of a large first batch, whose repeats of pilot keys count.
    #[test]
    fn the_pilot_counts_repeats_of_its_keys_in_the_rest_of_the_batch() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let strings = local_strings(&ctx, 0..8192);
            let mut keys = strings[..4096].to_vec();
            keys.extend_from_slice(&strings[..4096]);
            let pilot_sampled = keys[..4096].iter().filter(|id| sampled(**id)).count() as u32;
            assert!(pilot_sampled > 0);
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            assert!(
                !reuse
                    .apply(&ctx, &mut batch(input, &keys), target, &expr)
                    .unwrap()
            );
            assert_eq!(reuse.mode, Mode::Probe);
            assert_eq!(
                reuse.window,
                (pilot_sampled, pilot_sampled),
                "every sampled row after the pilot repeats a pilot key"
            );
        });
    }

    /// Item 293 round 1: the pull that returns a BIND's last rows releases its map, so
    /// the operators above it have that memory for the rest of their work.
    #[test]
    fn the_pull_that_ends_the_input_releases_the_map_with_the_last_rows() {
        use super::super::{CursorOptions, FallbackPolicy, Operator};
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("input");
        let target = ctx.var("bound");
        let mut values = Table::new(vec![input]);
        values.cols[0] = (0..4096)
            .map(|row| Id::from_i64((row % 1024) as i64).unwrap())
            .collect();
        values.len = 4096;
        let mut bind = Node::leaf(
            Kind::Extend(
                target,
                Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]),
            ),
            vec![input, target],
            4096.0,
            String::new(),
        );
        bind.children.push(Node::leaf(
            Kind::Values(values),
            vec![input],
            4096.0,
            String::new(),
        ));
        let mut operator =
            Operator::build(&ctx, bind, FallbackPolicy::RejectMaterialization).unwrap();
        let options = CursorOptions {
            batch_rows: 4096,
            ..Default::default()
        };
        let batch = operator.next(&ctx, &options, 4096).unwrap().unwrap();
        assert_eq!(batch.table.len, 4096);
        assert!(operator.done, "the input ends with this batch");
        let reuse = operator.bind_reuse.as_ref().unwrap();
        assert!(
            reuse.counters().is_some(),
            "the batch was answered from the map"
        );
        assert!(reuse.charge.is_none());
        assert!(reuse.values.is_empty());
        drop(batch);
        assert!(operator.next(&ctx, &options, 4096).unwrap().is_none());
        ctx.check().unwrap();
    }
}
