//! OWL 2 EL classification (spec F24): the inferred class hierarchy and the
//! unsatisfiable classes of an ontology, computed by a consequence-based, concurrent
//! calculus in the style of ELK (Kazakov, Krötzsch and Simančík, *The Incredible ELK*,
//! JAR 2014), and kept up to date incrementally as axioms are added and removed.
//!
//! The classifier reads the F21 axiom model only. It covers OWL 2 EL with the
//! rules of EL++ (Baader, Brandt and Lutz, IJCAI 2005 and OWLED 2008): individuals in
//! class expressions and assertions, ranges, the datatypes, literals and data ranges of
//! the profile, functional data properties, keys, negative property assertions and the
//! top and bottom properties. A class whose instances reach an individual through a
//! context that holds only where the class has an instance is classified again in a
//! world of its own.
//!
//! Beyond OWL 2 EL, the classifier takes the Horn axioms of OWL 2 DL with the rules of
//! Kazakov's calculus for Horn-SHIQ (IJCAI 2009), roadmap item 603's Tier H: inverse
//! roles, universal restrictions, at-most-one and at-least restrictions, functional and
//! inverse-functional roles, self restrictions, chains and transitivity under universals,
//! and disjoint, asymmetric and irreflexive roles. The Horn rules run as their own
//! instantiation of the saturation, which creates the contexts of conjunctions that the
//! index does not hold, and beside the rules of individuals, universal roles and
//! functional data roles when the ontology has them (roadmap item 692), with two rules
//! over the predecessors of a context that stands for one element and the worlds over
//! the conjunction contexts. An axiom the
//! classifier does not take is left out and reported, with the rule of the OWL 2 Profiles
//! recommendation it breaks, and the run's [`Stats`] count the axioms outside OWL 2 EL by
//! whether they are Horn, give the [`Tier`] of rules the ontology needs and count the
//! axioms the Horn rules take. An axiom with a literal or a
//! datatype that the classifier cannot compare, or with a property chain through the top
//! property, is used as far as the calculus goes and reported as partial. Every
//! subsumption the classifier answers is entailed. It derives every entailed subsumption
//! between named classes when no axiom is reported as partial or left out, except in a
//! rooted run ([`Limits::rooted`], off by default), whose calculus departs from the
//! published rules and whose completeness is not proved, so that its finished answer and
//! `complete` say only that the run ended and left nothing out.
//!
//! [`Saturation`] keeps the saturated contexts between runs over axioms that the caller
//! keeps, such as those of an open ontology, and reads them in place on each run.
//! [`Classifier`] holds its own copy of the axioms on top of one. A run after a change
//! saturates again only the contexts that the changed axioms can reach, and a change to
//! the role hierarchy, chains, reflexivity or ranges, to an axiom with an individual, a
//! data property, a datatype or a key, and any change after a run that merged a context
//! with an individual saturates everything.
//!
//! A run with the Horn rules without individuals, and a run of the disjunctive rules
//! with the cautious or eager strategy and without counting or a root, keep their
//! contexts too (roadmap item 753). The next run compares its index with the last one and
//! clears
//! the contexts that hold an expression whose rules differ, with every context that
//! links to a cleared one. Its answer is that of a run that starts again. Such a run
//! starts again itself when the change is to more than class axioms and domains over
//! classes and object restrictions, when the rules the ontology needs change, and once
//! it keeps more than twice the expressions, conjunctions, tests or clauses of the
//! last run that started again. Runs with a bound on their derived conclusions, which
//! a run that counts and a rooted run have, start again after every change.
//!
//! The profile check, the taxonomy and the saturation run on the threads of
//! [`Limits`], and the answer is the same for every number of threads.
//!
//! ```
//! use sparkles_owl::{classify::Classifier, model::*};
//! let c = |s: &str| Class::new(format!("http://ex.org/{s}"));
//! let part_of = ObjectProperty::new("http://ex.org/partOf");
//! let mut k = Classifier::new();
//! k.add([
//!     &Axiom::sub_class_of(c("Nucleus"), ClassExpression::some(part_of.clone(), c("Cell"))),
//!     &Axiom::equivalent_classes([
//!         c("CellPart").into(),
//!         ClassExpression::some(part_of.clone(), c("Cell")),
//!     ]),
//! ]);
//! let out = k.classify(&Default::default()).unwrap();
//! assert!(out.taxonomy.is_subclass(&c("Nucleus"), &c("CellPart")));
//! ```

#[cfg(test)]
mod admission_test;
mod data;
mod dl;
mod dl_clause;
mod dl_eq;
mod dl_norm;
mod dl_reduce;
mod dl_root;
mod dl_rules;
mod dt_sat;
#[cfg(test)]
mod dt_wiring_tests;
mod dtcheck;
mod horn;
#[cfg(test)]
mod horn_range_tests;
mod horn_rules;
mod index;
mod numeric;
mod polled;
mod query;
#[cfg(test)]
mod retained_accounting_tests;
#[cfg(test)]
mod rooted_adaptive_tests;
mod saturate;
#[cfg(test)]
mod scratch_admission_tests;
#[cfg(test)]
mod shared_world_found_row_tests;
mod taxonomy;
#[cfg(test)]
mod tierd_memory_tests;
mod top_chain;
#[cfg(test)]
mod top_chain_cases;
#[cfg(test)]
mod top_chain_diagnostic;
mod world;
#[cfg(test)]
mod world_retirement_mixed_tests;

pub use dl::{OutsideEl, Tier};
#[doc(hidden)]
pub use dl_root::marker_test_mode;
pub use index::Gap;
pub use query::Description;
pub use taxonomy::{Node as TaxonomyNode, Taxonomy};

use crate::model::*;
use crate::profiles;
use index::{Axioms, Ex, Index, Interner, Node, TOP};
use rustc_hash::{FxHashMap, FxHashSet};
use saturate::{Bounds, Contexts, Stop};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The rule of the EL profile that a chain breaks when its superproperty has a range that
/// its last property lacks.
const CHAIN_RANGE_RULE: &str = "OWL 2 Profiles §2.2.6";

/// Charges a positive increment in full before raising the phase's paid peak.
fn cover_memory(
    used: usize,
    max: Option<usize>,
    charge: Option<&(dyn Fn(usize) -> bool + Sync)>,
    paid: &mut usize,
) -> bool {
    if max.is_some_and(|max| used > max) {
        return false;
    }
    if let Some(charge) = charge {
        let more = used.saturating_sub(*paid);
        if more > 0 {
            if !charge(more) {
                return false;
            }
            *paid = used;
        }
    }
    true
}

fn lock_admission<T>(lock: &parking_lot::Mutex<T>) -> parking_lot::MutexGuard<'_, T> {
    #[cfg(test)]
    {
        if let Some(guard) = lock.try_lock() {
            return guard;
        }
        admission_test::hit(admission_test::Point::Contended);
    }
    lock.lock()
}

/// A pool's charge that takes `credit` bytes, which the pool has already admitted, before
/// it asks the pool for more. A charge the pool refuses gives back the credit it took, as
/// the refused growth is not the run's.
fn drawing_on(
    credit: usize,
    charge: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync>,
) -> std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> {
    let credit = AtomicUsize::new(credit);
    std::sync::Arc::new(move |bytes: usize| {
        let had = credit
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                Some(c - c.min(bytes))
            })
            .unwrap_or(0);
        let took = had.min(bytes);
        let rest = bytes - took;
        if rest == 0 || charge(rest) {
            return true;
        }
        credit.fetch_add(took, Ordering::Relaxed);
        false
    })
}

/// The most conclusions a rooted run of Tier D without counting derives before it passes
/// its bound and the run starts again without the rooted rules, which leaves the axioms
/// outside OWL 2 EL out and reports them, as a run without [`Limits::rooted`] does.
/// Of the 50 `shoi` fixtures, 45 finish within 190,000 conclusions on one and on four
/// threads, and the other five derive millions, as F24's measured effect records.
const ROOTED_WORK_BOUND: usize = 1 << 19;

/// Optional per-class rooted work allowance above [`ROOTED_WORK_BOUND`] for a rooted run
/// without counting, gated independently behind `SPARKLES_ROOTED_BOUND_PER_CLASS=1` (off
/// by default until qualified on the 2×2 matrix).
const ROOTED_WORK_PER_CLASS: usize = 32;

/// Whether the distinct-class scaled rooted work bound is enabled
/// (`SPARKLES_ROOTED_BOUND_PER_CLASS=1`), off by default.
fn rooted_bound_per_class() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_BOUND_PER_CLASS_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("SPARKLES_ROOTED_BOUND_PER_CLASS").is_some_and(|v| v == "1")
    })
}

/// The default non-counting rooted work bound for `distinct_named_classes` visible named
/// classes in the input signature.
fn rooted_work_bound(distinct_named_classes: usize, per_class: bool) -> usize {
    if per_class {
        ROOTED_WORK_BOUND.max(ROOTED_WORK_PER_CLASS.saturating_mul(distinct_named_classes))
    } else {
        ROOTED_WORK_BOUND
    }
}

#[cfg(test)]
thread_local! {
    static ROOTED_BOUND_PER_CLASS_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static LAST_ROOTED_WORK_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ROOTED_MERGE_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_MERGE_HUB_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_MERGE_ROOT_FIRST_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn with_rooted_bound_per_class<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_BOUND_PER_CLASS_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_BOUND_PER_CLASS_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

/// Policy selected for a rooted, non-counting Tier D attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootedBoundPolicy {
    /// Explicit caller `Limits::dl_work = Some(n)` (wins over all switches).
    ExplicitFixed(usize),
    /// Qualified deterministic Option 2 (`rawQ`) adaptive policy (fixed ceiling `2^25`).
    Adaptive(dl_rules::AdaptivePolicy),
    /// Fallback fixed rooted bound (`ROOTED_WORK_BOUND`, or per-class selector if present).
    DefaultFixed(usize),
}

impl RootedBoundPolicy {
    fn name(self) -> &'static str {
        match self {
            Self::ExplicitFixed(_) => "explicit_fixed",
            Self::Adaptive(_) => "adaptive",
            Self::DefaultFixed(_) => "default_fixed",
        }
    }

    fn work_limit(self) -> usize {
        match self {
            Self::ExplicitFixed(n) | Self::DefaultFixed(n) => n,
            Self::Adaptive(p) => p.ceiling,
        }
    }
}

/// Precedence selector for rooted non-counting Tier D attempts:
/// 1. Explicit `Some(n)` always wins with `n.max(1)`.
/// 2. Qualified deterministic adaptive (`dl_steps && adaptive_enabled`) uses `policy`
///    with its fixed ceiling (`2^25`), independent of any per-class fallback bound.
/// 3. Otherwise falls through to `fallback_fixed.max(1)`.
pub(crate) fn select_rooted_bound(
    explicit_work: Option<usize>,
    dl_steps: bool,
    adaptive_enabled: bool,
    policy: dl_rules::AdaptivePolicy,
    fallback_fixed: usize,
) -> RootedBoundPolicy {
    if let Some(n) = explicit_work {
        RootedBoundPolicy::ExplicitFixed(n.max(1))
    } else if dl_steps && adaptive_enabled {
        debug_assert!(policy.ceiling > 0);
        RootedBoundPolicy::Adaptive(policy)
    } else {
        RootedBoundPolicy::DefaultFixed(fallback_fixed.max(1))
    }
}

/// Winning outcome of a joined `saturate_counted` call, captured immediately on return
/// before error translation, `partial=true` routing, marker decline, or store teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WinningStop {
    SaturationFinished,
    Work,
    Memory,
    Timeout,
    Cancelled,
    Merge,
}

impl WinningStop {
    fn from_stop(stop: Option<Stop>) -> Self {
        match stop {
            None => Self::SaturationFinished,
            Some(Stop::Work { .. }) => Self::Work,
            Some(Stop::Memory { .. }) => Self::Memory,
            Some(Stop::Timeout) => Self::Timeout,
            Some(Stop::Cancelled) => Self::Cancelled,
            Some(Stop::Merge { .. }) => Self::Merge,
        }
    }

    fn event_name(self) -> &'static str {
        match self {
            Self::SaturationFinished => "saturation_finished",
            _ => "saturation_stopped",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SaturationFinished => "none",
            Self::Work => "work",
            Self::Memory => "memory",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Merge => "merge",
        }
    }
}

/// Snapshot captured immediately after `saturate_counted` for a tracked rooted attempt.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdaptiveSnapshot {
    pub attempt: u32,
    pub after_counting: bool,
    pub enforce: bool,
    pub policy_name: &'static str,
    pub effective_cap: usize,
    pub winning_stop: WinningStop,
    pub stop_guard: dl_rules::WorkStopGuard,
    pub stop_requested_at: usize,
    pub hypothetical_guard: dl_rules::WorkStopGuard,
    pub hypothetical_at: usize,
    pub final_work: usize,
    pub root_work: usize,
    pub raw_queue: usize,
    pub raw_pushed: usize,
    pub mrepair_work: usize,
    pub samples: usize,
    pub oracle_queue: usize,
}

#[cfg(test)]
impl AdaptiveSnapshot {
    pub(crate) fn overshoot(&self) -> usize {
        if self.stop_guard != dl_rules::WorkStopGuard::None {
            self.final_work.saturating_sub(self.stop_requested_at)
        } else {
            0
        }
    }
}

/// Final public classification/saturation outcome recorded when adaptive diagnostics run.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdaptiveOutcome {
    Ok {
        run_id: u64,
        attempts: u32,
        consistency: Consistency,
        complete: bool,
        complete_for_taken: bool,
        dl_rooted_over: bool,
        dl_rooted_declined: bool,
        dl_counting_over: bool,
        dl_over_budget: bool,
        horn_over_budget: bool,
        outside_horn: usize,
        outside_disj: usize,
        excluded: usize,
        incomplete: usize,
    },
    Err {
        run_id: u64,
        attempts: u32,
        kind: &'static str,
    },
}

fn consistency_str(c: Consistency) -> &'static str {
    match c {
        Consistency::Consistent => "consistent",
        Consistency::Inconsistent => "inconsistent",
        Consistency::Unknown => "unknown",
    }
}

fn error_kind_str(e: &Error) -> &'static str {
    match e {
        Error::Memory { .. } => "memory",
        Error::Timeout => "timeout",
        Error::Cancelled => "cancelled",
        Error::Incomplete(_) => "incomplete",
    }
}

fn next_adaptive_run_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Sums resident clause-heap lengths across all already-initialized base and dynamic
/// contexts without materializing any context or allocating a snapshot.
fn count_actual_queue_len(ctx: &Contexts, dynamic: Option<&saturate::Dynamic>) -> usize {
    let mut total = 0usize;
    for slot in ctx {
        if let Some(c) = slot.get()
            && let Some(dl) = c.st.lock().extra.as_ref().and_then(|x| x.dl.as_ref())
        {
            total += dl.store.queue_len();
        }
    }
    if let Some(dy) = dynamic {
        dy.each(|c| {
            if let Some(dl) = c.st.lock().extra.as_ref().and_then(|x| x.dl.as_ref()) {
                total += dl.store.queue_len();
            }
        });
    }
    total
}

/// Whether item 1061 phase 2 rooted constant merging is enabled by default
/// (`SPARKLES_ROOTED_MERGE=1`), off by default.
fn rooted_merge_switch() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_MERGE_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_MERGE").is_some_and(|v| v == "1"))
}

/// Whether a generated item 805 hub participates in constant merging when
/// `rooted_merge` is on (`SPARKLES_ROOTED_MERGE_HUB=0` disables it for ablation).
fn merge_hub() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_MERGE_HUB_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_MERGE_HUB").is_none_or(|v| v != "0"))
}

/// Whether eligible constant-merging runs rotate the root context to `roots[0]`
/// (`SPARKLES_ROOTED_MERGE_ROOT_FIRST=0` disables it for ablation).
fn merge_root_first() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_MERGE_ROOT_FIRST_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("SPARKLES_ROOTED_MERGE_ROOT_FIRST").is_none_or(|v| v != "0")
    })
}

/// Whether to print merge restart diagnostics to stderr (`SPARKLES_ROOTED_MERGE_TRACE=1`).
fn merge_trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_MERGE_TRACE").is_some_and(|v| v == "1"))
}

#[cfg(test)]
#[allow(dead_code)]
fn with_rooted_merge<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_MERGE_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_MERGE_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
#[allow(dead_code)]
fn with_merge_hub<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_MERGE_HUB_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_MERGE_HUB_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
#[allow(dead_code)]
fn with_merge_root_first<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_MERGE_ROOT_FIRST_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_MERGE_ROOT_FIRST_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

/// The most conclusions a run of Tier D with counting derives, kept or redundant, before
/// it passes its bound and the run starts again without counting.
/// Number restrictions can make a context derive millions of conclusions that subsumption
/// discards, which the memory budget does not see. A run that counts over a real ontology
/// either finishes in a few thousand conclusions or runs on past millions, so the bound
/// is the time a run spends before it gives up counting, about 0.2 s on one thread for a
/// small ontology. Of the 150 counting fixtures, with the order between number
/// restrictions, 146 derive at most 103,000 conclusions on one thread. Three derive
/// 393,000, 877,000 and 1.3 million, which [`Limits::dl_work`] lets a caller allow, and
/// one passes 8 million.
const DL_WORK_BOUND: usize = 1 << 18;

/// The bound on the conclusions of the run that starts again without counting once the run
/// that counts has passed its bound. The ontology has shown that its case analysis is
/// heavy, and without a bound such a run can go on for minutes where the Horn rules answer
/// in seconds. Four million conclusions take a few seconds.
const DL_RETRY_WORK: usize = 1 << 22;

/// The least bound on the conclusions that a run of Tier D without counting or the rooted
/// rules derives before it ends and the Horn rules answer (roadmap item 990). The largest
/// such run among the 2,166 generated ontologies of item 969 with a union derives 536,000
/// conclusions, and the dl603 and dl897 fixtures derive at most 5,700.
const DL_ATTEMPT_FLOOR: usize = 1 << 21;

/// The bound on the conclusions of a run of Tier D without counting or the rooted rules,
/// per class of the signature, which the EL saturation gives a context each, above
/// [`DL_ATTEMPT_FLOOR`]. Of the ORE 2015 DL development set's 12 attempts that finish,
/// those above the floor derive at most 212 conclusions per class, ore_ont_16680 with 6.9
/// million. The three that pass their memory budget, ore_ont_9663, ore_ont_14817 and
/// ore_ont_3215, derive 38 to 791 million first, 660 to 14,400 per class, in 10 to 88 s
/// on eight threads, and pass this bound in a third to a half of the time, ore_ont_3215
/// in a fortieth.
const DL_ATTEMPT_PER_CLASS: usize = 384;

/// The most choices of one product of a run of Tier D on one thread without counting or
/// the rooted rules before it ends and the Horn rules answer (roadmap item 1011). On one
/// thread the ORE 2015 DL development set's 12 attempts that finish make products of at
/// most 256 choices, and the 2,166 generated ontologies of item 969 with a union of at
/// most 3,645, while the attempts of ore_ont_14817 and ore_ont_9663 make one of 120
/// million choices after 0.5 million conclusions and one of 4.2 million after 4 million.
/// On four and eight threads the finishing attempts make products of up to 22,500
/// choices, and in three runs on four threads ore_ont_9663 made products of at most 22,707
/// to 923,521 choices and ore_ont_14817 of 1.3 to 5.1 million, so the width tells them
/// apart only on one thread, and a run on more threads has no such limit.
const DL_ATTEMPT_WIDTH: usize = 1 << 20;

/// The least memory budget, in estimated bytes, of a saturation with the Horn rules. The
/// largest Horn run of the ORE 2015 DL sample's development set peaks at 666 MB, 139 times
/// its index and axioms, on a small ontology.
const HORN_MEMORY_FLOOR: usize = 1 << 30;

/// The memory budget of a saturation with the Horn rules as a multiple of the bytes of its
/// index and axioms, above the floor. Larger ontologies of the development set peak at up
/// to 35 times them with the Horn rules and GO at 4, while two Uberon-based ontologies
/// grow past 10 GiB from 24 MB, their contexts of conjunctions composing the chains of
/// transitive parthood again (roadmap 693).
const HORN_MEMORY_FACTOR: usize = 64;

/// What bounds a run: the worker threads, a deadline, a memory budget and a cancellation
/// check, and the rules it may use. The default has every option off except
/// [`number_order`](Limits::number_order).
#[derive(Clone)]
pub struct Limits {
    /// classify by the OWL 2 EL rules alone, as ELK does: the Horn rules of item 603
    /// never run, and every axiom outside OWL 2 EL is left out and reported, so the
    /// taxonomy is ELK's. The default takes the Horn axioms outside OWL 2 EL.
    pub el_only: bool,
    /// worker threads; 0 takes the machine's available parallelism
    pub threads: usize,
    pub deadline: Option<Instant>,
    /// the estimated bytes the classifier's index and contexts may hold
    pub max_memory: Option<usize>,
    /// polled during the run: true stops it as cancelled
    pub cancelled: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    /// told each growth of the estimated memory, in bytes: false stops the run as over
    /// its memory budget, as a server's memory pool refuses a grant
    pub charge: Option<std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync>>,
    /// a run with Tier D (roadmap item 603) that its deadline, cancellation or memory
    /// budget stops answers with what it proved, in a [`Classification`] whose
    /// [`complete_for_taken`](Classification::complete_for_taken) is false, instead of
    /// failing. Runs of the other tiers fail as before.
    pub partial: bool,
    /// refuse a run that leaves an axiom out or uses one only in part with
    /// [`Error::Incomplete`], after the run, as a complete reasoner such as HermiT
    /// refuses what it does not support. The classifier keeps the run's state.
    pub strict: bool,
    /// decide, for a run that leaves axioms out, which classes keep their complete
    /// answer, from their bottom-locality modules
    /// ([`Classification::incomplete`]); false leaves every class incomplete and the
    /// consistency unknown instead, for a caller that reads only the taxonomy, such as
    /// ROBOT's commands. On by default.
    pub coverage: bool,
    /// the most axioms the modules' pass may look at, past which the classes it has not
    /// decided are incomplete ([`Stats::coverage_over`]); by default
    /// [`COVERAGE_WORK_PER_AXIOM`] for each axiom of the input
    pub coverage_work: Option<usize>,
    /// leave the modules' pass to the caller, which calls [`Classification::cover`]
    /// after the run with the axioms it did not give the run as well
    pub cover_later: bool,
    /// the expansion strategy of Tier D's Succ rule
    pub expansion: Expansion,
    /// the most conclusions, kept or redundant, that a run of Tier D with counting derives
    /// before the run starts again without counting, and that a rooted run without
    /// counting derives before the run starts again without the rooted rules. `None`
    /// takes a bound of [`DL_WORK_BOUND`] conclusions for a run that counts and
    /// [`ROOTED_WORK_BOUND`] for a rooted run. It applies only with
    /// [`counting`](Limits::counting) or [`rooted`](Limits::rooted). A run of Tier D
    /// with neither has the bound of [`dl_attempt_work`](Limits::dl_attempt_work),
    /// except the run that starts again after a run that counted passed it, whose bound
    /// is 4 million.
    pub dl_work: Option<usize>,
    /// the most conclusions, kept or redundant, that a run of Tier D without counting and
    /// without the rooted rules derives before it ends and the Horn rules answer, which
    /// leave the disjunctive axioms out and report them, as they do when the run passes
    /// its memory budget, with [`Stats::dl_attempt_over`] set (roadmap item 990). `None`
    /// takes 384 conclusions for each class of the signature, and at least 2,097,152.
    /// `Some(0)` takes no bound. A run that keeps the contexts of the last run (roadmap
    /// item 753) counts the conclusions of the runs since the last one that started
    /// again with its own, and a run that passes the bound starts again in full, whose
    /// answer is the one the caller gets. A run that starts again over the same axioms
    /// can derive more than that sum, so a kept run can answer in full where such a run
    /// falls back (F24, "Incremental native DL classification"). The run keeps the
    /// driver without steps ([`dl_steps`](Limits::dl_steps)), so on several threads an
    /// ontology whose run derives about the bound can answer in full on one schedule and
    /// fall back on another.
    pub dl_attempt_work: Option<usize>,
    /// the most choices of one product, the premises it combines, that a run of Tier D
    /// on one thread without counting and without the rooted rules makes before it ends
    /// as past [`dl_attempt_work`](Limits::dl_attempt_work) (roadmap item 1011). `None`
    /// takes 1,048,576, far above what the attempts that finish make on one thread, and
    /// `Some(0)` takes no limit. A run on more threads has no such limit, since there the
    /// width of a product depends on the schedule, so a run on one thread can fall back to
    /// the Horn rules where a run on several answers in full. A kept run that passes it
    /// starts again in full, as past the bound, and a kept run whose products stay within
    /// it can answer in full where a run that starts again passes it.
    pub dl_attempt_width: Option<usize>,
    /// whether Tier D counts, which is off by default. A run that counts takes number
    /// restrictions, functional and inverse functional properties, disjoint properties and
    /// asymmetric properties into the case analysis, and it is complete for them when it
    /// finishes within [`dl_work`](Limits::dl_work). On real ontologies it rarely does, so
    /// a run that does not count leaves those axioms out and reports them, as the
    /// classifier did before it counted, and costs what that run costs.
    pub counting: bool,
    /// whether Tier D takes an ontology with individuals or nominals through the root of a
    /// rooted run (F24, "Nominals in the disjunctive rules"), which is off by default. A
    /// rooted run takes nominals, class and property assertions, equalities and
    /// inequalities of individuals and negative assertions into the case analysis, and its
    /// run without counting has a bound of [`dl_work`](Limits::dl_work) conclusions, past
    /// which the run starts again without the rooted rules and reports the axioms outside
    /// OWL 2 EL as it does by default, with [`Stats::dl_rooted_over`] set. Every answer
    /// of a rooted run is entailed, and its completeness is not proved: F24 records the
    /// calculus's departures from the published rules and the fixtures it finishes.
    /// A rooted run takes keys over object properties and the universal object property
    /// by rewriting them into class axioms over fresh names, and a run of the rewritten
    /// axioms always starts from nothing. It refuses an ontology with keys over data
    /// properties, the top data property or a functional data property, and one whose keys
    /// or universal property the rewriting declines, which take the default paths. The
    /// option changes nothing for an ontology without individuals, nominals or the
    /// universal object property.
    pub rooted: bool,
    /// whether a rooted run withdraws an answer that can miss an entailment of a
    /// conditional ground fact (roadmap item 1231), which is on by default and changes
    /// nothing without [`rooted`](Limits::rooted). After its saturation the run looks for
    /// a context that holds a ground literal as a condition and a context registered with
    /// the root for one of the literal's individuals that the condition does not reach.
    /// When it finds one, the run starts again without the rooted rules, as past its bound,
    /// and reports [`Stats::dl_rooted_declined`]. The check is a partial containment: it
    /// exempts contexts that it assumes the condition reaches some other way, which is
    /// unproved. A run with the option off keeps the rooted answer, for measurements.
    pub rooted_decline: bool,
    /// whether a rooted run repairs the delivery of conditional ground facts (roadmap item
    /// 1231), a prototype that is off by default and changes nothing
    /// without [`rooted`](Limits::rooted). A context that holds a ground literal as a
    /// condition tells the root, which sends the literal's marker to the contexts
    /// registered for its individuals, and the root returns its clauses whose bodies are
    /// such literals to the contexts that hold them. By default this runs M-return-cf
    /// (`MREPAIR_ON | MREPAIR_CF`, item 1250), resolving returned clauses only against
    /// holder premises whose body has no ground or `f_e(u)` literal (at both initial return
    /// arrival and late-premise replay). `SPARKLES_ROOTED_MARKER_REPAIR` overrides it for
    /// measurements, with `0` (off), `1` (unfiltered `MREPAIR_ON`), or a sum of the bits of
    /// [`dl_rules::DlRun::mrepair`].
    pub rooted_marker_repair: bool,
    /// whether item 1061 phase 2 rooted constant merging on asserted `SameIndividual`
    /// axioms and unconditionally root-entailed unit constant equalities is enabled
    /// (default off; `SPARKLES_ROOTED_MERGE=1` turns it on).
    #[doc(hidden)]
    pub rooted_merge: bool,
    /// whether a run that counts without the root orders its number restrictions by
    /// entailment (F24, "The order between number restrictions"), which is on by default
    /// and changes nothing without [`counting`](Limits::counting). `≥m r.F` entails
    /// `≥n s.G` when `m ≥ n`, `r ⊑ s` and `F ⊑ G` are told, and contradicts `≤k s.G` when
    /// `m > k`. The run tells itself these consequences of the ontology, and a context
    /// that holds a minimum does not make the witnesses of a minimum that it entails. The
    /// answers are those of a run without the order that finishes, and a run with it
    /// finishes on ontologies with many related cardinalities, such as the Protein
    /// Ontology, where the run without it passes [`dl_work`](Limits::dl_work). A run
    /// that passes the bound either way can spend longer before it does: the
    /// classification of ore_ont_6485 of the ORE 2015 DL sample with counting, the
    /// attempt and the run without counting together, takes 213 ms with the order and
    /// 188 ms without it on one thread. False turns the order off.
    pub number_order: bool,
    /// Makes the saturations of a run of Tier D that has a bound on its derived
    /// conclusions proceed in steps, so that the run derives the same conclusions, passes
    /// or keeps its bound on them, and so answers in full or falls back, in the same way
    /// on every number of threads and every schedule (roadmap item 727). On by default.
    /// The runs with such a bound are a run that counts, the run that starts again
    /// without counting after it, and a rooted run. The bound of any other run of Tier D,
    /// [`dl_attempt_work`](Limits::dl_attempt_work), keeps the driver without steps. A
    /// step activates the waiting contexts in parallel and delivers their messages at its
    /// end in a fixed order.
    /// The run does not repeat on one thread an attempt that passed its bound. A rooted
    /// run and a run with [`Expansion::Eager`] that has the bound run their saturations
    /// on one thread with the option. A run without the bound has no outcome that a
    /// schedule decides, so the option changes nothing for it. The memory budget, the
    /// deadline and a cancellation still stop a run when they are reached. False takes
    /// the driver whose threads deliver their messages during the saturation, with no
    /// step between them. With it a run near its bound can answer in full on one
    /// schedule and fall back on another, and an attempt that passed the bound on
    /// several threads is repeated on one.
    pub dl_steps: bool,
    /// How far Tier D reduces the automata by which it unfolds the universal restrictions
    /// on properties that chains or transitivity reach (roadmap item 712), for the
    /// comparison of the forms: 0 not at all, 1 merging the states that accept the same
    /// words by the same transitions, and 2 also dropping a transition that another from
    /// the same state into the same state covers by a superproperty. `None` takes
    /// `SPARKLES_DL_AUTOMATA` when it is set and 2 otherwise, and a level above 2 is 2.
    /// A run that finishes gives the same taxonomy at every level, and 2 derives the fewest
    /// conclusions, so an attempt can pass its bound at a lower level where it finishes at 2.
    pub dl_automata: Option<u8>,
}

impl Limits {
    /// The level of [`dl_automata`](Limits::dl_automata) that a run takes.
    fn dl_automata_level(&self) -> u8 {
        self.dl_automata
            .map_or_else(horn::dl_reduction, |l| l.min(2))
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            el_only: false,
            threads: 0,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            partial: false,
            strict: false,
            coverage: true,
            coverage_work: None,
            cover_later: false,
            expansion: Expansion::default(),
            dl_work: None,
            dl_attempt_work: None,
            dl_attempt_width: None,
            counting: false,
            rooted: false,
            rooted_decline: true,
            rooted_marker_repair: false,
            rooted_merge: rooted_merge_switch(),
            number_order: true,
            dl_steps: true,
            dl_automata: None,
        }
    }
}

/// Which context the Succ rule of Tier D (roadmap item 603) gives a successor. Both
/// strategies give the same answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Expansion {
    /// the context of the existential restriction's filler with the role's range when
    /// the restriction holds, and else a context with the empty core for each
    /// restriction, as Sequoia's cautious strategy
    #[default]
    Cautious,
    /// the context of what holds of the successor for certain, as the eager strategy
    Eager,
}

/// Why a run stopped. The classifier keeps its axioms, and its next run starts the
/// saturation again from the beginning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Timeout,
    /// the estimated memory passed the budget
    Memory {
        limit: usize,
        used: usize,
    },
    /// a strict run ([`Limits::strict`]) left out or used only in part these axioms, so
    /// its answer would not be complete for the whole ontology
    Incomplete(Vec<Excluded>),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => f.write_str("classification cancelled"),
            Error::Timeout => f.write_str("classification timed out"),
            Error::Memory { limit, used } => write!(
                f,
                "classification needs more than its memory budget of {limit} bytes (at least {used})"
            ),
            Error::Incomplete(excluded) => f.write_str(&refusal(excluded)),
        }
    }
}

impl std::error::Error for Error {}

/// What a run did and how long its steps took.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// whether the run kept contexts of the previous one
    pub incremental: bool,
    /// the axioms added and removed since the previous run
    pub added: usize,
    pub removed: usize,
    /// the contexts saturated before the run that it cleared and saturated again
    pub cleared: usize,
    /// the contexts saturated after the run
    pub contexts: usize,
    /// the conclusions the rules applied, duplicates included
    pub conclusions: usize,
    /// the estimated bytes the run keeps: the index, the contexts, the taxonomy and a
    /// classifier's copy of the axioms
    pub memory: usize,
    /// the part of [`memory`](Stats::memory) that the taxonomy takes, 0 for a run
    /// without one
    pub taxonomy_memory: usize,
    /// the most estimated bytes the run held at once, which a memory budget bounds:
    /// [`memory`](Stats::memory) and, while the saturation ran, the messages waiting in
    /// the contexts' to-do lists
    pub peak_memory: usize,
    pub threads: usize,
    pub index: Duration,
    pub saturation: Duration,
    /// the part of [`saturation`](Stats::saturation) from the end of the first
    /// saturation and of Tier D's tests to the end of the stage: the marking of real
    /// contexts with its rounds, the facts of universal roles, the merges of keys, the
    /// witnesses and the worlds, and the counts of the contexts and the diagnostics of a
    /// rooted run that close the stage
    pub marking: Duration,
    pub taxonomy: Duration,
    /// the time the locality modules of a run that left axioms out took to decide which
    /// classes keep their complete answer, 0 for a run that left nothing out
    pub coverage: Duration,
    /// the modules' pass passed its bound on work ([`Limits::coverage_work`]), so the
    /// classes it had not decided are incomplete
    pub coverage_over: bool,
    /// the deadline or a cancellation stopped the modules' pass, so every class is
    /// incomplete and the consistency unknown
    pub coverage_stopped: bool,
    /// the triples of the input that map to no axiom, such as a domain of
    /// a property that is not declared, which the answer counts as left out
    /// ([`Classification::leave_out_unmapped`]): at least one when the run did not read
    /// the assertions, whose read stops at the first
    pub unmapped: usize,
    /// the axioms outside OWL 2 EL, by what classifying them needs, with those the Horn
    /// rules of roadmap item 603 take; the others are left out and reported. The counts
    /// go by each axiom's shape and by what the tiers take, not by what the run answers in
    /// full. An axiom answered in part, such as one the datatype checker covers, is
    /// reported as [`Gap::Partial`] among the run's excluded axioms.
    pub outside_el: OutsideEl,
    /// the rules the run's axioms need
    pub tier: Tier,
    /// the contexts of conjunctions that the Horn rules created, which
    /// [`contexts`](Stats::contexts) includes
    pub conjunctions: usize,
    /// the Horn rules passed their memory budget, so the run classified by the OWL 2 EL
    /// rules alone, as [`Limits::el_only`] does, and left the axioms outside OWL 2 EL out
    pub horn_over_budget: bool,
    /// the ranges of properties that chains reach whose universal restriction the Horn
    /// rules leave out, since the ranges of the links that end every path of the
    /// property already hold there (roadmap item 693)
    pub horn_ranges_left_out: usize,
    /// the clauses with two or more disjuncts that Tier D took, 0 for a run without it
    pub clauses: usize,
    /// the contexts that held clauses of Tier D
    pub dl_contexts: usize,
    /// the clauses that the contexts of Tier D stored, those deleted as subsumed included
    pub dl_stored: usize,
    /// the conclusions Tier D derived, kept or not, which its work bound counts
    pub dl_work: usize,
    /// prefix conclusions spent across merge restarts of the answering rooted attempt
    #[doc(hidden)]
    pub dl_rooted_spent: usize,
    /// number of constant-merge restarts in the answering rooted attempt
    #[doc(hidden)]
    pub dl_rooted_restarts: usize,
    /// number of non-representative individuals mapped away in the answering run
    #[doc(hidden)]
    pub dl_rooted_merged: usize,
    /// Tier D with counting passed its bound on derived conclusions, so the run
    /// classified again without counting and left the axioms that need it out
    pub dl_counting_over: bool,
    /// a rooted run of Tier D ([`Limits::rooted`]) passed its bound on derived
    /// conclusions, so the run classified again without the rooted rules and left the
    /// axioms outside OWL 2 EL out, as a run without the option does
    pub dl_rooted_over: bool,
    /// a rooted run of Tier D finished with a ground literal held as a condition that does
    /// not reach a context registered for one of its individuals
    /// ([`Limits::rooted_decline`]), so its answer could miss entailments and the run
    /// classified again without the rooted rules, as past its bound, and left the axioms
    /// outside OWL 2 EL out
    pub dl_rooted_declined: bool,
    /// with row 1231's diagnostic on, the marker repair's deficits at quiescence: the owed
    /// marker deliveries, the owed returns, and the returns with too many products to check
    pub dl_marker_deficits: [usize; 3],
    /// a run of Tier D without counting and without the rooted rules passed its bound on
    /// derived conclusions ([`Limits::dl_attempt_work`]), so the run classified with the
    /// Horn rules and left the disjunctive axioms out
    pub dl_attempt_over: bool,
    /// the candidate subsumers that Tier D tested in contexts of their own
    pub dl_tests: usize,
    /// the tested candidates that hold
    pub dl_tests_held: usize,
    /// the part of [`saturation`](Stats::saturation) that the tests of Tier D took
    pub dl_testing: Duration,
    /// Tier D passed its memory budget, or its bound on derived conclusions
    /// ([`dl_attempt_over`](Stats::dl_attempt_over)), so the run classified with the Horn
    /// rules and left the disjunctive axioms out, or Tier D with counting passed its bound
    /// on derived conclusions and the run classified without counting
    /// ([`dl_counting_over`](Stats::dl_counting_over)), or a rooted run passed its bound
    /// and the run classified without the rooted rules
    /// ([`dl_rooted_over`](Stats::dl_rooted_over)), or a rooted run withdrew its answer
    /// and the run classified without the rooted rules
    /// ([`dl_rooted_declined`](Stats::dl_rooted_declined))
    pub dl_over_budget: bool,
}

/// The result of a run that saturates without building a taxonomy
/// ([`Saturation::saturate`]).
#[derive(Clone, Debug)]
pub struct Saturated {
    /// false when the ontology has no model
    pub consistent: bool,
    pub stats: Stats,
}

/// The result of a run.
#[derive(Clone, Debug)]
pub struct Classification {
    /// false when the ontology has no model; every class is then unsatisfiable
    pub consistent: bool,
    pub taxonomy: Taxonomy,
    pub stats: Stats,
    /// whether every subsumption between named classes and every unsatisfiable class
    /// that the whole ontology entails is in the taxonomy: false when the run left an
    /// axiom out or used one only in part ([`Saturation::excluded`]) and for the partial
    /// answer of a run that stopped ([`Limits::partial`]). Every subsumption the taxonomy
    /// holds is entailed either way, and the classes not in
    /// [`incomplete`](Classification::incomplete) have their complete answer.
    pub complete: bool,
    /// whether the answer is complete for the axioms the run took: false only for the
    /// partial answer of a run that stopped. This is what
    /// [`complete`](Classification::complete) meant before it covered the axioms left
    /// out, and it is the meaning of a profile reasoner such as ELK, which ignores the
    /// axioms outside its fragment.
    pub complete_for_taken: bool,
    /// the classes whose superclasses, equivalent classes or satisfiability the answer
    /// may miss, in IRI order: those a partial answer leaves open and those whose
    /// bottom-locality module holds an axiom left out (see
    /// [`transform::Coverage`](crate::transform::Coverage)). It can be empty while
    /// [`complete`](Classification::complete) is false, when no class's answer depends on
    /// what was left out.
    pub incomplete: Vec<Class>,
    /// whether the ontology has a model, as far as the run knows: a partial answer can
    /// leave it unknown, and so can an axiom left out that could make the ontology
    /// inconsistent, and then no class is complete
    pub consistency: Consistency,
    /// the first [`UNMAPPED_LISTED`] of the triples that
    /// [`Stats::unmapped`] counts, in the order the input gave them, and whether there are
    /// more ([`Classification::leave_out_unmapped`])
    pub unmapped: Unmapped,
}

/// The most triples that map to no axiom that a classification lists; it counts the rest.
pub const UNMAPPED_LISTED: usize = 100;

/// Triples of the input that map to no axiom, counted, with the first
/// [`UNMAPPED_LISTED`] kept in the order they were added. The OWL API calls such triples
/// unparsed and reports them through its RDF parser's metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unmapped {
    pub count: usize,
    pub triples: Vec<oxrdf::Triple>,
    /// whether `count` is more than `triples` lists
    pub truncated: bool,
}

impl Unmapped {
    /// Counts `t`, and keeps it while fewer than [`UNMAPPED_LISTED`] are listed.
    pub fn push(&mut self, t: &oxrdf::Triple) {
        self.count += 1;
        if self.triples.len() < UNMAPPED_LISTED {
            self.triples.push(t.clone());
        } else {
            self.truncated = true;
        }
    }

    /// Adds `other`'s count and keeps its triples after these, up to the cap.
    pub fn merge(mut self, other: &Unmapped) -> Unmapped {
        let room = UNMAPPED_LISTED - self.triples.len();
        self.count += other.count;
        self.truncated |= other.truncated || other.triples.len() > room;
        self.triples
            .extend(other.triples.iter().take(room).cloned());
        self
    }

    /// Counts and keeps each of `ts`, in order.
    pub fn extend<'a>(mut self, ts: impl IntoIterator<Item = &'a oxrdf::Triple>) -> Unmapped {
        for t in ts {
            self.push(t);
        }
        self
    }
}

/// Whether the ontology has a model, as far as a run knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consistency {
    Consistent,
    Inconsistent,
    /// a partial answer that did not decide it
    Unknown,
}

impl Consistency {
    fn of(consistent: bool) -> Consistency {
        if consistent {
            Consistency::Consistent
        } else {
            Consistency::Inconsistent
        }
    }
}

impl Classification {
    fn new(
        consistent: bool,
        taxonomy: Option<Taxonomy>,
        stats: Stats,
        partial: Option<(Vec<Class>, Consistency)>,
    ) -> Classification {
        let (complete, incomplete, consistency) = match partial {
            Some((incomplete, consistency)) => (false, incomplete, consistency),
            None => (true, Vec::new(), Consistency::of(consistent)),
        };
        Classification {
            consistent,
            taxonomy: taxonomy.expect("a taxonomy was asked for"),
            stats,
            complete,
            complete_for_taken: complete,
            incomplete,
            consistency,
            unmapped: Unmapped::default(),
        }
    }

    /// Takes account of the axioms the run over `axioms` left out or used only in part
    /// (`excluded`) and of those of the ontology it was not given at all (`dropped`, such
    /// as the assertions of a classification of the terminology alone): the answer is not
    /// complete for the whole ontology when one is left out, and the classes whose module
    /// holds one, or every class when the consistency is no longer known, join
    /// [`incomplete`](Classification::incomplete).
    ///
    /// Any dropped axiom with logical content counts as left out, whether or not it could
    /// change an answer: the answer is then not complete, every class is incomplete, a
    /// consistency the run found is unknown, and the answer names that axiom for the
    /// caller to report beside `excluded`. An inconsistency stays, since it holds for the
    /// whole ontology too. A class that only a dropped axiom names is in no taxonomy, so
    /// it is not in `incomplete` and shows only through `complete` and that axiom. A run
    /// takes this pass itself unless [`Limits::cover_later`] leaves it to its caller.
    /// Costs nothing when nothing is left out or dropped, and one read of the dropped
    /// axioms up to the first with logical content when only they are.
    ///
    /// A pass that its deadline or cancellation stops leaves every class incomplete and
    /// the consistency unknown, with [`Stats::coverage_stopped`] set, rather than throw
    /// away the saturation: such an answer depends on timing.
    pub fn cover(
        &mut self,
        axioms: &[&AxiomKind],
        excluded: &[Excluded],
        dropped: Option<&mut crate::transform::Dropped<'_>>,
        limits: &Limits,
    ) -> Option<Excluded> {
        if excluded.is_empty() && dropped.is_none() {
            return None;
        }
        let start = Instant::now();
        if let Some(d) = dropped {
            let Ok(witness) = first_logical(d, limits) else {
                self.stats.coverage = start.elapsed();
                self.stopped_pass();
                return None;
            };
            if let Some(w) = witness {
                self.stats.coverage = start.elapsed();
                self.not_all_read();
                return Some(not_read(w));
            }
            if excluded.is_empty() {
                self.stats.coverage = start.elapsed();
                return None;
            }
        }
        self.complete = false;
        // an inconsistency is sound, and every class's answer is then complete
        if self.consistency == Consistency::Inconsistent {
            return None;
        }
        // A partial answer that left the consistency unknown has every class open, and
        // one that stopped is past its deadline, so neither takes the modules' pass,
        // and neither does a run that asks for none: every class is then incomplete.
        if self.consistency == Consistency::Unknown || !self.complete_for_taken || !limits.coverage
        {
            self.all_incomplete();
            if self.consistency == Consistency::Consistent {
                self.consistency = Consistency::Unknown;
            }
            return None;
        }
        let left: Vec<&AxiomKind> = excluded.iter().map(|e| &e.axiom).collect();
        let budget = limits.coverage_work.unwrap_or_else(|| {
            COVERAGE_WORK_PER_AXIOM
                .saturating_mul(axioms.len())
                .max(COVERAGE_WORK_MIN)
        });
        let stop = || poll(limits).is_err();
        let c = crate::transform::coverage(
            axioms,
            &left,
            self.consistency == Consistency::Consistent,
            by_depth(&self.taxonomy),
            budget,
            &stop,
        );
        self.stats.coverage = start.elapsed();
        let Ok(c) = c else {
            self.stopped_pass();
            return None;
        };
        if !c.consistency_known {
            self.consistency = Consistency::Unknown;
        }
        self.stats.coverage_over = c.over_budget;
        if !c.incomplete.is_empty() {
            let mut all = std::mem::take(&mut self.incomplete);
            all.extend(c.incomplete);
            // The node of the unsatisfiable classes is exact only when every one of them
            // is found, which an incomplete class may not be.
            all.extend(
                self.taxonomy
                    .unsatisfiable()
                    .filter(|k| k.as_str() != crate::vocab::OWL_NOTHING)
                    .cloned(),
            );
            all.sort();
            all.dedup();
            self.incomplete = all;
        }
        None
    }

    /// Counts the triples of the input that map to no axiom (`u`) as left out: with any,
    /// the answer is not complete, every class is incomplete and a consistency the run
    /// found is unknown, since such a triple may carry any logical content. The count goes
    /// to [`Stats::unmapped`] and the list to [`Classification::unmapped`], as no axiom of
    /// [`Excluded`] can name them.
    pub fn leave_out_unmapped(&mut self, u: Unmapped) {
        if u.count > 0 {
            self.unmapped = std::mem::take(&mut self.unmapped).merge(&u);
            self.stats.unmapped = self.unmapped.count;
            self.not_all_read();
        }
    }

    /// The answer when the run did not read part of the ontology that may carry any
    /// logical content: not complete, every class incomplete, and an inconsistency kept,
    /// as it holds for the whole ontology too, but a consistency unknown.
    fn not_all_read(&mut self) {
        self.complete = false;
        self.all_incomplete();
        if self.consistency == Consistency::Consistent {
            self.consistency = Consistency::Unknown;
        }
    }

    /// The answer of a modules' pass that its deadline, its cancellation or its stream of
    /// dropped axioms stopped: every class incomplete and the consistency unknown.
    fn stopped_pass(&mut self) {
        self.stats.coverage_stopped = true;
        self.complete = false;
        self.all_incomplete();
        self.consistency = Consistency::Unknown;
    }

    /// Every class but owl:Thing and owl:Nothing incomplete, in IRI order.
    fn all_incomplete(&mut self) {
        let mut all: Vec<Class> = self
            .taxonomy
            .classes()
            .filter(|c| {
                !matches!(
                    c.as_str(),
                    crate::vocab::OWL_THING | crate::vocab::OWL_NOTHING
                )
            })
            .cloned()
            .collect();
        all.sort();
        all.dedup();
        self.incomplete = all;
    }
}

/// The first dropped axiom with logical content, or `Err` when the stream could not
/// finish or the run's deadline or cancellation came first. Declarations and annotations
/// have no logical content, and every other axiom has.
fn first_logical(
    d: &mut crate::transform::Dropped<'_>,
    limits: &Limits,
) -> Result<Option<AxiomKind>, ()> {
    let mut first = None;
    let mut seen = 0usize;
    let mut stopped = false;
    let finished = d(&mut |k| {
        seen += 1;
        if seen.is_multiple_of(4096) && poll(limits).is_err() {
            stopped = true;
            return false;
        }
        if logical(k) && !matches!(k, AxiomKind::Declaration(_)) {
            first = Some(k.clone());
            false
        } else {
            true
        }
    });
    if stopped || !finished {
        Err(())
    } else {
        Ok(first)
    }
}

/// The rule of an axiom that a classification of the terminology alone did not read
/// ([`Classification::cover`]).
pub const NOT_READ: &str = "assertion-not-read";

/// A dropped axiom that counts as left out, as the run reports it.
fn not_read(axiom: AxiomKind) -> Excluded {
    Excluded {
        axiom,
        gap: Gap::Outside {
            rule: NOT_READ.to_string(),
            message: "the classification read the terminology alone, and an assertion it \
                did not read can change any class's answer or name a class the taxonomy \
                lacks: classify with the assertions"
                .to_string(),
        },
    }
}

/// The axioms the modules' pass of a run that left axioms out may look at for each axiom
/// of the input by default ([`Limits::coverage_work`]), and the fewest the default allows
/// in all.
pub const COVERAGE_WORK_PER_AXIOM: usize = 1024;
pub const COVERAGE_WORK_MIN: usize = 1 << 20;

/// The classes of a taxonomy by the depth of their node, each node after its parents, and
/// by IRI within a depth, so that the order depends on the taxonomy alone.
fn by_depth(t: &Taxonomy) -> Vec<&Class> {
    let nodes = t.nodes();
    let mut waiting: Vec<usize> = nodes.iter().map(|n| n.parents.len()).collect();
    let mut depth = vec![usize::MAX; nodes.len()];
    let mut queue: std::collections::VecDeque<usize> =
        (0..nodes.len()).filter(|&i| waiting[i] == 0).collect();
    for &i in &queue {
        depth[i] = 0;
    }
    while let Some(i) = queue.pop_front() {
        for &c in &nodes[i].children {
            waiting[c] = waiting[c].saturating_sub(1);
            if waiting[c] == 0 {
                depth[c] = depth[i] + 1;
                queue.push_back(c);
            }
        }
    }
    let mut out: Vec<(usize, &Class)> = nodes
        .iter()
        .enumerate()
        .flat_map(|(i, n)| n.classes.iter().map(move |c| (i, c)))
        .map(|(i, c)| (depth[i], c))
        .collect();
    out.sort();
    out.into_iter().map(|(_, c)| c).collect()
}

/// The message of a strict run's refusal ([`Error::Incomplete`]): how many axioms were
/// left out or used only in part, and the first of them with the rule each breaks.
pub fn refusal(excluded: &[Excluded]) -> String {
    const SHOWN: usize = 10;
    if excluded.is_empty() {
        return "the classification would not be complete: the run stopped before it decided every class"
            .to_string();
    }
    let mut m = format!(
        "the classification would not be complete: it leaves out or uses only in part {} axiom{}",
        excluded.len(),
        if excluded.len() == 1 { "" } else { "s" }
    );
    for (i, e) in excluded.iter().take(SHOWN).enumerate() {
        m.push_str(if i == 0 { ": " } else { "; " });
        m.push_str(&e.describe());
    }
    if excluded.len() > SHOWN {
        m.push_str(&format!("; and {} more", excluded.len() - SHOWN));
    }
    m
}

/// An axiom the classifier leaves out or uses only in part, and why.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Excluded {
    pub axiom: AxiomKind,
    pub gap: Gap,
}

impl Excluded {
    /// The axiom in functional-style syntax, with the rule it breaks or what the
    /// classifier misses of it.
    pub fn describe(&self) -> String {
        let ax = crate::ofn::Renderer::default().axiom(&Axiom::new(self.axiom.clone()));
        match &self.gap {
            Gap::Outside { rule, message } => format!("{ax} ({rule}: {message})"),
            Gap::Partial(why) => format!("{ax} (used only in part: {why})"),
        }
    }
}

/// One axiom the classifier holds: how many times it is added now and was at the start
/// of the last run.
struct Held {
    count: u32,
    /// the count when the last run started, 0 for an axiom added since
    ran: u32,
}

/// An incremental OWL 2 EL classifier that keeps its own copy of the axioms. See the
/// [module documentation](self), and [`Saturation`] for a classifier over axioms that
/// the caller keeps.
pub struct Classifier {
    /// the axioms held now or at the last run, an axiom held at neither being dropped
    /// after the next run
    axioms: FxHashMap<AxiomKind, Held>,
    /// the axioms held now
    live: usize,
    /// the estimated bytes of the held axioms
    held_bytes: usize,
    sat: Saturation,
}

impl Default for Classifier {
    fn default() -> Classifier {
        Classifier::new()
    }
}

/// The saturated contexts of a classification, kept between runs, over axioms that the
/// caller keeps, such as those of an open ontology, and gives to each run. A caller that
/// knows which axioms changed since the last run gives them too, and the run saturates
/// again only what they can reach.
pub struct Saturation {
    #[cfg(test)]
    top_chain_probe: Option<std::sync::Arc<top_chain_diagnostic::Probe>>,
    interner: Interner,
    index: Option<Index>,
    ctx: Contexts,
    /// the number of the caller's axioms while a run reads fewer, after
    /// [`interchangeable`] dropped the class assertions that another stands for
    source_len: Option<usize>,
    /// what the last run left out or used only in part, sorted
    excluded: Vec<Excluded>,
    /// set when a run stopped part way, so that the next run starts again
    dirty: bool,
    /// the subsumers of the classes that the last run classified in worlds of their own,
    /// with whether each is unsatisfiable
    worlds: FxHashMap<Ex, (bool, Vec<Ex>)>,
    /// cached upward top closure and equivalence-inferred bottom closure for
    /// [`Saturation::describe`], cleared whenever a new run starts or finishes
    query_cache: std::sync::OnceLock<query::QueryClosure>,
    /// whether the last run's contexts depend on one another through individuals in ways
    /// the invalidation does not follow: a context merged with an individual, a world, a
    /// key or a fact of a universal role
    entangled: bool,
    /// whether the interner may hold expressions that are not [`tier_neutral`] of a
    /// removed axiom or of an added axiom outside OWL 2 EL, or what a normalization
    /// outside OWL 2 EL interned that [`TierMark::passed`] finds, which a run that
    /// decides its tier over the interner would read as present, so a run with axioms
    /// outside OWL 2 EL starts again
    stale_tier: bool,
    /// whether the last run whose saturation ended found the ontology consistent
    consistent: bool,
    /// the contexts of conjunctions that the last run's Horn rules created
    dynamic: Option<saturate::Dynamic>,
    /// the disjointness sets of the last run's index, whose place among them numbers
    /// each set in the contexts that the run leaves
    disjoint: Vec<Vec<Ex>>,
    /// whether the current run's axioms went to the Horn normalization, from its start
    /// until the normalization took none
    horn_tried: bool,
    /// whether the current run's axioms went to the normalization of Tier D, from its
    /// start until the normalization took none
    dl_tried: bool,
    /// set while a run classifies again without Tier D, past Tier D's memory budget
    skip_dl: bool,
    /// set while a run classifies again without counting, past Tier D's bound on derived
    /// conclusions, which leaves the axioms that need counting out
    no_counting: bool,
    /// set while a run classifies again without the rooted rules, past a rooted run's
    /// bound on derived conclusions, which leaves the axioms outside OWL 2 EL out
    no_rooted: bool,
    /// set by a rooted run that withdrew its answer (item 1231), which classifies again
    /// without the rooted rules
    rooted_declined: bool,
    /// set while a run classifies the axioms that item 805's reductions made, so that the
    /// classes they made up stay out of the taxonomy
    hide_internal: bool,
    /// whether the last run classified rewritten axioms, which the next run's changes
    /// do not describe and whose made-up names [`Saturation::describe`] leaves out
    pub(crate) reduced: bool,
    /// whether the current run of Tier D is rooted and whether it counts, for the
    /// fallback its bound on derived conclusions takes
    dl_rooted: bool,
    dl_counted: bool,
    /// whether the current run of Tier D neither counts nor is rooted nor starts again
    /// without counting, so that its bound on derived conclusions ends it for the Horn
    /// rules (roadmap item 990)
    dl_attempt: bool,
    /// set by a run of Tier D that passed its bound on derived conclusions
    dl_worked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// set when a Tier D classification requesting partial answers cannot fit its
    /// completed or stopped result, so the error does not retry without Tier D
    partial_over: bool,
    /// A normalization resource halt must survive Horn/DL fallback unchanged.
    numeric_halt: bool,
    /// what the last run's partial answer leaves open: the classes not complete and the
    /// consistency
    partial: Option<(Vec<Class>, Consistency)>,
    /// what the last run whose saturation ended was, which decides whether the next
    /// run may keep its contexts (roadmap item 753)
    kept: Option<Kept>,
    /// whether the current run keeps contexts of an earlier run, so that a stop on its
    /// memory budget starts again in full before a lower tier takes over
    reused: bool,
    /// the clause table of the last run of Tier D whose contexts the next run may keep,
    /// which name its clauses by id
    dl_run: Option<dl_rules::DlRun>,
    /// the sorted root contexts of the last kept run, used to clear dead roots when
    /// axioms are removed (roadmap item 753)
    last_roots: Vec<Ex>,
    /// diagnostic run id for top-level `run_with_all` (non-zero only when adaptive diag is on)
    adaptive_run_id: u64,
    /// number of tracked rooted attempts executed in the current top-level `run_with_all`
    adaptive_attempts: u32,
    /// the high-water mark that the last completed [`Saturation::run_once`] paid to the
    /// memory pool, used to offset post-run original-axiom accounting on kept rewrites
    last_paid: usize,
    /// item 1061 phase 2 dense union-find and cached canonical map for rooted constant
    /// merging; cleared at the end of every `run_with_all`
    merge: Merge,
    /// canonical map requested for the current `run` / `run_once` attempt
    canon_want: Option<std::sync::Arc<index::CanonMap>>,
    /// generated hub of the currently active item 805 reduction, if any
    active_hub: Option<NamedIndividual>,
    /// winning merge-stop payload returned from `run_once` to `run_merging`
    merge_stop: Option<MergeStop>,
    /// set for the remainder of a call when `collect_same` declines on memory refusal
    merge_disabled: bool,
}

/// Payload transferred from a winning `Stop::Merge` out of `run_once` into `run_merging`.
struct MergeStop {
    pairs: Vec<(u32, u32)>,
    pairs_bytes: usize,
    spent: usize,
    bound: usize,
    peak: usize,
    active_hub: Option<NamedIndividual>,
}

enum BuildCanonErr {
    Poll(Error),
    Memory(usize),
}

/// Normalizes an individual key so asserted (`Individual::Anonymous`) and harvested
/// (`NamedIndividual` starting with `index::ANONYMOUS`) anonymous individuals use one
/// canonical `(is_anon, raw)` representation and hash identically.
#[inline]
fn canonical_merge_parts(is_anon: bool, raw: &str) -> (bool, &str) {
    if is_anon {
        (true, raw)
    } else if let Some(label) = raw.strip_prefix(index::ANONYMOUS) {
        (true, label)
    } else {
        (false, raw)
    }
}

/// Canonical hash of a normalized `(is_anon, raw)` key for `Merge::ids` without
/// allocating `"_:label"`. Callers normalize via [`canonical_merge_parts`] first so
/// asserted and harvested anonymous individuals hash and compare identically.
#[inline]
fn merge_name_hash(is_anon: bool, raw: &str) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    debug_assert!(is_anon || !raw.starts_with(index::ANONYMOUS));
    let mut h = rustc_hash::FxBuildHasher.build_hasher();
    if is_anon {
        h.write(index::ANONYMOUS.as_bytes());
    }
    h.write(raw.as_bytes());
    h.write_u8(0xff);
    h.finish()
}

/// Full key equality on hash collisions without allocating `"_:label"`.
#[inline]
fn merge_name_eq(stored: &NamedIndividual, is_anon: bool, raw: &str) -> bool {
    if is_anon {
        stored
            .as_str()
            .strip_prefix(index::ANONYMOUS)
            .is_some_and(|rest| rest == raw)
    } else {
        stored.as_str() == raw
    }
}

/// Provenance-aware 3-tier representative ordering:
/// Tier 0 = genuine user input named individual (including any legal input IRI starting
/// with `dl_reduce::PREFIX`), Tier 1 = item 1281 anonymous individual, Tier 2 = generated
/// hub of the active item 805 reduction.
#[inline]
fn rep_key<'a>(n: &'a NamedIndividual, active_hub: Option<&NamedIndividual>) -> (u8, &'a str) {
    let tier = if active_hub.is_some_and(|h| h == n) {
        2
    } else if index::is_anonymous(n) {
        1
    } else {
        0
    };
    (tier, n.as_str())
}

#[inline]
fn same_canon(
    a: &Option<std::sync::Arc<index::CanonMap>>,
    b: &Option<std::sync::Arc<index::CanonMap>>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => std::sync::Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// Dense integer union-find for item 1061 phase 2 constant merging, storing each
/// participating individual string once with exact Item 1291 byte tracking and
/// pre-admission before every allocation or container growth.
#[derive(Default)]
struct Merge {
    names: Vec<NamedIndividual>,
    hashes: Vec<u64>,
    ids: hashbrown::HashTable<u32>,
    parent: Vec<u32>,
    size: Vec<u32>,
    best: Vec<u32>,
    name_str_bytes: usize,
    active_hub: Option<NamedIndividual>,
    shared: Option<std::sync::Arc<index::CanonMap>>,
    spent: usize,
    restarts: usize,
    frozen_bound: Option<usize>,
}

impl Merge {
    fn clear(&mut self) {
        self.names = Vec::new();
        self.hashes = Vec::new();
        self.ids = hashbrown::HashTable::new();
        self.parent = Vec::new();
        self.size = Vec::new();
        self.best = Vec::new();
        self.name_str_bytes = 0;
        self.active_hub = None;
        self.shared = None;
        self.spent = 0;
        self.restarts = 0;
        self.frozen_bound = None;
    }

    fn uf_bytes(&self) -> usize {
        self.names
            .capacity()
            .saturating_mul(std::mem::size_of::<NamedIndividual>())
            .saturating_add(self.name_str_bytes)
            .saturating_add(
                self.hashes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u64>()),
            )
            .saturating_add(dl_clause::hash_table_bytes(
                self.ids.capacity(),
                std::mem::size_of::<u32>(),
            ))
            .saturating_add(
                self.parent
                    .capacity()
                    .saturating_add(self.size.capacity())
                    .saturating_add(self.best.capacity())
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(self.active_hub.as_ref().map_or(0, |h| h.as_str().len()))
    }

    fn uf_logical_bytes(&self) -> usize {
        self.names
            .len()
            .saturating_mul(
                std::mem::size_of::<NamedIndividual>()
                    + std::mem::size_of::<u64>()
                    + 3 * std::mem::size_of::<u32>(),
            )
            .saturating_add(self.name_str_bytes)
            .saturating_add(saturate::table_bytes(
                self.ids.len(),
                std::mem::size_of::<u32>(),
            ))
            .saturating_add(self.active_hub.as_ref().map_or(0, |h| h.as_str().len()))
    }

    fn find(&mut self, mut x: u32) -> u32 {
        let mut r = x;
        while self.parent[r as usize] != r {
            r = self.parent[r as usize];
        }
        while self.parent[x as usize] != r {
            let p = self.parent[x as usize];
            self.parent[x as usize] = r;
            x = p;
        }
        r
    }

    fn set_active_hub(
        &mut self,
        hub: Option<NamedIndividual>,
        live_base: usize,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<(), usize> {
        let Some(h) = hub else {
            return Ok(());
        };
        if self.active_hub.as_ref() == Some(&h) {
            return Ok(());
        }
        let add = h.as_str().len();
        let Some(peak) = live_base
            .checked_add(self.uf_bytes())
            .and_then(|b| b.checked_add(add))
        else {
            return Err(usize::MAX);
        };
        if !admit(peak) {
            return Err(peak);
        }
        self.active_hub = Some(h);
        Ok(())
    }

    fn intern_parts(
        &mut self,
        is_anon: bool,
        raw: &str,
        live_base: usize,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<u32, usize> {
        let h = merge_name_hash(is_anon, raw);
        if let Some(&id) = self
            .ids
            .find(h, |&i| merge_name_eq(&self.names[i as usize], is_anon, raw))
        {
            return Ok(id);
        }
        let Ok(id) = u32::try_from(self.names.len()) else {
            return Err(usize::MAX);
        };
        if self.names.len().checked_add(1).is_none() {
            return Err(usize::MAX);
        }
        let Some(str_len) = (if is_anon {
            index::ANONYMOUS.len().checked_add(raw.len())
        } else {
            Some(raw.len())
        }) else {
            return Err(usize::MAX);
        };
        let vec_grow = |len: usize, cap: usize, elem: usize| -> Option<usize> {
            if len == cap {
                dl_clause::next_vec_cap(cap, elem)?.checked_mul(elem)
            } else {
                Some(0)
            }
        };
        let names_add = vec_grow(
            self.names.len(),
            self.names.capacity(),
            std::mem::size_of::<NamedIndividual>(),
        );
        let hashes_add = vec_grow(
            self.hashes.len(),
            self.hashes.capacity(),
            std::mem::size_of::<u64>(),
        );
        let ids_add = if self.ids.len() == self.ids.capacity() {
            dl_clause::next_hash_table_bytes(self.ids.capacity(), std::mem::size_of::<u32>())
        } else {
            Some(0)
        };
        let parent_add = vec_grow(
            self.parent.len(),
            self.parent.capacity(),
            std::mem::size_of::<u32>(),
        );
        let size_add = vec_grow(
            self.size.len(),
            self.size.capacity(),
            std::mem::size_of::<u32>(),
        );
        let best_add = vec_grow(
            self.best.len(),
            self.best.capacity(),
            std::mem::size_of::<u32>(),
        );
        let Some(add) = Some(str_len)
            .and_then(|n| n.checked_add(names_add?))
            .and_then(|n| n.checked_add(hashes_add?))
            .and_then(|n| n.checked_add(ids_add?))
            .and_then(|n| n.checked_add(parent_add?))
            .and_then(|n| n.checked_add(size_add?))
            .and_then(|n| n.checked_add(best_add?))
        else {
            return Err(usize::MAX);
        };
        let Some(peak) = live_base
            .checked_add(self.uf_bytes())
            .and_then(|b| b.checked_add(add))
        else {
            return Err(usize::MAX);
        };
        if !admit(peak) {
            return Err(peak);
        }
        let owned = if is_anon {
            let mut s = String::with_capacity(str_len);
            s.push_str(index::ANONYMOUS);
            s.push_str(raw);
            NamedIndividual::new(s)
        } else {
            NamedIndividual::new(raw.to_owned())
        };
        self.names.push(owned);
        self.hashes.push(h);
        let hashes = &self.hashes;
        self.ids.insert_unique(h, id, |&i| hashes[i as usize]);
        self.parent.push(id);
        self.size.push(1);
        self.best.push(id);
        self.name_str_bytes = self.name_str_bytes.saturating_add(str_len);
        Ok(id)
    }

    fn union_parts(
        &mut self,
        a_anon: bool,
        a_raw: &str,
        b_anon: bool,
        b_raw: &str,
        live_base: usize,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<bool, usize> {
        let (a_anon, a_raw) = canonical_merge_parts(a_anon, a_raw);
        let (b_anon, b_raw) = canonical_merge_parts(b_anon, b_raw);
        if a_anon == b_anon && a_raw == b_raw {
            return Ok(false);
        }
        let a = self.intern_parts(a_anon, a_raw, live_base, admit)?;
        let b = self.intern_parts(b_anon, b_raw, live_base, admit)?;
        let mut ra = self.find(a);
        let mut rb = self.find(b);
        if ra == rb {
            return Ok(false);
        }
        if self.size[ra as usize] < self.size[rb as usize] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb as usize] = ra;
        self.size[ra as usize] = self.size[ra as usize].saturating_add(self.size[rb as usize]);
        let ba = self.best[ra as usize];
        let bb = self.best[rb as usize];
        let hub = self.active_hub.as_ref();
        if rep_key(&self.names[bb as usize], hub) < rep_key(&self.names[ba as usize], hub) {
            self.best[ra as usize] = bb;
        }
        self.shared = None;
        Ok(true)
    }

    fn union_individual(
        &mut self,
        a: &Individual,
        b: &Individual,
        live_base: usize,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<bool, usize> {
        fn parts(i: &Individual) -> (bool, &str) {
            match i {
                Individual::Named(n) => (false, n.as_str()),
                Individual::Anonymous(a) => (true, a.label()),
            }
        }
        let (aa, ar) = parts(a);
        let (ba, br) = parts(b);
        self.union_parts(aa, ar, ba, br, live_base, admit)
    }

    fn union_named(
        &mut self,
        a: &NamedIndividual,
        b: &NamedIndividual,
        live_base: usize,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<bool, usize> {
        self.union_parts(false, a.as_str(), false, b.as_str(), live_base, admit)
    }

    /// Builds a flat immutable `Arc<CanonMap>` where every alias points directly to its
    /// final canonical representative, pre-admitting exact heap bytes (including
    /// `ArcInner`, hash table, owned strings, and temporary root-to-slot index) before
    /// allocating. Returns `Ok(None)` without allocation when no alias exists.
    fn build_canon(
        &mut self,
        live_base: usize,
        limits: &Limits,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<Option<std::sync::Arc<index::CanonMap>>, BuildCanonErr> {
        if self.names.is_empty() {
            self.shared = None;
            return Ok(None);
        }
        let n = self.names.len();
        let mut rep_count = 0usize;
        let mut rep_str_bytes = 0usize;
        let mut alias_count = 0usize;
        let mut alias_str_bytes = 0usize;
        for i in 0..n {
            if i % POLL_AXIOMS == 0 {
                poll(limits).map_err(BuildCanonErr::Poll)?;
            }
            let r = self.find(i as u32) as usize;
            let b = self.best[r] as usize;
            if i == r && self.size[r] > 1 {
                rep_count += 1;
                rep_str_bytes = rep_str_bytes
                    .checked_add(self.names[b].as_str().len())
                    .ok_or(BuildCanonErr::Memory(usize::MAX))?;
            }
            if i != b {
                alias_count += 1;
                alias_str_bytes = alias_str_bytes
                    .checked_add(self.names[i].as_str().len())
                    .ok_or(BuildCanonErr::Memory(usize::MAX))?;
            }
        }
        if alias_count == 0 {
            self.shared = None;
            return Ok(None);
        }
        if u32::try_from(rep_count).is_err() {
            return Err(BuildCanonErr::Memory(usize::MAX));
        }
        let arc_bytes = index::arc_payload_bytes::<index::CanonMap>()
            .ok_or(BuildCanonErr::Memory(usize::MAX))?;
        let reps_vec = rep_count
            .checked_mul(std::mem::size_of::<NamedIndividual>())
            .ok_or(BuildCanonErr::Memory(usize::MAX))?;
        let entry_size = std::mem::size_of::<(NamedIndividual, u32)>();
        let map_table = dl_clause::hash_table_bytes(alias_count, entry_size);
        if map_table == usize::MAX {
            return Err(BuildCanonErr::Memory(usize::MAX));
        }
        let hub_str_bytes = self.active_hub.as_ref().map_or(0, |h| h.as_str().len());
        let planned_owned = arc_bytes
            .checked_add(reps_vec)
            .and_then(|b| b.checked_add(rep_str_bytes))
            .and_then(|b| b.checked_add(map_table))
            .and_then(|b| b.checked_add(alias_str_bytes))
            .and_then(|b| b.checked_add(hub_str_bytes))
            .ok_or(BuildCanonErr::Memory(usize::MAX))?;
        let tmp_slot = n
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(BuildCanonErr::Memory(usize::MAX))?;
        let peak = live_base
            .checked_add(self.uf_bytes())
            .and_then(|b| b.checked_add(planned_owned))
            .and_then(|b| b.checked_add(tmp_slot))
            .ok_or(BuildCanonErr::Memory(usize::MAX))?;
        if !admit(peak) {
            return Err(BuildCanonErr::Memory(peak));
        }
        let mut reps: Vec<NamedIndividual> = Vec::with_capacity(rep_count);
        let mut rep_slot: Vec<u32> = vec![u32::MAX; n];
        for (i, slot_out) in rep_slot.iter_mut().enumerate() {
            if i % POLL_AXIOMS == 0 {
                poll(limits).map_err(BuildCanonErr::Poll)?;
            }
            if self.parent[i] as usize == i && self.size[i] > 1 {
                let b = self.best[i] as usize;
                let slot = reps.len() as u32;
                reps.push(index::exact_individual(&self.names[b]));
                *slot_out = slot;
            }
        }
        let mut map: FxHashMap<NamedIndividual, u32> =
            FxHashMap::with_capacity_and_hasher(alias_count, Default::default());
        for i in 0..n {
            if i % POLL_AXIOMS == 0 {
                poll(limits).map_err(BuildCanonErr::Poll)?;
            }
            let r = self.parent[i] as usize;
            let b = self.best[r] as usize;
            if i != b {
                let slot = rep_slot[r];
                debug_assert_ne!(slot, u32::MAX);
                map.insert(index::exact_individual(&self.names[i]), slot);
            }
        }
        drop(rep_slot);
        let actual_table = dl_clause::hash_table_bytes(map.capacity(), entry_size);
        let actual_owned = arc_bytes
            .saturating_add(
                reps.capacity()
                    .saturating_mul(std::mem::size_of::<NamedIndividual>()),
            )
            .saturating_add(rep_str_bytes)
            .saturating_add(actual_table)
            .saturating_add(alias_str_bytes)
            .saturating_add(hub_str_bytes);
        let logical = arc_bytes
            .saturating_add(
                reps.len()
                    .saturating_mul(std::mem::size_of::<NamedIndividual>()),
            )
            .saturating_add(rep_str_bytes)
            .saturating_add(saturate::table_bytes(map.len(), entry_size))
            .saturating_add(alias_str_bytes)
            .saturating_add(hub_str_bytes);
        let generated_hub = self.active_hub.as_ref().map(index::exact_individual);
        let canon = std::sync::Arc::new(index::CanonMap::new_from_parts(
            reps,
            map,
            generated_hub,
            actual_owned,
            logical,
        ));
        self.shared = Some(canon.clone());
        Ok(Some(canon))
    }
}

/// The rules of a run whose contexts the next run may keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeptTier {
    /// the EL rules, whose invalidation follows the premises of the changed axioms
    El,
    /// the Horn rules without individuals, whose invalidation follows the expressions
    /// whose rules differ between the two indexes
    Horn,
    /// Tier D (cautious or eager strategy) without counting or a root, whose invalidation
    /// follows the expressions that the units, markers, denied classes, registered edge
    /// restrictions, dynamic cores and stored clauses of a context name, the predecessors
    /// of a context and the class of a test that held
    Disj,
}

/// The options that select a run's rules. A run keeps contexts only of a run with the
/// same ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RuleKey {
    el_only: bool,
    counting: bool,
    rooted: bool,
    rooted_merge: bool,
    number_order: bool,
    eager: bool,
    skip_dl: bool,
    no_counting: bool,
    no_rooted: bool,
    /// the reduction of Tier D's automata, which numbers the fresh classes of an unfolding
    dl_automata: u8,
}

/// What a run whose saturation ended leaves for the decision of the next one.
#[derive(Clone, Copy, Debug)]
struct Kept {
    tier: KeptTier,
    key: RuleKey,
    /// whether the run had axioms outside OWL 2 EL, taken or left out, whose tier the
    /// run decided over the interner as it was then
    outside: bool,
    /// whether one of them needs case analysis and [`dl_norm::may_accept`] says Tier D
    /// could accept it if blocking data, self restrictions or top/bottom properties leave
    disjunctive: bool,
    /// the interner's expressions and the contexts above the index's after the last
    /// run that started again, which bound what the runs since may keep
    nodes: usize,
    dynamic: usize,
    /// the clauses of Tier D's table after the last run that started again
    table: usize,
}

/// The expressions and the contexts above the index's that a run may keep beyond twice
/// those of the last run that started again. Past the bound a run starts again, which
/// drops the expressions of removed axioms and the contexts that nothing reaches.
const KEPT_SLACK: usize = 4096;

/// Contexts or edges visited between cooperative deadline/cancellation polls in
/// incremental invalidation, shifting, and orphan pruning.
const POLL_INVALIDATE: usize = 4096;

/// Pushes `e` onto `queue`, growing geometrically via `reserve_exact` without exceeding
/// `cap_limit` elements so worklist capacity never outgrows pre-admitted scratch.
fn push_bounded(queue: &mut Vec<Ex>, e: Ex, cap_limit: usize) {
    if queue.len() == queue.capacity() {
        let remaining = cap_limit.saturating_sub(queue.len());
        let grow = queue.capacity().max(8).min(remaining).max(1);
        queue.reserve_exact(grow);
    }
    queue.push(e);
}

impl Default for Saturation {
    fn default() -> Saturation {
        Saturation::new()
    }
}

/// The axioms added to an ontology and removed from it since a saturation's last run.
#[derive(Clone, Copy, Default)]
pub struct Changes<'a> {
    pub added: &'a [&'a AxiomKind],
    pub removed: &'a [&'a AxiomKind],
}

/// Whether a change to the axiom needs a run that starts again: one that changes the
/// role table or datatype definitions, which a run builds once, a key, one with an
/// individual, whose merges the invalidation does not follow, one with the top or bottom
/// data property, or one with a self restriction (roadmap item 1258). A negative self
/// restriction gives its subsumer to every context with a loop by a subrole, and a loop
/// comes from a self restriction of another role, a reflexive role or a chain as well as
/// from the restriction itself, so a kept context can have the loop without holding the
/// changed expression that the invalidation looks for.
fn needs_full_run(ax: &AxiomKind) -> bool {
    if role_axiom(ax)
        || matches!(
            ax,
            AxiomKind::HasKey(..) | AxiomKind::DatatypeDefinition(..)
        )
    {
        return true;
    }
    // an anonymous individual is an individual of the run, though no signature holds it
    // (roadmap item 1281)
    if profiles::individuals(ax)
        .iter()
        .any(|i| matches!(i, Individual::Anonymous(_)))
    {
        return true;
    }
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    sig.2
        || sig.0.iter().any(|e| match e {
            Entity::NamedIndividual(_) => true,
            Entity::DataProperty(p) => *p == DataProperty::top() || *p == DataProperty::bottom(),
            _ => false,
        })
}

/// Whether `ax` is a logical axiom or declaration mentioning any data property or
/// datatype, so a kept EL run must compare indexes instead of using the EL premise
/// bitset fast path.
fn mentions_data(ax: &AxiomKind) -> bool {
    if !logical(ax) && !matches!(ax, AxiomKind::Declaration(_)) {
        return false;
    }
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    sig.0
        .iter()
        .any(|e| matches!(e, Entity::DataProperty(_) | Entity::Datatype(_)))
}

/// Whether an edit of `ax` is compatible with keeping a Horn (or data-bearing EL)
/// interner when no disjunctive axiom is in play: like [`tier_neutral`], plus class
/// axioms, data property domains, declarations and `DisjointDataProperties` over
/// non-top, non-bottom data properties (roadmap item 753).
fn horn_neutral(ax: &AxiomKind) -> bool {
    use ClassExpression as C;
    fn named(p: &ObjectPropertyExpression) -> bool {
        let p = p.named();
        *p != ObjectProperty::top() && *p != ObjectProperty::bottom()
    }
    fn data_named(p: &DataProperty) -> bool {
        *p != DataProperty::top() && *p != DataProperty::bottom()
    }
    fn plain(c: &ClassExpression) -> bool {
        match c {
            C::Class(_) => true,
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().all(plain),
            C::ObjectComplementOf(c) => plain(c),
            C::ObjectSomeValuesFrom(p, c) | C::ObjectAllValuesFrom(p, c) => named(p) && plain(c),
            C::ObjectMinCardinality(_, p, c)
            | C::ObjectMaxCardinality(_, p, c)
            | C::ObjectExactCardinality(_, p, c) => named(p) && c.as_deref().is_none_or(plain),
            C::DataSomeValuesFrom(ps, _) | C::DataAllValuesFrom(ps, _) => ps.iter().all(data_named),
            C::DataHasValue(p, _)
            | C::DataMinCardinality(_, p, _)
            | C::DataMaxCardinality(_, p, _)
            | C::DataExactCardinality(_, p, _) => data_named(p),
            _ => false,
        }
    }
    match ax {
        _ if !logical(ax) => true,
        AxiomKind::Declaration(e) => match e {
            Entity::ObjectProperty(p) => {
                *p != ObjectProperty::top() && *p != ObjectProperty::bottom()
            }
            Entity::DataProperty(p) => data_named(p),
            _ => true,
        },
        AxiomKind::SubClassOf(a, b) => plain(a) && plain(b),
        AxiomKind::EquivalentClasses(cs) => cs.iter().all(plain),
        AxiomKind::DisjointClasses(cs) | AxiomKind::DisjointUnion(_, cs) => cs.iter().all(plain),
        AxiomKind::ObjectPropertyDomain(p, c) => named(p) && plain(c),
        AxiomKind::DataPropertyDomain(p, c) => data_named(p) && plain(c),
        AxiomKind::DisjointDataProperties(ps) => ps.iter().all(data_named),
        _ => false,
    }
}

/// Whether an edit of the axiom leaves alone what decides a run's tier and what its
/// rules read outside the index's expressions. The choice of tier looks for self
/// restrictions, nominals, data and the top and bottom object properties among every
/// expression the interner holds, so a removed axiom with any of them, or an added one
/// outside OWL 2 EL that a change interns as a premise, could keep Tier D from the
/// axioms that remain. Class axioms and domains built from classes, intersections,
/// unions, complements and existential, universal and number restrictions over other
/// properties are neutral, and so are declarations and annotations. An at-most
/// restriction also decides the tier, and a run that keeps the contexts of the Horn
/// rules or of Tier D finds a tier that it moved once its index is built
/// ([`Saturation::must_restart`]), while a run kept by the EL rules starts again
/// after every change to an axiom outside OWL 2 EL, which a universal or a number
/// restriction is.
fn tier_neutral(ax: &AxiomKind) -> bool {
    use ClassExpression as C;
    fn named(p: &ObjectPropertyExpression) -> bool {
        let p = p.named();
        *p != ObjectProperty::top() && *p != ObjectProperty::bottom()
    }
    fn plain(c: &ClassExpression) -> bool {
        match c {
            C::Class(_) => true,
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().all(plain),
            C::ObjectComplementOf(c) => plain(c),
            C::ObjectSomeValuesFrom(p, c) | C::ObjectAllValuesFrom(p, c) => named(p) && plain(c),
            C::ObjectMinCardinality(_, p, c)
            | C::ObjectMaxCardinality(_, p, c)
            | C::ObjectExactCardinality(_, p, c) => named(p) && c.as_deref().is_none_or(plain),
            _ => false,
        }
    }
    match ax {
        _ if !logical(ax) => true,
        AxiomKind::Declaration(e) => !matches!(
            e,
            Entity::ObjectProperty(p) if *p == ObjectProperty::top() || *p == ObjectProperty::bottom()
        ),
        AxiomKind::SubClassOf(a, b) => plain(a) && plain(b),
        AxiomKind::EquivalentClasses(cs) => cs.iter().all(plain),
        AxiomKind::DisjointClasses(cs) | AxiomKind::DisjointUnion(_, cs) => cs.iter().all(plain),
        AxiomKind::ObjectPropertyDomain(p, c) => named(p) && plain(c),
        _ => false,
    }
}

/// How far the interner's tables reached at one point of a run, to tell whether what
/// was interned since holds anything that a run's choice of tier reads.
struct TierMark {
    nodes: usize,
    roles: usize,
    individuals: usize,
    vals: usize,
}

impl TierMark {
    fn of(it: &Interner) -> TierMark {
        TierMark {
            nodes: it.nodes.len(),
            roles: it.roles.len(),
            individuals: it.individuals.len(),
            vals: it.vals.len(),
        }
    }

    /// Whether the interner has gained, since the mark, an individual, a data value, a
    /// data role, the top or bottom object property, or a nominal, self, data or
    /// at-most-one restriction, which [`dl_norm::eligible`] and
    /// [`Saturation::disj_allowed`] read.
    fn passed(&self, it: &Interner) -> bool {
        it.individuals.len() > self.individuals
            || it.vals.len() > self.vals
            || it.roles[self.roles..].iter().any(|r| match r {
                index::RoleName::Data(_) => true,
                index::RoleName::Object(p) => {
                    *p == ObjectProperty::top() || *p == ObjectProperty::bottom()
                }
                _ => false,
            })
            || it.nodes[self.nodes..].iter().any(|n| {
                matches!(
                    n,
                    Node::Nominal(_)
                        | Node::SelfLoop(_)
                        | Node::DataSome(..)
                        | Node::DtAtom(_)
                        | Node::DtFire(_)
                        | Node::AtMost1(..)
                )
            })
    }
}

/// Whether a change to the axiom affects the role table, which a run builds once.
fn role_axiom(ax: &AxiomKind) -> bool {
    matches!(
        ax,
        AxiomKind::SubObjectPropertyOf(..)
            | AxiomKind::EquivalentObjectProperties(_)
            | AxiomKind::TransitiveObjectProperty(_)
            | AxiomKind::ReflexiveObjectProperty(_)
            | AxiomKind::ObjectPropertyRange(..)
            | AxiomKind::SubDataPropertyOf(..)
            | AxiomKind::EquivalentDataProperties(_)
            | AxiomKind::DataPropertyRange(..)
            | AxiomKind::FunctionalDataProperty(_)
    )
}

/// The first rule of OWL 2 EL that the classifier holds the axiom to and that it breaks,
/// without the global restriction on the ranges of property chains. The classifier takes
/// an inclusion in a top property as an axiom of OWL 2 EL whatever its subproperty
/// expression, since it holds of every ontology ([`index::top_inclusion`]), and takes an
/// anonymous individual as a fresh individual that no answer names (roadmap item 1281).
fn el_break(ax: &AxiomKind) -> Option<profiles::Broken> {
    if index::top_inclusion(ax) {
        return None;
    }
    profiles::el_first(ax)
}

/// Allocation-free, cooperatively polled equivalent of `el_break(ax).is_some()`.
pub(super) fn el_breaks_polled(ax: &AxiomKind, budget: &impl ScratchBudget) -> Result<bool, Error> {
    if index::top_inclusion(ax) {
        return Ok(false);
    }
    profiles::el_breaks_with_anonymous_polled(ax, RANGE_POLL_STEP, &mut || budget.poll())
}

/// The bytes of scratch that [`interchangeable`] counts for each class assertion it
/// reads: the entry of its label and, at most, that of its class.
const LABEL_BYTES: usize = 96;

/// The axioms without the class assertions that another one stands for, or `None` when
/// there are none (roadmap item 1281). A class assertion `C(_:x)` of a named class whose
/// anonymous individual occurs in no other logical axiom, and only once in it, says that
/// `C` has an instance and no more. Two of them with the same class are interchangeable,
/// since an element that stands for one stands for the other, so the one whose
/// individual has the least label stands for every other, which adds nothing. The answer
/// depends on the axioms alone, whatever their order. A class assertion of a named class
/// is an axiom of OWL 2 EL that every run takes in full, so the one that stands for the
/// others is never left out and the report needs none of them. Ontologies that reify
/// axioms or cross-references as anonymous individuals, such as those with `oboInOwl`
/// annotations, have hundreds of thousands of such assertions, and the run then holds
/// one individual for each class instead of one for each assertion.
///
/// The scratch holds an entry for each such assertion's label and class and the list of
/// the axioms the run reads. It is admitted against `max_memory`, beside the `held`
/// bytes of the caller's axioms, and against the pool before it is allocated, and a
/// refusal leaves every assertion with its individual. The second value is the scratch
/// the scan admitted, which the run draws on first when a pool granted it and counts in
/// its peak in any case. Every pass over the
/// axioms polls the deadline and cancellation.
fn interchangeable<'a>(
    axioms: &[&'a AxiomKind],
    held: usize,
    limits: &Limits,
) -> Result<(Option<Vec<&'a AxiomKind>>, usize), Error> {
    fn candidate(k: &AxiomKind) -> Option<(&Class, &str)> {
        match k {
            AxiomKind::ClassAssertion(ClassExpression::Class(c), Individual::Anonymous(a)) => {
                Some((c, a.label()))
            }
            _ => None,
        }
    }
    let mut n = 0usize;
    for (i, k) in axioms.iter().enumerate() {
        if i % POLL_AXIOMS == 0 {
            poll(limits)?;
        }
        n += candidate(k).is_some() as usize;
    }
    if n < 2 {
        return Ok((None, 0));
    }
    let scratch = n.saturating_mul(LABEL_BYTES).saturating_add(
        axioms
            .len()
            .saturating_mul(std::mem::size_of::<&AxiomKind>()),
    );
    if limits
        .max_memory
        .is_some_and(|m| held.saturating_add(scratch) > m)
    {
        return Ok((None, 0));
    }
    if limits.charge.as_ref().is_some_and(|f| !f(scratch)) {
        return Ok((None, 0));
    }
    // how often the label of each candidate occurs in the logical axioms
    let mut count: FxHashMap<&str, u32> = FxHashMap::default();
    count.reserve(n);
    for (i, k) in axioms.iter().enumerate() {
        if i % POLL_AXIOMS == 0 {
            poll(limits)?;
        }
        if let Some((_, l)) = candidate(k) {
            count.insert(l, 0);
        }
    }
    for (i, k) in axioms.iter().enumerate() {
        if i % POLL_AXIOMS == 0 {
            poll(limits)?;
        }
        if !logical(k) {
            continue;
        }
        for ind in profiles::individuals(k) {
            if let Individual::Anonymous(a) = ind
                && let Some(c) = count.get_mut(a.label())
            {
                *c = c.saturating_add(1);
            }
        }
    }
    // the label that stands for each class: the least among its lone assertions
    let lone = |k: &'a AxiomKind| candidate(k).filter(|(_, l)| count.get(l) == Some(&1));
    let mut first: FxHashMap<&Class, &str> = FxHashMap::default();
    for (i, &k) in axioms.iter().enumerate() {
        if i % POLL_AXIOMS == 0 {
            poll(limits)?;
        }
        if let Some((c, l)) = lone(k) {
            first.entry(c).and_modify(|f| *f = (*f).min(l)).or_insert(l);
        }
    }
    let redundant = |k: &'a AxiomKind| lone(k).is_some_and(|(c, l)| first.get(c) != Some(&l));
    let mut dropped = 0usize;
    for (i, &k) in axioms.iter().enumerate() {
        if i % POLL_AXIOMS == 0 {
            poll(limits)?;
        }
        dropped += redundant(k) as usize;
    }
    if dropped == 0 {
        return Ok((None, scratch));
    }
    // the list the run reads, whose capacity the scratch admitted
    let mut kept = Vec::with_capacity(axioms.len() - dropped);
    for (i, &k) in axioms.iter().enumerate() {
        if i % POLL_AXIOMS == 0 {
            poll(limits)?;
        }
        if !redundant(k) {
            kept.push(k);
        }
    }
    Ok((Some(kept), scratch))
}

/// Whether the axiom has logical content for classification.
pub fn logical(ax: &AxiomKind) -> bool {
    !matches!(
        ax,
        AxiomKind::AnnotationAssertion(..)
            | AxiomKind::SubAnnotationPropertyOf(..)
            | AxiomKind::AnnotationPropertyDomain(..)
            | AxiomKind::AnnotationPropertyRange(..)
    )
}

/// Rough heap and inline storage of the operands in a `Multiset`-backed axiom, counting
/// backing vector slots and repeated operand allocations that a distinct-entity signature
/// alone omits. Depends only on the axiom's value, never on `Vec::capacity()`.
fn multiset_bytes(ax: &AxiomKind) -> usize {
    use std::mem::size_of;
    fn ce_extra(c: &ClassExpression) -> usize {
        use ClassExpression as C;
        match c {
            C::Class(k) => k.as_str().len(),
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs
                .len()
                .saturating_mul(size_of::<ClassExpression>())
                .saturating_add(cs.iter().map(ce_extra).sum()),
            C::ObjectComplementOf(f) => size_of::<ClassExpression>().saturating_add(ce_extra(f)),
            C::ObjectOneOf(is) => is
                .len()
                .saturating_mul(size_of::<Individual>())
                .saturating_add(is.iter().map(ind_extra).sum()),
            C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => p
                .named()
                .as_str()
                .len()
                .saturating_add(size_of::<ClassExpression>())
                .saturating_add(ce_extra(f)),
            C::ObjectHasValue(p, i) => p.named().as_str().len().saturating_add(ind_extra(i)),
            C::ObjectHasSelf(p) => p.named().as_str().len(),
            C::ObjectMinCardinality(_, p, f)
            | C::ObjectMaxCardinality(_, p, f)
            | C::ObjectExactCardinality(_, p, f) => {
                p.named()
                    .as_str()
                    .len()
                    .saturating_add(f.as_deref().map_or(0, |f| {
                        size_of::<ClassExpression>().saturating_add(ce_extra(f))
                    }))
            }
            C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => ps
                .len()
                .saturating_mul(size_of::<DataProperty>())
                .saturating_add(ps.iter().map(|d| d.as_str().len()).sum())
                .saturating_add(dtcheck::range::data_range_bytes(r)),
            C::DataHasValue(d, l) => d
                .as_str()
                .len()
                .saturating_add(l.value().len())
                .saturating_add(l.datatype().len())
                .saturating_add(l.language().map_or(0, str::len)),
            C::DataMinCardinality(_, d, r)
            | C::DataMaxCardinality(_, d, r)
            | C::DataExactCardinality(_, d, r) => d
                .as_str()
                .len()
                .saturating_add(r.as_ref().map_or(0, dtcheck::range::data_range_bytes)),
        }
    }
    fn ind_extra(i: &Individual) -> usize {
        match i {
            Individual::Named(n) => n.as_str().len(),
            Individual::Anonymous(a) => a.0.as_str().len(),
        }
    }
    fn dup_extra<T: Eq>(ms: &Multiset<T>, extra: impl Fn(&T) -> usize) -> usize {
        let slice = ms.as_slice();
        let slots = slice.len().saturating_mul(size_of::<T>());
        let dup_heap: usize = slice
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| extra(&w[1]))
            .sum();
        slots.saturating_add(dup_heap)
    }
    match ax {
        AxiomKind::DisjointClasses(cs) | AxiomKind::DisjointUnion(_, cs) => dup_extra(cs, ce_extra),
        AxiomKind::DisjointObjectProperties(ps) => dup_extra(ps, |p| p.named().as_str().len()),
        AxiomKind::DisjointDataProperties(ps) => dup_extra(ps, |d| d.as_str().len()),
        AxiomKind::DifferentIndividuals(is) => dup_extra(is, ind_extra),
        _ => 0,
    }
}

/// The bytes the classifier's copy of an axiom takes, roughly: the axiom, its entry,
/// each entity's IRI with the expression that holds it, and repeated multiset operand
/// storage.
fn held_bytes_of(ax: &AxiomKind) -> usize {
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    let mut bytes =
        std::mem::size_of::<AxiomKind>() + std::mem::size_of::<Held>() + 16 + multiset_bytes(ax);
    for e in &sig.0 {
        bytes += e.iri().as_str().len() + 64;
    }
    bytes
}

/// Exact allocation-free estimate of heap bytes owned by a retained copy of `excluded`,
/// including anonymous blank-node labels, literals, containers, repeated AST payloads,
/// and `Gap::Outside` strings. This is not a process RSS bound.
pub fn excluded_copy_bytes(excluded: &Vec<Excluded>) -> usize {
    use std::mem::size_of;
    excluded
        .capacity()
        .saturating_mul(size_of::<Excluded>())
        .saturating_add(
            excluded
                .iter()
                .map(|e| {
                    dl_reduce::axiom_retained_heap_bytes(&e.axiom).saturating_add(match &e.gap {
                        Gap::Outside { rule, message } => {
                            rule.capacity().saturating_add(message.capacity())
                        }
                        Gap::Partial(_) => 0,
                    })
                })
                .sum::<usize>(),
        )
}

/// The threads a run takes.
fn threads_of(limits: &Limits) -> usize {
    if limits.threads == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        limits.threads
    }
}

/// The classes below which a parallel step over classes or nodes runs on one thread.
pub(crate) const MIN_CLASSES: usize = 1024;
/// The axioms below which a parallel step over axioms runs on one thread, more than
/// [`MIN_CLASSES`] since an axiom costs less than a class's step (roadmap item 709).
// Tests keep the smaller chunks so that their few thousand axioms still split across
// threads.
const MIN_AXIOMS: usize = if cfg!(test) { 1024 } else { 4096 };

/// Runs `f` on chunks of `items` of at least `min` items, on up to `threads` scoped
/// threads.
pub(crate) fn par_chunks<T: Send>(
    items: &mut [T],
    threads: usize,
    min: usize,
    f: impl Fn(&mut [T]) + Sync,
) {
    let threads = threads.clamp(1, items.len().div_ceil(min).max(1));
    if threads == 1 {
        f(items);
        return;
    }
    let size = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        for chunk in items.chunks_mut(size) {
            let f = &f;
            s.spawn(move || f(chunk));
        }
    });
}

/// The answers of `f` on chunks of `items` of at least `min` items, in order, computed on
/// up to `threads` scoped threads. `f` takes the offset of its chunk.
pub(crate) fn par_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    min: usize,
    f: impl Fn(usize, &[T]) -> R + Sync,
) -> Vec<R> {
    let threads = threads.clamp(1, items.len().div_ceil(min).max(1));
    if threads == 1 {
        return vec![f(0, items)];
    }
    let size = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(size)
            .enumerate()
            .map(|(i, chunk)| {
                let f = &f;
                s.spawn(move || f(i * size, chunk))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

impl Classifier {
    pub fn new() -> Classifier {
        Classifier {
            axioms: FxHashMap::default(),
            live: 0,
            held_bytes: 0,
            sat: Saturation::new(),
        }
    }

    /// A classifier over the axioms of an ontology.
    pub fn of<'a>(axioms: impl IntoIterator<Item = &'a Axiom>) -> Classifier {
        let mut k = Classifier::new();
        k.add(axioms);
        k
    }

    /// Adds axioms. Annotations are ignored, and an axiom added twice must be removed
    /// twice.
    pub fn add<'a>(&mut self, axioms: impl IntoIterator<Item = &'a Axiom>) {
        self.add_kinds(axioms.into_iter().map(|a| &a.kind));
    }

    fn add_kinds<'a>(&mut self, axioms: impl IntoIterator<Item = &'a AxiomKind>) {
        let axioms = axioms.into_iter();
        // growing the table would hash every axiom it holds again
        self.axioms.reserve(axioms.size_hint().0);
        for ax in axioms {
            self.add_kind(ax);
        }
    }

    fn add_kind(&mut self, ax: &AxiomKind) {
        if !logical(ax) {
            return;
        }
        match self.axioms.get_mut(ax) {
            Some(h) => {
                if h.count == 0 {
                    self.live += 1;
                }
                h.count += 1;
            }
            None => {
                self.live += 1;
                self.axioms.insert(ax.clone(), Held { count: 1, ran: 0 });
            }
        }
    }

    /// Removes axioms. Removing an axiom the classifier does not hold, counting the
    /// changes not yet classified, does nothing.
    pub fn remove<'a>(&mut self, axioms: impl IntoIterator<Item = &'a Axiom>) {
        for ax in axioms {
            self.remove_kind(&ax.kind);
        }
    }

    fn remove_kind(&mut self, ax: &AxiomKind) {
        if let Some(h) = self.axioms.get_mut(ax)
            && h.count > 0
        {
            h.count -= 1;
            if h.count == 0 {
                self.live -= 1;
            }
        }
    }

    /// The number of distinct axioms held, changes not yet classified included.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The axioms of the last run that it left out or used only in part, in order.
    pub fn excluded(&self) -> Vec<Excluded> {
        self.sat.excluded().to_vec()
    }

    /// Classifies the axioms held, saturating again only what changed since the last
    /// run.
    pub fn classify(&mut self, limits: &Limits) -> Result<Classification, Error> {
        let r = self.run_held(limits, true);
        let partial = self.sat.partial.take();
        let res = (|| {
            let mut out = r.map(|(consistent, taxonomy, stats)| {
                Classification::new(consistent, taxonomy, stats, partial)
            })?;
            let excluded = self.sat.excluded();
            if !excluded.is_empty() {
                if limits.strict {
                    return Err(Error::Incomplete(excluded.to_vec()));
                }
                let held: Vec<&AxiomKind> = self
                    .axioms
                    .iter()
                    .filter(|(_, h)| h.count > 0)
                    .map(|(k, _)| k)
                    .collect();
                if !limits.cover_later {
                    out.cover(&held, excluded, None, limits);
                }
            }
            if limits.strict && !out.complete {
                return Err(Error::Incomplete(excluded.to_vec()));
            }
            Ok(out)
        })();
        self.sat.emit_adaptive_classify_outcome(&res);
        res
    }

    /// Saturates the axioms held as [`classify`](Self::classify) does, again only what
    /// changed since the last run, but builds no taxonomy. [`describe`](Self::describe)
    /// then answers for one class from the saturated contexts.
    pub fn saturate(&mut self, limits: &Limits) -> Result<Saturated, Error> {
        let res = self
            .run_held(limits, false)
            .map(|(consistent, _, stats)| Saturated { consistent, stats });
        self.sat.emit_adaptive_saturate_outcome(&res);
        res
    }

    /// What the last run says about the class `c` ([`Saturation::describe`]), or `None`
    /// also when axioms were added or removed since.
    pub fn describe(&self, c: &Class) -> Option<Description> {
        if self.axioms.values().any(|h| h.ran != h.count) {
            return None;
        }
        self.sat.describe(c)
    }

    fn run_held(
        &mut self,
        limits: &Limits,
        with_taxonomy: bool,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        let threads = threads_of(limits);
        let mut added: Vec<&AxiomKind> = Vec::new();
        let mut removed: Vec<&AxiomKind> = Vec::new();
        let mut held: Vec<&AxiomKind> = Vec::with_capacity(self.live);
        for (ax, h) in &self.axioms {
            match (h.ran > 0, h.count > 0) {
                (false, true) => added.push(ax),
                (true, false) => removed.push(ax),
                _ => {}
            }
            if h.count > 0 {
                held.push(ax);
            }
        }
        // the sizes of the axioms that came and went, which depend on each alone
        let size = |xs: &[&AxiomKind]| -> usize {
            par_map(xs, threads, MIN_AXIOMS, |_, c| {
                c.iter().map(|a| held_bytes_of(a)).sum::<usize>()
            })
            .into_iter()
            .sum()
        };
        let held_bytes = (self.held_bytes + size(&added)).saturating_sub(size(&removed));
        let changes = Changes {
            added: &added,
            removed: &removed,
        };
        let r = self
            .sat
            .run_with(&held, Some(changes), held_bytes, limits, with_taxonomy);
        drop((added, removed, held));
        // the changes are applied whether the run ends or not
        self.held_bytes = held_bytes;
        if self.axioms.len() > self.live {
            self.axioms.retain(|_, h| h.count > 0);
        }
        for h in self.axioms.values_mut() {
            h.ran = h.count;
        }
        r
    }
}

type RunOutcome = ControlFlow<LateRestart, (bool, Option<Taxonomy>, Stats)>;

/// Distinguishes the caller's original default attempt from rewrites and fallbacks.
#[derive(Clone, Copy)]
enum AttemptProvenance {
    OriginalDefault,
    Ineligible,
}

/// Why an incremental attempt must start over after building its index.
#[derive(Debug)]
enum LateRestart {
    Incompatible { normalized_peak: usize },
    NamedDomain { normalized_peak: usize },
}

impl Saturation {
    pub fn new() -> Saturation {
        Saturation {
            #[cfg(test)]
            top_chain_probe: None,
            interner: Interner::new(),
            index: None,
            ctx: Vec::new(),
            source_len: None,
            excluded: Vec::new(),
            dirty: true,
            worlds: FxHashMap::default(),
            query_cache: std::sync::OnceLock::new(),
            entangled: false,
            stale_tier: false,
            consistent: true,
            dynamic: None,
            disjoint: Vec::new(),
            horn_tried: false,
            dl_tried: false,
            skip_dl: false,
            no_counting: false,
            no_rooted: false,
            rooted_declined: false,
            hide_internal: false,
            reduced: false,
            dl_rooted: false,
            dl_counted: false,
            dl_attempt: false,
            dl_worked: Default::default(),
            partial_over: false,
            numeric_halt: false,
            partial: None,
            kept: None,
            reused: false,
            dl_run: None,
            last_roots: Vec::new(),
            adaptive_run_id: 0,
            adaptive_attempts: 0,
            last_paid: 0,
            merge: Merge::default(),
            canon_want: None,
            active_hub: None,
            merge_stop: None,
            merge_disabled: false,
        }
    }

    /// The axioms of the last run that it left out or used only in part, in order.
    pub fn excluded(&self) -> &[Excluded] {
        &self.excluded
    }

    /// Classifies `axioms`, the logical axioms of an ontology, of which annotations are
    /// passed over and repeats count once. `changes` are the axioms added and removed
    /// since the last run, which then saturates again only what they can reach; without
    /// them, and after a run that stopped before its saturation ended, the run starts
    /// again.
    pub fn classify(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        limits: &Limits,
    ) -> Result<Classification, Error> {
        let r = self.run_with(axioms, changes, 0, limits, true);
        let partial = self.partial.take();
        let res = (|| {
            let mut out = r.map(|(consistent, taxonomy, stats)| {
                Classification::new(consistent, taxonomy, stats, partial)
            })?;
            if !self.excluded.is_empty() {
                if limits.strict {
                    return Err(Error::Incomplete(self.excluded.clone()));
                }
                if !limits.cover_later {
                    out.cover(axioms, &self.excluded, None, limits);
                }
            }
            if limits.strict && !out.complete {
                return Err(Error::Incomplete(self.excluded.clone()));
            }
            Ok(out)
        })();
        self.emit_adaptive_classify_outcome(&res);
        res
    }

    /// Saturates `axioms` as [`classify`](Saturation::classify) does, but builds no
    /// taxonomy. [`describe`](Saturation::describe) then answers for one class from the
    /// saturated contexts, which costs far less than a taxonomy of every class when
    /// only a few classes are asked about.
    pub fn saturate(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        limits: &Limits,
    ) -> Result<Saturated, Error> {
        let res = self
            .run_with(axioms, changes, 0, limits, false)
            .map(|(consistent, _, stats)| Saturated { consistent, stats });
        self.emit_adaptive_saturate_outcome(&res);
        res
    }

    /// [`classify`](Saturation::classify), with the bytes of the caller's copy of the
    /// axioms, which the run counts as its own.
    fn run_with(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        with_taxonomy: bool,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        self.adaptive_attempts = 0;
        self.adaptive_run_id = 0;
        #[cfg(test)]
        LAST_ADAPTIVE_ATTEMPTS.with(|c| c.borrow_mut().clear());
        #[cfg(test)]
        LAST_ADAPTIVE_OUTCOME.with(|c| *c.borrow_mut() = None);
        #[cfg(test)]
        LAST_ADAPTIVE_PREADMIT.with(|c| c.set(None));
        // the class assertions that another one stands for add nothing (roadmap item
        // 1281), and the run reads the other axioms, whose list it holds beside the
        // caller's
        let (kept, scratch) = match interchangeable(axioms, held_bytes, limits) {
            Ok(k) => k,
            Err(e) => {
                // the caller has applied its changes, which a later run must still read
                self.dirty = true;
                return Err(e);
            }
        };
        // the run's charges draw first on what the pool admitted for the scan
        let credited;
        let limits = match &limits.charge {
            Some(f) if scratch > 0 => {
                credited = Limits {
                    charge: Some(drawing_on(scratch, f.clone())),
                    ..limits.clone()
                };
                &credited
            }
            _ => limits,
        };
        let mut r = match kept {
            None => self.run_with_all(axioms, changes, held_bytes, limits, with_taxonomy),
            Some(kept) => {
                let kept_bytes = kept.capacity() * std::mem::size_of::<&AxiomKind>();
                // a run that reads them all counts the caller's axioms as added
                self.source_len = Some(axioms.len());
                let r = self.run_with_all(
                    &kept,
                    changes,
                    held_bytes + kept_bytes,
                    limits,
                    with_taxonomy,
                );
                self.source_len = None;
                r
            }
        };
        if let Ok((_, _, stats)) = &mut r {
            stats.peak_memory = stats.peak_memory.max(held_bytes + scratch);
        }
        r
    }

    /// Wraps `limits.charge` so charges draw first on what `admitted` already covers, and
    /// any additional granted charge increases `admitted`.
    fn charge_drawing_on(
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
    ) -> Option<std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync>> {
        limits.charge.clone().map(|f| {
            let a = admitted.clone();
            let counting: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                std::sync::Arc::new(move |bytes: usize| {
                    let ok = f(bytes);
                    if ok {
                        a.fetch_add(bytes, Ordering::Relaxed);
                    }
                    ok
                });
            drawing_on(admitted.load(Ordering::Relaxed), counting)
        })
    }

    /// Collects asserted `SameIndividual` axioms (both named and anonymous members) into
    /// `self.merge` and builds the initial `CanonMap`. On memory admission refusal,
    /// discards the provisional partition and disables merging for the rest of this call
    /// (optional prepass policy). Cancellation and timeout errors propagate immediately
    /// after invalidating any cached state (`self.dirty = true`).
    fn collect_same(
        &mut self,
        axioms: &[&AxiomKind],
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
    ) -> Result<(), Error> {
        let mut has_same = false;
        for (ops, k) in axioms.iter().enumerate() {
            if ops.is_multiple_of(POLL_AXIOMS)
                && let Err(e) = poll(limits)
            {
                self.dirty = true;
                return Err(e);
            }
            if let AxiomKind::SameIndividual(is) = k
                && is.len() >= 2
            {
                has_same = true;
                break;
            }
        }
        if !has_same {
            return Ok(());
        }
        // Individuals always require a full run, so release any stale prior state before
        // growing the merge tables. `self.excluded` survives `reset()` so a cancelled
        // prepass keeps the prior public exclusion report, and its bytes count in
        // `prepass_base`.
        self.dirty = true;
        self.reset();
        self.merge.clear();
        self.canon_want = None;
        let prepass_base = held_bytes.saturating_add(excluded_copy_bytes(&self.excluded));
        let mut paid = admitted.load(Ordering::Relaxed);
        let charge_fn = limits
            .charge
            .as_deref()
            .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
        let mut admit = |used: usize| -> bool {
            let ok = cover_memory(used, limits.max_memory, charge_fn, &mut paid);
            if ok {
                *call_peak = (*call_peak).max(used);
                admitted.store(paid, Ordering::Relaxed);
            }
            ok
        };
        let mut step = 0usize;
        for k in axioms {
            if step.is_multiple_of(POLL_AXIOMS)
                && let Err(e) = poll(limits)
            {
                self.merge.clear();
                self.dirty = true;
                return Err(e);
            }
            step += 1;
            let AxiomKind::SameIndividual(is) = k else {
                continue;
            };
            if is.len() < 2 {
                continue;
            }
            let mut it = is.iter();
            let Some(first) = it.next() else {
                continue;
            };
            for other in it {
                step += 1;
                if step.is_multiple_of(POLL_AXIOMS)
                    && let Err(e) = poll(limits)
                {
                    self.merge.clear();
                    self.dirty = true;
                    return Err(e);
                }
                if self
                    .merge
                    .union_individual(first, other, prepass_base, &mut admit)
                    .is_err()
                {
                    self.merge.clear();
                    self.canon_want = None;
                    if let Err(e) = poll(limits) {
                        self.dirty = true;
                        return Err(e);
                    }
                    self.merge_disabled = true;
                    return Ok(());
                }
            }
        }
        match self.merge.build_canon(prepass_base, limits, &mut admit) {
            Ok(_) => Ok(()),
            Err(BuildCanonErr::Poll(e)) => {
                self.merge.clear();
                self.canon_want = None;
                self.dirty = true;
                Err(e)
            }
            Err(BuildCanonErr::Memory(_)) => {
                self.merge.clear();
                self.canon_want = None;
                if let Err(e) = poll(limits) {
                    self.dirty = true;
                    return Err(e);
                }
                self.merge_disabled = true;
                Ok(())
            }
        }
    }

    /// Runs `self.run(...)`, handling item 1061 phase 2 clean constant-merge restarts
    /// against the same enclosing work bound while preserving memory high-water marks,
    /// pool credit, deadlines, cancellation, and genuine fallback semantics.
    #[allow(clippy::too_many_arguments)]
    fn run_merging(
        &mut self,
        axioms: &[&AxiomKind],
        mut changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
        with_taxonomy: bool,
        attempt: AttemptProvenance,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        self.merge.spent = 0;
        self.merge.restarts = 0;
        self.merge.frozen_bound = None;
        self.merge_stop = None;
        loop {
            let r = self.run(
                axioms,
                changes.take(),
                held_bytes,
                limits,
                admitted,
                call_peak,
                with_taxonomy,
                attempt,
            );
            match r {
                Ok((consistent, taxonomy, mut stats)) => {
                    stats.dl_rooted_merged = self.interner.canon.as_ref().map_or(0, |c| c.len());
                    stats.dl_rooted_spent = self.merge.spent;
                    stats.dl_rooted_restarts = self.merge.restarts;
                    stats.peak_memory = stats.peak_memory.max(*call_peak);
                    *call_peak = stats.peak_memory;
                    // Drop UF tables after the answering run while `self.interner.canon`
                    // retains the sole `Arc<CanonMap>` for `describe()`.
                    self.merge.clear();
                    self.canon_want = None;
                    return Ok((consistent, taxonomy, stats));
                }
                Err(e) => {
                    let Some(stop) = self.merge_stop.take() else {
                        if let Error::Memory { used, .. } = e {
                            *call_peak = (*call_peak).max(used);
                        }
                        self.merge.clear();
                        self.canon_want = None;
                        return Err(e);
                    };
                    *call_peak = (*call_peak).max(stop.peak);
                    if let Err(poll_err) = poll(limits) {
                        self.merge.clear();
                        self.canon_want = None;
                        self.reset();
                        self.dirty = true;
                        return Err(poll_err);
                    }
                    // Detach individual names first, then drop all heavy state and old
                    // `Arc<CanonMap>` owners BEFORE growing `Merge` or building the new
                    // `CanonMap`, accounting explicitly for `self.excluded`, `old_inds`,
                    // and `stop.pairs`.
                    let old_inds = std::mem::take(&mut self.interner.individuals);
                    let old_inds_bytes = old_inds.exact_bytes();
                    let pair_count = stop.pairs.len();
                    self.reset();
                    self.reduced = false;
                    self.canon_want = None;
                    self.merge.shared = None;

                    let excluded_bytes = excluded_copy_bytes(&self.excluded);
                    let mut paid = admitted.load(Ordering::Relaxed);
                    let charge_fn = limits
                        .charge
                        .as_deref()
                        .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
                    let mut admit = |used: usize| -> bool {
                        let ok = cover_memory(used, limits.max_memory, charge_fn, &mut paid);
                        if ok {
                            *call_peak = (*call_peak).max(used);
                            admitted.store(paid, Ordering::Relaxed);
                        }
                        ok
                    };
                    let union_base = held_bytes
                        .saturating_add(excluded_bytes)
                        .saturating_add(old_inds_bytes)
                        .saturating_add(stop.pairs_bytes);
                    if let Err(used) =
                        self.merge
                            .set_active_hub(stop.active_hub, union_base, &mut admit)
                    {
                        drop(old_inds);
                        drop(stop.pairs);
                        self.merge.clear();
                        self.dl_worked.store(false, Ordering::Relaxed);
                        poll(limits)?;
                        return Err(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used,
                        });
                    }
                    let mut joined = false;
                    let mut invalid_id = false;
                    for (idx, &(u, v)) in stop.pairs.iter().enumerate() {
                        if idx % POLL_AXIOMS == 0
                            && let Err(poll_err) = poll(limits)
                        {
                            drop(old_inds);
                            drop(stop.pairs);
                            self.merge.clear();
                            self.dirty = true;
                            return Err(poll_err);
                        }
                        let old_slice: &[NamedIndividual] = &old_inds;
                        let (Some(nu), Some(nv)) =
                            (old_slice.get(u as usize), old_slice.get(v as usize))
                        else {
                            invalid_id = true;
                            break;
                        };
                        match self.merge.union_named(nu, nv, union_base, &mut admit) {
                            Ok(did_join) => joined |= did_join,
                            Err(used) => {
                                drop(old_inds);
                                drop(stop.pairs);
                                self.merge.clear();
                                self.dl_worked.store(false, Ordering::Relaxed);
                                poll(limits)?;
                                return Err(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used,
                                });
                            }
                        }
                    }
                    drop(stop.pairs);
                    drop(old_inds);
                    if invalid_id || !joined {
                        debug_assert!(
                            !invalid_id && joined,
                            "merge restart had no valid union progress (invalid_id={invalid_id}, joined={joined})"
                        );
                        self.merge.clear();
                        self.canon_want = None;
                        self.merge_disabled = true;
                        self.dl_worked.store(false, Ordering::Relaxed);
                        return Err(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: *call_peak,
                        });
                    }
                    let canon_base = held_bytes.saturating_add(excluded_bytes);
                    match self.merge.build_canon(canon_base, limits, &mut admit) {
                        Ok(_) => {}
                        Err(BuildCanonErr::Poll(poll_err)) => {
                            self.merge.clear();
                            self.canon_want = None;
                            self.dirty = true;
                            return Err(poll_err);
                        }
                        Err(BuildCanonErr::Memory(used)) => {
                            self.merge.clear();
                            self.canon_want = None;
                            self.dl_worked.store(false, Ordering::Relaxed);
                            poll(limits)?;
                            return Err(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used,
                            });
                        }
                    }
                    self.merge.spent = stop.spent;
                    self.merge.restarts += 1;
                    #[cfg(test)]
                    admission_test::emit_merge_phase(admission_test::MergePhase::MergeRestart {
                        restarts: self.merge.restarts,
                    });
                    self.merge.frozen_bound.get_or_insert(stop.bound);
                    if merge_trace() {
                        eprintln!(
                            "MERGE restart={} spent={} bound={} merged={} pairs={}",
                            self.merge.restarts,
                            self.merge.spent,
                            stop.bound,
                            self.merge.shared.as_ref().map_or(0, |c| c.len()),
                            pair_count,
                        );
                    }
                }
            }
        }
    }

    /// [`run_with`](Saturation::run_with) over every axiom it is given.
    fn run_with_all(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        with_taxonomy: bool,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        // what a pool admitted for a run that may fall back or start again, so that the
        // later run charges only what it needs beyond that
        let admitted = std::sync::Arc::new(AtomicUsize::new(0));
        self.merge.clear();
        self.canon_want = None;
        self.active_hub = None;
        self.merge_stop = None;
        self.merge_disabled = false;
        let mut call_peak = 0usize;
        if limits.rooted && limits.rooted_merge && !limits.el_only {
            self.collect_same(axioms, held_bytes, limits, &admitted, &mut call_peak)?;
        }
        self.dl_worked.store(false, Ordering::Relaxed);
        self.rooted_declined = false;
        let attempt = if limits.el_only || limits.rooted {
            AttemptProvenance::Ineligible
        } else {
            AttemptProvenance::OriginalDefault
        };
        let mut r = self.run_merging(
            axioms,
            changes,
            held_bytes,
            limits,
            &admitted,
            &mut call_peak,
            with_taxonomy,
            attempt,
        );
        if let Err(Error::Memory { .. }) = r
            && self.reused
        {
            #[cfg(test)]
            admission_test::hit(admission_test::Point::KeptRetry {
                dl_worked: self.dl_worked.load(Ordering::Relaxed),
            });
            // A run that kept contexts passed its budget, which a run that starts again
            // may not: what it keeps of the contexts that nothing reaches any more, and
            // for a run with the Horn rules or Tier D the last index, which it holds
            // until the new one is built, count against the budget too. That holds for
            // a run kept by the EL rules alone as well (roadmap item 1263). The run starts
            // again in full with the same rules before a lower tier takes over, so that
            // its answer is that of a run without the kept contexts (roadmap item 753).
            // That holds for a stop on the budget of exact numeric values too, which
            // counts the kept state as well, and for the bound on derived conclusions of
            // a run of Tier D that neither counts nor is rooted, which counts the
            // conclusions of the runs that derived the kept contexts (roadmap item 990).
            // Its charges draw first on what the pool admitted. Whether it passed a bound
            // on derived conclusions, or withdrew a rooted answer, is the new run's to say.
            self.reset();
            self.merge.clear();
            self.canon_want = None;
            self.dl_worked.store(false, Ordering::Relaxed);
            self.rooted_declined = false;
            if limits.rooted && limits.rooted_merge && !limits.el_only && !self.merge_disabled {
                self.collect_same(axioms, held_bytes, limits, &admitted, &mut call_peak)?;
            }
            r = self.run_merging(
                axioms,
                None,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
                attempt,
            );
        }
        // a rooted run that withdrew its answer starts again without the rooted rules, here
        // and after each retry below that can run the rooted rules again
        let mut declined = None;
        self.restart_declined(
            &mut r,
            &mut declined,
            axioms,
            held_bytes,
            limits,
            &admitted,
            &mut call_peak,
            with_taxonomy,
        );
        if let Err(Error::Memory { .. }) = r
            && self.dl_tried
            && limits.threads != 1
            && !limits.dl_steps
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
            && self.dl_worked.load(Ordering::Relaxed)
            && !self.dl_attempt
        {
            // Tier D passed its bound on derived conclusions, and how many it derives
            // depends on the order in which the threads delivered the contexts'
            // conclusions: the run starts again on one thread, whose order is fixed, before
            // it falls back to the Horn rules. A schedule that stays within the bound where
            // one thread does not still answers in full. A run in steps
            // (`Limits::dl_steps`) derives the same on every schedule and does not repeat.
            // Its charges draw first on what the pool admitted for the first run.
            self.reset();
            self.merge.clear();
            self.canon_want = None;
            self.dl_worked.store(false, Ordering::Relaxed);
            let one = Limits {
                threads: 1,
                ..limits.clone()
            };
            if one.rooted
                && one.rooted_merge
                && !one.el_only
                && !self.no_rooted
                && !self.merge_disabled
                && let Err(e) =
                    self.collect_same(axioms, held_bytes, &one, &admitted, &mut call_peak)
            {
                if declined.is_some() {
                    self.no_rooted = false;
                }
                return Err(e);
            }
            r = self.run_merging(
                axioms,
                None,
                held_bytes,
                &one,
                &admitted,
                &mut call_peak,
                with_taxonomy,
                AttemptProvenance::Ineligible,
            );
            self.restart_declined(
                &mut r,
                &mut declined,
                axioms,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
            );
        }
        let mut counting_over = None;
        if let Err(Error::Memory { used: counted, .. }) = r
            && self.dl_tried
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
            && self.dl_worked.load(Ordering::Relaxed)
            && self.dl_counted
        {
            // Tier D with counting passed its bound on derived conclusions, which a run
            // without counting never does: the run starts again with the axioms that need
            // counting left out and reported, as before counting was built, before it falls
            // back to the Horn rules. Its charges draw first on what the pool admitted.
            self.reset();
            self.merge.clear();
            self.canon_want = None;
            self.dl_worked.store(false, Ordering::Relaxed);
            self.no_counting = true;
            if limits.rooted
                && limits.rooted_merge
                && !limits.el_only
                && !self.no_rooted
                && !self.merge_disabled
            {
                let collect =
                    self.collect_same(axioms, held_bytes, limits, &admitted, &mut call_peak);
                if let Err(e) = collect {
                    self.no_counting = false;
                    if declined.is_some() {
                        self.no_rooted = false;
                    }
                    return Err(e);
                }
            }
            r = self.run_merging(
                axioms,
                None,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
                AttemptProvenance::Ineligible,
            );
            // the run without counting can be rooted and withdraw its answer, and its
            // restart counts no more than it did
            self.restart_declined(
                &mut r,
                &mut declined,
                axioms,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
            );
            self.no_counting = false;
            counting_over = Some(counted);
        }
        let mut rooted_over = None;
        if let Err(Error::Memory { used, .. }) = r
            && self.dl_tried
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
            && self.dl_worked.load(Ordering::Relaxed)
            && self.dl_rooted
            && !self.dl_counted
        {
            // A rooted run without counting passed its bound on derived conclusions: the
            // run starts again without the rooted rules, so that the ontology takes the
            // paths it takes without the option and the axioms outside OWL 2 EL are left
            // out and reported, before it falls back further. Its charges draw first on
            // what the pool admitted.
            self.merge.clear();
            self.canon_want = None;
            self.merge_stop = None;
            self.reset();
            self.dl_worked.store(false, Ordering::Relaxed);
            self.no_rooted = true;
            r = self.run(
                axioms,
                None,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
                AttemptProvenance::Ineligible,
            );
            self.no_rooted = false;
            rooted_over = Some(used);
        }
        // a run of Tier D without counting or the rooted rules that passed its bound on
        // derived conclusions, which falls back to the Horn rules as it does past its
        // memory budget
        let attempt_over = matches!(r, Err(Error::Memory { .. }))
            && self.dl_tried
            && self.dl_attempt
            && self.dl_worked.load(Ordering::Relaxed);
        let mut dl_over = None;
        if let Err(Error::Memory { used, .. }) = r
            && self.dl_tried
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
        {
            // Tier D passed its budget or the caller's: the run starts again with the Horn
            // rules, which leave the disjunctive axioms out. Its charges draw first on
            // what the pool admitted for the run of Tier D.
            self.merge.clear();
            self.canon_want = None;
            self.merge_stop = None;
            self.reset();
            self.skip_dl = true;
            r = self.run(
                axioms,
                None,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
                AttemptProvenance::Ineligible,
            );
            self.skip_dl = false;
            dl_over = Some(used);
            r = r.map(|(consistent, taxonomy, mut stats)| {
                stats.dl_over_budget = true;
                stats.dl_attempt_over = attempt_over;
                stats.peak_memory = stats.peak_memory.max(used);
                (consistent, taxonomy, stats)
            });
        }
        if let Err(Error::Memory { used, .. }) = r
            && self.horn_tried
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
        {
            // The Horn rules passed their budget or the caller's: the run starts again by
            // the EL rules alone, which leave the axioms outside OWL 2 EL out, as the
            // classifier did before the Horn rules and as the ELK mode does. Its charges
            // draw first on what the pool admitted for the Horn run, which the grant
            // still holds.
            self.merge.clear();
            self.canon_want = None;
            self.merge_stop = None;
            self.reset();
            let el = Limits {
                el_only: true,
                ..limits.clone()
            };
            r = self
                .run(
                    axioms,
                    None,
                    held_bytes,
                    &el,
                    &admitted,
                    &mut call_peak,
                    with_taxonomy,
                    AttemptProvenance::Ineligible,
                )
                .map(|(consistent, taxonomy, mut stats)| {
                    stats.horn_over_budget = true;
                    if let Some(d) = dl_over {
                        stats.dl_over_budget = true;
                        stats.dl_attempt_over = attempt_over;
                        stats.peak_memory = stats.peak_memory.max(d);
                    }
                    // the Horn run's high-water mark, which the run reached first
                    stats.peak_memory = stats.peak_memory.max(used);
                    (consistent, taxonomy, stats)
                });
        }
        if r.is_err() && self.dirty {
            self.reset();
        }
        self.merge.clear();
        self.canon_want = None;
        self.active_hub = None;
        self.merge_stop = None;
        self.merge_disabled = false;
        if let Ok((_, _, stats)) = &mut r {
            stats.peak_memory = stats.peak_memory.max(call_peak);
        }
        if let Some(counted) = counting_over
            && let Ok((_, _, stats)) = &mut r
        {
            // the high-water mark of the run that counted, which the retry's may not reach
            stats.peak_memory = stats.peak_memory.max(counted);
            stats.dl_counting_over = true;
            stats.dl_over_budget = true;
        }
        if let Some(used) = rooted_over
            && let Ok((_, _, stats)) = &mut r
        {
            // the high-water mark of the rooted run, which the retry's may not reach
            stats.peak_memory = stats.peak_memory.max(used);
            stats.dl_rooted_over = true;
            stats.dl_over_budget = true;
        }
        if declined.is_some() {
            self.no_rooted = false;
        }
        if let Some(used) = declined
            && let Ok((_, _, stats)) = &mut r
        {
            // the high-water mark of the rooted run, which the retry's may not reach
            stats.peak_memory = stats.peak_memory.max(used);
            stats.dl_rooted_declined = true;
            stats.dl_over_budget = true;
        }
        r
    }

    /// Emits `ADAPTIVE-OUTCOME` from the final `Result<Classification, Error>` returned by
    /// `Classifier::classify` or `Saturation::classify`, after partial construction,
    /// exclusion coverage, and `limits.strict` checks have completed.
    fn emit_adaptive_classify_outcome(&self, res: &Result<Classification, Error>) {
        if self.adaptive_run_id == 0 {
            return;
        }
        match res {
            Ok(out) => {
                let excluded = self.excluded.len();
                let incomplete = out.incomplete.len();
                eprintln!(
                    "ADAPTIVE-OUTCOME run={} attempts={} status=ok consistency={} complete={} complete_for_taken={} dl_rooted_over={} dl_rooted_declined={} dl_counting_over={} dl_over_budget={} horn_over_budget={} outside_horn={} outside_disj={} excluded={} incomplete={}",
                    self.adaptive_run_id,
                    self.adaptive_attempts,
                    consistency_str(out.consistency),
                    out.complete,
                    out.complete_for_taken,
                    out.stats.dl_rooted_over,
                    out.stats.dl_rooted_declined,
                    out.stats.dl_counting_over,
                    out.stats.dl_over_budget,
                    out.stats.horn_over_budget,
                    out.stats.outside_el.horn,
                    out.stats.outside_el.disjunctive,
                    excluded,
                    incomplete,
                );
                #[cfg(test)]
                LAST_ADAPTIVE_OUTCOME.with(|c| {
                    *c.borrow_mut() = Some(AdaptiveOutcome::Ok {
                        run_id: self.adaptive_run_id,
                        attempts: self.adaptive_attempts,
                        consistency: out.consistency,
                        complete: out.complete,
                        complete_for_taken: out.complete_for_taken,
                        dl_rooted_over: out.stats.dl_rooted_over,
                        dl_rooted_declined: out.stats.dl_rooted_declined,
                        dl_counting_over: out.stats.dl_counting_over,
                        dl_over_budget: out.stats.dl_over_budget,
                        horn_over_budget: out.stats.horn_over_budget,
                        outside_horn: out.stats.outside_el.horn,
                        outside_disj: out.stats.outside_el.disjunctive,
                        excluded,
                        incomplete,
                    });
                });
            }
            Err(e) => {
                let kind = error_kind_str(e);
                eprintln!(
                    "ADAPTIVE-OUTCOME run={} attempts={} status=err err={kind}",
                    self.adaptive_run_id, self.adaptive_attempts,
                );
                #[cfg(test)]
                LAST_ADAPTIVE_OUTCOME.with(|c| {
                    *c.borrow_mut() = Some(AdaptiveOutcome::Err {
                        run_id: self.adaptive_run_id,
                        attempts: self.adaptive_attempts,
                        kind,
                    });
                });
            }
        }
    }

    /// Emits `ADAPTIVE-OUTCOME` from the final `Result<Saturated, Error>` returned by
    /// `Classifier::saturate` or `Saturation::saturate`.
    fn emit_adaptive_saturate_outcome(&self, res: &Result<Saturated, Error>) {
        if self.adaptive_run_id == 0 {
            return;
        }
        match res {
            Ok(out) => {
                let consistency = if out.consistent {
                    Consistency::Consistent
                } else {
                    Consistency::Inconsistent
                };
                let excluded = self.excluded.len();
                let complete = excluded == 0;
                eprintln!(
                    "ADAPTIVE-OUTCOME run={} attempts={} status=ok consistency={} complete={} complete_for_taken=true dl_rooted_over={} dl_rooted_declined={} dl_counting_over={} dl_over_budget={} horn_over_budget={} outside_horn={} outside_disj={} excluded={} incomplete=0",
                    self.adaptive_run_id,
                    self.adaptive_attempts,
                    consistency_str(consistency),
                    complete,
                    out.stats.dl_rooted_over,
                    out.stats.dl_rooted_declined,
                    out.stats.dl_counting_over,
                    out.stats.dl_over_budget,
                    out.stats.horn_over_budget,
                    out.stats.outside_el.horn,
                    out.stats.outside_el.disjunctive,
                    excluded,
                );
                #[cfg(test)]
                LAST_ADAPTIVE_OUTCOME.with(|c| {
                    *c.borrow_mut() = Some(AdaptiveOutcome::Ok {
                        run_id: self.adaptive_run_id,
                        attempts: self.adaptive_attempts,
                        consistency,
                        complete,
                        complete_for_taken: true,
                        dl_rooted_over: out.stats.dl_rooted_over,
                        dl_rooted_declined: out.stats.dl_rooted_declined,
                        dl_counting_over: out.stats.dl_counting_over,
                        dl_over_budget: out.stats.dl_over_budget,
                        horn_over_budget: out.stats.horn_over_budget,
                        outside_horn: out.stats.outside_el.horn,
                        outside_disj: out.stats.outside_el.disjunctive,
                        excluded,
                        incomplete: 0,
                    });
                });
            }
            Err(e) => {
                let kind = error_kind_str(e);
                eprintln!(
                    "ADAPTIVE-OUTCOME run={} attempts={} status=err err={kind}",
                    self.adaptive_run_id, self.adaptive_attempts,
                );
                #[cfg(test)]
                LAST_ADAPTIVE_OUTCOME.with(|c| {
                    *c.borrow_mut() = Some(AdaptiveOutcome::Err {
                        run_id: self.adaptive_run_id,
                        attempts: self.adaptive_attempts,
                        kind,
                    });
                });
            }
        }
    }

    /// Captures the joined saturation's counters, resident clause-heap oracle, and winning
    /// stop reason immediately after `saturate_counted`.
    fn record_adaptive_saturation(
        &self,
        d: &dl_rules::DlRun,
        dynamic: Option<&saturate::Dynamic>,
        stop: Option<Stop>,
    ) {
        let winning = WinningStop::from_stop(stop);
        let final_work = d.work();
        let root_work = d.root_work();
        let raw_queue = d.raw_queue();
        let raw_pushed = d.raw_pushed();
        let mrepair_work = d.mrepair_work();
        let stop_guard = d.stop_guard();
        let stop_requested_at = d.stop_requested_at();
        let hyp_guard = d.hypothetical_guard();
        let hyp_at = d.hypothetical_at();
        let check_oracle = cfg!(any(debug_assertions, test)) || d.is_adaptive_diag();
        let oracle_queue = if check_oracle {
            let actual = count_actual_queue_len(&self.ctx, dynamic);
            assert_eq!(
                raw_queue, actual,
                "raw_queue ({raw_queue}) != sum(store.queue_len()) ({actual})"
            );
            assert!(
                raw_queue <= raw_pushed,
                "raw_queue ({raw_queue}) exceeded raw_pushed ({raw_pushed})"
            );
            actual
        } else {
            raw_queue
        };
        if d.is_adaptive_diag()
            && let Some(meta) = d.adaptive_meta()
        {
            let overshoot = if stop_guard != dl_rules::WorkStopGuard::None {
                final_work.saturating_sub(stop_requested_at)
            } else {
                0
            };
            eprintln!(
                "ADAPTIVE-SAT run={} attempt={} mode={} policy={} cap={} after_counting={} mrepair={} threads={} event={} stop={} guard={} req_at={} overshoot={} hyp_guard={} hyp_at={} work={} root_work={} raw_q={} raw_p={} oracle_q={} mrepair_work={} samples={}",
                meta.run_id,
                meta.attempt,
                if d.is_adaptive_enforced() {
                    "enforce"
                } else {
                    "observe"
                },
                meta.policy_name,
                meta.effective_cap,
                usize::from(meta.after_counting),
                meta.mrepair,
                meta.threads,
                winning.event_name(),
                winning.as_str(),
                stop_guard.as_str(),
                stop_requested_at,
                overshoot,
                hyp_guard.as_str(),
                hyp_at,
                final_work,
                root_work,
                raw_queue,
                raw_pushed,
                oracle_queue,
                mrepair_work,
                d.adaptive_samples(),
            );
        }
        #[cfg(test)]
        if let Some(meta) = d.adaptive_meta() {
            LAST_ADAPTIVE_ATTEMPTS.with(|c| {
                c.borrow_mut().push(AdaptiveSnapshot {
                    attempt: meta.attempt,
                    after_counting: meta.after_counting,
                    enforce: d.is_adaptive_enforced(),
                    policy_name: meta.policy_name,
                    effective_cap: meta.effective_cap,
                    winning_stop: winning,
                    stop_guard,
                    stop_requested_at,
                    hypothetical_guard: hyp_guard,
                    hypothetical_at: hyp_at,
                    final_work,
                    root_work,
                    raw_queue,
                    raw_pushed,
                    mrepair_work,
                    samples: d.adaptive_samples(),
                    oracle_queue,
                });
            });
        }
        #[cfg(not(test))]
        let _ = oracle_queue;
    }

    /// After a rooted run that withdrew its answer (item 1231), which `rooted_declined`
    /// records, starts the run again without the rooted rules, as past the rooted bound, so
    /// that the ontology takes the paths it takes without the option and the axioms outside
    /// OWL 2 EL are left out and reported. `no_rooted` stays set for the fallbacks that
    /// follow, and `run_with` clears it. `declined` keeps the high-water mark of the first
    /// rooted run that withdrew. The charges of the new run draw first on what the pool
    /// admitted.
    #[allow(clippy::too_many_arguments)]
    fn restart_declined(
        &mut self,
        r: &mut Result<(bool, Option<Taxonomy>, Stats), Error>,
        declined: &mut Option<usize>,
        axioms: &[&AxiomKind],
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
        with_taxonomy: bool,
    ) {
        if !std::mem::take(&mut self.rooted_declined) {
            return;
        }
        debug_assert!(matches!(r, Err(Error::Memory { .. })));
        let used = match r {
            Err(Error::Memory { used, .. }) => *used,
            _ => 0,
        };
        self.merge.clear();
        self.canon_want = None;
        self.merge_stop = None;
        self.reset();
        self.dl_worked.store(false, Ordering::Relaxed);
        self.no_rooted = true;
        *r = self.run(
            axioms,
            None,
            held_bytes,
            limits,
            admitted,
            call_peak,
            with_taxonomy,
            AttemptProvenance::Ineligible,
        );
        declined.get_or_insert(used);
    }

    /// Drops the state of a run that stopped part way, since the next run starts again.
    fn reset(&mut self) {
        self.index = None;
        self.interner = Interner::new();
        self.worlds = FxHashMap::default();
        self.partial = None;
        self.stale_tier = false;
        self.ctx = Vec::new();
        self.disjoint = Vec::new();
        self.dynamic = None;
        self.dl_run = None;
        self.last_roots = Vec::new();
    }

    /// Normalizes the axioms inside OWL 2 EL and builds the index. `outside` names the
    /// axioms the profile check leaves out.
    /// The answer's last part is the estimated peak of a parallel normalization, with
    /// `held_bytes`, which the pool has been charged for, or 0. `kept_bytes` are those of
    /// the contexts the run keeps, [`Saturation::context_bytes`].
    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        axioms: &[&AxiomKind],
        outside: &[(usize, (String, String))],
        threads: usize,
        held_bytes: usize,
        kept_bytes: usize,
        limits: &Limits,
        attempt: AttemptProvenance,
    ) -> Result<(Axioms, Index, usize, OutsideEl, usize), Error> {
        let budget = numeric::Budget::new(
            limits,
            held_bytes
                .saturating_add(kept_bytes)
                .saturating_add(self.merge_extra_bytes()),
            self.interner.ranges.ledger.clone(),
        );
        budget.baseline(self.interner.bytes())?;
        self.interner.ranges.begin(budget.clone());
        let result = self.build_inner(axioms, outside, threads, held_bytes, limits, attempt);
        // the run's components and definitions end with its normalization, on every exit
        self.interner.dt_run = None;
        self.numeric_halt |= self.interner.ranges.check().is_err();
        self.interner.ranges.end();
        result.map(|(ax, ix, peak, outside, visible_named_classes)| {
            (
                ax,
                ix,
                peak.max(budget.peak()),
                outside,
                visible_named_classes,
            )
        })
    }

    fn build_inner(
        &mut self,
        axioms: &[&AxiomKind],
        outside: &[(usize, (String, String))],
        threads: usize,
        held_bytes: usize,
        limits: &Limits,
        attempt: AttemptProvenance,
    ) -> Result<(Axioms, Index, usize, OutsideEl, usize), Error> {
        let mut ax = Axioms::default();
        let mut partial: BTreeMap<&AxiomKind, &'static str> = BTreeMap::new();
        let mut chains: Vec<&AxiomKind> = Vec::new();
        let mut chain_ranges: Vec<(&AxiomKind, String)> = Vec::new();
        let mut is_outside = vec![false; axioms.len()];
        for &(i, _) in outside {
            is_outside[i] = true;
        }
        // the classes the normalization meets are those of the signature
        self.interner.clear_met();
        // the fresh classes of this run's axioms are those of the last run over this
        // interner
        self.interner.begin_run();
        // the datatype definitions, which the data ranges that name them stand for
        self.interner.ranges.definitions.clear();
        for (i, &k) in axioms.iter().enumerate() {
            if let AxiomKind::DatatypeDefinition(d, r) = k
                && !is_outside[i]
            {
                self.interner
                    .ranges
                    .definitions
                    .insert(d.clone(), r.clone());
            }
        }
        // The components of the data properties, and which of them the datatype checker
        // takes, are chosen once from every axiom, inside the profile or outside it
        // (F24, "The EL and Horn tiers"). The checker reads every non-reserved datatype
        // definition, since a definition with a facet is outside the profile and still
        // names the range its uses stand for. EL-only runs never activate the checker,
        // and a zero-allocation pre-check skips component analysis on data-free and
        // plain built-in EL data ontologies (while still letting `rdf:langString` and
        // defined datatypes through).
        self.interner.dt_run = None;
        if !limits.el_only {
            let may = match dtcheck::comp::Comps::may_trigger(axioms, &|| poll(limits).is_err()) {
                Ok(v) => v,
                Err(dtcheck::comp::CompFail::Stopped) => {
                    poll(limits)?;
                    return Err(Error::Cancelled);
                }
                Err(dtcheck::comp::CompFail::Memory) => {
                    unreachable!("may_trigger allocates nothing")
                }
            };
            if may {
                let budget = self
                    .interner
                    .ranges
                    .budget
                    .clone()
                    .expect("build installs a numeric budget");
                let (comps, dt_defs, guard) = dt_comps(axioms, axioms, &budget, limits)?;
                if comps.any() {
                    let retained = comps.bytes().saturating_add(dt_defs.bytes());
                    self.interner.dt_run = Some(Box::new(dtcheck::atoms::DtRun {
                        comps,
                        defs: dt_defs,
                        stop: dtcheck::atoms::Stop {
                            deadline: limits.deadline,
                            cancelled: limits.cancelled.clone(),
                        },
                        budget: Some(budget.clone()),
                        stopped: None,
                    }));
                    guard.commit_retained(retained)?;
                }
            }
        }
        let dt_mark = self.interner.nodes.len();
        // Chunks of the axioms normalize on the run's threads into interners of their
        // own, which merge in order into the run's, so that every id is the one a
        // normalization on one thread gives. Data ranges and literals have ids inside
        // their values, which the merge does not map, so a run that has them normalizes
        // on one thread. So does a run whose chunks pass the memory budget or the pool's
        // grant, since each chunk holds the names it meets and the merge copies them.
        let mut merged = false;
        let mut peak = 0;
        // A chunk's interner has no checker components, so a run with them normalizes on
        // one thread, where each axiom over a checker property gets its atoms or report.
        if threads > 1 && axioms.len() >= PARALLEL_AXIOMS && self.interner.dt_run.is_none() {
            let gauge = Gauge {
                limits,
                numeric: self.interner.ranges.budget.clone(),
                base: held_bytes + self.interner.bytes() + self.merge_extra_bytes(),
                held: AtomicUsize::new(0),
                peak: parking_lot::Mutex::new(0),
                over: std::sync::atomic::AtomicBool::new(false),
            };
            let numeric = self.interner.ranges.budget.clone();
            let definitions = &self.interner.ranges.definitions;
            let canon = self.interner.canon.clone();
            let is_outside = &is_outside;
            let parts = par_map(axioms, threads, MIN_AXIOMS, |at, chunk| {
                let mut it = Interner::chunk(definitions);
                it.canon = canon.clone();
                if let Some(b) = &numeric {
                    it.ranges.begin(b.clone());
                }
                let mut out = Normalized::default();
                let r = normalize_axioms(
                    chunk,
                    at,
                    is_outside,
                    &mut it,
                    &mut out,
                    limits,
                    Some(&gauge),
                );
                it.ranges.end();
                (r, it, out)
            });
            for (r, _, _) in &parts {
                match r {
                    Err(Error::Memory { .. }) => {
                        // Numeric admission can refuse before the ordinary chunk
                        // gauge runs or before the chunk has a data value to flag.
                        gauge.over.store(true, Ordering::Relaxed);
                    }
                    r => r.clone()?,
                }
            }
            if !gauge.over.load(Ordering::Relaxed)
                && !parts.iter().any(|(_, it, _)| it.chunk_has_data())
            {
                // The chunks counted what their merge can add. The merge polls, and
                // what it adds past that counts against the budget and the pool.
                let start = self.interner.bytes();
                let reserved: usize = parts.iter().map(|(_, it, _)| it.merge_bytes()).sum();
                let mut told = 0usize;
                let mut check = |bytes: usize| -> Result<(), Error> {
                    poll(limits)?;
                    let extra = bytes.saturating_sub(start).saturating_sub(reserved);
                    if extra > told {
                        let grown = extra - told;
                        told = extra;
                        gauge.tell(grown)?;
                    }
                    Ok(())
                };
                let (its, outs): (Vec<Interner>, Vec<Normalized>) =
                    parts.into_iter().map(|(_, it, out)| (it, out)).unzip();
                let maps = self.interner.merge(its, &mut check)?;
                for ((nodes, roles), out) in maps.into_iter().zip(outs) {
                    ax.append_mapped(out.ax, &nodes, &roles);
                    partial.extend(out.partial);
                    chains.extend(out.chains);
                }
                merged = true;
            }
            peak = *gauge.peak.lock();
        }
        if !merged {
            let mut out = Normalized {
                ax,
                ..Normalized::default()
            };
            normalize_axioms(
                axioms,
                0,
                &is_outside,
                &mut self.interner,
                &mut out,
                limits,
                None,
            )?;
            ax = out.ax;
            partial.extend(out.partial);
            chains.extend(out.chains);
        }
        self.interner.check_dt_stop(limits)?;
        poll(limits)?;
        // the range restriction of OWL 2 EL (Profiles §2.2.6): a chain's superproperty
        // has no range that its last property lacks
        let mut named_domain_candidate = false;
        let mut chain_ranges_bytes = 0usize;
        if !chains.is_empty() {
            let budget = self
                .interner
                .ranges
                .budget
                .clone()
                .expect("build installs a numeric budget");
            let chains_bytes = vec_bytes(&chains);
            let mut ax_bytes = ax.bytes();
            let base = self
                .interner
                .bytes()
                .checked_add(ax_bytes)
                .and_then(|n| n.checked_add(chains_bytes))
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
            budget.refresh_base(base)?;
            #[cfg(test)]
            {
                let (b, s) = budget.retained_storage();
                admission_test::hit(admission_test::Point::BuildRangeScanStart {
                    base: b,
                    scratch: s,
                });
            }
            let mut scratch = BuildScratch {
                guard: budget.scratch_guard(),
                limits,
            };
            let missing =
                check_chain_ranges(axioms, |i, _| is_outside[i], &chains, true, &mut scratch)?;
            #[cfg(test)]
            {
                let (_, s) = budget.retained_storage();
                admission_test::hit(admission_test::Point::BuildMissingConsumeStart {
                    peak: budget.peak(),
                    scratch: s,
                });
            }
            let mut chain_ranges_msg_bytes = 0usize;
            for (i, (&k, miss)) in chains.iter().zip(missing.iter()).enumerate() {
                if i.is_multiple_of(RANGE_POLL_STEP) {
                    poll(limits)?;
                }
                if let Some((missing_ce, sup, last)) = miss {
                    index::intern_signature(k, &mut self.interner);
                    let mut counter = CountingWriter {
                        len: 0,
                        steps: 0,
                        limits,
                    };
                    render_missing_range_polled(missing_ce, sup, last, &mut counter)?;
                    let msg_len = counter.len;
                    let old_cr_cap = chain_ranges.capacity();
                    let next_cr_cap = if chain_ranges.len() == old_cr_cap {
                        old_cr_cap
                            .checked_mul(2)
                            .ok_or(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used: usize::MAX,
                            })?
                            .max(8)
                    } else {
                        old_cr_cap
                    };
                    let entry_size = std::mem::size_of::<(&AxiomKind, String)>();
                    let old_cr_vec_bytes =
                        old_cr_cap.checked_mul(entry_size).ok_or(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        })?;
                    let next_cr_vec_bytes =
                        next_cr_cap.checked_mul(entry_size).ok_or(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        })?;
                    let cr_vec_peak_bytes = if next_cr_cap > old_cr_cap {
                        old_cr_vec_bytes
                            .checked_add(next_cr_vec_bytes)
                            .ok_or(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used: usize::MAX,
                            })?
                    } else {
                        old_cr_vec_bytes
                    };
                    let next_msg_bytes =
                        chain_ranges_msg_bytes
                            .checked_add(msg_len)
                            .ok_or(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used: usize::MAX,
                            })?;
                    let pre_live = self
                        .interner
                        .bytes()
                        .checked_add(ax_bytes)
                        .and_then(|n| n.checked_add(chains_bytes))
                        .and_then(|n| n.checked_add(cr_vec_peak_bytes))
                        .and_then(|n| n.checked_add(next_msg_bytes))
                        .ok_or(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        })?;
                    budget.refresh_base(pre_live)?;
                    poll(limits)?;
                    if next_cr_cap > old_cr_cap {
                        chain_ranges.reserve_exact(next_cr_cap - chain_ranges.len());
                    }
                    let mut msg = String::with_capacity(msg_len);
                    #[cfg(test)]
                    admission_test::hit(admission_test::Point::BuildDiagnosticAllocated);
                    let mut writer = PolledStringWriter {
                        out: &mut msg,
                        steps: 0,
                        limits,
                    };
                    render_missing_range_polled(missing_ce, sup, last, &mut writer)?;
                    debug_assert_eq!(msg.len(), msg_len);
                    chain_ranges_msg_bytes = chain_ranges_msg_bytes.saturating_add(msg.capacity());
                    chain_ranges.push((k, msg));
                } else {
                    let old_role_subs_cap = ax.role_subs.capacity();
                    let old_chains_cap = ax.chains.capacity();
                    let old_chains_len = ax.chains.len();
                    if let Some(why) = index::normalize(k, &mut self.interner, &mut ax) {
                        if matches!(attempt, AttemptProvenance::OriginalDefault)
                            && !named_domain_candidate
                        {
                            named_domain_candidate = top_chain::shape(k, why).is_some();
                        }
                        partial.insert(k, why);
                    }
                    self.interner.ranges.check()?;
                    let new_role_subs_cap = ax.role_subs.capacity();
                    if new_role_subs_cap > old_role_subs_cap {
                        ax_bytes =
                            ax_bytes.saturating_add((new_role_subs_cap - old_role_subs_cap) * 8);
                    }
                    let new_chains_cap = ax.chains.capacity();
                    if new_chains_cap > old_chains_cap {
                        ax_bytes = ax_bytes.saturating_add(
                            (new_chains_cap - old_chains_cap)
                                * std::mem::size_of::<(Vec<index::Role>, index::Role)>(),
                        );
                    }
                    for (c, _) in &ax.chains[old_chains_len..] {
                        ax_bytes = ax_bytes
                            .saturating_add(c.capacity() * std::mem::size_of::<index::Role>());
                    }
                }
                chain_ranges_bytes =
                    vec_bytes(&chain_ranges).saturating_add(chain_ranges_msg_bytes);
                let live = self
                    .interner
                    .bytes()
                    .checked_add(ax_bytes)
                    .and_then(|n| n.checked_add(chains_bytes))
                    .and_then(|n| n.checked_add(chain_ranges_bytes))
                    .ok_or(Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: usize::MAX,
                    })?;
                budget.refresh_base(live)?;
            }
            let final_ax_bytes = ax.bytes();
            debug_assert_eq!(ax_bytes, final_ax_bytes);
            let live = self
                .interner
                .bytes()
                .checked_add(final_ax_bytes)
                .and_then(|n| n.checked_add(chains_bytes))
                .and_then(|n| n.checked_add(chain_ranges_bytes))
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
            budget.refresh_base(live)?;
            #[cfg(test)]
            {
                admission_test::hit(admission_test::Point::BuildMissingConsumeEnd {
                    peak: budget.peak(),
                });
                let (b, _) = budget.retained_storage();
                admission_test::hit(admission_test::Point::BuildBeforeChainsDrop { base: b });
            }
            drop(missing);
            drop(chains);
            drop(scratch);
            budget.refresh_base(live - chains_bytes)?;
            #[cfg(test)]
            {
                let (b, s) = budget.retained_storage();
                admission_test::hit(admission_test::Point::BuildAfterChainsDrop {
                    base: b,
                    scratch: s,
                });
            }
        }
        // what each axiom outside OWL 2 EL needs, counted once however often it occurs; a
        // chain whose superproperty has a range its last property lacks is Horn
        let mut candidates: Vec<&AxiomKind> = outside.iter().map(|(i, _)| axioms[*i]).collect();
        candidates.extend(chain_ranges.iter().map(|(k, _)| *k));
        // Capture the checker decision before Tier D can clear dt_run. The candidate
        // list includes chains rejected by the central last-role range check.
        let plan_named_domains =
            named_domain_candidate && candidates.is_empty() && self.interner.dt_run.is_none();
        candidates.sort();
        candidates.dedup();
        let mut outside_el = OutsideEl::default();
        for k in &candidates {
            if chain_ranges.iter().any(|(c, _)| c == k) {
                outside_el.horn += 1;
            } else {
                outside_el.add(dl::shape(k));
            }
        }
        // what the interner holds before the normalizations outside OWL 2 EL, which
        // intern into it whether or not they take their axioms
        let mark = TierMark::of(&self.interner);
        // The Horn rules take what they can of these (items 603 and 692).
        let mut taken: FxHashSet<&AxiomKind> = FxHashSet::default();
        let stop = || poll(limits).is_err();
        let halt = |h: index::Halt| match h {
            index::Halt::Memory(used) => Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            },
            index::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
        };
        // Tier D takes them instead when one is disjunctive and the EL part has nothing
        // that its rules leave out
        let mut dl_taken = false;
        // A run whose individuals, named or anonymous, are only in the axioms outside
        // OWL 2 EL takes Tier D through `disj_allowed`, and the normalization makes it
        // rooted. Keys that item 805's rewriting declined would then mark the contexts
        // real in rules that do not take those marks, so such a run normalizes as a run
        // without the option does and leaves the axioms with individuals out (roadmap
        // items 1265 and 1281).
        let rooted = limits.rooted && !self.no_rooted && self.rooted_allowed(&ax);
        if !candidates.is_empty()
            && !limits.el_only
            && !self.skip_dl
            && (self.disj_allowed(&ax) || rooted)
            && candidates.iter().any(|k| !dl::shape(k).horn)
            && dl_norm::eligible(&self.interner, &ax, rooted)
        {
            self.dl_tried = true;
            if let Some((flags, d)) = dl_norm::normalize(
                &candidates,
                &mut self.interner,
                &mut ax,
                limits.max_memory,
                &stop,
                dl_norm::Allowed {
                    counting: limits.counting && !self.no_counting,
                    rooted,
                    order: limits.number_order,
                    automata: limits.dl_automata_level(),
                },
            )
            .map_err(halt)?
            {
                taken.extend(
                    candidates
                        .iter()
                        .zip(flags)
                        .filter(|(_, t)| *t)
                        .map(|(k, _)| *k),
                );
                ax.dl = Some(Box::new(d));
                dl_taken = true;
                // Tier D leaves out data axioms and builds no checker atoms; drop dt_run
                // so Index::build keeps ix.dt = None and preserves incremental Disj reuse.
                self.interner.dt_run = None;
            }
        }
        if !dl_taken && !candidates.is_empty() && !limits.el_only {
            self.horn_tried = true;
            let flags = match horn::normalize(
                &candidates,
                &mut self.interner,
                &mut ax,
                limits.max_memory,
                &stop,
            ) {
                Ok(flags) => flags,
                Err(index::Halt::Stopped) => {
                    self.interner.check_dt_stop(limits)?;
                    return Err(poll(limits).err().unwrap_or(Error::Cancelled));
                }
                Err(h) => return Err(halt(h)),
            };
            self.interner.ranges.check()?;
            taken.extend(
                candidates
                    .iter()
                    .zip(flags)
                    .filter(|(_, t)| *t)
                    .map(|(k, _)| *k),
            );
        }
        self.interner.ranges.check()?;
        // a datatype conversion that the run's deadline, cancellation or memory budget
        // stopped stops the run, as the numeric construction's halt does, and is not an
        // axiom used in part
        self.interner.check_dt_stop(limits)?;
        // A fresh run decides its tier before these normalizations, so what they intern
        // that the decision reads would keep a later run over this interner from Tier D.
        // A run whose rules take it starts again anyway, as `entangled` says.
        self.stale_tier |= mark.passed(&self.interner);
        if self.hide_internal {
            self.interner.hide_prefix(dl_reduce::PREFIX);
        }
        // Count distinct visible named classes once from `met_classes` (excluding
        // owl:Thing, owl:Nothing, individuals/nominals, internal reduction classes, and
        // any double-counting from explicit `Declaration(Class(...))` axioms) without a
        // second scan over `met_classes`.
        let classes_before = ax.classes.len();
        ax.classes.extend(self.interner.met_classes());
        let visible_named_classes = ax.classes.len() - classes_before;
        if let Some(b) = &self.interner.ranges.budget {
            let live = self
                .interner
                .bytes()
                .checked_add(ax.bytes())
                .and_then(|n| n.checked_add(chain_ranges_bytes))
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
            b.baseline(live)?;
        }
        if cfg!(debug_assertions)
            && let Some(run) = &self.interner.dt_run
        {
            let checker = |r: index::Role| match &self.interner.roles[r as usize] {
                index::RoleName::Data(d) => run.comps.checker(d),
                _ => false,
            };
            for n in &self.interner.nodes[dt_mark..] {
                if let Node::DataSome(r, _) = n {
                    debug_assert!(
                        !checker(*r),
                        "a lattice data restriction over a checker property"
                    );
                }
            }
            for &(r, _) in &ax.data_ranges {
                debug_assert!(!checker(r), "a lattice data range over a checker property");
            }
        }
        let mut ix =
            Index::build(&ax, &mut self.interner, limits.max_memory, &stop).map_err(halt)?;
        if plan_named_domains && ax.horn.is_none() && ax.dl.is_none() {
            let budget = self.interner.ranges.budget.as_ref().expect("build budget");
            let base = self
                .interner
                .bytes()
                .checked_add(ax.bytes())
                .and_then(|n| n.checked_add(ix.bytes()))
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
            ix.top_chain = top_chain::plan(
                axioms,
                &partial,
                &self.interner,
                &ix,
                budget,
                base,
                #[cfg(test)]
                self.top_chain_probe.as_ref(),
            )?;
        }
        self.interner.dt_run = None;
        self.interner.end_run();
        let mut excluded: Vec<Excluded> = Vec::new();
        for (i, (rule, message)) in outside {
            if taken.contains(axioms[*i]) {
                continue;
            }
            excluded.push(Excluded {
                axiom: axioms[*i].clone(),
                gap: Gap::Outside {
                    rule: rule.clone(),
                    message: message.clone(),
                },
            });
        }
        for (k, message) in chain_ranges {
            if taken.contains(k) {
                continue;
            }
            excluded.push(Excluded {
                axiom: k.clone(),
                gap: Gap::Outside {
                    rule: CHAIN_RANGE_RULE.to_string(),
                    message,
                },
            });
        }
        for (k, why) in partial {
            excluded.push(Excluded {
                axiom: k.clone(),
                gap: Gap::Partial(why),
            });
        }
        excluded.sort();
        excluded.dedup();
        self.excluded = excluded;
        outside_el.taken = taken.len();
        Ok((ax, ix, peak, outside_el, visible_named_classes))
    }

    /// Whether a run of Tier D may be rooted: when the EL part has no keys, top
    /// properties or functional data roles. A key over object properties and the
    /// universal object property reach this check only when item 805's rewriting in
    /// [`Saturation::run`] declined them. The check holds for the individuals of the EL
    /// part and for those that only the axioms outside it hold, anonymous ones among them
    /// (roadmap items 1265 and 1281).
    fn rooted_allowed(&self, ax: &Axioms) -> bool {
        let it = &self.interner;
        ax.functional.is_empty()
            && ax.keys.is_empty()
            && it
                .role_id(&index::RoleName::Object(ObjectProperty::top()))
                .is_none()
            && it
                .role_id(&index::RoleName::Data(DataProperty::top()))
                .is_none()
    }

    /// Whether the disjunctive rules may take axioms outside OWL 2 EL without the rooted
    /// run: when the EL part has no individuals, top properties or functional data roles,
    /// whose rules they do not share. The Horn rules take all three (roadmap item 692).
    fn disj_allowed(&self, ax: &Axioms) -> bool {
        let it = &self.interner;
        it.individuals.is_empty()
            && ax.functional.is_empty()
            && it
                .role_id(&index::RoleName::Object(ObjectProperty::top()))
                .is_none()
            && it
                .role_id(&index::RoleName::Data(DataProperty::top()))
                .is_none()
    }

    /// One run of the fallback chain of [`Saturation::run_with`]. A run with changes
    /// that finds, once its index is built, that it cannot keep the contexts of the
    /// last run starts again without them and with a new interner.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
        with_taxonomy: bool,
        attempt: AttemptProvenance,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        let want_canon = if limits.rooted
            && limits.rooted_merge
            && !self.merge_disabled
            && !self.no_rooted
            && !limits.el_only
            && !self.skip_dl
        {
            self.merge.shared.clone()
        } else {
            None
        };
        if !same_canon(&self.interner.canon, &want_canon) {
            self.reset();
        }
        self.canon_want = want_canon;
        let reduce_mem = limits.max_memory.map(|m| {
            m.saturating_sub(
                self.merge
                    .uf_bytes()
                    .saturating_add(self.canon_live_bytes()),
            )
        });
        if limits.rooted && !self.no_rooted && !limits.el_only && !self.skip_dl {
            let plan_base = self.pre_plan_live_bytes(held_bytes);
            let mut paid = admitted.load(Ordering::Relaxed);
            let charge_fn = limits
                .charge
                .as_deref()
                .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
            let mut plan_admit = |used: usize| -> bool {
                let ok = cover_memory(used, limits.max_memory, charge_fn, &mut paid);
                if ok {
                    *call_peak = (*call_peak).max(used);
                    admitted.store(paid, Ordering::Relaxed);
                }
                ok
            };
            #[cfg(test)]
            let _plan_guard = admission_test::enter_plan(self.merge.restarts);
            let planned = match dl_reduce::plan_budgeted(
                axioms,
                reduce_mem,
                self.canon_want.as_deref(),
                plan_base,
                &mut || poll(limits),
                &mut plan_admit,
            ) {
                Ok(p) => {
                    #[cfg(test)]
                    admission_test::emit_merge_phase(admission_test::MergePhase::PlanDone {
                        restarts: self.merge.restarts,
                        outcome: match &p {
                            dl_reduce::Plan::NotNeeded => admission_test::PlanKind::NotNeeded,
                            dl_reduce::Plan::Declined => admission_test::PlanKind::Declined,
                            dl_reduce::Plan::Ready(_) => admission_test::PlanKind::Ready,
                        },
                    });
                    p
                }
                Err(e) => {
                    #[cfg(test)]
                    admission_test::emit_merge_phase(admission_test::MergePhase::PlanDone {
                        restarts: self.merge.restarts,
                        outcome: admission_test::PlanKind::PollErr,
                    });
                    self.dirty = true;
                    return Err(e);
                }
            };
            #[cfg(test)]
            drop(_plan_guard);
            let mut declined = false;
            match planned {
                dl_reduce::Plan::NotNeeded => {}
                dl_reduce::Plan::Declined => {
                    declined = true;
                }
                dl_reduce::Plan::Ready(mut red) => {
                    let all_bytes = red
                        .all_len(axioms)
                        .saturating_mul(std::mem::size_of::<&AxiomKind>());
                    let red_live = plan_base
                        .saturating_add(red.retained_bytes())
                        .saturating_add(all_bytes);
                    if !plan_admit(red_live) {
                        drop(red);
                        declined = true;
                    } else {
                        // the interner of an earlier run holds names and individuals that
                        // decide the tiers, so the rewritten axioms start from nothing
                        self.reset();
                        self.hide_internal = true;
                        self.active_hub = red.hub.take();
                        // Keep an eligible lower-tier result when item 805's rewriting
                        // removes all disjunctions and every generated axiom is taken.
                        let all = red.all(axioms);
                        let red_bytes = red.retained_bytes().saturating_add(
                            all.capacity()
                                .saturating_mul(std::mem::size_of::<&AxiomKind>()),
                        );
                        let step_limits = Limits {
                            charge: Self::charge_drawing_on(limits, admitted),
                            ..limits.clone()
                        };
                        let r = self.run_once(
                            &all,
                            None,
                            held_bytes.saturating_add(red_bytes),
                            &step_limits,
                            with_taxonomy,
                            AttemptProvenance::Ineligible,
                        );
                        self.active_hub = None;
                        self.hide_internal = false;
                        match r {
                            Ok(ControlFlow::Continue(mut r))
                                if self.dl_tried
                                    || (r.2.tier != Tier::Disjunctive
                                        && !red.excludes_rewritten(&self.excluded)) =>
                            {
                                red.restore(axioms, &mut self.excluded);
                                self.reduced = true;
                                drop(all);
                                drop(red);
                                #[cfg(test)]
                                let with_rewrite = r.2.memory;
                                r.2.memory = r.2.memory.saturating_sub(red_bytes);
                                r.2.added = self.source_len.unwrap_or(axioms.len());
                                let charge = step_limits
                                    .charge
                                    .as_deref()
                                    .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
                                let mut paid = self.last_paid;
                                let mut scratch = PostRunScratch {
                                    base: r.2.memory,
                                    peak: &mut r.2.peak_memory,
                                    paid: &mut paid,
                                    limits: &step_limits,
                                    charge,
                                    check_stop: self.partial.is_none(),
                                    scratch: 0,
                                };
                                #[cfg(test)]
                                admission_test::hit(admission_test::Point::PostRunStart {
                                    base: scratch.base,
                                    with_rewrite,
                                });
                                match dl_reduce::Reduction::outside_el(
                                    axioms,
                                    &self.excluded,
                                    &mut scratch,
                                ) {
                                    Ok(outside_el) => {
                                        self.last_paid = paid;
                                        r.2.outside_el = outside_el;
                                        r.2.tier = outside_el.tier();
                                        return Ok(r);
                                    }
                                    Err(e) => {
                                        self.partial = None;
                                        if matches!(e, Error::Memory { .. })
                                            && limits.partial
                                            && self.dl_tried
                                            && with_taxonomy
                                        {
                                            self.partial_over = true;
                                            self.dirty = true;
                                        }
                                        return Err(e);
                                    }
                                }
                            }
                            Err(e) => {
                                // the report of a run that stopped still names the original
                                // axioms
                                red.restore(axioms, &mut self.excluded);
                                self.reduced = true;
                                return Err(e);
                            }
                            _ => {
                                red.restore(axioms, &mut self.excluded);
                                if self.merge.restarts > 0
                                    || self.merge.active_hub.is_some()
                                    || self.canon_want.is_some()
                                {
                                    self.merge.clear();
                                    self.canon_want = None;
                                    self.active_hub = None;
                                    self.merge_disabled = true;
                                }
                                self.reset();
                            }
                        }
                    }
                }
            }
            if declined
                && (self.merge.restarts > 0
                    || self.merge.active_hub.is_some()
                    || self.canon_want.is_some())
            {
                // Reduction declined after a merge (or with an active partition): the
                // original axioms still contain the unreduced key/universal property and
                // are not rooted-eligible, so discard derived aliases and any generated
                // hub before Horn/EL fallback runs on the original axioms.
                self.merge.clear();
                self.canon_want = None;
                self.active_hub = None;
                self.merge_disabled = true;
                self.reset();
            }
        }
        // after a run of rewritten axioms, whose made-up names and hub individual the
        // interner still holds, the run starts from nothing
        let was = std::mem::take(&mut self.reduced);
        if was {
            self.reset();
        }
        let changes = changes.filter(|_| !was);
        let step_limits = Limits {
            charge: Self::charge_drawing_on(limits, admitted),
            ..limits.clone()
        };
        let restart = match self.run_once(
            axioms,
            changes,
            held_bytes,
            &step_limits,
            with_taxonomy,
            attempt,
        )? {
            ControlFlow::Continue(r) => return Ok(r),
            ControlFlow::Break(restart) => restart,
        };
        self.reset();
        let discarded_peak = match restart {
            LateRestart::Incompatible { normalized_peak }
            | LateRestart::NamedDomain { normalized_peak } => normalized_peak,
        };
        let retry_limits = Limits {
            charge: Self::charge_drawing_on(limits, admitted),
            ..limits.clone()
        };
        let ControlFlow::Continue(mut r) = self.run_once(
            axioms,
            None,
            held_bytes,
            &retry_limits,
            with_taxonomy,
            attempt,
        )?
        else {
            unreachable!("a run without changes starts fresh");
        };
        r.2.peak_memory = r.2.peak_memory.max(discarded_peak);
        Ok(r)
    }

    /// [`Saturation::run`], with a reason when a run with changes must start again.
    fn run_once(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        with_taxonomy: bool,
        attempt: AttemptProvenance,
    ) -> Result<RunOutcome, Error> {
        let mut stats = Stats::default();
        let start = Instant::now();
        let threads = threads_of(limits);
        stats.threads = threads;
        match changes {
            Some(c) => {
                stats.added = c.added.len();
                stats.removed = c.removed.len();
            }
            None => stats.added = self.source_len.unwrap_or(axioms.len()),
        }
        let again = self.dirty || self.index.is_none();
        // until the saturation ends, a stop leaves the state for a run that starts again
        self.dirty = true;
        self.query_cache = std::sync::OnceLock::new();
        self.horn_tried = false;
        self.dl_tried = false;
        self.partial_over = false;
        self.numeric_halt = false;
        self.last_paid = 0;
        // the profile check of each axiom, which depends on it alone
        let outside: Vec<(usize, (String, String))> =
            par_map(axioms, threads, MIN_AXIOMS, |at, chunk| {
                let mut out = Vec::new();
                for (i, k) in chunk.iter().enumerate() {
                    if logical(k)
                        && let Some(b) = el_break(k)
                    {
                        out.push((at + i, (b.rule.to_string(), b.message)));
                    }
                }
                out
            })
            .into_iter()
            .flatten()
            .collect();
        poll(limits)?;
        self.reused = false;
        let changes = changes.filter(|_| !again);
        let key = RuleKey {
            el_only: limits.el_only,
            counting: limits.counting,
            rooted: limits.rooted,
            rooted_merge: limits.rooted_merge,
            number_order: limits.number_order,
            eager: limits.expansion == Expansion::Eager,
            skip_dl: self.skip_dl,
            no_counting: self.no_counting,
            no_rooted: self.no_rooted,
            dl_automata: limits.dl_automata_level(),
        };
        // what the last run left, when this run may keep its contexts
        let kept = self.kept.take().filter(|k| !again && k.key == key);
        // The run decides its tier over the interner, which keeps the expressions of
        // removed axioms, those that the premises below intern for added axioms
        // outside OWL 2 EL, and what an earlier normalization outside OWL 2 EL
        // interned ([`TierMark::passed`]), none of which a fresh run holds when it
        // decides. One of them that the decision reads, such as a self restriction or
        // the top property, could keep Tier D from the axioms outside OWL 2 EL.
        let stale = self.stale_tier
            || changes.is_some_and(|c| {
                c.removed.iter().any(|k| !tier_neutral(k))
                    || c.added
                        .iter()
                        .any(|k| !tier_neutral(k) && el_break(k).is_some())
            });
        let had_top_chain = self.index.as_ref().is_some_and(|ix| ix.top_chain.is_some());
        let full = self.entangled
            || had_top_chain
            || match (&kept, changes) {
                (Some(k), Some(c)) => {
                    self.must_restart(k, c)
                        // a run kept by the EL rules follows only the premises of the
                        // changed axioms, and starts again once the interner is stale
                        // and axioms outside OWL 2 EL are there to choose a tier for.
                        // A run kept with the Horn rules or Tier D chose its tier with
                        // what its normalization interned, and starts again when the
                        // tier it finds after its index is built is another.
                        || k.tier == KeptTier::El
                            && stale
                            && !outside.is_empty()
                            && !limits.el_only
                }
                _ => true,
            };
        self.stale_tier = stale && !full;
        let kept = kept.filter(|_| !full);
        self.worlds = FxHashMap::default();
        self.query_cache = std::sync::OnceLock::new();
        if full {
            self.interner = Interner::new();
            self.interner.canon = self.canon_want.clone();
            self.ctx = Vec::new();
            self.disjoint = Vec::new();
            self.dynamic = None;
            self.dl_run = None;
            self.last_roots = Vec::new();
        }
        // a run that keeps the contexts of a run with the Horn rules, Tier D, datatype
        // checker components, or data-bearing EL edits compares the two indexes, so the
        // last one stays until the new one is built, and counts against the budget until
        // then, as do the contexts above the index's and Tier D's clause table
        let el_index_diff = kept.as_ref().is_some_and(|k| k.tier == KeptTier::El)
            && (self.index.as_ref().is_some_and(|ix| ix.dt.is_some())
                || changes
                    .is_some_and(|c| c.added.iter().chain(c.removed).any(|k| mentions_data(k))));
        let old_ix = match &kept {
            Some(k) if k.tier != KeptTier::El || el_index_diff => self.index.take(),
            _ => None,
        };
        let old_ix_bytes = match &old_ix {
            Some(ix) => {
                let mut since_poll = 0usize;
                ix.bytes_poll(&mut |work| {
                    since_poll = since_poll.saturating_add(work);
                    if since_poll >= POLL_INVALIDATE {
                        poll(limits)?;
                        since_poll = 0;
                    }
                    Ok(())
                })?
            }
            None => 0,
        };
        let dyn_retained_bytes = self.dynamic.as_ref().map_or(0, |d| d.retained_bytes());
        let old_bytes = old_ix_bytes
            + dyn_retained_bytes
            + self
                .dl_run
                .as_ref()
                .map_or(0, |d| d.table.bytes() + d.lits.bytes())
            + self.last_roots.capacity() * 4;
        // from here on a stop on the memory budget may be the kept state's, which a run
        // that starts again does not hold, whatever the tier of the kept contexts
        self.reused = kept.is_some();
        // the premises of the changed axioms, read from the interner before the index of
        // the new axioms is built
        let premises = match changes {
            Some(c) if !full && old_ix.is_none() => {
                debug_assert!(
                    !c.added.iter().chain(c.removed).any(|k| {
                        dtcheck::comp::Comps::may_trigger(&[*k], &|| false).unwrap_or(false)
                    }),
                    "el_index_diff routes data-bearing EL edits through index comparison rather than the premise fast path"
                );
                let mut scratch = Axioms::default();
                for &k in c.added.iter().chain(c.removed) {
                    index::normalize(k, &mut self.interner, &mut scratch);
                }
                Some(scratch)
            }
            _ => None,
        };
        // The old index is no longer read. Drop its handles before constructing the
        // replacement, while cached contexts retain their shared numeric ledger.
        self.index = None;
        // the kept contexts, which stay as they are until the invalidation, and the last
        // run's disjointness sets, which stay until the comparison below
        let kept_bytes = self.context_bytes();
        let kept_capacity = self.ctx.capacity();
        let sets_bytes = self.disjoint.capacity() * 24
            + self
                .disjoint
                .iter()
                .map(|d| d.capacity() * 4)
                .sum::<usize>();
        // the normalized premises of the changed axioms, which stay until the
        // invalidation
        let premises_bytes = premises.as_ref().map_or(0, |p| p.bytes());
        let (mut ax, mut ix, mut normalized_peak, outside_el, visible_named_classes) = self.build(
            axioms,
            &outside,
            threads,
            held_bytes + old_bytes + sets_bytes + premises_bytes,
            kept_bytes,
            limits,
            attempt,
        )?;
        if kept.is_some() && ix.top_chain.is_some() {
            #[cfg(test)]
            admission_test::hit(admission_test::Point::TopChainKeptPlan);
            return Ok(ControlFlow::Break(LateRestart::NamedDomain {
                normalized_peak,
            }));
        }
        let tier_d_disjunctive = outside.iter().any(|&(i, _)| {
            let k = axioms[i];
            !dl::shape(k).horn && dl_norm::may_accept(k)
        });
        drop(outside);
        // the disjointness sets by their numbers in this index, beside those of the
        // last run's, by which the contexts it leaves number the sets they meet; the run
        // reads them only in the build, so they move from the axioms, which the budget
        // counted, and the last run's are freed
        let renumbered = self.disjoint != ax.disjoint;
        self.disjoint = std::mem::take(&mut ax.disjoint);
        stats.outside_el = outside_el;
        stats.tier = stats.outside_el.tier();
        stats.horn_ranges_left_out = ax.horn.as_ref().map_or(0, |h| h.ranges_covered);
        poll(limits)?;
        let n = self.interner.len();
        // a run whose axioms the Horn normalization took starts its contexts again, with
        // the Horn rules or without, since the invalidation follows only the EL
        // normalization, and the conjunctions of the Horn rules get contexts above the
        // index's
        let horn = ax.horn.is_some();
        self.horn_tried = horn;
        let disj = ax.dl.is_some();
        self.dl_tried = disj;
        if self.merge.restarts > 0 && (!disj || ix.dl.as_deref().and_then(|d| d.root).is_none()) {
            // Rooted eligibility was lost after a derived merge restart: never answer in
            // Horn/EL/unrooted-DL using aliases derived by the discarded rooted theory.
            self.merge.clear();
            self.canon_want = None;
            self.active_hub = None;
            self.merge_disabled = true;
            return Ok(ControlFlow::Break(LateRestart::Incompatible {
                normalized_peak,
            }));
        }
        if let Some(d) = &ax.dl {
            stats.clauses = d.clauses.len();
        }
        // the rules of this run, when a later run may keep its contexts: the EL rules,
        // the Horn rules without individuals, universal roles or keys (including EL data
        // lattice, functional data properties and datatype-checker components), whose
        // contexts depend on one another only along their links, or Tier D (cautious or
        // eager) without a bound on its conclusions, which a run that counts, a rooted
        // run and the run after a passed bound have, and whose count of conclusions a
        // kept context would change
        let tier = match (horn, disj) {
            (false, false) => Some(KeptTier::El),
            // also a run whose Horn normalization gave the index no rule that can fire,
            // which the EL rules saturate: its contexts follow from the same entries
            (true, false) if ix.comparable() => Some(KeptTier::Horn),
            (false, true)
                if ix.horn.is_none()
                    && ix.dl.as_deref().is_some_and(|d| d.keyed_by_expressions())
                    && saturate::plain(&ix)
                    && ix.comparable()
                    && !self.no_counting =>
            {
                Some(KeptTier::Disj)
            }
            _ => None,
        };
        if let Some(k) = &kept
            && tier != Some(k.tier)
        {
            // the edit changed the rules the ontology needs, which the run decided over
            // an interner that still holds the expressions of removed axioms
            return Ok(ControlFlow::Break(LateRestart::Incompatible {
                normalized_peak,
            }));
        }
        let reuse = match &old_ix {
            Some(old) => {
                let stopped = std::cell::RefCell::new(None);
                let stop_tbl = || {
                    let r = poll(limits);
                    let stop = r.is_err();
                    if let Err(e) = r {
                        *stopped.borrow_mut() = Some(e);
                    }
                    stop
                };
                match ix.same_tables_with_stop(old, &stop_tbl) {
                    Some(true) => true,
                    Some(false) => {
                        return Ok(ControlFlow::Break(LateRestart::Incompatible {
                            normalized_peak,
                        }));
                    }
                    None => {
                        return Err(stopped.into_inner().unwrap_or(Error::Cancelled));
                    }
                }
            }
            None => false,
        };
        if !reuse {
            self.dynamic = None;
            self.dl_run = None;
            self.last_roots = Vec::new();
        }
        let premises = if (horn || disj) && !reuse {
            self.ctx = Vec::new();
            None
        } else {
            premises
        };
        self.ctx.resize_with(n, OnceLock::new);
        if ix.horn.is_some() {
            // the stop the pass saw, which a cancellation polled once does not repeat
            let stopped = std::cell::RefCell::new(None);
            horn::absorb(&mut ix, &|| {
                let r = poll(limits);
                let stop = r.is_err();
                if let Err(e) = r {
                    *stopped.borrow_mut() = Some(e);
                }
                stop
            });
            if let Some(e) = stopped.into_inner() {
                return Err(e);
            }
        }
        debug_assert!(ix.horn.as_ref().is_none_or(|h| h.base as usize == n));
        // Measure the rebuilt index and normalized axioms once with cooperative polling.
        // Neither `self.interner`, `self.disjoint`, `ix`, nor `ax` changes from here
        // through saturation, so rebase pre-admission, invalidation, and saturation
        // reuse these exact sizes without unpolled rescans.
        let (rebuilt_ix_bytes, rebuilt_ax_bytes) = {
            #[cfg(test)]
            if reuse {
                admission_test::hit(admission_test::Point::RebaseAccountStart);
            }
            let mut since_poll = 0usize;
            let mut tick = |work: usize| -> Result<(), Error> {
                since_poll = since_poll.saturating_add(work);
                if since_poll >= POLL_INVALIDATE {
                    poll(limits)?;
                    since_poll = 0;
                }
                Ok(())
            };
            let ix_bytes = self.index_bytes_poll(&ix, &mut tick)?;
            #[cfg(test)]
            if reuse {
                admission_test::hit(admission_test::Point::RebaseAccountAxiomsStart);
            }
            let ax_bytes = ax.bytes_poll(&mut tick)?;
            #[cfg(test)]
            if reuse {
                admission_test::hit(admission_test::Point::RebaseAccountEnd);
            }
            (ix_bytes, ax_bytes)
        };
        let dl_run = ix.dl.as_ref().map(|_| match self.dl_run.take() {
            // the kept contexts name the clauses of the last run's table by id
            Some(mut d) if reuse => {
                debug_assert_eq!(d.eager, limits.expansion == Expansion::Eager);
                d.begin_run();
                d
            }
            _ => dl_rules::DlRun::new(limits.expansion == Expansion::Eager, self.dl_worked.clone()),
        });
        let mut dl_run = dl_run.map(|mut d| {
            d.mrepair = dl_root::marker_repair(limits.rooted_marker_repair);
            d
        });
        let dynamic = if reuse {
            match self.dynamic.take() {
                Some(mut d) => {
                    // the conjunctions' contexts move above the expressions the edit added
                    let from = d.base();
                    let delta = n as Ex - from;
                    if delta > 0 {
                        let mut since_poll = 0usize;
                        let mut tick = |work: usize| -> Result<(), Error> {
                            since_poll = since_poll.saturating_add(work);
                            if since_poll >= POLL_INVALIDATE {
                                poll(limits)?;
                                since_poll = 0;
                            }
                            Ok(())
                        };
                        // Bound and pre-admit the peak single-table overlap during `shift_contexts`
                        // before allocating any replacement table.
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::RebaseScratchStart);
                        let mut rebase_scratch = 0usize;
                        for slot in &self.ctx {
                            tick(1)?;
                            if let Some(c) = slot.get() {
                                let s = c.st.lock().shift_scratch(from, &mut tick)?;
                                rebase_scratch = rebase_scratch.max(s);
                            }
                        }
                        for i in 0..d.len() as Ex {
                            tick(1)?;
                            if let Some(c) = d.get_ctx(from + i) {
                                let s = c.st.lock().shift_scratch(from, &mut tick)?;
                                rebase_scratch = rebase_scratch.max(s);
                            }
                        }
                        // Reuse precomputed rebuilt index/axiom sizes and retained static,
                        // dynamic, and old-index sizes (adjusting for any `self.ctx` slot
                        // growth from `resize_with`) so pre-admission performs no unpolled scan.
                        let slot = std::mem::size_of::<OnceLock<Box<saturate::Ctx>>>();
                        let rebase_ctx_bytes =
                            kept_bytes + self.ctx.capacity().saturating_sub(kept_capacity) * slot;
                        let rebase_live = rebuilt_ix_bytes
                            + held_bytes
                            + rebuilt_ax_bytes
                            + rebase_ctx_bytes
                            + dyn_retained_bytes
                            + dl_run.as_ref().map_or(0, |r| r.bytes())
                            + old_ix_bytes;
                        // Check for a stop after the scratch scan and before admission or mutation.
                        poll(limits)?;
                        let rebase_held = rebase_live.saturating_add(rebase_scratch);
                        let more = rebase_held.saturating_sub(normalized_peak);
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::RebaseAdmit {
                            live: rebase_live,
                            scratch: rebase_scratch,
                            paid: normalized_peak,
                        });
                        if limits.max_memory.is_some_and(|m| rebase_held > m)
                            || limits
                                .charge
                                .as_deref()
                                .is_some_and(|f| more > 0 && !f(more))
                        {
                            return Err(Error::Memory {
                                limit: limits.max_memory.unwrap_or(rebase_held),
                                used: rebase_held,
                            });
                        }
                        normalized_peak = normalized_peak.max(rebase_held);
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::RebaseShift);
                        for slot in &mut self.ctx {
                            tick(1)?;
                            if let Some(c) = slot.get_mut() {
                                c.st.get_mut().shift_contexts(from, delta, &mut tick)?;
                            }
                        }
                        for i in 0..d.len() as Ex {
                            tick(1)?;
                            if let Some(c) = d.get_ctx(from + i) {
                                c.st.lock().shift_contexts(from, delta, &mut tick)?;
                            }
                        }
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::RebaseDynamicStart);
                        d.rebase(n as Ex, &mut tick)?;
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::RebaseDynamicEnd);
                    }
                    Some(d)
                }
                None => None,
            }
        } else if let Some(h) = &ix.horn {
            debug_assert_eq!(h.base as usize, n);
            Some(saturate::Dynamic::new(n as Ex))
        } else if disj || horn {
            // the test contexts of Tier D get ids above the index's, and a run whose
            // Horn normalization gave no rule has no conjunctions and an empty table,
            // which a run that keeps its contexts expects
            Some(saturate::Dynamic::new(n as Ex))
        } else {
            None
        };
        if ix.dl.is_some() && dl_rules::tracing() {
            // the trace names each class by the last part of its IRI, once
            for (id, c) in self.interner.classes.iter().enumerate() {
                if let Some(e) = self.interner.get(&Node::Class(id as u32)) {
                    let name = c.as_str().rsplit(['#', '/']).next().unwrap_or("");
                    eprintln!("TR NAME {e} {name}");
                }
            }
        }
        if let Some(scratch) = premises {
            stats.incremental = true;
            // the invalidation's bit sets, one bit for each expression and each context,
            // beside the kept contexts, the index and the normalized axioms
            let bits = 8 * (self.interner.len().div_ceil(64) + self.ctx.len().div_ceil(64));
            // the context table may have grown for the expressions the edit added
            let slot = std::mem::size_of::<OnceLock<Box<saturate::Ctx>>>();
            let kept_bytes = kept_bytes + self.ctx.capacity().saturating_sub(kept_capacity) * slot;
            let mut budget = Scratch {
                limits,
                held: rebuilt_ix_bytes
                    + held_bytes
                    + rebuilt_ax_bytes
                    + kept_bytes
                    + premises_bytes
                    + bits,
                peak: normalized_peak,
                lists: 0,
            };
            budget.grow(0)?;
            stats.cleared = self.invalidate(&scratch, &ix, renumbered, &mut budget)?;
            #[cfg(test)]
            scratch_audit::premises(premises_bytes);
            normalized_peak = budget.peak;
            poll(limits)?;
        }
        let mut roots: Vec<Ex> = ax.classes.clone();
        roots.push(TOP);
        // every individual has an instance, whether an assertion or a class expression
        // names it
        roots.extend(ax.individuals.iter().copied());
        roots.extend(ix.nominals.iter().copied());
        // a rooted run's root context, where the facts of the individuals meet
        let rooted = ix.dl.as_deref().and_then(|d| d.root);
        roots.extend(rooted);
        roots.sort_unstable();
        roots.dedup();
        if let Some(old) = old_ix.filter(|_| reuse) {
            stats.incremental = true;
            let d_len = dynamic.as_ref().map_or(0, |d| d.len());
            let d_retained = dynamic.as_ref().map_or(0, |d| d.retained_bytes());
            // The comparison holds both indexes beside the kept contexts and their
            // clause table; once `old` drops, invalidation holds `changed` (during the
            // initial scan) plus `affected` and the slot-capped `queue`, including any
            // growth overlap. Pre-admit the peak of both phases; a run that passes it
            // starts again in full (`run_with`).
            let slots = self.ctx.len() + d_len;
            let inv_closure_peak = invalidation_scratch_peak(slots);
            let live_base = rebuilt_ix_bytes
                + held_bytes
                + rebuilt_ax_bytes
                + roots.capacity() * 4
                + self.last_roots.capacity() * 4
                + self.context_bytes()
                + d_retained
                + dl_run.as_ref().map_or(0, |d| d.bytes());
            let inv_peak = live_base
                .saturating_add(old.changed_bytes())
                .saturating_add(inv_closure_peak);
            let changed_scratch = ix.changed_scratch(&old, &|| poll(limits))?;
            let held = live_base
                .saturating_add(old_ix_bytes)
                .saturating_add(changed_scratch)
                .max(inv_peak);
            #[cfg(test)]
            admission_test::hit(admission_test::Point::CompareAdmit {
                live_base,
                old_ix_bytes,
                changed_scratch,
                inv_peak,
                held,
            });
            let more = held.saturating_sub(normalized_peak);
            if limits.max_memory.is_some_and(|m| held > m)
                || limits
                    .charge
                    .as_deref()
                    .is_some_and(|f| more > 0 && !f(more))
            {
                return Err(Error::Memory {
                    limit: limits.max_memory.unwrap_or(held),
                    used: held,
                });
            }
            normalized_peak = normalized_peak.max(held);
            // the stop the comparison saw, which a cancellation polled once does not
            // repeat
            let stopped = std::cell::RefCell::new(None);
            let stop_cmp = || {
                let r = poll(limits);
                let stop = r.is_err();
                if let Err(e) = r {
                    *stopped.borrow_mut() = Some(e);
                }
                stop
            };
            let dt_align = match (old.dt.as_deref(), ix.dt.as_deref()) {
                (Some(old_dt), Some(new_dt)) => {
                    match new_dt.align(old_dt, old.roles.sup.len(), &stop_cmp) {
                        dt_sat::DtAlignOutcome::Aligned(a) => Some(a),
                        dt_sat::DtAlignOutcome::Incompatible => {
                            return Ok(ControlFlow::Break(LateRestart::Incompatible {
                                normalized_peak,
                            }));
                        }
                        dt_sat::DtAlignOutcome::Stopped => {
                            return Err(stopped.into_inner().unwrap_or(Error::Cancelled));
                        }
                    }
                }
                _ => None,
            };
            let changed = ix.changed_with_dt(&old, dt_align.as_ref(), &|| poll(limits))?;
            drop(old);
            let last_roots = std::mem::take(&mut self.last_roots);
            stats.cleared = self.invalidate_changed(
                changed,
                &ix,
                dynamic.as_ref(),
                dt_align.as_ref(),
                &last_roots,
                &roots,
                renumbered,
                limits,
                live_base,
                &mut normalized_peak,
            )?;
            drop(last_roots);
            poll(limits)?;
        }
        stats.index = start.elapsed();
        debug_assert_eq!(self.last_roots.capacity(), 0);
        // the index, the caller's copy of the axioms, the normalized axioms and the roots,
        // and the contexts kept from the last run
        let base = rebuilt_ix_bytes
            + held_bytes
            + rebuilt_ax_bytes
            + roots.capacity() * 4
            + self.context_bytes()
            // the states of the conjunctions' contexts kept from the last run, whose
            // table counts at the end of the saturation
            + dynamic
                .as_ref()
                .map_or(0, |d| d.retained_bytes() - d.bytes());
        let cancelled = limits.cancelled.clone();
        let cancel_fn = cancelled
            .as_deref()
            .map(|f| f as &(dyn Fn() -> bool + Sync));
        let charge = limits.charge.clone();
        let charge_fn = charge
            .as_deref()
            .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
        // The Horn rules' contexts of conjunctions can need far more memory than the EL
        // rules on the same ontology, so a run with them has a budget of its own, past
        // which it classifies by the EL rules alone (run_with).
        self.dl_rooted = rooted.is_some();
        self.dl_counted = ax.dl.as_ref().is_some_and(|d| d.counting);
        self.dl_attempt =
            dl_run.is_some() && !self.dl_rooted && !self.dl_counted && !self.no_counting;
        let mut pending_adaptive: Option<(
            dl_rules::AdaptivePolicy,
            bool,
            bool,
            &'static str,
            usize,
        )> = None;
        let merge_discover =
            self.dl_rooted && !self.dl_counted && limits.rooted_merge && !self.merge_disabled;
        if merge_discover
            && merge_root_first()
            && let Some(root) = rooted
            && let Some(at) = roots.iter().position(|&r| r == root)
        {
            roots[..=at].rotate_right(1);
        }
        if let Some(d) = &mut dl_run {
            // a run that counts has the bound of the limits, and the run that starts again
            // without counting after it passed that bound has the larger one of a run that
            // cannot expect to finish soon, before the Horn rules take over; a rooted run
            // without counting has its own bound, past which the run starts again without
            // the rooted rules
            // only an attempt has a limit on the choices of a product
            d.set_width_limit(0);
            if self.dl_rooted && !self.dl_counted {
                let (adaptive_on, diag_on) = if limits.dl_steps {
                    (
                        limits.dl_work.is_none() && dl_rules::rooted_adaptive_bound(),
                        dl_rules::rooted_adaptive_diag(),
                    )
                } else {
                    (false, false)
                };
                let policy = if adaptive_on || diag_on {
                    dl_rules::active_adaptive_policy()
                } else {
                    dl_rules::AdaptivePolicy::PROVISIONAL
                };
                let fallback_fixed =
                    rooted_work_bound(visible_named_classes, rooted_bound_per_class());
                let selected = select_rooted_bound(
                    limits.dl_work,
                    limits.dl_steps,
                    adaptive_on,
                    policy,
                    fallback_fixed,
                );
                let effective_cap = self
                    .merge
                    .frozen_bound
                    .unwrap_or_else(|| selected.work_limit());
                #[cfg(test)]
                LAST_ROOTED_WORK_LIMIT.with(|c| c.set(effective_cap));
                d.set_work_limit(effective_cap);
                if limits.dl_steps
                    && (matches!(selected, RootedBoundPolicy::Adaptive(_)) || diag_on)
                {
                    let enforce = matches!(selected, RootedBoundPolicy::Adaptive(_));
                    pending_adaptive =
                        Some((policy, enforce, diag_on, selected.name(), effective_cap));
                }
                if merge_discover {
                    d.set_merge(true);
                    if self.merge.spent > 0 {
                        d.add_carried(self.merge.spent);
                    }
                    if !merge_hub()
                        && let Some(hub) = &self.active_hub
                        && let Some(id) = self.interner.individuals.get(hub)
                    {
                        d.merge_skip = Some(id);
                    }
                }
            } else if self.no_counting {
                d.set_work_limit(DL_RETRY_WORK);
            } else if self.dl_counted {
                d.set_work_limit(limits.dl_work.unwrap_or(DL_WORK_BOUND).max(1));
            } else {
                // Any other run has a bound that grows with the classes, past which the
                // Horn rules answer (roadmap item 990). The classes are the signature's,
                // which a run that keeps contexts and one that starts again share, so
                // both take the same bound.
                let classes = roots.len();
                d.set_work_limit(limits.dl_attempt_work.unwrap_or_else(|| {
                    DL_ATTEMPT_FLOOR.max(DL_ATTEMPT_PER_CLASS.saturating_mul(classes))
                }));
                // and on one thread, whose order is fixed, a product of more choices than
                // any attempt that finishes makes ends it too (roadmap item 1011)
                if threads == 1 {
                    d.set_width_limit(limits.dl_attempt_width.unwrap_or(DL_ATTEMPT_WIDTH));
                }
            }
        }
        let max_memory = if horn || disj {
            // A deterministic logical input-size measure determines the classifier's
            // discretionary Horn/Tier D memory allowance, independent of thread-count or
            // allocator container capacities, while `base` continues to charge actual
            // retained capacities against the caller's memory cap and reservation callback.
            let allowance_base = self.logical_index_bytes(&ix)
                + held_bytes
                + ax.logical_bytes()
                + roots.len() * 4
                + self.ctx.len() * std::mem::size_of::<OnceLock<Box<saturate::Ctx>>>();
            let own = HORN_MEMORY_FLOOR.max(allowance_base.saturating_mul(HORN_MEMORY_FACTOR));
            #[cfg(test)]
            LAST_ALLOWANCE.with(|c| c.set((base, allowance_base, own)));
            Some(limits.max_memory.map_or(own, |m| m.min(own)))
        } else {
            limits.max_memory
        };
        // With `Limits::dl_steps` every saturation of a run of Tier D that has a bound on
        // its derived conclusions runs in steps. A run without the bound gives the same
        // answer on every schedule and keeps the driver without steps. The ids of a rooted
        // run's literals and of the eager strategy's contexts follow the order in which
        // the threads make them, so those runs take one thread instead, whose order is
        // fixed. The worlds and the taxonomy keep the caller's threads.
        // The bound of a run that neither counts nor is rooted (roadmap item 990) keeps the
        // driver without steps, with which the attempts that finish on eight threads took
        // 1.3 to 1.9 times less time than in steps on the laptop. Such an attempt either finishes well within its
        // bound or runs on far past it, so a schedule decides its outcome only for an
        // ontology near the bound, as it does for the memory budget.
        let fixed = limits.dl_steps
            && disj
            && dl_run.as_ref().is_some_and(|d| d.bounded())
            && !self.dl_attempt;
        let steps = fixed && rooted.is_none() && limits.expansion == Expansion::Cautious;
        let saturation_threads = if fixed && !steps { 1 } else { threads };
        // Admit the pending `Box<AdaptiveState>` overlap before allocating it, reusing any
        // paid normalization headroom and charging the state once in the live ledger.
        let adaptive_bytes = if pending_adaptive.is_some() {
            std::mem::size_of::<dl_rules::AdaptiveState>()
        } else {
            0
        };
        let pre_sat = if adaptive_bytes > 0 {
            let dyn_before = dynamic.as_ref().map_or(0, |d| d.bytes());
            let dl_before = dl_run.as_ref().map_or(0, |d| d.bytes());
            let Some(total) = base
                .checked_add(dyn_before)
                .and_then(|n| n.checked_add(dl_before))
                .and_then(|n| n.checked_add(adaptive_bytes))
            else {
                return Err(Error::Memory {
                    limit: max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                });
            };
            total
        } else {
            base
        };
        let pre_cap = if adaptive_bytes > 0 {
            max_memory
        } else {
            limits.max_memory
        };
        // the pool has been charged for the peak of a parallel normalization
        let more = pre_sat.saturating_sub(normalized_peak);
        #[cfg(test)]
        if adaptive_bytes > 0 {
            LAST_ADAPTIVE_PREADMIT.with(|c| c.set(Some((normalized_peak, pre_sat, more))));
        }
        if pre_cap.is_some_and(|m| pre_sat > m) || charge_fn.is_some_and(|f| more > 0 && !f(more)) {
            return Err(Error::Memory {
                limit: pre_cap.unwrap_or(pre_sat),
                used: pre_sat,
            });
        }
        if let Some((policy, enforce, diag_on, policy_name, effective_cap)) = pending_adaptive
            && let Some(d) = &mut dl_run
        {
            if diag_on && self.adaptive_run_id == 0 {
                self.adaptive_run_id = next_adaptive_run_id();
            }
            self.adaptive_attempts += 1;
            let meta = dl_rules::AdaptiveMeta {
                run_id: self.adaptive_run_id,
                attempt: self.adaptive_attempts,
                after_counting: self.no_counting,
                policy_name,
                effective_cap,
                mrepair: d.mrepair,
                threads: limits.threads,
            };
            d.enable_adaptive(policy, enforce, diag_on, meta);
        }
        let bounds = Bounds {
            threads: saturation_threads,
            deadline: limits.deadline,
            max_memory,
            cancelled: cancel_fn,
            charge: charge_fn,
            base_memory: base,
            base_queued: 0,
            peak_memory: if adaptive_bytes > 0 {
                normalized_peak.max(pre_sat)
            } else {
                normalized_peak
            },
            paid_memory: if charge_fn.is_some() {
                normalized_peak.max(pre_sat)
            } else {
                0
            },
            dynamic: dynamic.as_ref(),
            dl: dl_run.as_ref(),
            partial: limits.partial && disj && with_taxonomy,
            steps,
        };
        let t = Instant::now();
        let stopped = |stop: Stop| match stop {
            Stop::Cancelled => Error::Cancelled,
            Stop::Timeout => Error::Timeout,
            // the bound on derived conclusions goes back as the memory error that the
            // fallback chain of `run_with` takes
            Stop::Memory { used } | Stop::Work { used } | Stop::Merge { used } => Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            },
        };
        // a run that answers in part keeps the reason it stopped and goes on to the answer
        let mut halted: Option<Error> = None;
        // whether the stop that won was Tier D's bound on derived conclusions, which the
        // fallback chain reads in `dl_worked`: a check that saw the bound passed but lost
        // the stop to the caller's deadline, cancellation or memory refusal does not count
        let mut over_work = false;
        let partial = bounds.partial;
        #[cfg(test)]
        if stats.incremental {
            admission_test::hit(admission_test::Point::KeptSaturationStart);
        }
        let (sat_totals, sat_stop) =
            saturate::saturate_counted(&ix, &self.ctx, &roots, &[], &[], &bounds);
        #[cfg(test)]
        if stats.incremental {
            admission_test::hit(admission_test::Point::KeptSaturationEnd {
                stopped_by_timeout: matches!(sat_stop, Some(Stop::Timeout)),
            });
        }
        // Capture saturation counters, heap occupancy oracle, and winning stop reason
        // immediately after `saturate_counted`, before error translation, `partial=true`
        // routing, marker decline, or partial store teardown.
        if let Some(d) = dl_run.as_ref().filter(|d| d.tracks_adaptive()) {
            self.record_adaptive_saturation(d, dynamic.as_ref(), sat_stop);
            d.stop_adaptive_sampling();
        }
        let mut totals = match (sat_totals, sat_stop) {
            (t, None) => t,
            (_, Some(Stop::Merge { used })) => {
                // Disable merge discovery outside the main round and inspect the winning
                // payload after all workers have joined. A merge stop never returns a
                // partial taxonomy.
                if let Some(d) = &dl_run {
                    d.set_merge(false);
                    let spent = d.work_total();
                    let bound = d.work_limit();
                    let exhausted = d.over_work() || (bound != 0 && spent >= bound);
                    let payload = d.take_merged();
                    if !exhausted
                        && let Some((pairs, pairs_bytes)) = payload
                        && !pairs.is_empty()
                    {
                        self.dl_worked.store(false, Ordering::Relaxed);
                        self.merge_stop = Some(MergeStop {
                            pairs,
                            pairs_bytes,
                            spent,
                            bound,
                            peak: used,
                            active_hub: self.active_hub.take(),
                        });
                        return Err(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used,
                        });
                    }
                    self.dl_worked.store(true, Ordering::Relaxed);
                    #[cfg(test)]
                    PASSED_AT.with(|p| p.set(d.work()));
                }
                return Err(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used,
                });
            }
            (t, Some(s)) if partial => {
                over_work = matches!(s, Stop::Work { .. });
                halted = Some(stopped(s));
                t
            }
            (_, Some(s)) => {
                self.dl_worked
                    .store(matches!(s, Stop::Work { .. }), Ordering::Relaxed);
                #[cfg(test)]
                if matches!(s, Stop::Work { .. }) {
                    PASSED_AT.with(|p| p.set(dl_run.as_ref().map_or(0, |d| d.work())));
                }
                return Err(stopped(s));
            }
        };
        if let Some(d) = &dl_run {
            // Never trigger merge discovery in later marking/test rounds.
            d.set_merge(false);
        }
        // the candidates whose tests ended without a contradiction
        let mut refuted: FxHashSet<(Ex, Ex)> = FxHashSet::default();
        // Tier D orders the atoms totally, which leaves the subsumers that only a case
        // analysis gives as clauses: each named class that the canonical model of a
        // class's context makes true is tested in a context of its own, and those that
        // hold join the class's subsumers, until no class has a new candidate. A rooted
        // run's order leaves named classes over x incomparable, so each named class's
        // context is complete for its subsumers without the tests (F24, condition C2 of
        // Theorem 2 of the journal's calculus).
        if disj && halted.is_none() && rooted.is_none() {
            let tests_start = Instant::now();
            let d = dynamic
                .as_ref()
                .expect("the dynamic contexts of a run with Tier D");
            let mut tested: FxHashSet<(Ex, Ex)> = FxHashSet::default();
            // the candidates of a group test that failed, each tested alone next
            let mut singles: Vec<(Ex, Ex)> = Vec::new();
            // one round of the saturation, which records a stop for a partial answer
            let round = |seeds: &[Ex], tests: &[Ex], totals: &mut saturate::Totals| {
                let base_queued = if charge_fn.is_some() {
                    seeds
                        .iter()
                        .filter_map(|&e| self.ctx[e as usize].get())
                        .map(|c| c.todo.lock().len())
                        .sum()
                } else {
                    0
                };
                let b = Bounds {
                    base_memory: totals.memory,
                    base_queued,
                    peak_memory: totals.peak,
                    paid_memory: totals.paid,
                    ..bounds
                };
                let (more, stop) =
                    saturate::saturate_counted(&ix, &self.ctx, tests, seeds, &[], &b);
                *totals = saturate::Totals {
                    conclusions: totals.conclusions + more.conclusions,
                    memory: more.memory,
                    peak: totals.peak.max(more.peak),
                    paid: more.paid,
                    merged: false,
                };
                stop.map_or(Ok(()), Err)
            };
            loop {
                if let Err(e) = poll(limits) {
                    halted = Some(e);
                    break;
                }
                let found = dl_rules::candidates(&ix, &self.ctx, &roots, &tested);
                tested.extend(found.iter().copied());
                // A class's candidates are tested together first, since most fail: a
                // context where none holds shows that no one of them is a subsumer.
                let mut tests: Vec<(Ex, Vec<Ex>)> = Vec::new();
                for (x, b) in found {
                    match tests.last_mut() {
                        Some((y, bs)) if *y == x => bs.push(b),
                        _ => tests.push((x, vec![b])),
                    }
                }
                if reuse {
                    // A run that keeps the tests of the last one finds a class's
                    // candidates in one round where that run found them over several,
                    // so their group is new while each has a test of its own. Those
                    // tests answer for the group.
                    let mut split: Vec<(Ex, Vec<Ex>)> = Vec::with_capacity(tests.len());
                    for (x, bs) in tests {
                        if bs.len() > 1
                            && !dl_rules::has_test(d, x, &bs)
                            && bs.iter().all(|&b| dl_rules::has_test(d, x, &[b]))
                        {
                            split.extend(bs.into_iter().map(|b| (x, vec![b])));
                        } else {
                            split.push((x, bs));
                        }
                    }
                    tests = split;
                }
                tests.extend(singles.drain(..).map(|(x, b)| (x, vec![b])));
                if tests.is_empty() {
                    break;
                }
                let ids: Vec<Ex> = tests
                    .iter()
                    .map(|(x, bs)| dl_rules::test(d, *x, bs))
                    .collect();
                if let Err(s) = round(&[], &ids, &mut totals) {
                    over_work = matches!(s, Stop::Work { .. });
                    halted = Some(stopped(s));
                    break;
                }
                let mut seeds: Vec<Ex> = Vec::new();
                for ((x, bs), &id) in tests.iter().zip(&ids) {
                    if !d.ctx(id).st.lock().bottom {
                        refuted.extend(bs.iter().map(|&b| (*x, b)));
                        continue;
                    }
                    if let [b] = bs.as_slice() {
                        if let Some(c) = self.ctx.get(*x as usize).and_then(|c| c.get()) {
                            c.todo.lock().push(saturate::Concl::Sub(*b));
                            seeds.push(*x);
                            stats.dl_tests_held += 1;
                        }
                    } else {
                        singles.extend(bs.iter().map(|&b| (*x, b)));
                    }
                }
                stats.dl_tests += tests.len();
                if seeds.is_empty() {
                    continue;
                }
                seeds.sort_unstable();
                seeds.dedup();
                if let Err(s) = round(&seeds, &[], &mut totals) {
                    over_work = matches!(s, Stop::Work { .. });
                    halted = Some(stopped(s));
                    break;
                }
            }
            stats.dl_testing = tests_start.elapsed();
        }
        // A stop at Tier D's bound on derived conclusions is the run's own and not the
        // caller's, so it is not a partial answer: it goes back as the error that the
        // fallback chain of `run_with` takes to the run that starts again, with counting
        // or the rooted rules left out, and the caller gets that run's answer.
        self.dl_worked.store(over_work, Ordering::Relaxed);
        #[cfg(test)]
        if over_work {
            PASSED_AT.with(|p| p.set(dl_run.as_ref().map_or(0, |d| d.work())));
        }
        if (!partial || over_work)
            && let Some(h) = halted.take()
        {
            return Err(h);
        }
        // the dependency record of a partial answer: the classes it leaves open and the
        // consistency
        let open = halted.as_ref().map(|_| {
            #[cfg(test)]
            if admission_test::observing() && matches!(halted, Some(Error::Memory { .. })) {
                admission_test::observe(admission_test::Attempt::Dependencies {
                    messages: admission_test::queued(&self.ctx, dynamic.as_ref()),
                    peak: totals.peak,
                    paid: totals.paid,
                });
            }
            // The dependency record cannot see the work in the root, in what the root
            // reaches or on a propagation to or from it, so a stopped rooted run leaves
            // every class open and its consistency unknown.
            if rooted.is_some() {
                let incomplete: Vec<Ex> = roots.iter().copied().filter(|&r| r != TOP).collect();
                return (incomplete, Consistency::Unknown);
            }
            let (complete, known) =
                dl_rules::record(&ix, &self.ctx, dynamic.as_ref(), &roots, &refuted);
            let incomplete: Vec<Ex> = roots
                .iter()
                .zip(complete)
                .filter(|&(&r, c)| !c && r != TOP)
                .map(|(&r, _)| r)
                .collect();
            let consistency = match known {
                dl_rules::Known::Consistent => Consistency::Consistent,
                dl_rules::Known::Inconsistent => Consistency::Inconsistent,
                dl_rules::Known::Unknown => Consistency::Unknown,
            };
            (incomplete, consistency)
        });
        // A non-work stop already won and a partial answer is being constructed: disarm
        // adaptive work-bound enforcement on `dl_run` before the individual-marking round
        // so a previously latched adaptive guard cannot abort the partial answer with a
        // late `Stop::Work`, while keeping `dl_run.bytes()` intact and preserving fixed /
        // non-adaptive work enforcement verbatim.
        if open.is_some()
            && let Some(d) = dl_run.as_ref()
        {
            d.disarm_adaptive_work_bound();
        }
        // Dependency recording has seen the stopped to-do lists. The messages still
        // occupy memory while marking and retained output run.
        let pending_messages = || {
            if charge_fn.is_some() && open.is_some() {
                self.pending_messages(dynamic.as_ref())
            } else {
                0
            }
        };
        let mut pending = pending_messages();
        #[cfg(test)]
        if open.is_some() && admission_test::observing() {
            admission_test::observe(admission_test::Attempt::Pending {
                actual: admission_test::queued(&self.ctx, dynamic.as_ref()),
                counted: pending,
            });
        }
        let world_base = |stored, queued| {
            if charge_fn.is_none() {
                return Ok(stored);
            }
            saturate::live_memory(stored, queued, dynamic.as_ref(), dl_run.as_ref()).ok_or(
                Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                },
            )
        };
        // Which contexts are real matters only when a context derived an individual, for
        // keys and for universal roles. Then owl:Thing and the individuals become real,
        // and so does what they reach, between rounds of the saturation, with the facts
        // of universal roles and the merges of keys, until none is new.
        let marking_start = Instant::now();
        let mut universal: Vec<Ex> = Vec::new();
        // a run with the Horn rules and individuals always marks them, since R7 reads
        // the real predecessors of the global state (roadmap item 692)
        let entangled = ix.has_individuals()
            && (totals.merged || !ix.universal.is_empty() || !ix.keys.is_empty() || horn);
        if entangled {
            let mut seeds: Vec<(Ex, saturate::Concl)> = std::iter::once(TOP)
                .chain(ix.nominals.iter().copied())
                .map(|e| (e, saturate::Concl::Real))
                .collect();
            loop {
                poll(limits)?;
                #[cfg(test)]
                let before = admission_test::observing()
                    .then(|| admission_test::queued(&self.ctx, dynamic.as_ref()));
                let mut woken: Vec<Ex> = Vec::with_capacity(seeds.len());
                for (e, c) in seeds.drain(..) {
                    if let Some(x) = self.ctx.get(e as usize).and_then(|x| x.get()) {
                        x.todo.lock().push(c);
                        woken.push(e);
                    }
                }
                let base_queued = if charge_fn.is_some() {
                    pending + woken.len()
                } else {
                    0
                };
                #[cfg(test)]
                if let Some(before) = before {
                    admission_test::observe(admission_test::Attempt::Marking {
                        before,
                        after: admission_test::queued(&self.ctx, dynamic.as_ref()),
                        seeds: woken.len(),
                        counted: base_queued,
                    });
                }
                woken.sort_unstable();
                woken.dedup();
                let round = Bounds {
                    base_memory: totals.memory,
                    base_queued,
                    peak_memory: totals.peak,
                    paid_memory: totals.paid,
                    ..bounds
                };
                let more = saturate::saturate(&ix, &self.ctx, &[], &woken, &universal, &round)
                    .map_err(stopped)?;
                totals = saturate::Totals {
                    conclusions: totals.conclusions + more.conclusions,
                    memory: more.memory,
                    peak: totals.peak.max(more.peak),
                    paid: more.paid,
                    merged: totals.merged || more.merged,
                };
                pending = pending_messages();
                let view = world::GlobalAll {
                    ctx: &self.ctx,
                    dynamic: dynamic.as_ref(),
                };
                let facts = world::universal_facts(&ix, &view, &universal);
                let key_budget = WorldBudget::start(
                    limits,
                    charge_fn,
                    world_base(totals.memory, pending)?,
                    &mut totals,
                )?;
                let key_bound = |delta: isize| key_budget.tell(delta);
                let merges = world::key_merges(&ix, &view, &key_bound).map_err(|h| {
                    key_budget.stopped().err().unwrap_or_else(|| match h {
                        index::Halt::Memory(used) => Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used,
                        },
                        index::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                    })
                })?;
                key_budget.finish(&mut totals);
                if facts.is_empty() && merges.is_empty() {
                    break;
                }
                if !facts.is_empty() {
                    for (e, c) in self.ctx.iter().enumerate() {
                        if c.get().is_some_and(|c| c.st.lock().init) {
                            seeds.extend(facts.iter().map(|&x| (e as Ex, saturate::Concl::Sub(x))));
                        }
                    }
                    if let Some(d) = &dynamic {
                        for i in 0..d.len() as Ex {
                            let e = d.base() + i;
                            if d.get_ctx(e).is_some_and(|c| c.st.lock().init) {
                                seeds.extend(facts.iter().map(|&x| (e, saturate::Concl::Sub(x))));
                            }
                        }
                    }
                }
                universal.extend_from_slice(&facts);
                for &(a, b) in merges.iter() {
                    seeds.push((a, saturate::Concl::Sub(b)));
                    seeds.push((b, saturate::Concl::Sub(a)));
                }
            }
        }
        // the classes whose instances reach an individual through a context that is not
        // real, each in a world of its own
        let mut world_bytes = 0usize;
        let mut world_live = 0usize;
        if entangled {
            let witnesses = world::witnesses(&ix, &self.ctx, dynamic.as_ref(), &universal);
            if !witnesses.is_empty() {
                let classes = world::reaching(&ix, &self.ctx, dynamic.as_ref(), &witnesses);
                let mut real: Vec<Ex> = (0..self.ctx.len() as Ex)
                    .filter(|&e| self.ctx[e as usize].get().is_some_and(|c| c.st.lock().real))
                    .collect();
                if let Some(d) = &dynamic {
                    real.extend(
                        (0..d.len() as Ex)
                            .map(|i| d.base() + i)
                            .filter(|&e| d.get_ctx(e).is_some_and(|c| c.st.lock().real)),
                    );
                }
                // the worlds running and the subsumers of those done count against the
                // budget beside the saturated state, and the pool is charged for what
                // passes the peak
                let budget = WorldBudget::start(
                    limits,
                    charge_fn,
                    world_base(totals.memory, pending)?,
                    &mut totals,
                )?;
                #[cfg(test)]
                let row_test = shared_world_found_row_tests::active();
                #[cfg(test)]
                let _row_phase = row_test
                    .as_deref()
                    .map(|test| test.phase(&budget, &self.interner, &classes, ix.dt.as_deref()));
                #[cfg(test)]
                let mixed_test = world_retirement_mixed_tests::active();
                #[cfg(test)]
                let _mixed_phase = mixed_test.as_deref().map(|test| {
                    assert!(ix.dt.is_none(), "mixed fixture excludes checker ownership");
                    test.phase(&budget, &classes, ix.top_chain.is_some(), self.worlds.len())
                });
                let bound = |delta: isize| budget.tell(delta);
                let found = par_map(&classes, threads, MIN_CLASSES, |_offset, chunk| {
                    #[cfg(test)]
                    let mixed_worker = mixed_test
                        .as_ref()
                        .map(|test| test.worker(_offset, chunk, &budget));
                    #[cfg(test)]
                    let _row_worker = row_test.as_deref().map(|test| test.worker(_offset, chunk));
                    let mut out = FoundWorldChunk {
                        rows: Vec::with_capacity(chunk.len()),
                        debit: FoundWorldDebit {
                            budget: &budget,
                            outstanding: 0,
                        },
                    };
                    for &d in chunk {
                        #[cfg(test)]
                        if let Some(test) = row_test.as_deref() {
                            test.before_world(_offset, out.rows.len())?;
                        }
                        #[cfg(test)]
                        if let Some(worker) = &mixed_worker {
                            worker.before_world(out.rows.len())?;
                        }
                        let w = world::World::new_with_budget(
                            &ix,
                            &self.ctx,
                            dynamic.as_ref(),
                            &universal,
                            &real,
                            &bound,
                            &budget,
                        );
                        let f = w.classify(d)?;
                        #[cfg(test)]
                        if let Some(worker) = &mixed_worker {
                            worker.after_world(d)?;
                        }
                        let kept = f
                            .subs
                            .len()
                            .checked_mul(4)
                            .and_then(|bytes| bytes.checked_add(48))
                            .and_then(|bytes| isize::try_from(bytes).ok())
                            .ok_or(index::Halt::Memory(usize::MAX))?;
                        let outstanding = out
                            .debit
                            .outstanding
                            .checked_add(kept)
                            .ok_or(index::Halt::Memory(usize::MAX))?;
                        #[cfg(test)]
                        if let Some(test) = row_test.as_deref() {
                            test.before_report(&budget, _offset, d, &f, kept as usize);
                        }
                        out.rows.push((d, f, kept));
                        #[cfg(test)]
                        if let Some(test) = row_test.as_deref() {
                            test.before_post(out.rows.len());
                        }
                        #[cfg(test)]
                        if let Some(worker) = &mixed_worker {
                            worker.before_row_post(out.rows.len(), d, kept);
                        }
                        if !out.debit.post(kept, outstanding) {
                            return Err(index::Halt::Stopped);
                        }
                        #[cfg(test)]
                        if let Some(test) = row_test.as_deref() {
                            test.admitted(_offset, d);
                        }
                        #[cfg(test)]
                        if let Some(worker) = &mixed_worker {
                            worker.admitted(d, kept);
                        }
                    }
                    Ok(out)
                })
                .into_iter()
                .collect::<Result<Vec<_>, index::Halt>>();
                #[cfg(test)]
                if let Some(test) = row_test.as_deref() {
                    test.collected(
                        &budget,
                        found
                            .as_ref()
                            .map(|chunks| chunks.iter().map(|chunk| chunk.rows.len()).sum())
                            .map_err(|&halt| halt),
                        ix.dt.as_deref(),
                    );
                }
                #[cfg(test)]
                if let Some(test) = mixed_test.as_deref() {
                    test.collected(&budget, found.as_ref().err().copied());
                }
                budget.stopped()?;
                let found = found.map_err(|h| match h {
                    index::Halt::Memory(used) => Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used,
                    },
                    index::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                })?;
                budget.finish(&mut totals);
                if charge_fn.is_some() {
                    world_live = budget.held.load(Ordering::Relaxed).max(0) as usize;
                    #[cfg(test)]
                    if let Some(test) = row_test.as_deref() {
                        test.pooled_live(world_live);
                    }
                }
                for FoundWorldChunk { rows, mut debit } in found {
                    // The iterator and each yielded row drop before the debit.
                    for (d, f, kept) in rows {
                        let next_bytes =
                            world_bytes
                                .checked_add(kept as usize)
                                .ok_or(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                })?;
                        let remaining = debit.outstanding - kept;
                        #[cfg(test)]
                        if let Some(test) = row_test.as_deref() {
                            test.before_insert(self.worlds.len());
                        }
                        self.worlds.insert(d, (f.bottom, f.subs));
                        world_bytes = next_bytes;
                        debit.outstanding = remaining;
                        #[cfg(test)]
                        if let Some(test) = row_test.as_deref() {
                            test.after_insert(self.worlds.len());
                        }
                    }
                }
                #[cfg(test)]
                if admission_test::observing() {
                    admission_test::observe(admission_test::Attempt::Joined {
                        base: budget.base,
                        live: budget.held.load(Ordering::Relaxed),
                        rows: self
                            .worlds
                            .values()
                            .map(|(_, subs)| 48 + 4 * subs.len())
                            .sum(),
                        paid: totals.paid,
                    });
                }
            }
            self.entangled = entangled;
        } else {
            self.entangled = false;
        }
        // a change after a run with the Horn normalization or Tier D saturates everything
        // again, unless the run's contexts depend on one another only in ways the
        // invalidation follows (roadmap item 753)
        self.entangled |= (horn || disj) && tier.is_none();
        let kept_now = tier.filter(|_| open.is_none()).map(|tier| Kept {
            tier,
            key,
            outside: stats.outside_el.horn + stats.outside_el.disjunctive > 0,
            disjunctive: tier_d_disjunctive,
            nodes: kept.map_or(self.interner.len(), |k| k.nodes),
            dynamic: kept.map_or(dynamic.as_ref().map_or(0, |d| d.len()), |k| k.dynamic),
            table: kept.map_or(dl_run.as_ref().map_or(0, |d| d.table.len()), |k| k.table),
        });
        if let Some(d) = &dl_run {
            // the contexts made during the run, the tests and the keyed hypothetical
            // successors, count with the index's
            let base = self.ctx.len() as Ex;
            let made = dynamic
                .iter()
                .flat_map(|dy| (0..dy.len() as Ex).filter_map(move |i| dy.get_ctx(base + i)));
            for c in self
                .ctx
                .iter()
                .filter_map(|c| c.get().map(|c| &**c))
                .chain(made)
            {
                if let Some(n) = c.st.lock().extra.as_ref().and_then(|x| x.dl.as_ref()) {
                    stats.dl_contexts += 1;
                    stats.dl_stored += n.store.clauses.len();
                }
            }
            stats.dl_work = d.work();
            let decline = dl_root::marker_decline(limits.rooted_decline);
            if self.dl_rooted && open.is_none() && (dl_root::marker_diag() || decline.is_some()) {
                #[cfg(test)]
                if admission_test::observing() {
                    admission_test::observe(admission_test::Attempt::Marker {
                        base: world_base(totals.memory, pending)?,
                        rows: self
                            .worlds
                            .values()
                            .map(|(_, subs)| 48 + 4 * subs.len())
                            .sum(),
                        paid: totals.paid,
                        contexts: self.ctx.len() + dynamic.as_ref().map_or(0, |d| d.len()),
                    });
                }
                // item 1231's decline, and its diagnostic, after a saturation that finished.
                // A stopped rooted run leaves every class open, so its partial answer
                // claims nothing.
                let made = dynamic.iter().flat_map(|dy| {
                    (0..dy.len() as Ex)
                        .filter_map(move |i| dy.get_ctx(base + i).map(|c| (base + i, c)))
                });
                // The scan's copies of the contexts' successors and triggers, their scratch
                // sets and the maps and sets of the search count beside the saturated state
                // and the table of conjunctions, which are still held. Each part is admitted
                // against the run's budget before it is allocated, and the pool is charged
                // for what passes the peak.
                let held = if charge_fn.is_some() {
                    world_base(totals.memory, pending)?
                        .checked_add(world_live)
                        .ok_or(Error::Memory {
                            limit: max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        })?
                } else {
                    totals.memory + d.bytes() + dynamic.as_ref().map_or(0, |dy| dy.bytes())
                };
                #[cfg(test)]
                let mut scan = SCAN_EXTRA.with(|x| x.get()) as isize;
                #[cfg(not(test))]
                let mut scan = 0isize;
                let mut refused = None;
                let mut admit = |delta: isize| {
                    scan += delta;
                    if delta <= 0 {
                        return true;
                    }
                    #[cfg(test)]
                    admission_test::observe(admission_test::Attempt::Scan {
                        scratch: scan.max(0) as usize,
                        paid: totals.paid,
                    });
                    let used = held + scan.max(0) as usize;
                    totals.peak = totals.peak.max(used);
                    if !cover_memory(used, max_memory, charge_fn, &mut totals.paid) {
                        refused = Some(used);
                        return false;
                    }
                    true
                };
                // the copies' own vector, reserved for every context before any copy
                let n = self.ctx.len() + dynamic.as_ref().map_or(0, |dy| dy.len());
                let mut ok = admit((n * std::mem::size_of::<dl_root::MarkerCtx>()) as isize);
                let mut cs = Vec::with_capacity(if ok { n } else { 0 });
                let diag = dl_root::marker_diag();
                let mut refs = Vec::new();
                for (e, c) in self
                    .ctx
                    .iter()
                    .enumerate()
                    .filter_map(|(e, c)| c.get().map(|c| (e as Ex, &**c)))
                    .chain(made)
                {
                    if !ok {
                        break;
                    }
                    let st = c.st.lock();
                    match dl_root::marker_ctx(&ix, e, &st, &d.lits, &mut admit) {
                        Ok(Some(m)) => {
                            cs.push(m);
                            if diag {
                                refs.push(c);
                            }
                        }
                        Ok(None) => {}
                        Err(()) => {
                            ok = false;
                            break;
                        }
                    }
                }
                ok = ok && admit(dl_root::marker_search_bytes(&cs) as isize);
                drop(admit);
                // the deficits' copies, outside the run's memory budget, as the diagnostic's
                // sets are: the root's live clauses with ground bodies first, which each
                // holder's copy reads
                let mut defs = Vec::new();
                // the deficits' work is counted apart from the run's, whose count a later
                // run that keeps these contexts carries, and the run's deadline and
                // cancellation stop it
                let stop = std::cell::Cell::new(None);
                let dwork = std::cell::Cell::new(0usize);
                let mut dpoll = |n: usize| {
                    dwork.set(dwork.get().saturating_add(n));
                    poll(limits).map_err(|e| stop.set(Some(e))).is_ok()
                };
                let cf = dl_root::marker_def_cf(d.mrepair);
                if diag && ok {
                    let rg = cs
                        .iter()
                        .zip(&refs)
                        .find(|(m, _)| dl_root::is_root(&ix, m.me))
                        .map(|(m, c)| {
                            dl_root::marker_root_gbody(&ix, m.me, &c.st.lock(), &d.lits, &mut dpoll)
                        })
                        .unwrap_or(Some(Vec::new()));
                    // a refusal recorded its stop, which the run returns below
                    for (m, c) in cs
                        .iter()
                        .zip(&refs)
                        .take(if rg.is_some() { cs.len() } else { 0 })
                    {
                        match dl_root::marker_def(
                            &ix,
                            m,
                            &c.st.lock(),
                            rg.as_deref().unwrap_or_default(),
                            &d.lits,
                            cf,
                            &mut dpoll,
                        ) {
                            Ok(def) => defs.push(def),
                            Err(()) => break,
                        }
                    }
                }
                if let Some(e) = stop.take() {
                    return Err(e);
                }
                if !ok {
                    return Err(Error::Memory {
                        limit: max_memory.unwrap_or(usize::MAX),
                        used: refused.unwrap_or(held),
                    });
                }
                let (trips, pr) =
                    dl_root::marker_trips(&ix, &cs, &d.lits, decline.filter(|_| !diag));
                if diag {
                    let t: Vec<String> = trips
                        .iter()
                        .map(|t| format!("{}/{}/{}", t[0], t[1], t[2]))
                        .collect();
                    let m = d
                        .mcount
                        .each_ref()
                        .map(|c| c.load(std::sync::atomic::Ordering::Relaxed));
                    let Some(def) = dl_root::marker_deficits(&ix, &cs, &defs, &d.lits, &mut dpoll)
                    else {
                        return Err(stop.take().unwrap_or(Error::Cancelled));
                    };
                    if let Some(e) = stop.take() {
                        return Err(e);
                    }
                    stats.dl_marker_deficits = def;
                    eprintln!(
                        "MARKER-DIAG work={} pr={}/{} tiers={} repair={} m={}/{}/{} def={}/{}/{} dwork={}",
                        stats.dl_work,
                        pr[0],
                        pr[1],
                        t.join(","),
                        d.mrepair,
                        m[0],
                        m[1],
                        m[2],
                        def[0],
                        def[1],
                        def[2],
                        dwork.get()
                    );
                }
                if let Some(t) = decline
                    && trips.get(t).is_some_and(|t| t[2] > 0)
                {
                    // run_with takes the flag first and starts again without the rooted
                    // rules, whatever the error
                    self.rooted_declined = true;
                    return Err(Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: totals.peak,
                    });
                }
            }
            if open.is_some() {
                // A partial answer has taken its record, and the next run starts again,
                // so the clause stores and the clause table go before its taxonomy.
                totals.peak = totals.peak.max(totals.memory + d.bytes());
                let base = self.ctx.len() as Ex;
                let dynamic_ctx = dynamic
                    .iter()
                    .flat_map(|dy| (0..dy.len() as Ex).map(move |i| dy.ctx(base + i)));
                for c in self
                    .ctx
                    .iter()
                    .filter_map(|c| c.get().map(|c| &**c))
                    .chain(dynamic_ctx)
                {
                    if let Some(x) = c.st.lock().extra.as_mut() {
                        x.dl = None;
                    }
                }
            } else {
                totals.memory += d.bytes();
                totals.peak = totals.peak.max(totals.memory);
            }
        }
        // the next run may keep the contexts of a run of Tier D, and with them the table
        // of the clauses they sent one another
        self.dl_run = dl_run.filter(|_| kept_now.is_some_and(|k| k.tier == KeptTier::Disj));
        if let Some(d) = &dynamic {
            totals.memory += d.bytes();
            totals.peak = totals.peak.max(totals.memory);
            if horn {
                stats.conjunctions = d.len();
            }
        }
        // Clear any unreachable kept orphan contexts before `dt_sat::collect` reads
        // every initialized context's last evaluation, so ghost `COVER`/`BOUND`/`UNCOUNTED`
        // statuses never leak into `excluded` (roadmap item 753).
        if stats.incremental && ix.dt.is_some() && open.is_none() {
            self.orphan_prune(
                &ix,
                dynamic.as_ref(),
                &roots,
                limits,
                max_memory,
                charge_fn,
                world_bytes,
                &mut totals,
            )?;
        }
        // the checker's covers and the answers it could not give, from each kept
        // context's last evaluation of a component and the worst of the others
        if let Some(dt) = ix.dt.as_deref() {
            let mut worst = Vec::new();
            dt_sat::collect(&ix, &self.ctx, dynamic.as_ref(), &mut worst);
            if worst.iter().any(|&w| w != dt_sat::FINE) {
                for &k in axioms {
                    let w = dt
                        .comps
                        .touched(k)
                        .into_iter()
                        .map(|c| worst[c as usize])
                        .max()
                        .unwrap_or(dt_sat::FINE);
                    if w != dt_sat::FINE {
                        self.excluded.push(Excluded {
                            axiom: k.clone(),
                            gap: Gap::Partial(dt_sat::reason(w)),
                        });
                    }
                }
                self.excluded.sort();
                self.excluded.dedup();
            }
        }
        self.dynamic = dynamic;
        stats.marking = marking_start.elapsed();
        stats.saturation = t.elapsed();
        stats.conclusions = totals.conclusions;
        totals.memory += world_bytes;
        totals.peak = totals.peak.max(totals.memory);
        // the saturated state is complete: a stop from here on keeps it for the next run. A
        // partial answer's is not, and the next run starts again.
        self.dirty = open.is_some();
        self.query_cache = std::sync::OnceLock::new();
        self.kept = kept_now;
        let ix_nominals = ix.nominals.clone();
        self.index = Some(ix);
        let t = Instant::now();
        let consistent = match &open {
            Some((_, c)) => *c != Consistency::Inconsistent,
            None => self.consistent(&ax, &ix_nominals, rooted),
        };
        if open.is_none() {
            self.consistent = consistent;
        }
        // The saturation charges fixed allowances for new contexts/conclusions;
        // reuse counts their retained tables instead. Reconcile once the workers
        // have stopped so both paths report the same retained-state estimate.
        let root_bytes = roots.capacity() * 4;
        let retained = self.retained_bytes(held_bytes, root_bytes, world_bytes);
        self.last_roots = if kept_now.is_some() {
            roots
        } else {
            Vec::new()
        };
        let held = retained + pending * std::mem::size_of::<saturate::Concl>();
        #[cfg(test)]
        if admission_test::observing() {
            admission_test::observe(admission_test::Attempt::Retained {
                actual: admission_test::queued(&self.ctx, self.dynamic.as_ref()),
                pending,
                bytes: retained,
                root_bytes,
                paid: totals.paid,
            });
        }
        drop(ax);
        Self::admit_retained(held, &mut totals, limits).inspect_err(|_| {
            if partial {
                self.partial_over = true;
                self.dirty = true;
            }
        })?;
        if !with_taxonomy {
            stats.memory = held;
            stats.peak_memory = totals.peak.max(held);
            stats.contexts = self.context_count();
            poll(limits)?;
            self.last_paid = totals.paid;
            return Ok(ControlFlow::Continue((consistent, None, stats)));
        }
        // a partial answer builds its taxonomy past the deadline that stopped it
        let stop = || open.is_none() && poll(limits).is_err();
        // Taxonomy asks again as its estimate grows. The observed saturation peak
        // may include a refusal, so only the paid peak offsets these requests.
        let paid = std::cell::Cell::new(totals.paid);
        let budget = Self::taxonomy_budget(held, &paid, limits.max_memory, charge_fn);
        let built = taxonomy::build(self, consistent, threads, open.is_some(), &stop, &budget);
        let (taxonomy, tax_bytes) = built.map_err(|h| match h {
            taxonomy::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
            taxonomy::Halt::Memory(bytes) => {
                // The requested partial contract also applies after saturation completed.
                if partial {
                    self.partial_over = true;
                    self.dirty = true;
                }
                Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: held + bytes,
                }
            }
        })?;
        stats.memory = held + tax_bytes;
        stats.taxonomy_memory = tax_bytes;
        stats.peak_memory = totals.peak.max(stats.memory).max(paid.get());
        stats.taxonomy = t.elapsed();
        stats.contexts = self.context_count();
        self.last_paid = paid.get();
        if let Some((incomplete, consistency)) = open {
            let mut classes: Vec<Class> = incomplete
                .into_iter()
                .filter_map(|e| match self.interner.nodes[e as usize] {
                    Node::Class(id) => Some(self.interner.classes[id as usize].clone()),
                    _ => None,
                })
                .filter(|k| !self.hide_internal || !k.as_str().starts_with(dl_reduce::PREFIX))
                .collect();
            classes.sort();
            self.partial = Some((classes, consistency));
            return Ok(ControlFlow::Continue((consistent, Some(taxonomy), stats)));
        }
        poll(limits)?;
        Ok(ControlFlow::Continue((consistent, Some(taxonomy), stats)))
    }

    /// The contexts saturated, those of the Horn rules' conjunctions included.
    fn context_count(&self) -> usize {
        self.ctx.iter().filter(|c| c.get().is_some()).count()
            + self.dynamic.as_ref().map_or(0, |d| d.len())
    }

    /// Whether the saturated contexts show a model: owl:Thing, the individuals and a rooted
    /// run's root are satisfiable.
    fn consistent(&self, ax: &Axioms, nominals: &[Ex], root: Option<Ex>) -> bool {
        let bottom = |e: Ex| {
            self.ctx
                .get(e as usize)
                .and_then(|c| c.get())
                .is_some_and(|c| c.st.lock().bottom)
        };
        !bottom(TOP)
            && !ax.individuals.iter().any(|&i| bottom(i))
            && !nominals.iter().any(|&i| bottom(i))
            && !root.is_some_and(bottom)
    }

    /// Exact live bytes of all distinct `Arc<CanonMap>` allocations currently owned by
    /// `self`, counting each distinct `Arc` allocation once even when shared across
    /// `interner.canon`, `canon_want`, and `merge.shared`.
    fn canon_live_bytes(&self) -> usize {
        let mut ptrs: [Option<*const index::CanonMap>; 3] = [None; 3];
        let mut n = 0usize;
        let mut sum = 0usize;
        for arc in [&self.interner.canon, &self.canon_want, &self.merge.shared]
            .into_iter()
            .flatten()
        {
            let p = std::sync::Arc::as_ptr(arc);
            if !ptrs[..n].contains(&Some(p)) {
                ptrs[n] = Some(p);
                n += 1;
                sum = sum.saturating_add(arc.owned_bytes());
            }
        }
        sum
    }

    /// Deterministic logical bytes of all distinct `Arc<CanonMap>` allocations owned by
    /// `self`.
    fn canon_logical_bytes(&self) -> usize {
        let mut ptrs: [Option<*const index::CanonMap>; 3] = [None; 3];
        let mut n = 0usize;
        let mut sum = 0usize;
        for arc in [&self.interner.canon, &self.canon_want, &self.merge.shared]
            .into_iter()
            .flatten()
        {
            let p = std::sync::Arc::as_ptr(arc);
            if !ptrs[..n].contains(&Some(p)) {
                ptrs[n] = Some(p);
                n += 1;
                sum = sum.saturating_add(arc.logical_bytes());
            }
        }
        sum
    }

    /// Merge/canonical-map/hub bytes live on `self` beyond what `self.interner.bytes()`
    /// already counts.
    fn merge_extra_bytes(&self) -> usize {
        self.merge
            .uf_bytes()
            .saturating_add(
                self.canon_live_bytes()
                    .saturating_sub(self.interner.canon_bytes()),
            )
            .saturating_add(self.active_hub.as_ref().map_or(0, |h| h.as_str().len()))
    }

    /// Retained bytes of the `self.worlds` map and its subsumer vectors.
    fn worlds_bytes(&self) -> usize {
        dl_clause::hash_table_bytes(
            self.worlds.capacity(),
            std::mem::size_of::<(Ex, (bool, Vec<Ex>))>(),
        )
        .saturating_add(
            self.worlds
                .values()
                .map(|(_, v)| v.capacity().saturating_mul(std::mem::size_of::<Ex>()))
                .sum::<usize>(),
        )
    }

    /// Live bytes held by `self` alongside `held_bytes` at the start of `dl_reduce::plan`.
    fn pre_plan_live_bytes(&self, held_bytes: usize) -> usize {
        let base = held_bytes
            .saturating_add(excluded_copy_bytes(&self.excluded))
            .saturating_add(self.worlds_bytes());
        if let Some(ix) = &self.index {
            base.saturating_add(self.index_bytes(ix))
                .saturating_add(self.context_bytes())
                .saturating_add(self.dynamic.as_ref().map_or(0, |d| d.retained_bytes()))
                .saturating_add(
                    self.dl_run
                        .as_ref()
                        .map_or(0, |d| d.table.bytes() + d.lits.bytes()),
                )
        } else {
            base.saturating_add(self.merge.uf_bytes())
                .saturating_add(self.canon_live_bytes())
                .saturating_add(self.active_hub.as_ref().map_or(0, |h| h.as_str().len()))
        }
    }

    fn index_bytes(&self, ix: &Index) -> usize {
        self.index_bytes_poll(ix, &mut |_| Ok::<(), std::convert::Infallible>(()))
            .unwrap_or_else(|e| match e {})
    }

    fn index_bytes_poll<E>(
        &self,
        ix: &Index,
        tick: &mut impl FnMut(usize) -> Result<(), E>,
    ) -> Result<usize, E> {
        // each class's IRI is held once, beside its hash and its entry in the table
        let mut total = self.interner.len() * 40
            + self.interner.classes.bytes()
            + self.interner.classes.name_bytes()
            + self.interner.data_bytes()
            + ix.numeric_ledger
                .as_ref()
                .or(self.interner.ranges.ledger.as_ref())
                .map_or(0, |l| l.bytes())
            + ix.bytes_poll(tick)?
            + self.interner.scope_bytes()
            + self.canon_live_bytes()
            + self.merge.uf_bytes()
            + self.active_hub.as_ref().map_or(0, |h| h.as_str().len())
            + self.disjoint.capacity() * 24;
        for d in &self.disjoint {
            tick(1)?;
            total += d.capacity() * 4;
        }
        Ok(total)
    }

    fn logical_index_bytes(&self, ix: &Index) -> usize {
        self.interner.len() * 40
            + self.interner.classes.logical_bytes()
            + self.interner.classes.name_bytes()
            + self.interner.logical_data_bytes()
            + ix.numeric_ledger
                .as_ref()
                .or(self.interner.ranges.ledger.as_ref())
                .map_or(0, |l| l.bytes())
            + ix.logical_bytes()
            + self.interner.logical_scope_bytes()
            + self.canon_logical_bytes()
            + self.merge.uf_logical_bytes()
            + self.disjoint.len() * 24
            + self.disjoint.iter().map(|d| d.len() * 4).sum::<usize>()
    }

    fn retained_bytes(&self, held_bytes: usize, root_bytes: usize, world_bytes: usize) -> usize {
        self.index_bytes(self.index.as_ref().expect("completed index"))
            + held_bytes
            + root_bytes
            + self.context_bytes()
            + self.dynamic.as_ref().map_or(0, |d| d.retained_bytes())
            + self
                .dl_run
                .as_ref()
                .map_or(0, |d| d.table.bytes() + d.lits.bytes())
            + world_bytes
    }

    fn admit_retained(
        held: usize,
        totals: &mut saturate::Totals,
        limits: &Limits,
    ) -> Result<(), Error> {
        totals.peak = totals.peak.max(held);
        let charge = limits
            .charge
            .as_deref()
            .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
        if !cover_memory(held, limits.max_memory, charge, &mut totals.paid) {
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: held,
            });
        }
        Ok(())
    }

    fn taxonomy_budget<'a>(
        held: usize,
        paid: &'a std::cell::Cell<usize>,
        max: Option<usize>,
        charge: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
    ) -> impl Fn(usize) -> bool + 'a {
        move |bytes| {
            let mut covered = paid.get();
            #[cfg(test)]
            admission_test::observe(admission_test::Attempt::Taxonomy {
                scratch: bytes,
                paid: covered,
            });
            if !cover_memory(held + bytes, max, charge, &mut covered) {
                return false;
            }
            paid.set(covered);
            true
        }
    }

    fn pending_messages(&self, dynamic: Option<&saturate::Dynamic>) -> usize {
        let made = dynamic
            .into_iter()
            .flat_map(|d| (0..d.len() as Ex).filter_map(move |i| d.get_ctx(d.base() + i)));
        self.ctx
            .iter()
            .filter_map(|c| c.get().map(Box::as_ref))
            .chain(made)
            .map(|c| c.todo.lock().len())
            .sum()
    }

    fn context_bytes(&self) -> usize {
        self.ctx.capacity() * std::mem::size_of::<std::sync::OnceLock<Box<saturate::Ctx>>>()
            + self
                .ctx
                .iter()
                .filter_map(|c| c.get())
                .map(|c| std::mem::size_of::<saturate::Ctx>() + c.st.lock().bytes())
                .sum::<usize>()
    }

    /// Whether a run with the changes `c` after the run `k` starts again with a new
    /// interner, by what the changed axioms are. The run decides its tier over the
    /// interner, which keeps the expressions of removed axioms, so a change that can
    /// move that decision starts again: with axioms outside OWL 2 EL in play, a change
    /// to an axiom that is not [`tier_neutral`]; after a run by the EL rules, a change
    /// to an axiom outside OWL 2 EL; and after a run with the Horn rules, such a change
    /// when an axiom that needs case analysis is in play, which Tier D may then take or
    /// leave. A run with the Horn rules also starts again once it keeps more than twice
    /// the expressions and conjunctions of the last run that did.
    fn must_restart(&self, k: &Kept, c: Changes<'_>) -> bool {
        let changed = || c.added.iter().chain(c.removed).copied();
        if changed().any(needs_full_run) {
            return true;
        }
        let outside: Vec<&AxiomKind> = changed()
            .filter(|a| logical(a) && el_break(a).is_some())
            .collect();
        let disj_in_play = k.disjunctive
            || outside
                .iter()
                .any(|a| !dl::shape(a).horn && dl_norm::may_accept(a));
        if disj_in_play && (self.stale_tier || changed().any(|a| logical(a) && !tier_neutral(a))) {
            return true;
        }
        let in_play = k.outside || !outside.is_empty();
        if in_play && changed().any(|a| logical(a) && !horn_neutral(a)) {
            return true;
        }
        match k.tier {
            KeptTier::El => {
                !outside.is_empty()
                    || ((self.index.as_ref().is_some_and(|ix| ix.dt.is_some())
                        || changed().any(mentions_data))
                        && self.interner.len() > 2 * k.nodes + KEPT_SLACK)
            }
            KeptTier::Horn => {
                !outside.is_empty() && disj_in_play
                    || self.interner.len() > 2 * k.nodes + KEPT_SLACK
                    || self.dynamic.as_ref().map_or(0, |d| d.len()) > 2 * k.dynamic + KEPT_SLACK
            }
            // Tier D takes or leaves each axiom by the axiom alone, and a run that finds
            // another tier once its index is built starts again
            KeptTier::Disj => {
                self.interner.len() > 2 * k.nodes + KEPT_SLACK
                    || self.dynamic.as_ref().map_or(0, |d| d.len()) > 2 * k.dynamic + KEPT_SLACK
                    || self.dl_run.as_ref().map_or(0, |d| d.table.len()) > 2 * k.table + KEPT_SLACK
            }
        }
    }

    /// Clears the contexts that hold an expression whose rules `changed` says differ
    /// from the last run's, and every context with a link to a cleared one, among the
    /// contexts of the index and those of conjunctions (roadmap item 753). A context's
    /// subsumers, links and Horn state follow from its conjunction, the rules of the
    /// expressions it holds and what the contexts it links to send back, so the others
    /// are as a run that starts again leaves them. They drop what they keep of cleared
    /// contexts as their predecessors, which register again when they are saturated,
    /// and number their disjointness sets by the new index. The answer is the number
    /// cleared.
    ///
    /// In a run of Tier D a context holds an expression when a unit, a marker, a denied
    /// test class, a registered restriction, its dynamic core, or a stored clause names
    /// it. The contexts above the index's are the tests and, under `Expansion::Eager`,
    /// the dynamic conjunction successors; only a test context (one whose core has a
    /// denied class) that held and is cleared clears its tested class, whose subsumer it
    /// gave. A kept context there need not be as a run that starts again leaves it: it
    /// keeps the markers that cleared predecessors sent, which are tautologies, and the
    /// clauses they gave. Each kept clause follows from the ontology as it is now, and the
    /// saturation closes every context under the rules, which is what the
    /// completeness of the calculus asks (Bate and others, JAIR 2018, Theorem 9, for
    /// any context structure to which no rule applies, with Theorem 8 for soundness).
    #[allow(clippy::too_many_arguments)]
    fn invalidate_changed(
        &mut self,
        changed: Vec<bool>,
        ix: &Index,
        dynamic: Option<&saturate::Dynamic>,
        dt_align: Option<&dt_sat::DtAlign>,
        last_roots: &[Ex],
        new_roots: &[Ex],
        renumbered: bool,
        limits: &Limits,
        live_base: usize,
        normalized_peak: &mut usize,
    ) -> Result<usize, Error> {
        let base = dynamic.map_or(self.ctx.len() as Ex, |d| d.base());
        let count = dynamic.map_or(0, |d| d.len() as Ex);
        let ctx = &self.ctx;
        let slots = ctx.len() + count as usize;
        let closure_peak = invalidation_scratch_peak(slots);
        let ctx_of = |e: Ex| -> Option<&saturate::Ctx> {
            if e < base {
                ctx.get(e as usize).and_then(|c| c.get()).map(|c| &**c)
            } else {
                dynamic.and_then(|d| d.get_ctx(e))
            }
        };
        let mut since_poll = 0usize;
        let mut tick = |work: usize| -> Result<(), Error> {
            since_poll = since_poll.saturating_add(work);
            if since_poll >= POLL_INVALIDATE {
                poll(limits)?;
                since_poll = 0;
            }
            Ok(())
        };
        let all = (0..ctx.len() as Ex).chain((0..count).map(|i| base + i));
        // Scope `changed` and `queue` to the invalidation closure so only `affected` stays
        // live during any `renumbered` disjoint-map growth scan and admission check.
        let (affected, affected_bytes) = {
            let mut affected: FxHashSet<Ex> = FxHashSet::default();
            let mut queue: Vec<Ex> = Vec::new();
            #[cfg(test)]
            admission_test::hit(admission_test::Point::InvalidateInitialStart);
            for e in all.clone() {
                tick(1)?;
                let Some(c) = ctx_of(e) else { continue };
                let st = c.st.lock();
                if !st.init {
                    continue;
                }
                let mut hit = false;
                for &s in &st.subs {
                    tick(1)?;
                    if changed.get(s as usize).copied().unwrap_or(true) {
                        hit = true;
                        break;
                    }
                }
                if !hit
                    && e >= base
                    && let Some(d) = dynamic
                {
                    let core = d.core(e);
                    #[cfg(test)]
                    if core.len() > POLL_INVALIDATE {
                        admission_test::hit(admission_test::Point::InvalidateLargeCoreStart);
                    }
                    let core_hit = dl_rules::core_changed_poll(core, &changed, &mut tick)?;
                    #[cfg(test)]
                    if core.len() > POLL_INVALIDATE {
                        admission_test::hit(admission_test::Point::InvalidateLargeCoreEnd);
                    }
                    if core_hit {
                        hit = true;
                    }
                }
                if !hit
                    && let Some(d) = dl_rules::dl(&st)
                    && d.names_changed_poll(&changed, &mut tick)?
                {
                    hit = true;
                }
                if !hit
                    && let Some(align) = dt_align
                    && let Some(x) = st.extra.as_ref()
                {
                    for s in &x.dt {
                        tick(1)?;
                        if align
                            .changed_comps
                            .get(s.comp as usize)
                            .copied()
                            .unwrap_or(true)
                        {
                            hit = true;
                            break;
                        }
                    }
                }
                if hit && affected.insert(e) {
                    push_capped(&mut queue, e, slots);
                }
            }
            // Clear dead roots that disappeared between runs so stale roots never pollute
            // `dt_sat::collect` or downstream scans.
            for &r in last_roots {
                tick(1)?;
                if (r as usize) < slots
                    && new_roots.binary_search(&r).is_err()
                    && ctx_of(r).is_some_and(|c| c.st.lock().init)
                    && affected.insert(r)
                {
                    push_capped(&mut queue, r, slots);
                }
            }
            #[cfg(test)]
            admission_test::hit(admission_test::Point::InvalidateInitialEnd);
            // `changed` is never read past the initial scan; free it before the BFS and any
            // `renumbered` disjoint-map growth check so only `affected` and `queue` stay live.
            drop(changed);
            // every context with a link into an affected context is affected, as is every
            // context that registered with one as its predecessor, which took its clauses
            // over the predecessor, and the class whose subsumer a test that held gave
            let tests = ix.dl.is_some();
            #[cfg(test)]
            admission_test::hit(admission_test::Point::InvalidateClosureStart);
            while let Some(e) = queue.pop() {
                tick(1)?;
                let Some(c) = ctx_of(e) else { continue };
                let st = c.st.lock();
                for (_, srcs) in &st.back {
                    #[cfg(test)]
                    if srcs.list.len() > POLL_INVALIDATE {
                        admission_test::hit(admission_test::Point::InvalidateBackListStart);
                    }
                    for &s in &srcs.list {
                        if affected.insert(s) {
                            push_capped(&mut queue, s, slots);
                        }
                        tick(1)?;
                    }
                    #[cfg(test)]
                    if srcs.list.len() > POLL_INVALIDATE {
                        admission_test::hit(admission_test::Point::InvalidateBackListEnd);
                    }
                }
                if let Some(d) = dl_rules::dl(&st) {
                    #[cfg(test)]
                    if d.dpreds.len() > POLL_INVALIDATE {
                        admission_test::hit(admission_test::Point::InvalidateDlPredsStart);
                    }
                    for &(u, _) in &d.dpreds {
                        if affected.insert(u) {
                            push_capped(&mut queue, u, slots);
                        }
                        tick(1)?;
                    }
                    #[cfg(test)]
                    if d.dpreds.len() > POLL_INVALIDATE {
                        admission_test::hit(admission_test::Point::InvalidateDlPredsEnd);
                    }
                }
                if tests
                    && e >= base
                    && st.bottom
                    && let Some(d) = dynamic
                    && let Some(x) = dl_rules::tested_class_poll(d.core(e), &mut tick)?
                    && affected.insert(x)
                {
                    push_capped(&mut queue, x, slots);
                }
            }
            #[cfg(test)]
            admission_test::hit(admission_test::Point::InvalidateClosureEnd);
            let affected_bytes =
                dl_clause::hash_table_bytes(affected.capacity(), std::mem::size_of::<Ex>());
            let queue_bytes = queue.capacity().saturating_mul(std::mem::size_of::<Ex>());
            debug_assert!(
                affected.len() <= slots && queue.capacity() <= slots,
                "invalidation scratch exceeded slot count {slots}: affected={}, queue_cap={}",
                affected.len(),
                queue.capacity()
            );
            debug_assert!(
                affected_bytes.saturating_add(queue_bytes) <= closure_peak,
                "invalidation scratch ({affected_bytes} + {queue_bytes}) exceeded bound {closure_peak}"
            );
            #[cfg(test)]
            admission_test::hit(admission_test::Point::InvalidateScratch {
                slots,
                affected_len: affected.len(),
                affected_bytes,
                queue_cap: queue.capacity(),
                queue_bytes,
                queue_ptr: queue.as_ptr().addr(),
                bound: closure_peak,
                live_base,
            });
            (affected, affected_bytes)
        };
        if renumbered {
            let mut maps = 0usize;
            for e in all.clone() {
                tick(1)?;
                if affected.contains(&e) {
                    continue;
                }
                let Some(c) = ctx_of(e) else { continue };
                let st = c.st.lock();
                if st.init && !st.disj.is_empty() {
                    maps = maps.saturating_add(st.disjoint_growth_poll(ix, &mut tick)?);
                }
            }
            if maps > 0 {
                let live_without_old = live_base.saturating_add(affected_bytes);
                let used = live_without_old.saturating_add(maps);
                let more = used.saturating_sub(*normalized_peak);
                #[cfg(test)]
                admission_test::hit(admission_test::Point::DisjointGrowthAdmit {
                    live: live_without_old,
                    growth: maps,
                    paid: *normalized_peak,
                });
                if limits.max_memory.is_some_and(|m| used > m)
                    || limits
                        .charge
                        .as_deref()
                        .is_some_and(|f| more > 0 && !f(more))
                {
                    return Err(Error::Memory {
                        limit: limits.max_memory.unwrap_or(used),
                        used,
                    });
                }
                *normalized_peak = (*normalized_peak).max(used);
            }
        }
        #[cfg(test)]
        admission_test::hit(admission_test::Point::InvalidateFinalStart);
        let mut cleared = 0;
        for e in all {
            tick(1)?;
            let Some(c) = ctx_of(e) else { continue };
            if affected.contains(&e) {
                if c.st.lock().init {
                    cleared += 1;
                }
                c.clear();
            } else {
                let mut st = c.st.lock();
                if st.init {
                    st.drop_predecessors_poll(&affected, &mut tick)?;
                    if renumbered && !st.disj.is_empty() {
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::DisjointReindex);
                        st.reindex_disjoint_poll(ix, &mut tick)?;
                        #[cfg(test)]
                        admission_test::hit(admission_test::Point::DisjointReindexEnd);
                    }
                    if let Some(align) = dt_align {
                        st.reindex_dt_poll(&align.comp_map, &mut tick)?;
                    }
                }
            }
        }
        #[cfg(test)]
        admission_test::hit(admission_test::Point::InvalidateFinalEnd);
        Ok(cleared)
    }

    /// Clears initialized contexts that are no longer reachable from `roots` after an
    /// incremental re-saturation, and removes cleared orphans from the predecessor sets
    /// of kept reachable contexts so a later re-connection replays cleanly.
    #[allow(clippy::too_many_arguments)]
    fn orphan_prune(
        &self,
        ix: &Index,
        dynamic: Option<&saturate::Dynamic>,
        roots: &[Ex],
        limits: &Limits,
        max_memory: Option<usize>,
        charge_fn: Option<&(dyn Fn(usize) -> bool + Sync)>,
        world_bytes: usize,
        totals: &mut saturate::Totals,
    ) -> Result<(), Error> {
        let base = dynamic.map_or(self.ctx.len() as Ex, |d| d.base());
        let count = dynamic.map_or(0, |d| d.len() as Ex);
        let total_slots = self.ctx.len() + count as usize;
        if total_slots == 0 {
            return Ok(());
        }
        let scratch = total_slots.div_ceil(64) * 8 + total_slots * std::mem::size_of::<Ex>();
        let used = totals
            .memory
            .saturating_add(world_bytes)
            .saturating_add(scratch);
        totals.peak = totals.peak.max(used);
        if !cover_memory(used, max_memory, charge_fn, &mut totals.paid) {
            return Err(Error::Memory {
                limit: max_memory.unwrap_or(usize::MAX),
                used,
            });
        }
        let get_ctx = |e: Ex| -> Option<&saturate::Ctx> {
            if e < base {
                self.ctx.get(e as usize).and_then(|c| c.get()).map(|c| &**c)
            } else {
                dynamic.and_then(|d| d.get_ctx(e))
            }
        };
        let mut reachable = Marks::new(total_slots);
        let mut queue: Vec<Ex> = Vec::with_capacity(total_slots);
        let mut ticks = 0usize;
        let mut tick = |work: usize| -> Result<(), Error> {
            ticks = ticks.saturating_add(work);
            if ticks >= POLL_INVALIDATE {
                ticks = 0;
                poll(limits)?;
            }
            Ok(())
        };
        for &r in roots {
            tick(1)?;
            if (r as usize) < total_slots && reachable.insert(r) {
                queue.push(r);
            }
        }
        while let Some(e) = queue.pop() {
            tick(1)?;
            let Some(c) = get_ctx(e) else { continue };
            let st = c.st.lock();
            if !st.init {
                continue;
            }
            for &d in &st.subs {
                tick(1)?;
                if matches!(ix.head(d).node, Node::Exist(..)) {
                    let succ = ix.head(d).succ;
                    if (succ as usize) < total_slots && reachable.insert(succ) {
                        queue.push(succ);
                    }
                }
            }
            for &(_, f) in &st.fwd.list {
                tick(1)?;
                if (f as usize) < total_slots && reachable.insert(f) {
                    queue.push(f);
                }
            }
            if let Some(x) = &st.extra {
                for &(_, n) in &x.horn.succ.list {
                    tick(1)?;
                    if (n as usize) < total_slots && reachable.insert(n) {
                        queue.push(n);
                    }
                }
            }
        }
        let all = (0..self.ctx.len() as Ex).chain((0..count).map(|i| base + i));
        let mut pruned = false;
        for e in all.clone() {
            tick(1)?;
            if !reachable.has(e)
                && let Some(c) = get_ctx(e)
                && c.st.lock().init
            {
                c.clear();
                pruned = true;
            }
        }
        if pruned {
            for e in all {
                tick(1)?;
                if reachable.has(e)
                    && let Some(c) = get_ctx(e)
                {
                    let mut st = c.st.lock();
                    if st.init {
                        st.drop_predecessors_where_poll(|p| !reachable.has(p), &mut tick)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Clears the contexts that the changed axioms can affect: those that derived the
    /// left side of a changed subsumption or part of it, and every context with a link
    /// to one of them. The parts take in the filler of each existential restriction, so
    /// a context with a link that a changed restriction can use links to a cleared
    /// context. The other contexts drop their links from cleared contexts. The answer
    /// is the number cleared.
    ///
    /// What a kept context takes from the index stays as it is (roadmap item 1256). Its
    /// propagations are the negative existential restrictions `∃s.d` of the index for
    /// each subsumer `d`. The index finds them among the left sides of the subsumptions,
    /// the members of the disjointness sets and the classes of keys, and a change to a
    /// key, a role axiom or a self restriction starts again ([`needs_full_run`]). So a restriction that an
    /// edit adds or removes has its filler `d` among the parts of the changed left sides
    /// or sets walked below, and every context that holds `d` is cleared. The
    /// disjointness sets of the index are numbered by their place among the run's sets,
    /// so `renumbered` says that they are not those of the last run, by which a kept
    /// context numbers what it holds. Then a kept context that holds a member of one
    /// numbers them again. One that holds none still holds none: a member of a new set
    /// is a part of a changed set, which clears the contexts that hold it.
    ///
    /// The search for the contexts that hold a changed part still reads every context,
    /// as does the pass that drops their links from cleared ones, since no table maps
    /// an expression to the contexts that hold it.
    fn invalidate(
        &mut self,
        changed: &Axioms,
        ix: &Index,
        renumbered: bool,
        budget: &mut Scratch<'_>,
    ) -> Result<usize, Error> {
        let nodes = &self.interner.nodes;
        // the two bit sets are in the budget's start, and the lists ask it as they grow
        let mut parts = Marks::new(nodes.len());
        let mut gone = Marks::new(self.ctx.len());
        let mut few: Vec<Ex> = Vec::new();
        let mut stack: Vec<Ex> = Vec::new();
        for &(a, _) in &changed.subs {
            budget.push(&mut stack, a)?;
        }
        for &e in changed.disjoint.iter().flatten() {
            budget.push(&mut stack, e)?;
        }
        while let Some(e) = stack.pop() {
            if !parts.insert(e) {
                continue;
            }
            budget.push(&mut few, e)?;
            match nodes[e as usize] {
                Node::Conj(a, b) => {
                    budget.push(&mut stack, a)?;
                    budget.push(&mut stack, b)?;
                }
                Node::Exist(_, f) => budget.push(&mut stack, f)?,
                _ => {}
            }
        }
        // the contexts that hold a part, then every context with a link into one
        let mut queue: Vec<Ex> = Vec::new();
        for (e, c) in self.ctx.iter().enumerate() {
            let Some(c) = c.get() else { continue };
            let st = c.st.lock();
            if st.init && st.subs.meets(&few, |s| parts.has(s)) {
                gone.insert(e as Ex);
                budget.push(&mut queue, e as Ex)?;
            }
        }
        let mut any = !queue.is_empty();
        while let Some(e) = queue.pop() {
            let Some(c) = self.ctx.get(e as usize).and_then(|c| c.get()) else {
                continue;
            };
            let st = c.st.lock();
            for (_, srcs) in &st.back {
                for &s in &srcs.list {
                    if gone.insert(s) {
                        any = true;
                        budget.push(&mut queue, s)?;
                    }
                }
            }
        }
        debug_assert!(
            (few.capacity() + stack.capacity() + queue.capacity()) * std::mem::size_of::<Ex>()
                <= budget.lists
                && parts.0.len() == nodes.len().div_ceil(64)
                && gone.0.len() == self.ctx.len().div_ceil(64),
            "the invalidation's scratch passed what the budget admitted"
        );
        #[cfg(test)]
        let live =
            (few.capacity() + stack.capacity() + queue.capacity()) * std::mem::size_of::<Ex>();
        drop((few, stack, queue, parts));
        if !any && !renumbered {
            #[cfg(test)]
            scratch_audit::record(budget, live, 0, 0);
            return Ok(0);
        }
        // the kept contexts that number their sets again may need larger maps, which
        // the budget admits before any context changes
        let mut maps = 0;
        if renumbered {
            for (e, c) in self.ctx.iter().enumerate() {
                let Some(c) = c.get() else { continue };
                let st = c.st.lock();
                if st.init && !gone.has(e as Ex) && !st.disj.is_empty() {
                    maps += st.disjoint_growth(ix);
                }
            }
            budget.grow(maps)?;
        }
        #[cfg(any(test, debug_assertions))]
        let mut grown = 0;
        let mut cleared = 0;
        for (e, c) in self.ctx.iter_mut().enumerate() {
            let Some(c) = c.get_mut() else { continue };
            let st = c.st.get_mut();
            if gone.has(e as Ex) {
                if st.init {
                    cleared += 1;
                }
                *st = saturate::State::default();
                *c.started.get_mut() = false;
            } else if st.init {
                if any {
                    st.drop_links_where(|s| gone.has(s));
                }
                if renumbered && !st.disj.is_empty() {
                    #[cfg(any(test, debug_assertions))]
                    let had = st.disj.capacity();
                    st.reindex_disjoint(ix);
                    #[cfg(any(test, debug_assertions))]
                    if st.disj.capacity() > had {
                        grown += saturate::table_bytes(st.disj.capacity(), 8);
                    }
                }
            }
        }
        #[cfg(any(test, debug_assertions))]
        debug_assert!(
            grown <= maps,
            "the disjointness maps grew past what the budget admitted"
        );
        #[cfg(test)]
        scratch_audit::record(budget, live, maps, grown);
        Ok(cleared)
    }

    /// The subsumers of a saturated context that are named classes, with their
    /// expressions.
    fn named_subsumers(&self, e: Ex) -> Option<(bool, Vec<Ex>)> {
        if let Some((bottom, subs)) = self.worlds.get(&e) {
            let named = subs
                .iter()
                .copied()
                .filter(|&s| matches!(self.interner.nodes[s as usize], Node::Class(_) | Node::Top))
                .collect();
            return Some((*bottom, named));
        }
        let c = self.ctx.get(e as usize)?.get()?;
        let st = c.st.lock();
        let named = st
            .subs
            .iter()
            .copied()
            .filter(|&s| matches!(self.interner.nodes[s as usize], Node::Class(_) | Node::Top))
            .collect();
        Some((st.bottom, named))
    }

    /// Whether a saturated context is marked unsatisfiable, without copying its row.
    fn is_bottom(&self, e: Ex) -> bool {
        if let Some((bottom, _)) = self.worlds.get(&e) {
            return *bottom;
        }
        self.ctx
            .get(e as usize)
            .and_then(|c| c.get())
            .is_some_and(|c| c.st.lock().bottom)
    }

    /// Whether `b` is explicitly recorded in `a`'s saturated subsumer row, without
    /// treating `bottom` as implying every class.
    fn raw_has_subsumer(&self, a: Ex, b: Ex) -> bool {
        if a == b {
            return true;
        }
        if let Some((_, subs)) = self.worlds.get(&a) {
            return subs.contains(&b);
        }
        self.ctx
            .get(a as usize)
            .and_then(|c| c.get())
            .is_some_and(|c| c.st.lock().subs.contains(&b))
    }

    /// Whether `e` is a visible class of the run's signature (`met` is set).
    fn is_visible_class(&self, e: Ex) -> bool {
        matches!(
            self.interner.nodes.get(e as usize),
            Some(Node::Class(id))
                if self.interner.met.get(*id as usize).copied().unwrap_or(false)
        )
    }

    /// Streams the named subsumers (`Node::Class` or `Node::Top`) of a saturated context
    /// without allocating an intermediate vector. Returns `Ok(None)` when `e` has no
    /// context, `Ok(Some(true))` when `e` is unsatisfiable (without calling `f`), or
    /// `Ok(Some(false))` after visiting every named subsumer.
    ///
    /// Callers must not lock any context inside `f`.
    fn visit_satisfiable_named_subsumers<E>(
        &self,
        e: Ex,
        mut f: impl FnMut(Ex) -> Result<(), E>,
    ) -> Result<Option<bool>, E> {
        if let Some((bottom, subs)) = self.worlds.get(&e) {
            if *bottom {
                return Ok(Some(true));
            }
            for &s in subs {
                if matches!(self.interner.nodes[s as usize], Node::Class(_) | Node::Top) {
                    f(s)?;
                }
            }
            return Ok(Some(false));
        }
        let Some(c) = self.ctx.get(e as usize).and_then(|c| c.get()) else {
            return Ok(None);
        };
        let st = c.st.lock();
        if st.bottom {
            return Ok(Some(true));
        }
        for &s in st.subs.iter() {
            if matches!(self.interner.nodes[s as usize], Node::Class(_) | Node::Top) {
                f(s)?;
            }
        }
        Ok(Some(false))
    }

    /// Whether `owl:Thing`'s own row is guaranteed upward closed, so [`Saturation::describe`]
    /// does not need to scan the rows of every class equivalent to `owl:Thing`.
    fn top_row_closed(&self) -> bool {
        self.worlds.is_empty()
            && !self.entangled
            && self
                .index
                .as_ref()
                .is_some_and(|ix| ix.horn.is_none() && ix.dl.is_none())
    }
}

/// What the last invalidation on a thread admitted and held, which the tests of its
/// admission read.
#[cfg(test)]
mod scratch_audit {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default)]
    pub(super) struct Audit {
        /// the estimate held after the invalidation, and the run's peak
        pub held: usize,
        pub peak: usize,
        /// the bytes admitted for the lists, and those the lists held
        pub lists: usize,
        pub live: usize,
        /// the bytes admitted for the disjointness maps, and those the maps took
        pub maps: usize,
        pub grown: usize,
        /// the bytes of the normalized premises of the changed axioms
        pub premises: usize,
    }

    thread_local! {
        static LAST: Cell<Option<Audit>> = const { Cell::new(None) };
    }

    pub(super) fn record(b: &super::Scratch<'_>, live: usize, maps: usize, grown: usize) {
        LAST.set(Some(Audit {
            held: b.held,
            peak: b.peak,
            lists: b.lists,
            live,
            maps,
            grown,
            premises: 0,
        }));
    }

    pub(super) fn premises(bytes: usize) {
        LAST.set(LAST.get().map(|a| Audit {
            premises: bytes,
            ..a
        }));
    }

    pub(super) fn take() -> Option<Audit> {
        LAST.take()
    }
}

/// The memory that the invalidation of a run kept by the EL rules holds beside the kept
/// contexts, the index and the normalized axioms, which it admits against the run's
/// limit and pool before each list of ids grows.
struct Scratch<'l> {
    limits: &'l Limits,
    /// the estimated bytes held, which grow and are never given back before the run's
    /// memory is reconciled
    held: usize,
    /// the run's peak estimate, past which a growth is charged to the pool
    peak: usize,
    /// the bytes admitted for the lists
    lists: usize,
}

impl Scratch<'_> {
    /// Admits `bytes` more, or stops the run with [`Error::Memory`].
    fn grow(&mut self, bytes: usize) -> Result<(), Error> {
        self.held += bytes;
        let more = self.held.saturating_sub(self.peak);
        if self.limits.max_memory.is_some_and(|m| self.held > m)
            || more > 0 && self.limits.charge.as_deref().is_some_and(|f| !f(more))
        {
            return Err(Error::Memory {
                limit: self.limits.max_memory.unwrap_or(self.held),
                used: self.held,
            });
        }
        self.peak = self.peak.max(self.held);
        Ok(())
    }

    /// Pushes `e` onto `v`, admitting the list's growth before it allocates.
    fn push(&mut self, v: &mut Vec<Ex>, e: Ex) -> Result<(), Error> {
        if v.len() == v.capacity() {
            let more = v.capacity().max(4);
            let bytes = more * std::mem::size_of::<Ex>();
            self.grow(bytes)?;
            self.lists += bytes;
            v.reserve_exact(more);
        }
        v.push(e);
        Ok(())
    }
}

/// Upper bound on peak live scratch bytes held by `affected: FxHashSet<Ex>` and the
/// slot-capped `queue: Vec<Ex>` during [`Saturation::invalidate_changed`], including
/// single-collection resize overlap (they never reallocate simultaneously).
fn invalidation_scratch_peak(slots: usize) -> usize {
    if slots == 0 {
        return 0;
    }
    let ex = std::mem::size_of::<Ex>();
    let affected_live = dl_clause::hash_table_bytes(slots, ex);
    let affected_peak = dl_clause::hash_table_grow_bound(0, 0, slots, ex).unwrap_or(usize::MAX);
    let queue_live = slots.saturating_mul(ex);
    let queue_peak = if slots <= 4 {
        queue_live
    } else {
        let prev = 1usize << (usize::BITS - 1 - (slots - 1).leading_zeros());
        prev.saturating_add(slots).saturating_mul(ex)
    };
    affected_peak
        .saturating_add(queue_live)
        .max(affected_live.saturating_add(queue_peak))
}

/// Pushes `e` onto `queue`, doubling capacity on growth up to `max_len` so the backing
/// buffer never overshoots the total number of context slots.
#[inline]
fn push_capped(queue: &mut Vec<Ex>, e: Ex, max_len: usize) {
    if queue.len() == queue.capacity() {
        let next = queue
            .capacity()
            .saturating_mul(2)
            .max(4)
            .min(max_len.max(queue.len().saturating_add(1)));
        queue.reserve_exact(next - queue.len());
    }
    queue.push(e);
}

/// A set of expressions or contexts by id, one bit each, which the invalidation reads
/// once for each member of every context it passes.
struct Marks(Vec<u64>);

impl Marks {
    fn new(n: usize) -> Marks {
        Marks(vec![0; n.div_ceil(64)])
    }

    /// Adds `e`, and answers whether it was new.
    fn insert(&mut self, e: Ex) -> bool {
        let (w, b) = (e as usize / 64, 1u64 << (e % 64));
        if w >= self.0.len() {
            self.0.resize(w + 1, 0);
        }
        let new = self.0[w] & b == 0;
        self.0[w] |= b;
        new
    }

    #[inline]
    fn has(&self, e: Ex) -> bool {
        self.0
            .get(e as usize / 64)
            .is_some_and(|w| w & (1u64 << (e % 64)) != 0)
    }
}

/// The axioms below which a run normalizes on one thread.
const PARALLEL_AXIOMS: usize = if cfg!(test) { 64 } else { 16_384 };

/// What the normalization of a run of axioms gives: the normalized axioms, the axioms used
/// in part, and the chains, which the range restriction checks once every axiom is read.
#[derive(Default)]
struct Normalized<'k> {
    ax: Axioms,
    partial: BTreeMap<&'k AxiomKind, &'static str>,
    chains: Vec<&'k AxiomKind>,
}

/// Normalizes a run of axioms that starts at `at` in the run's axioms into `it`. Axioms
/// outside OWL 2 EL give only their classes, and chains wait in `out.chains`.
/// A chunk of a parallel normalization tells `gauge` what it holds every
/// [`GAUGE_AXIOMS`] axioms and at its end, and stops with [`Error::Memory`] once the
/// chunks pass the budget.
#[allow(clippy::too_many_arguments)]
fn normalize_axioms<'k>(
    axioms: &[&'k AxiomKind],
    at: usize,
    is_outside: &[bool],
    it: &mut Interner,
    out: &mut Normalized<'k>,
    limits: &Limits,
    gauge: Option<&Gauge>,
) -> Result<(), Error> {
    let mut told = 0usize;
    let mut tell = |it: &Interner, out: &Normalized| -> Result<(), Error> {
        if let Some(g) = gauge {
            // the chunk's interner, and room for what its merge adds
            let now = it.bytes() + it.merge_bytes() + out.ax.rough_bytes();
            let grown = now.saturating_sub(told);
            told = told.max(now);
            g.tell(grown)?;
        }
        Ok(())
    };
    for (i, &k) in axioms.iter().enumerate() {
        if (at + i).is_multiple_of(POLL_AXIOMS) {
            poll(limits)?;
        }
        if i > 0 && i.is_multiple_of(GAUGE_AXIOMS) {
            tell(it, out)?;
        }
        if !logical(k) {
            continue;
        }
        if is_outside[at + i] {
            index::intern_signature(k, it);
            continue;
        }
        // a chain into the top property holds of every ontology, and its normalization
        // adds nothing, so it meets no range restriction (roadmap item 1282)
        if matches!(
            k,
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _)
        ) && !index::top_inclusion(k)
        {
            out.chains.push(k);
            continue;
        }
        if gauge.is_none()
            && let Some(b) = &it.ranges.budget
        {
            b.refresh_base(it.bytes() + out.ax.rough_bytes())?;
        }
        if let Some(why) = index::normalize(k, it, &mut out.ax) {
            out.partial.insert(k, why);
        }
        it.ranges.check()?;
        it.check_dt_stop(limits)?;
    }
    tell(it, out)
}

/// Runs the datatype component pre-pass under `budget`, pre-admitting non-reserved
/// datatype definitions and component scratch before allocation and propagating any
/// cancellation, deadline or memory halt immediately.
fn dt_comps(
    axioms: &[&AxiomKind],
    all_axioms: &[&AxiomKind],
    budget: &std::sync::Arc<numeric::Budget>,
    limits: &Limits,
) -> Result<
    (
        dtcheck::comp::Comps,
        dtcheck::range::Defs,
        numeric::ScratchGuard,
    ),
    Error,
> {
    let mut guard = budget.scratch_guard();
    let mut dt_defs: FxHashMap<Datatype, std::sync::Arc<DataRange>> = FxHashMap::default();
    for &k in all_axioms {
        if let AxiomKind::DatatypeDefinition(d, r) = k
            && !crate::vocab::is_reserved(d.as_str())
        {
            let map_growth = if dt_defs.len() == dt_defs.capacity() {
                dt_defs.capacity().max(8).saturating_add(8).saturating_mul(
                    std::mem::size_of::<(Datatype, std::sync::Arc<DataRange>)>() + 1,
                )
            } else {
                0
            };
            let est = map_growth
                .saturating_add(std::mem::size_of::<(Datatype, std::sync::Arc<DataRange>)>())
                .saturating_add(d.as_str().len())
                .saturating_add(dtcheck::range::data_range_bytes(r))
                .saturating_add(48);
            guard.grow(est)?;
            dt_defs.insert(d.clone(), std::sync::Arc::new(r.clone()));
        }
    }
    let dt_defs = dtcheck::range::Defs::new(dt_defs);
    let guard = std::cell::RefCell::new(guard);
    let mem_used = std::cell::Cell::new(0usize);
    let admit = |bytes: usize| -> bool {
        match guard.borrow_mut().grow(bytes) {
            Ok(()) => true,
            Err(Error::Memory { used, .. }) => {
                mem_used.set(used.max(1));
                false
            }
            Err(_) => false,
        }
    };
    let comps =
        match dtcheck::comp::Comps::of_bounded(axioms, &dt_defs, &|| poll(limits).is_err(), &admit)
        {
            Ok(c) => c,
            Err(dtcheck::comp::CompFail::Stopped) => {
                poll(limits)?;
                return Err(Error::Cancelled);
            }
            Err(dtcheck::comp::CompFail::Memory) => {
                return Err(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: mem_used.get().max(1),
                });
            }
        };
    Ok((comps, dt_defs, guard.into_inner()))
}

/// The axioms a chunk of a parallel normalization reads between two reports of what it
/// holds.
const GAUGE_AXIOMS: usize = if cfg!(test) { 16 } else { 4096 };

/// What the chunks of a parallel normalization hold, against the memory budget and the
/// pool. The pool is charged for each growth of the peak, from nothing, so the run
/// charges only what its state passes the peak by.
struct Gauge<'a> {
    limits: &'a Limits,
    numeric: Option<std::sync::Arc<numeric::Budget>>,
    /// the caller's bytes and the run's interner before the normalization
    base: usize,
    held: AtomicUsize,
    /// the peak of `base` and `held`, which the pool has been charged for
    peak: parking_lot::Mutex<usize>,
    /// set when a chunk passed the budget or the pool refused a grant
    over: std::sync::atomic::AtomicBool,
}

impl Gauge<'_> {
    /// Adds `grown` bytes, and answers [`Error::Memory`] once the chunks pass the
    /// budget, the pool refuses the growth, or another chunk found either. The numeric
    /// admission also polls the run, and its cancellation or deadline stays that error,
    /// since a one-shot cancellation must not turn into a serial retry.
    fn tell(&self, grown: usize) -> Result<(), Error> {
        let over = || {
            self.over.store(true, Ordering::Relaxed);
            Error::Memory {
                limit: self.limits.max_memory.unwrap_or(usize::MAX),
                used: self.base + self.held.load(Ordering::Relaxed),
            }
        };
        if self.over.load(Ordering::Relaxed) {
            return Err(over());
        }
        let used = self.base + self.held.fetch_add(grown, Ordering::Relaxed) + grown;
        if let Some(numeric) = &self.numeric {
            match numeric.grow_base(grown) {
                Ok(()) => {}
                Err(Error::Memory { .. }) => return Err(over()),
                Err(e) => return Err(e),
            }
            *self.peak.lock() = numeric.peak();
            return Ok(());
        }
        if self.limits.max_memory.is_some_and(|m| used > m) {
            return Err(over());
        }
        let mut peak = self.peak.lock();
        let more = used.saturating_sub(*peak);
        if more == 0 {
            return Ok(());
        }
        if self.limits.charge.as_ref().is_some_and(|f| !f(more)) {
            return Err(over());
        }
        *peak = used;
        Ok(())
    }
}

/// Steps between cooperative deadline and cancellation polls in the chain-range scan.
pub(super) const RANGE_POLL_STEP: usize = 1024;

/// Budget and cooperative polling interface for temporary scratch allocations.
pub(super) trait ScratchBudget {
    fn poll(&self) -> Result<(), Error>;
    fn grow(&mut self, more: usize) -> Result<(), Error>;
    fn release(&mut self, less: usize);
    fn overflow(&self) -> Error;
}

struct BuildScratch<'a> {
    guard: numeric::ScratchGuard,
    limits: &'a Limits,
}

impl ScratchBudget for BuildScratch<'_> {
    fn poll(&self) -> Result<(), Error> {
        poll(self.limits)
    }

    fn grow(&mut self, more: usize) -> Result<(), Error> {
        self.guard.grow(more)
    }

    fn release(&mut self, less: usize) {
        self.guard.release(less);
    }

    fn overflow(&self) -> Error {
        Error::Memory {
            limit: self.limits.max_memory.unwrap_or(usize::MAX),
            used: usize::MAX,
        }
    }
}

struct PostRunScratch<'a> {
    base: usize,
    peak: &'a mut usize,
    paid: &'a mut usize,
    limits: &'a Limits,
    charge: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
    check_stop: bool,
    scratch: usize,
}

impl ScratchBudget for PostRunScratch<'_> {
    fn poll(&self) -> Result<(), Error> {
        if self.check_stop {
            poll(self.limits)?;
        }
        Ok(())
    }

    fn grow(&mut self, more: usize) -> Result<(), Error> {
        if more == 0 {
            return Ok(());
        }
        self.poll()?;
        let limit = self.limits.max_memory.unwrap_or(usize::MAX);
        let next = self.scratch.checked_add(more).ok_or(Error::Memory {
            limit,
            used: usize::MAX,
        })?;
        let used = self.base.checked_add(next).ok_or(Error::Memory {
            limit,
            used: usize::MAX,
        })?;
        *self.peak = (*self.peak).max(used);
        if !cover_memory(used, self.limits.max_memory, self.charge, self.paid) {
            return Err(Error::Memory { limit, used });
        }
        self.scratch = next;
        Ok(())
    }

    fn release(&mut self, less: usize) {
        self.scratch = self.scratch.saturating_sub(less);
    }

    fn overflow(&self) -> Error {
        Error::Memory {
            limit: self.limits.max_memory.unwrap_or(usize::MAX),
            used: usize::MAX,
        }
    }
}

pub(super) struct ScopedScratch<'a, B: ScratchBudget + ?Sized> {
    inner: &'a mut B,
    bytes: usize,
}

impl<'a, B: ScratchBudget + ?Sized> ScopedScratch<'a, B> {
    pub(super) fn new(inner: &'a mut B) -> Self {
        Self { inner, bytes: 0 }
    }
}

struct CountingWriter<'a> {
    len: usize,
    steps: usize,
    limits: &'a Limits,
}

impl crate::ofn::PolledWrite<Error> for CountingWriter<'_> {
    fn push_str(&mut self, s: &str) -> Result<(), Error> {
        self.len = self.len.checked_add(s.len()).ok_or(Error::Memory {
            limit: self.limits.max_memory.unwrap_or(usize::MAX),
            used: usize::MAX,
        })?;
        Ok(())
    }

    fn tick(&mut self, work: usize) -> Result<(), Error> {
        self.steps = self.steps.saturating_add(work.max(1));
        if self.steps >= RANGE_POLL_STEP {
            self.steps = 0;
            poll(self.limits)?;
        }
        Ok(())
    }
}

struct PolledStringWriter<'a> {
    out: &'a mut String,
    steps: usize,
    limits: &'a Limits,
}

impl crate::ofn::PolledWrite<Error> for PolledStringWriter<'_> {
    fn push_str(&mut self, s: &str) -> Result<(), Error> {
        self.out.push_str(s);
        Ok(())
    }

    fn tick(&mut self, work: usize) -> Result<(), Error> {
        self.steps = self.steps.saturating_add(work.max(1));
        if self.steps >= RANGE_POLL_STEP {
            self.steps = 0;
            poll(self.limits)?;
        }
        Ok(())
    }
}

fn render_missing_range_polled(
    missing_ce: &ClassExpression,
    sup: &str,
    last: &str,
    w: &mut impl crate::ofn::PolledWrite<Error>,
) -> Result<(), Error> {
    w.write_str_polled("the range ")?;
    crate::ofn::write_default_class_expression_polled(missing_ce, w)?;
    w.write_str_polled(" of <")?;
    w.write_str_polled(sup)?;
    w.write_str_polled("> is not a range of the chain's last property <")?;
    w.write_str_polled(last)?;
    w.write_str_polled("> in OWL 2 EL")
}

impl<B: ScratchBudget + ?Sized> ScratchBudget for ScopedScratch<'_, B> {
    fn poll(&self) -> Result<(), Error> {
        self.inner.poll()
    }

    fn grow(&mut self, more: usize) -> Result<(), Error> {
        self.inner.grow(more)?;
        self.bytes = self.bytes.saturating_add(more);
        Ok(())
    }

    fn release(&mut self, less: usize) {
        let drop_bytes = less.min(self.bytes);
        self.bytes -= drop_bytes;
        self.inner.release(drop_bytes);
    }

    fn overflow(&self) -> Error {
        self.inner.overflow()
    }
}

impl<B: ScratchBudget + ?Sized> Drop for ScopedScratch<'_, B> {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.inner.release(self.bytes);
            self.bytes = 0;
        }
    }
}

pub(super) fn vec_bytes<T>(v: &Vec<T>) -> usize {
    v.capacity().saturating_mul(std::mem::size_of::<T>())
}

pub(super) fn vec_reserve<T>(
    v: &mut Vec<T>,
    extra: usize,
    budget: &mut impl ScratchBudget,
) -> Result<(), Error> {
    let need = v
        .len()
        .checked_add(extra)
        .ok_or_else(|| budget.overflow())?;
    if need <= v.capacity() {
        return Ok(());
    }
    let old_cap = v.capacity();
    let new_cap = old_cap
        .checked_mul(2)
        .ok_or_else(|| budget.overflow())?
        .max(need)
        .max(8);
    let old_bytes = old_cap
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| budget.overflow())?;
    let new_bytes = new_cap
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| budget.overflow())?;
    budget.grow(new_bytes)?;
    v.reserve_exact(new_cap - v.len());
    budget.release(old_bytes);
    Ok(())
}

pub(super) fn hash_table_bytes(cap: usize, entry_bytes: usize) -> Option<usize> {
    cap.checked_mul(entry_bytes)
        .and_then(|b| b.checked_add(b / 7))
}

fn set_bytes<K>(s: &FxHashSet<K>) -> usize {
    let entry_bytes = std::mem::size_of::<K>() + 1;
    hash_table_bytes(s.capacity(), entry_bytes).unwrap_or(usize::MAX)
}

pub(super) fn map_admit<K: Eq + std::hash::Hash, V>(
    m: &mut FxHashMap<K, V>,
    budget: &mut impl ScratchBudget,
) -> Result<(), Error> {
    if m.len() < m.capacity() {
        return Ok(());
    }
    let old_cap = m.capacity();
    let next_cap = old_cap
        .checked_mul(2)
        .ok_or_else(|| budget.overflow())?
        .max(14);
    let entry_bytes = std::mem::size_of::<(K, V)>() + 1;
    let old_bytes = hash_table_bytes(old_cap, entry_bytes).ok_or_else(|| budget.overflow())?;
    let new_bytes = hash_table_bytes(next_cap, entry_bytes).ok_or_else(|| budget.overflow())?;
    budget.grow(new_bytes)?;
    m.reserve(next_cap - m.len());
    budget.release(old_bytes);
    Ok(())
}

pub(super) fn set_admit<K: Eq + std::hash::Hash>(
    s: &mut FxHashSet<K>,
    budget: &mut impl ScratchBudget,
) -> Result<(), Error> {
    if s.len() < s.capacity() {
        return Ok(());
    }
    let old_cap = s.capacity();
    let next_cap = old_cap
        .checked_mul(2)
        .ok_or_else(|| budget.overflow())?
        .max(14);
    let entry_bytes = std::mem::size_of::<K>() + 1;
    let old_bytes = hash_table_bytes(old_cap, entry_bytes).ok_or_else(|| budget.overflow())?;
    let new_bytes = hash_table_bytes(next_cap, entry_bytes).ok_or_else(|| budget.overflow())?;
    budget.grow(new_bytes)?;
    s.reserve(next_cap - s.len());
    budget.release(old_bytes);
    Ok(())
}

/// In-place, zero-allocation heapsort on `(last, sup, idx)` entries that polls `budget`
/// every [`RANGE_POLL_STEP`] sift iterations.
fn sort_entries_polled<'a>(
    entries: &mut [(&'a str, &'a str, usize)],
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    let n = entries.len();
    if n <= 1 {
        return Ok(());
    }
    let cmp =
        |a: &(&str, &str, usize), b: &(&str, &str, usize)| a.0.cmp(b.0).then_with(|| a.1.cmp(b.1));
    let sift_down = |entries: &mut [(&'a str, &'a str, usize)],
                     mut root: usize,
                     end: usize,
                     steps: &mut usize|
     -> Result<(), Error> {
        loop {
            let left = 2 * root + 1;
            if left >= end {
                break;
            }
            *steps += 1;
            if (*steps).is_multiple_of(RANGE_POLL_STEP) {
                budget.poll()?;
            }
            let mut swap = root;
            if cmp(&entries[swap], &entries[left]) == std::cmp::Ordering::Less {
                swap = left;
            }
            let right = left + 1;
            if right < end && cmp(&entries[swap], &entries[right]) == std::cmp::Ordering::Less {
                swap = right;
            }
            if swap == root {
                break;
            }
            entries.swap(root, swap);
            root = swap;
        }
        Ok(())
    };
    let mut start = n / 2;
    while start > 0 {
        start -= 1;
        sift_down(entries, start, n, steps)?;
    }
    let mut end = n;
    while end > 1 {
        end -= 1;
        entries.swap(0, end);
        sift_down(entries, 0, end, steps)?;
    }
    Ok(())
}

enum Targets<'a> {
    One(&'a str),
    Many(Vec<&'a str>),
}

impl<'a> Targets<'a> {
    fn as_slice(&self) -> &[&'a str] {
        match self {
            Self::One(s) => std::slice::from_ref(s),
            Self::Many(v) => v.as_slice(),
        }
    }
}

enum DirectRanges<'a> {
    One(&'a ClassExpression),
    Many(Vec<&'a ClassExpression>),
}

impl<'a> DirectRanges<'a> {
    fn as_slice(&self) -> &[&'a ClassExpression] {
        match self {
            Self::One(c) => std::slice::from_ref(c),
            Self::Many(v) => v.as_slice(),
        }
    }
}

fn push_sup_edge<'a>(
    sup: &mut FxHashMap<&'a str, Targets<'a>>,
    a: &'a str,
    b: &'a str,
    budget: &mut impl ScratchBudget,
) -> Result<(), Error> {
    if let Some(t) = sup.get_mut(a) {
        match t {
            Targets::One(first) => {
                let first = *first;
                let mut v = Vec::new();
                vec_reserve(&mut v, 2, budget)?;
                v.push(first);
                v.push(b);
                *t = Targets::Many(v);
            }
            Targets::Many(v) => {
                vec_reserve(v, 1, budget)?;
                v.push(b);
            }
        }
    } else {
        map_admit(sup, budget)?;
        sup.insert(a, Targets::One(b));
    }
    Ok(())
}

fn push_direct_range<'a>(
    direct: &mut FxHashMap<&'a str, DirectRanges<'a>>,
    p: &'a str,
    c: &'a ClassExpression,
    budget: &mut impl ScratchBudget,
) -> Result<(), Error> {
    if let Some(r) = direct.get_mut(p) {
        match r {
            DirectRanges::One(first) => {
                let first = *first;
                let mut v = Vec::new();
                vec_reserve(&mut v, 2, budget)?;
                v.push(first);
                v.push(c);
                *r = DirectRanges::Many(v);
            }
            DirectRanges::Many(v) => {
                vec_reserve(v, 1, budget)?;
                v.push(c);
            }
        }
    } else {
        map_admit(direct, budget)?;
        direct.insert(p, DirectRanges::One(c));
    }
    Ok(())
}

type MissingRange<'a> = (&'a ClassExpression, &'a str, &'a str);

/// Checks OWL 2 Profiles §2.2.6 for each candidate chain in `chains`, resolving
/// inherited ranges only for properties reachable from candidate chains' superproperties
/// and last properties.
///
/// When `need_first` is true, each violating chain gets its lexicographically first
/// missing range expression (matching `BTreeSet` iteration order); when false, the
/// search stops at the first missing range encountered.
pub(super) fn check_chain_ranges<'a>(
    axioms: &[&'a AxiomKind],
    is_outside: impl Fn(usize, &'a AxiomKind) -> bool,
    chains: &[&'a AxiomKind],
    need_first: bool,
    budget: &mut impl ScratchBudget,
) -> Result<Vec<Option<MissingRange<'a>>>, Error> {
    budget.poll()?;
    let mut out: Vec<Option<MissingRange<'a>>> = Vec::new();
    vec_reserve(&mut out, chains.len(), budget)?;
    out.resize(chains.len(), None);

    let mut work = ScopedScratch::new(budget);

    // Collect only the (last, sup, chain_index) triples that can fail §2.2.6.
    let mut entries: Vec<(&'a str, &'a str, usize)> = Vec::new();
    for (idx, &k) in chains.iter().enumerate() {
        if idx.is_multiple_of(RANGE_POLL_STEP) {
            work.poll()?;
        }
        let AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(ps),
            ObjectPropertyExpression::Property(sup),
        ) = k
        else {
            continue;
        };
        let Some(last) = ps.last() else {
            continue;
        };
        vec_reserve(&mut entries, 1, &mut work)?;
        entries.push((last.named().as_str(), sup.as_str(), idx));
    }
    if entries.is_empty() {
        return Ok(out);
    }

    // Build the EL superproperty graph (linear in subproperty/equivalence axioms).
    let mut sup_edges: FxHashMap<&'a str, Targets<'a>> = FxHashMap::default();
    let mut has_any_range = false;
    for (i, &k) in axioms.iter().enumerate() {
        if i.is_multiple_of(RANGE_POLL_STEP) {
            work.poll()?;
        }
        if is_outside(i, k) {
            continue;
        }
        match k {
            AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ObjectPropertyExpression::Property(a)),
                ObjectPropertyExpression::Property(b),
            ) => {
                if a.as_str() != b.as_str() {
                    push_sup_edge(&mut sup_edges, a.as_str(), b.as_str(), &mut work)?;
                }
            }
            AxiomKind::EquivalentObjectProperties(ps) if ps.len() >= 2 => {
                let mut iter = ps.iter();
                if let Some(first) = iter.next() {
                    let first_s = first.named().as_str();
                    let mut prev_s = first_s;
                    for (j, cur) in iter.enumerate() {
                        if j.is_multiple_of(RANGE_POLL_STEP) {
                            work.poll()?;
                        }
                        let cur_s = cur.named().as_str();
                        if prev_s != cur_s {
                            push_sup_edge(&mut sup_edges, prev_s, cur_s, &mut work)?;
                        }
                        prev_s = cur_s;
                    }
                    if prev_s != first_s {
                        push_sup_edge(&mut sup_edges, prev_s, first_s, &mut work)?;
                    }
                }
            }
            AxiomKind::ObjectPropertyRange(ObjectPropertyExpression::Property(_), _) => {
                has_any_range = true;
            }
            _ => {}
        }
    }
    if !has_any_range {
        return Ok(out);
    }

    #[cfg(test)]
    if !need_first {
        admission_test::hit(admission_test::Point::OriginalRangeScan);
        work.poll()?;
    }

    // Resolve only properties reachable from candidate chains' `sup` or `last`.
    let mut needed: FxHashSet<&'a str> = FxHashSet::default();
    let mut stack: Vec<&'a str> = Vec::new();
    let mut steps = 0usize;
    for &(last, sup, _) in &entries {
        steps += 1;
        if steps.is_multiple_of(RANGE_POLL_STEP) {
            work.poll()?;
        }
        for seed in [last, sup] {
            if !needed.contains(seed) {
                set_admit(&mut needed, &mut work)?;
                needed.insert(seed);
                vec_reserve(&mut stack, 1, &mut work)?;
                stack.push(seed);
            }
        }
        while let Some(q) = stack.pop() {
            steps += 1;
            if steps.is_multiple_of(RANGE_POLL_STEP) {
                work.poll()?;
            }
            if let Some(targets) = sup_edges.get(q) {
                for &t in targets.as_slice() {
                    steps += 1;
                    if steps.is_multiple_of(RANGE_POLL_STEP) {
                        work.poll()?;
                    }
                    if !needed.contains(t) {
                        set_admit(&mut needed, &mut work)?;
                        needed.insert(t);
                        vec_reserve(&mut stack, 1, &mut work)?;
                        stack.push(t);
                    }
                }
            }
        }
    }

    // Collect direct EL ranges only for properties in `needed`, borrowing expressions.
    let mut direct: FxHashMap<&'a str, DirectRanges<'a>> = FxHashMap::default();
    for (i, &k) in axioms.iter().enumerate() {
        if i.is_multiple_of(RANGE_POLL_STEP) {
            work.poll()?;
        }
        if let AxiomKind::ObjectPropertyRange(ObjectPropertyExpression::Property(p), c) = k
            && needed.contains(p.as_str())
            && !is_outside(i, k)
        {
            push_direct_range(&mut direct, p.as_str(), c, &mut work)?;
        }
    }
    if direct.is_empty() {
        return Ok(out);
    }
    #[cfg(test)]
    admission_test::hit(admission_test::Point::OriginalRangeBeforeDropNeeded);
    let needed_bytes = set_bytes(&needed);
    drop(needed);
    work.release(needed_bytes);
    #[cfg(test)]
    admission_test::hit(admission_test::Point::OriginalRangeAfterDropNeeded);

    // Group candidate chains by `(last, sup)` so each `last` computes its inherited
    // ranges once and each `(last, sup)` pair is checked once without materializing a
    // full property x range table.
    #[cfg(test)]
    admission_test::hit(admission_test::Point::OriginalRangeSort);
    sort_entries_polled(&mut entries, &mut steps, &work)?;
    #[cfg(test)]
    admission_test::hit(admission_test::Point::OriginalRangeAfterSort);
    let mut last_supers: FxHashSet<&'a str> = FxHashSet::default();
    let mut have_ranges: polled::PolledSet<'a, ClassExpression> = polled::PolledSet::new();
    let mut sup_seen: FxHashSet<&'a str> = FxHashSet::default();

    let mut pos = 0;
    while pos < entries.len() {
        work.poll()?;
        let last = entries[pos].0;
        #[cfg(test)]
        admission_test::hit(admission_test::Point::OriginalRangeGroup);
        let mut last_end = pos + 1;
        while last_end < entries.len() && entries[last_end].0 == last {
            steps += 1;
            if steps.is_multiple_of(RANGE_POLL_STEP) {
                work.poll()?;
            }
            last_end += 1;
        }
        #[cfg(test)]
        admission_test::hit(admission_test::Point::OriginalRangeAfterGroup);

        last_supers.clear();
        have_ranges.clear();
        stack.clear();
        set_admit(&mut last_supers, &mut work)?;
        last_supers.insert(last);
        vec_reserve(&mut stack, 1, &mut work)?;
        stack.push(last);
        while let Some(q) = stack.pop() {
            steps += 1;
            if steps.is_multiple_of(RANGE_POLL_STEP) {
                work.poll()?;
            }
            if let Some(rs) = direct.get(q) {
                for &c in rs.as_slice() {
                    steps += 1;
                    if steps.is_multiple_of(RANGE_POLL_STEP) {
                        work.poll()?;
                    }
                    have_ranges.insert_polled(c, &mut steps, &mut work)?;
                }
            }
            if let Some(targets) = sup_edges.get(q) {
                for &t in targets.as_slice() {
                    steps += 1;
                    if steps.is_multiple_of(RANGE_POLL_STEP) {
                        work.poll()?;
                    }
                    if !last_supers.contains(t) {
                        set_admit(&mut last_supers, &mut work)?;
                        last_supers.insert(t);
                        vec_reserve(&mut stack, 1, &mut work)?;
                        stack.push(t);
                    }
                }
            }
        }

        let mut sub = pos;
        while sub < last_end {
            work.poll()?;
            let sup = entries[sub].1;
            #[cfg(test)]
            admission_test::hit(admission_test::Point::OriginalRangeSubGroup);
            let mut sub_end = sub + 1;
            while sub_end < last_end && entries[sub_end].1 == sup {
                steps += 1;
                if steps.is_multiple_of(RANGE_POLL_STEP) {
                    work.poll()?;
                }
                sub_end += 1;
            }
            #[cfg(test)]
            admission_test::hit(admission_test::Point::OriginalRangeAfterSubGroup);

            let mut missing: Option<&'a ClassExpression> = None;
            if !last_supers.contains(sup) {
                sup_seen.clear();
                stack.clear();
                set_admit(&mut sup_seen, &mut work)?;
                sup_seen.insert(sup);
                vec_reserve(&mut stack, 1, &mut work)?;
                stack.push(sup);
                'walk: while let Some(q) = stack.pop() {
                    steps += 1;
                    if steps.is_multiple_of(RANGE_POLL_STEP) {
                        work.poll()?;
                    }
                    if let Some(rs) = direct.get(q) {
                        for &c in rs.as_slice() {
                            steps += 1;
                            if steps.is_multiple_of(RANGE_POLL_STEP) {
                                work.poll()?;
                            }
                            if !have_ranges.contains_polled(c, &mut steps, &work)? {
                                if !need_first {
                                    missing = Some(c);
                                    break 'walk;
                                }
                                missing = Some(match missing {
                                    None => c,
                                    Some(m) => {
                                        if polled::cmp_ce_polled(c, m, &mut steps, &work)?
                                            == std::cmp::Ordering::Less
                                        {
                                            c
                                        } else {
                                            m
                                        }
                                    }
                                });
                            }
                        }
                    }
                    if let Some(targets) = sup_edges.get(q) {
                        for &t in targets.as_slice() {
                            steps += 1;
                            if steps.is_multiple_of(RANGE_POLL_STEP) {
                                work.poll()?;
                            }
                            if !last_supers.contains(t) && !sup_seen.contains(t) {
                                set_admit(&mut sup_seen, &mut work)?;
                                sup_seen.insert(t);
                                vec_reserve(&mut stack, 1, &mut work)?;
                                stack.push(t);
                            }
                        }
                    }
                }
            }

            if let Some(c) = missing {
                #[cfg(test)]
                admission_test::hit(admission_test::Point::OriginalRangeFill);
                for &(_, _, idx) in &entries[sub..sub_end] {
                    steps += 1;
                    if steps.is_multiple_of(RANGE_POLL_STEP) {
                        work.poll()?;
                    }
                    out[idx] = Some((c, sup, last));
                }
                #[cfg(test)]
                admission_test::hit(admission_test::Point::OriginalRangeAfterFill);
            }
            sub = sub_end;
        }

        pos = last_end;
    }

    work.poll()?;
    Ok(out)
}

/// The axioms normalized between two polls of the run's deadline and cancellation.
const POLL_AXIOMS: usize = 65_536;

/// The contexts or edges scanned between two polls of the run's deadline and cancellation
/// during index-diff invalidation and dynamic context rebasing.
const POLL_INVALIDATE: usize = 4096;

/// The run's deadline and cancellation, as an error when either has come.
/// Why the worlds stopped.
const WORLD_MEMORY: u8 = 1;
const WORLD_TIMEOUT: u8 = 2;
const WORLD_CANCEL: u8 = 3;

/// Whether the thread began unwinding after entry. A classifier called by a
/// destructor may start while its thread is already unwinding.
fn unwinds(already_panicking: bool) -> bool {
    !already_panicking && std::thread::panicking()
}

/// The bound the worlds of a run share. Their live bytes count alongside the
/// saturated state, and every report polls the deadline and cancellation.
struct WorldBudget<'a> {
    limits: &'a Limits,
    charge: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
    base: usize,
    held: AtomicIsize,
    peak: AtomicUsize,
    paid: AtomicUsize,
    growth: parking_lot::Mutex<()>,
    halt: AtomicU8,
    halted_at: AtomicUsize,
}

struct FoundWorldChunk<'r, 'a> {
    // Drop the rows and the vector allocation before returning their live bytes.
    rows: Vec<(Ex, world::Found, isize)>,
    debit: FoundWorldDebit<'r, 'a>,
}

struct FoundWorldDebit<'r, 'a> {
    budget: &'r WorldBudget<'a>,
    outstanding: isize,
}

impl FoundWorldDebit<'_, '_> {
    fn post(&mut self, bytes: isize, outstanding: isize) -> bool {
        let before = self.budget.post(bytes);
        self.outstanding = outstanding;
        #[cfg(test)]
        world_retirement_mixed_tests::row_posted(self.budget, bytes, before, outstanding);
        self.budget.check_post(bytes, before)
    }
}

impl Drop for FoundWorldDebit<'_, '_> {
    fn drop(&mut self) {
        self.budget.retire(self.outstanding);
    }
}

/// Declared after the growth guard so the stop is visible before a waiter enters.
/// The reporters still own their deltas and retire them during teardown.
struct WorldChargePanic<'r, 'a>(&'r WorldBudget<'a>, bool);

impl Drop for WorldChargePanic<'_, '_> {
    fn drop(&mut self) {
        if unwinds(self.1) {
            self.0.publish_stop(WORLD_CANCEL);
        }
    }
}

impl<'a> WorldBudget<'a> {
    fn start(
        limits: &'a Limits,
        charge: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
        base: usize,
        totals: &mut saturate::Totals,
    ) -> Result<Self, Error> {
        if charge.is_some() {
            Saturation::admit_retained(base, totals, limits)?;
        }
        Ok(Self {
            limits,
            charge,
            base,
            held: AtomicIsize::new(0),
            peak: AtomicUsize::new(totals.peak),
            paid: AtomicUsize::new(totals.paid),
            growth: parking_lot::Mutex::new(()),
            halt: AtomicU8::new(0),
            halted_at: AtomicUsize::new(0),
        })
    }

    fn finish(&self, totals: &mut saturate::Totals) {
        totals.peak = self.peak.load(Ordering::Relaxed);
        if self.charge.is_some() {
            totals.paid = self.paid.load(Ordering::Relaxed);
        }
    }

    /// Takes a change of what a world holds, and answers whether the worlds go on.
    fn tell(&self, delta: isize) -> bool {
        self.check_post(delta, self.post(delta))
    }

    /// Posts a signed change and returns the previous ledger value. A scoped owner can
    /// arm its inverse here, before any hook, admission or poll.
    fn post(&self, delta: isize) -> isize {
        self.held.fetch_add(delta, Ordering::Relaxed)
    }

    fn retire(&self, delta: isize) {
        if delta != 0 {
            self.held.fetch_sub(delta, Ordering::Relaxed);
        }
    }

    fn check_post(&self, delta: isize, previous: isize) -> bool {
        let now = previous + delta;
        #[cfg(test)]
        if delta > 0 {
            admission_test::hit(admission_test::Point::Observed);
        }
        if self.halt.load(Ordering::Relaxed) != 0 {
            return false;
        }
        let limits = self.limits;
        if delta > 0 {
            let used = self.base + now.max(0) as usize;
            self.peak.fetch_max(used, Ordering::Relaxed);
            if let Some(charge) = self.charge {
                if !self.admit_pooled(used, charge) {
                    return false;
                }
            } else if limits.max_memory.is_some_and(|m| used > m) {
                self.stop_for_memory(used);
                return false;
            }
        }
        let why = if limits.deadline.is_some_and(deadline_passed) {
            WORLD_TIMEOUT
        } else if limits.cancelled.as_ref().is_some_and(|f| f()) {
            WORLD_CANCEL
        } else {
            return true;
        };
        self.publish_stop(why);
        false
    }

    fn admit_pooled(&self, used: usize, charge: &(dyn Fn(usize) -> bool + Sync)) -> bool {
        let max = self.limits.max_memory;
        // The signed ledger is a single atomic aggregate. A delta already covered
        // by the paid mark needs no lock, unlike saturation's separate ledgers.
        if max.is_none_or(|m| used <= m) && used <= self.paid.load(Ordering::Acquire) {
            return self.halt.load(Ordering::Relaxed) == 0;
        }
        let already_panicking = std::thread::panicking();
        let _gate = lock_admission(&self.growth);
        if self.halt.load(Ordering::Relaxed) != 0 {
            return false;
        }
        let _panic = WorldChargePanic(self, already_panicking);
        let before = self.paid.load(Ordering::Relaxed);
        let mut paid = before;
        if !cover_memory(used, max, Some(charge), &mut paid) {
            self.stop_for_memory(used);
            return false;
        }
        if paid > before {
            self.paid.store(paid, Ordering::Release);
        }
        self.halt.load(Ordering::Relaxed) == 0
    }

    fn publish_stop(&self, why: u8) -> bool {
        self.halt
            .compare_exchange(0, why, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    fn stop_for_memory(&self, used: usize) {
        if self.publish_stop(WORLD_MEMORY) {
            // The driver reads this payload after the world workers have joined.
            self.halted_at.store(used, Ordering::Relaxed);
        }
    }

    /// The error of the worlds' stop, if they stopped.
    fn stopped(&self) -> Result<(), Error> {
        match self.halt.load(Ordering::Relaxed) {
            WORLD_MEMORY => Err(Error::Memory {
                limit: self.limits.max_memory.unwrap_or(usize::MAX),
                used: self.halted_at.load(Ordering::Relaxed),
            }),
            WORLD_TIMEOUT => Err(Error::Timeout),
            WORLD_CANCEL => Err(Error::Cancelled),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The conclusions that the last run of Tier D on this thread had derived when it
    /// stopped at its bound on them, so that a test sees how far past the bound it went.
    static PASSED_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Bytes that a test adds to the estimate of item 1231's scan, so that it can reach
    /// the memory budget and the pool's refusal.
    static SCAN_EXTRA: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// `(base, allowance_base, own)` observed by the last Horn/Tier D run on this thread.
    pub(super) static LAST_ALLOWANCE: std::cell::Cell<(usize, usize, usize)> =
        const { std::cell::Cell::new((0, 0, 0)) };
    /// Snapshots captured immediately after `saturate_counted` for each tracked rooted
    /// attempt in the last top-level classification on this thread.
    pub(super) static LAST_ADAPTIVE_ATTEMPTS: std::cell::RefCell<Vec<AdaptiveSnapshot>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Final public outcome recorded by `emit_adaptive_*_outcome` when diagnostics ran.
    pub(super) static LAST_ADAPTIVE_OUTCOME: std::cell::RefCell<Option<AdaptiveOutcome>> =
        const { std::cell::RefCell::new(None) };
    /// `(normalized_peak, pre_sat, more)` witnessed at the pre-box adaptive admission
    /// check in `Saturation::run`.
    pub(super) static LAST_ADAPTIVE_PREADMIT: std::cell::Cell<Option<(usize, usize, usize)>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn last_adaptive_attempts() -> Vec<AdaptiveSnapshot> {
    LAST_ADAPTIVE_ATTEMPTS.with(|c| c.borrow().clone())
}

#[cfg(test)]
pub(super) fn last_adaptive_outcome() -> Option<AdaptiveOutcome> {
    LAST_ADAPTIVE_OUTCOME.with(|c| *c.borrow())
}

#[cfg(test)]
pub(super) fn reset_adaptive_preadmit() {
    LAST_ADAPTIVE_PREADMIT.with(|c| c.set(None));
}

#[cfg(test)]
pub(super) fn last_adaptive_preadmit() -> Option<(usize, usize, usize)> {
    LAST_ADAPTIVE_PREADMIT.with(|c| c.get())
}

#[inline]
pub(super) fn deadline_passed(deadline: Instant) -> bool {
    #[cfg(test)]
    if admission_test::is_deadline_expired(deadline) {
        return true;
    }
    Instant::now() >= deadline
}

fn poll(limits: &Limits) -> Result<(), Error> {
    if limits.deadline.is_some_and(deadline_passed) {
        return Err(Error::Timeout);
    }
    if limits.cancelled.as_ref().is_some_and(|f| f()) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod admission_handoff_tests {
    use super::admission_test::{Attempt, PendingProbe, with_attempt};
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    const MESSAGES: usize = 4096;
    const MESSAGE_BYTES: usize = std::mem::size_of::<saturate::Concl>();
    const WORLD_AXIOMS: &str = "
        SubClassOf(:Witness ObjectOneOf(:w))
        SubClassOf(:Witness :Tag)
        SubClassOf(:Reaches ObjectSomeValuesFrom(:r :Witness))
        SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
    ";

    fn ontology(body: &str) -> Vec<Axiom> {
        crate::ofn_read::parse(&format!("Prefix(:=<urn:handoff:>) Ontology({body})"))
            .unwrap()
            .axioms
    }

    #[derive(Clone, Copy)]
    enum Refuse {
        Nothing,
        Retained(usize),
        Taxonomy { check: usize, cap: usize },
        Marker(usize),
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    enum Stage {
        #[default]
        Prefix,
        Probe,
        Retained,
        Taxonomy,
        Marker,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Request {
        stage: Stage,
        paid: usize,
        more: usize,
        accepted: bool,
    }

    #[derive(Clone, Default)]
    struct Trace {
        stage: Stage,
        paid: usize,
        cap: Option<usize>,
        refuse_next: bool,
        taxonomy_checks: usize,
        events: Vec<Attempt>,
        calls: Vec<Request>,
    }

    impl Trace {
        fn observe(&mut self, event: Attempt, refuse: Refuse) {
            self.events.push(event);
            match event {
                Attempt::Probe { .. } => {
                    self.stage = Stage::Probe;
                    self.refuse_next = true;
                }
                Attempt::Pending { .. } => self.stage = Stage::Prefix,
                Attempt::Retained { .. } => {
                    self.stage = Stage::Retained;
                    if let Refuse::Retained(cap) = refuse {
                        self.cap = Some(cap);
                    }
                }
                Attempt::Taxonomy { .. } => {
                    self.stage = Stage::Taxonomy;
                    if let Refuse::Taxonomy { check, cap } = refuse
                        && self.taxonomy_checks == check
                    {
                        self.cap = Some(cap);
                    }
                    self.taxonomy_checks += 1;
                }
                Attempt::Marker { .. } => {
                    self.stage = Stage::Marker;
                    if let Refuse::Marker(cap) = refuse {
                        self.cap = Some(cap);
                    }
                }
                _ => {}
            }
        }

        fn charge(&mut self, more: usize) -> bool {
            assert!(more > 0);
            let next = self.paid + more;
            let accepted =
                !std::mem::take(&mut self.refuse_next) && self.cap.is_none_or(|cap| next <= cap);
            self.calls.push(Request {
                stage: self.stage,
                paid: self.paid,
                more,
                accepted,
            });
            if accepted {
                self.paid = next;
            }
            accepted
        }

        fn first<T>(&self, select: impl Fn(Attempt) -> Option<T>) -> T {
            self.events
                .iter()
                .copied()
                .find_map(select)
                .unwrap_or_else(|| panic!("missing checkpoint in {:?}", self.events))
        }

        fn calls(&self, stage: Stage) -> impl Iterator<Item = Request> + '_ {
            self.calls.iter().copied().filter(move |r| r.stage == stage)
        }

        fn retained(&self) -> (usize, usize, usize, usize, usize) {
            self.first(|e| match e {
                Attempt::Retained {
                    actual,
                    pending,
                    bytes,
                    root_bytes,
                    paid,
                } => Some((actual, pending, bytes, root_bytes, paid)),
                _ => None,
            })
        }
    }

    type AttemptResult = Result<Option<(bool, Option<Taxonomy>, Stats)>, Error>;

    fn attempt(
        classifier: &mut Saturation,
        axioms: &[&AxiomKind],
        limits: &Limits,
        probe: Option<PendingProbe>,
        individuals: bool,
        refuse: Refuse,
    ) -> (AttemptResult, Trace) {
        let recorded = Arc::new(Mutex::new(Trace::default()));
        let charge = recorded.clone();
        let observer = recorded.clone();
        let limits = Limits {
            charge: Some(Arc::new(move |more| charge.lock().charge(more))),
            ..limits.clone()
        };
        let result = with_attempt(
            probe,
            individuals,
            move |e| observer.lock().observe(e, refuse),
            || {
                classifier
                    .run_once(
                        axioms,
                        None,
                        0,
                        &limits,
                        true,
                        AttemptProvenance::Ineligible,
                    )
                    .map(ControlFlow::continue_value)
            },
        );
        let trace = recorded.lock().clone();
        (result, trace)
    }

    fn assert_refused(
        result: AttemptResult,
        trace: &Trace,
        stage: Stage,
        paid: usize,
        used: usize,
    ) {
        let Err(error) = result else {
            panic!("{stage:?} admission should have refused {used} bytes");
        };
        // Marker admission uses the attempt ceiling. Later output stages
        // use the caller's configured ceiling.
        let limit = if stage == Stage::Marker {
            HORN_MEMORY_FLOOR
        } else {
            usize::MAX
        };
        assert_eq!(error, Error::Memory { limit, used }, "{stage:?} admission");
        assert!(used > paid);
        assert_eq!(
            trace.calls.last().copied(),
            Some(Request {
                stage,
                paid,
                more: used - paid,
                accepted: false,
            })
        );
        assert_eq!(trace.paid, paid);
    }

    #[test]
    fn partial_pending_messages_overlap_retained_output_and_taxonomy() {
        let axioms = ontology(
            "SubClassOf(:A ObjectUnionOf(:B :C))
             SubClassOf(:B :D)
             SubClassOf(:C :D)
             SubClassOf(:D :E)",
        );
        let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let limits = Limits {
            threads: 1,
            partial: true,
            ..Limits::default()
        };
        let run = |classifier: &mut Saturation, refuse| {
            attempt(
                classifier,
                &kinds,
                &limits,
                Some(PendingProbe {
                    messages: MESSAGES,
                    after_merge: false,
                }),
                false,
                refuse,
            )
        };
        let mut classifier = Saturation::new();
        let (accepted, calibration) = run(&mut classifier, Refuse::Nothing);
        let (consistent, taxonomy, stats) = accepted.unwrap().unwrap();
        assert!(classifier.dl_tried && !classifier.entangled);
        let (messages, peak, paid) = calibration.first(|e| match e {
            Attempt::Dependencies {
                messages,
                peak,
                paid,
            } => Some((messages, peak, paid)),
            _ => None,
        });
        let probe_paid = calibration.first(|e| match e {
            Attempt::Probe { paid } => Some(paid),
            _ => None,
        });
        assert!(peak > paid);
        assert_eq!(probe_paid, paid);
        assert!(messages >= MESSAGES);
        let pending = calibration.first(|e| match e {
            Attempt::Pending { actual, counted } => Some((actual, counted)),
            _ => None,
        });
        assert_eq!(pending, (messages, messages));
        let probe_calls: Vec<_> = calibration.calls(Stage::Probe).collect();
        assert_eq!(probe_calls.len(), 1);
        assert_eq!(probe_calls[0].paid, paid);
        assert!(!probe_calls[0].accepted && probe_calls[0].more > 0);

        let (actual, pending, base, _, retained_paid) = calibration.retained();
        assert_eq!((actual, pending, retained_paid), (messages, messages, paid));
        let queue = actual * MESSAGE_BYTES;
        let retained = base + queue;
        let cap = paid.max(base);
        assert!(cap < retained);
        let mut retained_run = Saturation::new();
        let (result, trace) = run(&mut retained_run, Refuse::Retained(cap));
        assert_eq!(trace.retained(), calibration.retained());
        assert_refused(result, &trace, Stage::Retained, paid, retained);
        assert!(retained_run.partial_over && retained_run.dirty);

        let (check, (scratch, paid, cap)) = calibration
            .events
            .iter()
            .filter_map(|e| match *e {
                Attempt::Taxonomy { scratch, paid } => Some((scratch, paid)),
                _ => None,
            })
            .enumerate()
            .find_map(|(check, (scratch, paid))| {
                let cap = paid.max(base + scratch);
                (cap < retained + scratch).then_some((check, (scratch, paid, cap)))
            })
            .expect("taxonomy grew beyond the paid overlap");
        let mut taxonomy_run = Saturation::new();
        let (result, trace) = run(&mut taxonomy_run, Refuse::Taxonomy { check, cap });
        assert_eq!(trace.retained(), calibration.retained());
        assert_refused(result, &trace, Stage::Taxonomy, paid, retained + scratch);
        assert_eq!(trace.taxonomy_checks, check + 1);
        assert!(taxonomy_run.partial_over && taxonomy_run.dirty);

        let partial = Classification::new(consistent, taxonomy, stats, classifier.partial.take());
        assert!(!partial.complete && !partial.incomplete.is_empty());
        assert!(partial.stats.memory <= calibration.paid);
        assert_eq!(
            partial.stats.memory,
            retained + partial.stats.taxonomy_memory
        );
        let full = Saturation::new().classify(&kinds, None, &limits).unwrap();
        assert!(full.complete);
        for sub in partial.taxonomy.classes() {
            for sup in partial.taxonomy.classes() {
                if partial.taxonomy.is_subclass(sub, sup) {
                    assert!(full.taxonomy.is_subclass(sub, sup), "{sub:?} ⊑ {sup:?}");
                }
            }
        }
        for mut stopped in [classifier, retained_run, taxonomy_run] {
            assert!(stopped.dirty);
            let recovered = stopped.classify(&kinds, None, &limits).unwrap();
            assert!(recovered.complete);
            assert!(!stopped.reused);
            assert_eq!(recovered.consistency, full.consistency);
            assert_eq!(recovered.taxonomy, full.taxonomy);
        }
    }

    #[test]
    fn partial_pending_messages_count_individual_marking_seeds_once() {
        let axioms = ontology(WORLD_AXIOMS);
        let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let limits = Limits {
            threads: 1,
            partial: true,
            rooted: true,
            ..Limits::default()
        };
        let mut classifier = Saturation::new();
        let (result, trace) = attempt(
            &mut classifier,
            &kinds,
            &limits,
            Some(PendingProbe {
                messages: MESSAGES,
                after_merge: true,
            }),
            true,
            Refuse::Nothing,
        );
        result.unwrap().unwrap();
        assert!(classifier.partial.is_some() && classifier.dl_rooted && classifier.entangled);
        let (before, after, seeds, counted) = trace.first(|e| match e {
            Attempt::Marking {
                before,
                after,
                seeds,
                counted,
            } => Some((before, after, seeds, counted)),
            _ => None,
        });
        assert!(before >= MESSAGES && seeds > 0);
        assert_eq!(after, before + seeds);
        assert_eq!(counted * MESSAGE_BYTES, after * MESSAGE_BYTES);
    }

    #[test]
    fn marker_scratch_overlaps_joined_world_rows() {
        const EXTRA: usize = 1 << 20;
        struct RestoreScanExtra(usize);
        impl Drop for RestoreScanExtra {
            fn drop(&mut self) {
                SCAN_EXTRA.with(|extra| extra.set(self.0));
            }
        }
        let _restore = RestoreScanExtra(SCAN_EXTRA.with(|extra| extra.replace(EXTRA)));
        let axioms = ontology(WORLD_AXIOMS);
        let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let limits = Limits {
            threads: 1,
            rooted: true,
            rooted_decline: true,
            ..Limits::default()
        };
        let run = |classifier: &mut Saturation, refuse| {
            attempt(classifier, &kinds, &limits, None, true, refuse)
        };
        let mut accepted = Saturation::new();
        let (result, calibration) = run(&mut accepted, Refuse::Nothing);
        let (_, _, stats) = result.unwrap().unwrap();
        assert!(accepted.dl_rooted && accepted.partial.is_none() && !accepted.dirty);
        let (base, live, rows, paid) = calibration.first(|e| match e {
            Attempt::Joined {
                base,
                live,
                rows,
                paid,
            } => Some((base, live, rows, paid)),
            _ => None,
        });
        assert!(rows > 0);
        assert_eq!(live, rows as isize);
        let entry = |trace: &Trace| {
            trace.first(|e| match e {
                Attempt::Marker {
                    base,
                    rows,
                    paid,
                    contexts,
                } => Some((base, rows, paid, contexts)),
                _ => None,
            })
        };
        let (marker_base, marker_rows, marker_paid, contexts) = entry(&calibration);
        assert_eq!((marker_base, marker_rows, marker_paid), (base, rows, paid));
        let scratch = EXTRA + contexts * std::mem::size_of::<dl_root::MarkerCtx>();
        let first_scan = |trace: &Trace| {
            trace.first(|e| match e {
                Attempt::Scan { scratch, paid } => Some((scratch, paid)),
                _ => None,
            })
        };
        assert_eq!(first_scan(&calibration), (scratch, paid));
        let cap = base + scratch;
        assert!(cap >= paid);

        let mut refused = Saturation::new();
        let (result, trace) = run(&mut refused, Refuse::Marker(cap));
        assert_eq!(entry(&trace), entry(&calibration));
        assert_eq!(first_scan(&trace), (scratch, paid));
        assert!(refused.dl_rooted && !refused.rooted_declined);
        let final_rows = |classifier: &Saturation| -> usize {
            classifier
                .worlds
                .values()
                .map(|(_, subs)| 48 + 4 * subs.len())
                .sum()
        };
        assert_eq!(final_rows(&refused), rows);
        assert_refused(result, &trace, Stage::Marker, paid, cap + rows);

        assert_eq!(
            calibration.calls(Stage::Marker).next(),
            Some(Request {
                stage: Stage::Marker,
                paid,
                more: cap + rows - paid,
                accepted: true,
            })
        );
        let marker_paid = calibration
            .calls(Stage::Marker)
            .map(|r| r.paid + r.more)
            .max()
            .unwrap();
        let (actual, pending, retained, root_bytes, retained_paid) = calibration.retained();
        assert_eq!((actual, pending, retained_paid), (0, 0, marker_paid));
        assert_eq!(final_rows(&accepted), rows);
        assert_eq!(retained, accepted.retained_bytes(0, root_bytes, 0) + rows);
        assert_eq!(stats.memory, retained + stats.taxonomy_memory);
        assert!(stats.memory < scratch);
        assert_eq!(calibration.paid, marker_paid);
        assert!(calibration.calls(Stage::Retained).next().is_none());
        assert!(calibration.calls(Stage::Taxonomy).next().is_none());
    }
}

#[cfg(test)]
mod tests {
    /// A check that sees Tier D's bound passed sets `dl_worked` before it publishes its
    /// stop, and the caller's cancellation, deadline or memory refusal can win the stop
    /// instead. The run then follows the stop that won: the partial answer the caller
    /// asked for, and no fallback past the bound.
    #[test]
    fn a_caller_stop_that_wins_over_a_passed_bound_keeps_the_partial_answer() {
        let doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)
             Ontology(
             ClassAssertion(ObjectUnionOf(:B :C) :a)
             ClassAssertion(ObjectComplementOf(:C) :a)
             SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
             SubClassOf(ObjectSomeValuesFrom(:r :B) :D)
             )",
        )
        .unwrap();
        for threads in [1, 4] {
            // The cancellation holds from its n-th poll on. The polls before the
            // saturation fail the run as cancelled, so the first n whose run answers is
            // a cancellation the saturation saw, and the answer is partial.
            let mut answered = false;
            for n in 0..64 {
                let mut k = Classifier::of(&doc.axioms);
                // the cancellation sets the losing check's flag as it is published, as
                // the check that saw the bound passed did before it lost the stop
                let worked = k.sat.dl_worked.clone();
                let polls = Arc::new(AtomicUsize::new(0));
                let p = polls.clone();
                let out = k.classify(&Limits {
                    threads,
                    rooted: true,
                    partial: true,
                    cancelled: Some(Arc::new(move || {
                        if p.fetch_add(1, Ordering::Relaxed) >= n {
                            worked.store(true, Ordering::Relaxed);
                            true
                        } else {
                            false
                        }
                    })),
                    ..Limits::default()
                });
                match out {
                    Err(Error::Cancelled) => continue,
                    Err(e) => panic!("{threads} threads, poll {n}: {e:?}"),
                    Ok(out) => {
                        assert!(
                            !out.complete_for_taken,
                            "{threads} threads, poll {n}: the saturation was not cancelled"
                        );
                        assert_eq!(out.consistency, Consistency::Unknown);
                        assert!(!out.stats.dl_rooted_over);
                        assert!(!out.stats.dl_over_budget);
                        assert!(!k.sat.dl_worked.load(Ordering::Relaxed));
                        answered = true;
                        break;
                    }
                }
            }
            assert!(
                answered,
                "{threads} threads: no poll reached the saturation"
            );
        }
    }

    /// A strict run refuses a partial answer, which has no exclusion to name.
    #[test]
    fn a_strict_run_refuses_a_partial_answer() {
        let doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)
             Ontology(
             ClassAssertion(ObjectUnionOf(:B :C) :a)
             ClassAssertion(ObjectComplementOf(:C) :a)
             SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
             SubClassOf(ObjectSomeValuesFrom(:r :B) :D)
             )",
        )
        .unwrap();
        for cover_later in [false, true] {
            let mut refused = false;
            for n in 0..64 {
                let mut k = Classifier::of(&doc.axioms);
                let polls = Arc::new(AtomicUsize::new(0));
                let p = polls.clone();
                let out = k.classify(&Limits {
                    threads: 1,
                    rooted: true,
                    partial: true,
                    strict: true,
                    cover_later,
                    cancelled: Some(Arc::new(move || p.fetch_add(1, Ordering::Relaxed) >= n)),
                    ..Limits::default()
                });
                match out {
                    Err(Error::Cancelled) => continue,
                    Err(Error::Incomplete(e)) => {
                        assert!(e.is_empty() && refusal(&e).contains("stopped"));
                        refused = true;
                        break;
                    }
                    Err(e) => panic!("poll {n}: {e:?}"),
                    Ok(out) => assert!(out.complete, "poll {n}: a strict run answered in part"),
                }
            }
            assert!(refused);
        }
    }

    fn ofn(body: &str) -> Vec<Axiom> {
        crate::ofn_read::parse(&format!(
            "Prefix(:=<http://e/#>)\nOntology(<http://e/o>\n{body}\n)"
        ))
        .unwrap()
        .axioms
    }

    /// A stream over `d`, as a classification of the terminology alone passes the
    /// assertions it did not read.
    fn stream_of(d: &[Axiom]) -> impl FnMut(&mut dyn FnMut(&AxiomKind) -> bool) -> bool + '_ {
        move |f: &mut dyn FnMut(&AxiomKind) -> bool| {
            for a in d {
                if !f(&a.kind) {
                    break;
                }
            }
            true
        }
    }

    /// Unmapped triples left out in two calls add up: the count and the list keep the
    /// first call's triples, and an empty call changes nothing.
    #[test]
    fn a_second_set_of_unmapped_triples_adds_to_the_first() {
        let t = ofn("Declaration(Class(:A)) Declaration(Class(:B)) SubClassOf(:A :B)");
        let kinds: Vec<&AxiomKind> = t.iter().map(|a| &a.kind).collect();
        let mut out = Saturation::new()
            .classify(&kinds, None, &Limits::default())
            .unwrap();
        assert!(out.complete);
        let triple = |s: &str| {
            oxrdf::Triple::new(
                oxrdf::NamedNode::new_unchecked(format!("http://ex.org/{s}")),
                oxrdf::NamedNode::new_unchecked(crate::vocab::RDFS_DOMAIN),
                oxrdf::NamedNode::new_unchecked("http://ex.org/A"),
            )
        };
        let one = |s: &str| Unmapped::default().extend([&triple(s)]);
        out.leave_out_unmapped(one("p"));
        out.leave_out_unmapped(Unmapped::default());
        out.leave_out_unmapped(one("q"));
        assert_eq!(out.stats.unmapped, 2);
        assert_eq!(out.unmapped.count, 2);
        assert_eq!(out.unmapped.triples, vec![triple("p"), triple("q")]);
        assert!(!out.unmapped.truncated && !out.complete);
        assert_eq!(out.consistency, Consistency::Unknown);
        // past the cap the count goes on and the list is truncated
        let many: Vec<oxrdf::Triple> = (0..UNMAPPED_LISTED)
            .map(|i| triple(&format!("r{i}")))
            .collect();
        out.leave_out_unmapped(Unmapped::default().extend(&many));
        assert_eq!(out.stats.unmapped, UNMAPPED_LISTED + 2);
        assert_eq!(out.unmapped.count, UNMAPPED_LISTED + 2);
        assert_eq!(out.unmapped.triples.len(), UNMAPPED_LISTED);
        assert_eq!(out.unmapped.triples[..2], [triple("p"), triple("q")]);
        assert!(out.unmapped.truncated);
    }

    /// Any dropped axiom with logical content leaves the answer incomplete, every class of
    /// the taxonomy incomplete and its consistency unknown, whether or not the axiom could
    /// change an answer. Declarations and annotations count for nothing.
    #[test]
    fn any_dropped_assertion_leaves_every_class_incomplete() {
        let t = ofn("Declaration(Class(:A)) Declaration(Class(:B)) SubClassOf(:A :B)");
        let kinds: Vec<&AxiomKind> = t.iter().map(|a| &a.kind).collect();
        let limits = Limits {
            cover_later: true,
            ..Limits::default()
        };
        for body in [
            // harmless for every answer, and still counted
            "ClassAssertion(:A :a)",
            // names a class the taxonomy lacks, which no surface then lists
            "ClassAssertion(:Z :a)",
            "Declaration(NamedIndividual(:a)) ObjectPropertyAssertion(:p :a :b)",
        ] {
            for coverage in [true, false] {
                let d = ofn(body);
                let mut s = Saturation::new();
                let limits = Limits {
                    coverage,
                    ..limits.clone()
                };
                let mut out = s.classify(&kinds, None, &limits).unwrap();
                let w = out.cover(&kinds, s.excluded(), Some(&mut stream_of(&d)), &limits);
                let w = w.expect(body);
                assert!(!matches!(w.axiom, AxiomKind::Declaration(_)), "{body}");
                assert!(
                    matches!(&w.gap, Gap::Outside { rule, .. } if rule == NOT_READ),
                    "{body}"
                );
                assert!(!out.complete && out.complete_for_taken, "{body}");
                assert_eq!(out.consistency, Consistency::Unknown, "{body}");
                let names: Vec<&str> = out.incomplete.iter().map(|c| c.as_str()).collect();
                assert_eq!(names, ["http://e/#A", "http://e/#B"], "{body}");
            }
        }
        for body in [
            "Declaration(NamedIndividual(:a))",
            "AnnotationAssertion(rdfs:label :A \"a\")",
            "",
        ] {
            let d = ofn(body);
            let mut s = Saturation::new();
            let mut out = s.classify(&kinds, None, &limits).unwrap();
            let w = out.cover(&kinds, s.excluded(), Some(&mut stream_of(&d)), &limits);
            assert!(w.is_none() && out.complete, "{body}");
            assert_eq!(out.consistency, Consistency::Consistent, "{body}");
            assert!(out.incomplete.is_empty(), "{body}");
        }
    }

    /// An inconsistent terminology stays inconsistent beside a dropped assertion, which
    /// still leaves the answer incomplete: a class only the assertion names is in no
    /// taxonomy.
    #[test]
    fn an_inconsistent_run_with_a_dropped_assertion_is_not_complete() {
        let t = ofn("SubClassOf(owl:Thing :A)
                     SubClassOf(owl:Thing :B)
                     DisjointClasses(:A :B)");
        let kinds: Vec<&AxiomKind> = t.iter().map(|a| &a.kind).collect();
        let limits = Limits {
            cover_later: true,
            ..Limits::default()
        };
        for body in ["ClassAssertion(:C :a)", "ClassAssertion(:A :a)"] {
            let d = ofn(body);
            let mut s = Saturation::new();
            let mut out = s.classify(&kinds, None, &limits).unwrap();
            assert_eq!(out.consistency, Consistency::Inconsistent);
            let w = out.cover(&kinds, s.excluded(), Some(&mut stream_of(&d)), &limits);
            assert!(w.is_some() && !out.complete, "{body}");
            assert_eq!(out.consistency, Consistency::Inconsistent, "{body}");
            assert!(!out.incomplete.is_empty(), "{body}");
        }
    }

    /// When a class is incomplete, the node of the unsatisfiable classes may miss one, so
    /// no unsatisfiable class is complete.
    #[test]
    fn an_incomplete_class_leaves_the_unsatisfiable_node_incomplete() {
        let t = ofn("SubClassOf(:A owl:Nothing)
                     SubClassOf(:B ObjectUnionOf(:C :D))
                     SubClassOf(:C owl:Nothing)
                     SubClassOf(:D owl:Nothing)");
        let kinds: Vec<&AxiomKind> = t.iter().map(|a| &a.kind).collect();
        let mut s = Saturation::new();
        let out = s
            .classify(
                &kinds,
                None,
                &Limits {
                    el_only: true,
                    ..Limits::default()
                },
            )
            .unwrap();
        let names: Vec<&str> = out.incomplete.iter().map(|c| c.as_str()).collect();
        assert!(names.contains(&"http://e/#B"), "{names:?}");
        assert!(names.contains(&"http://e/#A"), "{names:?}");
    }

    /// The read of the dropped axioms asks the run's deadline, and one that passed stops
    /// it with every class incomplete.
    #[test]
    fn a_read_of_dropped_axioms_past_the_deadline_stops() {
        let t = ofn("SubClassOf(:A :B)");
        let mut body = String::new();
        for i in 0..10_000 {
            body += &format!("Declaration(NamedIndividual(:x{i}))\n");
        }
        let d = ofn(&body);
        let kinds: Vec<&AxiomKind> = t.iter().map(|a| &a.kind).collect();
        let mut s = Saturation::new();
        let mut limits = Limits {
            cover_later: true,
            ..Limits::default()
        };
        let mut out = s.classify(&kinds, None, &limits).unwrap();
        limits.deadline = Some(Instant::now());
        let w = out.cover(&kinds, s.excluded(), Some(&mut stream_of(&d)), &limits);
        assert!(w.is_none() && out.stats.coverage_stopped && !out.complete);
        assert_eq!(out.consistency, Consistency::Unknown);
    }

    /// A stream of dropped axioms that cannot finish leaves the answer incomplete, even
    /// for a run that takes no modules' pass.
    #[test]
    fn a_dropped_stream_that_fails_leaves_every_class_incomplete() {
        let t = ofn("SubClassOf(:A :B)");
        let kinds: Vec<&AxiomKind> = t.iter().map(|a| &a.kind).collect();
        for coverage in [true, false] {
            let mut s = Saturation::new();
            let limits = Limits {
                cover_later: true,
                coverage,
                ..Limits::default()
            };
            let mut out = s.classify(&kinds, None, &limits).unwrap();
            let mut stream = |_: &mut dyn FnMut(&AxiomKind) -> bool| false;
            out.cover(&kinds, s.excluded(), Some(&mut stream), &limits);
            assert!(
                out.stats.coverage_stopped && !out.complete,
                "coverage {coverage}"
            );
            assert_eq!(out.consistency, Consistency::Unknown);
            assert!(!out.incomplete.is_empty());
        }
    }

    /// An ontology whose saturation with counting takes more contexts than two steps of a
    /// run in steps. Each `C` has two successors that a maximum merges, a union on the
    /// merged successor gives `P`, every seventh `C` has disjoint fillers, and a filler
    /// reaches the next `C` in one case of a union.
    fn many_contexts(n: usize) -> Vec<Axiom> {
        let mut text = String::from("Prefix(:=<http://ex.org/>)\nOntology(\n");
        for i in 0..n {
            text += &format!(
                "SubClassOf(:C{i} ObjectSomeValuesFrom(:r :D{i}))
                 SubClassOf(:C{i} ObjectSomeValuesFrom(:r :E{i}))
                 SubClassOf(:C{i} ObjectMaxCardinality(1 :r))
                 SubClassOf(:D{i} ObjectUnionOf(:F{i} :G{i}))
                 SubClassOf(ObjectIntersectionOf(:F{i} :E{i}) :H{i})
                 SubClassOf(:G{i} :H{i})
                 SubClassOf(ObjectSomeValuesFrom(:r :H{i}) :P{i})
                 SubClassOf(:E{i} ObjectUnionOf(:Q{i} ObjectSomeValuesFrom(:r :C{})))\n",
                (i + 1) % n
            );
            if i % 7 == 0 {
                text += &format!("DisjointClasses(:D{i} :E{i})\n");
            }
        }
        text += ")";
        crate::ofn_read::parse(&text).unwrap().axioms
    }

    /// Roadmap item 727: a run in steps over more contexts than one step takes, where
    /// under test the workers share every step of two contexts and every delivery, derives
    /// the same conclusions and the same answer on one, three and eight threads, and the
    /// answer is that of a run without steps. A run that a cancellation stops fails or answers in
    /// part, and the next run of that classifier gives the outcome of a new one.
    #[test]
    fn a_run_in_steps_over_many_steps_derives_the_same_on_every_number_of_threads() {
        let n = 700;
        let axioms = many_contexts(n);
        let class = |s: String| Class::new(format!("http://ex.org/{s}"));
        let limits = |threads: usize, dl_steps: bool| Limits {
            threads,
            counting: true,
            dl_steps,
            ..Limits::default()
        };
        // the answer on the classes the test reads, and what Tier D derived
        let outcome = |out: &Classification| {
            let mut got = Vec::new();
            for i in 0..n {
                got.push((
                    out.taxonomy
                        .is_subclass(&class(format!("C{i}")), &class(format!("P{i}"))),
                    out.taxonomy
                        .is_subclass(&class(format!("C{i}")), &Class::nothing()),
                    out.taxonomy
                        .is_subclass(&class(format!("D{i}")), &class(format!("H{i}"))),
                ));
            }
            (
                got,
                out.stats.dl_work,
                out.stats.dl_stored,
                out.stats.dl_over_budget,
            )
        };
        let first = {
            let out = Classifier::of(&axioms).classify(&limits(1, true)).unwrap();
            assert!(out.complete && !out.stats.dl_over_budget);
            assert!(out.stats.contexts > 2 * saturate::STEP_CONTEXTS);
            outcome(&out)
        };
        for (i, &(p, unsat, h)) in first.0.iter().enumerate() {
            assert_eq!((p, unsat, h), (true, i % 7 == 0, false), "C{i}");
        }
        for threads in [3, 8] {
            let out = Classifier::of(&axioms)
                .classify(&limits(threads, true))
                .unwrap();
            assert_eq!(outcome(&out), first, "{threads} threads");
        }
        let default = Classifier::of(&axioms).classify(&limits(4, false)).unwrap();
        assert_eq!(outcome(&default).0, first.0);
        // a cancellation at its n-th poll, then the same classifier again
        for threads in [1, 8] {
            for at in [0, 3, 10, 30, 100, 300] {
                let mut k = Classifier::of(&axioms);
                let polls = Arc::new(AtomicUsize::new(0));
                let p = polls.clone();
                let stopped = k.classify(&Limits {
                    partial: true,
                    cancelled: Some(Arc::new(move || p.fetch_add(1, Ordering::Relaxed) >= at)),
                    ..limits(threads, true)
                });
                match stopped {
                    Err(Error::Cancelled) => {}
                    Err(e) => panic!("{threads} threads, poll {at}: {e:?}"),
                    Ok(out) if out.complete_for_taken => assert_eq!(outcome(&out), first),
                    Ok(out) => {
                        for (i, &(p, unsat, h)) in outcome(&out).0.iter().enumerate() {
                            assert!(p <= first.0[i].0 && unsat <= first.0[i].1 && !h);
                        }
                    }
                }
                let out = k.classify(&limits(threads, true)).unwrap();
                assert_eq!(outcome(&out), first, "{threads} threads after poll {at}");
            }
        }
    }

    /// Roadmap item 727: a run that counts, which has a bound on its derived conclusions,
    /// proceeds in steps by default, and `dl_steps: false` gives it the driver without
    /// steps. A run without counting or the root has no bound and takes the driver
    /// without steps whatever the option says. The poll of a cancellation sees which
    /// driver calls it. Both drivers give a run that counts the same answer.
    #[test]
    fn a_run_with_a_bound_proceeds_in_steps_by_default_and_false_turns_them_off() {
        use std::cell::Cell;
        assert!(Limits::default().dl_steps);
        let n = 700;
        let axioms = many_contexts(n);
        let class = |s: String| Class::new(format!("http://ex.org/{s}"));
        // the drivers whose workers polled, as (in steps, without steps), and the answer
        let run = |limits: Limits| {
            let seen = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
            let s = seen.clone();
            let out = Classifier::of(&axioms)
                .classify(&Limits {
                    threads: 4,
                    cancelled: Some(Arc::new(move || {
                        match saturate::WORKER.with(Cell::get) {
                            saturate::STEP_CALLER | saturate::STEP_HELPER => {
                                s[0].store(true, Ordering::Relaxed);
                            }
                            saturate::FIRST_WORKER | saturate::HELPER => {
                                s[1].store(true, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                        false
                    })),
                    ..limits
                })
                .unwrap();
            assert!(out.complete_for_taken && !out.stats.dl_over_budget);
            let answer: Vec<bool> = (0..n)
                .map(|i| {
                    out.taxonomy
                        .is_subclass(&class(format!("C{i}")), &class(format!("P{i}")))
                })
                .collect();
            (
                seen[0].load(Ordering::Relaxed),
                seen[1].load(Ordering::Relaxed),
                answer,
            )
        };
        let counting = Limits {
            counting: true,
            ..Limits::default()
        };
        let (steps, without, by_default) = run(counting.clone());
        assert!(steps && !without);
        assert!(by_default.iter().all(|&p| p));
        let (steps, without, off) = run(Limits {
            dl_steps: false,
            ..counting
        });
        assert!(!steps && without);
        assert_eq!(off, by_default);
        // without counting the maximum is left out, so no `C` is below its `P`
        for dl_steps in [true, false] {
            let (steps, without, answer) = run(Limits {
                dl_steps,
                ..Limits::default()
            });
            assert!(!steps && without, "steps {dl_steps}");
            assert!(answer.iter().all(|&p| !p), "steps {dl_steps}");
        }
    }

    /// An inference of a run in steps that derives more conclusions than the run's bound
    /// ends at its next look at the bounds, also when every conclusion is new and the
    /// inference reserves memory for its record of them. `Z` holds 60 clauses with `A`,
    /// 60 with `B` and three with `C`, which it processes in that order, so that each
    /// of the three makes 3,600 new conclusions in one inference with
    /// `A ⊓ B ⊓ C ⊑ D ⊔ E`. The maximum makes the run one that counts, which has the
    /// bound. The reservation asked only the memory budget, so the activation derived
    /// the 3,600 of the first inference before it looked at its count.
    #[test]
    fn a_wide_inference_of_a_run_in_steps_ends_once_it_passes_the_bound() {
        let k = 60;
        // The other classes of the unions come first, in an axiom that derives nothing,
        // so that `A`, `B` and `C` are the maximal literals of their clauses.
        let unions = |a: &str, p: &str, n: usize| {
            let us: Vec<String> = (0..n)
                .map(|i| format!("ObjectUnionOf(:{a} :{p}{i})"))
                .collect();
            us.join(" ")
        };
        let others: Vec<String> = (0..k)
            .map(|i| format!(":P{i}"))
            .chain((0..k).map(|i| format!(":Q{i}")))
            .chain((0..3).map(|i| format!(":R{i}")))
            .collect();
        let text = format!(
            "Prefix(:=<http://ex.org/>)
             Ontology(
             SubClassOf(ObjectIntersectionOf({}) :W)
             SubClassOf(:X ObjectMaxCardinality(1 :r))
             SubClassOf(:Z ObjectIntersectionOf({} {} {}))
             SubClassOf(ObjectIntersectionOf(:A :B :C) ObjectUnionOf(:D :E))
             )",
            others.join(" "),
            unions("A", "P", k),
            unions("B", "Q", k),
            unions("C", "R", 3),
        );
        let doc = crate::ofn_read::parse(&text).unwrap();
        // in the order of the document, which the order of the classes follows
        let axioms: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let limits = |threads: usize, dl_work: usize| Limits {
            threads,
            counting: true,
            dl_steps: true,
            dl_work: Some(dl_work),
            ..Limits::default()
        };
        // the whole run derives each product
        let whole = Saturation::new()
            .classify(&axioms, None, &limits(1, 1 << 20))
            .unwrap();
        assert!(!whole.stats.dl_counting_over);
        assert!(whole.stats.dl_work > 3 * k * k, "{}", whole.stats.dl_work);
        for threads in [1, 4] {
            PASSED_AT.with(|p| p.set(0));
            let out = Saturation::new()
                .classify(&axioms, None, &limits(threads, 300))
                .unwrap();
            assert!(out.stats.dl_counting_over);
            let at = PASSED_AT.with(std::cell::Cell::get);
            // the bound, the conclusions before the inference and those between two
            // looks at the bounds
            assert!(at > 300 && at < 1000, "{threads} threads: {at}");
        }
    }

    /// A panic in a worker of a saturation ends the run with the panic, in a helper and
    /// in the calling thread, in a run in steps and in a run without them. A helper that is
    /// not to panic stays in the callback for a while, and the calling thread panics
    /// only while a helper is there, so that it panics while a helper works in the same
    /// saturation. The other workers waited for the one that panicked without end: those
    /// of a run in steps at the barrier between the phases, and those of a run without steps
    /// for a count of busy workers that never reached zero.
    #[test]
    fn a_panic_in_a_worker_ends_the_saturation_with_the_panic() {
        use std::cell::Cell;
        use std::time::Duration;
        let axioms = many_contexts(700);
        for dl_steps in [true, false] {
            for in_helper in [true, false] {
                let (first, helper) = if dl_steps {
                    (saturate::STEP_CALLER, saturate::STEP_HELPER)
                } else {
                    (saturate::FIRST_WORKER, saturate::HELPER)
                };
                let (tx, rx) = std::sync::mpsc::channel();
                let axioms = axioms.clone();
                std::thread::spawn(move || {
                    // the helpers that are in the callback now
                    let waiting = Arc::new(AtomicUsize::new(0));
                    // whether the callback panicked where the case wants it
                    let fired = Arc::new(AtomicBool::new(false));
                    let hit = fired.clone();
                    let cancelled = move || {
                        let at = saturate::WORKER.with(Cell::get);
                        if at == helper && in_helper {
                            hit.store(true, Ordering::Relaxed);
                            panic!("the poll panics in a helper");
                        } else if at == helper {
                            // long enough for the calling thread's next poll, and
                            // short enough for a saturation in which it has none left
                            waiting.fetch_add(1, Ordering::SeqCst);
                            let since = Instant::now();
                            while !hit.load(Ordering::Relaxed)
                                && since.elapsed() < Duration::from_millis(200)
                            {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            waiting.fetch_sub(1, Ordering::SeqCst);
                        } else if at == first && !in_helper && waiting.load(Ordering::SeqCst) > 0 {
                            hit.store(true, Ordering::Relaxed);
                            panic!("the poll panics in the calling thread");
                        }
                        false
                    };
                    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        Classifier::of(&axioms)
                            .classify(&Limits {
                                threads: 8,
                                counting: true,
                                dl_steps,
                                cancelled: Some(Arc::new(cancelled)),
                                ..Limits::default()
                            })
                            .is_ok()
                    }));
                    let _ = tx.send((run.is_err(), fired.load(Ordering::Relaxed)));
                });
                // the run ended with a panic, and the panic was the callback's
                assert_eq!(
                    rx.recv_timeout(Duration::from_secs(120)),
                    Ok((true, true)),
                    "steps {dl_steps}, in a helper {in_helper}"
                );
            }
        }
    }

    #[test]
    fn pooled_panic_already_in_progress_allows_an_independent_classifier() {
        use std::cell::Cell;
        use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};

        #[derive(Debug, PartialEq, Eq)]
        struct Outer;

        struct ClassifyOnDrop<'a> {
            axioms: &'a [Axiom],
            completed: &'a Cell<bool>,
            charged: Arc<AtomicUsize>,
        }

        impl Drop for ClassifyOnDrop<'_> {
            fn drop(&mut self) {
                let charged = self.charged.clone();
                let limits = Limits {
                    threads: 1,
                    charge: Some(Arc::new(move |bytes| {
                        if saturate::WORKER.with(Cell::get) == saturate::FIRST_WORKER {
                            charged.fetch_add(bytes, Ordering::Relaxed);
                        }
                        true
                    })),
                    ..Limits::default()
                };
                let answer = Classifier::of(self.axioms).classify(&limits);
                self.completed.set(answer.is_ok_and(|out| out.complete));
            }
        }

        let mut ontology = String::from("Prefix(:=<urn:unwind:>) Ontology(");
        for i in 0..64 {
            ontology += &format!("SubClassOf(:A{i} :B{i}) ");
        }
        ontology.push(')');
        let axioms = crate::ofn_read::parse(&ontology).unwrap().axioms;
        let completed = Cell::new(false);
        let charged = Arc::new(AtomicUsize::new(0));
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _on_drop = ClassifyOnDrop {
                axioms: &axioms,
                completed: &completed,
                charged: charged.clone(),
            };
            panic_any(Outer);
        }))
        .unwrap_err();
        assert_eq!(panic.downcast_ref::<Outer>(), Some(&Outer));
        assert!(completed.get());
        assert!(charged.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn a_refused_charge_gives_back_the_credit_it_took() {
        // a pool with nothing left, and 100 bytes it admitted for an earlier run
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let charge = drawing_on(
            100,
            std::sync::Arc::new(move |bytes: usize| {
                a.fetch_add(bytes, Ordering::Relaxed);
                false
            }),
        );
        assert!(charge(60));
        assert_eq!(asked.load(Ordering::Relaxed), 0);
        // 40 of credit and 60 the pool refuses: the charge fails and keeps the 40
        assert!(!charge(100));
        assert_eq!(asked.load(Ordering::Relaxed), 60);
        assert!(charge(30));
        assert!(charge(10));
        assert!(!charge(1));
    }

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// A modules' pass that its cancellation stops answers with every class incomplete
    /// and the consistency unknown, instead of failing a finished saturation.
    #[test]
    fn a_stopped_coverage_pass_answers_with_every_class_incomplete() {
        let mut text = String::from("Prefix(:=<http://e/#>)\nOntology(<http://e/o>\n");
        for i in 0..6000 {
            text.push_str(&format!("SubClassOf(:A{i} :B{i})\n"));
        }
        text.push_str("SubClassOf(:X ObjectUnionOf(:Y :Z))\n)");
        let doc = crate::ofn_read::parse(&text).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut s = Saturation::new();
        let limits = Limits {
            el_only: true,
            cover_later: true,
            ..Limits::default()
        };
        let mut out = s.classify(&kinds, None, &limits).unwrap();
        assert!(!s.excluded().is_empty());
        assert!(
            out.complete && out.incomplete.is_empty(),
            "the pass is the caller's"
        );
        let stop = Limits {
            cancelled: Some(Arc::new(|| true)),
            ..limits.clone()
        };
        let witness = out.cover(&kinds, s.excluded(), None, &stop);
        assert!(witness.is_none());
        assert!(out.stats.coverage_stopped && !out.complete && out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Unknown);
        assert_eq!(out.incomplete.len(), 12003);
        // the same pass, not stopped, decides the classes
        let mut again = s.classify(&kinds, None, &limits).unwrap();
        again.cover(&kinds, s.excluded(), None, &limits);
        assert!(!again.stats.coverage_stopped && again.consistency == Consistency::Consistent);
        assert!(
            !again
                .incomplete
                .iter()
                .any(|c| c.as_str() == "http://e/#A0"),
            "{} incomplete, over {}, {:?}",
            again.incomplete.len(),
            again.stats.coverage_over,
            again.incomplete.iter().take(5).collect::<Vec<_>>()
        );
    }

    /// Item 1231's scan counts against the run's budget. A scan that would pass the
    /// caller's memory budget, or that the pool refuses, ends the rooted run as past its
    /// memory budget, which falls back to the Horn rules and reports no decline.
    #[test]
    fn the_decline_scan_counts_against_the_memory_budget_and_the_pool() {
        let doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)
             Ontology(
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
             SubClassOf(:K ObjectHasValue(:q :v))
             SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
             ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
             SubClassOf(:D ObjectSomeValuesFrom(:r :E))
             SubClassOf(:E ObjectHasValue(:r2 :w))
             SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
             SubClassOf(:C2 ObjectHasValue(:s :v))
             SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             )",
        )
        .unwrap();
        let base = Limits {
            threads: 1,
            rooted: true,
            ..Limits::default()
        };
        let out = Classifier::of(&doc.axioms).classify(&base).unwrap();
        assert!(out.stats.dl_rooted_declined);
        // The scan needs 512 MiB. The first case stops at a 256 MiB run budget and
        // the second stops when the pool refuses a grant of 256 MiB or more.
        let charge: Arc<dyn Fn(usize) -> bool + Send + Sync> = Arc::new(|b| b < 1 << 28);
        for (name, extra, limits) in [
            (
                "budget",
                1 << 29,
                Limits {
                    max_memory: Some(1 << 28),
                    ..base.clone()
                },
            ),
            (
                "pool",
                1 << 29,
                Limits {
                    charge: Some(charge),
                    ..base.clone()
                },
            ),
        ] {
            // without the scan's extra bytes the run withdraws its answer
            let out = Classifier::of(&doc.axioms).classify(&limits).unwrap();
            assert!(out.stats.dl_rooted_declined, "{name}");
            SCAN_EXTRA.with(|x| x.set(extra));
            let out = Classifier::of(&doc.axioms).classify(&limits);
            SCAN_EXTRA.with(|x| x.set(0));
            let out = out.unwrap();
            assert!(out.stats.dl_over_budget, "{name}");
            assert!(!out.stats.dl_rooted_declined, "{name}");
            assert!(!out.stats.dl_rooted_over, "{name}");
            assert!(out.stats.peak_memory >= extra, "{name}");
        }
    }

    #[test]
    fn axioms_outside_el_count_once_however_often_they_occur() {
        let c = |n: &str| ClassExpression::Class(Class::new(format!("http://ex.org/{n}")));
        let or = AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectUnionOf([c("B"), c("C")].into()),
        );
        let all = AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectAllValuesFrom(
                ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r")),
                Box::new(c("B")),
            ),
        );
        let mut axioms = vec![&or; 3000];
        axioms.extend(vec![&all; 3000]);
        for threads in [1, 8] {
            let out = Saturation::default()
                .classify(
                    &axioms,
                    None,
                    &Limits {
                        threads,
                        ..Limits::default()
                    },
                )
                .unwrap();
            assert_eq!(
                out.stats.outside_el,
                OutsideEl {
                    horn: 1,
                    disjunctive: 1,
                    global: 0,
                    // Tier D takes the union and the universal
                    taken: 2
                }
            );
            assert_eq!(out.stats.tier, Tier::Disjunctive);
        }
    }

    /// The run's index after classifying a functional-syntax body with the prefix `:`
    /// for `http://ex.org/`, and the entailment of `A ⊑ C`.
    fn horn_run(body: &str) -> (Index, bool) {
        let doc = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\nSubClassOf(:X ObjectAllValuesFrom(:u :Y))\n{body}\n)"
        ))
        .unwrap();
        let axioms: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut s = Saturation::default();
        let out = s.classify(&axioms, None, &Limits::default()).unwrap();
        assert_eq!(out.stats.tier, Tier::Horn);
        let c = |n: &str| Class::new(format!("http://ex.org/{n}"));
        let yes = out.taxonomy.is_subclass(&c("A"), &c("C"));
        (s.index.take().unwrap(), yes)
    }

    #[test]
    fn a_left_linear_chain_stays_composed_with_direct_right_operands() {
        // regulates ∘ part of ⊑ regulates, with part of transitive: the negative
        // existential over regulates stays on composed links, and part of's own chain
        // is not composed, since the links of regulates take part of one link at a time
        let (ix, yes) = horn_run(
            "TransitiveObjectProperty(:part_of)
             SubObjectPropertyOf(ObjectPropertyChain(:regulates :part_of) :regulates)
             SubClassOf(:A ObjectSomeValuesFrom(:regulates ObjectSomeValuesFrom(:part_of ObjectSomeValuesFrom(:part_of :B))))
             EquivalentClasses(:C ObjectSomeValuesFrom(:regulates :B))",
        );
        assert!(yes);
        // one chain is composed, over the subroles of its operands
        let results: std::collections::BTreeSet<index::Role> =
            ix.roles.comp.values().flatten().copied().collect();
        assert_eq!(results.len(), 1, "{:?}", ix.roles.comp);
        let &(left, right) = ix.roles.comp.keys().next().unwrap();
        // part of is a right operand, but no chain composes part of links
        assert!(ix.roles.keeps_forward(right));
        assert!(!results.contains(&right));
        assert!(results.contains(&left));
    }

    #[test]
    fn a_chain_of_three_falls_back_to_unfolding() {
        let (ix, yes) = horn_run(
            "SubObjectPropertyOf(ObjectPropertyChain(:p :q :s) :t)
             SubClassOf(:A ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q ObjectSomeValuesFrom(:s :B))))
             EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))",
        );
        assert!(yes);
        assert!(ix.roles.comp.is_empty(), "{:?}", ix.roles.comp);
        assert!(ix.roles.right.iter().all(|&r| !r));
    }

    #[test]
    fn a_chain_that_is_not_left_linear_composes_its_right_operand_too() {
        // negatively regulates ∘ negatively regulates ⊑ positively regulates: the right
        // operand's links must be complete, so its own chain is composed as well
        let (ix, yes) = horn_run(
            "SubObjectPropertyOf(ObjectPropertyChain(:neg :neg) :C_pos)
             SubObjectPropertyOf(ObjectPropertyChain(:neg :part_of) :neg)
             TransitiveObjectProperty(:part_of)
             SubClassOf(:A ObjectSomeValuesFrom(:neg ObjectSomeValuesFrom(:part_of ObjectSomeValuesFrom(:neg :B))))
             EquivalentClasses(:C ObjectSomeValuesFrom(:C_pos :B))",
        );
        assert!(yes);
        let results: std::collections::BTreeSet<index::Role> =
            ix.roles.comp.values().flatten().copied().collect();
        assert_eq!(results.len(), 2, "{:?}", ix.roles.comp);
    }

    /// The index of a Horn run over a functional-syntax body, before the saturation.
    fn horn_index_of(body: &str) -> Index {
        let doc =
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap();
        let axioms: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let outside: Vec<(usize, (String, String))> = axioms
            .iter()
            .enumerate()
            .filter_map(|(i, k)| {
                let b = el_break(k)?;
                Some((i, (b.rule.to_string(), b.message)))
            })
            .collect();
        let mut s = Saturation::default();
        let (_, ix, _, _, _) = s
            .build(
                &axioms,
                &outside,
                1,
                0,
                0,
                &Limits::default(),
                AttemptProvenance::Ineligible,
            )
            .unwrap();
        assert!(ix.horn.is_some());
        ix
    }

    #[test]
    fn absorbed_fillers_take_linear_work_and_stop_when_asked() {
        // C0 ⊑ C1 ⊑ … ⊑ Cn: every class is absorbed, unless Cn holds a universal, which
        // rejects them all, one after another down the chain
        let n = 50_000;
        let chain: String = (0..n)
            .map(|i| format!("SubClassOf(:C{i:05} :C{:05})\n", i + 1))
            .collect();
        let absorbed = |ix: &Index| {
            let h = ix.horn.as_ref().unwrap();
            (0..h.flags.len() as Ex)
                .filter(|&e| h.is(e, horn::HornIx::ABSORBED))
                .count()
        };
        let free = format!("{chain}SubClassOf(:X ObjectAllValuesFrom(:u :Y))");
        let mut ix = horn_index_of(&free);
        horn::absorb(&mut ix, &|| true);
        assert_eq!(absorbed(&ix), 0, "a stopped pass marks nothing");
        let mut ix = horn_index_of(&free);
        horn::absorb(&mut ix, &|| false);
        assert!(absorbed(&ix) > n, "{}", absorbed(&ix));
        let held = format!("{chain}SubClassOf(:C{n:05} ObjectAllValuesFrom(:u :Y))");
        let mut ix = horn_index_of(&held);
        let polls = std::cell::Cell::new(0usize);
        horn::absorb(&mut ix, &|| {
            polls.set(polls.get() + 1);
            false
        });
        // the universal's filler Y alone
        assert_eq!(absorbed(&ix), 1);
        // a pass per rejected class would poll thousands of times
        assert!(polls.get() <= 16, "{}", polls.get());
    }

    /// Random axioms over a few hundred classes, with intersections whose operands come
    /// in every order, so that chunks meet expressions in orders other than the run's.
    fn mixed_axioms(n: usize, seed: u64) -> Vec<AxiomKind> {
        let mut seed = seed;
        let mut next = move |m: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as usize
        };
        let c = |i: usize| ClassExpression::Class(Class::new(format!("http://ex.org/C{i}")));
        let p = |i: usize| ObjectProperty::new(format!("http://ex.org/p{i}"));
        let ind = |i: usize| Individual::Named(NamedIndividual::new(format!("http://ex.org/i{i}")));
        let mut out = vec![
            AxiomKind::TransitiveObjectProperty(ObjectPropertyExpression::Property(p(0))),
            AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Chain(vec![
                    ObjectPropertyExpression::Property(p(1)),
                    ObjectPropertyExpression::Property(p(2)),
                ]),
                ObjectPropertyExpression::Property(p(1)),
            ),
        ];
        let classes = 300;
        for _ in 0..n {
            let expr = |next: &mut dyn FnMut(usize) -> usize| -> ClassExpression {
                match next(4) {
                    0 => c(next(classes)),
                    1 => ClassExpression::some(p(next(4)), c(next(classes))),
                    2 => ClassExpression::ObjectIntersectionOf(
                        (0..2 + next(3)).map(|_| c(next(classes))).collect(),
                    ),
                    _ => ClassExpression::some(
                        p(next(4)),
                        ClassExpression::ObjectIntersectionOf(
                            [
                                c(next(classes)),
                                ClassExpression::some(p(next(4)), c(next(classes))),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                    ),
                }
            };
            let k = match next(10) {
                0 => AxiomKind::EquivalentClasses(
                    [expr(&mut next), c(next(classes))].into_iter().collect(),
                ),
                1 => AxiomKind::DisjointClasses(
                    [c(next(classes)), c(next(classes))].into_iter().collect(),
                ),
                2 => AxiomKind::ClassAssertion(c(next(classes)), ind(next(40))),
                3 => AxiomKind::ObjectPropertyAssertion(
                    ObjectPropertyExpression::Property(p(next(4))),
                    ind(next(40)),
                    ind(next(40)),
                ),
                4 => AxiomKind::SubObjectPropertyOf(
                    SubObjectPropertyExpression::Expression(ObjectPropertyExpression::Property(p(
                        4 + next(4),
                    ))),
                    ObjectPropertyExpression::Property(p(next(4))),
                ),
                5 => AxiomKind::Declaration(Entity::Class(Class::new(format!(
                    "http://ex.org/D{}",
                    next(classes)
                )))),
                _ => AxiomKind::SubClassOf(expr(&mut next), expr(&mut next)),
            };
            out.push(k);
        }
        out
    }

    #[test]
    fn an_incremental_parallel_normalization_keeps_the_ids_of_one_thread() {
        // without individuals a change keeps the interner, so the merge meets names and
        // expressions the interner already has
        let axioms: Vec<AxiomKind> = mixed_axioms(4000, 645)
            .into_iter()
            .filter(|k| {
                !matches!(
                    k,
                    AxiomKind::ClassAssertion(..) | AxiomKind::ObjectPropertyAssertion(..)
                )
            })
            .collect();
        let (before, added) = axioms.split_at(axioms.len() * 2 / 3);
        let added: Vec<&AxiomKind> = added.iter().filter(|k| !needs_full_run(k)).collect();
        let first: Vec<&AxiomKind> = before.iter().collect();
        let all: Vec<&AxiomKind> = first.iter().copied().chain(added.iter().copied()).collect();
        let run = |threads: usize| {
            let limits = Limits {
                threads,
                ..Limits::default()
            };
            let mut s = Saturation::new();
            s.classify(&first, None, &limits).unwrap();
            let changes = Changes {
                added: &added,
                removed: &[],
            };
            let out = s.classify(&all, Some(changes), &limits).unwrap();
            assert!(out.stats.incremental);
            (s, out)
        };
        let (one, a) = run(1);
        let (many, b) = run(8);
        assert_eq!(one.interner.nodes, many.interner.nodes);
        assert_eq!(one.interner.classes[..], many.interner.classes[..]);
        assert_eq!(one.interner.met, many.interner.met);
        assert_eq!(a.taxonomy, b.taxonomy);
        let fresh = Saturation::new()
            .classify(&all, None, &Limits::default())
            .unwrap();
        assert_eq!(a.taxonomy, fresh.taxonomy);
    }

    #[test]
    fn a_parallel_normalization_gives_the_ids_of_one_thread() {
        for seed in [1u64, 0x9e37_79b9_7f4a_7c15, 587] {
            let axioms = mixed_axioms(3000, seed);
            let kinds: Vec<&AxiomKind> = axioms.iter().collect();
            let run = |threads: usize| {
                let mut s = Saturation::new();
                let out = s
                    .classify(
                        &kinds,
                        None,
                        &Limits {
                            threads,
                            ..Limits::default()
                        },
                    )
                    .unwrap();
                (s, out)
            };
            let (one, a) = run(1);
            for threads in [2, 3, 8] {
                let (many, b) = run(threads);
                assert_eq!(
                    one.interner.nodes, many.interner.nodes,
                    "seed {seed}, {threads} threads"
                );
                assert_eq!(one.interner.classes[..], many.interner.classes[..]);
                assert_eq!(one.interner.roles, many.interner.roles);
                assert_eq!(one.interner.individuals[..], many.interner.individuals[..]);
                assert_eq!(one.interner.met, many.interner.met);
                assert_eq!(one.excluded, many.excluded);
                assert_eq!(a.taxonomy, b.taxonomy);
                assert_eq!(a.consistent, b.consistent);
            }
        }
    }

    /// Classifies on each thread count, and checks that the interner, the answer and the
    /// taxonomy are those of one thread.
    fn same_on_every_thread_count(axioms: &[AxiomKind]) -> Classification {
        let kinds: Vec<&AxiomKind> = axioms.iter().collect();
        let run = |threads: usize| {
            let mut s = Saturation::new();
            let out = s
                .classify(
                    &kinds,
                    None,
                    &Limits {
                        threads,
                        ..Limits::default()
                    },
                )
                .unwrap();
            (s, out)
        };
        let (one, a) = run(1);
        for threads in [2, 3, 8] {
            let (many, b) = run(threads);
            assert_eq!(one.interner.nodes, many.interner.nodes, "{threads} threads");
            assert_eq!(one.interner.classes[..], many.interner.classes[..]);
            assert_eq!(one.excluded, many.excluded);
            assert_eq!(a.consistent, b.consistent, "{threads} threads");
            assert_eq!(a.taxonomy, b.taxonomy, "{threads} threads");
        }
        a
    }

    fn padding(n: usize, at: usize) -> impl Iterator<Item = AxiomKind> {
        (at..at + n).map(|i| {
            AxiomKind::Declaration(Entity::Class(Class::new(format!("http://ex.org/P{i}"))))
        })
    }

    /// A saturation records its start of the helper threads as soon as they start, so
    /// that a saturation that starts while it runs waits as long (roadmap item 720). The
    /// cancellation hook, which the helpers poll at their checks of the bounds, looks at
    /// the record while they run, and a bounded handshake makes a helper look after the
    /// record is written.
    /// The steps outside the saturation run on the calling thread for so small an
    /// ontology.
    #[test]
    fn a_saturation_records_its_start_of_the_helpers_while_they_run() {
        let c = |i: usize| ClassExpression::Class(Class::new(format!("http://ex.org/C{i}")));
        let p = ObjectProperty::new("http://ex.org/p");
        let axioms: Vec<AxiomKind> = (0..1000)
            .flat_map(|i| {
                [
                    AxiomKind::SubClassOf(c(i), c(i + 1)),
                    AxiomKind::SubClassOf(c(i), ClassExpression::some(p.clone(), c(i + 2))),
                ]
            })
            .collect();
        let kinds: Vec<&AxiomKind> = axioms.iter().collect();
        let started: &'static std::sync::atomic::AtomicU64 =
            Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
        let seen = Arc::new(AtomicBool::new(false));
        let caller = std::thread::current().id();
        // a handshake, bounded so that a failure ends: another thread that looks before
        // the record waits for it once, and the first worker waits once for a helper to
        // have seen it
        thread_local! {
            static WAITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        let wait = |ready: &dyn Fn() -> bool, limit: Duration| {
            let until = Instant::now() + limit;
            while !ready() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let caller_waited = Arc::new(AtomicBool::new(false));
        let hook = {
            let seen = seen.clone();
            move || {
                let on = || started.load(std::sync::atomic::Ordering::Relaxed) > 0;
                let relaxed = std::sync::atomic::Ordering::Relaxed;
                if std::thread::current().id() != caller {
                    if !on() && !WAITED.replace(true) {
                        wait(&on, Duration::from_millis(250));
                    }
                    if on() {
                        seen.store(true, relaxed);
                    }
                } else if on() && !caller_waited.swap(true, relaxed) {
                    wait(&|| seen.load(relaxed), Duration::from_secs(2));
                }
                false
            }
        };
        saturate::STARTED_IN.set(Some(started));
        let out = Saturation::new().classify(
            &kinds,
            None,
            &Limits {
                threads: 4,
                cancelled: Some(Arc::new(hook)),
                ..Limits::default()
            },
        );
        saturate::STARTED_IN.set(None);
        out.unwrap();
        assert!(started.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert!(seen.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// A saturation counts as congested only with hundreds of megabytes of waiting
    /// messages, so the test makes every saturation congested from its start. The
    /// answers are those of an ordinary run on every thread count, and a run on one
    /// thread, which sends through its outbox once congested, drops repeated messages
    /// (roadmap item 721).
    #[test]
    fn congested_runs_drop_repeated_messages_and_keep_the_answer() {
        for seed in [3, 11, 29] {
            let axioms = mixed_axioms(1500, seed);
            let kinds: Vec<&AxiomKind> = axioms.iter().collect();
            let run = |threads: usize, congested: bool| {
                saturate::ALWAYS_CONGESTED.set(congested);
                let out = Saturation::new().classify(
                    &kinds,
                    None,
                    &Limits {
                        threads,
                        ..Limits::default()
                    },
                );
                saturate::ALWAYS_CONGESTED.set(false);
                out.unwrap()
            };
            let a = run(1, false);
            for threads in [1, 4] {
                let b = run(threads, true);
                assert_eq!(a.consistent, b.consistent, "seed {seed}, {threads} threads");
                assert_eq!(a.taxonomy, b.taxonomy, "seed {seed}, {threads} threads");
            }
            let b = run(1, true);
            assert!(
                b.stats.conclusions < a.stats.conclusions,
                "seed {seed}: {} conclusions congested, {} not",
                b.stats.conclusions,
                a.stats.conclusions
            );
        }
    }

    #[test]
    fn a_parallel_normalization_keeps_functional_data_roles() {
        let mut axioms = mixed_axioms(400, 7);
        axioms.push(AxiomKind::FunctionalDataProperty(DataProperty::new(
            crate::vocab::OWL_TOP_DATA_PROPERTY,
        )));
        axioms.extend(padding(100, 0));
        let a = same_on_every_thread_count(&axioms);
        assert!(!a.consistent);
    }

    #[test]
    fn operands_that_the_merge_makes_one_expression_count_once() {
        let c = |s: &str| ClassExpression::Class(Class::new(format!("http://ex.org/{s}")));
        let and = |xs: Vec<ClassExpression>| {
            ClassExpression::ObjectIntersectionOf(xs.into_iter().collect())
        };
        let abc = || and(vec![c("A"), c("B"), c("C")]);
        let c_ab = || and(vec![c("C"), and(vec![c("A"), c("B")])]);
        let mut axioms: Vec<AxiomKind> = padding(200, 0).collect();
        axioms.push(AxiomKind::DisjointClasses(
            [abc(), c_ab()].into_iter().collect(),
        ));
        for s in ["A", "B", "C"] {
            axioms.push(AxiomKind::SubClassOf(c("X"), c(s)));
        }
        axioms.push(AxiomKind::SubClassOf(
            and(vec![c("D"), abc(), c_ab()]),
            c("E"),
        ));
        axioms.extend(padding(200, 200));
        let a = same_on_every_thread_count(&axioms);
        let x = Class::new("http://ex.org/X");
        assert!(a.taxonomy.unsatisfiable().any(|u| *u == x));
    }

    /// Ranges of a few properties over many classes, so that every chunk of a parallel
    /// normalization meets every class.
    fn ranges(n: usize, properties: usize, classes: usize) -> Vec<AxiomKind> {
        (0..n)
            .map(|i| {
                AxiomKind::ObjectPropertyRange(
                    ObjectPropertyExpression::Property(ObjectProperty::new(format!(
                        "http://ex.org/p{}",
                        i / classes % properties
                    ))),
                    ClassExpression::Class(Class::new(format!("http://ex.org/C{}", i % classes))),
                )
            })
            .collect()
    }

    #[test]
    fn the_chunks_of_a_parallel_normalization_count_against_the_budget() {
        let axioms = ranges(8192, 16, 512);
        let kinds: Vec<&AxiomKind> = axioms.iter().collect();
        let run = |threads: usize, max_memory: Option<usize>| {
            let granted = Arc::new(AtomicUsize::new(0));
            let g = granted.clone();
            let out = Saturation::new()
                .classify(
                    &kinds,
                    None,
                    &Limits {
                        threads,
                        max_memory,
                        charge: Some(Arc::new(move |b| {
                            g.fetch_add(b, Ordering::Relaxed);
                            true
                        })),
                        ..Limits::default()
                    },
                )
                .unwrap();
            (out, granted.load(Ordering::Relaxed))
        };
        let (one, one_granted) = run(1, None);
        // each of the chunks holds every class, which the peak and the pool's grant count
        let (many, many_granted) = run(16, None);
        assert!(
            many.stats.peak_memory > 2 * one.stats.peak_memory,
            "{} against {}",
            many.stats.peak_memory,
            one.stats.peak_memory
        );
        assert!(
            many_granted > 2 * one_granted,
            "{many_granted} against {one_granted}"
        );
        assert!(many_granted >= many.stats.peak_memory);
        // under a budget that one thread fits in, the run normalizes on one thread
        let limit = one.stats.peak_memory + one.stats.peak_memory / 10;
        let (bounded, _) = run(16, Some(limit));
        assert!(
            bounded.stats.peak_memory <= limit,
            "{} over {limit}",
            bounded.stats.peak_memory
        );
        assert_eq!(bounded.taxonomy, one.taxonomy);
    }

    #[test]
    fn a_merge_of_chunks_polls() {
        let axioms = mixed_axioms(500, 3);
        let chunks: Vec<Interner> = axioms
            .chunks(250)
            .map(|part| {
                let mut it = Interner::chunk(&FxHashMap::default());
                let mut ax = Axioms::default();
                for k in part {
                    index::normalize(k, &mut it, &mut ax);
                }
                it
            })
            .collect();
        let mut calls = 0;
        let r = Interner::new().merge(chunks, &mut |_| {
            calls += 1;
            Err(())
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    /// The interners of chunks of `axioms`, `n` axioms each.
    fn chunk_interners(axioms: &[AxiomKind], n: usize) -> Vec<Interner> {
        axioms
            .chunks(n)
            .map(|part| {
                let mut it = Interner::chunk(&FxHashMap::default());
                let mut ax = Axioms::default();
                for k in part {
                    index::normalize(k, &mut it, &mut ax);
                }
                it
            })
            .collect()
    }

    #[test]
    fn a_merge_of_chunks_polls_over_chunks_and_roles() {
        // chunks of roles alone, each shorter than the polls' interval
        let axioms: Vec<AxiomKind> = (0..4 * index::POLL_MERGE / 2)
            .map(|i| {
                AxiomKind::FunctionalDataProperty(DataProperty::new(format!("http://ex.org/d{i}")))
            })
            .collect();
        let chunks = chunk_interners(&axioms, index::POLL_MERGE / 2);
        let mut calls = 0;
        Interner::new()
            .merge(chunks, &mut |_| -> Result<(), ()> {
                calls += 1;
                Ok(())
            })
            .unwrap();
        assert!(calls > 2, "{calls} polls");
    }

    #[test]
    fn the_chunks_count_what_their_merge_adds() {
        // wide intersections, each of which the merge makes into many binary ones, since
        // the class that differs comes first in the order of ids
        let c = |s: String| ClassExpression::Class(Class::new(format!("http://ex.org/{s}")));
        let declared = (0..256).map(|i| {
            AxiomKind::Declaration(Entity::Class(Class::new(format!("http://ex.org/A{i}"))))
        });
        let axioms: Vec<AxiomKind> = declared
            .chain((0..256).map(|i| {
                AxiomKind::SubClassOf(
                    ClassExpression::ObjectIntersectionOf(
                        std::iter::once(c(format!("A{i}")))
                            .chain((0..64).map(|j| c(format!("S{j}"))))
                            .collect(),
                    ),
                    c("X".into()),
                )
            }))
            .collect();
        let chunks = chunk_interners(&axioms, 32);
        let reserved: usize = chunks.iter().map(|it| it.merge_bytes()).sum();
        let mut merged = Interner::new();
        let start = merged.bytes();
        merged
            .merge(chunks, &mut |_| -> Result<(), ()> { Ok(()) })
            .unwrap();
        let added = merged.bytes() - start;
        assert!(added <= reserved, "{added} added, {reserved} counted");
    }

    #[test]
    fn rounds_after_individuals_charge_the_pool_no_more_than_the_peak() {
        let mut axioms = ranges(8192, 16, 512);
        axioms.push(AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("http://ex.org/C0")),
            ClassExpression::ObjectOneOf(
                [Individual::Named(NamedIndividual::new("http://ex.org/a"))]
                    .into_iter()
                    .collect(),
            ),
        ));
        let kinds: Vec<&AxiomKind> = axioms.iter().collect();
        for threads in [1, 8] {
            let granted = Arc::new(AtomicUsize::new(0));
            let g = granted.clone();
            let out = Saturation::new()
                .classify(
                    &kinds,
                    None,
                    &Limits {
                        threads,
                        charge: Some(Arc::new(move |b| {
                            g.fetch_add(b, Ordering::Relaxed);
                            true
                        })),
                        ..Limits::default()
                    },
                )
                .unwrap();
            let granted = granted.load(Ordering::Relaxed);
            assert!(
                granted <= out.stats.peak_memory,
                "{threads} threads: {granted} granted, peak {}",
                out.stats.peak_memory
            );
        }
    }

    #[test]
    fn refused_saturation_growth_is_not_credit_for_partial_output() {
        for taxonomy in [false, true] {
            for accept in [false, true] {
                let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
                let seen = requests.clone();
                let limits = Limits {
                    charge: Some(Arc::new(move |more| {
                        seen.lock().push(more);
                        accept
                    })),
                    ..Limits::default()
                };
                let mut totals = saturate::Totals {
                    memory: 4096,
                    peak: 4288,
                    paid: 4096,
                    ..saturate::Totals::default()
                };
                if taxonomy {
                    Saturation::admit_retained(4096, &mut totals, &limits).unwrap();
                    let paid = std::cell::Cell::new(totals.paid);
                    let charge = limits
                        .charge
                        .as_deref()
                        .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
                    let budget = Saturation::taxonomy_budget(4096, &paid, None, charge);
                    assert_eq!(budget(64), accept);
                    if accept {
                        assert!(budget(64));
                    }
                    assert_eq!(paid.get(), if accept { 4160 } else { 4096 });
                } else {
                    let result = Saturation::admit_retained(4160, &mut totals, &limits);
                    let want = if accept {
                        Ok(())
                    } else {
                        Err(Error::Memory {
                            limit: usize::MAX,
                            used: 4160,
                        })
                    };
                    assert_eq!(result, want);
                    assert_eq!(totals.paid, if accept { 4160 } else { 4096 });
                }
                assert_eq!(*requests.lock(), [64]);
                assert_eq!(totals.peak, 4288);
            }
        }
    }

    #[test]
    fn a_pooled_marker_scan_pays_once_before_building_taxonomy() {
        let doc = crate::ofn_read::parse(
            "Prefix(:=<urn:marker-paid:>)
             Ontology(ClassAssertion(:T :a) SubClassOf(:A ObjectUnionOf(:B :C)))",
        )
        .unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let charged = accepted.clone();
        let limits = Limits {
            threads: 1,
            rooted: true,
            charge: Some(Arc::new(move |bytes| {
                assert!(bytes > 0);
                charged.fetch_add(bytes, Ordering::Relaxed);
                true
            })),
            ..Limits::default()
        };
        let kinds: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut classifier = Saturation::new();
        let extra = 1 << 20;
        SCAN_EXTRA.with(|x| x.set(extra));
        let result = classifier.classify(&kinds, None, &limits);
        SCAN_EXTRA.with(|x| x.set(0));
        let out = result.unwrap();
        assert!(out.complete && classifier.dl_rooted);
        assert!(!out.stats.dl_rooted_declined);
        assert!(out.stats.peak_memory >= extra);
        assert_eq!(accepted.load(Ordering::Relaxed), out.stats.peak_memory);
        assert!(out.stats.memory < extra);
    }

    #[test]
    fn world_reservation_waits_for_pool_admission() {
        world_reservation_race(false);
    }

    #[test]
    fn world_reservation_cannot_buy_an_unpaid_suffix() {
        world_reservation_race(true);
    }

    /// The reporter, like key scratch storage, owns the delta before posting it.
    struct WorldDebit<'a, 'b>(&'a WorldBudget<'b>, isize);

    impl Drop for WorldDebit<'_, '_> {
        fn drop(&mut self) {
            self.0.tell(-self.1);
        }
    }

    fn report_world_debit(budget: &WorldBudget<'_>, bytes: isize) -> bool {
        let _debit = WorldDebit(budget, bytes);
        budget.tell(bytes)
    }

    #[test]
    fn world_charge_panic_stops_growth_waiters() {
        use admission_test::{ChargePanic, FirstCharge, Point, WAIT, set_hook};
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::mpsc;

        for prior_memory_stop in [false, true] {
            let first = FirstCharge::new();
            let charge = first.charge(true);
            let limits = Limits::default();
            let mut totals = saturate::Totals {
                peak: 4096,
                paid: 4096,
                ..saturate::Totals::default()
            };
            let budget = WorldBudget::start(&limits, Some(&charge), 4096, &mut totals).unwrap();
            let (send, events) = mpsc::channel();
            let (requested, observed, contended, caught, second) = std::thread::scope(|scope| {
                let a = scope
                    .spawn(|| catch_unwind(AssertUnwindSafe(|| report_world_debit(&budget, 64))));
                let requested = first.entered.recv_timeout(WAIT);
                let b = scope.spawn(|| {
                    set_hook(move |point| {
                        let _ = send.send(point);
                    });
                    report_world_debit(&budget, 128)
                });
                let observed = events.recv_timeout(WAIT);
                let contended = events.recv_timeout(WAIT);
                if prior_memory_stop {
                    budget.stop_for_memory(5000);
                }
                let _ = first.release.send(());
                (requested, observed, contended, a.join(), b.join())
            });
            assert_eq!(requested.unwrap(), 64);
            assert_eq!(observed.unwrap(), Point::Observed);
            assert_eq!(contended.unwrap(), Point::Contended);
            let panic = caught.unwrap().unwrap_err();
            assert_eq!(panic.downcast_ref::<ChargePanic>(), Some(&ChargePanic));
            assert!(!second.unwrap());
            assert_eq!(*first.requests.lock(), [64]);
            assert_eq!(budget.paid.load(Ordering::Relaxed), 4096);
            assert_eq!(budget.peak.load(Ordering::Relaxed), 4288);
            assert_eq!(budget.held.load(Ordering::Relaxed), 0);
            assert_eq!(
                budget.stopped(),
                if prior_memory_stop {
                    Err(Error::Memory {
                        limit: usize::MAX,
                        used: 5000,
                    })
                } else {
                    Err(Error::Cancelled)
                }
            );
        }
    }

    #[test]
    fn world_reservation_keeps_successful_payment_when_a_stop_wins() {
        use admission_test::{FirstCharge, WAIT};

        for memory in [false, true] {
            let first = FirstCharge::new();
            let charge = first.charge(false);
            let cancelled = Arc::new(AtomicBool::new(false));
            let flag = cancelled.clone();
            let limits = Limits {
                cancelled: Some(Arc::new(move || flag.load(Ordering::Relaxed))),
                ..Limits::default()
            };
            let mut totals = saturate::Totals {
                peak: 4096,
                paid: 4096,
                ..saturate::Totals::default()
            };
            let budget = WorldBudget::start(&limits, Some(&charge), 4096, &mut totals).unwrap();
            let (requested, stopped, admitted) = std::thread::scope(|scope| {
                let a = scope.spawn(|| report_world_debit(&budget, 64));
                let requested = first.entered.recv_timeout(WAIT);
                let stopped = if memory {
                    budget.stop_for_memory(5000);
                    false
                } else {
                    cancelled.store(true, Ordering::Relaxed);
                    budget.tell(0)
                };
                let _ = first.release.send(());
                (requested, stopped, a.join())
            });
            assert_eq!(requested.unwrap(), 64);
            assert!(!stopped);
            assert!(!admitted.unwrap());
            assert_eq!(*first.requests.lock(), [64]);
            assert_eq!(budget.paid.load(Ordering::Relaxed), 4160);
            assert_eq!(budget.held.load(Ordering::Relaxed), 0);
            assert_eq!(
                budget.stopped(),
                if memory {
                    Err(Error::Memory {
                        limit: usize::MAX,
                        used: 5000,
                    })
                } else {
                    Err(Error::Cancelled)
                }
            );
        }
    }

    fn world_reservation_race(smaller_first: bool) {
        for accept in [false, true] {
            let race = admission_test::Race::new(accept);
            let charge = race.charge();
            let limits = Limits::default();
            let budget = WorldBudget {
                limits: &limits,
                charge: Some(&charge),
                base: 4096,
                held: AtomicIsize::new(0),
                peak: AtomicUsize::new(4096),
                paid: AtomicUsize::new(4096),
                growth: parking_lot::Mutex::new(()),
                halt: AtomicU8::new(0),
                halted_at: AtomicUsize::new(0),
            };
            let reserve = |bytes| {
                let ok = budget.tell(bytes);
                if !ok {
                    budget.tell(-bytes);
                }
                ok
            };
            race.run(|| reserve(64), || reserve(128), smaller_first);
            assert_eq!(budget.peak.load(Ordering::Relaxed), 4288);
            assert_eq!(
                budget.paid.load(Ordering::Relaxed),
                if accept { 4288 } else { 4096 }
            );
            if accept {
                assert_eq!(budget.held.load(Ordering::Relaxed), 192);
                assert!(budget.tell(-192));
                assert!(budget.tell(192));
                assert!(budget.tell(-192));
                assert_eq!(budget.stopped(), Ok(()));
            } else {
                assert_eq!(
                    budget.stopped(),
                    Err(Error::Memory {
                        limit: usize::MAX,
                        used: if smaller_first { 4160 } else { 4288 },
                    })
                );
            }
            assert_eq!(budget.held.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn the_world_budget_polls_the_cancellation_when_nothing_grows() {
        let flag = Arc::new(AtomicBool::new(false));
        let seen = flag.clone();
        let limits = Limits {
            cancelled: Some(Arc::new(move || seen.load(Ordering::Relaxed))),
            ..Limits::default()
        };
        let budget = WorldBudget {
            limits: &limits,
            charge: None,
            base: 0,
            held: AtomicIsize::new(0),
            peak: AtomicUsize::new(0),
            paid: AtomicUsize::new(0),
            growth: parking_lot::Mutex::new(()),
            halt: AtomicU8::new(0),
            halted_at: AtomicUsize::new(0),
        };
        assert!(budget.tell(100));
        assert!(budget.tell(0));
        flag.store(true, Ordering::Relaxed);
        // a world whose working set stays flat or shrinks still sees the cancellation
        assert!(!budget.tell(0));
        assert!(!budget.tell(-50));
        assert_eq!(budget.stopped(), Err(Error::Cancelled));
        assert_eq!(budget.held.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn the_world_budget_stops_at_the_memory_limit_and_keeps_the_first_reason() {
        let limits = Limits {
            max_memory: Some(1_000),
            deadline: Some(Instant::now() + Duration::from_secs(3600)),
            ..Limits::default()
        };
        let budget = WorldBudget {
            limits: &limits,
            charge: None,
            base: 600,
            held: AtomicIsize::new(0),
            peak: AtomicUsize::new(600),
            paid: AtomicUsize::new(0),
            growth: parking_lot::Mutex::new(()),
            halt: AtomicU8::new(0),
            halted_at: AtomicUsize::new(0),
        };
        assert!(budget.tell(300));
        assert!(!budget.tell(200));
        assert!(!budget.tell(-500));
        assert_eq!(
            budget.stopped(),
            Err(Error::Memory {
                limit: 1_000,
                used: 1_100
            })
        );
    }

    #[test]
    fn the_horn_closure_of_the_roles_counts_against_the_budget_and_stops() {
        // a line of 3,000 properties, whose closure with the inverses holds about nine
        // million pairs, under one universal
        let p = |i: usize| {
            ObjectPropertyExpression::Property(ObjectProperty::new(format!("http://ex.org/r{i}")))
        };
        let c = |n: &str| ClassExpression::Class(Class::new(format!("http://ex.org/{n}")));
        let subs: Vec<AxiomKind> = (0..3000)
            .map(|i| {
                AxiomKind::SubObjectPropertyOf(
                    SubObjectPropertyExpression::Expression(p(i)),
                    p(i + 1),
                )
            })
            .collect();
        let all = AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectAllValuesFrom(p(0), Box::new(c("B"))),
        );
        let horn = |limit: Option<usize>, stop: &dyn Fn() -> bool| {
            let mut it = Interner::new();
            let mut ax = index::Axioms::default();
            for k in &subs {
                assert!(index::normalize(k, &mut it, &mut ax).is_none());
            }
            horn::normalize(&[&all], &mut it, &mut ax, limit, stop)
        };
        assert!(
            matches!(horn(Some(1 << 20), &|| false), Err(index::Halt::Memory(b)) if b <= (1 << 20) + 64 * 1024)
        );
        assert!(matches!(horn(None, &|| true), Err(index::Halt::Stopped)));
        assert_eq!(horn(None, &|| false).ok(), Some(vec![true]));
    }

    #[test]
    fn the_shared_individuals_poll_and_respect_the_budget() {
        // roadmap item 1281: the scratch of `interchangeable` is admitted before it is
        // allocated and grows only with the class assertions it may share, and the scan
        // polls the deadline and cancellation
        let class = ClassExpression::Class(Class::new("http://ex.org/C"));
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let anon = |i: usize| Individual::Anonymous(AnonymousIndividual::new(format!("x{i}")));
        let mut kinds: Vec<AxiomKind> = (0..4)
            .map(|i| AxiomKind::ClassAssertion(class.clone(), anon(i)))
            .collect();
        kinds.extend((10..20_000).map(|i| {
            AxiomKind::ObjectPropertyAssertion(
                r.clone(),
                anon(i),
                Individual::named("http://ex.org/a"),
            )
        }));
        let axioms: Vec<&AxiomKind> = kinds.iter().collect();
        let (kept, admitted) = interchangeable(&axioms, 0, &Limits::default()).unwrap();
        let kept = kept.unwrap();
        // the scratch admits the list the run reads, which it allocates once
        let scratch = 4 * LABEL_BYTES + axioms.len() * std::mem::size_of::<&AxiomKind>();
        assert_eq!((kept.len(), admitted), (axioms.len() - 3, scratch));
        assert_eq!(kept.capacity(), kept.len());
        // a budget below the scratch beside the caller's axioms leaves every assertion
        // its individual
        let small = Limits {
            max_memory: Some(scratch),
            ..Limits::default()
        };
        assert!(interchangeable(&axioms, 0, &small).unwrap().0.is_some());
        assert!(interchangeable(&axioms, 1, &small).unwrap().0.is_none());
        // so does a pool that refuses it, which is told the scratch alone
        let told = std::sync::Arc::new(AtomicUsize::new(0));
        let t = told.clone();
        let refusing = Limits {
            charge: Some(std::sync::Arc::new(move |b: usize| {
                t.fetch_add(b, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        };
        assert_eq!(interchangeable(&axioms, 0, &refusing).unwrap(), (None, 0));
        assert_eq!(told.load(Ordering::Relaxed), scratch);
        let cancelled = Limits {
            cancelled: Some(std::sync::Arc::new(|| true)),
            ..Limits::default()
        };
        assert!(matches!(
            interchangeable(&axioms, 0, &cancelled),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn the_horn_normalization_takes_assertions_about_an_anonymous_individual() {
        // roadmap item 1281: an anonymous individual is a fresh individual of the run, so
        // the assertions about it are Horn axioms like those about a named one, which
        // replaced item 869's refusal of them
        let c = |n: &str| ClassExpression::Class(Class::new(format!("http://ex.org/{n}")));
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let x = || Individual::Anonymous(AnonymousIndividual::new("x"));
        let all = AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectAllValuesFrom(r.clone(), Box::new(c("B"))),
        );
        let assertions = [
            AxiomKind::ClassAssertion(c("A"), x()),
            AxiomKind::ObjectPropertyAssertion(
                r.clone(),
                x(),
                Individual::named("http://ex.org/a"),
            ),
            AxiomKind::ObjectPropertyAssertion(
                r.clone(),
                Individual::named("http://ex.org/a"),
                x(),
            ),
            AxiomKind::NegativeObjectPropertyAssertion(r, x(), x()),
        ];
        let mut with: Vec<&AxiomKind> = assertions.iter().collect();
        with.push(&all);
        let mut it = Interner::new();
        let mut ax = index::Axioms::default();
        let taken = horn::normalize(&with, &mut it, &mut ax, None, &|| false).unwrap();
        assert_eq!(taken, vec![true; 5]);
        assert!(
            it.individuals.iter().any(index::is_anonymous),
            "the anonymous individual is an individual of the run"
        );
    }

    #[test]
    fn held_and_excluded_byte_estimates_count_repeated_multiset_operands() {
        let a = ClassExpression::Class(Class::new("http://ex.org/RepeatedClass"));
        let small = AxiomKind::DisjointClasses(vec![a.clone(); 2].into());
        let large = AxiomKind::DisjointClasses(vec![a; 1000].into());
        let delta = held_bytes_of(&large).saturating_sub(held_bytes_of(&small));
        assert!(
            delta >= 998 * std::mem::size_of::<ClassExpression>(),
            "expected held_bytes_of to grow with repeated operands, delta={delta}"
        );
        let small_ex = vec![Excluded {
            axiom: small,
            gap: Gap::Partial("test"),
        }];
        let large_ex = vec![Excluded {
            axiom: large,
            gap: Gap::Partial("test"),
        }];
        assert!(
            excluded_copy_bytes(&large_ex)
                >= excluded_copy_bytes(&small_ex) + 998 * std::mem::size_of::<ClassExpression>()
        );
    }

    #[test]
    fn held_and_excluded_byte_estimates_count_repeated_data_range_payloads() {
        let d = DataProperty::new("http://ex.org/d");
        let short_lit = Literal::typed("x".repeat(10), crate::vocab::XSD_STRING);
        let long_lit = Literal::typed("x".repeat(1010), crate::vocab::XSD_STRING);

        let some_short = ClassExpression::DataSomeValuesFrom(
            vec![d.clone()],
            DataRange::OneOf([short_lit.clone()].into()),
        );
        let some_long = ClassExpression::DataSomeValuesFrom(
            vec![d.clone()],
            DataRange::OneOf([long_lit.clone()].into()),
        );
        let ax_short = AxiomKind::DisjointClasses(vec![some_short; 200].into());
        let ax_long = AxiomKind::DisjointClasses(vec![some_long; 200].into());
        let delta = held_bytes_of(&ax_long).saturating_sub(held_bytes_of(&ax_short));
        assert!(
            delta >= 199 * 1000,
            "expected DataSomeValuesFrom DataRange payload growth across repeats, delta={delta}"
        );
        let ex_short = vec![Excluded {
            axiom: ax_short,
            gap: Gap::Partial("test"),
        }];
        let ex_long = vec![Excluded {
            axiom: ax_long,
            gap: Gap::Partial("test"),
        }];
        assert!(excluded_copy_bytes(&ex_long) >= excluded_copy_bytes(&ex_short) + 199 * 1000);

        let card_short = ClassExpression::DataMinCardinality(
            1,
            d.clone(),
            Some(DataRange::OneOf([short_lit].into())),
        );
        let card_long =
            ClassExpression::DataMinCardinality(1, d, Some(DataRange::OneOf([long_lit].into())));
        let card_ax_short = AxiomKind::DisjointClasses(vec![card_short; 200].into());
        let card_ax_long = AxiomKind::DisjointClasses(vec![card_long; 200].into());
        let card_delta = held_bytes_of(&card_ax_long).saturating_sub(held_bytes_of(&card_ax_short));
        assert!(
            card_delta >= 199 * 1000,
            "expected DataMinCardinality DataRange payload growth across repeats, delta={card_delta}"
        );
    }

    #[test]
    fn rooted_work_bound_scales_by_distinct_visible_named_classes() {
        // Unit check on both sides of 16,384 (= 524,288 / 32) and 398,583 (ore_ont_15803).
        assert_eq!(rooted_work_bound(0, false), ROOTED_WORK_BOUND);
        assert_eq!(rooted_work_bound(398_583, false), ROOTED_WORK_BOUND);
        assert_eq!(rooted_work_bound(0, true), ROOTED_WORK_BOUND);
        assert_eq!(rooted_work_bound(16_384, true), ROOTED_WORK_BOUND);
        assert_eq!(rooted_work_bound(16_385, true), 32 * 16_385);
        assert_eq!(rooted_work_bound(398_583, true), 12_754_656);

        // End-to-end check: explicit Declaration(Class(...)) axioms must NOT double-count
        // named classes, and individuals/nominals/owl:Thing/owl:Nothing must not inflate it.
        let make_ontology = |n: usize, with_declarations: bool| -> Vec<Axiom> {
            let mut ax = Vec::with_capacity(if with_declarations { 2 * n + 4 } else { n + 4 });
            let a = Individual::named("http://example.org/b#a");
            let b = Individual::named("http://example.org/b#b");
            let p =
                ObjectPropertyExpression::Property(ObjectProperty::new("http://example.org/b#p"));
            // Force rooted Tier D via disjunction + nominal.
            let c0 = Class::new("http://example.org/b#C0");
            let c1 = Class::new("http://example.org/b#C1");
            ax.push(Axiom::new(AxiomKind::ClassAssertion(
                ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([
                    ClassExpression::Class(c0.clone()),
                    ClassExpression::Class(c1.clone()),
                ])),
                a.clone(),
            )));
            ax.push(Axiom::new(AxiomKind::ObjectPropertyAssertion(p, a, b)));
            if with_declarations {
                ax.push(Axiom::new(AxiomKind::Declaration(Entity::Class(
                    Class::thing(),
                ))));
                ax.push(Axiom::new(AxiomKind::Declaration(Entity::Class(
                    Class::nothing(),
                ))));
            }
            for i in 0..n {
                let ci = Class::new(format!("http://example.org/b#C{i}"));
                if with_declarations {
                    ax.push(Axiom::new(AxiomKind::Declaration(Entity::Class(
                        ci.clone(),
                    ))));
                }
                if i > 0 {
                    ax.push(Axiom::new(AxiomKind::SubClassOf(
                        ClassExpression::Class(ci),
                        ClassExpression::Class(c0.clone()),
                    )));
                }
            }
            ax
        };

        // 16,385 distinct named classes: with and without declarations must set the exact
        // same scaled bound `32 * 16_385 = 524_320` (not `32 * 32_770`!).
        let no_decl = make_ontology(16_385, false);
        let with_decl = make_ontology(16_385, true);
        let limits_rooted = Limits {
            threads: 1,
            rooted: true,
            ..Limits::default()
        };
        let lim_off = with_rooted_bound_per_class(false, || {
            LAST_ROOTED_WORK_LIMIT.with(|c| c.set(0));
            let mut k = Classifier::of(&with_decl);
            let _ = k.classify(&limits_rooted).unwrap();
            LAST_ROOTED_WORK_LIMIT.with(|c| c.get())
        });
        assert_eq!(
            lim_off, ROOTED_WORK_BOUND,
            "default-off keeps fixed 524,288"
        );

        let lim_no_decl = with_rooted_bound_per_class(true, || {
            LAST_ROOTED_WORK_LIMIT.with(|c| c.set(0));
            let mut k = Classifier::of(&no_decl);
            let _ = k.classify(&limits_rooted).unwrap();
            LAST_ROOTED_WORK_LIMIT.with(|c| c.get())
        });
        let lim_with_decl = with_rooted_bound_per_class(true, || {
            LAST_ROOTED_WORK_LIMIT.with(|c| c.set(0));
            let mut k = Classifier::of(&with_decl);
            let _ = k.classify(&limits_rooted).unwrap();
            LAST_ROOTED_WORK_LIMIT.with(|c| c.get())
        });
        assert_eq!(lim_no_decl, 32 * 16_385);
        assert_eq!(
            lim_with_decl, lim_no_decl,
            "explicit declarations must not double-count visible named classes"
        );

        // Explicit `Limits::dl_work` override wins even when per-class scaling is on.
        let lim_override = with_rooted_bound_per_class(true, || {
            LAST_ROOTED_WORK_LIMIT.with(|c| c.set(0));
            let mut k = Classifier::of(&with_decl);
            let _ = k
                .classify(&Limits {
                    dl_work: Some(77_777),
                    ..limits_rooted.clone()
                })
                .unwrap();
            LAST_ROOTED_WORK_LIMIT.with(|c| c.get())
        });
        assert_eq!(lim_override, 77_777);

        // Below 16,384 classes stays at floor 524,288 even with declarations.
        let small = make_ontology(100, true);
        let lim_small = with_rooted_bound_per_class(true, || {
            LAST_ROOTED_WORK_LIMIT.with(|c| c.set(0));
            let mut k = Classifier::of(&small);
            let _ = k.classify(&limits_rooted).unwrap();
            LAST_ROOTED_WORK_LIMIT.with(|c| c.get())
        });
        assert_eq!(lim_small, ROOTED_WORK_BOUND);

        // Counted+rooted preserves DL_WORK_BOUND (does not hit the non-counting rooted branch).
        let mut small_counted = make_ontology(100, true);
        small_counted.push(Axiom::new(AxiomKind::FunctionalObjectProperty(
            ObjectPropertyExpression::Property(ObjectProperty::new("http://example.org/b#p")),
        )));
        let lim_counted = with_rooted_bound_per_class(true, || {
            LAST_ROOTED_WORK_LIMIT.with(|c| c.set(0));
            let mut k = Classifier::of(&small_counted);
            let _ = k
                .classify(&Limits {
                    counting: true,
                    ..limits_rooted.clone()
                })
                .unwrap();
            LAST_ROOTED_WORK_LIMIT.with(|c| c.get())
        });
        assert_eq!(
            lim_counted, 0,
            "counted+rooted run with counting axiom must preserve DL_WORK_BOUND branch"
        );
    }

    #[test]
    fn kept_rewritten_original_range_scan_stops_on_cancel_and_timeout_and_restarts_cleanly() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
             Ontology(
                 DisjointUnion(:C
                     ObjectSomeValuesFrom(:r owl:Thing)
                     ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
                 SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
                 SubObjectPropertyOf(:s owl:topObjectProperty)
                 ObjectPropertyRange(owl:topObjectProperty :T)
             )",
        )
        .unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let c_cls = Class::new("http://ex.org/C");
        let rooted = Limits {
            threads: 1,
            rooted: true,
            strict: true,
            ..Limits::default()
        };
        let baseline = Saturation::new().classify(&kinds, None, &rooted).unwrap();
        let plain_baseline = Saturation::new()
            .classify(&kinds, None, &Limits::default())
            .unwrap();

        // 1. Cancellation raised right inside the original range scan stops the run.
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = cancelled.clone();
        let check = cancelled.clone();
        let hit_cancel_scan = Arc::new(AtomicBool::new(false));
        let hit_cancel_scan_flag = hit_cancel_scan.clone();
        let mut sat = Saturation::new();
        let res = admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::OriginalRangeScan {
                    hit_cancel_scan_flag.store(true, Ordering::Relaxed);
                    trigger.store(true, Ordering::Relaxed);
                }
            },
            || {
                sat.classify(
                    &kinds,
                    None,
                    &Limits {
                        cancelled: Some(Arc::new(move || check.load(Ordering::Relaxed))),
                        ..rooted.clone()
                    },
                )
            },
        );
        assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
        assert!(
            hit_cancel_scan.load(Ordering::Relaxed),
            "expected cancellation to trigger at OriginalRangeScan"
        );
        assert!(sat.reduced);
        if let Some(desc) = sat.describe(&c_cls) {
            for cls in desc
                .equivalents
                .iter()
                .chain(&desc.superclasses)
                .chain(&desc.subclasses)
            {
                assert!(
                    !cls.as_str().starts_with(dl_reduce::PREFIX),
                    "internal class leaked after cancel: {cls:?}"
                );
            }
        }
        // Unrooted restart resets the reduced interner cleanly.
        let after_unrooted = sat.classify(&kinds, None, &Limits::default()).unwrap();
        assert_eq!(after_unrooted.taxonomy, plain_baseline.taxonomy);
        assert_eq!(
            after_unrooted.stats.outside_el,
            plain_baseline.stats.outside_el
        );
        assert!(!sat.reduced);

        // 2. Deadline expiring during the original range scan stops with Timeout.
        let mut sat_timeout = Saturation::new();
        let hit_timeout_scan = Arc::new(AtomicBool::new(false));
        let hit_timeout_scan_flag = hit_timeout_scan.clone();
        let deadline = Instant::now() + Duration::from_millis(500);
        let res = admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::OriginalRangeScan {
                    hit_timeout_scan_flag.store(true, Ordering::Relaxed);
                    while Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            },
            || {
                sat_timeout.classify(
                    &kinds,
                    None,
                    &Limits {
                        deadline: Some(deadline),
                        ..rooted.clone()
                    },
                )
            },
        );
        assert!(matches!(res, Err(Error::Timeout)), "{res:?}");
        assert!(
            hit_timeout_scan.load(Ordering::Relaxed),
            "expected deadline to trigger at OriginalRangeScan"
        );
        assert!(sat_timeout.reduced);
        let after_rooted = sat_timeout.classify(&kinds, None, &rooted).unwrap();
        assert_eq!(after_rooted.taxonomy, baseline.taxonomy);
        assert_eq!(after_rooted.stats.outside_el, baseline.stats.outside_el);
        assert_eq!(after_rooted.stats.outside_el.horn, 1);
        assert_eq!(after_rooted.stats.outside_el.taken, 2);

        // 3. Pool refusal specifically at an admission inside OriginalRangeScan.
        let mut wide_ofn = String::from(
            "Prefix(:=<http://ex.org/>)
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
             Ontology(
                 DisjointUnion(:C
                     ObjectSomeValuesFrom(:r owl:Thing)
                     ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
                 SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
                 SubObjectPropertyOf(:s :chain0)\n",
        );
        for i in 0..3999 {
            wide_ofn.push_str(&format!("SubObjectPropertyOf(:chain{i} :chain{})\n", i + 1));
        }
        wide_ofn.push_str("SubObjectPropertyOf(:chain3999 owl:topObjectProperty)\n");
        wide_ofn.push_str("ObjectPropertyRange(owl:topObjectProperty :T0)\n");
        wide_ofn.push(')');
        let wide_doc = crate::ofn_read::parse(&wide_ofn).unwrap();
        let wide_kinds: Vec<&AxiomKind> = wide_doc.axioms.iter().map(|a| &a.kind).collect();
        let in_scan = Arc::new(AtomicBool::new(false));
        let in_scan_hook = in_scan.clone();
        let in_scan_charge = in_scan.clone();
        let refused_in_scan = Arc::new(AtomicBool::new(false));
        let refused_in_scan_flag = refused_in_scan.clone();
        let mut sat_scan_refuse = Saturation::new();
        let res = admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::OriginalRangeScan {
                    in_scan_hook.store(true, Ordering::Relaxed);
                }
            },
            || {
                sat_scan_refuse.classify(
                    &wide_kinds,
                    None,
                    &Limits {
                        charge: Some(Arc::new(move |_| {
                            if in_scan_charge.load(Ordering::Relaxed) {
                                refused_in_scan_flag.store(true, Ordering::Relaxed);
                                return false;
                            }
                            true
                        })),
                        ..rooted.clone()
                    },
                )
            },
        );
        assert!(matches!(res, Err(Error::Memory { .. })), "{res:?}");
        assert!(
            in_scan.load(Ordering::Relaxed) && refused_in_scan.load(Ordering::Relaxed),
            "expected pool refusal specifically inside OriginalRangeScan"
        );
    }

    #[test]
    fn shared_facts_dl1061_suite_and_el_invariance() {
        // 1. All 9 dl1061 fixtures pass with shared_facts ON across cautious/eager,
        //    1t/4t, dl_steps true/false, and rooted_marker_repair off/on, matching OFF.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl1061");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "ofn"))
            .collect();
        files.sort();
        assert_eq!(files.len(), 9);
        let z = Class::new("http://example.org/n#Z");
        let ans = Class::new("http://example.org/n#Ans");
        let mut repair_off_checked = 0usize;
        let mut repair_on_checked = 0usize;
        for path in &files {
            let ax = crate::ofn_read::parse(&std::fs::read_to_string(path).unwrap())
                .unwrap()
                .axioms;
            let name = path.file_name().unwrap().to_string_lossy();
            for (threads, expansion, dl_steps, repair) in [
                (1, Expansion::Cautious, true, false),
                (4, Expansion::Cautious, false, false),
                (1, Expansion::Eager, true, false),
                (1, Expansion::Cautious, true, true),
                (4, Expansion::Eager, false, true),
            ] {
                let limits = Limits {
                    threads,
                    expansion,
                    dl_steps,
                    rooted: true,
                    rooted_marker_repair: repair,
                    ..Limits::default()
                };
                let off = dl_root::with_marker_diag_override(repair, || {
                    dl_rules::with_shared_facts_override(false, || {
                        let mut k = Classifier::of(&ax);
                        (k.classify(&limits).unwrap(), k.excluded().len())
                    })
                });
                let on = dl_root::with_marker_diag_override(repair, || {
                    dl_rules::with_shared_facts_override(true, || {
                        let mut k = Classifier::of(&ax);
                        (k.classify(&limits).unwrap(), k.excluded().len())
                    })
                });
                assert_eq!(off.0.consistency, on.0.consistency, "{name}");
                assert_eq!(off.0.complete, on.0.complete, "{name}");
                assert_eq!(off.1, on.1, "{name} excluded");
                assert_eq!(off.0.stats.tier, Tier::Disjunctive, "{name} off tier");
                assert_eq!(on.0.stats.tier, Tier::Disjunctive, "{name} on tier");
                assert!(off.0.stats.dl_work > 0 && on.0.stats.dl_work > 0, "{name}");
                if repair {
                    repair_on_checked += 1;
                    assert!(
                        off.0.complete && on.0.complete,
                        "{name} ({threads}t {expansion:?} steps={dl_steps} repair=true): must complete"
                    );
                    assert_eq!(
                        off.1, 0,
                        "{name} ({threads}t {expansion:?} steps={dl_steps} repair=true): must have 0 exclusions"
                    );
                    assert!(
                        !on.0.stats.dl_rooted_declined && !on.0.stats.dl_over_budget,
                        "{name}: must not decline or exhaust budget with repair=true"
                    );
                    assert!(
                        off.0.taxonomy.is_subclass(&z, &ans),
                        "{name}: OFF must entail Z ⊑ Ans with repair=true"
                    );
                    assert!(
                        on.0.taxonomy.is_subclass(&z, &ans),
                        "{name} ({threads}t {expansion:?} steps={dl_steps} repair=true): misses Z ⊑ Ans"
                    );
                    assert_eq!(
                        on.0.stats.dl_marker_deficits,
                        [0, 0, 0],
                        "{name}: real marker deficits must be zero with repair=true"
                    );
                } else if on.0.complete && on.1 == 0 {
                    repair_off_checked += 1;
                    assert!(
                        off.0.taxonomy.is_subclass(&z, &ans),
                        "{name} ({threads}t {expansion:?} steps={dl_steps} repair=false): OFF misses Z ⊑ Ans"
                    );
                    assert!(
                        on.0.taxonomy.is_subclass(&z, &ans),
                        "{name} ({threads}t {expansion:?} steps={dl_steps} repair=false): ON misses Z ⊑ Ans"
                    );
                }
            }
        }
        assert_eq!(
            repair_on_checked,
            files.len() * 2,
            "every repair=true run across all 9 dl1061 fixtures must complete and be verified"
        );
        assert!(
            repair_off_checked > 0,
            "repair=false checks must be non-vacuous"
        );

        // 2. Zero EL and non-rooted DL regression: identical taxonomy and work.
        let el_ofn = "Prefix(:=<http://example.org/el#>)\nOntology(<http://example.org/el>\n  SubClassOf(:A :B)\n  SubClassOf(:B :C)\n  SubClassOf(:C ObjectSomeValuesFrom(:r :D))\n)";
        let el_ax = crate::ofn_read::parse(el_ofn).unwrap().axioms;
        let el_off = dl_rules::with_shared_facts_override(false, || {
            Classifier::of(&el_ax).classify(&Limits::default()).unwrap()
        });
        let el_on = dl_rules::with_shared_facts_override(true, || {
            Classifier::of(&el_ax).classify(&Limits::default()).unwrap()
        });
        assert_eq!(el_off.stats.dl_work, 0);
        assert_eq!(el_on.stats.dl_work, 0);
        assert_eq!(
            el_off.taxonomy.is_subclass(
                &Class::new("http://example.org/el#A"),
                &Class::new("http://example.org/el#C")
            ),
            el_on.taxonomy.is_subclass(
                &Class::new("http://example.org/el#A"),
                &Class::new("http://example.org/el#C")
            )
        );
    }

    #[test]
    fn post_run_scratch_respects_paid_offset_and_partial_stop_flag() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
             Ontology(
                 SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
                 SubObjectPropertyOf(:s owl:topObjectProperty)
                 ObjectPropertyRange(owl:topObjectProperty :B)
                 ObjectPropertyRange(owl:topObjectProperty :A)
             )",
        )
        .unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();

        // When check_stop is false (partial answer), an already-expired deadline and
        // active cancellation do not abort original outside_el accounting.
        let charged = Arc::new(AtomicUsize::new(0));
        let counter = charged.clone();
        let limits = Limits {
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            cancelled: Some(Arc::new(|| true)),
            charge: Some(Arc::new(move |bytes| {
                counter.fetch_add(bytes, Ordering::Relaxed);
                true
            })),
            ..Limits::default()
        };
        let charge_ref = limits
            .charge
            .as_deref()
            .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
        let mut peak = 2000usize;
        let mut paid = 1000usize;
        let mut scratch = PostRunScratch {
            base: 1000,
            peak: &mut peak,
            paid: &mut paid,
            limits: &limits,
            charge: charge_ref,
            check_stop: false,
            scratch: 0,
        };
        let out = dl_reduce::Reduction::outside_el(&kinds, &[], &mut scratch).unwrap();
        assert_eq!(out.horn, 1);
        assert_eq!(out.taken, 1);
        // Only paid (1000), not refused peak (2000), offset the pool charge.
        assert!(charged.load(Ordering::Relaxed) > 0);
        assert_eq!(paid, 1000 + charged.load(Ordering::Relaxed));

        // When check_stop is true, the same expired deadline fails immediately.
        let mut peak2 = 1000usize;
        let mut paid2 = 1000usize;
        let mut scratch2 = PostRunScratch {
            base: 1000,
            peak: &mut peak2,
            paid: &mut paid2,
            limits: &limits,
            charge: charge_ref,
            check_stop: true,
            scratch: 0,
        };
        let err = dl_reduce::Reduction::outside_el(&kinds, &[], &mut scratch2);
        assert!(matches!(err, Err(Error::Timeout)), "{err:?}");

        // build_inner mode (`need_first = true`) selects the lexicographically smallest
        // missing range (`:A` before `:B`), matching the prior BTreeSet ordering.
        let unlimited = Limits::default();
        let mut peak3 = 0usize;
        let mut paid3 = 0usize;
        let mut scratch3 = PostRunScratch {
            base: 0,
            peak: &mut peak3,
            paid: &mut paid3,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let chains = [kinds[0]];
        let missing =
            check_chain_ranges(&kinds, |_, _| false, &chains, true, &mut scratch3).unwrap();
        let Some((expr, sup, last)) = missing[0] else {
            panic!("expected missing range for chain");
        };
        assert_eq!(expr, &ClassExpression::Class(Class::new("http://ex.org/A")));
        assert_eq!(sup, "http://ex.org/s");
        assert_eq!(last, "http://ex.org/q");
    }

    #[test]
    fn map_and_set_admit_cover_resize_overlap() {
        let est = |cap: usize, entry: usize| {
            let b = cap * entry;
            b + b / 7
        };
        let map_entry = std::mem::size_of::<(&str, usize)>() + 1;
        let old_map = est(14, map_entry);
        let new_map = est(28, map_entry);
        let overlap_map = old_map + new_map;
        let mid_map = new_map + old_map / 2;

        // Cap admitting the replacement table alone must refuse the 14 -> 28 overlap.
        let mut m: FxHashMap<&str, usize> = FxHashMap::default();
        let limits = Limits {
            max_memory: Some(mid_map),
            ..Limits::default()
        };
        let mut peak = 0usize;
        let mut paid = 0usize;
        let mut scratch = PostRunScratch {
            base: 0,
            peak: &mut peak,
            paid: &mut paid,
            limits: &limits,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let keys: Vec<String> = (0..15).map(|i| format!("k{i}")).collect();
        for (i, k) in keys.iter().take(14).enumerate() {
            map_admit(&mut m, &mut scratch).unwrap();
            m.insert(k.as_str(), i);
        }
        assert_eq!(m.capacity(), 14);
        assert_eq!(scratch.scratch, old_map);
        let refused = map_admit(&mut m, &mut scratch);
        assert!(
            matches!(refused, Err(Error::Memory { .. })),
            "expected resize overlap refusal under cap={mid_map}, got {refused:?}"
        );
        assert_eq!(m.capacity(), 14);

        // Cap covering old + replacement succeeds and releases the old table afterward.
        let limits_ok = Limits {
            max_memory: Some(overlap_map),
            ..Limits::default()
        };
        let mut peak_ok = 0usize;
        let mut paid_ok = 0usize;
        let mut scratch_ok = PostRunScratch {
            base: 0,
            peak: &mut peak_ok,
            paid: &mut paid_ok,
            limits: &limits_ok,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let mut m2: FxHashMap<&str, usize> = FxHashMap::default();
        for (i, k) in keys.iter().enumerate() {
            map_admit(&mut m2, &mut scratch_ok).unwrap();
            m2.insert(k.as_str(), i);
        }
        assert_eq!(m2.capacity(), 28);
        assert_eq!(scratch_ok.scratch, new_map);
        assert_eq!(peak_ok, overlap_map);

        // Same 14 -> 28 overlap check for set_admit.
        let set_entry = std::mem::size_of::<&str>() + 1;
        let old_set = est(14, set_entry);
        let new_set = est(28, set_entry);
        let overlap_set = old_set + new_set;
        let mid_set = new_set + old_set / 2;
        let limits_set = Limits {
            max_memory: Some(mid_set),
            ..Limits::default()
        };
        let mut peak_s = 0usize;
        let mut paid_s = 0usize;
        let mut scratch_s = PostRunScratch {
            base: 0,
            peak: &mut peak_s,
            paid: &mut paid_s,
            limits: &limits_set,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let mut s: FxHashSet<&str> = FxHashSet::default();
        for k in keys.iter().take(14) {
            set_admit(&mut s, &mut scratch_s).unwrap();
            s.insert(k.as_str());
        }
        assert_eq!(s.capacity(), 14);
        assert!(matches!(
            set_admit(&mut s, &mut scratch_s),
            Err(Error::Memory { .. })
        ));
        let limits_set_ok = Limits {
            max_memory: Some(overlap_set),
            ..Limits::default()
        };
        let mut peak_s2 = 0usize;
        let mut paid_s2 = 0usize;
        let mut scratch_s2 = PostRunScratch {
            base: 0,
            peak: &mut peak_s2,
            paid: &mut paid_s2,
            limits: &limits_set_ok,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let mut s2: FxHashSet<&str> = FxHashSet::default();
        for k in &keys {
            set_admit(&mut s2, &mut scratch_s2).unwrap();
            s2.insert(k.as_str());
        }
        assert_eq!(s2.capacity(), 28);
        assert_eq!(scratch_s2.scratch, new_set);
        assert_eq!(peak_s2, overlap_set);

        // Same 8 -> 16 overlap check for vec_reserve.
        let old_vec = 8 * std::mem::size_of::<usize>();
        let new_vec = 16 * std::mem::size_of::<usize>();
        let overlap_vec = old_vec + new_vec;
        let mid_vec = new_vec + old_vec / 2;
        let limits_vec = Limits {
            max_memory: Some(mid_vec),
            ..Limits::default()
        };
        let mut peak_v = 0usize;
        let mut paid_v = 0usize;
        let mut scratch_v = PostRunScratch {
            base: 0,
            peak: &mut peak_v,
            paid: &mut paid_v,
            limits: &limits_vec,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let mut v: Vec<usize> = Vec::new();
        for i in 0..8 {
            vec_reserve(&mut v, 1, &mut scratch_v).unwrap();
            v.push(i);
        }
        assert_eq!(v.capacity(), 8);
        assert_eq!(scratch_v.scratch, old_vec);
        assert!(matches!(
            vec_reserve(&mut v, 1, &mut scratch_v),
            Err(Error::Memory { .. })
        ));
        assert_eq!(v.capacity(), 8);
        let limits_vec_ok = Limits {
            max_memory: Some(overlap_vec),
            ..Limits::default()
        };
        let mut peak_v2 = 0usize;
        let mut paid_v2 = 0usize;
        let mut scratch_v2 = PostRunScratch {
            base: 0,
            peak: &mut peak_v2,
            paid: &mut paid_v2,
            limits: &limits_vec_ok,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let mut v2: Vec<usize> = Vec::new();
        for i in 0..9 {
            vec_reserve(&mut v2, 1, &mut scratch_v2).unwrap();
            v2.push(i);
        }
        assert_eq!(v2.capacity(), 16);
        assert_eq!(scratch_v2.scratch, new_vec);
        assert_eq!(peak_v2, overlap_vec);
    }

    #[test]
    fn build_inner_refreshes_baseline_for_wide_final_axiom_parallel_merge_and_missing_overlap() {
        // 1. Wide final serial axiom (EquivalentClasses of 2,000 named classes) followed by
        //    chain range scan: the scan must see the post-normalization live baseline, not
        //    the stale pre-final-axiom baseline.
        let mut wide_classes = String::new();
        for i in 0..2000 {
            wide_classes.push_str(&format!(" :C{i}"));
        }
        let mut body_no_chain =
            String::from("Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:r :T)\n");
        for i in 0..500 {
            body_no_chain.push_str(&format!("SubObjectPropertyOf(:extra{i} :r)\n"));
        }
        body_no_chain.push_str(&format!("EquivalentClasses({wide_classes})\n)"));
        let mut body_with_chain = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
             SubObjectPropertyOf(:s :r)\n\
             ObjectPropertyRange(:r :T)\n",
        );
        for i in 0..500 {
            body_with_chain.push_str(&format!("SubObjectPropertyOf(:extra{i} :r)\n"));
        }
        body_with_chain.push_str(&format!("EquivalentClasses({wide_classes})\n)"));

        let doc_no = crate::ofn_read::parse(&body_no_chain).unwrap();
        let doc_with = crate::ofn_read::parse(&body_with_chain).unwrap();
        let kinds_no: Vec<&AxiomKind> = doc_no.axioms.iter().map(|a| &a.kind).collect();
        let kinds_with: Vec<&AxiomKind> = doc_with.axioms.iter().map(|a| &a.kind).collect();
        let serial = Limits {
            threads: 1,
            ..Limits::default()
        };
        let (_, _, peak_no, _, _) = Saturation::new()
            .build(
                &kinds_no,
                &[],
                1,
                0,
                0,
                &serial,
                AttemptProvenance::OriginalDefault,
            )
            .unwrap();
        let (_, _, peak_with, _, _) = Saturation::new()
            .build(
                &kinds_with,
                &[],
                1,
                0,
                0,
                &serial,
                AttemptProvenance::OriginalDefault,
            )
            .unwrap();
        assert!(
            peak_with > peak_no + 32 * 1024,
            "scan did not coexist with wide final axiom baseline: with={peak_with} no={peak_no}"
        );
        let cap_wide = peak_no + (peak_with - peak_no) / 2;
        let wide_reached_scan = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wide_reached_scan_h = wide_reached_scan.clone();
        let err_wide = admission_test::with_hook(
            move |pt| {
                if matches!(pt, admission_test::Point::BuildRangeScanStart { .. }) {
                    wide_reached_scan_h.store(true, Ordering::Relaxed);
                }
            },
            || {
                Saturation::new()
                    .build(
                        &kinds_with,
                        &[],
                        1,
                        0,
                        0,
                        &Limits {
                            max_memory: Some(cap_wide),
                            ..serial.clone()
                        },
                        AttemptProvenance::OriginalDefault,
                    )
                    .err()
            },
        );
        assert!(
            matches!(err_wide, Some(Error::Memory { .. })),
            "expected Memory refusal at wide-final-axiom + scan overlap cap={cap_wide}, got {err_wide:?}"
        );
        assert!(
            wide_reached_scan.load(Ordering::Relaxed),
            "expected wide-final-axiom cap refusal during scan after BuildRangeScanStart"
        );

        // 2. Populated missing-range vector must keep its reservation through consumption
        //    while chain signatures and diagnostics are built.
        let mut body_miss =
            String::from("Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:s :T)\n");
        for i in 0..2000 {
            body_miss.push_str(&format!(
                "SubObjectPropertyOf(ObjectPropertyChain(:p{i} :q) :s)\n"
            ));
        }
        body_miss.push(')');
        let doc_miss = crate::ofn_read::parse(&body_miss).unwrap();
        let kinds_miss: Vec<&AxiomKind> = doc_miss.axioms.iter().map(|a| &a.kind).collect();
        let mut sat_miss = Saturation::new();
        let consume_start_peak = std::sync::Arc::new(AtomicUsize::new(0));
        let consume_start_peak_h = consume_start_peak.clone();
        let consume_end_peak = std::sync::Arc::new(AtomicUsize::new(0));
        let consume_end_peak_h = consume_end_peak.clone();
        let (ax_miss, _, peak_miss, _, _) = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::BuildMissingConsumeStart { peak, .. } => {
                    consume_start_peak_h.store(peak, Ordering::Relaxed);
                }
                admission_test::Point::BuildMissingConsumeEnd { peak } => {
                    consume_end_peak_h.store(peak, Ordering::Relaxed);
                }
                _ => {}
            },
            || {
                sat_miss
                    .build(
                        &kinds_miss,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        let post_base = sat_miss.interner.bytes() + ax_miss.bytes();
        let missing_vec_bytes = 2000 * std::mem::size_of::<Option<MissingRange<'_>>>();
        let before_consume_peak = consume_start_peak.load(Ordering::Relaxed);
        let after_consume_peak = consume_end_peak.load(Ordering::Relaxed);
        assert!(before_consume_peak > 0 && after_consume_peak > 0);
        assert!(
            peak_miss >= post_base + missing_vec_bytes,
            "build peak did not cover overlap of missing vector ({missing_vec_bytes}) with interned signatures and diagnostics (post_base={post_base}, peak={peak_miss})"
        );
        assert!(
            after_consume_peak > before_consume_peak + missing_vec_bytes / 2,
            "expected missing-result consumption to push peak past scan peak: before={before_consume_peak}, after={after_consume_peak}"
        );
        let cap_miss = before_consume_peak + (after_consume_peak - before_consume_peak) / 2;
        let miss_started_consume = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let miss_finished_consume = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let miss_start_h = miss_started_consume.clone();
        let miss_end_h = miss_finished_consume.clone();
        let err_miss = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::BuildMissingConsumeStart { .. } => {
                    miss_start_h.store(true, Ordering::Relaxed);
                }
                admission_test::Point::BuildMissingConsumeEnd { .. } => {
                    miss_end_h.store(true, Ordering::Relaxed);
                }
                _ => {}
            },
            || {
                Saturation::new()
                    .build(
                        &kinds_miss,
                        &[],
                        1,
                        0,
                        0,
                        &Limits {
                            max_memory: Some(cap_miss),
                            ..serial.clone()
                        },
                        AttemptProvenance::OriginalDefault,
                    )
                    .err()
            },
        );
        assert!(
            matches!(err_miss, Some(Error::Memory { .. })),
            "expected Memory refusal at missing-vector consumption overlap cap={cap_miss}, got {err_miss:?}"
        );
        assert!(
            miss_started_consume.load(Ordering::Relaxed)
                && !miss_finished_consume.load(Ordering::Relaxed),
            "expected refusal specifically during missing-result consumption"
        );

        // 3. Parallel normalization must refresh live baseline after dropping chunk allowances.
        let mut body_par = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
             SubObjectPropertyOf(:s :r)\n\
             ObjectPropertyRange(:r :T)\n",
        );
        for i in 0..(PARALLEL_AXIOMS + 128) {
            body_par.push_str(&format!("SubClassOf(:A{i} :B{i})\n"));
        }
        body_par.push(')');
        let doc_par = crate::ofn_read::parse(&body_par).unwrap();
        let kinds_par: Vec<&AxiomKind> = doc_par.axioms.iter().map(|a| &a.kind).collect();
        let parallel = Limits {
            threads: 2,
            ..Limits::default()
        };
        let par_scan_base = std::sync::Arc::new(AtomicUsize::new(0));
        let par_scan_base_h = par_scan_base.clone();
        let mut sat_par = Saturation::new();
        let (_, _, peak_par, _, _) = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::BuildRangeScanStart { base, .. } = pt {
                    par_scan_base_h.store(base, Ordering::Relaxed);
                }
            },
            || {
                sat_par
                    .build(
                        &kinds_par,
                        &[],
                        2,
                        0,
                        0,
                        &parallel,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        let ser_scan_base = std::sync::Arc::new(AtomicUsize::new(0));
        let ser_scan_base_h = ser_scan_base.clone();
        let mut sat_par_ser = Saturation::new();
        let (_, _, peak_ser, _, _) = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::BuildRangeScanStart { base, .. } = pt {
                    ser_scan_base_h.store(base, Ordering::Relaxed);
                }
            },
            || {
                sat_par_ser
                    .build(
                        &kinds_par,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        assert!(peak_par >= peak_ser);
        // Parallel scan baseline must reflect the merged live state (not stale chunk+merge
        // gauge allowances, which exceed the merged live baseline by >50%).
        let par_base = par_scan_base.load(Ordering::Relaxed);
        let ser_base = ser_scan_base.load(Ordering::Relaxed);
        assert!(par_base > 0 && ser_base > 0);
        assert!(
            par_base < peak_par,
            "parallel scan baseline ({par_base}) retained stale parallel peak ({peak_par})"
        );
        assert!(
            par_base <= ser_base + 4096,
            "parallel scan baseline ({par_base}) diverged from serial live baseline ({ser_base})"
        );
    }

    #[test]
    fn kept_rewritten_profile_check_is_allocation_free_and_polled() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Sol r3 fixture: kept Horn ontology extended with a huge ObjectIntersectionOf of
        // ObjectMinCardinality(0 owl:topObjectProperty ObjectSomeValuesFrom(:p_i owl:Thing)).
        const N_OPS: usize = 2500;
        let mut body = String::from(
            "Prefix(:=<http://ex.org/>)
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
             Ontology(
                 SubClassOf(:Z ObjectIntersectionOf(",
        );
        for i in 0..N_OPS {
            body.push_str(&format!(
                " ObjectMinCardinality(0 owl:topObjectProperty ObjectSomeValuesFrom(:p{i} owl:Thing))"
            ));
        }
        body.push_str("))\n)");
        let doc = crate::ofn_read::parse(&body).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();

        // Single huge valid EL axiom (2,500 conjuncts) must poll mid-expression.
        let mut el_body = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\nSubClassOf(:W ObjectIntersectionOf(",
        );
        for i in 0..N_OPS {
            el_body.push_str(&format!(" :C{i}"));
        }
        el_body.push_str("))\n)");
        let el_doc = crate::ofn_read::parse(&el_body).unwrap();
        let el_kinds: Vec<&AxiomKind> = el_doc.axioms.iter().map(|a| &a.kind).collect();

        let el_polls = Arc::new(AtomicUsize::new(0));
        let el_polls_c = el_polls.clone();
        let limits_el = Limits {
            cancelled: Some(Arc::new(move || {
                el_polls_c.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        };
        let mut peak = 0usize;
        let mut paid = 0usize;
        let mut scratch = PostRunScratch {
            base: 0,
            peak: &mut peak,
            paid: &mut paid,
            limits: &limits_el,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let out_el = dl_reduce::Reduction::outside_el(&el_kinds, &[], &mut scratch).unwrap();
        assert_eq!(out_el, OutsideEl::default());
        let el_full_polls = el_polls.load(Ordering::Relaxed);
        assert!(
            el_full_polls >= 5,
            "expected mid-expression polling during EL profile check, saw only {el_full_polls} polls"
        );
        // Cancellation returning true mid-expression aborts before finishing the walk.
        let el_cancel_polls = Arc::new(AtomicUsize::new(0));
        let el_cancel_c = el_cancel_polls.clone();
        let limits_el_cancel = Limits {
            cancelled: Some(Arc::new(move || {
                el_cancel_c.fetch_add(1, Ordering::Relaxed) >= 3
            })),
            ..Limits::default()
        };
        let mut peak_ec = 0usize;
        let mut paid_ec = 0usize;
        let mut scratch_ec = PostRunScratch {
            base: 0,
            peak: &mut peak_ec,
            paid: &mut paid_ec,
            limits: &limits_el_cancel,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        assert!(matches!(
            dl_reduce::Reduction::outside_el(&el_kinds, &[], &mut scratch_ec),
            Err(Error::Cancelled)
        ));
        assert!(el_cancel_polls.load(Ordering::Relaxed) < el_full_polls);

        // On the wide ObjectMinCardinality(0 owl:topObjectProperty ...) axiom,
        // outside_el must poll mid-expression during hashing and dl::shape, allocate no
        // diagnostic/signature tables, and succeed under a tight memory cap.
        let horn_polls = Arc::new(AtomicUsize::new(0));
        let horn_polls_c = horn_polls.clone();
        let limits_horn = Limits {
            max_memory: Some(2048),
            cancelled: Some(Arc::new(move || {
                horn_polls_c.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        };
        let mut peak2 = 0usize;
        let mut paid2 = 0usize;
        let mut scratch2 = PostRunScratch {
            base: 0,
            peak: &mut peak2,
            paid: &mut paid2,
            limits: &limits_horn,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let polls_at_shape_start = Arc::new(AtomicUsize::new(0));
        let polls_at_shape_start_h = polls_at_shape_start.clone();
        let horn_polls_h = horn_polls.clone();
        let (out_horn, sample_horn) = a1_probe_alloc::measure(|| {
            admission_test::with_hook(
                move |pt| {
                    if pt == admission_test::Point::OutsideElBeforeShape {
                        polls_at_shape_start_h
                            .store(horn_polls_h.load(Ordering::Relaxed), Ordering::Relaxed);
                    }
                },
                || dl_reduce::Reduction::outside_el(&kinds, &[], &mut scratch2).unwrap(),
            )
        });
        assert_eq!(out_horn.horn, 1);
        assert_eq!(out_horn.disjunctive, 0);
        assert_eq!(out_horn.taken, 1);
        assert!(
            sample_horn.peak < 4096,
            "wide original profile/shape check peaked at {} bytes (expected tiny hash table only)",
            sample_horn.peak
        );
        let horn_full_polls = horn_polls.load(Ordering::Relaxed);
        let shape_start_polls = polls_at_shape_start.load(Ordering::Relaxed);
        assert!(
            shape_start_polls >= 4,
            "expected hashing polls before shape check, saw {shape_start_polls}"
        );
        assert!(
            horn_full_polls >= shape_start_polls + 5,
            "expected mid-expression polling inside dl::shape_polled itself: shape_start={shape_start_polls}, full={horn_full_polls}"
        );
        // Trigger cancellation strictly AFTER OutsideElBeforeShape so hashing alone
        // cannot satisfy the cancellation assertion.
        let shape_armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shape_armed_h = shape_armed.clone();
        let shape_armed_c = shape_armed.clone();
        let shape_post_polls = Arc::new(AtomicUsize::new(0));
        let shape_post_polls_c = shape_post_polls.clone();
        let horn_cancel_polls = Arc::new(AtomicUsize::new(0));
        let horn_cancel_c = horn_cancel_polls.clone();
        let limits_horn_cancel = Limits {
            cancelled: Some(Arc::new(move || {
                horn_cancel_c.fetch_add(1, Ordering::Relaxed);
                shape_armed_c.load(Ordering::Relaxed)
                    && shape_post_polls_c.fetch_add(1, Ordering::Relaxed) >= 2
            })),
            ..Limits::default()
        };
        let mut peak_hc = 0usize;
        let mut paid_hc = 0usize;
        let mut scratch_hc = PostRunScratch {
            base: 0,
            peak: &mut peak_hc,
            paid: &mut paid_hc,
            limits: &limits_horn_cancel,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let res_hc = admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::OutsideElBeforeShape {
                    shape_armed_h.store(true, Ordering::Relaxed);
                }
            },
            || dl_reduce::Reduction::outside_el(&kinds, &[], &mut scratch_hc),
        );
        assert!(matches!(res_hc, Err(Error::Cancelled)));
        assert!(
            shape_armed.load(Ordering::Relaxed),
            "expected OutsideElBeforeShape before shape-walk cancellation"
        );
        let cancel_total = horn_cancel_polls.load(Ordering::Relaxed);
        assert!(
            cancel_total > shape_start_polls && cancel_total < horn_full_polls,
            "expected cancellation inside shape walk: start={shape_start_polls}, cancel={cancel_total}, full={horn_full_polls}"
        );

        // End-to-end kept-Horn classification with the kept Horn fixture extended with
        // the wide rewritten intersection succeeds on the reduced path.
        let mut full_body = String::from(
            "Prefix(:=<http://ex.org/>)
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
             Ontology(
                 DisjointUnion(:C
                     ObjectSomeValuesFrom(:r owl:Thing)
                     ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
                 SubClassOf(:Z ObjectIntersectionOf(",
        );
        for i in 0..N_OPS {
            full_body.push_str(&format!(
                " ObjectMinCardinality(0 owl:topObjectProperty ObjectSomeValuesFrom(:p{i} owl:Thing))"
            ));
        }
        full_body.push_str("))\n)");
        let full_doc = crate::ofn_read::parse(&full_body).unwrap();
        let full_kinds: Vec<&AxiomKind> = full_doc.axioms.iter().map(|a| &a.kind).collect();
        let rooted = Limits {
            threads: 1,
            rooted: true,
            strict: true,
            ..Limits::default()
        };
        let mut sat_full = Saturation::new();
        let res = sat_full.classify(&full_kinds, None, &rooted).unwrap();
        assert!(sat_full.reduced, "expected kept rewritten path");
        assert_eq!(res.stats.tier, Tier::Disjunctive);
        assert_eq!(res.stats.outside_el.disjunctive, 1);
        assert_eq!(res.stats.outside_el.horn, 1);
        assert_eq!(res.stats.outside_el.taken, 2);
    }

    #[test]
    fn check_chain_ranges_polls_sort_group_and_fill_phases() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        const N_CHAINS: usize = 3000;
        let mut body =
            String::from("Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:s :T)\n");
        for i in 0..N_CHAINS {
            body.push_str(&format!(
                "SubObjectPropertyOf(ObjectPropertyChain(:p{i} :q) :s)\n"
            ));
        }
        body.push(')');
        let doc = crate::ofn_read::parse(&body).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let chains: Vec<&AxiomKind> = kinds[1..].to_vec();

        // Cancel inside each phase and assert the phase stops before its end marker.
        for (start_pt, end_pt, label) in [
            (
                admission_test::Point::OriginalRangeSort,
                admission_test::Point::OriginalRangeAfterSort,
                "sort",
            ),
            (
                admission_test::Point::OriginalRangeGroup,
                admission_test::Point::OriginalRangeAfterGroup,
                "group",
            ),
            (
                admission_test::Point::OriginalRangeSubGroup,
                admission_test::Point::OriginalRangeAfterSubGroup,
                "sub-group",
            ),
            (
                admission_test::Point::OriginalRangeFill,
                admission_test::Point::OriginalRangeAfterFill,
                "fill",
            ),
        ] {
            let cancelled = Arc::new(AtomicBool::new(false));
            let trigger = cancelled.clone();
            let check = cancelled.clone();
            let hit_start = Arc::new(AtomicBool::new(false));
            let hit_start_c = hit_start.clone();
            let hit_end = Arc::new(AtomicBool::new(false));
            let hit_end_c = hit_end.clone();
            let limits = Limits {
                cancelled: Some(Arc::new(move || check.load(Ordering::Relaxed))),
                ..Limits::default()
            };
            let mut peak = 0usize;
            let mut paid = 0usize;
            let mut scratch = PostRunScratch {
                base: 0,
                peak: &mut peak,
                paid: &mut paid,
                limits: &limits,
                charge: None,
                check_stop: true,
                scratch: 0,
            };
            let res = admission_test::with_hook(
                move |pt| {
                    if pt == start_pt {
                        hit_start_c.store(true, Ordering::Relaxed);
                        trigger.store(true, Ordering::Relaxed);
                    } else if pt == end_pt {
                        hit_end_c.store(true, Ordering::Relaxed);
                    }
                },
                || check_chain_ranges(&kinds, |_, _| false, &chains, false, &mut scratch),
            );
            assert!(
                matches!(res, Err(Error::Cancelled)),
                "{label}: expected Cancelled, got {res:?}"
            );
            assert!(hit_start.load(Ordering::Relaxed), "{label}: start not hit");
            assert!(
                !hit_end.load(Ordering::Relaxed),
                "{label}: phase ran to completion without cooperative polling"
            );
        }

        // Without cancellation, all 3,000 chains report the missing range.
        let unlimited = Limits::default();
        let mut peak = 0usize;
        let mut paid = 0usize;
        let mut scratch = PostRunScratch {
            base: 0,
            peak: &mut peak,
            paid: &mut paid,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let out = check_chain_ranges(&kinds, |_, _| false, &chains, false, &mut scratch).unwrap();
        assert_eq!(out.len(), N_CHAINS);
        assert!(out.iter().all(|m| m.is_some()));

        // Multi-key unsorted input exercises `sort_entries_polled` across distinct `(last, sup)`
        // keys and verifies both direct sorted order and exact per-chain violation results.
        let mut multi_body = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             ObjectPropertyRange(:s0 :T0)\n\
             ObjectPropertyRange(:s1 :T1)\n\
             ObjectPropertyRange(:q0 :T0)\n",
        );
        // Interleave keys in reverse order: (:q1, :s1) violates, (:q1, :s0) violates,
        // (:q0, :s1) violates, (:q0, :s0) conforms.
        let pairs = [
            (":q1", ":s1", true),
            (":q0", ":s0", false),
            (":q1", ":s0", true),
            (":q0", ":s1", true),
            (":q0", ":s0", false),
            (":q1", ":s1", true),
        ];
        for (i, (q, s, _)) in pairs.iter().enumerate() {
            multi_body.push_str(&format!(
                "SubObjectPropertyOf(ObjectPropertyChain(:p{i} {q}) {s})\n"
            ));
        }
        multi_body.push(')');
        let multi_doc = crate::ofn_read::parse(&multi_body).unwrap();
        let multi_kinds: Vec<&AxiomKind> = multi_doc.axioms.iter().map(|a| &a.kind).collect();
        let multi_chains: Vec<&AxiomKind> = multi_kinds[3..].to_vec();
        let mut peak_m = 0usize;
        let mut paid_m = 0usize;
        let mut scratch_m = PostRunScratch {
            base: 0,
            peak: &mut peak_m,
            paid: &mut paid_m,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        // Direct sorted-order assertion on `sort_entries_polled` with the interleaved input.
        let mut direct_entries: Vec<(&str, &str, usize)> = pairs
            .iter()
            .enumerate()
            .map(|(idx, &(q, s, _))| (q, s, idx))
            .collect();
        let mut sort_steps = 0usize;
        sort_entries_polled(&mut direct_entries, &mut sort_steps, &scratch_m).unwrap();
        let sorted_keys: Vec<(&str, &str)> =
            direct_entries.iter().map(|&(q, s, _)| (q, s)).collect();
        assert_eq!(
            sorted_keys,
            vec![
                (":q0", ":s0"),
                (":q0", ":s0"),
                (":q0", ":s1"),
                (":q1", ":s0"),
                (":q1", ":s1"),
                (":q1", ":s1"),
            ]
        );
        let mut sorted_indices: Vec<usize> = direct_entries.iter().map(|&(_, _, i)| i).collect();
        sorted_indices.sort_unstable();
        assert_eq!(sorted_indices, (0..pairs.len()).collect::<Vec<_>>());
        // Inside `check_chain_ranges`, sorting must coalesce into exactly 2 `last` groups
        // and 4 `(last, sup)` subgroups (an unsorted pass would emit 6 groups / 6 subgroups).
        let group_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let subgroup_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gh = group_hits.clone();
        let sgh = subgroup_hits.clone();
        let multi_out = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::OriginalRangeGroup => {
                    gh.fetch_add(1, Ordering::Relaxed);
                }
                admission_test::Point::OriginalRangeSubGroup => {
                    sgh.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            },
            || {
                check_chain_ranges(
                    &multi_kinds,
                    |_, _| false,
                    &multi_chains,
                    true,
                    &mut scratch_m,
                )
                .unwrap()
            },
        );
        assert_eq!(group_hits.load(Ordering::Relaxed), 2);
        assert_eq!(subgroup_hits.load(Ordering::Relaxed), 4);
        for ((_, _, expect_viol), got) in pairs.iter().zip(multi_out.iter()) {
            assert_eq!(got.is_some(), *expect_viol);
        }
    }

    #[test]
    fn post_run_scratch_excludes_dropped_rewrite_storage_and_requests_no_extra_pool_grant() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Construct a kept rewrite whose retained result and original range scan fit
        // within the memory already admitted for planning and classification.
        let mut body = String::from(
            "Prefix(:=<http://ex.org/>)
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
             Ontology(
                 DisjointUnion(:C
                     ObjectSomeValuesFrom(:r owl:Thing)
                     ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
                 SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
                 SubObjectPropertyOf(:s owl:topObjectProperty)
                 ObjectPropertyRange(owl:topObjectProperty :T0)\n",
        );
        for i in 0..1500 {
            body.push_str(&format!(
                "SubClassOf(:A{i} ObjectIntersectionOf(:B{i} ObjectMinCardinality(0 owl:topObjectProperty :X)))\n"
            ));
        }
        for i in 0..1200 {
            body.push_str(&format!(
                "SubObjectPropertyOf(:extra{i} owl:topObjectProperty)\n"
            ));
        }
        body.push(')');
        let doc = crate::ofn_read::parse(&body).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let rooted = Limits {
            threads: 1,
            rooted: true,
            strict: true,
            ..Limits::default()
        };

        let granted = Arc::new(AtomicUsize::new(0));
        let granted_charge = granted.clone();
        let granted_at_scan = Arc::new(AtomicUsize::new(0));
        let granted_at_scan_hook = granted_at_scan.clone();
        let granted_for_hook = granted.clone();
        let baselines = Arc::new(parking_lot::Mutex::new(None));
        let baselines_for_hook = baselines.clone();
        let mut sat_full = Saturation::new();

        let full = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::PostRunStart { base, with_rewrite } = pt {
                    *baselines_for_hook.lock() = Some((base, with_rewrite));
                    granted_at_scan_hook
                        .store(granted_for_hook.load(Ordering::Relaxed), Ordering::Relaxed);
                }
            },
            || {
                sat_full.classify(
                    &kinds,
                    None,
                    &Limits {
                        charge: Some(Arc::new(move |bytes| {
                            granted_charge.fetch_add(bytes, Ordering::Relaxed);
                            true
                        })),
                        ..rooted.clone()
                    },
                )
            },
        )
        .unwrap();
        assert!(sat_full.reduced, "expected kept rewritten path");
        let before_scan = granted_at_scan.load(Ordering::Relaxed);
        let total_granted = granted.load(Ordering::Relaxed);
        assert!(before_scan > 0, "expected PostRunStart checkpoint to fire");
        assert_eq!(
            total_granted, before_scan,
            "post-run scan requested extra pool grant ({total_granted} vs {before_scan}) for dropped rewrite storage"
        );
        assert_eq!(full.stats.peak_memory, before_scan);
        assert!(
            full.stats.memory < full.stats.peak_memory,
            "retained memory ({}) should exclude dropped rewrite storage below rewrite peak ({})",
            full.stats.memory,
            full.stats.peak_memory
        );

        let red = dl_reduce::plan(&kinds, None, None).expect("expected rewrite plan");
        let red_bytes = red.retained_bytes() - red.hub.as_ref().map_or(0, |hub| hub.as_str().len())
            + vec_bytes(&red.all(&kinds));
        let (base, with_rewrite) = baselines.lock().expect("post-run scratch baseline");
        assert_eq!(with_rewrite - base, red_bytes);
        assert_eq!(base, full.stats.memory);
        let mut scan_peak = 0usize;
        let mut scan_paid = 0usize;
        let unlimited = Limits::default();
        let mut scan_only = PostRunScratch {
            base: 0,
            peak: &mut scan_peak,
            paid: &mut scan_paid,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        dl_reduce::Reduction::outside_el(&kinds, &[], &mut scan_only).unwrap();
        assert!(
            full.stats.memory + scan_peak <= before_scan,
            "expected live + scan ({}) <= cap ({before_scan})",
            full.stats.memory + scan_peak
        );
        // Reuse only the credit already granted before scanning the original axioms.
        let capped = Saturation::new()
            .classify(
                &kinds,
                None,
                &Limits {
                    max_memory: Some(before_scan),
                    ..rooted
                },
            )
            .unwrap();
        assert_eq!(capped.stats.outside_el, full.stats.outside_el);
        assert_eq!(capped.stats.peak_memory, before_scan);
    }

    #[test]
    fn check_chain_ranges_releases_needed_set_before_range_walks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Chain reaches 2,000 properties so `needed` grows large before `drop(needed)`.
        let mut body = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :r0)\n",
        );
        for i in 0..1999 {
            body.push_str(&format!("SubObjectPropertyOf(:r{i} :r{})\n", i + 1));
        }
        body.push_str("ObjectPropertyRange(:r1999 :T0)\n)");
        let doc = crate::ofn_read::parse(&body).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let chains = [kinds[0]];

        struct RecordingScratch {
            live: Arc<AtomicUsize>,
            max_live: Arc<AtomicUsize>,
        }
        impl ScratchBudget for RecordingScratch {
            fn poll(&self) -> Result<(), Error> {
                Ok(())
            }
            fn grow(&mut self, more: usize) -> Result<(), Error> {
                let next = self.live.fetch_add(more, Ordering::Relaxed) + more;
                self.max_live.fetch_max(next, Ordering::Relaxed);
                Ok(())
            }
            fn release(&mut self, less: usize) {
                let prev = self.live.load(Ordering::Relaxed);
                self.live
                    .store(prev.saturating_sub(less), Ordering::Relaxed);
            }
            fn overflow(&self) -> Error {
                Error::Memory {
                    limit: usize::MAX,
                    used: usize::MAX,
                }
            }
        }

        let live = Arc::new(AtomicUsize::new(0));
        let max_live = Arc::new(AtomicUsize::new(0));
        let live_before_drop = Arc::new(AtomicUsize::new(0));
        let live_after_drop = Arc::new(AtomicUsize::new(0));
        let live_h = live.clone();
        let before_h = live_before_drop.clone();
        let after_h = live_after_drop.clone();
        let mut budget = RecordingScratch { live, max_live };

        let out = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::OriginalRangeBeforeDropNeeded => {
                    before_h.store(live_h.load(Ordering::Relaxed), Ordering::Relaxed);
                }
                admission_test::Point::OriginalRangeAfterDropNeeded => {
                    after_h.store(live_h.load(Ordering::Relaxed), Ordering::Relaxed);
                }
                _ => {}
            },
            || check_chain_ranges(&kinds, |_, _| false, &chains, false, &mut budget),
        )
        .unwrap();
        assert!(out[0].is_some());
        let before = live_before_drop.load(Ordering::Relaxed);
        let after = live_after_drop.load(Ordering::Relaxed);
        // 2,001 reachable properties (`:q` + `:r0..=:r1999`) grow `needed` to 3,584 usable slots.
        let expected_needed_bytes =
            hash_table_bytes(3584, std::mem::size_of::<&str>() + 1).unwrap();
        assert_eq!(
            before.saturating_sub(after),
            expected_needed_bytes,
            "drop(needed) must release exact needed-set bytes ({expected_needed_bytes}): before={before}, after={after}"
        );
    }

    #[test]
    fn build_inner_admits_diagnostics_before_allocation_and_charges_chains_buffer() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        // 1. Long range IRI (256 KiB): a cap covering normalized state + chains + scan
        //    scratch must refuse diagnostic creation BEFORE allocating the 256 KiB message.
        let long_iri = format!("http://ex.org/{}", "a".repeat(256 * 1024));
        let doc_no = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:s <{long_iri}>)\n)"
        ))
        .unwrap();
        let doc_with = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
             ObjectPropertyRange(:s <{long_iri}>)\n)"
        ))
        .unwrap();
        let kinds_no: Vec<&AxiomKind> = doc_no.axioms.iter().map(|a| &a.kind).collect();
        let kinds_with: Vec<&AxiomKind> = doc_with.axioms.iter().map(|a| &a.kind).collect();
        let serial = Limits {
            threads: 1,
            ..Limits::default()
        };

        let (_, sample_no) = a1_probe_alloc::measure(|| {
            Saturation::new()
                .build(
                    &kinds_no,
                    &[],
                    1,
                    0,
                    0,
                    &serial,
                    AttemptProvenance::OriginalDefault,
                )
                .unwrap();
        });
        let scan_base = Arc::new(AtomicUsize::new(0));
        let scan_base_h = scan_base.clone();
        let (_, _, peak_with, _, _) = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::BuildRangeScanStart { base, .. } = pt {
                    scan_base_h.store(base, Ordering::Relaxed);
                }
            },
            || {
                Saturation::new()
                    .build(
                        &kinds_with,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        let base_at_scan = scan_base.load(Ordering::Relaxed);
        assert!(base_at_scan > 0);
        // Cap covers normalized state + scan scratch + 32 KiB headroom, well below the
        // 256 KiB diagnostic message.
        let diag_cap = base_at_scan + 32 * 1024;
        assert!(diag_cap < peak_with);
        let reached_consume = Arc::new(AtomicBool::new(false));
        let reached_consume_h = reached_consume.clone();
        let (err, sample_refused) = a1_probe_alloc::measure(|| {
            admission_test::with_hook(
                move |pt| {
                    if matches!(pt, admission_test::Point::BuildMissingConsumeStart { .. }) {
                        reached_consume_h.store(true, Ordering::Relaxed);
                    }
                },
                || {
                    Saturation::new()
                        .build(
                            &kinds_with,
                            &[],
                            1,
                            0,
                            0,
                            &Limits {
                                max_memory: Some(diag_cap),
                                ..serial.clone()
                            },
                            AttemptProvenance::OriginalDefault,
                        )
                        .err()
                },
            )
        });
        assert!(
            matches!(err, Some(Error::Memory { .. })),
            "expected Memory refusal at diagnostic pre-admission cap={diag_cap}, got {err:?}"
        );
        assert!(
            reached_consume.load(Ordering::Relaxed),
            "expected refusal inside BuildMissingConsumeStart"
        );
        // Must not have allocated the 256 KiB formatted diagnostic string before refusing.
        assert!(
            sample_refused.peak < sample_no.peak + 128 * 1024,
            "diagnostic string was allocated before admission refused: refused_peak={} baseline_peak={}",
            sample_refused.peak,
            sample_no.peak
        );

        // 1b. Wide normalized DisjointClasses (~131 KiB inner Vec omitted by `ax.rough_bytes()`)
        //     paired with a 96 KiB missing-range IRI: pre-allocation admission must keep full
        //     `ax.bytes()` so it refuses BEFORE allocating the 96 KiB diagnostic string.
        let wide_diag_iri = format!("http://ex.org/{}", "b".repeat(96 * 1024));
        let mut wide_disj = String::from("DisjointClasses(");
        for i in 0..32_768 {
            wide_disj.push_str(&format!(" :D{i}"));
        }
        wide_disj.push(')');
        let doc_wide_no = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\n{wide_disj}\nObjectPropertyRange(:s <{wide_diag_iri}>)\n)"
        ))
        .unwrap();
        let doc_wide_with = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\n{wide_disj}\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
             ObjectPropertyRange(:s <{wide_diag_iri}>)\n)"
        ))
        .unwrap();
        let kinds_wide_no: Vec<&AxiomKind> = doc_wide_no.axioms.iter().map(|a| &a.kind).collect();
        let kinds_wide_with: Vec<&AxiomKind> =
            doc_wide_with.axioms.iter().map(|a| &a.kind).collect();
        let (_, sample_wide_no) = a1_probe_alloc::measure(|| {
            Saturation::new()
                .build(
                    &kinds_wide_no,
                    &[],
                    1,
                    0,
                    0,
                    &serial,
                    AttemptProvenance::OriginalDefault,
                )
                .unwrap();
        });
        let wide_scan_base = Arc::new(AtomicUsize::new(0));
        let wide_scan_base_h = wide_scan_base.clone();
        let (_, _, wide_peak_with, _, _) = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::BuildRangeScanStart { base, .. } = pt {
                    wide_scan_base_h.store(base, Ordering::Relaxed);
                }
            },
            || {
                Saturation::new()
                    .build(
                        &kinds_wide_with,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        // 16 KiB headroom above full scan base: enough for scan scratch, well below 96 KiB
        // diagnostic, whereas `ax.rough_bytes()` would drop ~131 KiB and wrongly admit it.
        let wide_diag_cap = wide_scan_base.load(Ordering::Relaxed) + 16 * 1024;
        assert!(wide_diag_cap < wide_peak_with);
        let wide_reached_consume = Arc::new(AtomicBool::new(false));
        let wide_reached_consume_h = wide_reached_consume.clone();
        let wide_allocated_diag = Arc::new(AtomicBool::new(false));
        let wide_allocated_diag_h = wide_allocated_diag.clone();
        let (wide_err, sample_wide_refused) = a1_probe_alloc::measure(|| {
            admission_test::with_hook(
                move |pt| match pt {
                    admission_test::Point::BuildMissingConsumeStart { .. } => {
                        wide_reached_consume_h.store(true, Ordering::Relaxed);
                    }
                    admission_test::Point::BuildDiagnosticAllocated => {
                        wide_allocated_diag_h.store(true, Ordering::Relaxed);
                    }
                    _ => {}
                },
                || {
                    Saturation::new()
                        .build(
                            &kinds_wide_with,
                            &[],
                            1,
                            0,
                            0,
                            &Limits {
                                max_memory: Some(wide_diag_cap),
                                ..serial.clone()
                            },
                            AttemptProvenance::OriginalDefault,
                        )
                        .err()
                },
            )
        });
        assert!(
            matches!(wide_err, Some(Error::Memory { .. })),
            "expected Memory refusal with wide DisjointClasses + 96 KiB diagnostic, got {wide_err:?}"
        );
        assert!(wide_reached_consume.load(Ordering::Relaxed));
        assert!(
            !wide_allocated_diag.load(Ordering::Relaxed),
            "diagnostic string was allocated past cap because normalized DisjointClasses bytes were dropped"
        );
        assert!(
            sample_wide_refused.peak < sample_wide_no.peak + 48 * 1024,
            "wide-disjoint refusal peak={} exceeded no-chain peak={}",
            sample_wide_refused.peak,
            sample_wide_no.peak
        );

        // 1c. Accepted two-role chains before a long missing-range diagnostic: 1,025 accepted
        //     chains grow `ax.chains` to 2,048 outer slots, leaving 1,023 spare slots
        //     (32,736 bytes on 64-bit). Pre-allocation admission must charge `ax.chains` by
        //     capacity so a cap above the occupied-only estimate still refuses BEFORE
        //     allocating the 96 KiB diagnostic string.
        const ACCEPTED_CHAINS: usize = 1025;
        let chains_diag_iri = format!("http://ex.org/{}", "d".repeat(96 * 1024));
        let mut accepted_body = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(:a :b)\n\
             SubObjectPropertyOf(:b :c)\n",
        );
        accepted_body.push_str(&format!("ObjectPropertyRange(:s <{chains_diag_iri}>)\n"));
        for _ in 0..ACCEPTED_CHAINS {
            accepted_body.push_str("SubObjectPropertyOf(ObjectPropertyChain(:a :b) :c)\n");
        }
        let doc_accepted_no = crate::ofn_read::parse(&format!("{accepted_body})")).unwrap();
        let doc_accepted_with = crate::ofn_read::parse(&format!(
            "{accepted_body}SubObjectPropertyOf(ObjectPropertyChain(:a :b) :s)\n)"
        ))
        .unwrap();
        let kinds_accepted_no: Vec<&AxiomKind> =
            doc_accepted_no.axioms.iter().map(|a| &a.kind).collect();
        let kinds_accepted_with: Vec<&AxiomKind> =
            doc_accepted_with.axioms.iter().map(|a| &a.kind).collect();
        let ((ax_accepted_no, _, _, _, _), sample_accepted_no) = a1_probe_alloc::measure(|| {
            Saturation::new()
                .build(
                    &kinds_accepted_no,
                    &[],
                    1,
                    0,
                    0,
                    &serial,
                    AttemptProvenance::OriginalDefault,
                )
                .unwrap()
        });
        assert_eq!(ax_accepted_no.chains.len(), ACCEPTED_CHAINS);
        let chain_cap = ax_accepted_no.chains.capacity();
        assert!(
            chain_cap > ACCEPTED_CHAINS,
            "expected spare outer capacity after {ACCEPTED_CHAINS} accepted chains, got cap={chain_cap}"
        );
        let acc_scan_base = Arc::new(AtomicUsize::new(0));
        let acc_scan_base_h = acc_scan_base.clone();
        let acc_consume_peak = Arc::new(AtomicUsize::new(0));
        let acc_consume_peak_h = acc_consume_peak.clone();
        let acc_consume_scratch = Arc::new(AtomicUsize::new(0));
        let acc_consume_scratch_h = acc_consume_scratch.clone();
        let (_, _, acc_peak_with, _, _) = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::BuildRangeScanStart { base, .. } => {
                    acc_scan_base_h.store(base, Ordering::Relaxed);
                }
                admission_test::Point::BuildMissingConsumeStart { peak, scratch } => {
                    acc_consume_peak_h.store(peak, Ordering::Relaxed);
                    acc_consume_scratch_h.store(scratch, Ordering::Relaxed);
                }
                _ => {}
            },
            || {
                Saturation::new()
                    .build(
                        &kinds_accepted_with,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        let slot_bytes = std::mem::size_of::<(Vec<index::Role>, index::Role)>();
        let inner_role_bytes = ACCEPTED_CHAINS * 2 * std::mem::size_of::<index::Role>();
        let occupied_only_chain_bytes = ACCEPTED_CHAINS * slot_bytes + inner_role_bytes;
        let full_chain_bytes = chain_cap * slot_bytes + inner_role_bytes;
        let spare_chain_bytes = full_chain_bytes - occupied_only_chain_bytes;
        // Place the cap midway through the spare-slot delta above the occupied-only
        // estimate + diagnostic size: an occupied-only `ax_bytes` would admit and allocate
        // the diagnostic, while capacity-based accounting refuses before allocation.
        let acc_base = acc_scan_base.load(Ordering::Relaxed);
        let acc_scratch = acc_consume_scratch.load(Ordering::Relaxed);
        let acc_diag_cap = acc_base
            + acc_scratch
            + occupied_only_chain_bytes
            + chains_diag_iri.len()
            + spare_chain_bytes / 2;
        assert!(
            acc_diag_cap > acc_consume_peak.load(Ordering::Relaxed),
            "cap ({acc_diag_cap}) must exceed scan peak ({}) so refusal happens in diagnostic admission",
            acc_consume_peak.load(Ordering::Relaxed)
        );
        assert!(
            acc_diag_cap > acc_base + acc_scratch + full_chain_bytes,
            "cap ({acc_diag_cap}) must admit all {ACCEPTED_CHAINS} accepted chains before the diagnostic"
        );
        assert!(acc_diag_cap < acc_peak_with);
        let acc_reached_consume = Arc::new(AtomicBool::new(false));
        let acc_reached_consume_h = acc_reached_consume.clone();
        let acc_allocated_diag = Arc::new(AtomicBool::new(false));
        let acc_allocated_diag_h = acc_allocated_diag.clone();
        let (acc_err, sample_accepted_refused) = a1_probe_alloc::measure(|| {
            admission_test::with_hook(
                move |pt| match pt {
                    admission_test::Point::BuildMissingConsumeStart { .. } => {
                        acc_reached_consume_h.store(true, Ordering::Relaxed);
                    }
                    admission_test::Point::BuildDiagnosticAllocated => {
                        acc_allocated_diag_h.store(true, Ordering::Relaxed);
                    }
                    _ => {}
                },
                || {
                    Saturation::new()
                        .build(
                            &kinds_accepted_with,
                            &[],
                            1,
                            0,
                            0,
                            &Limits {
                                max_memory: Some(acc_diag_cap),
                                ..serial.clone()
                            },
                            AttemptProvenance::OriginalDefault,
                        )
                        .err()
                },
            )
        });
        assert!(
            matches!(acc_err, Some(Error::Memory { .. })),
            "expected Memory refusal with {ACCEPTED_CHAINS} accepted chains + 96 KiB diagnostic, got {acc_err:?}"
        );
        assert!(acc_reached_consume.load(Ordering::Relaxed));
        assert!(
            !acc_allocated_diag.load(Ordering::Relaxed),
            "diagnostic string was allocated past cap because spare ax.chains slots were dropped"
        );
        assert!(
            sample_accepted_refused.peak < sample_accepted_no.peak + 48 * 1024,
            "accepted-chains refusal peak={} exceeded no-diagnostic peak={}",
            sample_accepted_refused.peak,
            sample_accepted_no.peak
        );

        // 2. Live `chains` vector capacity must be included in the scan baseline AND released
        //    from `budget` immediately after `drop(chains)` before dl_norm/horn run.
        let mut body_chains =
            String::from("Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:s :T)\n");
        for i in 0..4000 {
            body_chains.push_str(&format!(
                "SubObjectPropertyOf(ObjectPropertyChain(:p{i} :q) :s)\n"
            ));
        }
        body_chains.push(')');
        let doc_chains = crate::ofn_read::parse(&body_chains).unwrap();
        let kinds_chains: Vec<&AxiomKind> = doc_chains.axioms.iter().map(|a| &a.kind).collect();
        let doc_one_chain = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:s :T)\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p0 :q) :s)\n)",
        )
        .unwrap();
        let kinds_one_chain: Vec<&AxiomKind> =
            doc_one_chain.axioms.iter().map(|a| &a.kind).collect();
        let one_chain_base = Arc::new(AtomicUsize::new(0));
        let one_chain_base_h = one_chain_base.clone();
        admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::BuildRangeScanStart { base, .. } = pt {
                    one_chain_base_h.store(base, Ordering::Relaxed);
                }
            },
            || {
                Saturation::new()
                    .build(
                        &kinds_one_chain,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        let chains_scan_base = Arc::new(AtomicUsize::new(0));
        let chains_scan_base_h = chains_scan_base.clone();
        let before_chains_drop_base = Arc::new(AtomicUsize::new(0));
        let before_chains_drop_base_h = before_chains_drop_base.clone();
        let after_chains_drop_base = Arc::new(AtomicUsize::new(0));
        let after_chains_drop_base_h = after_chains_drop_base.clone();
        let after_chains_drop_scratch = Arc::new(AtomicUsize::new(usize::MAX));
        let after_chains_drop_scratch_h = after_chains_drop_scratch.clone();
        let mut sat_chains = Saturation::new();
        admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::BuildRangeScanStart { base, .. } => {
                    chains_scan_base_h.store(base, Ordering::Relaxed);
                }
                admission_test::Point::BuildBeforeChainsDrop { base } => {
                    before_chains_drop_base_h.store(base, Ordering::Relaxed);
                }
                admission_test::Point::BuildAfterChainsDrop { base, scratch } => {
                    after_chains_drop_base_h.store(base, Ordering::Relaxed);
                    after_chains_drop_scratch_h.store(scratch, Ordering::Relaxed);
                }
                _ => {}
            },
            || {
                sat_chains
                    .build(
                        &kinds_chains,
                        &[],
                        1,
                        0,
                        0,
                        &serial,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        // At scan entry only the 1 range axiom has been normalized; the 1-chain `Vec`
        // starts at capacity 4 while the 4,000-chain `Vec` holds at least 4,000 slots.
        let delta_chains_bytes = (4000 - 4) * std::mem::size_of::<&AxiomKind>();
        let recorded = chains_scan_base.load(Ordering::Relaxed);
        let one_recorded = one_chain_base.load(Ordering::Relaxed);
        assert!(
            recorded >= one_recorded + delta_chains_bytes,
            "scan baseline ({recorded}) omitted live chains buffer relative to 1-chain baseline ({one_recorded}, expected +{delta_chains_bytes} bytes)"
        );
        assert_eq!(
            after_chains_drop_scratch.load(Ordering::Relaxed),
            0,
            "scratch must be zero after dropping missing/chains/scratch"
        );
        let before_drop = before_chains_drop_base.load(Ordering::Relaxed);
        let after_drop = after_chains_drop_base.load(Ordering::Relaxed);
        let min_chains_bytes = 4000 * std::mem::size_of::<&AxiomKind>();
        assert!(
            before_drop.saturating_sub(after_drop) >= min_chains_bytes,
            "chains_bytes was not released from budget base after drop(chains): before={before_drop}, after={after_drop}, expected drop >= {min_chains_bytes}"
        );
    }

    #[test]
    fn diagnostic_rendering_polls_both_passes_prefix_validation_and_alloc_boundary() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        // 1. Long IRI in the standard `owl:` namespace exercises `is_local_name_polled`
        //    during prefix validation in pass 1 (both valid and trailing-invalid local names).
        let long_owl_valid = format!("{}A{}", crate::vocab::OWL, "a".repeat(128 * 1024));
        let long_owl_invalid = format!("{}A{}!", crate::vocab::OWL, "a".repeat(128 * 1024));
        for (iri, label) in [
            (&long_owl_valid, "valid owl local name"),
            (&long_owl_invalid, "invalid owl local name"),
        ] {
            let ce = ClassExpression::Class(Class::new(iri.clone()));
            let run_count = |cancel_at: Option<usize>| {
                let polls = Arc::new(AtomicUsize::new(0));
                let polls_c = polls.clone();
                let limits = Limits {
                    cancelled: Some(Arc::new(move || {
                        let n = polls_c.fetch_add(1, Ordering::Relaxed) + 1;
                        cancel_at.is_some_and(|at| n >= at)
                    })),
                    ..Limits::default()
                };
                let mut counter = CountingWriter {
                    len: 0,
                    steps: 0,
                    limits: &limits,
                };
                let res = render_missing_range_polled(
                    &ce,
                    "http://ex.org/s",
                    "http://ex.org/q",
                    &mut counter,
                );
                (res, counter.len, polls.load(Ordering::Relaxed))
            };
            let (full_res, full_len, full_polls) = run_count(None);
            full_res.unwrap();
            assert!(full_len > 128 * 1024, "{label}: unexpected len {full_len}");
            assert!(
                full_polls >= 240,
                "{label}: expected chunked polling during prefix validation and write, saw {full_polls}"
            );
            // Cancel early inside `is_local_name_polled` (well before half the IRI).
            let (cancel_res, cancel_len, cancel_polls) = run_count(Some(3));
            assert!(
                matches!(cancel_res, Err(Error::Cancelled)),
                "{label}: expected Cancelled, got {cancel_res:?}"
            );
            assert_eq!(cancel_polls, 3, "{label}: did not stop on poll 3");
            assert_eq!(
                cancel_len,
                "the range ".len(),
                "{label}: cancellation must fire inside is_local_name_polled before writing either the prefixed name or bracketed fallback"
            );
        }

        // 2. Cancellation raised right when the pool grants the diagnostic allocation
        //    must abort at the allocation boundary BEFORE `String::with_capacity` and pass 2.
        let long_iri = format!("http://ex.org/{}", "c".repeat(256 * 1024));
        let doc = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
             ObjectPropertyRange(:s <{long_iri}>)\n)"
        ))
        .unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();

        let in_consume = Arc::new(AtomicBool::new(false));
        let in_consume_h = in_consume.clone();
        let in_consume_charge = in_consume.clone();
        let cancelled_on_grant = Arc::new(AtomicBool::new(false));
        let cancelled_on_grant_charge = cancelled_on_grant.clone();
        let cancelled_on_grant_check = cancelled_on_grant.clone();
        let allocated_after_grant = Arc::new(AtomicBool::new(false));
        let allocated_after_grant_h = allocated_after_grant.clone();
        let limits_grant_cancel = Limits {
            threads: 1,
            charge: Some(Arc::new(move |bytes| {
                if in_consume_charge.load(Ordering::Relaxed) && bytes >= 64 * 1024 {
                    cancelled_on_grant_charge.store(true, Ordering::Relaxed);
                }
                true
            })),
            cancelled: Some(Arc::new(move || {
                cancelled_on_grant_check.load(Ordering::Relaxed)
            })),
            ..Limits::default()
        };
        let res_grant = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::BuildMissingConsumeStart { .. } => {
                    in_consume_h.store(true, Ordering::Relaxed);
                }
                admission_test::Point::BuildDiagnosticAllocated => {
                    allocated_after_grant_h.store(true, Ordering::Relaxed);
                }
                _ => {}
            },
            || {
                Saturation::new()
                    .build(
                        &kinds,
                        &[],
                        1,
                        0,
                        0,
                        &limits_grant_cancel,
                        AttemptProvenance::OriginalDefault,
                    )
                    .err()
            },
        );
        assert!(
            cancelled_on_grant.load(Ordering::Relaxed),
            "expected diagnostic allocation grant callback to fire"
        );
        assert!(
            matches!(res_grant, Some(Error::Cancelled)),
            "expected Cancelled at diagnostic allocation boundary, got {res_grant:?}"
        );
        assert!(
            !allocated_after_grant.load(Ordering::Relaxed),
            "cancellation raised during pool grant must stop before String allocation and pass 2"
        );

        // 3. Cancellation raised strictly AFTER `BuildDiagnosticAllocated` interrupts
        //    pass 2 mid-write rather than traversing the entire diagnostic unpolled.
        let full_polls = Arc::new(AtomicUsize::new(0));
        let full_polls_c = full_polls.clone();
        let full_polls_h = full_polls.clone();
        let polls_at_alloc = Arc::new(AtomicUsize::new(0));
        let polls_at_alloc_h = polls_at_alloc.clone();
        let limits_full = Limits {
            threads: 1,
            cancelled: Some(Arc::new(move || {
                full_polls_c.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        };
        admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::BuildDiagnosticAllocated {
                    polls_at_alloc_h.store(full_polls_h.load(Ordering::Relaxed), Ordering::Relaxed);
                }
            },
            || {
                Saturation::new()
                    .build(
                        &kinds,
                        &[],
                        1,
                        0,
                        0,
                        &limits_full,
                        AttemptProvenance::OriginalDefault,
                    )
                    .unwrap()
            },
        );
        let at_alloc = polls_at_alloc.load(Ordering::Relaxed);
        let total_full = full_polls.load(Ordering::Relaxed);
        assert!(
            total_full >= at_alloc + 100,
            "expected pass 2 to poll across the 256 KiB write: at_alloc={at_alloc}, full={total_full}"
        );

        let pass2_armed = Arc::new(AtomicBool::new(false));
        let pass2_armed_h = pass2_armed.clone();
        let pass2_armed_c = pass2_armed.clone();
        let pass2_polls = Arc::new(AtomicUsize::new(0));
        let pass2_polls_c = pass2_polls.clone();
        let total_cancel_polls = Arc::new(AtomicUsize::new(0));
        let total_cancel_polls_c = total_cancel_polls.clone();
        let limits_pass2 = Limits {
            threads: 1,
            cancelled: Some(Arc::new(move || {
                total_cancel_polls_c.fetch_add(1, Ordering::Relaxed);
                pass2_armed_c.load(Ordering::Relaxed)
                    && pass2_polls_c.fetch_add(1, Ordering::Relaxed) >= 3
            })),
            ..Limits::default()
        };
        let res_pass2 = admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::BuildDiagnosticAllocated {
                    pass2_armed_h.store(true, Ordering::Relaxed);
                }
            },
            || {
                Saturation::new()
                    .build(
                        &kinds,
                        &[],
                        1,
                        0,
                        0,
                        &limits_pass2,
                        AttemptProvenance::OriginalDefault,
                    )
                    .err()
            },
        );
        assert!(pass2_armed.load(Ordering::Relaxed));
        assert!(
            matches!(res_pass2, Some(Error::Cancelled)),
            "expected Cancelled inside pass 2 write, got {res_pass2:?}"
        );
        let cancel_total = total_cancel_polls.load(Ordering::Relaxed);
        assert!(
            cancel_total > at_alloc && cancel_total < total_full,
            "expected pass-2 mid-write cancellation: at_alloc={at_alloc}, cancel={cancel_total}, full={total_full}"
        );
    }

    #[test]
    fn outside_el_distinguishes_directional_literals_in_data_has_value() {
        let ltr = Literal::from(
            oxrdf::Literal::new_directional_language_tagged_literal(
                "hello",
                "en",
                oxrdf::BaseDirection::Ltr,
            )
            .unwrap(),
        );
        let rtl = Literal::from(
            oxrdf::Literal::new_directional_language_tagged_literal(
                "hello",
                "en",
                oxrdf::BaseDirection::Rtl,
            )
            .unwrap(),
        );
        assert_ne!(ltr, rtl);
        let make_ax = |lit: Literal| {
            AxiomKind::SubClassOf(
                ClassExpression::Class(Class::new("http://ex.org/Z")),
                ClassExpression::ObjectMinCardinality(
                    0,
                    ObjectPropertyExpression::Property(ObjectProperty::new(
                        crate::vocab::OWL_TOP_OBJECT_PROPERTY,
                    )),
                    Some(Box::new(ClassExpression::DataHasValue(
                        DataProperty::new("http://ex.org/dp"),
                        lit,
                    ))),
                ),
            )
        };
        let ax_ltr = make_ax(ltr.clone());
        let ax_rtl = make_ax(rtl);
        let ax_ltr_dup = make_ax(ltr);

        let unlimited = Limits::default();
        let mut peak = 0usize;
        let mut paid = 0usize;
        let mut scratch = PostRunScratch {
            base: 0,
            peak: &mut peak,
            paid: &mut paid,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let out = dl_reduce::Reduction::outside_el(&[&ax_ltr, &ax_rtl], &[], &mut scratch).unwrap();
        assert_eq!(
            out.horn, 2,
            "LTR and RTL directional literals must stay distinct"
        );
        assert_eq!(out.taken, 2);

        let mut peak2 = 0usize;
        let mut paid2 = 0usize;
        let mut scratch2 = PostRunScratch {
            base: 0,
            peak: &mut peak2,
            paid: &mut paid2,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let out_dup =
            dl_reduce::Reduction::outside_el(&[&ax_ltr, &ax_rtl, &ax_ltr_dup], &[], &mut scratch2)
                .unwrap();
        assert_eq!(
            out_dup.horn, 2,
            "identical directional literal must still deduplicate"
        );
        assert_eq!(out_dup.taken, 2);
    }

    #[test]
    fn polled_structural_hashing_and_range_lookup_stop_cooperatively() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // 1. Wide duplicate axiom after `seen` is already populated: `el_breaks_polled`
        //    short-circuits on member 0 and `dl::shape_polled` does not run on the duplicate,
        //    so extra polls come from polled structural hashing AND equality in `seen`.
        const N_OPS: usize = 2500;
        let build_wide = |last_prefix: &str| {
            let mut s = String::from(
                "Prefix(:=<http://ex.org/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n\
                 DisjointUnion(:C :A :B)\nSubClassOf(:Z ObjectIntersectionOf(",
            );
            for i in 0..N_OPS {
                let p = if i + 1 == N_OPS { last_prefix } else { ":p" };
                s.push_str(&format!(
                    " ObjectMinCardinality(0 owl:topObjectProperty ObjectSomeValuesFrom({p}{i} owl:Thing))"
                ));
            }
            s.push_str("))\n)");
            crate::ofn_read::parse(&s).unwrap()
        };
        let doc1 = build_wide(":p");
        let doc2 = build_wide(":p");
        let doc_diff = build_wide(":q");
        let single_kinds = [&doc1.axioms[0].kind, &doc1.axioms[1].kind];
        let dup_kinds = [
            &doc1.axioms[0].kind,
            &doc1.axioms[1].kind,
            &doc2.axioms[1].kind,
        ];

        // Measure pure hashing polls (no equality) vs hashing + equality on `PolledSet`.
        let mut probe_set: polled::PolledSet<'_, AxiomKind> = polled::PolledSet::new();
        {
            let unlimited = Limits::default();
            let mut peak = 0usize;
            let mut paid = 0usize;
            let mut scratch = PostRunScratch {
                base: 0,
                peak: &mut peak,
                paid: &mut paid,
                limits: &unlimited,
                charge: None,
                check_stop: true,
                scratch: 0,
            };
            let mut steps = 0usize;
            assert!(
                probe_set
                    .insert_polled(&doc1.axioms[1].kind, &mut steps, &mut scratch)
                    .unwrap()
            );
        }
        let probe_contains = |item: &AxiomKind, cancel_at: Option<usize>| {
            let polls = Arc::new(AtomicUsize::new(0));
            let polls_c = polls.clone();
            let limits = Limits {
                cancelled: Some(Arc::new(move || {
                    let n = polls_c.fetch_add(1, Ordering::Relaxed) + 1;
                    cancel_at.is_some_and(|at| n >= at)
                })),
                ..Limits::default()
            };
            let mut peak = 0usize;
            let mut paid = 0usize;
            let scratch = PostRunScratch {
                base: 0,
                peak: &mut peak,
                paid: &mut paid,
                limits: &limits,
                charge: None,
                check_stop: true,
                scratch: 0,
            };
            let mut steps = 0usize;
            let res = probe_set.contains_polled(item, &mut steps, &scratch);
            (res, polls.load(Ordering::Relaxed))
        };
        let (diff_res, hash_only_polls) = probe_contains(&doc_diff.axioms[1].kind, None);
        assert!(!diff_res.unwrap());
        assert!(hash_only_polls >= 4);
        let (eq_res, hash_plus_eq_polls) = probe_contains(&doc2.axioms[1].kind, None);
        assert!(eq_res.unwrap());
        assert!(
            hash_plus_eq_polls >= hash_only_polls + 4,
            "expected structural equality itself to poll: hash_only={hash_only_polls}, hash_plus_eq={hash_plus_eq_polls}"
        );
        // Trigger cancellation strictly AFTER hashing finishes, inside `eq_polled`.
        let (eq_cancel_res, eq_cancel_polls) =
            probe_contains(&doc2.axioms[1].kind, Some(hash_only_polls + 2));
        assert!(matches!(eq_cancel_res, Err(Error::Cancelled)));
        assert!(
            eq_cancel_polls > hash_only_polls && eq_cancel_polls < hash_plus_eq_polls,
            "expected cancellation inside eq_polled: hash_only={hash_only_polls}, cancel={eq_cancel_polls}, full={hash_plus_eq_polls}"
        );

        let count_polls = |ks: &[&AxiomKind], cancel_at: Option<usize>| {
            let polls = Arc::new(AtomicUsize::new(0));
            let polls_c = polls.clone();
            let limits = Limits {
                cancelled: Some(Arc::new(move || {
                    let n = polls_c.fetch_add(1, Ordering::Relaxed) + 1;
                    cancel_at.is_some_and(|at| n >= at)
                })),
                ..Limits::default()
            };
            let mut peak = 0usize;
            let mut paid = 0usize;
            let mut scratch = PostRunScratch {
                base: 0,
                peak: &mut peak,
                paid: &mut paid,
                limits: &limits,
                charge: None,
                check_stop: true,
                scratch: 0,
            };
            let res = dl_reduce::Reduction::outside_el(ks, &[], &mut scratch);
            (res, polls.load(Ordering::Relaxed))
        };

        let (res1, p1) = count_polls(&single_kinds, None);
        let out1 = res1.unwrap();
        assert_eq!(out1.disjunctive, 1);
        assert_eq!(out1.horn, 1);

        let (res2, p2) = count_polls(&dup_kinds, None);
        let out2 = res2.unwrap();
        assert_eq!(out2, out1);
        assert!(
            p2 >= p1 + hash_only_polls + 4,
            "expected polled hashing + equality on wide duplicate in seen: single={p1}, hash_only={hash_only_polls}, dup={p2}"
        );

        // Cancel inside the duplicate's hashing phase...
        let (hash_cancel_res, hash_cancel_polls) = count_polls(&dup_kinds, Some(p1 + 1));
        assert!(matches!(hash_cancel_res, Err(Error::Cancelled)));
        assert!(hash_cancel_polls < p1 + hash_only_polls);
        // ...and separately inside the duplicate's equality phase (past hashing).
        let (dup_eq_cancel_res, dup_eq_cancel_polls) =
            count_polls(&dup_kinds, Some(p1 + hash_only_polls + 2));
        assert!(
            matches!(dup_eq_cancel_res, Err(Error::Cancelled)),
            "expected mid-equality cancellation in outside_el, got {dup_eq_cancel_res:?}"
        );
        assert!(
            dup_eq_cancel_polls > p1 + hash_only_polls && dup_eq_cancel_polls < p2,
            "expected cancellation inside duplicate equality: after_hash={}, cancel={dup_eq_cancel_polls}, full={p2}",
            p1 + hash_only_polls
        );

        // 2. Wide ClassExpression range lookup in `check_chain_ranges` must poll during both
        //    hashing and equality in `have_ranges`.
        let make_range_doc = |n: usize| {
            let mut s = String::from(
                "Prefix(:=<http://ex.org/>)\nOntology(\n\
                 SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
                 ObjectPropertyRange(:q ObjectIntersectionOf(",
            );
            for i in 0..n {
                s.push_str(&format!(" :C{i}"));
            }
            s.push_str("))\nObjectPropertyRange(:s ObjectIntersectionOf(");
            for i in 0..n {
                s.push_str(&format!(" :C{i}"));
            }
            s.push_str("))\n)");
            crate::ofn_read::parse(&s).unwrap()
        };
        let small_doc = make_range_doc(2);
        let wide_doc = make_range_doc(N_OPS);
        let run_range_polls =
            |doc: &crate::ofn_read::Document, need_first: bool, cancel_at: Option<usize>| {
                let ks: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
                let chains = [ks[0]];
                let polls = Arc::new(AtomicUsize::new(0));
                let polls_c = polls.clone();
                let limits = Limits {
                    cancelled: Some(Arc::new(move || {
                        let n = polls_c.fetch_add(1, Ordering::Relaxed) + 1;
                        cancel_at.is_some_and(|at| n >= at)
                    })),
                    ..Limits::default()
                };
                let mut peak = 0usize;
                let mut paid = 0usize;
                let mut scratch = PostRunScratch {
                    base: 0,
                    peak: &mut peak,
                    paid: &mut paid,
                    limits: &limits,
                    charge: None,
                    check_stop: true,
                    scratch: 0,
                };
                let res = check_chain_ranges(&ks, |_, _| false, &chains, need_first, &mut scratch)
                    .map(|v| v[0].map(|(c, _, _)| c.to_string()));
                (res, polls.load(Ordering::Relaxed))
            };
        let (r_small, ps) = run_range_polls(&small_doc, true, None);
        assert!(r_small.unwrap().is_none());
        let (r_wide, pw) = run_range_polls(&wide_doc, true, None);
        assert!(r_wide.unwrap().is_none());
        assert!(
            pw >= ps + 4,
            "expected polled range-expression hashing/equality in check_chain_ranges: small_polls={ps}, wide_polls={pw}"
        );
        let (r_cancel, pc) = run_range_polls(&wide_doc, true, Some(ps + 1));
        assert!(matches!(r_cancel, Err(Error::Cancelled)));
        assert!(pc < pw);

        // 3. Missing-range comparison (`cmp_ce_polled`) cancellation with zero hashing:
        //    `:q` has no ranges so `have_ranges` is empty (`contains_polled` short-circuits
        //    without hashing), while `:s` has two wide ranges differing only at the last
        //    member. `need_first = true` must poll inside `cmp_ce_polled` and select `:Z1`.
        const N_CMP_OPS: usize = 5000;
        let mut cmp_s = String::from(
            "Prefix(:=<http://ex.org/>)\nOntology(\n\
             SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)\n\
             ObjectPropertyRange(:s ObjectIntersectionOf(",
        );
        for i in 0..N_CMP_OPS {
            cmp_s.push_str(&format!(" :C{i}"));
        }
        cmp_s.push_str(" :Z2))\nObjectPropertyRange(:s ObjectIntersectionOf(");
        for i in 0..N_CMP_OPS {
            cmp_s.push_str(&format!(" :C{i}"));
        }
        cmp_s.push_str(" :Z1))\n)");
        let cmp_doc = crate::ofn_read::parse(&cmp_s).unwrap();
        let (res_no_cmp, p_no_cmp) = run_range_polls(&cmp_doc, false, None);
        assert!(res_no_cmp.unwrap().is_some());
        let (res_with_cmp, p_with_cmp) = run_range_polls(&cmp_doc, true, None);
        let picked = res_with_cmp.unwrap().expect("expected missing range");
        assert!(
            picked.contains("Z1") && !picked.contains("Z2"),
            "expected lexicographically smaller range :Z1, got {picked}"
        );
        assert!(
            p_with_cmp >= p_no_cmp + 4,
            "expected cmp_ce_polled to poll across wide ranges: no_cmp={p_no_cmp}, with_cmp={p_with_cmp}"
        );
        let (cmp_cancel_res, p_cmp_cancel) = run_range_polls(&cmp_doc, true, Some(p_no_cmp + 2));
        assert!(
            matches!(cmp_cancel_res, Err(Error::Cancelled)),
            "expected Cancelled inside cmp_ce_polled, got {cmp_cancel_res:?}"
        );
        assert!(
            p_cmp_cancel > p_no_cmp && p_cmp_cancel < p_with_cmp,
            "expected cancellation inside cmp_ce_polled: no_cmp={p_no_cmp}, cancel={p_cmp_cancel}, with_cmp={p_with_cmp}"
        );
    }

    #[test]
    fn outside_el_releases_missing_and_chains_before_left_set() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // 2,000 violating chains produce `is_outside_bits` (256 B), `missing` (80,000 B),
        // and `chains` (16,384 B). With 2,000 exclusions, `left` resizes 1,792 -> 3,584
        // (overlap 104,448 B) alongside `seen` (69,632 B) = 174,080 B <= `peak_no_excl`
        // (~219,520 B) < 174,080 + 80,000 = 254,080 B. We verify BOTH exact live-scratch
        // release deltas for all 3 buffers (plus the no-chains `else` branch) AND the
        // calibrated `peak_no_excl` cap so deleting ANY single release fails the test.
        const N_CHAINS: usize = 2000;
        const N_EXCL: usize = 2000;
        let mut body =
            String::from("Prefix(:=<http://ex.org/>)\nOntology(\nObjectPropertyRange(:s :T)\n");
        for i in 0..N_CHAINS {
            body.push_str(&format!(
                "SubObjectPropertyOf(ObjectPropertyChain(:p{i} :q) :s)\n"
            ));
        }
        body.push(')');
        let doc = crate::ofn_read::parse(&body).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let excluded: Vec<Excluded> = doc.axioms[1..=N_EXCL]
            .iter()
            .map(|a| Excluded {
                axiom: a.kind.clone(),
                gap: Gap::Outside {
                    rule: CHAIN_RANGE_RULE.to_string(),
                    message: "missing range".to_string(),
                },
            })
            .collect();

        struct RecordingScratch {
            live: Arc<AtomicUsize>,
        }
        impl ScratchBudget for RecordingScratch {
            fn poll(&self) -> Result<(), Error> {
                Ok(())
            }
            fn grow(&mut self, more: usize) -> Result<(), Error> {
                self.live.fetch_add(more, Ordering::Relaxed);
                Ok(())
            }
            fn release(&mut self, less: usize) {
                let prev = self.live.load(Ordering::Relaxed);
                self.live
                    .store(prev.saturating_sub(less), Ordering::Relaxed);
            }
            fn overflow(&self) -> Error {
                Error::Memory {
                    limit: usize::MAX,
                    used: usize::MAX,
                }
            }
        }

        // 1. Exact live-scratch release deltas around each of the three drops.
        let live = Arc::new(AtomicUsize::new(0));
        let bits_before = Arc::new(AtomicUsize::new(0));
        let bits_after = Arc::new(AtomicUsize::new(0));
        let missing_before = Arc::new(AtomicUsize::new(0));
        let missing_after = Arc::new(AtomicUsize::new(0));
        let chains_before = Arc::new(AtomicUsize::new(0));
        let chains_after = Arc::new(AtomicUsize::new(0));
        let live_h = live.clone();
        let bb = bits_before.clone();
        let ba = bits_after.clone();
        let mb = missing_before.clone();
        let ma = missing_after.clone();
        let cb = chains_before.clone();
        let ca = chains_after.clone();
        let mut rec = RecordingScratch { live: live.clone() };
        let out_rec = admission_test::with_hook(
            move |pt| {
                let cur = live_h.load(Ordering::Relaxed);
                match pt {
                    admission_test::Point::OutsideElBeforeDropBits => {
                        bb.store(cur, Ordering::Relaxed);
                    }
                    admission_test::Point::OutsideElAfterDropBits => {
                        ba.store(cur, Ordering::Relaxed);
                    }
                    admission_test::Point::OutsideElBeforeDropMissing => {
                        mb.store(cur, Ordering::Relaxed);
                    }
                    admission_test::Point::OutsideElAfterDropMissing => {
                        ma.store(cur, Ordering::Relaxed);
                    }
                    admission_test::Point::OutsideElBeforeDropChains => {
                        cb.store(cur, Ordering::Relaxed);
                    }
                    admission_test::Point::OutsideElAfterDropChains => {
                        ca.store(cur, Ordering::Relaxed);
                    }
                    _ => {}
                }
            },
            || dl_reduce::Reduction::outside_el(&kinds, &excluded, &mut rec).unwrap(),
        );
        assert_eq!(out_rec.horn, N_CHAINS);
        assert_eq!(out_rec.taken, 0);
        let expected_bits_bytes = kinds.len().div_ceil(64).max(8) * std::mem::size_of::<u64>();
        let expected_missing_bytes = N_CHAINS * std::mem::size_of::<Option<MissingRange<'_>>>();
        let expected_chains_bytes =
            N_CHAINS.next_power_of_two() * std::mem::size_of::<&AxiomKind>();
        let seen_entry_bytes = std::mem::size_of::<(u64, &AxiomKind)>() + 1;
        let expected_seen_bytes = hash_table_bytes(3584, seen_entry_bytes).unwrap();
        let expected_left_overlap = hash_table_bytes(1792, seen_entry_bytes).unwrap()
            + hash_table_bytes(3584, seen_entry_bytes).unwrap();
        assert_eq!(
            bits_before
                .load(Ordering::Relaxed)
                .saturating_sub(bits_after.load(Ordering::Relaxed)),
            expected_bits_bytes,
            "is_outside_bits release delta mismatch"
        );
        assert_eq!(
            missing_before
                .load(Ordering::Relaxed)
                .saturating_sub(missing_after.load(Ordering::Relaxed)),
            expected_missing_bytes,
            "missing release delta mismatch"
        );
        assert_eq!(
            chains_before
                .load(Ordering::Relaxed)
                .saturating_sub(chains_after.load(Ordering::Relaxed)),
            expected_chains_bytes,
            "chains release delta mismatch"
        );
        assert_eq!(
            chains_after.load(Ordering::Relaxed),
            expected_seen_bytes,
            "only `seen` may remain live before building `left`"
        );

        // Also check `is_outside_bits` release on the no-chains (`else`) branch.
        let mut no_chains_body = String::from("Prefix(:=<http://ex.org/>)\nOntology(\n");
        for i in 0..200 {
            no_chains_body.push_str(&format!("DisjointUnion(:C{i} :A{i} :B{i})\n"));
        }
        no_chains_body.push(')');
        let no_chains_doc = crate::ofn_read::parse(&no_chains_body).unwrap();
        let no_chains_kinds: Vec<&AxiomKind> =
            no_chains_doc.axioms.iter().map(|a| &a.kind).collect();
        let nc_live = Arc::new(AtomicUsize::new(0));
        let nc_bb = Arc::new(AtomicUsize::new(0));
        let nc_ba = Arc::new(AtomicUsize::new(0));
        let nc_live_h = nc_live.clone();
        let nc_bb_h = nc_bb.clone();
        let nc_ba_h = nc_ba.clone();
        let mut nc_rec = RecordingScratch { live: nc_live };
        admission_test::with_hook(
            move |pt| {
                let cur = nc_live_h.load(Ordering::Relaxed);
                match pt {
                    admission_test::Point::OutsideElBeforeDropBits => {
                        nc_bb_h.store(cur, Ordering::Relaxed);
                    }
                    admission_test::Point::OutsideElAfterDropBits => {
                        nc_ba_h.store(cur, Ordering::Relaxed);
                    }
                    _ => {}
                }
            },
            || dl_reduce::Reduction::outside_el(&no_chains_kinds, &[], &mut nc_rec).unwrap(),
        );
        let expected_nc_bits =
            no_chains_kinds.len().div_ceil(64).max(8) * std::mem::size_of::<u64>();
        assert_eq!(
            nc_bb
                .load(Ordering::Relaxed)
                .saturating_sub(nc_ba.load(Ordering::Relaxed)),
            expected_nc_bits,
            "no-chains is_outside_bits release delta mismatch"
        );

        // 2. Calibrated cap check with N_EXCL = 2,000: baseline without exclusions establishes
        //    `peak_no_excl`, which lies strictly between the released `left` peak and the stale
        //    `missing`-retained peak.
        let unlimited = Limits::default();
        let mut peak_no_excl = 0usize;
        let mut paid_no_excl = 0usize;
        let mut scratch_no_excl = PostRunScratch {
            base: 0,
            peak: &mut peak_no_excl,
            paid: &mut paid_no_excl,
            limits: &unlimited,
            charge: None,
            check_stop: true,
            scratch: 0,
        };
        let out_no_excl =
            dl_reduce::Reduction::outside_el(&kinds, &[], &mut scratch_no_excl).unwrap();
        assert_eq!(out_no_excl.horn, N_CHAINS);
        assert_eq!(out_no_excl.taken, N_CHAINS);
        assert!(
            expected_seen_bytes + expected_left_overlap <= peak_no_excl
                && peak_no_excl
                    < expected_seen_bytes + expected_left_overlap + expected_missing_bytes,
            "cap calibration broken: released_left={}, peak_no_excl={peak_no_excl}, stale_missing_left={}",
            expected_seen_bytes + expected_left_overlap,
            expected_seen_bytes + expected_left_overlap + expected_missing_bytes
        );

        // With 2,000 exclusions, `left` must fit within `peak_no_excl` with zero extra
        // pool grant once `is_outside_bits`, `missing`, and `chains` are released.
        let granted = Arc::new(AtomicUsize::new(0));
        let granted_c = granted.clone();
        let capped_limits = Limits {
            max_memory: Some(peak_no_excl),
            charge: Some(Arc::new(move |bytes| {
                granted_c.fetch_add(bytes, Ordering::Relaxed);
                true
            })),
            ..Limits::default()
        };
        let charge_ref = capped_limits
            .charge
            .as_deref()
            .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
        let mut peak_with_excl = peak_no_excl;
        let mut paid_with_excl = peak_no_excl;
        let mut scratch_with_excl = PostRunScratch {
            base: 0,
            peak: &mut peak_with_excl,
            paid: &mut paid_with_excl,
            limits: &capped_limits,
            charge: charge_ref,
            check_stop: true,
            scratch: 0,
        };
        let out_with_excl =
            dl_reduce::Reduction::outside_el(&kinds, &excluded, &mut scratch_with_excl).unwrap();
        assert_eq!(out_with_excl.horn, N_CHAINS);
        assert_eq!(out_with_excl.taken, 0);
        assert_eq!(
            granted.load(Ordering::Relaxed),
            0,
            "left set requested pool grant for already-freed missing/chains buffers"
        );
    }

    /// Rebase pre-admission (Item 753 Phase 1): an Eager edit that adds a fresh name and
    /// would shift populated DL tables (`pred_in`/`pred_seen`/`sent`) pre-admits the peak
    /// single-table scratch before remapping. Refusing the grant at `RebaseAdmit` stops
    /// before `RebaseShift` runs and retries fresh under the same limits with identical
    /// classification results.
    #[test]
    fn eager_rebase_admits_dl_table_scratch_before_shifting() {
        let mut body = String::from(
            "SubClassOf(:A ObjectUnionOf(:X ObjectSomeValuesFrom(:r :B)))
             SubClassOf(:A ObjectAllValuesFrom(:r :C))
             SubClassOf(ObjectIntersectionOf(:B :C) owl:Nothing)
             SubClassOf(:X :Y)
             SubClassOf(:K :L)\n",
        );
        // Populate `:S` with many distinct eager conjunction successors `[B, Ci]` so its
        // `pred_in`, `pred_seen`, and `sent` tables hold dynamic context IDs >= base.
        for i in 0..48 {
            body.push_str(&format!(
                "SubClassOf(:S ObjectSomeValuesFrom(:r{i} :B))
                 SubClassOf(:S ObjectAllValuesFrom(:r{i} :C{i}))
                 SubClassOf(ObjectIntersectionOf(:B :C{i}) ObjectUnionOf(:P{i} :Q{i}))
                 SubClassOf(:P{i} :R{i})
                 SubClassOf(:Q{i} :R{i})
                 SubClassOf(ObjectSomeValuesFrom(:r{i} :R{i}) :W{i})\n"
            ));
        }
        let base_doc = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
        ))
        .unwrap();
        let edit_doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)\nOntology(\nSubClassOf(:FreshName :K)\n)",
        )
        .unwrap();

        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let mut k = Classifier::of(&base_doc.axioms);
        let first = k.classify(&eager).unwrap();
        assert_eq!(first.stats.tier, Tier::Disjunctive);
        assert!(first.taxonomy.is_subclass(
            &Class::new("http://ex.org/A"),
            &Class::new("http://ex.org/Y")
        ));

        // Verify the kept state actually has non-zero shift_scratch on populated DL tables.
        let from = k.sat.dynamic.as_ref().expect("dynamic contexts").base();
        let mut noop = |_| Ok(());
        let max_scratch = k
            .sat
            .ctx
            .iter()
            .filter_map(|s| s.get())
            .map(|c| c.st.lock().shift_scratch(from, &mut noop).unwrap())
            .max()
            .unwrap_or(0);
        assert!(
            max_scratch > 0,
            "expected populated DL tables referencing dynamic successors >= {from}"
        );

        let mut held = base_doc.axioms.clone();
        held.extend(edit_doc.axioms.clone());
        let want = Classifier::of(&held).classify(&eager).unwrap();

        // 1. Observe the exact `(live, scratch, paid)` values at `RebaseAdmit` and assert
        // that `scratch == max_scratch > 0` and that `live + scratch` strictly exceeds
        // `live.max(paid)`. Choosing `cap` strictly between `live.max(paid)` and
        // `live + scratch` isolates the scratch contribution: omitting `+ rebase_scratch`
        // would allow admission and reach `RebaseShift`.
        let observed = Arc::new(parking_lot::Mutex::new(None::<(usize, usize, usize)>));
        let obs_hook = observed.clone();
        let mut k_probe = Classifier::of(&base_doc.axioms);
        k_probe.classify(&eager).unwrap();
        k_probe.add(&edit_doc.axioms);
        let probe_out = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::RebaseAdmit {
                    live,
                    scratch,
                    paid,
                } = pt
                {
                    *obs_hook.lock() = Some((live, scratch, paid));
                }
            },
            || k_probe.classify(&eager).unwrap(),
        );
        assert!(probe_out.stats.incremental);
        let (live, scratch, paid) = observed
            .lock()
            .expect("RebaseAdmit was not reached on probe run");
        assert_eq!(scratch, max_scratch);
        let baseline = live.max(paid);
        assert!(
            live + scratch > baseline,
            "rebase_held ({live} + {scratch}) did not exceed baseline ({baseline})"
        );
        let cap = baseline + (live + scratch - baseline) / 2;
        assert!(cap >= baseline && cap < live + scratch);

        // 2. `max_memory: Some(cap)` passes normalization (`paid <= cap`) and reaches
        // `RebaseAdmit`, refuses specifically because of `rebase_scratch`, skips
        // `RebaseShift`, and retries fresh under the same cap with identical results.
        let saw_admit = Arc::new(AtomicBool::new(false));
        let saw_shift = Arc::new(AtomicBool::new(false));
        let sa = saw_admit.clone();
        let ss = saw_shift.clone();
        k.add(&edit_doc.axioms);
        let out_max = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::RebaseAdmit { .. } => sa.store(true, Ordering::SeqCst),
                admission_test::Point::RebaseShift => ss.store(true, Ordering::SeqCst),
                _ => {}
            },
            || {
                k.classify(&Limits {
                    max_memory: Some(cap),
                    ..eager.clone()
                })
                .unwrap()
            },
        );
        assert!(saw_admit.load(Ordering::SeqCst), "RebaseAdmit not reached");
        assert!(
            !saw_shift.load(Ordering::SeqCst),
            "RebaseShift ran despite max_memory cap below live + scratch"
        );
        assert!(!out_max.stats.incremental);
        assert_eq!(out_max.taxonomy, want.taxonomy);
        assert_eq!(out_max.consistency, want.consistency);
        assert_eq!(out_max.complete, want.complete);
        assert!(k.excluded().is_empty());

        // 3. Pool `charge` capped at `cap` also admits everything through `paid <= cap`,
        // refuses the `RebaseAdmit` scratch delta, skips `RebaseShift`, and retries fresh.
        let mut k_charge = Classifier::of(&base_doc.axioms);
        k_charge.classify(&eager).unwrap();
        k_charge.add(&edit_doc.axioms);
        let saw_admit = Arc::new(AtomicBool::new(false));
        let saw_shift = Arc::new(AtomicBool::new(false));
        let charged = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicUsize::new(0));
        let sa = saw_admit.clone();
        let ss = saw_shift.clone();
        let ch = charged.clone();
        let rf = refused.clone();
        let out_charge = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::RebaseAdmit { .. } => sa.store(true, Ordering::SeqCst),
                admission_test::Point::RebaseShift => ss.store(true, Ordering::SeqCst),
                _ => {}
            },
            || {
                k_charge
                    .classify(&Limits {
                        charge: Some(Arc::new(move |bytes| {
                            let prev = ch.fetch_add(bytes, Ordering::SeqCst);
                            if prev + bytes > cap {
                                ch.fetch_sub(bytes, Ordering::SeqCst);
                                rf.fetch_add(1, Ordering::SeqCst);
                                false
                            } else {
                                true
                            }
                        })),
                        ..eager.clone()
                    })
                    .unwrap()
            },
        );
        assert!(saw_admit.load(Ordering::SeqCst), "RebaseAdmit not reached");
        assert!(refused.load(Ordering::SeqCst) > 0, "charge did not refuse");
        assert!(
            !saw_shift.load(Ordering::SeqCst),
            "RebaseShift ran despite charge cap below live + scratch"
        );
        assert!(!out_charge.stats.incremental);
        assert!(charged.load(Ordering::SeqCst) <= cap);
        assert_eq!(out_charge.taxonomy, want.taxonomy);
        assert_eq!(out_charge.consistency, want.consistency);
        assert!(k_charge.excluded().is_empty());

        // 4. Cancellation arriving during the rebase scratch scan stops before
        // `RebaseAdmit`, `charge`, and `RebaseShift` run.
        let mut k_stop = Classifier::of(&base_doc.axioms);
        k_stop.classify(&eager).unwrap();
        k_stop.add(&edit_doc.axioms);
        let cancel_armed = Arc::new(AtomicBool::new(false));
        let saw_admit = Arc::new(AtomicBool::new(false));
        let saw_shift = Arc::new(AtomicBool::new(false));
        let charge_after_arm = Arc::new(AtomicUsize::new(0));
        let ca_hook = cancel_armed.clone();
        let ca_poll = cancel_armed.clone();
        let ca_charge = cancel_armed.clone();
        let sa = saw_admit.clone();
        let ss = saw_shift.clone();
        let caa = charge_after_arm.clone();
        let err = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::RebaseScratchStart => {
                    ca_hook.store(true, Ordering::SeqCst);
                }
                admission_test::Point::RebaseAdmit { .. } => sa.store(true, Ordering::SeqCst),
                admission_test::Point::RebaseShift => ss.store(true, Ordering::SeqCst),
                _ => {}
            },
            || {
                k_stop.classify(&Limits {
                    cancelled: Some(Arc::new(move || ca_poll.load(Ordering::SeqCst))),
                    charge: Some(Arc::new(move |_| {
                        if ca_charge.load(Ordering::SeqCst) {
                            caa.fetch_add(1, Ordering::SeqCst);
                        }
                        true
                    })),
                    ..eager.clone()
                })
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(
            !saw_admit.load(Ordering::SeqCst),
            "RebaseAdmit ran after cancellation during scratch scan"
        );
        assert_eq!(
            charge_after_arm.load(Ordering::SeqCst),
            0,
            "charge callback ran after cancellation during scratch scan"
        );
        assert!(!saw_shift.load(Ordering::SeqCst));
        let recovered = k_stop.classify(&eager).unwrap();
        assert!(!recovered.stats.incremental);
        assert_eq!(recovered.taxonomy, want.taxonomy);
    }

    /// Cooperative polling in `invalidate_changed` (Item 753 Phase 1):
    /// 1. A 4500-node reverse-dependency chain (`edges == 1`) cancels inside the BFS closure
    ///    loop (`InvalidateClosureStart..InvalidateClosureEnd`).
    /// 2. A single high-fan-in successor (`:Hub`, 4500 predecessors in `st.back`) cancels
    ///    strictly inside `InvalidateBackListStart..InvalidateBackListEnd`.
    /// 3. A single high-fan-in eager DL successor (4500 predecessors in `d.dpreds`) cancels
    ///    strictly inside `InvalidateDlPredsStart..InvalidateDlPredsEnd`.
    /// 4. A 4500-context seed set cancels inside `InvalidateInitialStart..InvalidateInitialEnd`
    ///    with no unpolled seed-copy pass before `InvalidateClosureStart`.
    #[test]
    fn invalidate_changed_polls_chain_and_high_fan_in_and_recovers() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms
        };

        let assert_point_window_cancels = |base: &[Axiom],
                                           edit: &[Axiom],
                                           start_pt: admission_test::Point,
                                           end_pt: admission_test::Point,
                                           label: &str| {
            let mut k_ok = Classifier::of(base);
            let base_out = k_ok.classify(&eager).unwrap();
            assert_eq!(base_out.stats.tier, Tier::Disjunctive);
            k_ok.add(edit);
            let ok = k_ok.classify(&eager).unwrap();
            assert!(ok.stats.incremental, "{label}: expected incremental run");
            assert!(
                ok.stats.cleared >= 4500,
                "{label}: cleared={}",
                ok.stats.cleared
            );

            let mut k = Classifier::of(base);
            k.classify(&eager).unwrap();
            k.add(edit);
            let in_window = Arc::new(AtomicBool::new(false));
            let saw_start = Arc::new(AtomicBool::new(false));
            let saw_end = Arc::new(AtomicBool::new(false));
            let window_polls = Arc::new(AtomicUsize::new(0));
            let iw_hook = in_window.clone();
            let iw_poll = in_window.clone();
            let ss = saw_start.clone();
            let se = saw_end.clone();
            let wp = window_polls.clone();
            let err = admission_test::with_hook(
                move |pt| {
                    if pt == start_pt {
                        ss.store(true, Ordering::SeqCst);
                        iw_hook.store(true, Ordering::SeqCst);
                    } else if pt == end_pt {
                        iw_hook.store(false, Ordering::SeqCst);
                        se.store(true, Ordering::SeqCst);
                    }
                },
                || {
                    k.classify(&Limits {
                        cancelled: Some(Arc::new(move || {
                            if iw_poll.load(Ordering::SeqCst) {
                                wp.fetch_add(1, Ordering::SeqCst);
                                true
                            } else {
                                false
                            }
                        })),
                        ..eager.clone()
                    })
                },
            )
            .unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{label}: {err:?}");
            assert!(
                saw_start.load(Ordering::SeqCst),
                "{label}: {start_pt:?} was not reached"
            );
            assert!(
                window_polls.load(Ordering::SeqCst) > 0,
                "{label}: no cooperative poll ran between {start_pt:?} and {end_pt:?}"
            );
            assert!(
                !saw_end.load(Ordering::SeqCst),
                "{label}: reached {end_pt:?} without stopping on cancellation"
            );
            let recovered = k.classify(&eager).unwrap();
            assert!(!recovered.stats.incremental);
            assert_eq!(recovered.taxonomy, ok.taxonomy);
        };

        // Case 1: 4500-node reverse-dependency chain with 1 backward edge per context
        // (verifies polling across closure queue pops).
        let mut chain_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..4500usize {
            chain_body.push_str(&format!(
                "SubClassOf(:C{i} ObjectSomeValuesFrom(:r :C{}))\n",
                i + 1
            ));
        }
        let chain_base = parse(&chain_body);
        let chain_edit = parse("SubClassOf(:C4500 :Target)");
        assert_point_window_cancels(
            &chain_base,
            &chain_edit,
            admission_test::Point::InvalidateClosureStart,
            admission_test::Point::InvalidateClosureEnd,
            "4500-node chain",
        );

        // Case 2: Single high-fan-in successor `:Hub` with 4500 predecessors in `st.back`:
        // cancellation is armed strictly inside `:Hub`'s predecessor list walk.
        let mut fan_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..4500usize {
            fan_body.push_str(&format!(
                "SubClassOf(:F{i} ObjectSomeValuesFrom(:r :Hub))\n"
            ));
        }
        let fan_base = parse(&fan_body);
        let fan_edit = parse("SubClassOf(:Hub :Target)");
        assert_point_window_cancels(
            &fan_base,
            &fan_edit,
            admission_test::Point::InvalidateBackListStart,
            admission_test::Point::InvalidateBackListEnd,
            "4500-predecessor st.back fan-in",
        );

        // Case 3: Single high-fan-in eager DL successor (`:HubA ⊓ :HubB`) with 4500
        // predecessors in `d.dpreds`: cancellation is armed strictly inside `d.dpreds`.
        let mut dl_fan_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..4500usize {
            dl_fan_body.push_str(&format!(
                "SubClassOf(:D{i} ObjectSomeValuesFrom(:r :HubA))\n\
                 SubClassOf(:D{i} ObjectAllValuesFrom(:r :HubB))\n"
            ));
        }
        let dl_fan_base = parse(&dl_fan_body);
        let dl_fan_edit = parse("SubClassOf(:HubA :Target)");
        assert_point_window_cancels(
            &dl_fan_base,
            &dl_fan_edit,
            admission_test::Point::InvalidateDlPredsStart,
            admission_test::Point::InvalidateDlPredsEnd,
            "4500-predecessor d.dpreds fan-in",
        );

        // Case 4: 4500 seed-affected contexts all holding `:Seed`; initial scan enqueues
        // hits directly without an unpolled post-scan `.collect()` copy.
        let mut seed_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..4500usize {
            seed_body.push_str(&format!("SubClassOf(:S{i} :Seed)\n"));
        }
        let seed_base = parse(&seed_body);
        let seed_edit = parse("SubClassOf(:Seed :Target)");
        assert_point_window_cancels(
            &seed_base,
            &seed_edit,
            admission_test::Point::InvalidateInitialStart,
            admission_test::Point::InvalidateInitialEnd,
            "4500-seed initial scan",
        );
    }

    /// Dense invalidation seeding above a queue capacity boundary (Item 753 Phase 1):
    /// 1. `invalidation_scratch_peak` bounds both steady-state and single-collection resize
    ///    overlap of `affected: FxHashSet<Ex>` and slot-capped `queue: Vec<Ex>` across
    ///    power-of-two and 7/8 load-factor boundaries.
    /// 2. With >8192 seeded contexts out of <16384 total slots and a renumbered disjointness
    ///    edit, `queue` stays capped at `slots` (never doubling to 16384), `affected + queue`
    ///    stays within the pre-admitted scratch bound, and `queue` is released before
    ///    `DisjointGrowthAdmit` so only `affected` remains live alongside `live_base`.
    #[test]
    fn invalidate_changed_dense_seeds_stay_within_scratch_bound_and_release_queue() {
        let ex = std::mem::size_of::<Ex>();
        for slots in [
            0usize, 1, 3, 4, 5, 7, 8, 15, 16, 7_168, 7_169, 8_192, 8_193, 8_200, 14_336, 14_337,
            16_384, 16_385,
        ] {
            let bound = invalidation_scratch_peak(slots);
            let mut affected: FxHashSet<Ex> = FxHashSet::default();
            let mut queue: Vec<Ex> = Vec::new();
            let mut peak = 0usize;
            for i in 0..slots as Ex {
                let old_a = dl_clause::hash_table_bytes(affected.capacity(), ex);
                let q_cur = queue.capacity() * ex;
                affected.insert(i);
                let new_a = dl_clause::hash_table_bytes(affected.capacity(), ex);
                let a_step = if new_a != old_a { old_a + new_a } else { new_a };
                peak = peak.max(a_step + q_cur);

                let old_q = queue.capacity() * ex;
                push_capped(&mut queue, i, slots);
                let new_q = queue.capacity() * ex;
                let q_step = if new_q != old_q { old_q + new_q } else { new_q };
                peak = peak.max(new_a + q_step);
            }
            assert_eq!(queue.capacity(), slots, "slots={slots}");
            assert!(
                peak <= bound,
                "slots={slots}: resize peak {peak} exceeded bound {bound}"
            );
            let live = dl_clause::hash_table_bytes(affected.capacity(), ex) + queue.capacity() * ex;
            assert!(
                live <= bound,
                "slots={slots}: live scratch {live} exceeded bound {bound}"
            );
        }

        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms
        };

        // >8192 seeded classes (:S0..:S8199 -> :Seed) plus a small kept disjoint cluster
        // (:Keep0..:Keep3 -> :Hub -> :DA{j} disjoint :DB{j}) so `slots` sits just above 8192
        // (well below 16384), `affected.len() >= 8200`, and a renumbered disjoint edit triggers
        // `DisjointGrowthAdmit` right after closure.
        let mut body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..8_200usize {
            body.push_str(&format!("SubClassOf(:S{i} :Seed)\n"));
        }
        for j in 0..32usize {
            body.push_str(&format!(
                "SubClassOf(:Hub :DA{j})\nDisjointClasses(:DA{j} :DB{j})\n"
            ));
        }
        for k in 0..4usize {
            body.push_str(&format!("SubClassOf(:Keep{k} :Hub)\n"));
        }
        let base = parse(&body);
        let edit = parse("SubClassOf(:Seed :Target)\nDisjointClasses(:DZ0 :DZ1)\n");

        let mut k = Classifier::of(&base);
        let first = k.classify(&eager).unwrap();
        assert_eq!(first.stats.tier, Tier::Disjunctive);
        // Shrink `:Keep{k}` disjointness maps to 1 entry so `reindex_disjoint` must grow them
        // and reach `DisjointGrowthAdmit`.
        for idx in 0..4usize {
            let iri = format!("http://ex.org/Keep{idx}");
            let id = k
                .sat
                .interner
                .get(&Node::Class(
                    k.sat
                        .interner
                        .classes
                        .iter()
                        .position(|c| c.as_str() == iri)
                        .unwrap() as u32,
                ))
                .unwrap();
            let ctx = k.sat.ctx[id as usize].get().unwrap();
            let mut st = ctx.st.lock();
            let (&first_key, &first_val) = st.disj.iter().next().unwrap();
            st.disj = FxHashMap::default();
            st.disj.insert(first_key, first_val);
            assert!(st.disj.capacity() < 32);
        }
        k.add(&edit);

        struct WatchReset;
        impl Drop for WatchReset {
            fn drop(&mut self) {
                let _ = numeric::tests::take_watched_deallocations();
            }
        }
        let _watch_reset = WatchReset;
        let scratch_obs = Arc::new(parking_lot::Mutex::new(
            None::<(usize, usize, usize, usize, usize, usize, usize)>,
        ));
        let admit_obs = Arc::new(parking_lot::Mutex::new(
            None::<(usize, usize, usize, Option<usize>)>,
        ));
        let so = scratch_obs.clone();
        let ao = admit_obs.clone();
        let out = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::InvalidateScratch {
                    slots,
                    affected_len,
                    affected_bytes,
                    queue_cap,
                    queue_bytes,
                    queue_ptr,
                    bound,
                    live_base,
                } => {
                    assert!(queue_cap > 0);
                    numeric::tests::watch_deallocation(
                        queue_ptr,
                        std::alloc::Layout::array::<Ex>(queue_cap).unwrap(),
                    );
                    *so.lock() = Some((
                        slots,
                        affected_len,
                        affected_bytes,
                        queue_cap,
                        queue_bytes,
                        bound,
                        live_base,
                    ));
                }
                admission_test::Point::DisjointGrowthAdmit { live, growth, paid } => {
                    let deallocs = numeric::tests::watched_deallocations();
                    *ao.lock() = Some((live, growth, paid, deallocs));
                }
                _ => {}
            },
            || k.classify(&eager).unwrap(),
        );
        assert!(out.stats.incremental);
        assert!(out.stats.cleared >= 8_200);
        let (slots, affected_len, affected_bytes, queue_cap, queue_bytes, bound, live_base) =
            scratch_obs
                .lock()
                .expect("InvalidateScratch was not observed");
        assert!(
            affected_len > 8_192 && slots < 16_384,
            "expected >8192 seeds in <16384 slots, got affected_len={affected_len}, slots={slots}"
        );
        assert!(
            queue_cap <= slots && queue_cap < 16_384,
            "queue overshot slot bound: queue_cap={queue_cap}, slots={slots}"
        );
        assert!(
            affected_bytes + queue_bytes <= bound,
            "live invalidation scratch ({affected_bytes} + {queue_bytes}) exceeded bound {bound}"
        );

        let (admit_live, admit_growth, _, queue_deallocs_at_admit) = admit_obs
            .lock()
            .expect("DisjointGrowthAdmit was not observed");
        assert!(admit_growth > 0);
        // `queue` is physically deallocated before `DisjointGrowthAdmit`, so its backing
        // buffer must already have been freed and live memory equals `live_base + affected_bytes`.
        assert!(
            queue_deallocs_at_admit.is_some_and(|count| count >= 1),
            "queue backing buffer was still live at DisjointGrowthAdmit"
        );
        assert_eq!(admit_live, live_base + affected_bytes);

        let mut held = base;
        held.extend(edit);
        let want = Classifier::of(&held).classify(&eager).unwrap();
        assert_eq!(out.taxonomy, want.taxonomy);
    }

    /// Equal-length differing index lists (`told_of`, `conj_of`, `exist_of`, and disjointness
    /// sets) must stay within `Index::changed_scratch` and the Tier D `Expansion::Eager`
    /// comparison admission reservation (Item 753 Phase 1, Sol r5 P2).
    #[test]
    fn tier_d_eager_equal_length_index_lists_stay_within_comparison_reservation() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms
        };
        let dl_seed = "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n";

        // Build two consecutive indexes on the same `Interner` and verify actual peak heap
        // bytes during `Index::changed` never exceed the scratch estimate.
        let assert_changed_within_scratch = |label: &str, base: &[Axiom], updated: &[Axiom]| {
            let mut sat = Saturation::default();
            let build_ix = |sat: &mut Saturation, axioms: &[Axiom]| {
                let kinds: Vec<&AxiomKind> = axioms.iter().map(|a| &a.kind).collect();
                let outside: Vec<(usize, (String, String))> = kinds
                    .iter()
                    .enumerate()
                    .filter_map(|(i, k)| {
                        let b = el_break(k)?;
                        Some((i, (b.rule.to_string(), b.message)))
                    })
                    .collect();
                let (_, ix, _, _, _) = sat
                    .build(
                        &kinds,
                        &outside,
                        1,
                        0,
                        0,
                        &eager,
                        AttemptProvenance::Ineligible,
                    )
                    .unwrap();
                ix
            };
            let old_ix = build_ix(&mut sat, base);
            let new_ix = build_ix(&mut sat, updated);
            assert!(
                new_ix.same_tables(&old_ix),
                "{label}: fixture must remain eligible for index comparison"
            );
            let poll = || Ok::<(), Error>(());
            let estimate = new_ix.changed_scratch(&old_ix, &poll).unwrap();
            let (changed, sample) = numeric::tests::measure(|| new_ix.changed(&old_ix, &poll));
            let changed = changed.expect("uncancelled comparison must return marks");
            assert!(
                changed.iter().any(|&c| c),
                "{label}: replacement must mark at least one expression changed"
            );
            assert!(
                sample.peak <= estimate,
                "{label}: Index::changed peak heap ({}) exceeded changed_scratch ({estimate})",
                sample.peak
            );
            (sat, old_ix, new_ix, estimate, sample.peak)
        };

        // 1. Equal-length told superclasses (Sol r5 exact example: 256 distinct told
        // superclasses on `:A`, replacing `:T255` with already-declared `:T256`).
        let mut told_base_body = String::from(dl_seed);
        told_base_body.push_str("Declaration(Class(:T256))\n");
        for i in 0..256usize {
            told_base_body.push_str(&format!("SubClassOf(:A :T{i})\n"));
        }
        let told_base = parse(&told_base_body);
        let told_rm = parse("SubClassOf(:A :T255)\n");
        let told_add = parse("SubClassOf(:A :T256)\n");
        let mut told_updated: Vec<Axiom> = told_base
            .iter()
            .filter(|&a| a != &told_rm[0])
            .cloned()
            .collect();
        told_updated.extend(told_add.iter().cloned());
        let (sat_told, old_told, new_told, _, _) =
            assert_changed_within_scratch("told_of 256", &told_base, &told_updated);
        let a_told_ex = sat_told
            .interner
            .get(&Node::Class(
                sat_told
                    .interner
                    .classes
                    .iter()
                    .position(|c| c.as_str() == "http://ex.org/A")
                    .unwrap() as u32,
            ))
            .unwrap();
        assert_eq!(old_told.told_of(old_told.head(a_told_ex)).len(), 256);
        assert_eq!(new_told.told_of(new_told.head(a_told_ex)).len(), 256);
        assert_ne!(
            old_told.told_of(old_told.head(a_told_ex)),
            new_told.told_of(new_told.head(a_told_ex))
        );

        // 2. Equal-length conjunction lists (256 distinct binary conjunctions on `:A`,
        // replacing the 256th partner `:C255` with already-declared `:C256`).
        let mut conj_base_body = String::from(dl_seed);
        conj_base_body.push_str("Declaration(Class(:C256))\n");
        for i in 0..256usize {
            conj_base_body.push_str(&format!(
                "SubClassOf(ObjectIntersectionOf(:A :C{i}) :Out)\n"
            ));
        }
        let conj_base = parse(&conj_base_body);
        let conj_rm = parse("SubClassOf(ObjectIntersectionOf(:A :C255) :Out)\n");
        let conj_add = parse("SubClassOf(ObjectIntersectionOf(:A :C256) :Out)\n");
        let mut conj_updated: Vec<Axiom> = conj_base
            .iter()
            .filter(|&a| a != &conj_rm[0])
            .cloned()
            .collect();
        conj_updated.extend(conj_add.iter().cloned());
        let (sat_conj, old_conj, new_conj, _, _) =
            assert_changed_within_scratch("conj_of 256", &conj_base, &conj_updated);
        let a_conj_ex = sat_conj
            .interner
            .get(&Node::Class(
                sat_conj
                    .interner
                    .classes
                    .iter()
                    .position(|c| c.as_str() == "http://ex.org/A")
                    .unwrap() as u32,
            ))
            .unwrap();
        assert_eq!(old_conj.conj_of(old_conj.head(a_conj_ex)).len(), 256);
        assert_eq!(new_conj.conj_of(new_conj.head(a_conj_ex)).len(), 256);
        assert_ne!(
            old_conj.conj_of(old_conj.head(a_conj_ex)),
            new_conj.conj_of(new_conj.head(a_conj_ex))
        );

        // 3. Equal-length existential filler lists (256 distinct negative existentials on `:A`,
        // replacing `:r255` with already-declared `:r256`).
        let mut exist_base_body = String::from(dl_seed);
        exist_base_body.push_str("SubObjectPropertyOf(:r256 :r256)\n");
        for i in 0..256usize {
            exist_base_body.push_str(&format!(
                "SubClassOf(ObjectSomeValuesFrom(:r{i} :A) :Out)\n"
            ));
        }
        let exist_base = parse(&exist_base_body);
        let exist_rm = parse("SubClassOf(ObjectSomeValuesFrom(:r255 :A) :Out)\n");
        let exist_add = parse("SubClassOf(ObjectSomeValuesFrom(:r256 :A) :Out)\n");
        let mut exist_updated: Vec<Axiom> = exist_base
            .iter()
            .filter(|&a| a != &exist_rm[0])
            .cloned()
            .collect();
        exist_updated.extend(exist_add.iter().cloned());
        let (sat_exist, old_exist, new_exist, _, _) =
            assert_changed_within_scratch("exist_of 256", &exist_base, &exist_updated);
        let a_exist_ex = sat_exist
            .interner
            .get(&Node::Class(
                sat_exist
                    .interner
                    .classes
                    .iter()
                    .position(|c| c.as_str() == "http://ex.org/A")
                    .unwrap() as u32,
            ))
            .unwrap();
        assert_eq!(old_exist.exist_of(old_exist.head(a_exist_ex)).len(), 256);
        assert_eq!(new_exist.exist_of(new_exist.head(a_exist_ex)).len(), 256);
        assert_ne!(
            old_exist.exist_of(old_exist.head(a_exist_ex)),
            new_exist.exist_of(new_exist.head(a_exist_ex))
        );

        // 4. Disjointness sets (small 1-set case and 32-set equal-length replacement).
        for n_disj in [1usize, 32] {
            let mut disj_base_body = String::from(dl_seed);
            disj_base_body.push_str("Declaration(Class(:ExtraD))\n");
            for i in 0..n_disj {
                disj_base_body.push_str(&format!("DisjointClasses(:A :D{i})\n"));
            }
            let disj_base = parse(&disj_base_body);
            let disj_rm = parse(&format!("DisjointClasses(:A :D{})\n", n_disj - 1));
            let disj_add = parse("DisjointClasses(:A :ExtraD)\n");
            let mut disj_updated: Vec<Axiom> = disj_base
                .iter()
                .filter(|&a| a != &disj_rm[0])
                .cloned()
                .collect();
            disj_updated.extend(disj_add.iter().cloned());
            assert_changed_within_scratch(&format!("disjoint {n_disj}"), &disj_base, &disj_updated);
        }

        let mut wide_body = String::from(dl_seed);
        wide_body.push_str("DisjointClasses(");
        for i in 0..4096 {
            wide_body.push_str(&format!(" :Wide{i}"));
        }
        wide_body.push_str(")\n");
        let wide_base = parse(&wide_body);
        wide_body.push_str("SubClassOf(:P :Q)\n");
        let wide_updated = parse(&wide_body);
        let (_, _, _, estimate, peak) =
            assert_changed_within_scratch("one 4096-member set", &wide_base, &wide_updated);
        // The two member buffers take 32 KiB. One KiB also covers the two table entries,
        // the outer vector and the numbering arrays.
        assert!(
            estimate <= 33 * 1024,
            "one 4096-member set reserved {estimate} bytes for a measured {peak}-byte peak"
        );

        // 5. Check that both admission limits enforce the comparison reservation on equal-length
        // `told_of`, `conj_of`, and `exist_of` replacements.
        // include an unrelated 81-role transitive hierarchy (:p0 ⊑ ... ⊑ :p80, transitive :p80)
        // so `old_ix_bytes` (~400 KiB of role-composition tables, untouched by the edit)
        // dominates both the post-drop invalidation peak and fresh saturation/taxonomy scratch
        // (`want.stats.peak_memory < held`), then verify `held - 1` refuses right at
        // `CompareAdmit` (never reaching `InvalidateInitialStart`) and falls back cleanly to a
        // complete Tier D fresh run under both `max_memory` and pool `charge`, while exact
        // `held` admits incremental reuse.
        let mut weight = String::from("TransitiveObjectProperty(:p80)\n");
        for r in 0..80usize {
            weight.push_str(&format!("SubObjectPropertyOf(:p{r} :p{})\n", r + 1));
        }
        let with_weight = |body: &str| parse(&format!("{weight}{body}"));
        let told_e2e_base = with_weight(&told_base_body);
        let mut told_e2e_updated: Vec<Axiom> = told_e2e_base
            .iter()
            .filter(|&a| a != &told_rm[0])
            .cloned()
            .collect();
        told_e2e_updated.extend(told_add.iter().cloned());
        let conj_e2e_base = with_weight(&conj_base_body);
        let mut conj_e2e_updated: Vec<Axiom> = conj_e2e_base
            .iter()
            .filter(|&a| a != &conj_rm[0])
            .cloned()
            .collect();
        conj_e2e_updated.extend(conj_add.iter().cloned());
        let exist_e2e_base = with_weight(&exist_base_body);
        let mut exist_e2e_updated: Vec<Axiom> = exist_e2e_base
            .iter()
            .filter(|&a| a != &exist_rm[0])
            .cloned()
            .collect();
        exist_e2e_updated.extend(exist_add.iter().cloned());

        for (label, base, rm, add, updated) in [
            (
                "told_of 256",
                &told_e2e_base,
                &told_rm,
                &told_add,
                &told_e2e_updated,
            ),
            (
                "conj_of 256",
                &conj_e2e_base,
                &conj_rm,
                &conj_add,
                &conj_e2e_updated,
            ),
            (
                "exist_of 256",
                &exist_e2e_base,
                &exist_rm,
                &exist_add,
                &exist_e2e_updated,
            ),
        ] {
            let want = Classifier::of(updated).classify(&eager).unwrap();
            assert_eq!(want.stats.tier, Tier::Disjunctive);

            let make_edited = || {
                let mut k = Classifier::of(base);
                let first = k.classify(&eager).unwrap();
                assert_eq!(first.stats.tier, Tier::Disjunctive);
                k.remove(rm);
                k.add(add);
                k
            };

            let cmp_obs = Arc::new(parking_lot::Mutex::new(
                None::<(usize, usize, usize, usize, usize)>,
            ));
            let co = cmp_obs.clone();
            let mut k_probe = make_edited();
            let probe = admission_test::with_hook(
                move |pt| {
                    if let admission_test::Point::CompareAdmit {
                        live_base,
                        old_ix_bytes,
                        changed_scratch,
                        inv_peak,
                        held,
                    } = pt
                    {
                        *co.lock() =
                            Some((live_base, old_ix_bytes, changed_scratch, inv_peak, held));
                    }
                },
                || k_probe.classify(&eager).unwrap(),
            );
            assert!(
                probe.stats.incremental,
                "{label}: probe must run incrementally"
            );
            assert_eq!(probe.taxonomy, want.taxonomy);

            let (live_base, old_ix_bytes, changed_scratch, inv_peak, held) =
                cmp_obs.lock().expect("CompareAdmit was not observed");
            let cmp_arm = live_base + old_ix_bytes + changed_scratch;
            assert!(
                cmp_arm > inv_peak,
                "{label}: expected comparison arm ({cmp_arm}) to exceed post-drop invalidation peak ({inv_peak})"
            );
            assert_eq!(held, cmp_arm);
            assert!(
                want.stats.peak_memory < held,
                "{label}: expected fresh peak ({}) < comparison reservation ({held})",
                want.stats.peak_memory
            );

            // Refusal at `held - 1` under `max_memory` stops at `CompareAdmit` (before
            // `InvalidateInitialStart`) and falls back cleanly to a fresh Tier D run.
            let mut k_refuse_mem = make_edited();
            let saw_cmp = Arc::new(AtomicBool::new(false));
            let saw_inv = Arc::new(AtomicBool::new(false));
            let sc = saw_cmp.clone();
            let si = saw_inv.clone();
            let out_refuse_mem = admission_test::with_hook(
                move |pt| match pt {
                    admission_test::Point::CompareAdmit { .. } => {
                        sc.store(true, Ordering::SeqCst);
                    }
                    admission_test::Point::InvalidateInitialStart => {
                        si.store(true, Ordering::SeqCst);
                    }
                    _ => {}
                },
                || {
                    k_refuse_mem
                        .classify(&Limits {
                            max_memory: Some(held - 1),
                            ..eager.clone()
                        })
                        .unwrap()
                },
            );
            assert!(
                saw_cmp.load(Ordering::SeqCst),
                "{label}: CompareAdmit was not reached under max_memory=held-1"
            );
            assert!(
                !saw_inv.load(Ordering::SeqCst),
                "{label}: InvalidateInitialStart ran despite refusal at CompareAdmit"
            );
            assert!(
                !out_refuse_mem.stats.incremental,
                "{label}: max_memory=held-1 must refuse incremental comparison"
            );
            assert_eq!(out_refuse_mem.stats.tier, Tier::Disjunctive);
            assert_eq!(out_refuse_mem.taxonomy, want.taxonomy);

            // Refusal at `held - 1` under pool `charge` stops at `CompareAdmit` and falls
            // back cleanly to a fresh Tier D run.
            let mut k_refuse_pool = make_edited();
            let saw_cmp = Arc::new(AtomicBool::new(false));
            let saw_inv = Arc::new(AtomicBool::new(false));
            let charged = Arc::new(AtomicUsize::new(0));
            let refused = Arc::new(AtomicUsize::new(0));
            let sc = saw_cmp.clone();
            let si = saw_inv.clone();
            let ch = charged.clone();
            let rf = refused.clone();
            let out_refuse_pool = admission_test::with_hook(
                move |pt| match pt {
                    admission_test::Point::CompareAdmit { .. } => {
                        sc.store(true, Ordering::SeqCst);
                    }
                    admission_test::Point::InvalidateInitialStart => {
                        si.store(true, Ordering::SeqCst);
                    }
                    _ => {}
                },
                || {
                    k_refuse_pool
                        .classify(&Limits {
                            charge: Some(Arc::new(move |bytes| {
                                let prev = ch.fetch_add(bytes, Ordering::SeqCst);
                                if prev + bytes > held - 1 {
                                    ch.fetch_sub(bytes, Ordering::SeqCst);
                                    rf.fetch_add(1, Ordering::SeqCst);
                                    false
                                } else {
                                    true
                                }
                            })),
                            ..eager.clone()
                        })
                        .unwrap()
                },
            );
            assert!(saw_cmp.load(Ordering::SeqCst));
            assert!(!saw_inv.load(Ordering::SeqCst));
            assert!(
                !out_refuse_pool.stats.incremental,
                "{label}: pool cap=held-1 must refuse incremental comparison"
            );
            assert!(refused.load(Ordering::SeqCst) > 0);
            assert_eq!(out_refuse_pool.stats.tier, Tier::Disjunctive);
            assert_eq!(out_refuse_pool.taxonomy, want.taxonomy);

            // Admission at exact `held` under `max_memory` succeeds incrementally.
            let mut k_admit_mem = make_edited();
            let out_admit_mem = k_admit_mem
                .classify(&Limits {
                    max_memory: Some(held),
                    ..eager.clone()
                })
                .unwrap();
            assert!(
                out_admit_mem.stats.incremental,
                "{label}: max_memory=held must admit incremental comparison"
            );
            assert_eq!(out_admit_mem.stats.tier, Tier::Disjunctive);
            assert_eq!(out_admit_mem.taxonomy, want.taxonomy);

            // Admission at exact `held` under pool `charge` succeeds incrementally.
            let mut k_admit_pool = make_edited();
            let charged_ok = Arc::new(AtomicUsize::new(0));
            let ch_ok = charged_ok.clone();
            let out_admit_pool = k_admit_pool
                .classify(&Limits {
                    charge: Some(Arc::new(move |bytes| {
                        let prev = ch_ok.fetch_add(bytes, Ordering::SeqCst);
                        if prev + bytes > held {
                            ch_ok.fetch_sub(bytes, Ordering::SeqCst);
                            false
                        } else {
                            true
                        }
                    })),
                    ..eager.clone()
                })
                .unwrap();
            assert!(
                out_admit_pool.stats.incremental,
                "{label}: pool cap=held must admit incremental comparison"
            );
            assert_eq!(out_admit_pool.stats.tier, Tier::Disjunctive);
            assert_eq!(out_admit_pool.taxonomy, want.taxonomy);
        }
    }

    /// Cooperative polling inside `invalidate_changed` for large dynamic cores and large
    /// disjointness membership rebuilds (Item 753 Phase 1): both a >4096-element core scan
    /// and a >4096-membership `reindex_disjoint_poll` inside the final pass cancel mid-walk
    /// and recover cleanly on the next classification.
    #[test]
    fn invalidate_changed_polls_large_core_and_disjoint_rebuild_and_recovers() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms
        };

        // Direct unit check on `core_changed_poll` and `tested_class_poll` with > 4096 core entries.
        let large_core: Vec<Ex> = (1..=4500u32).collect();
        let mut ticks = 0usize;
        let err = dl_rules::core_changed_poll(&large_core, &vec![false; 5000], &mut |w| {
            ticks += w;
            if ticks >= POLL_INVALIDATE {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(ticks, POLL_INVALIDATE);

        let mut test_core: Vec<Ex> = (1..=4500u32).map(|x| x | (1 << 31)).collect();
        test_core.push(7);
        let mut ticks = 0usize;
        let err = dl_rules::tested_class_poll(&test_core, &mut |w| {
            ticks += w;
            if ticks >= POLL_INVALIDATE {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(ticks, POLL_INVALIDATE);

        // End-to-end large dynamic test core (> 4096 grouped subsumption candidates for `:A`)
        // cancels inside `core_changed_poll` itself and recovers cleanly.
        let mut core_body = String::from(
            "SubClassOf(:A ObjectUnionOf(:P :Q))\n\
             SubClassOf(:P :MidP)\n\
             SubClassOf(:Q :MidQ)\n",
        );
        for i in 0..4200usize {
            core_body.push_str(&format!(
                "SubClassOf(:MidP :CP{i})\nSubClassOf(:MidQ :CQ{i})\n"
            ));
        }
        let core_base = parse(&core_body);
        let core_edit = parse("SubClassOf(:Unrelated :P)");
        let mut kc = Classifier::of(&core_base);
        let base_out = kc.classify(&eager).unwrap();
        assert_eq!(base_out.stats.tier, Tier::Disjunctive);
        let dyn_ref = kc.sat.dynamic.as_ref().expect("dynamic table");
        let max_core = (0..dyn_ref.len() as Ex)
            .map(|i| dyn_ref.core(dyn_ref.base() + i).len())
            .max()
            .unwrap_or(0);
        assert!(
            max_core > POLL_INVALIDATE,
            "expected dynamic core len > {POLL_INVALIDATE}, got {max_core}"
        );
        kc.add(&core_edit);
        let in_core = Arc::new(AtomicBool::new(false));
        let saw_core_start = Arc::new(AtomicBool::new(false));
        let saw_core_end = Arc::new(AtomicBool::new(false));
        let core_polls = Arc::new(AtomicUsize::new(0));
        let ic_hook = in_core.clone();
        let ic_poll = in_core.clone();
        let scs = saw_core_start.clone();
        let sce = saw_core_end.clone();
        let cp = core_polls.clone();
        let err = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::InvalidateLargeCoreStart => {
                    scs.store(true, Ordering::SeqCst);
                    ic_hook.store(true, Ordering::SeqCst);
                }
                admission_test::Point::InvalidateLargeCoreEnd => {
                    ic_hook.store(false, Ordering::SeqCst);
                    sce.store(true, Ordering::SeqCst);
                }
                _ => {}
            },
            || {
                kc.classify(&Limits {
                    cancelled: Some(Arc::new(move || {
                        if ic_poll.load(Ordering::SeqCst) {
                            cp.fetch_add(1, Ordering::SeqCst);
                            true
                        } else {
                            false
                        }
                    })),
                    ..eager.clone()
                })
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(
            saw_core_start.load(Ordering::SeqCst),
            "InvalidateLargeCoreStart was not reached"
        );
        assert!(
            core_polls.load(Ordering::SeqCst) > 0,
            "no cooperative poll ran inside core_changed_poll over >4096-entry dynamic core"
        );
        assert!(
            !saw_core_end.load(Ordering::SeqCst),
            "core_changed_poll completed without stopping on cancellation"
        );
        let mut core_held = core_base.clone();
        core_held.extend(core_edit);
        let core_want = Classifier::of(&core_held).classify(&eager).unwrap();
        let recovered = kc.classify(&eager).unwrap();
        assert!(!recovered.stats.incremental);
        assert_eq!(recovered.taxonomy, core_want.taxonomy);

        // End-to-end kept context whose subsumer `:M` belongs to 4500 unchanged disjoint sets:
        // an unrelated `DisjointClasses(:Z0 :Z1)` renumbers sets and triggers
        // `reindex_disjoint_poll`, which cancels strictly inside `:M`'s 4500-membership loop
        // (`DisjointMembersStart..DisjointMembersEnd`), not in short `:D{i}` contexts or
        // subsumer loops, and recovers cleanly.
        let mut disj_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\n\
             SubClassOf(:P :R)\nSubClassOf(:Q :R)\n\
             SubClassOf(:Keep :M)\n",
        );
        for i in 0..4500usize {
            disj_body.push_str(&format!("DisjointClasses(:M :D{i})\n"));
        }
        let disj_base = parse(&disj_body);
        let disj_edit = parse("DisjointClasses(:Z0 :Z1)");
        let mut kd = Classifier::of(&disj_base);
        let base_out = kd.classify(&eager).unwrap();
        assert_eq!(base_out.stats.tier, Tier::Disjunctive);

        // Direct unit check on `State::reindex_disjoint_poll`: a single-subsumer state whose
        // subsumer `:M` belongs to 4500 disjoint sets ticks only 2 times across the two outer
        // subsumer loops, so crossing `POLL_INVALIDATE` must happen inside the membership loop.
        {
            let m_id = kd
                .sat
                .interner
                .get(&Node::Class(
                    kd.sat
                        .interner
                        .classes
                        .iter()
                        .position(|c| c.as_str() == "http://ex.org/M")
                        .unwrap() as u32,
                ))
                .unwrap();
            let ix_ref = kd.sat.index.as_ref().expect("classified index");
            assert!(ix_ref.disjoint(m_id).len() >= 4500);
            let mut st = saturate::State {
                init: true,
                ..Default::default()
            };
            st.subs.insert(m_id);
            let mut ticks = 0usize;
            let err = st
                .reindex_disjoint_poll(ix_ref, &mut |w| {
                    ticks += w;
                    if ticks >= POLL_INVALIDATE {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert!(matches!(err, Error::Cancelled));
            assert_eq!(ticks, POLL_INVALIDATE);
            assert!(
                st.disj.len() < 4500,
                "expected membership loop to stop before inserting all 4500 disjoint entries"
            );
        }

        kd.add(&disj_edit);
        let in_members = Arc::new(AtomicBool::new(false));
        let saw_members_start = Arc::new(AtomicBool::new(false));
        let saw_members_end = Arc::new(AtomicBool::new(false));
        let member_polls = Arc::new(AtomicUsize::new(0));
        let im_hook = in_members.clone();
        let im_poll = in_members.clone();
        let sms = saw_members_start.clone();
        let sme = saw_members_end.clone();
        let mp = member_polls.clone();
        let err = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::DisjointMembersStart => {
                    sms.store(true, Ordering::SeqCst);
                    im_hook.store(true, Ordering::SeqCst);
                }
                admission_test::Point::DisjointMembersEnd => {
                    im_hook.store(false, Ordering::SeqCst);
                    sme.store(true, Ordering::SeqCst);
                }
                _ => {}
            },
            || {
                kd.classify(&Limits {
                    cancelled: Some(Arc::new(move || {
                        if im_poll.load(Ordering::SeqCst) {
                            mp.fetch_add(1, Ordering::SeqCst);
                            true
                        } else {
                            false
                        }
                    })),
                    ..eager.clone()
                })
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(
            saw_members_start.load(Ordering::SeqCst),
            "DisjointMembersStart was not reached on :M's 4500-membership list"
        );
        assert!(
            member_polls.load(Ordering::SeqCst) > 0,
            "no cooperative poll ran inside the 4500-entry disjoint membership loop"
        );
        assert!(
            !saw_members_end.load(Ordering::SeqCst),
            "disjoint membership loop completed without stopping on cancellation"
        );
        let mut disj_held = disj_base.clone();
        disj_held.extend(disj_edit);
        let disj_want = Classifier::of(&disj_held).classify(&eager).unwrap();
        let recovered = kd.classify(&eager).unwrap();
        assert!(!recovered.stats.incremental);
        assert_eq!(recovered.taxonomy, disj_want.taxonomy);
    }

    /// Cooperative polling during `Dynamic::rebase` (Item 753 Phase 1): saturating > 4096
    /// dynamic conjunction contexts under `Expansion::Eager` and adding a fresh name polls
    /// inside `Dynamic::rebase`'s shard walk, cancels before `RebaseDynamicEnd`, and recovers
    /// cleanly.
    #[test]
    fn rebase_polls_inside_dynamic_walk_and_recovers() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let mut body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..4200usize {
            body.push_str(&format!(
                "SubClassOf(:X{i} ObjectSomeValuesFrom(:r :A{i}))\n\
                 SubClassOf(:X{i} ObjectAllValuesFrom(:r :B))\n"
            ));
        }
        let base =
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms;
        let edit = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)\nOntology(\nSubClassOf(:FreshRebase :P)\n)",
        )
        .unwrap()
        .axioms;

        let mut k = Classifier::of(&base);
        let first = k.classify(&eager).unwrap();
        assert_eq!(first.stats.tier, Tier::Disjunctive);
        let dyn_len = k.sat.dynamic.as_ref().expect("dynamic table").len();
        assert!(
            dyn_len > POLL_INVALIDATE,
            "fixture must create > {POLL_INVALIDATE} dynamic entries, got {dyn_len}"
        );
        k.add(&edit);
        let in_dynamic = Arc::new(AtomicBool::new(false));
        let saw_start = Arc::new(AtomicBool::new(false));
        let saw_end = Arc::new(AtomicBool::new(false));
        let dynamic_polls = Arc::new(AtomicUsize::new(0));
        let id_hook = in_dynamic.clone();
        let id_cancel = in_dynamic.clone();
        let ss = saw_start.clone();
        let se = saw_end.clone();
        let dp = dynamic_polls.clone();
        let err = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::RebaseDynamicStart => {
                    ss.store(true, Ordering::SeqCst);
                    id_hook.store(true, Ordering::SeqCst);
                }
                admission_test::Point::RebaseDynamicEnd => {
                    id_hook.store(false, Ordering::SeqCst);
                    se.store(true, Ordering::SeqCst);
                }
                _ => {}
            },
            || {
                k.classify(&Limits {
                    cancelled: Some(Arc::new(move || {
                        if id_cancel.load(Ordering::SeqCst) {
                            dp.fetch_add(1, Ordering::SeqCst);
                            true
                        } else {
                            false
                        }
                    })),
                    ..eager.clone()
                })
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(
            saw_start.load(Ordering::SeqCst),
            "RebaseDynamicStart was not reached"
        );
        assert!(
            dynamic_polls.load(Ordering::SeqCst) > 0,
            "expected cooperative poll inside Dynamic::rebase over {dyn_len} entries"
        );
        assert!(
            !saw_end.load(Ordering::SeqCst),
            "Dynamic::rebase finished without stopping on cancellation"
        );
        let mut held = base.clone();
        held.extend(edit);
        let want = Classifier::of(&held).classify(&eager).unwrap();
        let recovered = k.classify(&eager).unwrap();
        assert!(!recovered.stats.incremental);
        assert_eq!(recovered.taxonomy, want.taxonomy);
    }

    #[test]
    fn tier_d_comparison_scratch_polls_before_admission() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms
        };
        let mut body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\n\
             SubClassOf(:Q :R)\nDisjointClasses(:Left :Right)\n",
        );
        for i in 0..4200 {
            body.push_str(&format!("SubClassOf(:A{i} :B{i})\n"));
        }
        let base = parse(&body);
        let edit = parse("SubClassOf(:P :Q)");

        for expire in [false, true] {
            let mut classifier = Classifier::of(&base);
            assert_eq!(
                classifier.classify(&eager).unwrap().stats.tier,
                Tier::Disjunctive
            );
            classifier.add(&edit);
            let stop = Arc::new(AtomicBool::new(false));
            let deadline = Arc::new(admission_test::TestDeadline::future());
            let stop_in_scan = Arc::clone(&stop);
            let deadline_in_scan = Arc::clone(&deadline);
            let result = admission_test::with_hook(
                move |point| match point {
                    admission_test::Point::CompareScratchHeadChunk(1) => {
                        stop_in_scan.store(true, Ordering::SeqCst);
                        if expire {
                            deadline_in_scan.expire();
                        }
                    }
                    admission_test::Point::CompareScratchHeadChunk(2) => {
                        panic!("scratch accounting entered another head chunk after the stop");
                    }
                    admission_test::Point::CompareAdmit { .. } => {
                        panic!("comparison reached admission after the stop");
                    }
                    _ => {}
                },
                || {
                    let mut limits = eager.clone();
                    if expire {
                        limits.deadline = Some(deadline.instant());
                    } else {
                        let stop = Arc::clone(&stop);
                        limits.cancelled = Some(Arc::new(move || stop.load(Ordering::SeqCst)));
                    }
                    classifier.classify(&limits)
                },
            );
            assert!(
                stop.load(Ordering::SeqCst),
                "scratch head scan was not reached"
            );
            let err = result.unwrap_err();
            if expire {
                assert!(matches!(err, Error::Timeout), "{err:?}");
            } else {
                assert!(matches!(err, Error::Cancelled), "{err:?}");
            }
        }
    }

    /// Renumbered `DisjointClasses` pre-admission under `Expansion::Eager` (Item 753 Phase 1):
    /// covers both incremental `reindex_disjoint` parity and an isolated `disjoint_growth`
    /// refusal whose cap lies strictly between `paid` (after two-index comparison) and
    /// `live_without_old + growth`, proving refusal occurs at `DisjointGrowthAdmit` before
    /// `DisjointReindex` and recovers via fresh retry.
    #[test]
    fn eager_renumbered_disjoint_growth_preadmission_and_parity() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let mut body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\n\
             SubClassOf(:P :R)\n\
             SubClassOf(:Q :R)\n",
        );
        for i in 0..64usize {
            body.push_str(&format!(
                "SubClassOf(:Hub :A{i})\nDisjointClasses(:A{i} :B{i})\n"
            ));
        }
        for j in 0..128usize {
            body.push_str(&format!("SubClassOf(:Base{j} :Hub)\n"));
        }
        let mut drop_body = String::new();
        for g in 0..256usize {
            drop_body.push_str(&format!("SubClassOf(:Gone{g} :Hub)\n"));
        }
        let base_ax =
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms;
        let drop_ax = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\n{drop_body}\n)"
        ))
        .unwrap()
        .axioms;
        let edit_ax = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)\nOntology(\nDisjointClasses(:Z0 :Z1)\n)",
        )
        .unwrap()
        .axioms;

        let mut initial_ax = base_ax.clone();
        initial_ax.extend(drop_ax.clone());

        let mut held = base_ax.clone();
        held.extend(edit_ax.clone());
        let want = Classifier::of(&held).classify(&eager).unwrap();

        // 1. Uncapped kept run reindexes disjointness sets incrementally with exact parity.
        let mut k = Classifier::of(&initial_ax);
        let first = k.classify(&eager).unwrap();
        assert_eq!(first.stats.tier, Tier::Disjunctive);
        k.remove(&drop_ax);
        k.add(&edit_ax);
        let out = k.classify(&eager).unwrap();
        assert!(
            out.stats.incremental,
            "renumbered disjoint edit started fresh"
        );
        assert_eq!(out.taxonomy, want.taxonomy);
        assert_eq!(out.consistency, want.consistency);
        assert!(k.excluded().is_empty());

        // Helper that prepares a classified kept state whose 128 `:Base{j}` contexts have
        // single-entry `st.disj` capacity (< 64 memberships), while the 256 removed `:Gone{g}`
        // contexts remain in the kept state until invalidation clears them. That ensures both
        // `live_without_old + growth > paid` and `want.stats.peak_memory < paid`, so a cap
        // between `paid` and `live_without_old + growth` refuses specifically at
        // `DisjointGrowthAdmit` and succeeds on the fresh retry.
        let make_shrunk = || {
            let mut kr = Classifier::of(&initial_ax);
            kr.classify(&eager).unwrap();
            for j in 0..128usize {
                let iri = format!("http://ex.org/Base{j}");
                let id = kr
                    .sat
                    .interner
                    .get(&Node::Class(
                        kr.sat
                            .interner
                            .classes
                            .iter()
                            .position(|c| c.as_str() == iri)
                            .unwrap() as u32,
                    ))
                    .unwrap();
                let ctx = kr.sat.ctx[id as usize].get().unwrap();
                let mut st = ctx.st.lock();
                let (&first_entry, &first_val) = st.disj.iter().next().unwrap();
                st.disj = FxHashMap::default();
                st.disj.insert(first_entry, first_val);
                assert!(st.disj.capacity() < 64);
            }
            kr.remove(&drop_ax);
            kr.add(&edit_ax);
            kr
        };

        // 2. Observe `(live, growth, paid)` at `DisjointGrowthAdmit` on an uncapped run and
        // choose `cap` strictly between `paid` and `live + growth`.
        let observed = Arc::new(parking_lot::Mutex::new(None::<(usize, usize, usize)>));
        let obs_hook = observed.clone();
        let mut k_probe = make_shrunk();
        let probe_out = admission_test::with_hook(
            move |pt| {
                if let admission_test::Point::DisjointGrowthAdmit { live, growth, paid } = pt {
                    *obs_hook.lock() = Some((live, growth, paid));
                }
            },
            || k_probe.classify(&eager).unwrap(),
        );
        assert!(probe_out.stats.incremental);
        let (live, growth, paid) = observed
            .lock()
            .expect("DisjointGrowthAdmit was not reached on probe run");
        assert!(growth > 0, "expected disjoint_growth > 0");
        assert!(
            live + growth > paid,
            "live_without_old + growth ({live} + {growth}) must exceed two-index peak ({paid})"
        );
        let cap = paid + (live + growth - paid) / 2;
        assert!(cap >= paid && cap < live + growth);

        // 3. `max_memory: Some(cap)` passes normalization, rebase, and two-index comparison
        // (`paid <= cap`), refuses at `DisjointGrowthAdmit`, skips `DisjointReindex`, and
        // retries fresh under the same cap with full parity.
        let mut k_max = make_shrunk();
        let saw_admit = Arc::new(AtomicBool::new(false));
        let saw_reindex = Arc::new(AtomicBool::new(false));
        let sa = saw_admit.clone();
        let sr = saw_reindex.clone();
        let out_max = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::DisjointGrowthAdmit { .. } => {
                    sa.store(true, Ordering::SeqCst);
                }
                admission_test::Point::DisjointReindex => {
                    sr.store(true, Ordering::SeqCst);
                }
                _ => {}
            },
            || {
                k_max
                    .classify(&Limits {
                        max_memory: Some(cap),
                        ..eager.clone()
                    })
                    .unwrap()
            },
        );
        assert!(
            saw_admit.load(Ordering::SeqCst),
            "DisjointGrowthAdmit was not reached under max_memory={cap}"
        );
        assert!(
            !saw_reindex.load(Ordering::SeqCst),
            "DisjointReindex ran despite refusal at DisjointGrowthAdmit"
        );
        assert!(!out_max.stats.incremental);
        assert_eq!(out_max.taxonomy, want.taxonomy);
        assert_eq!(out_max.consistency, want.consistency);
        assert!(k_max.excluded().is_empty());

        // 4. Pool `charge` capped at `cap` likewise reaches `DisjointGrowthAdmit`, refuses
        // the growth delta, skips `DisjointReindex`, and retries fresh with full parity.
        let mut k_charge = make_shrunk();
        let saw_admit = Arc::new(AtomicBool::new(false));
        let saw_reindex = Arc::new(AtomicBool::new(false));
        let charged = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicUsize::new(0));
        let sa = saw_admit.clone();
        let sr = saw_reindex.clone();
        let ch = charged.clone();
        let rf = refused.clone();
        let out_charge = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::DisjointGrowthAdmit { .. } => {
                    sa.store(true, Ordering::SeqCst);
                }
                admission_test::Point::DisjointReindex => {
                    sr.store(true, Ordering::SeqCst);
                }
                _ => {}
            },
            || {
                k_charge
                    .classify(&Limits {
                        charge: Some(Arc::new(move |bytes| {
                            let prev = ch.fetch_add(bytes, Ordering::SeqCst);
                            if prev + bytes > cap {
                                ch.fetch_sub(bytes, Ordering::SeqCst);
                                rf.fetch_add(1, Ordering::SeqCst);
                                false
                            } else {
                                true
                            }
                        })),
                        ..eager.clone()
                    })
                    .unwrap()
            },
        );
        assert!(saw_admit.load(Ordering::SeqCst));
        assert!(refused.load(Ordering::SeqCst) > 0);
        assert!(!saw_reindex.load(Ordering::SeqCst));
        assert!(!out_charge.stats.incremental);
        assert!(charged.load(Ordering::SeqCst) <= cap);
        assert_eq!(out_charge.taxonomy, want.taxonomy);
        assert_eq!(out_charge.consistency, want.consistency);
        assert!(k_charge.excluded().is_empty());
    }

    /// Post-counting fallback under `Expansion::Eager` (`dl_counting_over == true`, where
    /// the fallback runs Tier D with `self.no_counting = true`) must not retain `self.kept`
    /// state (verified directly on `k.sat.kept` so a `RuleKey` mismatch cannot mask a bug).
    #[test]
    fn eager_post_counting_fallback_does_not_retain_kept_state() {
        let doc = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)
             Ontology(
                 SubClassOf(:A ObjectUnionOf(:B :C))
                 SubClassOf(:B :D)
                 SubClassOf(:C :D)
                 SubClassOf(:A ObjectMaxCardinality(2 :r :B))
             )",
        )
        .unwrap();
        let lim = Limits {
            threads: 1,
            counting: true,
            dl_work: Some(1),
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let mut k = Classifier::of(&doc.axioms);
        let out = k.classify(&lim).unwrap();
        assert!(
            out.stats.dl_counting_over,
            "expected post-counting fallback under dl_work=1"
        );
        assert!(out.stats.dl_over_budget);
        assert_eq!(
            out.stats.tier,
            Tier::Disjunctive,
            "expected post-counting fallback to complete in Tier D, not fall back to Horn"
        );
        assert!(
            k.sat.kept.is_none(),
            "post-counting Tier D fallback unexpectedly retained kept contexts"
        );
    }

    /// Kept-run width-stop observation under `Expansion::Eager` (Item 753 Phase 1): using pure
    /// class axioms whose 2x2 product triggers the width limit at 3 (and stays incremental at 4),
    /// directly verify that `DlWidthStop` fires during the kept run *before* `KeptRetry` with
    /// `dl_worked == true`, followed by the fresh retry's `DlWidthStop` and exact parity.
    #[test]
    fn eager_kept_width_stop_fires_before_fresh_retry() {
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!(
                "Prefix(:=<http://ex.org/>)\n\
                 Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
                 Ontology(\n{body}\n)"
            ))
            .unwrap()
            .axioms
        };
        let base = parse(
            "SubClassOf(:A ObjectUnionOf(:B :C))
             SubClassOf(:B :D)
             SubClassOf(:C :D)",
        );
        let wide = parse(
            "SubClassOf(:W ObjectSomeValuesFrom(:r :S))
             SubClassOf(:W ObjectUnionOf(:Q1 ObjectAllValuesFrom(:r :P1)))
             SubClassOf(:W ObjectUnionOf(:Q2 ObjectAllValuesFrom(:r :P1)))
             SubClassOf(:W ObjectUnionOf(:R1 ObjectAllValuesFrom(:r :P2)))
             SubClassOf(:W ObjectUnionOf(:R2 ObjectAllValuesFrom(:r :P2)))
             SubClassOf(ObjectIntersectionOf(:P1 :P2) owl:Nothing)",
        );
        let eager = |width: usize| Limits {
            threads: 1,
            expansion: Expansion::Eager,
            dl_attempt_width: Some(width),
            ..Limits::default()
        };

        // At width limit 4, adding `wide` stays incremental with no width stop or kept retry.
        let events_4 = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let ev4 = events_4.clone();
        let mut k4 = Classifier::of(&base);
        k4.classify(&eager(4)).unwrap();
        k4.add(&wide);
        let out4 = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::DlWidthStop | admission_test::Point::KeptRetry { .. } => {
                    ev4.lock().push(pt);
                }
                _ => {}
            },
            || k4.classify(&eager(4)).unwrap(),
        );
        assert!(out4.stats.incremental);
        assert!(!out4.stats.dl_attempt_over && !out4.stats.dl_over_budget);
        assert!(events_4.lock().is_empty(), "{:?}", events_4.lock());

        // At width limit 3, the kept run itself hits `DlWidthStop`, triggers
        // `KeptRetry { dl_worked: true }`, and then the fresh retry hits `DlWidthStop` again.
        let events_3 = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let ev3 = events_3.clone();
        let mut k3 = Classifier::of(&base);
        k3.classify(&eager(3)).unwrap();
        assert!(k3.sat.kept.is_some());
        k3.add(&wide);
        let out3 = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::DlWidthStop | admission_test::Point::KeptRetry { .. } => {
                    ev3.lock().push(pt);
                }
                _ => {}
            },
            || k3.classify(&eager(3)).unwrap(),
        );
        let seq = events_3.lock().clone();
        let retry_pos = seq
            .iter()
            .position(|pt| matches!(pt, admission_test::Point::KeptRetry { dl_worked: true }))
            .unwrap_or_else(|| panic!("expected KeptRetry {{ dl_worked: true }}, got {seq:?}"));
        assert!(
            seq[..retry_pos]
                .iter()
                .any(|pt| matches!(pt, admission_test::Point::DlWidthStop)),
            "kept run did not hit DlWidthStop before KeptRetry: {seq:?}"
        );
        assert!(
            seq[retry_pos + 1..]
                .iter()
                .any(|pt| matches!(pt, admission_test::Point::DlWidthStop)),
            "fresh retry did not hit DlWidthStop after KeptRetry: {seq:?}"
        );
        assert!(!out3.stats.incremental);
        assert!(out3.stats.dl_attempt_over && out3.stats.dl_over_budget);

        let mut held = base.clone();
        held.extend(wide.clone());
        let want = Classifier::of(&held).classify(&eager(3)).unwrap();
        assert_eq!(out3.taxonomy, want.taxonomy);
        assert_eq!(out3.consistency, want.consistency);
        assert_eq!(out3.complete, want.complete);

        // Removing the wide axioms recovers cleanly without width fallback.
        k3.remove(&wide);
        let rec = k3.classify(&eager(3)).unwrap();
        assert!(!rec.stats.dl_attempt_over && !rec.stats.dl_over_budget);
        assert!(rec.taxonomy.is_subclass(
            &Class::new("http://ex.org/A"),
            &Class::new("http://ex.org/D")
        ));
    }

    /// Polled rebuilt index and normalized-axiom accounting (Item 753 Phase 1):
    /// 1. Direct unit checks prove `Saturation::index_bytes_poll` and `Axioms::bytes_poll`
    ///    match `index_bytes`/`Axioms::bytes` exactly and stop at `POLL_INVALIDATE` on >4096
    ///    Tier D clauses.
    /// 2. End-to-end checks isolate cancellation strictly inside rebuilt index accounting
    ///    (`RebaseAccountStart..RebaseAccountAxiomsStart`) and strictly inside normalized-axiom
    ///    accounting (`RebaseAccountAxiomsStart..RebaseAccountEnd`), both before
    ///    `RebaseScratchStart`/`RebaseAdmit`/`RebaseShift`, with clean fresh recovery.
    #[test]
    fn rebase_pre_account_polls_and_matches_bytes() {
        let eager = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let mut body = String::new();
        for i in 0..4200usize {
            body.push_str(&format!("SubClassOf(:C{i} ObjectUnionOf(:P{i} :Q{i}))\n"));
        }
        let base =
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms;
        let edit = crate::ofn_read::parse(
            "Prefix(:=<http://ex.org/>)\nOntology(\nSubClassOf(:FreshAccount :P0)\n)",
        )
        .unwrap()
        .axioms;

        // Direct unit check on both `Saturation::index_bytes_poll` and `Axioms::bytes_poll`.
        let axiom_kinds: Vec<&AxiomKind> = base.iter().map(|a| &a.kind).collect();
        let outside: Vec<(usize, (String, String))> = axiom_kinds
            .iter()
            .enumerate()
            .filter_map(|(i, k)| {
                let b = el_break(k)?;
                Some((i, (b.rule.to_string(), b.message)))
            })
            .collect();
        let mut probe_sat = Saturation::default();
        let (ax_probe, ix_probe, _, _, _) = probe_sat
            .build(
                &axiom_kinds,
                &outside,
                1,
                0,
                0,
                &eager,
                AttemptProvenance::Ineligible,
            )
            .unwrap();
        assert!(
            ax_probe
                .dl
                .as_ref()
                .is_some_and(|d| d.clauses.len() >= 4200),
            "expected >= 4200 normalized DL clauses"
        );

        let exact_ix = probe_sat.index_bytes(&ix_probe);
        let mut ix_noop_ticks = 0usize;
        let polled_ix = probe_sat
            .index_bytes_poll(&ix_probe, &mut |w| {
                ix_noop_ticks += w;
                Ok::<(), Error>(())
            })
            .unwrap();
        assert_eq!(polled_ix, exact_ix);
        assert!(
            ix_noop_ticks > POLL_INVALIDATE,
            "expected > {POLL_INVALIDATE} index accounting ticks, got {ix_noop_ticks}"
        );
        let mut ix_stop_ticks = 0usize;
        let err = probe_sat
            .index_bytes_poll(&ix_probe, &mut |w| {
                ix_stop_ticks += w;
                if ix_stop_ticks >= POLL_INVALIDATE {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(ix_stop_ticks, POLL_INVALIDATE);

        let exact_ax = ax_probe.bytes();
        let mut ax_noop_ticks = 0usize;
        let polled_ax = ax_probe
            .bytes_poll(&mut |w| {
                ax_noop_ticks += w;
                Ok::<(), Error>(())
            })
            .unwrap();
        assert_eq!(polled_ax, exact_ax);
        assert!(
            ax_noop_ticks > POLL_INVALIDATE,
            "expected > {POLL_INVALIDATE} axiom accounting ticks, got {ax_noop_ticks}"
        );
        let mut ax_stop_ticks = 0usize;
        let err = ax_probe
            .bytes_poll(&mut |w| {
                ax_stop_ticks += w;
                if ax_stop_ticks >= POLL_INVALIDATE {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(ax_stop_ticks, POLL_INVALIDATE);

        let mut held = base.clone();
        held.extend(edit.clone());
        let want = Classifier::of(&held).classify(&eager).unwrap();

        let assert_account_window_cancels =
            |arm_at: admission_test::Point, stop_before: admission_test::Point, label: &str| {
                let mut k = Classifier::of(&base);
                let first = k.classify(&eager).unwrap();
                assert_eq!(first.stats.tier, Tier::Disjunctive);
                k.add(&edit);

                let armed = Arc::new(AtomicBool::new(false));
                let saw_arm = Arc::new(AtomicBool::new(false));
                let saw_stop_before = Arc::new(AtomicBool::new(false));
                let saw_end = Arc::new(AtomicBool::new(false));
                let saw_scratch = Arc::new(AtomicBool::new(false));
                let saw_admit = Arc::new(AtomicBool::new(false));
                let saw_shift = Arc::new(AtomicBool::new(false));
                let account_polls = Arc::new(AtomicUsize::new(0));
                let ar_hook = armed.clone();
                let ar_poll = armed.clone();
                let sa = saw_arm.clone();
                let sb = saw_stop_before.clone();
                let se = saw_end.clone();
                let sscr = saw_scratch.clone();
                let sadm = saw_admit.clone();
                let ssh = saw_shift.clone();
                let ap = account_polls.clone();
                let err = admission_test::with_hook(
                    move |pt| {
                        if pt == arm_at {
                            sa.store(true, Ordering::SeqCst);
                            ar_hook.store(true, Ordering::SeqCst);
                        } else if pt == stop_before {
                            ar_hook.store(false, Ordering::SeqCst);
                            sb.store(true, Ordering::SeqCst);
                        }
                        match pt {
                            admission_test::Point::RebaseAccountEnd => {
                                ar_hook.store(false, Ordering::SeqCst);
                                se.store(true, Ordering::SeqCst);
                            }
                            admission_test::Point::RebaseScratchStart => {
                                sscr.store(true, Ordering::SeqCst);
                            }
                            admission_test::Point::RebaseAdmit { .. } => {
                                sadm.store(true, Ordering::SeqCst);
                            }
                            admission_test::Point::RebaseShift => ssh.store(true, Ordering::SeqCst),
                            _ => {}
                        }
                    },
                    || {
                        k.classify(&Limits {
                            cancelled: Some(Arc::new(move || {
                                if ar_poll.load(Ordering::SeqCst) {
                                    ap.fetch_add(1, Ordering::SeqCst);
                                    true
                                } else {
                                    false
                                }
                            })),
                            ..eager.clone()
                        })
                    },
                )
                .unwrap_err();
                assert!(matches!(err, Error::Cancelled), "{label}: {err:?}");
                assert!(
                    saw_arm.load(Ordering::SeqCst),
                    "{label}: {arm_at:?} was not reached"
                );
                assert!(
                    account_polls.load(Ordering::SeqCst) > 0,
                    "{label}: no cooperative poll ran between {arm_at:?} and {stop_before:?}"
                );
                assert!(
                    !saw_stop_before.load(Ordering::SeqCst),
                    "{label}: reached {stop_before:?} without stopping on cancellation"
                );
                assert!(
                    !saw_end.load(Ordering::SeqCst),
                    "{label}: RebaseAccountEnd ran despite cancellation"
                );
                assert!(!saw_scratch.load(Ordering::SeqCst));
                assert!(!saw_admit.load(Ordering::SeqCst));
                assert!(!saw_shift.load(Ordering::SeqCst));

                let recovered = k.classify(&eager).unwrap();
                assert!(!recovered.stats.incremental);
                assert_eq!(recovered.taxonomy, want.taxonomy);
            };

        // End-to-end 1: cancellation inside rebuilt index accounting stops before axiom accounting.
        assert_account_window_cancels(
            admission_test::Point::RebaseAccountStart,
            admission_test::Point::RebaseAccountAxiomsStart,
            "rebuilt index accounting",
        );
        // End-to-end 2: cancellation armed strictly at normalized-axiom accounting stops before
        // `RebaseAccountEnd` (fails if `ax.bytes_poll` is replaced with `ax.bytes()`).
        assert_account_window_cancels(
            admission_test::Point::RebaseAccountAxiomsStart,
            admission_test::Point::RebaseAccountEnd,
            "normalized-axiom accounting",
        );
    }

    /// Phase-verified deadline timeouts under `Expansion::Eager` (Item 753 Phase 1): using a
    /// controlled future deadline (`TestDeadline::future()`, immune to runner scheduling delays),
    /// prove `Error::Timeout` and clean fresh recovery when the deadline expires strictly inside:
    /// 1. kept invalidation (`InvalidateClosureStart..InvalidateClosureEnd`), and
    /// 2. kept saturation (`KeptSaturationStart..KeptSaturationEnd`, after the first in-saturation
    ///    poll passes unexpired) on both 1 and 4 threads.
    #[test]
    fn eager_kept_phase_deadline_stops_and_recovers() {
        let parse = |body: &str| {
            crate::ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
                .unwrap()
                .axioms
        };

        // 1. Deadline crossing strictly inside kept invalidation closure.
        let mut inv_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..4500usize {
            inv_body.push_str(&format!(
                "SubClassOf(:C{i} ObjectSomeValuesFrom(:r :C{}))\n",
                i + 1
            ));
        }
        let inv_base = parse(&inv_body);
        let inv_edit = parse("SubClassOf(:C4500 :Target)");
        let eager_1t = Limits {
            threads: 1,
            expansion: Expansion::Eager,
            ..Limits::default()
        };
        let mut k_inv = Classifier::of(&inv_base);
        k_inv.classify(&eager_1t).unwrap();
        k_inv.add(&inv_edit);

        let clock = Arc::new(admission_test::TestDeadline::future());
        let deadline = clock.instant();
        let clk_hook = clock.clone();
        let saw_start = Arc::new(AtomicBool::new(false));
        let saw_end = Arc::new(AtomicBool::new(false));
        let ss = saw_start.clone();
        let se = saw_end.clone();
        let err = admission_test::with_hook(
            move |pt| match pt {
                admission_test::Point::InvalidateClosureStart => {
                    ss.store(true, Ordering::SeqCst);
                    clk_hook.expire();
                }
                admission_test::Point::InvalidateClosureEnd => {
                    se.store(true, Ordering::SeqCst);
                }
                _ => {}
            },
            || {
                k_inv.classify(&Limits {
                    deadline: Some(deadline),
                    ..eager_1t.clone()
                })
            },
        )
        .unwrap_err();
        drop(clock);
        assert!(
            matches!(err, Error::Timeout),
            "expected Timeout, got {err:?}"
        );
        assert!(saw_start.load(Ordering::SeqCst));
        assert!(
            !saw_end.load(Ordering::SeqCst),
            "invalidation closure completed despite deadline expiring at entry"
        );
        let mut inv_held = inv_base.clone();
        inv_held.extend(inv_edit);
        let inv_want = Classifier::of(&inv_held).classify(&eager_1t).unwrap();
        let inv_rec = k_inv.classify(&eager_1t).unwrap();
        assert!(!inv_rec.stats.incremental);
        assert_eq!(inv_rec.taxonomy, inv_want.taxonomy);

        // 2. Deadline crossing strictly inside kept saturation on 1 and 4 threads.
        // A 350-class transitive chain invalidated at its top triggers >60,000 kept-saturation
        // conclusions so worker threads hit `CHECK_EVERY = 4096` multiple times on 1 and 4 threads.
        let mut sat_body = String::from(
            "SubClassOf(:U ObjectUnionOf(:P :Q))\nSubClassOf(:P :R)\nSubClassOf(:Q :R)\n",
        );
        for i in 0..350usize {
            sat_body.push_str(&format!("SubClassOf(:T{i} :T{})\n", i + 1));
        }
        let sat_base = parse(&sat_body);
        let sat_edit = parse("SubClassOf(:T350 :Done)");
        let mut sat_held = sat_base.clone();
        sat_held.extend(sat_edit.clone());

        for threads in [1usize, 4] {
            let lim = Limits {
                threads,
                expansion: Expansion::Eager,
                ..Limits::default()
            };
            let want = Classifier::of(&sat_held).classify(&lim).unwrap();
            let mut k = Classifier::of(&sat_base);
            let first = k.classify(&lim).unwrap();
            assert_eq!(first.stats.tier, Tier::Disjunctive);
            k.add(&sat_edit);

            let clock = Arc::new(admission_test::TestDeadline::future());
            let deadline = clock.instant();
            let in_sat = Arc::new(AtomicBool::new(false));
            let saw_sat_start = Arc::new(AtomicBool::new(false));
            let saw_sat_timeout = Arc::new(AtomicBool::new(false));
            let sat_polls = Arc::new(AtomicUsize::new(0));
            let is_hook = in_sat.clone();
            let is_poll = in_sat.clone();
            let sss = saw_sat_start.clone();
            let sst = saw_sat_timeout.clone();
            let sp = sat_polls.clone();
            let clk_poll = clock.clone();
            let err = admission_test::with_hook(
                move |pt| match pt {
                    admission_test::Point::KeptSaturationStart => {
                        sss.store(true, Ordering::SeqCst);
                        is_hook.store(true, Ordering::SeqCst);
                    }
                    admission_test::Point::KeptSaturationEnd { stopped_by_timeout } => {
                        is_hook.store(false, Ordering::SeqCst);
                        sst.store(stopped_by_timeout, Ordering::SeqCst);
                    }
                    _ => {}
                },
                || {
                    k.classify(&Limits {
                        deadline: Some(deadline),
                        cancelled: Some(Arc::new(move || {
                            if is_poll.load(Ordering::SeqCst) {
                                sp.fetch_add(1, Ordering::SeqCst);
                                clk_poll.expire();
                            }
                            false
                        })),
                        ..lim.clone()
                    })
                },
            )
            .unwrap_err();
            drop(clock);
            assert!(
                matches!(err, Error::Timeout),
                "expected Timeout during kept saturation ({threads}t), got {err:?}"
            );
            assert!(
                saw_sat_start.load(Ordering::SeqCst),
                "KeptSaturationStart was not reached ({threads}t)"
            );
            assert!(
                sat_polls.load(Ordering::SeqCst) > 0,
                "expected at least one in-saturation poll before expiring deadline ({threads}t)"
            );
            assert!(
                saw_sat_timeout.load(Ordering::SeqCst),
                "saturate_counted did not stop with Stop::Timeout ({threads}t)"
            );
            let recovered = k.classify(&lim).unwrap();
            assert!(!recovered.stats.incremental);
            assert_eq!(recovered.taxonomy, want.taxonomy);
            assert_eq!(recovered.consistency, want.consistency);
        }
    }
}

#[cfg(test)]
#[path = "a1_probe_alloc.rs"]
mod a1_probe_alloc;
