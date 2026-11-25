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
//! with the cautious strategy and without counting or a root, keep their contexts too
//! (roadmap item 753). The next run compares its index with the last one and clears
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
mod query;
#[cfg(test)]
mod retained_accounting_tests;
mod saturate;
#[cfg(test)]
mod scratch_admission_tests;
mod taxonomy;
#[cfg(test)]
mod tierd_memory_tests;
mod world;

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
    static ROOTED_EQ_SPLIT_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_EQ_SPLIT_PROBE_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_EQ_SPLIT_CHILD_EAGER_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_EQ_SPLIT_MAX_DEPTH_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_EQ_SPLIT_MAX_LEAVES_TEST: std::cell::Cell<Option<usize>> =
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

/// Whether item 1075 rooted disjunctive constant-equality splitting is enabled by default
/// (`SPARKLES_ROOTED_EQ_SPLIT=1`), off by default.
fn rooted_eq_split_switch() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_EQ_SPLIT_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_EQ_SPLIT").is_some_and(|v| v == "1"))
}

const DEFAULT_SPLIT_PROBE: usize = 16_384;
const DEFAULT_SPLIT_MAX_DEPTH: usize = 8;
const DEFAULT_SPLIT_MAX_LEAVES: usize = 32;

fn split_probe_threshold() -> usize {
    #[cfg(test)]
    if let Some(v) = ROOTED_EQ_SPLIT_PROBE_TEST.with(|c| c.get()) {
        return v;
    }
    static VAL: OnceLock<usize> = OnceLock::new();
    *VAL.get_or_init(|| {
        std::env::var("SPARKLES_ROOTED_EQ_SPLIT_PROBE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_SPLIT_PROBE)
    })
}

fn split_child_eager() -> bool {
    #[cfg(test)]
    if let Some(v) = ROOTED_EQ_SPLIT_CHILD_EAGER_TEST.with(|c| c.get()) {
        return v;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("SPARKLES_ROOTED_EQ_SPLIT_CHILD_EAGER").is_none_or(|v| v != "0")
    })
}

fn split_max_depth() -> usize {
    #[cfg(test)]
    if let Some(v) = ROOTED_EQ_SPLIT_MAX_DEPTH_TEST.with(|c| c.get()) {
        return v;
    }
    static VAL: OnceLock<usize> = OnceLock::new();
    *VAL.get_or_init(|| {
        std::env::var("SPARKLES_ROOTED_EQ_SPLIT_MAX_DEPTH")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_SPLIT_MAX_DEPTH)
    })
}

fn split_max_leaves() -> usize {
    #[cfg(test)]
    if let Some(v) = ROOTED_EQ_SPLIT_MAX_LEAVES_TEST.with(|c| c.get()) {
        return v;
    }
    static VAL: OnceLock<usize> = OnceLock::new();
    *VAL.get_or_init(|| {
        std::env::var("SPARKLES_ROOTED_EQ_SPLIT_MAX_LEAVES")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_SPLIT_MAX_LEAVES)
    })
}

fn split_trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_EQ_SPLIT_TRACE").is_some_and(|v| v == "1"))
}

#[cfg(test)]
#[allow(dead_code)]
fn with_rooted_eq_split<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_EQ_SPLIT_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_EQ_SPLIT_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
#[allow(dead_code)]
fn with_split_knobs<R>(
    probe: Option<usize>,
    child_eager: Option<bool>,
    max_depth: Option<usize>,
    max_leaves: Option<usize>,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset {
        p: Option<usize>,
        e: Option<bool>,
        d: Option<usize>,
        l: Option<usize>,
    }
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_EQ_SPLIT_PROBE_TEST.with(|c| c.set(self.p));
            ROOTED_EQ_SPLIT_CHILD_EAGER_TEST.with(|c| c.set(self.e));
            ROOTED_EQ_SPLIT_MAX_DEPTH_TEST.with(|c| c.set(self.d));
            ROOTED_EQ_SPLIT_MAX_LEAVES_TEST.with(|c| c.set(self.l));
        }
    }
    let _reset = Reset {
        p: ROOTED_EQ_SPLIT_PROBE_TEST.with(|c| c.replace(probe)),
        e: ROOTED_EQ_SPLIT_CHILD_EAGER_TEST.with(|c| c.replace(child_eager)),
        d: ROOTED_EQ_SPLIT_MAX_DEPTH_TEST.with(|c| c.replace(max_depth)),
        l: ROOTED_EQ_SPLIT_MAX_LEAVES_TEST.with(|c| c.replace(max_leaves)),
    };
    f()
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
    /// such literals to the contexts that hold them. `SPARKLES_ROOTED_MARKER_REPAIR`
    /// overrides it for measurements, with `0`, `1`, or a sum of the bits of
    /// [`dl_rules::DlRun::mrepair`].
    pub rooted_marker_repair: bool,
    /// whether item 1061 phase 2 rooted constant merging on asserted `SameIndividual`
    /// axioms and unconditionally root-entailed unit constant equalities is enabled
    /// (default off; `SPARKLES_ROOTED_MERGE=1` turns it on).
    #[doc(hidden)]
    pub rooted_merge: bool,
    /// whether item 1075 demand-driven binary equality case splitting on disjunctive
    /// constant equalities in rooted runs is enabled (default off;
    /// `SPARKLES_ROOTED_EQ_SPLIT=1` turns it on).
    #[doc(hidden)]
    pub rooted_eq_split: bool,
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
            rooted_eq_split: rooted_eq_split_switch(),
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
    /// number of binary equality splits executed in the answering rooted attempt
    #[doc(hidden)]
    pub dl_rooted_splits: usize,
    /// number of terminal leaves (consistent + inconsistent) evaluated in the answering
    /// rooted split tree (0 when no fork occurred)
    #[doc(hidden)]
    pub dl_rooted_leaves: usize,
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
    /// the chains outside OWL 2 EL for their ranges alone in a run of rewritten axioms,
    /// which the statistics of the original axioms count
    chain_ranged: Vec<AxiomKind>,
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
    /// retained self-contained answer published by a completed item 1075 split tree
    pub(super) split_answer: Option<Box<SplitAnswer>>,
    /// winning split-stop payload returned from `run_once` to `run_splitting`
    split_stop: Option<SplitStop>,
    /// active item 1075 branch configuration during a `run_splitting` branch evaluation
    split_cfg: Option<SplitBranchCfg>,
    /// expressions of `ax.individuals` retained from a completed forked leaf for
    /// complete nominal consistency validation in `export_leaf`
    leaf_individuals: Vec<Ex>,
    /// set when a forked branch loses rooted eligibility (or reduction declines)
    split_ineligible: bool,
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

/// Payload transferred from a winning `Stop::Split` out of `run_once` into `run_splitting`.
/// Carries only interner individual IDs so no IRI string is cloned while the branch is live.
struct SplitStop {
    u_id: u32,
    v_id: u32,
    spent: usize,
    bound: usize,
    peak: usize,
    prio: u8,
}

/// Per-branch configuration passed from `run_splitting` into `run` / `run_once`.
struct SplitBranchCfg {
    mode: dl_rules::SplitMode,
    probe: usize,
    epoch: u32,
    forked: bool,
    orig_individuals: std::sync::Arc<[NamedIndividual]>,
    neq: Vec<(NamedIndividual, NamedIndividual)>,
}

/// Self-contained classification & `describe` answer published by a completed item 1075
/// split tree (`0`, `1`, or `>= 2` consistent leaves).
pub(super) struct SplitAnswer {
    pub(super) consistent: bool,
    pub(super) taxonomy: Taxonomy,
    /// Sorted, deduplicated genuine original named individuals (never anonymous witnesses
    /// or reduction-generated hubs).
    pub(super) individuals: Vec<NamedIndividual>,
    /// Per taxonomy node index, sorted deduplicated indices into `individuals` that are
    /// instances of that node's classes in every consistent leaf. `TOP_NODE` and
    /// `BOTTOM_NODE` stay empty and are served directly by `describe`.
    pub(super) node_insts: Vec<Vec<u32>>,
}

impl SplitAnswer {
    pub(super) fn owned_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<SplitAnswer>()
            .saturating_add(taxonomy::taxonomy_owned_bytes(&self.taxonomy))
            .saturating_add(
                self.individuals
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NamedIndividual>()),
            )
            .saturating_add(
                self.node_insts
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Vec<u32>>()),
            );
        for ind in &self.individuals {
            bytes = bytes.saturating_add(ind.as_str().len());
        }
        for insts in &self.node_insts {
            bytes =
                bytes.saturating_add(insts.capacity().saturating_mul(std::mem::size_of::<u32>()));
        }
        bytes
    }

    pub(super) fn describe(&self, c: &Class) -> Option<Description> {
        let node = self.taxonomy.node_of(c)?;
        let sorted = |mut v: Vec<Class>| {
            v.sort();
            v.dedup();
            v
        };
        if !self.consistent {
            let eq: Vec<Class> = self
                .taxonomy
                .bottom()
                .classes
                .iter()
                .filter(|&k| k != c)
                .cloned()
                .collect();
            return Some(Description {
                satisfiable: false,
                equivalents: sorted(eq),
                instances: self.individuals.clone(),
                ..Default::default()
            });
        }
        if node == taxonomy::BOTTOM_NODE {
            let eq: Vec<Class> = self
                .taxonomy
                .bottom()
                .classes
                .iter()
                .filter(|&k| k != c)
                .cloned()
                .collect();
            return Some(Description {
                satisfiable: false,
                equivalents: sorted(eq),
                ..Default::default()
            });
        }
        let equivalents: Vec<Class> = self
            .taxonomy
            .node(node)
            .classes
            .iter()
            .filter(|&k| k != c)
            .cloned()
            .collect();
        let classes_of = |nodes: &[usize]| -> Vec<Class> {
            let mut out = Vec::new();
            for &ni in nodes {
                out.extend(self.taxonomy.node(ni).classes.iter().cloned());
            }
            sorted(out)
        };
        let direct_superclasses = classes_of(&self.taxonomy.node(node).parents);
        let superclasses = classes_of(&self.taxonomy.ancestors(node));
        let direct_subclasses = classes_of(&self.taxonomy.node(node).children);
        let subclasses = classes_of(&self.taxonomy.descendants(node));
        let instances = if node == taxonomy::TOP_NODE {
            self.individuals.clone()
        } else {
            self.node_insts
                .get(node)
                .map(|is| {
                    is.iter()
                        .map(|&j| self.individuals[j as usize].clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        Some(Description {
            satisfiable: true,
            equivalents: sorted(equivalents),
            superclasses,
            direct_superclasses,
            subclasses,
            direct_subclasses,
            instances,
        })
    }
}

enum BuildCanonErr {
    Poll(Error),
    Memory(usize),
}

enum LeafExportResult {
    Inconsistent,
    Consistent {
        vis_subs: Vec<(bool, Vec<u32>)>,
        vis_top: Vec<u32>,
        leaf_insts: Vec<Vec<u32>>,
        owned_bytes: usize,
    },
    ExcludedOrIncomplete,
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

    /// Returns the canonical representative of `n` in this partition without mutating
    /// path compression (or `n` itself when `n` is not in the union-find).
    fn rep_of_named_ref<'a>(&'a self, n: &'a NamedIndividual) -> &'a NamedIndividual {
        let (is_anon, raw) = canonical_merge_parts(false, n.as_str());
        let h = merge_name_hash(is_anon, raw);
        let Some(&id) = self
            .ids
            .find(h, |&i| merge_name_eq(&self.names[i as usize], is_anon, raw))
        else {
            return n;
        };
        let mut r = id;
        while self.parent[r as usize] != r {
            r = self.parent[r as usize];
        }
        &self.names[self.best[r as usize] as usize]
    }

    /// Clones this union-find table for a child split branch, pre-admitting exact UF bytes
    /// against `admit` before allocation while sharing immutable `Arc<CanonMap>` read-only.
    fn clone_admitted(
        &self,
        live_base: usize,
        admit: &mut impl FnMut(usize) -> bool,
    ) -> Result<Merge, usize> {
        let add = self.uf_bytes();
        let Some(peak) = live_base.checked_add(add) else {
            return Err(usize::MAX);
        };
        if !admit(peak) {
            return Err(peak);
        }
        let mut names = Vec::with_capacity(self.names.len());
        for n in &self.names {
            names.push(index::exact_individual(n));
        }
        let mut hashes = Vec::with_capacity(self.hashes.len());
        hashes.extend_from_slice(&self.hashes);
        let mut ids = hashbrown::HashTable::with_capacity(self.ids.len());
        for (i, &h) in hashes.iter().enumerate() {
            ids.insert_unique(h, i as u32, |&idx| hashes[idx as usize]);
        }
        let mut parent = Vec::with_capacity(self.parent.len());
        parent.extend_from_slice(&self.parent);
        let mut size = Vec::with_capacity(self.size.len());
        size.extend_from_slice(&self.size);
        let mut best = Vec::with_capacity(self.best.len());
        best.extend_from_slice(&self.best);
        let active_hub = self.active_hub.as_ref().map(index::exact_individual);
        Ok(Merge {
            names,
            hashes,
            ids,
            parent,
            size,
            best,
            name_str_bytes: self.name_str_bytes,
            active_hub,
            shared: self.shared.clone(),
            spent: 0,
            restarts: 0,
            frozen_bound: None,
        })
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
    /// Tier D without counting, a root or the eager strategy, whose invalidation
    /// follows the expressions that the units, markers and stored clauses of a context
    /// name, the predecessors of a context and the class of a test that held
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
    rooted_eq_split: bool,
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
    /// whether one of them needs case analysis
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
/// role table, which a run builds once, one with data, whose told subsumers the index
/// derives from every data axiom at once, a key, one with an individual, whose
/// merges the invalidation does not follow, or one with a self restriction (roadmap
/// item 1258). A negative self restriction gives its subsumer to every context with a
/// loop by a subrole, and a loop comes from a self restriction of another role, a
/// reflexive role or a chain as well as from the restriction itself, so a kept context
/// can have the loop without holding the changed expression that the invalidation
/// looks for.
fn needs_full_run(ax: &AxiomKind) -> bool {
    if role_axiom(ax) || matches!(ax, AxiomKind::HasKey(..)) {
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
        || sig.0.iter().any(|e| {
            matches!(
                e,
                Entity::DataProperty(_) | Entity::Datatype(_) | Entity::NamedIndividual(_)
            )
        })
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
        AxiomKind::EquivalentClasses(cs) | AxiomKind::DisjointClasses(cs) => cs.iter().all(plain),
        AxiomKind::DisjointUnion(_, cs) => cs.iter().all(plain),
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

/// The bytes the classifier's copy of an axiom takes, roughly: the axiom, its entry and
/// each entity's IRI with the expression that holds it.
fn held_bytes_of(ax: &AxiomKind) -> usize {
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    let mut bytes = std::mem::size_of::<AxiomKind>() + std::mem::size_of::<Held>() + 16;
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
        let mut out = r.map(|(consistent, taxonomy, stats)| {
            Classification::new(consistent, taxonomy, stats, partial)
        })?;
        let excluded = self.sat.excluded();
        if !excluded.is_empty() {
            if limits.strict {
                let err = excluded.to_vec();
                self.sat.invalidate_for_error();
                return Err(Error::Incomplete(err));
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
            let err = self.sat.excluded().to_vec();
            self.sat.invalidate_for_error();
            return Err(Error::Incomplete(err));
        }
        Ok(out)
    }

    /// Saturates the axioms held as [`classify`](Self::classify) does, again only what
    /// changed since the last run, but builds no taxonomy. [`describe`](Self::describe)
    /// then answers for one class from the saturated contexts.
    pub fn saturate(&mut self, limits: &Limits) -> Result<Saturated, Error> {
        self.run_held(limits, false)
            .map(|(consistent, _, stats)| Saturated { consistent, stats })
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

impl Saturation {
    pub fn new() -> Saturation {
        Saturation {
            interner: Interner::new(),
            index: None,
            ctx: Vec::new(),
            source_len: None,
            excluded: Vec::new(),
            dirty: true,
            worlds: FxHashMap::default(),
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
            chain_ranged: Vec::new(),
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
            merge: Merge::default(),
            canon_want: None,
            active_hub: None,
            merge_stop: None,
            merge_disabled: false,
            split_answer: None,
            split_stop: None,
            split_cfg: None,
            leaf_individuals: Vec::new(),
            split_ineligible: false,
        }
    }

    /// Invalidates any retained classification/saturation answer before returning an
    /// error through a public API (`Classifier::classify`, `Saturation::classify`, or
    /// split abort) so `describe(&C)` returns `None`.
    fn invalidate_for_error(&mut self) {
        self.dirty = true;
        self.split_answer = None;
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
        let mut out = r.map(|(consistent, taxonomy, stats)| {
            Classification::new(consistent, taxonomy, stats, partial)
        })?;
        if !self.excluded.is_empty() {
            if limits.strict {
                self.invalidate_for_error();
                return Err(Error::Incomplete(self.excluded.clone()));
            }
            if !limits.cover_later {
                out.cover(axioms, &self.excluded, None, limits);
            }
        }
        if limits.strict && !out.complete {
            self.invalidate_for_error();
            return Err(Error::Incomplete(self.excluded.clone()));
        }
        Ok(out)
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
        self.run_with(axioms, changes, 0, limits, false)
            .map(|(consistent, _, stats)| Saturated { consistent, stats })
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
        // Drop any retained split answer without forcing `self.dirty = true`, so ordinary
        // switch-off incremental runs keep their contexts untouched.
        self.split_answer = None;
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

    /// Runs `self.run(...)`, handling item 1075 demand-driven binary equality splitting
    /// (`S1-Bin` + `S2`) composed with item 1061 phase 2 clean constant-merge restarts.
    ///
    /// - Eligible ONLY on full `classify` calls (`with_taxonomy && !limits.partial`) when
    ///   `limits.rooted && limits.rooted_eq_split && !limits.el_only && !self.no_rooted && !self.skip_dl`.
    /// - When 0 splits occur, returns directly on the no-fork fast path with
    ///   `self.split_answer = None`, `self.index.is_some()`, `self.dirty = false`.
    /// - When a fork occurs, evaluates the binary DFS tree within one frozen `attempt_bound`,
    ///   pre-admits every structure against one synchronized ledger, validates complete
    ///   consistency across all real nominals (including anonymous witnesses and generated
    ///   hubs), projects to the original visible signature, and publishes one `SplitAnswer`.
    #[allow(clippy::too_many_arguments)]
    fn run_splitting(
        &mut self,
        axioms: &[&AxiomKind],
        mut changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
        with_taxonomy: bool,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        let eligible = limits.rooted
            && limits.rooted_eq_split
            && !limits.el_only
            && !limits.partial
            && with_taxonomy
            && !self.no_rooted
            && !self.skip_dl;
        if !eligible {
            return self.run_merging(
                axioms,
                changes,
                held_bytes,
                limits,
                admitted,
                call_peak,
                with_taxonomy,
            );
        }

        // Single synchronized outer ledger shared across the whole split tree.
        struct SplitLedger<'a> {
            held_base: usize,
            live: AtomicUsize,
            canon: AtomicUsize,
            peak: AtomicUsize,
            admitted: &'a std::sync::Arc<AtomicUsize>,
            limits: &'a Limits,
            lock: parking_lot::Mutex<()>,
        }
        impl SplitLedger<'_> {
            fn delta_checked(&self, d: isize) -> Result<usize, usize> {
                let _g = self.lock.lock();
                if d >= 0 {
                    let add = d as usize;
                    let Some(next) = self.live.load(Ordering::Relaxed).checked_add(add) else {
                        self.peak.fetch_max(usize::MAX, Ordering::Relaxed);
                        return Err(usize::MAX);
                    };
                    let Some(total_live) = next.checked_add(self.canon.load(Ordering::Relaxed))
                    else {
                        self.peak.fetch_max(usize::MAX, Ordering::Relaxed);
                        return Err(usize::MAX);
                    };
                    let Some(used) = self.held_base.checked_add(total_live) else {
                        self.peak.fetch_max(usize::MAX, Ordering::Relaxed);
                        return Err(usize::MAX);
                    };
                    let paid_before = self.admitted.load(Ordering::Relaxed);
                    let mut paid = paid_before;
                    let charge_fn = self
                        .limits
                        .charge
                        .as_deref()
                        .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
                    let ok = cover_memory(used, self.limits.max_memory, charge_fn, &mut paid);
                    #[cfg(test)]
                    admission_test::emit_merge_phase(admission_test::MergePhase::LedgerCharge {
                        source: admission_test::LedgerChargeSource::Delta,
                        used,
                        paid_before,
                        paid_after: if ok { paid } else { paid_before },
                        delta: if ok {
                            paid.saturating_sub(paid_before)
                        } else {
                            0
                        },
                        admitted: ok,
                    });
                    if !ok {
                        self.peak.fetch_max(used, Ordering::Relaxed);
                        return Err(used);
                    }
                    self.admitted.store(paid, Ordering::Relaxed);
                    self.live.store(next, Ordering::Relaxed);
                    self.peak.fetch_max(used, Ordering::Relaxed);
                    Ok(used)
                } else {
                    let sub = d.unsigned_abs();
                    let cur = self.live.load(Ordering::Relaxed);
                    let next = cur.saturating_sub(sub);
                    self.live.store(next, Ordering::Relaxed);
                    Ok(self
                        .held_base
                        .saturating_add(next)
                        .saturating_add(self.canon.load(Ordering::Relaxed)))
                }
            }
            fn delta(&self, d: isize) -> Result<(), usize> {
                self.delta_checked(d).map(|_| ())
            }
            fn add_checked(&self, bytes: usize) -> Result<usize, usize> {
                if bytes == 0 {
                    let _g = self.lock.lock();
                    return Ok(self
                        .held_base
                        .saturating_add(self.live.load(Ordering::Relaxed))
                        .saturating_add(self.canon.load(Ordering::Relaxed)));
                }
                let Ok(d) = isize::try_from(bytes) else {
                    self.peak.fetch_max(usize::MAX, Ordering::Relaxed);
                    return Err(usize::MAX);
                };
                self.delta_checked(d)
            }
            fn add(&self, bytes: usize) -> Result<(), usize> {
                self.add_checked(bytes).map(|_| ())
            }
            fn sub(&self, bytes: usize) {
                if bytes == 0 {
                    return;
                }
                let _g = self.lock.lock();
                let cur = self.live.load(Ordering::Relaxed);
                self.live
                    .store(cur.saturating_sub(bytes), Ordering::Relaxed);
            }
            fn add_already_paid(&self, bytes: usize) {
                if bytes == 0 {
                    return;
                }
                let _g = self.lock.lock();
                let next = self.live.load(Ordering::Relaxed).saturating_add(bytes);
                self.live.store(next, Ordering::Relaxed);
                let used = self
                    .held_base
                    .saturating_add(next)
                    .saturating_add(self.canon.load(Ordering::Relaxed));
                self.peak.fetch_max(used, Ordering::Relaxed);
            }
            fn set_canon(&self, bytes: usize) {
                let _g = self.lock.lock();
                self.canon.store(bytes, Ordering::Relaxed);
            }
            fn live_bytes(&self) -> usize {
                self.live.load(Ordering::Relaxed)
            }
            fn base_with_canon(&self, subtract_uf: usize) -> Option<usize> {
                let _g = self.lock.lock();
                let live = self
                    .live
                    .load(Ordering::Relaxed)
                    .saturating_sub(subtract_uf);
                self.held_base
                    .checked_add(live)?
                    .checked_add(self.canon.load(Ordering::Relaxed))
            }
            fn note_peak(&self, p: usize) {
                self.peak.fetch_max(p, Ordering::Relaxed);
            }
            fn cover(&self, used: usize) -> bool {
                let _g = self.lock.lock();
                self.peak.fetch_max(used, Ordering::Relaxed);
                let paid_before = self.admitted.load(Ordering::Relaxed);
                let mut paid = paid_before;
                let charge_fn = self
                    .limits
                    .charge
                    .as_deref()
                    .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
                let ok = cover_memory(used, self.limits.max_memory, charge_fn, &mut paid);
                #[cfg(test)]
                admission_test::emit_merge_phase(admission_test::MergePhase::LedgerCharge {
                    source: admission_test::LedgerChargeSource::Cover,
                    used,
                    paid_before,
                    paid_after: if ok { paid } else { paid_before },
                    delta: if ok {
                        paid.saturating_sub(paid_before)
                    } else {
                        0
                    },
                    admitted: ok,
                });
                if ok {
                    self.admitted.store(paid, Ordering::Relaxed);
                }
                ok
            }
        }

        struct SplitFrame {
            depth: usize,
            merge: Merge,
            uf_bytes: usize,
            neq: Vec<(NamedIndividual, NamedIndividual)>,
            neq_bytes: usize,
        }
        impl SplitFrame {
            fn neq_owned_bytes(neq: &Vec<(NamedIndividual, NamedIndividual)>) -> usize {
                neq.capacity()
                    .saturating_mul(std::mem::size_of::<(NamedIndividual, NamedIndividual)>())
                    .saturating_add(
                        neq.iter()
                            .map(|(a, b)| a.as_str().len().saturating_add(b.as_str().len()))
                            .sum::<usize>(),
                    )
            }
        }

        struct SplitAcc {
            consistent_leaves: usize,
            subs: Vec<(bool, Vec<u32>)>,
            top: Vec<u32>,
            insts: Vec<Vec<u32>>,
            debit: usize,
        }
        impl SplitAcc {
            fn new() -> Self {
                Self {
                    consistent_leaves: 0,
                    subs: Vec::new(),
                    top: Vec::new(),
                    insts: Vec::new(),
                    debit: 0,
                }
            }
        }

        fn sorted_intersect_in_place(a: &mut Vec<u32>, b: &[u32]) {
            let (mut i, mut j, mut out) = (0, 0, 0);
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        a[out] = a[i];
                        out += 1;
                        i += 1;
                        j += 1;
                    }
                }
            }
            a.truncate(out);
        }

        fn distinct_canon_bytes_excluding(
            stack: &[SplitFrame],
            extra: &[Option<&std::sync::Arc<index::CanonMap>>],
            exclude: &[Option<&std::sync::Arc<index::CanonMap>>],
        ) -> usize {
            let is_excluded = |p: *const index::CanonMap| {
                exclude
                    .iter()
                    .flatten()
                    .any(|ex| std::sync::Arc::as_ptr(ex) == p)
            };
            let mut sum = 0usize;
            for (i, f) in stack.iter().enumerate() {
                let Some(arc) = f.merge.shared.as_ref() else {
                    continue;
                };
                let p = std::sync::Arc::as_ptr(arc);
                if is_excluded(p) {
                    continue;
                }
                let seen_earlier = stack[..i]
                    .iter()
                    .filter_map(|g| g.merge.shared.as_ref())
                    .any(|g| std::sync::Arc::as_ptr(g) == p);
                if !seen_earlier {
                    sum = sum.saturating_add(arc.owned_bytes());
                }
            }
            for (j, arc) in extra.iter().enumerate() {
                let Some(arc) = arc else {
                    continue;
                };
                let p = std::sync::Arc::as_ptr(arc);
                if is_excluded(p) {
                    continue;
                }
                let seen_in_stack = stack
                    .iter()
                    .filter_map(|g| g.merge.shared.as_ref())
                    .any(|g| std::sync::Arc::as_ptr(g) == p);
                let seen_in_extra = extra[..j]
                    .iter()
                    .flatten()
                    .any(|g| std::sync::Arc::as_ptr(g) == p);
                if !seen_in_stack && !seen_in_extra {
                    sum = sum.saturating_add(arc.owned_bytes());
                }
            }
            sum
        }

        fn distinct_canon_bytes(
            stack: &[SplitFrame],
            extra: &[Option<&std::sync::Arc<index::CanonMap>>],
        ) -> usize {
            distinct_canon_bytes_excluding(stack, extra, &[])
        }

        fn ensure_stack_cap(
            stack: &mut Vec<SplitFrame>,
            stack_debit: &mut usize,
            additional: usize,
            ledger: &SplitLedger<'_>,
        ) -> Result<Option<usize>, usize> {
            let Some(need_len) = stack.len().checked_add(additional) else {
                ledger.note_peak(usize::MAX);
                return Err(usize::MAX);
            };
            if need_len <= stack.capacity() {
                return Ok(None);
            }
            let elem = std::mem::size_of::<SplitFrame>();
            let mut next_cap = stack.capacity();
            while next_cap < need_len {
                let Some(c) = dl_clause::next_vec_cap(next_cap, elem) else {
                    ledger.note_peak(usize::MAX);
                    return Err(usize::MAX);
                };
                next_cap = c;
            }
            let Some(new_b) = next_cap.checked_mul(elem) else {
                ledger.note_peak(usize::MAX);
                return Err(usize::MAX);
            };
            let old_b = *stack_debit;
            let mut proposed = ledger.add_checked(new_b)?;
            stack.reserve_exact(next_cap - stack.len());
            let Some(actual_new) = stack.capacity().checked_mul(elem) else {
                ledger.sub(old_b);
                *stack_debit = new_b;
                ledger.note_peak(usize::MAX);
                return Err(usize::MAX);
            };
            ledger.sub(old_b);
            *stack_debit = new_b;
            if new_b > actual_new {
                ledger.sub(new_b - actual_new);
                *stack_debit = actual_new;
            } else if actual_new > new_b {
                proposed = proposed.max(ledger.add_checked(actual_new - new_b)?);
                *stack_debit = actual_new;
            }
            Ok(Some(proposed))
        }

        // Establish the full live entry baseline before collecting the original signature:
        // caller `held_bytes`, any asserted-`SameIndividual` union-find in `self.merge`,
        // any distinct canonical maps on `self`, and any retained prior state on a reused
        // `Saturation` when `collect_same` returned early without resetting.
        let root_uf_bytes = self.merge.uf_bytes();
        let mut entry_state_bytes = self.leaf_live_bytes_without_canon_and_merge();
        let entry_canon_bytes = self.canon_live_bytes();
        let entry_total = held_bytes
            .saturating_add(root_uf_bytes)
            .saturating_add(entry_state_bytes)
            .saturating_add(entry_canon_bytes);
        let ledger = SplitLedger {
            held_base: held_bytes,
            live: AtomicUsize::new(root_uf_bytes.saturating_add(entry_state_bytes)),
            canon: AtomicUsize::new(entry_canon_bytes),
            peak: AtomicUsize::new((*call_peak).max(entry_total)),
            admitted,
            limits,
            lock: parking_lot::Mutex::new(()),
        };
        let mut raw_classes: FxHashSet<&str> = FxHashSet::default();
        let mut raw_inds: FxHashMap<&str, bool> = FxHashMap::default();
        let mut raw_classes_debit = 0usize;
        let mut raw_inds_debit = 0usize;
        let sig_mem_err: std::cell::Cell<Option<usize>> = std::cell::Cell::new(None);
        for (idx, k) in axioms.iter().enumerate() {
            if idx.is_multiple_of(POLL_AXIOMS)
                && let Err(e) = poll(limits)
            {
                drop((raw_classes, raw_inds));
                ledger.sub(raw_classes_debit.saturating_add(raw_inds_debit));
                *call_peak = (*call_peak).max(ledger.peak.load(Ordering::Relaxed));
                self.invalidate_for_error();
                return Err(e);
            }
            let non_decl = !matches!(k, AxiomKind::Declaration(_));
            crate::model::Signature::visit_classes_and_individuals(
                k,
                &mut |c| {
                    if sig_mem_err.get().is_some() {
                        return;
                    }
                    let s = c.as_str();
                    if s == crate::vocab::OWL_THING || s == crate::vocab::OWL_NOTHING {
                        return;
                    }
                    if raw_classes.contains(s) {
                        return;
                    }
                    if raw_classes.len() == raw_classes.capacity() {
                        let Some(need) = dl_clause::next_hash_table_bytes(
                            raw_classes.capacity(),
                            std::mem::size_of::<&str>(),
                        ) else {
                            ledger.note_peak(usize::MAX);
                            sig_mem_err.set(Some(usize::MAX));
                            return;
                        };
                        let cls_admit = ledger.add_checked(need);
                        #[cfg(test)]
                        admission_test::emit_merge_phase(admission_test::MergePhase::SplitAlloc {
                            site: admission_test::SplitAllocSite::OrigSigTable,
                            proposed: cls_admit.unwrap_or_else(|u| u),
                            admitted: cls_admit.is_ok(),
                        });
                        if let Err(used) = cls_admit {
                            sig_mem_err.set(Some(used));
                            return;
                        }
                        raw_classes.insert(s);
                        let new_live = dl_clause::hash_table_bytes(
                            raw_classes.capacity(),
                            std::mem::size_of::<&str>(),
                        );
                        let charged = raw_classes_debit.saturating_add(need);
                        if charged > new_live {
                            ledger.sub(charged - new_live);
                        } else if new_live > charged {
                            ledger.add_already_paid(new_live - charged);
                        }
                        raw_classes_debit = new_live;
                    } else {
                        raw_classes.insert(s);
                    }
                },
                &mut |n| {
                    if sig_mem_err.get().is_some() {
                        return;
                    }
                    let s = n.as_str();
                    if let Some(v) = raw_inds.get_mut(s) {
                        *v |= non_decl;
                        return;
                    }
                    if raw_inds.len() == raw_inds.capacity() {
                        let Some(need) = dl_clause::next_hash_table_bytes(
                            raw_inds.capacity(),
                            std::mem::size_of::<(&str, bool)>(),
                        ) else {
                            ledger.note_peak(usize::MAX);
                            sig_mem_err.set(Some(usize::MAX));
                            return;
                        };
                        let cls_admit = ledger.add_checked(need);
                        #[cfg(test)]
                        admission_test::emit_merge_phase(admission_test::MergePhase::SplitAlloc {
                            site: admission_test::SplitAllocSite::OrigSigTable,
                            proposed: cls_admit.unwrap_or_else(|u| u),
                            admitted: cls_admit.is_ok(),
                        });
                        if let Err(used) = cls_admit {
                            sig_mem_err.set(Some(used));
                            return;
                        }
                        raw_inds.insert(s, non_decl);
                        let new_live = dl_clause::hash_table_bytes(
                            raw_inds.capacity(),
                            std::mem::size_of::<(&str, bool)>(),
                        );
                        let charged = raw_inds_debit.saturating_add(need);
                        if charged > new_live {
                            ledger.sub(charged - new_live);
                        } else if new_live > charged {
                            ledger.add_already_paid(new_live - charged);
                        }
                        raw_inds_debit = new_live;
                    } else {
                        raw_inds.insert(s, non_decl);
                    }
                },
            );
            if let Some(used) = sig_mem_err.get() {
                drop((raw_classes, raw_inds));
                ledger.sub(raw_classes_debit.saturating_add(raw_inds_debit));
                *call_peak = (*call_peak).max(ledger.peak.load(Ordering::Relaxed));
                self.invalidate_for_error();
                return Err(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used,
                });
            }
        }
        // Checked domain bounds: disable splitting cleanly if < 2 genuine named individuals
        // or if class/individual counts exceed sentinel/rank bounds.
        if raw_inds.len() < 2
            || raw_classes.len() > taxonomy::MAX_CLASS_COUNT
            || raw_inds.len() >= u32::MAX as usize
        {
            drop((raw_classes, raw_inds));
            ledger.sub(raw_classes_debit.saturating_add(raw_inds_debit));
            *call_peak = (*call_peak).max(ledger.peak.load(Ordering::Relaxed));
            return self.run_merging(
                axioms,
                changes,
                held_bytes,
                limits,
                admitted,
                call_peak,
                with_taxonomy,
            );
        }

        let orig_classes_bytes = raw_classes
            .len()
            .checked_mul(std::mem::size_of::<Class>())
            .and_then(|b| {
                raw_classes
                    .iter()
                    .try_fold(b, |acc, s| acc.checked_add(s.len()))
            })
            .ok_or_else(|| {
                ledger.note_peak(usize::MAX);
                *call_peak = usize::MAX;
                self.invalidate_for_error();
                Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                }
            })?;
        let arc_slice_hdr = 2usize * std::mem::size_of::<usize>();
        let orig_inds_bytes = raw_inds
            .len()
            .checked_mul(
                std::mem::size_of::<(NamedIndividual, bool)>()
                    + std::mem::size_of::<NamedIndividual>(),
            )
            .and_then(|b| b.checked_add(arc_slice_hdr))
            .and_then(|b| {
                raw_inds
                    .keys()
                    .try_fold(b, |acc, s| acc.checked_add(s.len())?.checked_add(s.len()))
            })
            .ok_or_else(|| {
                ledger.note_peak(usize::MAX);
                *call_peak = usize::MAX;
                self.invalidate_for_error();
                Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                }
            })?;
        let orig_sig_bytes = orig_classes_bytes
            .checked_add(orig_inds_bytes)
            .ok_or_else(|| {
                ledger.note_peak(usize::MAX);
                *call_peak = usize::MAX;
                self.invalidate_for_error();
                Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                }
            })?;
        let orig_sig_admit = ledger.add_checked(orig_sig_bytes);
        #[cfg(test)]
        admission_test::emit_merge_phase(admission_test::MergePhase::SplitAlloc {
            site: admission_test::SplitAllocSite::OrigSig,
            proposed: orig_sig_admit.unwrap_or_else(|u| u),
            admitted: orig_sig_admit.is_ok(),
        });
        if let Err(used) = orig_sig_admit {
            drop((raw_classes, raw_inds));
            ledger.sub(raw_classes_debit.saturating_add(raw_inds_debit));
            *call_peak = (*call_peak).max(ledger.peak.load(Ordering::Relaxed));
            self.invalidate_for_error();
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            });
        }
        let mut orig_classes: Vec<Class> = Vec::with_capacity(raw_classes.len());
        for s in raw_classes {
            orig_classes.push(Class::new(s.to_owned()));
        }
        ledger.sub(raw_classes_debit);
        orig_classes.sort_unstable();
        let mut orig_individuals: Vec<(NamedIndividual, bool)> = Vec::with_capacity(raw_inds.len());
        for (s, non_decl) in raw_inds {
            orig_individuals.push((NamedIndividual::new(s.to_owned()), non_decl));
        }
        ledger.sub(raw_inds_debit);
        orig_individuals.sort_unstable_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let orig_ind_names_arc: std::sync::Arc<[NamedIndividual]> = orig_individuals
            .iter()
            .map(|(n, _)| index::exact_individual(n))
            .collect();

        let probe = split_probe_threshold();
        let child_eager = split_child_eager();
        let max_depth = split_max_depth();
        let max_leaves = split_max_leaves().max(1);

        // Initial root frame inherits any prepass asserted `SameIndividual` partition
        // whose `root_uf_bytes` is already debited in `ledger` from entry.
        let mut stack: Vec<SplitFrame> = Vec::new();
        let mut stack_debit = 0usize;
        let root_stack_admit = ensure_stack_cap(&mut stack, &mut stack_debit, 1, &ledger);
        #[cfg(test)]
        if let Some(proposed) = match root_stack_admit {
            Ok(Some(p)) => Some(p),
            Ok(None) => None,
            Err(u) => Some(u),
        } {
            admission_test::emit_merge_phase(admission_test::MergePhase::SplitAlloc {
                site: admission_test::SplitAllocSite::RootStack,
                proposed,
                admitted: root_stack_admit.is_ok(),
            });
        }
        if let Err(used) = root_stack_admit {
            drop((orig_classes, orig_individuals, orig_ind_names_arc));
            *call_peak = (*call_peak)
                .max(ledger.peak.load(Ordering::Relaxed))
                .max(used);
            self.merge.clear();
            self.canon_want = None;
            self.invalidate_for_error();
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            });
        }
        let root_merge = std::mem::take(&mut self.merge);
        stack.push(SplitFrame {
            depth: 0,
            merge: root_merge,
            uf_bytes: root_uf_bytes,
            neq: Vec::new(),
            neq_bytes: 0,
        });
        ledger.set_canon(distinct_canon_bytes(
            &stack,
            &[self.interner.canon.as_ref(), self.canon_want.as_ref()],
        ));
        let mut acc = SplitAcc::new();
        let mut forked = false;
        let mut planned_leaves = 1usize;
        let mut splits = 0usize;
        let mut leaves = 0usize;
        let mut restarts = 0usize;
        let mut dl_rooted_spent = 0usize;
        let mut leaf_work = 0usize;
        let mut attempt_bound: Option<usize> = None;
        let mut epoch_counter = 0u32;
        let mut agg_stats = Stats {
            threads: threads_of(limits),
            added: self.source_len.unwrap_or(axioms.len()),
            ..Stats::default()
        };
        let split_start = Instant::now();

        enum LoopExit {
            Done,
            ExcludedAbort,
            Error(Error),
        }

        let mut exit = LoopExit::Done;

        // Remaps every disequality in `neq` through `merge`. Returns `Ok((true, None))` if
        // any pair collapsed to `a == b` (proving the branch inconsistent),
        // `Ok((false, proposed))` on surviving deduplicated pairs, or `Err(used)` on memory
        // admission failure.
        let remap_neq = |merge: &Merge,
                         neq: &mut Vec<(NamedIndividual, NamedIndividual)>,
                         neq_bytes: &mut usize,
                         ledger: &SplitLedger<'_>|
         -> Result<(bool, Option<usize>), usize> {
            if neq.is_empty() {
                return Ok((false, None));
            }
            let Some(mut new_bytes) = neq
                .len()
                .checked_mul(std::mem::size_of::<(NamedIndividual, NamedIndividual)>())
            else {
                ledger.note_peak(usize::MAX);
                return Err(usize::MAX);
            };
            for (a, b) in neq.iter() {
                let ra = merge.rep_of_named_ref(a);
                let rb = merge.rep_of_named_ref(b);
                if ra == rb {
                    return Ok((true, None));
                }
                let (lo, hi) = if ra.as_str() <= rb.as_str() {
                    (ra, rb)
                } else {
                    (rb, ra)
                };
                let Some(next) = new_bytes
                    .checked_add(lo.as_str().len())
                    .and_then(|n| n.checked_add(hi.as_str().len()))
                else {
                    ledger.note_peak(usize::MAX);
                    return Err(usize::MAX);
                };
                new_bytes = next;
            }
            let proposed = ledger.add_checked(new_bytes)?;
            let mut remapped: Vec<(NamedIndividual, NamedIndividual)> =
                Vec::with_capacity(neq.len());
            for (a, b) in neq.iter() {
                let ra = merge.rep_of_named_ref(a);
                let rb = merge.rep_of_named_ref(b);
                let (lo, hi) = if ra.as_str() <= rb.as_str() {
                    (ra, rb)
                } else {
                    (rb, ra)
                };
                remapped.push((index::exact_individual(lo), index::exact_individual(hi)));
            }
            remapped.sort_unstable_by(|x, y| {
                x.0.as_str()
                    .cmp(y.0.as_str())
                    .then_with(|| x.1.as_str().cmp(y.1.as_str()))
            });
            remapped.dedup();
            let old_bytes = *neq_bytes;
            let exact_new = SplitFrame::neq_owned_bytes(&remapped);
            let old = std::mem::replace(neq, remapped);
            drop(old);
            *neq_bytes = exact_new;
            let total_before = old_bytes.saturating_add(new_bytes);
            if total_before > exact_new {
                ledger.sub(total_before - exact_new);
            } else if exact_new > total_before {
                ledger.add_already_paid(exact_new - total_before);
            }
            Ok((false, Some(proposed)))
        };

        'dfs: while let Some(mut frame) = stack.pop() {
            if let Err(e) = poll(limits) {
                ledger.sub(frame.uf_bytes.saturating_add(frame.neq_bytes));
                exit = LoopExit::Error(e);
                break 'dfs;
            }
            let mut frame_consumed = false;
            let mut force_no_split = false;

            'branch: loop {
                if let Err(e) = poll(limits) {
                    exit = LoopExit::Error(e);
                    break 'dfs;
                }
                epoch_counter = epoch_counter.wrapping_add(1).max(1);
                let can_split =
                    !force_no_split && frame.depth < max_depth && planned_leaves < max_leaves;
                let mode = if !can_split {
                    dl_rules::SplitMode::Disabled
                } else if (frame.depth >= 1 && child_eager) || probe == 0 {
                    dl_rules::SplitMode::EagerPureThenProbe
                } else {
                    dl_rules::SplitMode::ProbeOnly
                };

                // Build synthetic `DifferentIndividuals` axioms for branch disequalities.
                let mut synth_axioms: Vec<AxiomKind> = Vec::new();
                let mut branch_refs: Vec<&AxiomKind> = Vec::new();
                let mut synth_bytes = 0usize;
                if !frame.neq.is_empty() {
                    let per_synth = std::mem::size_of::<AxiomKind>()
                        .checked_add(dl_reduce::btree_set_bytes::<Individual>(2));
                    let ax_bytes = per_synth
                        .and_then(|per| frame.neq.len().checked_mul(per))
                        .and_then(|b| {
                            frame.neq.iter().try_fold(b, |acc, (a, b)| {
                                acc.checked_add(a.as_str().len())?
                                    .checked_add(b.as_str().len())
                            })
                        })
                        .and_then(|b| {
                            b.checked_add(
                                axioms
                                    .len()
                                    .checked_add(frame.neq.len())?
                                    .checked_mul(std::mem::size_of::<&AxiomKind>())?,
                            )
                        })
                        .and_then(|b| b.checked_add(frame.neq_bytes));
                    let Some(sb) = ax_bytes else {
                        exit = LoopExit::Error(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        });
                        break 'dfs;
                    };
                    let synth_admit = ledger.add_checked(sb);
                    #[cfg(test)]
                    admission_test::emit_merge_phase(admission_test::MergePhase::SplitAlloc {
                        site: admission_test::SplitAllocSite::SynthAxioms,
                        proposed: synth_admit.unwrap_or_else(|u| u),
                        admitted: synth_admit.is_ok(),
                    });
                    if let Err(used) = synth_admit {
                        ledger.note_peak(used);
                        exit = LoopExit::Error(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used,
                        });
                        break 'dfs;
                    }
                    synth_bytes = sb;
                    synth_axioms.reserve_exact(frame.neq.len());
                    for (a, b) in &frame.neq {
                        synth_axioms.push(AxiomKind::DifferentIndividuals(
                            [
                                Individual::Named(index::exact_individual(a)),
                                Individual::Named(index::exact_individual(b)),
                            ]
                            .into_iter()
                            .collect(),
                        ));
                    }
                    branch_refs.reserve_exact(axioms.len() + synth_axioms.len());
                    branch_refs.extend_from_slice(axioms);
                    for a in &synth_axioms {
                        branch_refs.push(a);
                    }
                }
                let run_axioms: &[&AxiomKind] = if synth_axioms.is_empty() {
                    axioms
                } else {
                    &branch_refs
                };

                // Hand off any pre-existing `self` state to `self.run` (which accounts for
                // `self`'s own retained tables on top of `outer_held`) on the first branch.
                if entry_state_bytes > 0 {
                    ledger.sub(entry_state_bytes);
                    entry_state_bytes = 0;
                }

                // Move branch partition into `self.merge` and set frozen bound / carried work.
                self.merge = frame.merge;
                self.merge.spent = dl_rooted_spent.saturating_add(leaf_work);
                self.merge.restarts = restarts;
                self.merge.frozen_bound = attempt_bound;
                self.merge_stop = None;
                self.split_stop = None;
                self.split_ineligible = false;
                self.split_cfg = Some(SplitBranchCfg {
                    mode,
                    probe,
                    epoch: epoch_counter,
                    forked,
                    orig_individuals: orig_ind_names_arc.clone(),
                    neq: frame.neq.clone(),
                });

                // Outer live bytes held concurrently while this branch saturates:
                // everything tracked in `ledger` EXCEPT `frame.uf_bytes` (which `self.merge`
                // accounts for inside `self.run`), plus any distinct `Arc<CanonMap>` on
                // `stack` that is NOT already owned by `self` (avoiding double-counting).
                let stack_only_canon = distinct_canon_bytes_excluding(
                    &stack,
                    &[],
                    &[
                        self.merge.shared.as_ref(),
                        self.interner.canon.as_ref(),
                        self.canon_want.as_ref(),
                    ],
                );
                let outer_held = held_bytes
                    .saturating_add(ledger.live_bytes().saturating_sub(frame.uf_bytes))
                    .saturating_add(stack_only_canon);
                let mut branch_peak = ledger.peak.load(Ordering::Relaxed);
                let r = self.run(
                    run_axioms,
                    if !forked { changes.take() } else { None },
                    outer_held,
                    limits,
                    admitted,
                    &mut branch_peak,
                    !forked,
                );
                ledger.note_peak(branch_peak);
                self.split_cfg = None;
                drop(branch_refs);
                drop(synth_axioms);
                ledger.sub(synth_bytes);
                frame.merge = std::mem::take(&mut self.merge);
                ledger.set_canon(distinct_canon_bytes(
                    &stack,
                    &[
                        frame.merge.shared.as_ref(),
                        self.interner.canon.as_ref(),
                        self.canon_want.as_ref(),
                    ],
                ));

                match r {
                    Ok((consistent, taxonomy, mut stats)) => {
                        if !forked {
                            // No-fork fast path: preserve normal `index`, `dirty = false`, and `describe`,
                            // returning directly without boxing `Stats` outside the ledger.
                            stats.dl_rooted_merged =
                                self.interner.canon.as_ref().map_or(0, |c| c.len());
                            stats.dl_rooted_spent = dl_rooted_spent;
                            stats.dl_rooted_restarts = restarts;
                            stats.dl_rooted_splits = 0;
                            stats.dl_rooted_leaves = 0;
                            stats.peak_memory =
                                stats.peak_memory.max(ledger.peak.load(Ordering::Relaxed));
                            *call_peak = (*call_peak).max(stats.peak_memory);
                            self.merge.clear();
                            self.canon_want = None;
                            return Ok((consistent, taxonomy, stats));
                        }
                        // Forked terminal leaf finished saturation!
                        leaf_work = leaf_work.saturating_add(stats.dl_work);
                        agg_stats.conclusions =
                            agg_stats.conclusions.saturating_add(stats.conclusions);
                        agg_stats.dl_contexts = agg_stats.dl_contexts.max(stats.dl_contexts);
                        agg_stats.dl_stored = agg_stats.dl_stored.max(stats.dl_stored);
                        agg_stats.clauses = agg_stats.clauses.max(stats.clauses);
                        agg_stats.outside_el = stats.outside_el;
                        agg_stats.tier = stats.tier;
                        if attempt_bound.is_none() {
                            attempt_bound = frame.merge.frozen_bound;
                        }
                        if !self.excluded.is_empty() {
                            exit = LoopExit::ExcludedAbort;
                            break 'dfs;
                        }
                        if !consistent {
                            leaves += 1;
                            if split_trace() {
                                eprintln!(
                                    "SPLIT leaf={} depth={} consistent=false work={} spent={}",
                                    leaves, frame.depth, stats.dl_work, dl_rooted_spent
                                );
                            }
                            self.reset_empty();
                            self.canon_want = None;
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitPostLeafReset {
                                    canon_want_none: self.canon_want.is_none(),
                                    leaf_inds_cap: self.leaf_individuals.capacity(),
                                    interner_bytes: self.interner.bytes(),
                                    residual_bytes: self.residual_outer_bytes(),
                                },
                            );
                            break 'branch;
                        }
                        // Complete leaf consistency check + original-signature S2 export.
                        // Include the full live saturated leaf state (including interner
                        // individual-name storage) in every export admission until
                        // `self.reset_empty()` frees it with zero replacement allocation.
                        let leaf_live = self.leaf_live_bytes_without_canon_and_merge();
                        let leaf_live_admit = ledger.add_checked(leaf_live);
                        #[cfg(test)]
                        admission_test::emit_merge_phase(admission_test::MergePhase::SplitAlloc {
                            site: admission_test::SplitAllocSite::LeafLive,
                            proposed: leaf_live_admit.unwrap_or_else(|u| u),
                            admitted: leaf_live_admit.is_ok(),
                        });
                        if let Err(used) = leaf_live_admit {
                            self.reset_empty();
                            self.canon_want = None;
                            ledger.set_canon(distinct_canon_bytes(
                                &stack,
                                &[frame.merge.shared.as_ref()],
                            ));
                            ledger.note_peak(used);
                            exit = LoopExit::Error(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used,
                            });
                            break 'dfs;
                        }
                        #[cfg(test)]
                        admission_test::emit_merge_phase(
                            admission_test::MergePhase::SplitLeafExportStart { leaves },
                        );
                        let stop = || poll(limits).is_err();
                        let budget = |d: isize| ledger.delta(d);
                        let exported = self.export_split_leaf(
                            &orig_classes,
                            &orig_individuals,
                            limits,
                            &stop,
                            &budget,
                        );
                        #[cfg(test)]
                        match &exported {
                            Ok(_) => admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitLeafExportDone { leaves },
                            ),
                            Err(Error::Memory { .. }) => admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitLeafExportRefused,
                            ),
                            Err(_) => {}
                        }
                        self.reset_empty();
                        self.canon_want = None;
                        ledger.sub(leaf_live);
                        ledger.set_canon(distinct_canon_bytes(
                            &stack,
                            &[frame.merge.shared.as_ref()],
                        ));
                        #[cfg(test)]
                        admission_test::emit_merge_phase(
                            admission_test::MergePhase::SplitPostLeafReset {
                                canon_want_none: self.canon_want.is_none(),
                                leaf_inds_cap: self.leaf_individuals.capacity(),
                                interner_bytes: self.interner.bytes(),
                                residual_bytes: self.residual_outer_bytes(),
                            },
                        );
                        match exported {
                            Ok(LeafExportResult::Inconsistent) => {
                                leaves += 1;
                                if split_trace() {
                                    eprintln!(
                                        "SPLIT leaf={} depth={} consistent=false(closed) work={}",
                                        leaves, frame.depth, stats.dl_work
                                    );
                                }
                                break 'branch;
                            }
                            Ok(LeafExportResult::Consistent {
                                vis_subs,
                                vis_top,
                                leaf_insts,
                                owned_bytes,
                            }) => {
                                leaves += 1;
                                if split_trace() {
                                    eprintln!(
                                        "SPLIT leaf={} depth={} consistent=true work={}",
                                        leaves, frame.depth, stats.dl_work
                                    );
                                }
                                if acc.consistent_leaves == 0 {
                                    acc.subs = vis_subs;
                                    acc.top = vis_top;
                                    acc.insts = leaf_insts;
                                    acc.debit = owned_bytes;
                                } else {
                                    sorted_intersect_in_place(&mut acc.top, &vis_top);
                                    for (i, ((acc_u, acc_row), (leaf_u, leaf_row))) in
                                        acc.subs.iter_mut().zip(vis_subs).enumerate()
                                    {
                                        match (*acc_u, leaf_u) {
                                            (true, true) => {
                                                acc_row.clear();
                                            }
                                            (true, false) => {
                                                *acc_u = false;
                                                *acc_row = leaf_row;
                                            }
                                            (false, true) => {}
                                            (false, false) => {
                                                sorted_intersect_in_place(acc_row, &leaf_row);
                                            }
                                        }
                                        sorted_intersect_in_place(
                                            &mut acc.insts[i],
                                            &leaf_insts[i],
                                        );
                                    }
                                    // Reconcile accumulator capacity debit after in-place meets.
                                    let new_debit = acc
                                        .subs
                                        .capacity()
                                        .saturating_mul(std::mem::size_of::<(bool, Vec<u32>)>())
                                        .saturating_add(
                                            acc.subs
                                                .iter()
                                                .map(|(_, v)| v.capacity() * 4)
                                                .sum::<usize>(),
                                        )
                                        .saturating_add(acc.top.capacity() * 4)
                                        .saturating_add(
                                            acc.insts
                                                .capacity()
                                                .saturating_mul(std::mem::size_of::<Vec<u32>>()),
                                        )
                                        .saturating_add(
                                            acc.insts
                                                .iter()
                                                .map(|v| v.capacity() * 4)
                                                .sum::<usize>(),
                                        );
                                    let total_before = acc.debit.saturating_add(owned_bytes);
                                    if total_before > new_debit {
                                        ledger.sub(total_before - new_debit);
                                    }
                                    acc.debit = new_debit;
                                }
                                acc.consistent_leaves += 1;
                                break 'branch;
                            }
                            Ok(LeafExportResult::ExcludedOrIncomplete) => {
                                exit = LoopExit::ExcludedAbort;
                                break 'dfs;
                            }
                            Err(e) => {
                                if let Error::Memory { used, .. } = e {
                                    ledger.note_peak(used);
                                }
                                exit = LoopExit::Error(e);
                                break 'dfs;
                            }
                        }
                    }
                    Err(e) => {
                        if self.split_ineligible {
                            self.split_ineligible = false;
                            exit = LoopExit::ExcludedAbort;
                            break 'dfs;
                        }
                        if let Some(stop) = self.merge_stop.take() {
                            // Item 1061 Phase 2 STOP_MERGE inside this branch!
                            ledger.note_peak(stop.peak);
                            if let Err(pe) = poll(limits) {
                                exit = LoopExit::Error(pe);
                                break 'dfs;
                            }
                            let old_inds = std::mem::take(&mut self.interner.individuals);
                            let old_inds_bytes = old_inds.exact_bytes();
                            self.reset_empty();
                            self.reduced = false;
                            self.canon_want = None;
                            frame.merge.shared = None;
                            ledger.set_canon(distinct_canon_bytes(&stack, &[]));

                            let mut admit_fn = |used: usize| -> bool { ledger.cover(used) };
                            let Some(union_base) = ledger
                                .base_with_canon(frame.uf_bytes)
                                .and_then(|b| b.checked_add(old_inds_bytes))
                                .and_then(|b| b.checked_add(stop.pairs_bytes))
                            else {
                                drop((old_inds, stop.pairs));
                                ledger.note_peak(usize::MAX);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                });
                                break 'dfs;
                            };
                            if let Err(used) = frame.merge.set_active_hub(
                                stop.active_hub,
                                union_base,
                                &mut admit_fn,
                            ) {
                                drop((old_inds, stop.pairs));
                                ledger.note_peak(used);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit =
                                    LoopExit::Error(poll(limits).err().unwrap_or(Error::Memory {
                                        limit: limits.max_memory.unwrap_or(usize::MAX),
                                        used,
                                    }));
                                break 'dfs;
                            }
                            let mut joined = false;
                            let mut invalid_id = false;
                            for (idx, &(u, v)) in stop.pairs.iter().enumerate() {
                                if idx.is_multiple_of(POLL_AXIOMS)
                                    && let Err(pe) = poll(limits)
                                {
                                    drop((old_inds, stop.pairs));
                                    exit = LoopExit::Error(pe);
                                    break 'dfs;
                                }
                                let old_slice: &[NamedIndividual] = &old_inds;
                                let (Some(nu), Some(nv)) =
                                    (old_slice.get(u as usize), old_slice.get(v as usize))
                                else {
                                    invalid_id = true;
                                    break;
                                };
                                match frame.merge.union_named(nu, nv, union_base, &mut admit_fn) {
                                    Ok(did) => joined |= did,
                                    Err(used) => {
                                        drop((old_inds, stop.pairs));
                                        ledger.note_peak(used);
                                        self.dl_worked.store(false, Ordering::Relaxed);
                                        exit = LoopExit::Error(poll(limits).err().unwrap_or(
                                            Error::Memory {
                                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                                used,
                                            },
                                        ));
                                        break 'dfs;
                                    }
                                }
                            }
                            drop((stop.pairs, old_inds));
                            let new_uf = frame.merge.uf_bytes();
                            if new_uf > frame.uf_bytes {
                                ledger.add_already_paid(new_uf - frame.uf_bytes);
                            } else {
                                ledger.sub(frame.uf_bytes - new_uf);
                            }
                            frame.uf_bytes = new_uf;
                            if invalid_id || !joined {
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: ledger.peak.load(Ordering::Relaxed),
                                });
                                break 'dfs;
                            }
                            dl_rooted_spent = stop.spent.saturating_sub(leaf_work);
                            restarts += 1;
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::MergeRestart { restarts },
                            );
                            attempt_bound.get_or_insert(stop.bound);
                            // Mandatory full disequality remapping after derived unit merge!
                            let merge_remap = remap_neq(
                                &frame.merge,
                                &mut frame.neq,
                                &mut frame.neq_bytes,
                                &ledger,
                            );
                            #[cfg(test)]
                            if let Some(proposed) = match merge_remap {
                                Ok((_, Some(p))) => Some(p),
                                Ok((_, None)) => None,
                                Err(u) => Some(u),
                            } {
                                admission_test::emit_merge_phase(
                                    admission_test::MergePhase::SplitAlloc {
                                        site: admission_test::SplitAllocSite::MergeRemap,
                                        proposed,
                                        admitted: merge_remap.is_ok(),
                                    },
                                );
                            }
                            match merge_remap {
                                Ok((true, _)) => {
                                    // Self-disequality collision proves branch inconsistent immediately!
                                    leaves += 1;
                                    if split_trace() {
                                        eprintln!(
                                            "SPLIT leaf={} depth={} inconsistent(merge-neq-collision)",
                                            leaves, frame.depth
                                        );
                                    }
                                    break 'branch;
                                }
                                Ok((false, _)) => {}
                                Err(used) => {
                                    ledger.note_peak(used);
                                    self.dl_worked.store(false, Ordering::Relaxed);
                                    exit = LoopExit::Error(Error::Memory {
                                        limit: limits.max_memory.unwrap_or(usize::MAX),
                                        used,
                                    });
                                    break 'dfs;
                                }
                            }
                            let Some(canon_base) = ledger.base_with_canon(frame.uf_bytes) else {
                                ledger.note_peak(usize::MAX);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                });
                                break 'dfs;
                            };
                            #[cfg(test)]
                            let mut merge_canon_peak = canon_base;
                            let mut merge_canon_admit = |used: usize| -> bool {
                                #[cfg(test)]
                                {
                                    merge_canon_peak = merge_canon_peak.max(used);
                                }
                                admit_fn(used)
                            };
                            let merge_canon =
                                frame
                                    .merge
                                    .build_canon(canon_base, limits, &mut merge_canon_admit);
                            #[cfg(test)]
                            match &merge_canon {
                                Ok(_) => admission_test::emit_merge_phase(
                                    admission_test::MergePhase::SplitAlloc {
                                        site: admission_test::SplitAllocSite::MergeCanon,
                                        proposed: merge_canon_peak,
                                        admitted: true,
                                    },
                                ),
                                Err(BuildCanonErr::Memory(used)) => {
                                    admission_test::emit_merge_phase(
                                        admission_test::MergePhase::SplitAlloc {
                                            site: admission_test::SplitAllocSite::MergeCanon,
                                            proposed: *used,
                                            admitted: false,
                                        },
                                    );
                                }
                                Err(BuildCanonErr::Poll(_)) => {}
                            }
                            match merge_canon {
                                Ok(_) => {
                                    ledger.set_canon(distinct_canon_bytes(
                                        &stack,
                                        &[frame.merge.shared.as_ref()],
                                    ));
                                    continue 'branch;
                                }
                                Err(BuildCanonErr::Poll(pe)) => {
                                    exit = LoopExit::Error(pe);
                                    break 'dfs;
                                }
                                Err(BuildCanonErr::Memory(used)) => {
                                    ledger.note_peak(used);
                                    self.dl_worked.store(false, Ordering::Relaxed);
                                    exit = LoopExit::Error(poll(limits).err().unwrap_or(
                                        Error::Memory {
                                            limit: limits.max_memory.unwrap_or(usize::MAX),
                                            used,
                                        },
                                    ));
                                    break 'dfs;
                                }
                            }
                        }

                        if let Some(stop) = self.split_stop.take() {
                            // Item 1075 STOP_SPLIT! Fork binary children B_eq (u == v) and B_ne (u != v).
                            ledger.note_peak(stop.peak);
                            if let Err(pe) = poll(limits) {
                                exit = LoopExit::Error(pe);
                                break 'dfs;
                            }
                            // Move the two winning individual IRIs directly out of the still-live
                            // interner table with zero heap allocation BEFORE resetting the branch.
                            let mut old_inds = std::mem::take(&mut self.interner.individuals);
                            let (mut old_items, old_hashes) = old_inds.take();
                            drop((old_inds, old_hashes));
                            let extracted = if stop.u_id != stop.v_id
                                && (stop.u_id as usize) < old_items.len()
                                && (stop.v_id as usize) < old_items.len()
                            {
                                let u = std::mem::replace(
                                    &mut old_items[stop.u_id as usize],
                                    NamedIndividual::new(String::new()),
                                );
                                let v = std::mem::replace(
                                    &mut old_items[stop.v_id as usize],
                                    NamedIndividual::new(String::new()),
                                );
                                Some((u, v))
                            } else {
                                None
                            };
                            drop(old_items);
                            self.reset_empty();
                            self.reduced = false;
                            self.canon_want = None;
                            ledger.set_canon(distinct_canon_bytes(
                                &stack,
                                &[frame.merge.shared.as_ref()],
                            ));
                            let Some((stop_u, stop_v)) = extracted else {
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: ledger.peak.load(Ordering::Relaxed),
                                });
                                break 'dfs;
                            };
                            dl_rooted_spent = stop.spent.saturating_sub(leaf_work);
                            attempt_bound.get_or_insert(stop.bound);

                            let stop_bytes =
                                stop_u.as_str().len().saturating_add(stop_v.as_str().len());
                            let stop_admit = ledger.add_checked(stop_bytes);
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitAlloc {
                                    site: admission_test::SplitAllocSite::StopHandoff,
                                    proposed: stop_admit.unwrap_or_else(|u| u),
                                    admitted: stop_admit.is_ok(),
                                },
                            );
                            if let Err(used) = stop_admit {
                                drop((stop_u, stop_v));
                                ledger.note_peak(used);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used,
                                });
                                break 'dfs;
                            }

                            // Inspect canonical representatives by reference (zero allocation)
                            // before checking termination or admitting child structures.
                            let (lo_len, hi_len, already_decided) = {
                                let ru = frame.merge.rep_of_named_ref(&stop_u);
                                let rv = frame.merge.rep_of_named_ref(&stop_v);
                                let (lo, hi) = if ru.as_str() <= rv.as_str() {
                                    (ru, rv)
                                } else {
                                    (rv, ru)
                                };
                                let already_neq = frame
                                    .neq
                                    .binary_search_by(|p| {
                                        p.0.as_str()
                                            .cmp(lo.as_str())
                                            .then_with(|| p.1.as_str().cmp(hi.as_str()))
                                    })
                                    .is_ok();
                                (
                                    lo.as_str().len(),
                                    hi.as_str().len(),
                                    lo == hi
                                        || already_neq
                                        || frame.depth >= max_depth
                                        || planned_leaves >= max_leaves,
                                )
                            };
                            if already_decided {
                                // Candidate is already decided or caps reached; finish this branch
                                // without further splitting.
                                drop((stop_u, stop_v));
                                ledger.sub(stop_bytes);
                                force_no_split = true;
                                continue 'branch;
                            }

                            forked = true;
                            splits += 1;
                            planned_leaves += 1;
                            let next_depth = frame.depth + 1;
                            #[cfg(test)]
                            {
                                let ru = frame.merge.rep_of_named_ref(&stop_u);
                                let rv = frame.merge.rep_of_named_ref(&stop_v);
                                let (lo, hi) = if ru.as_str() <= rv.as_str() {
                                    (ru, rv)
                                } else {
                                    (rv, ru)
                                };
                                admission_test::emit_split_pair(
                                    splits,
                                    next_depth,
                                    stop.prio,
                                    lo.as_str(),
                                    hi.as_str(),
                                );
                                admission_test::emit_merge_phase(
                                    admission_test::MergePhase::SplitFork {
                                        splits,
                                        depth: next_depth,
                                        prio: stop.prio,
                                    },
                                );
                            }
                            if split_trace() {
                                let ru = frame.merge.rep_of_named_ref(&stop_u);
                                let rv = frame.merge.rep_of_named_ref(&stop_v);
                                let (lo, hi) = if ru.as_str() <= rv.as_str() {
                                    (ru, rv)
                                } else {
                                    (rv, ru)
                                };
                                eprintln!(
                                    "SPLIT fork={} depth={}->{next_depth} prio={} pair=({}, {}) spent={} bound={}",
                                    splits,
                                    frame.depth,
                                    stop.prio,
                                    lo.as_str(),
                                    hi.as_str(),
                                    dl_rooted_spent,
                                    stop.bound
                                );
                            }

                            // Pre-admit DFS stack growth for both child pushes before
                            // allocating child state or consuming `frame`.
                            let fork_stack_admit =
                                ensure_stack_cap(&mut stack, &mut stack_debit, 2, &ledger);
                            #[cfg(test)]
                            if let Some(proposed) = match fork_stack_admit {
                                Ok(Some(p)) => Some(p),
                                Ok(None) => None,
                                Err(u) => Some(u),
                            } {
                                admission_test::emit_merge_phase(
                                    admission_test::MergePhase::SplitAlloc {
                                        site: admission_test::SplitAllocSite::ForkStack,
                                        proposed,
                                        admitted: fork_stack_admit.is_ok(),
                                    },
                                );
                            }
                            if let Err(used) = fork_stack_admit {
                                drop((stop_u, stop_v));
                                ledger.sub(stop_bytes);
                                ledger.note_peak(used);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used,
                                });
                                break 'dfs;
                            }

                            let admit_fn = |used: usize| -> bool { ledger.cover(used) };

                            // Clone parent Merge for B_eq before moving parent into B_ne.
                            let Some(clone_base) = ledger.base_with_canon(0) else {
                                drop((stop_u, stop_v));
                                ledger.sub(stop_bytes);
                                ledger.note_peak(usize::MAX);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                });
                                break 'dfs;
                            };
                            #[cfg(test)]
                            let mut clone_peak = clone_base;
                            let mut clone_admit = |used: usize| -> bool {
                                #[cfg(test)]
                                {
                                    clone_peak = clone_peak.max(used);
                                }
                                admit_fn(used)
                            };
                            let cloned_merge =
                                frame.merge.clone_admitted(clone_base, &mut clone_admit);
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitAlloc {
                                    site: admission_test::SplitAllocSite::ForkMergeClone,
                                    proposed: match &cloned_merge {
                                        Ok(_) => clone_peak,
                                        Err(used) => *used,
                                    },
                                    admitted: cloned_merge.is_ok(),
                                },
                            );
                            let mut eq_merge = match cloned_merge {
                                Ok(m) => m,
                                Err(used) => {
                                    drop((stop_u, stop_v));
                                    ledger.sub(stop_bytes);
                                    ledger.note_peak(used);
                                    self.dl_worked.store(false, Ordering::Relaxed);
                                    exit = LoopExit::Error(poll(limits).err().unwrap_or(
                                        Error::Memory {
                                            limit: limits.max_memory.unwrap_or(usize::MAX),
                                            used,
                                        },
                                    ));
                                    break 'dfs;
                                }
                            };
                            let mut eq_uf_bytes = eq_merge.uf_bytes();
                            ledger.add_already_paid(eq_uf_bytes);

                            // Prepare right branch B_ne (u != v): pre-admit the full cloned Vec
                            // plus new pair BEFORE cloning `frame.neq` or the selected IRIs.
                            let ne_cap = frame.neq.len().checked_add(1);
                            let ne_add = ne_cap
                                .and_then(|c| {
                                    c.checked_mul(std::mem::size_of::<(
                                        NamedIndividual,
                                        NamedIndividual,
                                    )>())
                                })
                                .and_then(|b| {
                                    frame.neq.iter().try_fold(b, |acc, (a, x)| {
                                        acc.checked_add(a.as_str().len())?
                                            .checked_add(x.as_str().len())
                                    })
                                })
                                .and_then(|b| b.checked_add(lo_len))
                                .and_then(|b| b.checked_add(hi_len));
                            let Some(ne_add) = ne_add else {
                                drop((stop_u, stop_v));
                                ledger.sub(stop_bytes.saturating_add(eq_uf_bytes));
                                ledger.note_peak(usize::MAX);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                });
                                break 'dfs;
                            };
                            let ne_admit = ledger.add_checked(ne_add);
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitAlloc {
                                    site: admission_test::SplitAllocSite::ForkNeq,
                                    proposed: ne_admit.unwrap_or_else(|u| u),
                                    admitted: ne_admit.is_ok(),
                                },
                            );
                            if let Err(used) = ne_admit {
                                drop((stop_u, stop_v));
                                ledger.sub(stop_bytes.saturating_add(eq_uf_bytes));
                                ledger.note_peak(used);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used,
                                });
                                break 'dfs;
                            }
                            let mut ne_neq = Vec::with_capacity(frame.neq.len() + 1);
                            for (a, b) in &frame.neq {
                                ne_neq
                                    .push((index::exact_individual(a), index::exact_individual(b)));
                            }
                            {
                                let ru = frame.merge.rep_of_named_ref(&stop_u);
                                let rv = frame.merge.rep_of_named_ref(&stop_v);
                                let (lo, hi) = if ru.as_str() <= rv.as_str() {
                                    (ru, rv)
                                } else {
                                    (rv, ru)
                                };
                                ne_neq.push((
                                    index::exact_individual(lo),
                                    index::exact_individual(hi),
                                ));
                            }
                            ne_neq.sort_unstable_by(|x, y| {
                                x.0.as_str()
                                    .cmp(y.0.as_str())
                                    .then_with(|| x.1.as_str().cmp(y.1.as_str()))
                            });
                            ne_neq.dedup();
                            let ne_neq_bytes = SplitFrame::neq_owned_bytes(&ne_neq);
                            if ne_add > ne_neq_bytes {
                                ledger.sub(ne_add - ne_neq_bytes);
                            } else if ne_neq_bytes > ne_add {
                                ledger.add_already_paid(ne_neq_bytes - ne_add);
                            }

                            // Move parent's `merge` (keeping `frame.uf_bytes` in ledger) to B_ne,
                            // and parent's `neq` (keeping `frame.neq_bytes` in ledger) to B_eq.
                            let mut eq_neq = std::mem::take(&mut frame.neq);
                            let mut eq_neq_bytes = frame.neq_bytes;
                            frame_consumed = true;
                            stack.push(SplitFrame {
                                depth: next_depth,
                                merge: std::mem::take(&mut frame.merge),
                                uf_bytes: frame.uf_bytes,
                                neq: ne_neq,
                                neq_bytes: ne_neq_bytes,
                            });

                            // Prepare left branch B_eq (u == v): union + mandatory full disequality remap.
                            let Some(eq_base) = ledger.base_with_canon(eq_uf_bytes) else {
                                drop((stop_u, stop_v));
                                ledger.sub(
                                    stop_bytes
                                        .saturating_add(eq_uf_bytes)
                                        .saturating_add(eq_neq_bytes),
                                );
                                ledger.note_peak(usize::MAX);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                });
                                break 'dfs;
                            };
                            #[cfg(test)]
                            let mut union_peak = eq_base;
                            let mut union_admit = |used: usize| -> bool {
                                #[cfg(test)]
                                {
                                    union_peak = union_peak.max(used);
                                }
                                admit_fn(used)
                            };
                            let union_res =
                                eq_merge.union_named(&stop_u, &stop_v, eq_base, &mut union_admit);
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::SplitAlloc {
                                    site: admission_test::SplitAllocSite::ForkEqUnion,
                                    proposed: match &union_res {
                                        Ok(_) => union_peak,
                                        Err(used) => *used,
                                    },
                                    admitted: union_res.is_ok(),
                                },
                            );
                            match union_res {
                                Ok(_) => {}
                                Err(used) => {
                                    drop((stop_u, stop_v));
                                    ledger.sub(
                                        stop_bytes
                                            .saturating_add(eq_uf_bytes)
                                            .saturating_add(eq_neq_bytes),
                                    );
                                    ledger.note_peak(used);
                                    self.dl_worked.store(false, Ordering::Relaxed);
                                    exit = LoopExit::Error(poll(limits).err().unwrap_or(
                                        Error::Memory {
                                            limit: limits.max_memory.unwrap_or(usize::MAX),
                                            used,
                                        },
                                    ));
                                    break 'dfs;
                                }
                            }
                            drop((stop_u, stop_v));
                            ledger.sub(stop_bytes);
                            let new_eq_uf = eq_merge.uf_bytes();
                            if new_eq_uf > eq_uf_bytes {
                                ledger.add_already_paid(new_eq_uf - eq_uf_bytes);
                            } else {
                                ledger.sub(eq_uf_bytes - new_eq_uf);
                            }
                            eq_uf_bytes = new_eq_uf;
                            ledger.set_canon(distinct_canon_bytes(
                                &stack,
                                &[eq_merge.shared.as_ref()],
                            ));
                            let eq_remap =
                                remap_neq(&eq_merge, &mut eq_neq, &mut eq_neq_bytes, &ledger);
                            #[cfg(test)]
                            if let Some(proposed) = match eq_remap {
                                Ok((_, Some(p))) => Some(p),
                                Ok((_, None)) => None,
                                Err(u) => Some(u),
                            } {
                                admission_test::emit_merge_phase(
                                    admission_test::MergePhase::SplitAlloc {
                                        site: admission_test::SplitAllocSite::ForkEqRemap,
                                        proposed,
                                        admitted: eq_remap.is_ok(),
                                    },
                                );
                            }
                            match eq_remap {
                                Ok((true, _)) => {
                                    // B_eq immediately inconsistent via disequality collision!
                                    leaves += 1;
                                    ledger.sub(eq_uf_bytes.saturating_add(eq_neq_bytes));
                                    drop((eq_merge, eq_neq));
                                    ledger.set_canon(distinct_canon_bytes(&stack, &[]));
                                    break 'branch;
                                }
                                Ok((false, _)) => {}
                                Err(used) => {
                                    ledger.sub(eq_uf_bytes.saturating_add(eq_neq_bytes));
                                    ledger.note_peak(used);
                                    self.dl_worked.store(false, Ordering::Relaxed);
                                    exit = LoopExit::Error(Error::Memory {
                                        limit: limits.max_memory.unwrap_or(usize::MAX),
                                        used,
                                    });
                                    break 'dfs;
                                }
                            }
                            let Some(canon_base) = ledger.base_with_canon(eq_uf_bytes) else {
                                ledger.sub(eq_uf_bytes.saturating_add(eq_neq_bytes));
                                ledger.note_peak(usize::MAX);
                                self.dl_worked.store(false, Ordering::Relaxed);
                                exit = LoopExit::Error(Error::Memory {
                                    limit: limits.max_memory.unwrap_or(usize::MAX),
                                    used: usize::MAX,
                                });
                                break 'dfs;
                            };
                            #[cfg(test)]
                            let mut eq_canon_peak = canon_base;
                            let mut eq_canon_admit = |used: usize| -> bool {
                                #[cfg(test)]
                                {
                                    eq_canon_peak = eq_canon_peak.max(used);
                                }
                                admit_fn(used)
                            };
                            let eq_canon =
                                eq_merge.build_canon(canon_base, limits, &mut eq_canon_admit);
                            #[cfg(test)]
                            match &eq_canon {
                                Ok(_) => admission_test::emit_merge_phase(
                                    admission_test::MergePhase::SplitAlloc {
                                        site: admission_test::SplitAllocSite::ForkEqCanon,
                                        proposed: eq_canon_peak,
                                        admitted: true,
                                    },
                                ),
                                Err(BuildCanonErr::Memory(used)) => {
                                    admission_test::emit_merge_phase(
                                        admission_test::MergePhase::SplitAlloc {
                                            site: admission_test::SplitAllocSite::ForkEqCanon,
                                            proposed: *used,
                                            admitted: false,
                                        },
                                    );
                                }
                                Err(BuildCanonErr::Poll(_)) => {}
                            }
                            match eq_canon {
                                Ok(_) => {}
                                Err(BuildCanonErr::Poll(pe)) => {
                                    ledger.sub(eq_uf_bytes.saturating_add(eq_neq_bytes));
                                    exit = LoopExit::Error(pe);
                                    break 'dfs;
                                }
                                Err(BuildCanonErr::Memory(used)) => {
                                    ledger.sub(eq_uf_bytes.saturating_add(eq_neq_bytes));
                                    ledger.note_peak(used);
                                    self.dl_worked.store(false, Ordering::Relaxed);
                                    exit = LoopExit::Error(poll(limits).err().unwrap_or(
                                        Error::Memory {
                                            limit: limits.max_memory.unwrap_or(usize::MAX),
                                            used,
                                        },
                                    ));
                                    break 'dfs;
                                }
                            }
                            stack.push(SplitFrame {
                                depth: next_depth,
                                merge: eq_merge,
                                uf_bytes: eq_uf_bytes,
                                neq: eq_neq,
                                neq_bytes: eq_neq_bytes,
                            });
                            ledger.set_canon(distinct_canon_bytes(&stack, &[]));
                            break 'branch;
                        }

                        // Ordinary abort (`WorkExhausted`, `Declined`, `Memory`, `Cancelled`, `Timeout`).
                        if let Error::Memory { used, .. } = e {
                            ledger.note_peak(used);
                        }
                        exit = LoopExit::Error(e);
                        break 'dfs;
                    }
                }
            }
            if !frame_consumed {
                let uf_b = frame.uf_bytes;
                let neq_b = frame.neq_bytes;
                drop(frame);
                ledger.sub(uf_b.saturating_add(neq_b));
                ledger.set_canon(distinct_canon_bytes(&stack, &[]));
            }
        }

        // Handle non-completion exits cleanly before assembling `SplitAnswer`.
        match exit {
            LoopExit::Error(e) => {
                *call_peak = (*call_peak).max(ledger.peak.load(Ordering::Relaxed));
                drop((
                    stack,
                    acc,
                    orig_classes,
                    orig_individuals,
                    orig_ind_names_arc,
                ));
                self.merge.clear();
                self.canon_want = None;
                self.split_cfg = None;
                self.split_stop = None;
                self.reset_empty();
                self.invalidate_for_error();
                return Err(e);
            }
            LoopExit::ExcludedAbort => {
                let spent_so_far = dl_rooted_spent.saturating_add(leaf_work);
                let bound = attempt_bound.unwrap_or_else(|| {
                    limits
                        .dl_work
                        .unwrap_or_else(|| {
                            rooted_work_bound(orig_classes.len(), rooted_bound_per_class())
                        })
                        .max(1)
                });
                *call_peak = (*call_peak).max(ledger.peak.load(Ordering::Relaxed));
                drop((
                    stack,
                    acc,
                    orig_classes,
                    orig_individuals,
                    orig_ind_names_arc,
                ));
                self.merge.clear();
                self.canon_want = None;
                self.split_cfg = None;
                self.split_stop = None;
                self.reset_empty();
                self.invalidate_for_error();
                let remaining = bound.saturating_sub(spent_so_far);
                #[cfg(test)]
                admission_test::emit_merge_phase(admission_test::MergePhase::SplitExcludedAbort {
                    spent: spent_so_far,
                    bound,
                    remaining,
                });
                if remaining == 0 {
                    self.dl_worked.store(true, Ordering::Relaxed);
                    return Err(Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: *call_peak,
                    });
                }
                // Run ONE clean disabled-split retry on original axioms carrying `spent_so_far`
                // against the exact same frozen `attempt_bound`.
                let disabled = Limits {
                    rooted_eq_split: false,
                    ..limits.clone()
                };
                if disabled.rooted
                    && disabled.rooted_merge
                    && !disabled.el_only
                    && !self.merge_disabled
                {
                    self.collect_same(axioms, held_bytes, &disabled, admitted, call_peak)?;
                }
                return self.run_merging_with_carry(
                    axioms,
                    None,
                    held_bytes,
                    &disabled,
                    admitted,
                    call_peak,
                    with_taxonomy,
                    spent_so_far,
                    Some(bound),
                );
            }
            LoopExit::Done => {
                drop(stack);
                ledger.sub(stack_debit);
            }
        }

        // All DFS frames completed! Assemble final Taxonomy + SplitAnswer simultaneously.
        let threads = threads_of(limits);
        let stop = || poll(limits).is_err();
        let budget = |d: isize| ledger.delta(d);
        let tax_start = Instant::now();
        let (consistent, taxonomy, tax_bytes, node_insts) = if acc.consistent_leaves == 0 {
            let (tax, bytes) = taxonomy::build_inconsistent(&orig_classes, threads, &stop, &budget)
                .map_err(|h| {
                    self.invalidate_for_error();
                    match h {
                        taxonomy::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                        taxonomy::Halt::Memory(used) => {
                            *call_peak = (*call_peak).max(used);
                            Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used,
                            }
                        }
                    }
                })?;
            (false, tax, bytes, Vec::new())
        } else {
            let (tax, bytes) = taxonomy::build_from_rows(
                &orig_classes,
                &acc.subs,
                &acc.top,
                threads,
                &stop,
                &budget,
            )
            .map_err(|h| {
                self.invalidate_for_error();
                match h {
                    taxonomy::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                    taxonomy::Halt::Memory(used) => {
                        *call_peak = (*call_peak).max(used);
                        Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used,
                        }
                    }
                }
            })?;
            let ni_hdr = tax
                .nodes()
                .len()
                .checked_mul(std::mem::size_of::<Vec<u32>>())
                .ok_or_else(|| {
                    self.invalidate_for_error();
                    Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: usize::MAX,
                    }
                })?;
            if let Err(used) = ledger.add(ni_hdr) {
                *call_peak = (*call_peak).max(used);
                self.invalidate_for_error();
                return Err(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used,
                });
            }
            let mut node_insts: Vec<Vec<u32>> = Vec::with_capacity(tax.nodes().len());
            node_insts.resize_with(tax.nodes().len(), Vec::new);
            for (ni, node) in tax.nodes().iter().enumerate().skip(2) {
                if let Some(first) = node.classes.first()
                    && let Ok(vis_c) =
                        orig_classes.binary_search_by(|c| c.as_str().cmp(first.as_str()))
                {
                    node_insts[ni] = std::mem::take(&mut acc.insts[vis_c]);
                }
            }
            (true, tax, bytes, node_insts)
        };

        // Pre-admit BOTH returned `Classification.taxonomy` and retained `SplitAnswer`
        // simultaneously BEFORE allocating `retained_ind_names` or cloning `taxonomy`.
        // Moving `NamedIndividual` values out of `orig_individuals` transfers their
        // already-admitted string buffers with zero duplicate string allocation.
        let retained_ind_vec = orig_individuals
            .len()
            .checked_mul(std::mem::size_of::<NamedIndividual>());
        let retained_split_extra = std::mem::size_of::<SplitAnswer>()
            .checked_add(tax_bytes)
            .and_then(|b| b.checked_add(retained_ind_vec?))
            .ok_or_else(|| {
                ledger.note_peak(usize::MAX);
                *call_peak = usize::MAX;
                self.invalidate_for_error();
                Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                }
            })?;
        if let Err(used) = ledger.add(retained_split_extra) {
            *call_peak = (*call_peak)
                .max(ledger.peak.load(Ordering::Relaxed))
                .max(used);
            self.invalidate_for_error();
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            });
        }
        let mut retained_ind_names: Vec<NamedIndividual> =
            Vec::with_capacity(orig_individuals.len());
        for (n, _) in orig_individuals {
            retained_ind_names.push(n);
        }
        let retained_tax = taxonomy.clone();
        let split_answer = Box::new(SplitAnswer {
            consistent,
            taxonomy: retained_tax,
            individuals: retained_ind_names,
            node_insts,
        });
        drop(acc);
        drop((orig_classes, orig_ind_names_arc));

        self.index = None;
        self.interner = Interner::default();
        self.ctx = Vec::new();
        self.disjoint = Vec::new();
        self.worlds = FxHashMap::default();
        self.dynamic = None;
        self.dl_run = None;
        self.merge.clear();
        self.canon_want = None;
        self.active_hub = None;
        self.excluded = Vec::new();
        self.leaf_individuals = Vec::new();
        self.reduced = false;
        self.dirty = false;
        self.consistent = consistent;
        let split_bytes = split_answer.owned_bytes();
        self.split_answer = Some(split_answer);

        let final_peak = ledger.peak.load(Ordering::Relaxed).max(
            held_bytes
                .saturating_add(split_bytes)
                .saturating_add(tax_bytes),
        );
        *call_peak = (*call_peak).max(final_peak);
        agg_stats.dl_work = leaf_work;
        agg_stats.dl_rooted_spent = dl_rooted_spent;
        agg_stats.dl_rooted_restarts = restarts;
        agg_stats.dl_rooted_splits = splits;
        agg_stats.dl_rooted_leaves = leaves;
        agg_stats.taxonomy = tax_start.elapsed();
        agg_stats.saturation = split_start.elapsed().saturating_sub(agg_stats.taxonomy);
        agg_stats.taxonomy_memory = tax_bytes;
        agg_stats.memory = held_bytes
            .saturating_add(split_bytes)
            .saturating_add(tax_bytes)
            .saturating_add(self.residual_outer_bytes());
        agg_stats.peak_memory = final_peak.max(agg_stats.memory);
        poll(limits).inspect_err(|_| self.invalidate_for_error())?;
        Ok((consistent, Some(taxonomy), agg_stats))
    }

    /// Exports one completed forked leaf into visible original-signature closed subsumer
    /// rows and public instance lists, after validating complete consistency across ALL
    /// real nominals (including anonymous witnesses and reduction-generated hubs) and
    /// closing over ALL met classes (including hidden reduction-generated classes).
    fn export_split_leaf(
        &self,
        orig_classes: &[Class],
        orig_individuals: &[(NamedIndividual, bool)],
        limits: &Limits,
        stop: &(dyn Fn() -> bool + Sync),
        budget: &(dyn Fn(isize) -> Result<(), usize> + Sync),
    ) -> Result<LeafExportResult, Error> {
        let Some(ix) = self.index.as_ref() else {
            return Ok(LeafExportResult::ExcludedOrIncomplete);
        };
        let n = orig_classes.len();
        let admit_bytes = |bytes: usize| -> Result<(), Error> {
            if bytes == 0 {
                return Ok(());
            }
            let d = isize::try_from(bytes).map_err(|_| Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
            budget(d).map_err(|used| Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            })
        };
        let credit_bytes = |bytes: usize| {
            if bytes > 0
                && let Ok(d) = isize::try_from(bytes)
            {
                let _ = budget(-d);
            }
        };
        struct ScratchRelease<'b> {
            budget: &'b (dyn Fn(isize) -> Result<(), usize> + Sync),
            bytes: usize,
        }
        impl Drop for ScratchRelease<'_> {
            fn drop(&mut self) {
                if self.bytes > 0
                    && let Ok(d) = isize::try_from(self.bytes)
                {
                    let _ = (self.budget)(-d);
                }
            }
        }
        // Owns `all_subs` and always refunds its exact live vector capacities on drop,
        // even if `close_rows_admitted` fails mid-way through replacing slots.
        struct SubsGuard<'b> {
            subs: Vec<(bool, Vec<u32>)>,
            hdr_bytes: usize,
            budget: &'b (dyn Fn(isize) -> Result<(), usize> + Sync),
        }
        impl Drop for SubsGuard<'_> {
            fn drop(&mut self) {
                let vec_bytes: usize = self
                    .subs
                    .iter()
                    .map(|(_, v)| v.capacity().saturating_mul(std::mem::size_of::<u32>()))
                    .sum();
                let total = self.hdr_bytes.saturating_add(vec_bytes);
                self.subs.clear();
                self.subs.shrink_to_fit();
                if total > 0
                    && let Ok(d) = isize::try_from(total)
                {
                    let _ = (self.budget)(-d);
                }
            }
        }

        // Collect ALL met `Node::Class` expressions in `self.interner`, explicitly
        // including hidden reduction-generated `dl_reduce::PREFIX` classes whose `met`
        // bit was cleared by `Interner::hide_prefix` during `Saturation::build`.
        let include_class = |id: usize, m: bool| -> bool {
            m || (self.reduced
                && self.interner.classes[id]
                    .as_str()
                    .starts_with(dl_reduce::PREFIX))
        };
        let m_all_count = self
            .interner
            .met
            .iter()
            .enumerate()
            .filter(|&(id, &m)| {
                include_class(id, m) && self.interner.get(&Node::Class(id as u32)).is_some()
            })
            .count();
        if m_all_count > taxonomy::MAX_CLASS_COUNT {
            return Ok(LeafExportResult::ExcludedOrIncomplete);
        }
        let map_bytes = m_all_count
            .checked_mul(2 * std::mem::size_of::<u32>())
            .and_then(|b| b.checked_add(n.checked_mul(std::mem::size_of::<u32>())?))
            .and_then(|b| {
                b.checked_add(
                    self.interner
                        .len()
                        .checked_mul(std::mem::size_of::<u32>())?,
                )
            })
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        let subs_hdr_bytes = m_all_count
            .checked_mul(std::mem::size_of::<(bool, Vec<u32>)>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(map_bytes)?;
        let mut scratch_rel = ScratchRelease {
            budget,
            bytes: map_bytes,
        };
        let mut met_ex: Vec<Ex> = Vec::with_capacity(m_all_count);
        let mut all_to_vis: Vec<u32> = Vec::with_capacity(m_all_count);
        let mut vis_to_all: Vec<u32> = vec![u32::MAX; n];
        let mut ex_to_all: Vec<u32> = vec![u32::MAX; self.interner.len()];
        for (id, &m) in self.interner.met.iter().enumerate() {
            if id.is_multiple_of(POLL_AXIOMS) {
                poll(limits)?;
            }
            if !include_class(id, m) {
                continue;
            }
            let Some(e) = self.interner.get(&Node::Class(id as u32)) else {
                continue;
            };
            let idx = met_ex.len() as u32;
            met_ex.push(e);
            ex_to_all[e as usize] = idx;
            let cls = &self.interner.classes[id];
            let vis = if self.reduced && cls.as_str().starts_with(dl_reduce::PREFIX) {
                u32::MAX
            } else {
                orig_classes
                    .binary_search_by(|c| c.as_str().cmp(cls.as_str()))
                    .ok()
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(u32::MAX)
            };
            all_to_vis.push(vis);
            if vis != u32::MAX {
                vis_to_all[vis as usize] = idx;
            }
        }

        // Two-pass count -> pre-admit -> fill raw subsumers for 0..m_all and TOP.
        admit_bytes(subs_hdr_bytes)?;
        let mut subs_guard = SubsGuard {
            subs: Vec::with_capacity(m_all_count),
            hdr_bytes: subs_hdr_bytes,
            budget,
        };
        for (idx, &e) in met_ex.iter().enumerate() {
            if idx.is_multiple_of(POLL_AXIOMS) {
                poll(limits)?;
            }
            let mut count = 0usize;
            let mut unsat = false;
            if let Some(u) = self.for_each_raw_subsumer(e, |s| {
                if ex_to_all[s as usize] != u32::MAX {
                    count += 1;
                }
            }) {
                unsat = u;
            } else {
                count = 1;
            }
            let row_bytes = count
                .checked_mul(std::mem::size_of::<u32>())
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
            admit_bytes(row_bytes)?;
            let mut row_rel = ScratchRelease {
                budget,
                bytes: row_bytes,
            };
            let mut row = Vec::with_capacity(count);
            let actual_b = row
                .capacity()
                .checked_mul(std::mem::size_of::<u32>())
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
            if actual_b > row_bytes {
                admit_bytes(actual_b - row_bytes)?;
                row_rel.bytes = actual_b;
            } else if row_bytes > actual_b {
                credit_bytes(row_bytes - actual_b);
                row_rel.bytes = actual_b;
            }
            if self
                .for_each_raw_subsumer(e, |s| {
                    let a = ex_to_all[s as usize];
                    if a != u32::MAX {
                        row.push(a);
                    }
                })
                .is_none()
            {
                row.push(idx as u32);
            }
            row.sort_unstable();
            row.dedup();
            // Transfer ownership of `row`'s debit to `subs_guard`.
            row_rel.bytes = 0;
            subs_guard.subs.push((unsat, row));
        }

        let mut raw_top_count = 0usize;
        let _ = self.for_each_raw_subsumer(TOP, |s| {
            if ex_to_all[s as usize] != u32::MAX {
                raw_top_count += 1;
            }
        });
        let raw_top_bytes = raw_top_count
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(raw_top_bytes)?;
        scratch_rel.bytes = scratch_rel.bytes.saturating_add(raw_top_bytes);
        let mut raw_top = Vec::with_capacity(raw_top_count);
        let actual_raw_top = raw_top
            .capacity()
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        if actual_raw_top > raw_top_bytes {
            admit_bytes(actual_raw_top - raw_top_bytes)?;
            scratch_rel.bytes = scratch_rel
                .bytes
                .saturating_add(actual_raw_top - raw_top_bytes);
        } else if raw_top_bytes > actual_raw_top {
            credit_bytes(raw_top_bytes - actual_raw_top);
            scratch_rel.bytes = scratch_rel
                .bytes
                .saturating_sub(raw_top_bytes - actual_raw_top);
        }
        let _ = self.for_each_raw_subsumer(TOP, |s| {
            let a = ex_to_all[s as usize];
            if a != u32::MAX {
                raw_top.push(a);
            }
        });
        raw_top.sort_unstable();
        raw_top.dedup();

        let closed_top =
            taxonomy::close_rows_admitted(&mut subs_guard.subs, &raw_top, stop, budget).map_err(
                |h| match h {
                    taxonomy::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                    taxonomy::Halt::Memory(used) => Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used,
                    },
                },
            )?;
        let closed_top_bytes = closed_top
            .capacity()
            .saturating_mul(std::mem::size_of::<u32>());
        scratch_rel.bytes = scratch_rel.bytes.saturating_add(closed_top_bytes);

        if closed_top.iter().any(|&c| subs_guard.subs[c as usize].0) {
            return Ok(LeafExportResult::Inconsistent);
        }

        // Validate closed consistency across ALL real nominals in the leaf (including
        // scoped anonymous witnesses and any active reduction-generated hub), using
        // strictly pre-admitted sorted `Vec<Ex>` inventories.
        let req_count = self
            .leaf_individuals
            .len()
            .checked_add(ix.nominals.len())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        let req_bytes = req_count
            .checked_mul(std::mem::size_of::<Ex>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(req_bytes)?;
        scratch_rel.bytes = scratch_rel.bytes.saturating_add(req_bytes);
        let mut required_noms: Vec<Ex> = Vec::with_capacity(req_count);
        required_noms.extend_from_slice(&self.leaf_individuals);
        required_noms.extend_from_slice(&ix.nominals);
        required_noms.sort_unstable();
        required_noms.dedup();

        let nom_nodes_count = self
            .interner
            .nodes
            .iter()
            .filter(|node| matches!(node, Node::Nominal(_)))
            .count();
        let all_noms_count =
            nom_nodes_count
                .checked_add(required_noms.len())
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
        let all_noms_bytes = all_noms_count
            .checked_mul(std::mem::size_of::<Ex>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(all_noms_bytes)?;
        scratch_rel.bytes = scratch_rel.bytes.saturating_add(all_noms_bytes);
        let mut all_noms: Vec<Ex> = Vec::with_capacity(all_noms_count);
        for (e, node) in self.interner.nodes.iter().enumerate() {
            if matches!(node, Node::Nominal(_)) {
                all_noms.push(e as Ex);
            }
        }
        all_noms.extend_from_slice(&required_noms);
        all_noms.sort_unstable();
        all_noms.dedup();
        for (ni, &e) in all_noms.iter().enumerate() {
            if ni.is_multiple_of(POLL_AXIOMS) {
                poll(limits)?;
            }
            let mut reaches_unsat = false;
            let ctx_res = self.for_each_raw_subsumer(e, |s| {
                if s == index::BOTTOM {
                    reaches_unsat = true;
                } else if let Some(&all_c) = ex_to_all.get(s as usize)
                    && all_c != u32::MAX
                    && subs_guard.subs[all_c as usize].0
                {
                    reaches_unsat = true;
                }
            });
            match ctx_res {
                None => {
                    if required_noms.binary_search(&e).is_ok() {
                        return Ok(LeafExportResult::ExcludedOrIncomplete);
                    }
                }
                Some(nom_unsat) => {
                    if nom_unsat || reaches_unsat {
                        return Ok(LeafExportResult::Inconsistent);
                    }
                }
            }
        }

        // Project `closed_top` to visible `0..n` indices.
        let vis_top_count = closed_top
            .iter()
            .filter(|&&c| all_to_vis[c as usize] != u32::MAX)
            .count();
        let vis_top_bytes = vis_top_count
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(vis_top_bytes)?;
        let mut out_rel = ScratchRelease {
            budget,
            bytes: vis_top_bytes,
        };
        let mut vis_top = Vec::with_capacity(vis_top_count);
        let actual_vt = vis_top
            .capacity()
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        if actual_vt > vis_top_bytes {
            admit_bytes(actual_vt - vis_top_bytes)?;
            out_rel.bytes = out_rel.bytes.saturating_add(actual_vt - vis_top_bytes);
        } else if vis_top_bytes > actual_vt {
            credit_bytes(vis_top_bytes - actual_vt);
            out_rel.bytes = out_rel.bytes.saturating_sub(vis_top_bytes - actual_vt);
        }
        for &c in &closed_top {
            let v = all_to_vis[c as usize];
            if v != u32::MAX {
                vis_top.push(v);
            }
        }
        vis_top.sort_unstable();
        vis_top.dedup();

        // Project `subs_guard.subs` to visible `vis_subs[0..n]`.
        let vis_subs_hdr = n
            .checked_mul(std::mem::size_of::<(bool, Vec<u32>)>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(vis_subs_hdr)?;
        out_rel.bytes = out_rel.bytes.saturating_add(vis_subs_hdr);
        let mut vis_subs: Vec<(bool, Vec<u32>)> = Vec::with_capacity(n);
        for (vis_c, &all_c) in vis_to_all.iter().enumerate().take(n) {
            if vis_c.is_multiple_of(POLL_AXIOMS) {
                poll(limits)?;
            }
            if all_c == u32::MAX {
                // Class not met in this leaf: satisfiable, subsumed by itself and `vis_top`.
                let len = vis_top.len().checked_add(1).ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
                let rb = len
                    .checked_mul(std::mem::size_of::<u32>())
                    .ok_or(Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: usize::MAX,
                    })?;
                admit_bytes(rb)?;
                out_rel.bytes = out_rel.bytes.saturating_add(rb);
                let mut row = Vec::with_capacity(len);
                let actual_rb = row
                    .capacity()
                    .checked_mul(std::mem::size_of::<u32>())
                    .ok_or(Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: usize::MAX,
                    })?;
                if actual_rb > rb {
                    admit_bytes(actual_rb - rb)?;
                    out_rel.bytes = out_rel.bytes.saturating_add(actual_rb - rb);
                } else if rb > actual_rb {
                    credit_bytes(rb - actual_rb);
                    out_rel.bytes = out_rel.bytes.saturating_sub(rb - actual_rb);
                }
                row.extend_from_slice(&vis_top);
                row.push(vis_c as u32);
                row.sort_unstable();
                row.dedup();
                vis_subs.push((false, row));
            } else {
                let (unsat, full_row) = &subs_guard.subs[all_c as usize];
                if *unsat {
                    vis_subs.push((true, Vec::new()));
                } else {
                    let count = full_row
                        .iter()
                        .filter(|&&k| all_to_vis[k as usize] != u32::MAX)
                        .count()
                        .checked_add(1)
                        .ok_or(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        })?;
                    let rb =
                        count
                            .checked_mul(std::mem::size_of::<u32>())
                            .ok_or(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used: usize::MAX,
                            })?;
                    admit_bytes(rb)?;
                    out_rel.bytes = out_rel.bytes.saturating_add(rb);
                    let mut row = Vec::with_capacity(count);
                    let actual_rb = row
                        .capacity()
                        .checked_mul(std::mem::size_of::<u32>())
                        .ok_or(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        })?;
                    if actual_rb > rb {
                        admit_bytes(actual_rb - rb)?;
                        out_rel.bytes = out_rel.bytes.saturating_add(actual_rb - rb);
                    } else if rb > actual_rb {
                        credit_bytes(rb - actual_rb);
                        out_rel.bytes = out_rel.bytes.saturating_sub(rb - actual_rb);
                    }
                    for &k in full_row {
                        let v = all_to_vis[k as usize];
                        if v != u32::MAX {
                            row.push(v);
                        }
                    }
                    row.push(vis_c as u32);
                    row.sort_unstable();
                    row.dedup();
                    vis_subs.push((false, row));
                }
            }
        }

        // Public instance export (genuine original public names ONLY + declared-only rule),
        // pre-admitting O(n) stamp dedup arrays (`vis_mark`, `seen_vis`) so duplicate
        // superclasses never materialize quadratic buffers.
        let inst_hdr = n
            .checked_mul(std::mem::size_of::<Vec<u32>>())
            .ok_or(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: usize::MAX,
            })?;
        admit_bytes(inst_hdr)?;
        out_rel.bytes = out_rel.bytes.saturating_add(inst_hdr);
        let mut leaf_insts: Vec<Vec<u32>> = Vec::with_capacity(n);
        leaf_insts.resize_with(n, Vec::new);

        let seen_buf_bytes =
            n.checked_mul(2 * std::mem::size_of::<u32>())
                .ok_or(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: usize::MAX,
                })?;
        admit_bytes(seen_buf_bytes)?;
        scratch_rel.bytes = scratch_rel.bytes.saturating_add(seen_buf_bytes);
        let mut vis_mark: Vec<u32> = vec![u32::MAX; n];
        let mut seen_vis: Vec<u32> = Vec::with_capacity(n);

        for (j, (ind, in_non_decl)) in orig_individuals.iter().enumerate() {
            if j.is_multiple_of(POLL_AXIOMS) {
                poll(limits)?;
            }
            let j32 = u32::try_from(j).expect("checked orig_individuals len");
            let rep = self
                .interner
                .canon
                .as_deref()
                .and_then(|c| c.get(ind))
                .unwrap_or(ind);
            let ex_opt = self
                .interner
                .individuals
                .get(rep)
                .and_then(|id| self.interner.get(&Node::Nominal(id)));
            seen_vis.clear();
            let mut mark_vis = |v: u32| {
                if vis_mark[v as usize] != j32 {
                    vis_mark[v as usize] = j32;
                    seen_vis.push(v);
                }
            };
            match ex_opt {
                Some(e_j) => {
                    let has_ctx = self.for_each_raw_subsumer(e_j, |s| {
                        if let Some(&all_c) = ex_to_all.get(s as usize)
                            && all_c != u32::MAX
                        {
                            let v = all_to_vis[all_c as usize];
                            if v != u32::MAX {
                                mark_vis(v);
                            }
                            for &k in &subs_guard.subs[all_c as usize].1 {
                                let vk = all_to_vis[k as usize];
                                if vk != u32::MAX {
                                    mark_vis(vk);
                                }
                            }
                        }
                    });
                    if has_ctx.is_some() {
                        for &v in &vis_top {
                            mark_vis(v);
                        }
                    } else if *in_non_decl || required_noms.binary_search(&e_j).is_ok() {
                        return Ok(LeafExportResult::ExcludedOrIncomplete);
                    } else {
                        // Unconstrained declared-only individual interned without a context.
                        for &v in &vis_top {
                            mark_vis(v);
                        }
                    }
                }
                None => {
                    if *in_non_decl {
                        return Ok(LeafExportResult::ExcludedOrIncomplete);
                    }
                    // Truly unconstrained declared-only individual inherits `vis_top`.
                    for &v in &vis_top {
                        mark_vis(v);
                    }
                }
            }
            seen_vis.sort_unstable();
            for &vis_c in &seen_vis {
                // Skip unsatisfiable visible classes: they contribute zero instances.
                if vis_subs[vis_c as usize].0 {
                    continue;
                }
                let slot = &mut leaf_insts[vis_c as usize];
                if slot.len() == slot.capacity() {
                    let next_cap =
                        dl_clause::next_vec_cap(slot.capacity(), std::mem::size_of::<u32>())
                            .ok_or(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used: usize::MAX,
                            })?;
                    let old_b = slot.capacity().saturating_mul(std::mem::size_of::<u32>());
                    let new_b =
                        next_cap
                            .checked_mul(std::mem::size_of::<u32>())
                            .ok_or(Error::Memory {
                                limit: limits.max_memory.unwrap_or(usize::MAX),
                                used: usize::MAX,
                            })?;
                    admit_bytes(new_b)?;
                    slot.reserve_exact(next_cap - slot.len());
                    let actual_new = slot.capacity().saturating_mul(std::mem::size_of::<u32>());
                    credit_bytes(old_b);
                    if new_b > actual_new {
                        credit_bytes(new_b - actual_new);
                    } else if actual_new > new_b {
                        admit_bytes(actual_new - new_b)?;
                    }
                    out_rel.bytes = out_rel
                        .bytes
                        .saturating_sub(old_b)
                        .saturating_add(actual_new);
                }
                slot.push(j32);
            }
        }
        drop(seen_vis);
        drop(vis_mark);
        drop(all_noms);
        drop(required_noms);
        drop(closed_top);
        drop(raw_top);
        drop(subs_guard);
        drop((met_ex, all_to_vis, vis_to_all, ex_to_all));
        drop(scratch_rel);

        let owned_bytes = out_rel.bytes;
        out_rel.bytes = 0;
        Ok(LeafExportResult::Consistent {
            vis_subs,
            vis_top,
            leaf_insts,
            owned_bytes,
        })
    }

    /// Runs `self.run(...)`, handling item 1061 phase 2 clean constant-merge restarts
    /// against the same enclosing work bound while preserving memory high-water marks,
    /// pool credit, deadlines, cancellation, and genuine fallback semantics.
    #[allow(clippy::too_many_arguments)]
    fn run_merging(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
        with_taxonomy: bool,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        self.run_merging_with_carry(
            axioms,
            changes,
            held_bytes,
            limits,
            admitted,
            call_peak,
            with_taxonomy,
            0,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_merging_with_carry(
        &mut self,
        axioms: &[&AxiomKind],
        mut changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        admitted: &std::sync::Arc<AtomicUsize>,
        call_peak: &mut usize,
        with_taxonomy: bool,
        init_spent: usize,
        init_bound: Option<usize>,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        self.merge.spent = init_spent;
        self.merge.restarts = 0;
        self.merge.frozen_bound = init_bound;
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
        self.reused = false;
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
        let mut r = self.run_splitting(
            axioms,
            changes,
            held_bytes,
            limits,
            &admitted,
            &mut call_peak,
            with_taxonomy,
        );
        if let Err(Error::Memory { .. }) = r
            && self.reused
        {
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
            r = self.run_splitting(
                axioms,
                None,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
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
            r = self.run_splitting(
                axioms,
                None,
                held_bytes,
                &one,
                &admitted,
                &mut call_peak,
                with_taxonomy,
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
            r = self.run_splitting(
                axioms,
                None,
                held_bytes,
                limits,
                &admitted,
                &mut call_peak,
                with_taxonomy,
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
        );
        declined.get_or_insert(used);
    }

    /// Drops all run state without allocating a replacement interner; the next
    /// [`Saturation::build`] pre-admits and initializes `Node::Top` / `Node::Bottom`.
    fn reset_empty(&mut self) {
        self.index = None;
        self.interner = Interner::default();
        self.worlds = FxHashMap::default();
        self.chain_ranged = Vec::new();
        self.partial = None;
        self.stale_tier = false;
        self.ctx = Vec::new();
        self.disjoint = Vec::new();
        self.dynamic = None;
        self.dl_run = None;
        self.split_answer = None;
        self.leaf_individuals = Vec::new();
    }

    /// Drops the state of a run that stopped part way, since the next run starts again.
    fn reset(&mut self) {
        self.reset_empty();
        self.interner.ensure_top_bottom();
    }

    /// Normalizes the axioms inside OWL 2 EL and builds the index. `outside` names the
    /// axioms the profile check leaves out.
    /// The answer's last part is the estimated peak of a parallel normalization, with
    /// `held_bytes`, which the pool has been charged for, or 0. `kept_bytes` are those of
    /// the contexts the run keeps, [`Saturation::context_bytes`].
    fn build(
        &mut self,
        axioms: &[&AxiomKind],
        outside: &[(usize, (String, String))],
        threads: usize,
        held_bytes: usize,
        kept_bytes: usize,
        limits: &Limits,
    ) -> Result<(Axioms, Index, usize, OutsideEl, usize), Error> {
        let budget = numeric::Budget::new(
            limits,
            held_bytes + kept_bytes,
            self.interner.ranges.ledger.clone(),
        );
        let init_interner = if self.interner.len() == 0 {
            Interner::EMPTY_INIT_BYTES
        } else {
            0
        };
        budget.baseline(
            self.interner
                .bytes()
                .saturating_add(init_interner)
                .saturating_add(self.merge_extra_bytes()),
        )?;
        self.interner.ensure_top_bottom();
        self.interner.ranges.begin(budget.clone());
        let result = self.build_inner(axioms, outside, threads, held_bytes, limits);
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
        if !chains.is_empty() {
            let ranges = range_table(axioms, &is_outside);
            for k in chains {
                if let AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), sup) =
                    k
                    && let (Some(last), ObjectPropertyExpression::Property(sup)) = (ps.last(), sup)
                {
                    let want = ranges.get(sup.as_str());
                    let have = ranges.get(last.named().as_str());
                    if let Some(missing) =
                        want.and_then(|w| w.iter().find(|c| have.is_none_or(|h| !h.contains(*c))))
                    {
                        index::intern_signature(k, &mut self.interner);
                        chain_ranges.push((
                            k,
                            format!(
                                "the range {missing} of <{}> is not a range of the chain's last property <{}> in OWL 2 EL",
                                sup.as_str(),
                                last.named().as_str()
                            ),
                        ));
                        continue;
                    }
                }
                if let Some(why) = index::normalize(k, &mut self.interner, &mut ax) {
                    partial.insert(k, why);
                }
                self.interner.ranges.check()?;
            }
        }
        // what each axiom outside OWL 2 EL needs, counted once however often it occurs; a
        // chain whose superproperty has a range its last property lacks is Horn
        let mut candidates: Vec<&AxiomKind> = outside.iter().map(|(i, _)| axioms[*i]).collect();
        candidates.extend(chain_ranges.iter().map(|(k, _)| *k));
        if self.hide_internal {
            self.chain_ranged = chain_ranges.iter().map(|(k, _)| (*k).clone()).collect();
        }
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
            b.baseline(self.interner.bytes() + self.merge_extra_bytes() + ax.bytes())?;
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
        let ix = Index::build(&ax, &mut self.interner, limits.max_memory, &stop).map_err(halt)?;
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
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        let want_canon = if limits.rooted
            && ((limits.rooted_merge && !self.merge_disabled) || self.split_cfg.is_some())
            && !self.no_rooted
            && !limits.el_only
            && !self.skip_dl
        {
            self.merge.shared.clone()
        } else {
            None
        };
        if !same_canon(&self.interner.canon, &want_canon) {
            self.reset_empty();
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
                        self.reset_empty();
                        self.hide_internal = true;
                        self.active_hub = red.hub.take();
                        // item 805: keys over object properties and the universal property
                        // go through class axioms over fresh names, which a run that does
                        // not take Tier D leaves to the run without them
                        let all = red.all(axioms);
                        // the rewritten axioms and reference slice are held beside the
                        // caller's for the run
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
                        );
                        self.active_hub = None;
                        self.hide_internal = false;
                        match r {
                            Ok(Some(mut r)) if self.dl_tried => {
                                red.restore(axioms, &mut self.excluded);
                                r.2.added = self.source_len.unwrap_or(axioms.len());
                                let tier = r.2.tier;
                                let mut chains: Vec<Excluded> =
                                    std::mem::take(&mut self.chain_ranged)
                                        .into_iter()
                                        .map(|axiom| Excluded {
                                            axiom,
                                            gap: index::Gap::Partial("chain range"),
                                        })
                                        .collect();
                                red.restore(axioms, &mut chains);
                                let chains: Vec<&AxiomKind> =
                                    chains.iter().map(|e| &e.axiom).collect();
                                r.2.outside_el = red.outside_el(axioms, &chains, &self.excluded);
                                r.2.tier = tier;
                                self.reduced = true;
                                return Ok(r);
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
                                if self.split_cfg.as_ref().is_some_and(|c| c.forked) {
                                    self.split_ineligible = true;
                                    self.reset();
                                    return Err(Error::Memory {
                                        limit: limits.max_memory.unwrap_or(usize::MAX),
                                        used: *call_peak,
                                    });
                                }
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
            if declined && self.split_cfg.as_ref().is_some_and(|c| c.forked) {
                self.split_ineligible = true;
                self.reset();
                return Err(Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: *call_peak,
                });
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
        if let Some(r) = self.run_once(axioms, changes, held_bytes, &step_limits, with_taxonomy)? {
            return Ok(r);
        }
        self.reset_empty();
        let retry_limits = Limits {
            charge: Self::charge_drawing_on(limits, admitted),
            ..limits.clone()
        };
        let r = self.run_once(axioms, None, held_bytes, &retry_limits, with_taxonomy)?;
        Ok(r.expect("a run without changes starts again"))
    }

    /// [`Saturation::run`], or `None` when a run with changes must start again.
    fn run_once(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        with_taxonomy: bool,
    ) -> Result<Option<(bool, Option<Taxonomy>, Stats)>, Error> {
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
        self.horn_tried = false;
        self.dl_tried = false;
        self.partial_over = false;
        self.numeric_halt = false;
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
            rooted_eq_split: limits.rooted_eq_split,
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
        // A run with checker components starts again, since what the premises below
        // intern for a checker property would be the lattice's. Every edit that names a
        // data property or datatype already satisfies `needs_full_run` inside
        // `must_restart`, so `!full` guarantees no changed axiom can trigger a checker
        // component.
        let full = self.entangled
            || self.index.as_ref().is_some_and(|ix| ix.dt.is_some())
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
        if full {
            self.interner = Interner::default();
            self.interner.canon = self.canon_want.clone();
            self.ctx = Vec::new();
            self.disjoint = Vec::new();
            self.dynamic = None;
            self.dl_run = None;
        }
        // a run that keeps the contexts of a run with the Horn rules or Tier D compares
        // the two indexes, so the last one stays until the new one is built, and counts
        // against the budget until then, as do the contexts above the index's and Tier
        // D's clause table
        let old_ix = match &kept {
            Some(k) if k.tier != KeptTier::El => self.index.take(),
            _ => None,
        };
        let old_bytes = old_ix.as_ref().map_or(0, |ix| ix.bytes())
            + self.dynamic.as_ref().map_or(0, |d| d.retained_bytes())
            + self.dl_run.as_ref().map_or(0, |d| {
                d.table.bytes() + d.lits.bytes() + d.split_meta_bytes()
            });
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
                    "must_restart already forces a full run for any edit naming a data property or datatype"
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
        )?;
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
        if self.split_cfg.as_ref().is_some_and(|c| c.forked)
            && (!disj
                || ix.dl.as_deref().and_then(|d| d.root).is_none()
                || ax.dl.as_ref().is_some_and(|d| d.counting))
        {
            self.split_ineligible = true;
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: normalized_peak,
            });
        }
        if self.merge.restarts > 0 && (!disj || ix.dl.as_deref().and_then(|d| d.root).is_none()) {
            // Rooted eligibility was lost after a derived merge restart: never answer in
            // Horn/EL/unrooted-DL using aliases derived by the discarded rooted theory.
            self.merge.clear();
            self.canon_want = None;
            self.active_hub = None;
            self.merge_disabled = true;
            return Ok(None);
        }
        if let Some(d) = &ax.dl {
            stats.clauses = d.clauses.len();
        }
        // the rules of this run, when a later run may keep its contexts: the EL rules,
        // the Horn rules without individuals, universal roles, keys or data, whose
        // contexts depend on one another only along their links, or Tier D
        // with the cautious strategy and without a bound on its conclusions, which a
        // run that counts, a rooted run and the run after a passed bound have, and whose
        // count of conclusions a kept context would change
        let tier = match (horn, disj) {
            (false, false) => Some(KeptTier::El),
            // also a run whose Horn normalization gave the index no rule that can fire,
            // which the EL rules saturate: its contexts follow from the same entries
            (true, false) if saturate::plain(&ix) && ix.comparable() => Some(KeptTier::Horn),
            (false, true)
                if ix.horn.is_none()
                    && ix.dl.as_deref().is_some_and(|d| d.keyed_by_expressions())
                    && saturate::plain(&ix)
                    && ix.comparable()
                    && limits.expansion == Expansion::Cautious
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
            return Ok(None);
        }
        let reuse = match &old_ix {
            Some(old) => {
                if !ix.same_tables(old) {
                    return Ok(None);
                }
                true
            }
            None => false,
        };
        if !reuse {
            self.dynamic = None;
            self.dl_run = None;
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
        let dynamic = match &ix.horn {
            _ if reuse => {
                let mut d = self.dynamic.take().expect(
                    "the conjunctions of the last run with the Horn rules, or the tests of \
                     the last run of Tier D",
                );
                // the conjunctions' contexts move above the expressions the edit added
                let from = d.base();
                let delta = n as Ex - from;
                if delta > 0 {
                    for c in self.ctx.iter_mut().filter_map(|c| c.get_mut()) {
                        c.st.get_mut().shift_contexts(from, delta);
                    }
                    for i in 0..d.len() as Ex {
                        if let Some(c) = d.get_ctx(from + i) {
                            c.st.lock().shift_contexts(from, delta);
                        }
                    }
                    d.rebase(n as Ex);
                }
                d.sync_unadmitted_on_begin_run();
                Some(d)
            }
            Some(h) => {
                debug_assert_eq!(h.base as usize, n);
                Some(saturate::Dynamic::new(n as Ex))
            }
            // the test contexts of Tier D get ids above the index's, and a run whose
            // Horn normalization gave no rule has no conjunctions and an empty table,
            // which a run that keeps its contexts expects
            None if disj || horn => Some(saturate::Dynamic::new(n as Ex)),
            None => None,
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
        let dl_run = ix.dl.as_ref().map(|_| match self.dl_run.take() {
            // the kept contexts name the clauses of the last run's table by id
            Some(mut d) if reuse => {
                d.begin_run();
                d
            }
            _ => dl_rules::DlRun::new(limits.expansion == Expansion::Eager, self.dl_worked.clone()),
        });
        let mut dl_run = dl_run.map(|mut d| {
            d.mrepair = dl_root::marker_repair(limits.rooted_marker_repair);
            d
        });
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
                held: self.index_bytes(&ix)
                    + held_bytes
                    + ax.bytes()
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
        if let Some(old) = old_ix.filter(|_| reuse) {
            stats.incremental = true;
            let d = dynamic
                .as_ref()
                .expect("the contexts above the index's of a run that keeps them");
            // the comparison holds both indexes beside the kept contexts and their
            // clause table, with its marks and the set and the queue of the
            // invalidation, which count against the budget as the kept state does; a
            // run that passes it starts again in full (`run_with`)
            let scratch = ix.changed_scratch(&old) + (self.ctx.len() + d.len()) * 16;
            let held = self.index_bytes(&ix)
                + old.bytes()
                + held_bytes
                + ax.bytes()
                + self.context_bytes()
                + d.retained_bytes()
                + dl_run.as_ref().map_or(0, |d| d.bytes())
                + scratch;
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
            let changed = ix.changed(&old, &|| {
                let r = poll(limits);
                let stop = r.is_err();
                if let Err(e) = r {
                    *stopped.borrow_mut() = Some(e);
                }
                stop
            });
            let Some(changed) = changed else {
                return Err(stopped.into_inner().unwrap_or(Error::Cancelled));
            };
            stats.cleared = self.invalidate_changed(&changed, &ix, d);
            drop(old);
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
        stats.index = start.elapsed();
        // the index, the caller's copy of the axioms, the normalized axioms and the roots,
        // and the contexts kept from the last run
        let base = self.index_bytes(&ix)
            + held_bytes
            + ax.bytes()
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
        let merge_discover =
            self.dl_rooted && !self.dl_counted && limits.rooted_merge && !self.merge_disabled;
        let split_active = self.dl_rooted
            && !self.dl_counted
            && self
                .split_cfg
                .as_ref()
                .is_some_and(|c| c.mode != dl_rules::SplitMode::Disabled);
        if (merge_discover || split_active)
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
                let limit = self.merge.frozen_bound.unwrap_or_else(|| {
                    limits
                        .dl_work
                        .unwrap_or_else(|| {
                            rooted_work_bound(visible_named_classes, rooted_bound_per_class())
                        })
                        .max(1)
                });
                #[cfg(test)]
                LAST_ROOTED_WORK_LIMIT.with(|c| c.set(limit));
                d.set_work_limit(limit);
                if self.merge.spent > 0 {
                    d.add_carried(self.merge.spent);
                }
                if merge_discover {
                    d.set_merge(true);
                    if !merge_hub()
                        && let Some(hub) = &self.active_hub
                        && let Some(id) = self.interner.individuals.get(hub)
                    {
                        d.merge_skip = Some(id);
                    }
                }
                if let Some(cfg) = &self.split_cfg
                    && cfg.mode != dl_rules::SplitMode::Disabled
                    && self.interner.individuals.len() < u32::MAX as usize
                {
                    let n_inds = self.interner.individuals.len();
                    let rank_bytes = n_inds.checked_mul(std::mem::size_of::<u32>());
                    let neq_bytes = cfg.neq.len().checked_mul(std::mem::size_of::<(u32, u32)>());
                    let Some(meta_bytes) =
                        rank_bytes.and_then(|r| neq_bytes.and_then(|q| r.checked_add(q)))
                    else {
                        return Err(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        });
                    };
                    let Some(meta_peak) = base.checked_add(meta_bytes) else {
                        return Err(Error::Memory {
                            limit: limits.max_memory.unwrap_or(usize::MAX),
                            used: usize::MAX,
                        });
                    };
                    let meta_more = meta_peak.saturating_sub(normalized_peak);
                    if limits.max_memory.is_some_and(|m| meta_peak > m)
                        || charge_fn.is_some_and(|f| meta_more > 0 && !f(meta_more))
                    {
                        return Err(Error::Memory {
                            limit: limits.max_memory.unwrap_or(meta_peak),
                            used: meta_peak,
                        });
                    }
                    normalized_peak = normalized_peak.max(meta_peak);
                    let mut const_rank = vec![u32::MAX; n_inds];
                    for (epoch_id, ind) in self.interner.individuals.iter().enumerate() {
                        if index::is_anonymous(ind)
                            || self.active_hub.as_ref().is_some_and(|h| h == ind)
                        {
                            continue;
                        }
                        if let Ok(rank) = cfg
                            .orig_individuals
                            .binary_search_by(|o| o.as_str().cmp(ind.as_str()))
                            && let Ok(r32) = u32::try_from(rank)
                        {
                            const_rank[epoch_id] = r32;
                        }
                    }
                    let mut branch_neq = Vec::with_capacity(cfg.neq.len());
                    for (a, b) in &cfg.neq {
                        let ra = self
                            .interner
                            .canon
                            .as_deref()
                            .and_then(|c| c.get(a))
                            .unwrap_or(a);
                        let rb = self
                            .interner
                            .canon
                            .as_deref()
                            .and_then(|c| c.get(b))
                            .unwrap_or(b);
                        if let (Some(ua), Some(ub)) = (
                            self.interner.individuals.get(ra),
                            self.interner.individuals.get(rb),
                        ) && ua != ub
                        {
                            branch_neq.push((ua.min(ub), ua.max(ub)));
                        }
                    }
                    branch_neq.sort_unstable();
                    branch_neq.dedup();
                    d.configure_split(
                        cfg.mode,
                        cfg.probe,
                        cfg.epoch,
                        const_rank.into_boxed_slice(),
                        branch_neq.into_boxed_slice(),
                    );
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
        let bounds = Bounds {
            threads: saturation_threads,
            deadline: limits.deadline,
            max_memory,
            cancelled: cancel_fn,
            charge: charge_fn,
            base_memory: base,
            base_queued: 0,
            peak_memory: normalized_peak,
            paid_memory: if charge_fn.is_some() {
                normalized_peak.max(base)
            } else {
                0
            },
            dynamic: dynamic.as_ref(),
            dl: dl_run.as_ref(),
            partial: limits.partial && disj && with_taxonomy,
            steps,
        };
        // the pool has been charged for the peak of a parallel normalization
        let more = base.saturating_sub(normalized_peak);
        if limits.max_memory.is_some_and(|m| base > m)
            || charge_fn.is_some_and(|f| more > 0 && !f(more))
        {
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(base),
                used: base,
            });
        }
        let t = Instant::now();
        let stopped = |stop: Stop| match stop {
            Stop::Cancelled => Error::Cancelled,
            Stop::Timeout => Error::Timeout,
            // the bound on derived conclusions goes back as the memory error that the
            // fallback chain of `run_with` takes
            Stop::Memory { used }
            | Stop::Work { used }
            | Stop::Merge { used }
            | Stop::Split { used } => Error::Memory {
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
        let mut totals = match saturate::saturate_counted(&ix, &self.ctx, &roots, &[], &[], &bounds)
        {
            (t, None) => t,
            (_, Some(Stop::Merge { used })) => {
                // Disable merge and split discovery outside the main round and inspect the
                // winning payload after all workers have joined.
                if let Some(d) = &dl_run {
                    d.set_merge(false);
                    d.set_split_mode(dl_rules::SplitMode::Disabled);
                    let _ = d.take_winning_split();
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
            (_, Some(Stop::Split { used })) => {
                if let Some(d) = &dl_run {
                    d.set_merge(false);
                    d.set_split_mode(dl_rules::SplitMode::Disabled);
                    let _ = d.take_merged();
                    let spent = d.work_total();
                    let bound = d.work_limit();
                    let exhausted = d.over_work() || (bound != 0 && spent >= bound);
                    let winner = d.take_winning_split();
                    let inds: &[NamedIndividual] = &self.interner.individuals;
                    if !exhausted
                        && let Some(cand) = winner
                        && cand.epoch == d.split_epoch()
                        && let (Some(u), Some(v)) =
                            (inds.get(cand.u_id as usize), inds.get(cand.v_id as usize))
                        && u != v
                        && !index::is_anonymous(u)
                        && !index::is_anonymous(v)
                        && self.active_hub.as_ref().is_none_or(|h| h != u && h != v)
                    {
                        self.dl_worked.store(false, Ordering::Relaxed);
                        self.split_stop = Some(SplitStop {
                            u_id: cand.u_id,
                            v_id: cand.v_id,
                            spent,
                            bound,
                            peak: used,
                            prio: cand.prio,
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
                if let Some(d) = &dl_run {
                    let _ = d.take_merged();
                    let _ = d.take_winning_split();
                }
                over_work = matches!(s, Stop::Work { .. });
                halted = Some(stopped(s));
                t
            }
            (_, Some(s)) => {
                if let Some(d) = &dl_run {
                    let _ = d.take_merged();
                    let _ = d.take_winning_split();
                }
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
            // Never trigger merge or split discovery in later marking/test rounds.
            d.set_merge(false);
            d.set_split_mode(dl_rules::SplitMode::Disabled);
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
                struct TestDriverBudget<'a> {
                    totals: &'a mut saturate::Totals,
                    d: &'a saturate::Dynamic,
                    dl_unadmitted: usize,
                    max_memory: Option<usize>,
                    charge_fn: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
                    refused: Option<usize>,
                }
                impl saturate::DynamicBudget for TestDriverBudget<'_> {
                    fn admit(&mut self, add: usize) -> bool {
                        let Some(next_mem) = self.totals.memory.checked_add(add) else {
                            self.totals.peak = usize::MAX;
                            self.refused = Some(usize::MAX);
                            return false;
                        };
                        let Some(live) = next_mem
                            .checked_add(self.d.unadmitted_bytes())
                            .and_then(|n| n.checked_add(self.dl_unadmitted))
                        else {
                            self.totals.peak = usize::MAX;
                            self.refused = Some(usize::MAX);
                            return false;
                        };
                        self.totals.peak = self.totals.peak.max(live);
                        if !cover_memory(
                            live,
                            self.max_memory,
                            self.charge_fn,
                            &mut self.totals.paid,
                        ) {
                            self.refused = Some(live);
                            return false;
                        }
                        self.totals.memory = next_mem;
                        true
                    }
                    fn credit(&mut self, back: usize) {
                        self.totals.memory = self.totals.memory.saturating_sub(back);
                    }
                    fn on_overflow(&mut self) {
                        self.totals.peak = usize::MAX;
                        self.refused = Some(usize::MAX);
                    }
                }
                let mut ids: Vec<Ex> = Vec::with_capacity(tests.len());
                let mut test_budget = TestDriverBudget {
                    totals: &mut totals,
                    d,
                    dl_unadmitted: dl_run.as_ref().map_or(0, |r| r.bytes()),
                    max_memory,
                    charge_fn,
                    refused: None,
                };
                #[cfg(test)]
                admission_test::emit_merge_phase(
                    admission_test::MergePhase::BetweenRoundsTestStart,
                );
                for (x, bs) in &tests {
                    match dl_rules::test_admitted(d, *x, bs, &mut test_budget) {
                        Some(id) => ids.push(id),
                        None => {
                            #[cfg(test)]
                            admission_test::emit_merge_phase(
                                admission_test::MergePhase::BetweenRoundsTestRefused,
                            );
                            over_work = false;
                            halted = Some(Error::Memory {
                                limit: max_memory.unwrap_or(usize::MAX),
                                used: test_budget.refused.unwrap_or(test_budget.totals.peak),
                            });
                            break;
                        }
                    }
                }
                if halted.is_some() {
                    break;
                }
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
                let bound = |delta: isize| budget.tell(delta);
                let found = par_map(&classes, threads, MIN_CLASSES, |_, chunk| {
                    let mut out = Vec::with_capacity(chunk.len());
                    for &d in chunk {
                        let w = world::World::new(
                            &ix,
                            &self.ctx,
                            dynamic.as_ref(),
                            &universal,
                            &real,
                            &bound,
                        );
                        let f = w.classify(d)?;
                        let kept = 48 + f.subs.len() * 4;
                        if !bound(kept as isize) {
                            return Err(index::Halt::Stopped);
                        }
                        out.push((d, f, kept));
                    }
                    Ok(out)
                })
                .into_iter()
                .collect::<Result<Vec<_>, index::Halt>>();
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
                }
                for (d, f, kept) in found.into_iter().flatten() {
                    world_bytes += kept;
                    self.worlds.insert(d, (f.bottom, f.subs));
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
            disjunctive: stats.outside_el.disjunctive > 0,
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
                    totals.memory
                        + d.bytes()
                        + dynamic.as_ref().map_or(0, |dy| dy.unadmitted_bytes())
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
            totals.memory =
                totals
                    .memory
                    .checked_add(d.unadmitted_bytes())
                    .ok_or(Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used: usize::MAX,
                    })?;
            totals.peak = totals.peak.max(totals.memory);
            if horn {
                stats.conjunctions = d.len();
            }
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
        // Move `ax.individuals` out of `ax` before computing `retained` so the retained
        // footprint includes `self.leaf_individuals` with zero extra clone allocation.
        if self.split_cfg.is_some() && !with_taxonomy {
            self.leaf_individuals = std::mem::take(&mut ax.individuals);
        }
        // The saturation charges fixed allowances for new contexts/conclusions;
        // reuse counts their retained tables instead. Reconcile once the workers
        // have stopped so both paths report the same retained-state estimate.
        let root_bytes = roots.capacity() * 4;
        let retained = self.retained_bytes(held_bytes, root_bytes, world_bytes);
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
            return Ok(Some((consistent, None, stats)));
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
            return Ok(Some((consistent, Some(taxonomy), stats)));
        }
        poll(limits)?;
        Ok(Some((consistent, Some(taxonomy), stats)))
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
            base.saturating_add(
                self.interner
                    .bytes()
                    .saturating_sub(self.interner.canon_bytes()),
            )
            .saturating_add(self.merge.uf_bytes())
            .saturating_add(self.canon_live_bytes())
            .saturating_add(self.active_hub.as_ref().map_or(0, |h| h.as_str().len()))
        }
    }

    fn index_bytes(&self, ix: &Index) -> usize {
        // Every interner-owned table (nodes, class and individual names/IRIs, roles,
        // data values, scopes), deduplicating `canon` via `canon_live_bytes()`.
        self.interner
            .bytes()
            .saturating_sub(self.interner.canon_bytes())
            + ix.numeric_ledger
                .as_ref()
                .or(self.interner.ranges.ledger.as_ref())
                .map_or(0, |l| l.bytes())
            + ix.bytes()
            + self.canon_live_bytes()
            + self.merge.uf_bytes()
            + self.active_hub.as_ref().map_or(0, |h| h.as_str().len())
            + self.disjoint.capacity() * 24
            + self
                .disjoint
                .iter()
                .map(|d| d.capacity() * 4)
                .sum::<usize>()
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

    fn residual_outer_bytes(&self) -> usize {
        self.ctx
            .capacity()
            .saturating_mul(std::mem::size_of::<OnceLock<Box<saturate::Ctx>>>())
            .saturating_add(self.disjoint.capacity().saturating_mul(24))
            .saturating_add(
                self.disjoint
                    .iter()
                    .map(|d| d.capacity().saturating_mul(4))
                    .sum::<usize>(),
            )
            .saturating_add(self.worlds_bytes())
            .saturating_add(excluded_copy_bytes(&self.excluded))
            .saturating_add(
                self.chain_ranged
                    .capacity()
                    .saturating_mul(std::mem::size_of::<AxiomKind>()),
            )
            .saturating_add(
                self.leaf_individuals
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Ex>()),
            )
            .saturating_add(self.interner.bytes())
    }

    /// Live saturated leaf footprint on `self` excluding `canon_live_bytes()` and
    /// `merge.uf_bytes()`, which are accounted once by `SplitLedger` across all frames.
    fn leaf_live_bytes_without_canon_and_merge(&self) -> usize {
        if let Some(ix) = &self.index {
            self.index_bytes(ix)
                .saturating_sub(self.canon_live_bytes())
                .saturating_sub(self.merge.uf_bytes())
                .saturating_add(self.context_bytes())
                .saturating_add(self.dynamic.as_ref().map_or(0, |d| d.retained_bytes()))
                .saturating_add(self.dl_run.as_ref().map_or(0, |d| {
                    d.table.bytes() + d.lits.bytes() + d.split_meta_bytes()
                }))
                .saturating_add(self.worlds_bytes())
                .saturating_add(
                    self.chain_ranged
                        .capacity()
                        .saturating_mul(std::mem::size_of::<AxiomKind>()),
                )
                .saturating_add(
                    self.leaf_individuals
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Ex>()),
                )
                .saturating_add(excluded_copy_bytes(&self.excluded))
        } else {
            self.residual_outer_bytes()
                .saturating_sub(self.interner.canon_bytes())
                .saturating_add(self.active_hub.as_ref().map_or(0, |h| h.as_str().len()))
        }
    }

    fn retained_bytes(&self, held_bytes: usize, root_bytes: usize, world_bytes: usize) -> usize {
        if let Some(split) = &self.split_answer {
            return held_bytes
                .saturating_add(split.owned_bytes())
                .saturating_add(self.residual_outer_bytes());
        }
        self.index_bytes(self.index.as_ref().expect("completed index"))
            + held_bytes
            + root_bytes
            + self.context_bytes()
            + self.dynamic.as_ref().map_or(0, |d| d.retained_bytes())
            + self.dl_run.as_ref().map_or(0, |d| {
                d.table.bytes() + d.lits.bytes() + d.split_meta_bytes()
            })
            + self
                .leaf_individuals
                .capacity()
                .saturating_mul(std::mem::size_of::<Ex>())
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
        let in_play = k.outside || !outside.is_empty();
        if in_play && changed().any(|a| logical(a) && !tier_neutral(a)) {
            return true;
        }
        match k.tier {
            KeptTier::El => !outside.is_empty(),
            KeptTier::Horn => {
                !outside.is_empty() && (k.disjunctive || outside.iter().any(|a| !dl::shape(a).horn))
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
    /// In a run of Tier D a context holds an expression when a unit, a marker or a
    /// stored clause names it, the contexts above the index's are the tests, and a
    /// test that held and is cleared clears its class, whose subsumer it gave. A kept
    /// context there need not be as a run that starts again leaves it: it keeps the
    /// markers that cleared predecessors sent, which are tautologies, and the clauses
    /// they gave. Each kept clause follows from the ontology as it is now, and the
    /// saturation closes every context under the rules, which is what the
    /// completeness of the calculus asks (Bate and others, JAIR 2018, Theorem 9, for
    /// any context structure to which no rule applies, with Theorem 8 for soundness).
    fn invalidate_changed(
        &mut self,
        changed: &[bool],
        ix: &Index,
        dynamic: &saturate::Dynamic,
    ) -> usize {
        let base = dynamic.base();
        let count = dynamic.len() as Ex;
        let ctx = &self.ctx;
        let with = |e: Ex, f: &mut dyn FnMut(&saturate::State)| {
            let c = if e < base {
                ctx.get(e as usize).and_then(|c| c.get()).map(|c| &**c)
            } else {
                dynamic.get_ctx(e)
            };
            if let Some(c) = c {
                f(&c.st.lock());
            }
        };
        let all = (0..ctx.len() as Ex).chain((0..count).map(|i| base + i));
        let mut affected: FxHashSet<Ex> = FxHashSet::default();
        for e in all.clone() {
            with(e, &mut |st| {
                if st.init
                    && (st
                        .subs
                        .iter()
                        .any(|&s| changed.get(s as usize).copied().unwrap_or(true))
                        || dl_rules::dl(st).is_some_and(|d| d.names_changed(changed)))
                {
                    affected.insert(e);
                }
            });
        }
        // every context with a link into an affected context is affected, as is every
        // context that registered with one as its predecessor, which took its clauses
        // over the predecessor, and the class whose subsumer a test that held gave
        let tests = ix.dl.is_some();
        let mut queue: Vec<Ex> = affected.iter().copied().collect();
        while let Some(e) = queue.pop() {
            with(e, &mut |st| {
                let mut add = |s: Ex| {
                    if affected.insert(s) {
                        queue.push(s);
                    }
                };
                for (_, srcs) in &st.back {
                    for &s in &srcs.list {
                        add(s);
                    }
                }
                if let Some(d) = dl_rules::dl(st) {
                    for &(u, _) in &d.dpreds {
                        add(u);
                    }
                }
                if tests
                    && e >= base
                    && st.bottom
                    && let Some(x) = dl_rules::tested_class(dynamic.core(e))
                {
                    add(x);
                }
            });
        }
        let mut cleared = 0;
        for e in all {
            let c = if e < base {
                ctx.get(e as usize).and_then(|c| c.get()).map(|c| &**c)
            } else {
                dynamic.get_ctx(e)
            };
            let Some(c) = c else { continue };
            if affected.contains(&e) {
                if c.st.lock().init {
                    cleared += 1;
                }
                c.clear();
            } else {
                let mut st = c.st.lock();
                if st.init {
                    st.drop_predecessors(&affected);
                    st.reindex_disjoint(ix);
                }
            }
        }
        cleared
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

    /// Zero-allocation streaming visitor over raw named subsumers of a saturated context
    /// (`self.worlds` or `self.ctx`). Returns `None` if `e` has no saturated context, or
    /// `Some(unsat)` after invoking `f(sub)` for every `Node::Class` or `Node::Top` subsumer.
    fn for_each_raw_subsumer(&self, e: Ex, mut f: impl FnMut(Ex)) -> Option<bool> {
        if let Some((bottom, subs)) = self.worlds.get(&e) {
            for &s in subs {
                if matches!(self.interner.nodes[s as usize], Node::Class(_) | Node::Top) {
                    f(s);
                }
            }
            return Some(*bottom);
        }
        let c = self.ctx.get(e as usize)?.get()?;
        let st = c.st.lock();
        for &s in &st.subs {
            if matches!(self.interner.nodes[s as usize], Node::Class(_) | Node::Top) {
                f(s);
            }
        }
        Some(st.bottom)
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

/// The ranges of each object property through its superproperties, as class expressions,
/// for the range restriction on chains.
fn range_table(
    axioms: &[&AxiomKind],
    outside: &[bool],
) -> FxHashMap<String, std::collections::BTreeSet<ClassExpression>> {
    let mut sup: FxHashMap<&str, Vec<&str>> = FxHashMap::default();
    let mut direct: FxHashMap<&str, Vec<&ClassExpression>> = FxHashMap::default();
    for (i, &k) in axioms.iter().enumerate() {
        if outside[i] {
            continue;
        }
        match k {
            AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ObjectPropertyExpression::Property(a)),
                ObjectPropertyExpression::Property(b),
            ) => sup.entry(a.as_str()).or_default().push(b.as_str()),
            AxiomKind::EquivalentObjectProperties(ps) => {
                for a in ps {
                    for b in ps {
                        if a != b {
                            sup.entry(a.named().as_str())
                                .or_default()
                                .push(b.named().as_str());
                        }
                    }
                }
            }
            AxiomKind::ObjectPropertyRange(ObjectPropertyExpression::Property(p), c) => {
                direct.entry(p.as_str()).or_default().push(c)
            }
            _ => {}
        }
    }
    let mut out = FxHashMap::default();
    let props: FxHashSet<&str> = sup.keys().chain(direct.keys()).copied().collect();
    for p in props {
        let mut seen = FxHashSet::default();
        let mut stack = vec![p];
        let mut rs = std::collections::BTreeSet::new();
        while let Some(q) = stack.pop() {
            if !seen.insert(q) {
                continue;
            }
            if let Some(d) = direct.get(q) {
                rs.extend(d.iter().map(|c| (*c).clone()));
            }
            if let Some(s) = sup.get(q) {
                stack.extend(s.iter().copied());
            }
        }
        out.insert(p.to_string(), rs);
    }
    out
}

/// The axioms normalized between two polls of the run's deadline and cancellation.
const POLL_AXIOMS: usize = 65_536;

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

/// Declared after the growth guard so the stop is visible before a waiter enters.
/// The reporters still own their deltas and retire them through `tell`.
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
        let now = self.held.fetch_add(delta, Ordering::Relaxed) + delta;
        // Negative rollback during panic unwind retires the in-flight delta cleanly
        // without re-invoking a potentially panicking cancellation callback.
        if delta < 0 && std::thread::panicking() {
            return false;
        }
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
        let _panic = WorldChargePanic(self, std::thread::panicking());
        let why = if limits.deadline.is_some_and(|d| Instant::now() >= d) {
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
}

fn poll(limits: &Limits) -> Result<(), Error> {
    if limits.deadline.is_some_and(|d| Instant::now() >= d) {
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
            || classifier.run_once(axioms, None, 0, &limits, true),
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
        // the pool refuses a grant of 256 MiB, and the scan's 512 MiB stay within the run's
        // own budget of at least 1 GiB, so that only the pool stops it
        let charge: Arc<dyn Fn(usize) -> bool + Send + Sync> = Arc::new(|b| b < 1 << 28);
        for (name, extra, limits) in [
            (
                "budget",
                1usize << 40,
                Limits {
                    max_memory: Some(1 << 34),
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
            .build(&axioms, &outside, 1, 0, 0, &Limits::default())
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

    /// Neon r1 B1 fail-first regression: `export_split_leaf` MUST include hidden
    /// `dl_reduce::PREFIX` classes whose `interner.met` bit was cleared by `hide_prefix`,
    /// closing both visible subsumptions (`Src ⊑ K_internal ⊑ Dst`) and nominal
    /// unsatisfiability (`a ∈ K_internal ⊑ ⊥`) through `K_internal` before projection.
    #[test]
    fn export_split_leaf_includes_hidden_dl_reduce_prefix_classes() {
        let mut sat = Saturation::new();
        let src_c = Class::new("http://example.org/h#Src");
        let dst_c = Class::new("http://example.org/h#Dst");
        let hid_c = Class::new(format!("{}k_hidden", dl_reduce::PREFIX));
        let ind_a = NamedIndividual::new("http://example.org/h#a");

        let e_src = sat.interner.class(&src_c);
        let e_dst = sat.interner.class(&dst_c);
        let e_hid = sat.interner.class(&hid_c);
        let e_nom = sat.interner.nominal(&ind_a);
        sat.interner.hide_prefix(dl_reduce::PREFIX);
        assert!(
            !sat.interner.in_signature(&hid_c),
            "hide_prefix must have cleared met bit for hidden class"
        );
        sat.reduced = true;
        let ax = index::Axioms {
            individuals: vec![e_nom],
            ..index::Axioms::default()
        };
        let ix = Index::build(&ax, &mut sat.interner, None, &|| false).unwrap();
        sat.index = Some(ix);
        sat.leaf_individuals = vec![e_nom];

        let make_ctx = |n: usize| -> Vec<OnceLock<Box<saturate::Ctx>>> {
            (0..n).map(|_| OnceLock::new()).collect()
        };
        let set_ctx = |ctxs: &[OnceLock<Box<saturate::Ctx>>], e: Ex, bottom: bool, subs: &[Ex]| {
            let c = Box::new(saturate::Ctx::default());
            {
                let mut st = c.st.lock();
                st.bottom = bottom;
                for &s in subs {
                    st.subs.insert(s);
                }
            }
            let _ = ctxs[e as usize].set(c);
        };

        let mut orig_classes = vec![src_c.clone(), dst_c.clone()];
        orig_classes.sort_unstable();
        let src_idx = orig_classes.binary_search(&src_c).unwrap();
        let dst_idx = orig_classes.binary_search(&dst_c).unwrap() as u32;
        let orig_individuals = vec![(ind_a.clone(), true)];
        let limits = Limits::default();
        let stop = || false;
        let budget = |_d: isize| -> Result<(), usize> { Ok(()) };

        // Case 1: `Src ⊑ K_internal ⊑ Dst` and `a ∈ K_internal` (no direct `Src -> Dst` or
        // `a -> Dst` edge in raw `subs`). `export_split_leaf` must close through hidden
        // `K_internal`, preserving `Src ⊑ Dst` and `a ∈ Dst` while keeping `K_internal` hidden.
        {
            let ctxs = make_ctx(sat.interner.len());
            set_ctx(&ctxs, TOP, false, &[TOP]);
            set_ctx(&ctxs, e_src, false, &[e_src, e_hid]);
            set_ctx(&ctxs, e_hid, false, &[e_hid, e_dst]);
            set_ctx(&ctxs, e_dst, false, &[e_dst]);
            set_ctx(&ctxs, e_nom, false, &[e_nom, e_hid]);
            sat.ctx = ctxs;

            match sat
                .export_split_leaf(&orig_classes, &orig_individuals, &limits, &stop, &budget)
                .unwrap()
            {
                LeafExportResult::Consistent {
                    vis_subs,
                    leaf_insts,
                    ..
                } => {
                    assert!(
                        vis_subs[src_idx].1.contains(&dst_idx),
                        "Src must reach Dst through hidden dl_reduce::PREFIX class: {:?}",
                        vis_subs[src_idx]
                    );
                    assert_eq!(
                        leaf_insts[dst_idx as usize],
                        vec![0],
                        "individual a must inherit Dst through hidden dl_reduce::PREFIX class"
                    );
                }
                other => panic!(
                    "expected Consistent leaf export, got {}",
                    match other {
                        LeafExportResult::Inconsistent => "Inconsistent",
                        LeafExportResult::ExcludedOrIncomplete => "ExcludedOrIncomplete",
                        LeafExportResult::Consistent { .. } => "Consistent",
                    }
                ),
            }
        }

        // Case 2: Hidden class `K_internal` is unsatisfiable (`bottom = true`) and real
        // nominal `a` has `bottom = false` directly but raw subsumer `K_internal`.
        // `export_split_leaf` must catch the leaf as `Inconsistent`!
        {
            let ctxs = make_ctx(sat.interner.len());
            set_ctx(&ctxs, TOP, false, &[TOP]);
            set_ctx(&ctxs, e_src, false, &[e_src]);
            set_ctx(&ctxs, e_dst, false, &[e_dst]);
            set_ctx(&ctxs, e_hid, true, &[e_hid]);
            set_ctx(&ctxs, e_nom, false, &[e_nom, e_hid]);
            sat.ctx = ctxs;

            let res = sat
                .export_split_leaf(&orig_classes, &orig_individuals, &limits, &stop, &budget)
                .unwrap();
            assert!(
                matches!(res, LeafExportResult::Inconsistent),
                "nominal reaching unsatisfiable hidden dl_reduce::PREFIX class must prove leaf inconsistent"
            );
        }
    }

    /// Design v12 §4.3 + Sol r1 P2 #1 + Neon r1 NB3:
    /// 1. Forked 0-consistent-leaf `SplitAnswer` (`dl_rooted_splits >= 1`,
    ///    `dl_rooted_leaves >= 2`) + `describe(&Boom)` and `describe(&owl:Nothing)` listing
    ///    declared-only `:ghost`.
    /// 2. `max_depth` and `max_leaves` cap exhaustion (continuing cap-bound branches with
    ///    `SplitMode::Disabled` without dropping siblings).
    /// 3. `ExcludedOrIncomplete` disabled-split retry after a paid prefix/sibling, both with
    ///    remaining budget (`remaining > 0`) and with zero remaining budget (`remaining == 0`).
    /// 4. `ground_total = false` control on `shoi-042`.
    #[test]
    fn split_zero_consistent_leaves_caps_excluded_retry_and_ground_total_off() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
        let shoi042 =
            crate::ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap())
                .unwrap()
                .axioms;
        let lim_split = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };

        // 1. Forked 0-consistent-leaf `SplitAnswer` via probe=0:
        //    `a = b ∨ a = c` with existential/universal contradictions on both `:b` and `:c`
        //    forks BEFORE either branch resolves, producing 2 inconsistent leaves (0 consistent
        //    leaves) and retaining `SplitAnswer` whose `describe` lists declared-only `:ghost`
        //    for `:Boom` and `owl:Nothing`.
        {
            let ofn = "Prefix(:=<http://example.org/z#>)\nOntology(<http://example.org/z>\n  Declaration(NamedIndividual(:ghost))\n  Declaration(Class(:Boom))\n  ClassAssertion(ObjectOneOf(:b :c) :a)\n  ClassAssertion(ObjectSomeValuesFrom(:r :P) :a)\n  ClassAssertion(ObjectAllValuesFrom(:r ObjectComplementOf(:P)) :b)\n  ClassAssertion(ObjectAllValuesFrom(:r ObjectComplementOf(:P)) :c)\n)";
            let doc = crate::ofn_read::parse(ofn).unwrap();
            let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
            let mut sat = Saturation::new();
            let out = with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                sat.classify(&refs, None, &lim_split).unwrap()
            });
            assert!(out.complete);
            assert_eq!(out.consistency, Consistency::Inconsistent);
            assert!(
                out.stats.dl_rooted_splits >= 1 && out.stats.dl_rooted_leaves >= 2,
                "expected real fork with >= 2 inconsistent leaves: {:?}",
                out.stats
            );
            assert!(
                sat.split_answer.is_some(),
                "0-consistent-leaf forked run must retain SplitAnswer"
            );
            let ghost = NamedIndividual::new("http://example.org/z#ghost");
            let boom = Class::new("http://example.org/z#Boom");
            assert!(
                sat.describe(&boom).unwrap().instances.contains(&ghost),
                "forked 0-consistent-leaf SplitAnswer must list :ghost for :Boom"
            );
            assert!(
                sat.describe(&Class::nothing())
                    .unwrap()
                    .instances
                    .contains(&ghost),
                "forked 0-consistent-leaf SplitAnswer must list :ghost for owl:Nothing"
            );
        }

        // 2. `max_depth = 1` and `max_leaves = 2` cap exhaustion: forks once (`splits == 1`,
        //    `leaves == 2`) and runs cap-bound branches with `SplitMode::Disabled` without
        //    dropping siblings.
        {
            let ofn = "Prefix(:=<http://example.org/cap#>)\nOntology(<http://example.org/cap>\n  ClassAssertion(ObjectOneOf(:b :c) :a)\n  ClassAssertion(ObjectOneOf(:e :f) :d)\n  ClassAssertion(:P :b)\n  ClassAssertion(:P :c)\n  SubClassOf(:A ObjectOneOf(:a))\n)";
            let doc = crate::ofn_read::parse(ofn).unwrap();
            let mut k_depth = Classifier::of(&doc.axioms);
            let r_depth = with_split_knobs(Some(0), Some(true), Some(1), Some(16), || {
                k_depth.classify(&lim_split).unwrap()
            });
            assert!(r_depth.complete);
            assert_eq!(r_depth.stats.dl_rooted_splits, 1);
            assert_eq!(r_depth.stats.dl_rooted_leaves, 2);
            assert!(r_depth.taxonomy.is_subclass(
                &Class::new("http://example.org/cap#A"),
                &Class::new("http://example.org/cap#P"),
            ));

            let mut k_leaves = Classifier::of(&doc.axioms);
            let r_leaves = with_split_knobs(Some(0), Some(true), Some(8), Some(2), || {
                k_leaves.classify(&lim_split).unwrap()
            });
            assert!(r_leaves.complete);
            assert_eq!(r_leaves.stats.dl_rooted_splits, 1);
            assert_eq!(r_leaves.stats.dl_rooted_leaves, 2);
            assert_eq!(r_leaves.taxonomy, r_depth.taxonomy);
        }

        // 3. `ExcludedOrIncomplete` disabled-split retry after a paid prefix/fork:
        //    (a) remaining > 0 -> disabled-split retry carries exact `spent` into
        //        `run_merging_with_carry` against the frozen `bound`, completes with
        //        original-input exclusion (and strict mode returns `Error::Incomplete` with
        //        `describe == None`);
        //    (b) remaining == 0 -> reaches `SplitExcludedAbort { remaining: 0 }` and sets
        //        `dl_rooted_over` without granting a fresh budget.
        {
            let ofn = "Prefix(:=<http://example.org/ex#>)\nOntology(<http://example.org/ex>\n  ClassAssertion(ObjectOneOf(:b :c) :a)\n  IrreflexiveObjectProperty(:irref)\n  ClassAssertion(:P :b)\n  ClassAssertion(:P :c)\n  SubClassOf(:A ObjectOneOf(:a))\n)";
            let doc = crate::ofn_read::parse(ofn).unwrap();
            let mut k_rem = Classifier::of(&doc.axioms);
            let rem_events = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
            let rem_events_cb = rem_events.clone();
            let out_rem = admission_test::with_merge_probe(
                move |p| rem_events_cb.lock().push(p),
                || {
                    with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                        k_rem
                            .classify(&Limits {
                                dl_work: Some(50_000),
                                ..lim_split.clone()
                            })
                            .unwrap()
                    })
                },
            );
            assert!(!out_rem.complete);
            assert!(!out_rem.stats.dl_rooted_over);
            assert_eq!(k_rem.excluded().len(), 1);
            assert!(out_rem.taxonomy.is_subclass(
                &Class::new("http://example.org/ex#A"),
                &Class::new("http://example.org/ex#P"),
            ));
            let (abort_spent, abort_bound, abort_rem) = rem_events
                .lock()
                .iter()
                .find_map(|p| match *p {
                    admission_test::MergePhase::SplitExcludedAbort {
                        spent,
                        bound,
                        remaining,
                    } => Some((spent, bound, remaining)),
                    _ => None,
                })
                .expect("run (a) must emit SplitExcludedAbort");
            assert!(
                abort_spent > 0,
                "expected paid prefix/leaf work before abort"
            );
            assert_eq!(abort_bound, 50_000);
            assert_eq!(abort_rem, abort_bound - abort_spent);
            assert_eq!(
                out_rem.stats.dl_rooted_spent, abort_spent,
                "disabled-split retry must carry exact spent work into run_merging_with_carry"
            );

            // Strict mode on the same ontology returns `Err(Error::Incomplete)` and clears describe.
            let mut k_strict = Classifier::of(&doc.axioms);
            let err = with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                k_strict
                    .classify(&Limits {
                        dl_work: Some(50_000),
                        strict: true,
                        ..lim_split.clone()
                    })
                    .unwrap_err()
            });
            assert!(matches!(err, Error::Incomplete(_)), "got {err:?}");
            assert!(
                k_strict
                    .describe(&Class::new("http://example.org/ex#A"))
                    .is_none()
            );

            // Zero remaining budget: set `dl_work` to the exact `abort_spent` observed at
            // `SplitExcludedAbort` above so saturation finishes the leaf (`work == bound`,
            // not `> bound`) and enters `SplitExcludedAbort` with `remaining == 0`.
            let mut k_zero = Classifier::of(&doc.axioms);
            let zero_events = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
            let zero_events_cb = zero_events.clone();
            let out_zero = admission_test::with_merge_probe(
                move |p| zero_events_cb.lock().push(p),
                || {
                    with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                        k_zero
                            .classify(&Limits {
                                dl_work: Some(abort_spent),
                                ..lim_split.clone()
                            })
                            .unwrap()
                    })
                },
            );
            assert!(
                out_zero.stats.dl_rooted_over,
                "zero remaining budget on ExcludedAbort must report dl_rooted_over: {:?}",
                out_zero.stats
            );
            let zero_abort = zero_events.lock().iter().find_map(|p| match *p {
                admission_test::MergePhase::SplitExcludedAbort {
                    spent,
                    bound,
                    remaining,
                } => Some((spent, bound, remaining)),
                _ => None,
            });
            assert_eq!(
                zero_abort,
                Some((abort_spent, abort_spent, 0)),
                "zero-remaining case must anchor to SplitExcludedAbort {{ remaining: 0 }}"
            );
        }

        // 4. `ground_total = false` control on `shoi-042` (design §4.1.3): Layer B does not
        //    depend logically on `ground_total`.
        {
            let mut k_gt_on = Classifier::of(&shoi042);
            let on = k_gt_on.classify(&lim_split).unwrap();
            let mut k_gt_off = Classifier::of(&shoi042);
            let off =
                dl_clause::with_ground_total(false, || k_gt_off.classify(&lim_split).unwrap());
            assert!(off.complete && !off.stats.dl_rooted_over && !off.stats.dl_rooted_declined);
            assert!(off.stats.dl_rooted_splits >= 1);
            assert_eq!(off.taxonomy, on.taxonomy);
            assert_eq!(
                k_gt_off
                    .describe(&Class::new("http://example.org/d#C0"))
                    .unwrap()
                    .instances,
                k_gt_on
                    .describe(&Class::new("http://example.org/d#C0"))
                    .unwrap()
                    .instances
            );
        }
    }

    /// Design v12 §4.3.4 + Sol r1 P2 #1 + Neon r1 NB3.2: end-to-end between-rounds
    /// `dl_rules::test_admitted` refusal in `Saturation::run_once` for BOTH `partial = false`
    /// (returns `Err(Error::Memory)` with no test round launched) and `partial = true`
    /// (returns partial answer leaving affected root open, with no test round launched).
    #[test]
    fn between_rounds_test_admitted_driver_refusal_handles_partial_false_and_true() {
        let ofn = "Prefix(:=<http://example.org/br#>)\nOntology(<http://example.org/br>\n  SubClassOf(:A ObjectUnionOf(:B :C))\n  SubClassOf(:B :D)\n  SubClassOf(:C :D)\n)";
        let doc = crate::ofn_read::parse(ofn).unwrap();
        let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();

        // Baseline: confirm this ontology reaches the between-rounds test loop (`dl_tests >= 1`).
        let mut sat_base = Saturation::new();
        let base_lim = Limits {
            threads: 1,
            ..Limits::default()
        };
        let base_out = sat_base.classify(&refs, None, &base_lim).unwrap();
        assert!(base_out.complete);
        assert!(
            base_out.stats.dl_tests >= 1,
            "fixture must exercise between-rounds test loop: {:?}",
            base_out.stats
        );
        let a_cls = Class::new("http://example.org/br#A");
        let d_cls = Class::new("http://example.org/br#D");
        assert!(base_out.taxonomy.is_subclass(&a_cls, &d_cls));

        // Drive `run_once` with a phase-gated `charge` callback that lets normalization and
        // primary saturation succeed untouched, and refuses ONLY after `BetweenRoundsTestStart`
        // is emitted right before `dl_rules::test_admitted`.
        let run_with_partial = |partial: bool| {
            let mut sat = Saturation::new();
            let in_between_rounds = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let refused_in_phase = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let saw_phase_refused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let in_br_charge = in_between_rounds.clone();
            let refused_flag = refused_in_phase.clone();
            let charge: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                std::sync::Arc::new(move |_more: usize| {
                    if in_br_charge.load(Ordering::Acquire)
                        && !refused_flag.swap(true, Ordering::AcqRel)
                    {
                        return false;
                    }
                    true
                });
            let lim = Limits {
                threads: 1,
                partial,
                charge: Some(charge),
                ..Limits::default()
            };
            let in_br_probe = in_between_rounds.clone();
            let saw_ref_probe = saw_phase_refused.clone();
            let res = admission_test::with_merge_probe(
                move |phase| match phase {
                    admission_test::MergePhase::BetweenRoundsTestStart => {
                        in_br_probe.store(true, Ordering::Release);
                    }
                    admission_test::MergePhase::BetweenRoundsTestRefused => {
                        in_br_probe.store(false, Ordering::Release);
                        saw_ref_probe.store(true, Ordering::Release);
                    }
                    _ => {}
                },
                || sat.run_once(&refs, None, 0, &lim, true),
            );
            (
                sat,
                res,
                refused_in_phase.load(Ordering::Acquire),
                saw_phase_refused.load(Ordering::Acquire),
            )
        };

        // 1. `partial = false` -> returns `Err(Error::Memory { .. })`, refused specifically in
        //    `dl_rules::test_admitted`, zero test rounds launched.
        let (sat_full, res_full, hit_full, phase_full) = run_with_partial(false);
        assert!(
            hit_full,
            "charge callback must have refused inside between-rounds loop"
        );
        assert!(
            phase_full,
            "BetweenRoundsTestRefused must fire inside dl_rules::test_admitted"
        );
        assert!(
            matches!(res_full, Err(Error::Memory { .. })),
            "partial=false must return Err(Error::Memory) on between-rounds refusal, got {res_full:?}"
        );
        assert!(
            sat_full.dynamic.as_ref().is_none_or(|d| d.len() == 0),
            "refused test_admitted must publish no Dynamic test ID"
        );

        // 2. `partial = true` -> returns `Ok(Some(...))` partial answer with affected root `:A`
        //    open (`sat.partial.is_some()`), refused specifically in `dl_rules::test_admitted`,
        //    zero test rounds launched (`stats.dl_tests == 0`)!
        let (sat_part, res_part, hit_part, phase_part) = run_with_partial(true);
        assert!(
            hit_part,
            "charge callback must have refused inside between-rounds loop"
        );
        assert!(
            phase_part,
            "BetweenRoundsTestRefused must fire inside dl_rules::test_admitted"
        );
        let (_, tax_opt, stats) = res_part
            .expect("partial=true must not error on between-rounds test_admitted refusal")
            .expect("partial=true must return a partial classification");
        assert!(tax_opt.is_some());
        assert_eq!(
            stats.dl_tests, 0,
            "refused test_admitted must break before launching the test round"
        );
        let (open_classes, _) = sat_part
            .partial
            .as_ref()
            .expect("partial=true refusal must record open incomplete classes");
        assert!(
            open_classes.contains(&a_cls),
            "partial answer must leave affected root :A open, got {open_classes:?}"
        );
        assert!(
            sat_part.dynamic.as_ref().is_none_or(|d| d.len() == 0),
            "refused test_admitted must publish no Dynamic test ID"
        );
    }

    /// Sol r2 P2 #2 + Sol r3 P2 #2: four-thread unstepped (`dl_steps = false`) `shoi-042`
    /// sweep verifying for EACH expansion mode that a helper worker actually processes work
    /// inside a `STOP_SPLIT`-producing saturation (`SatStop { stop: STOP_SPLIT, helper_worked: true }`),
    /// capturing and validating the selected canonical fork pairs `(u, v)`, checking zero-residual
    /// post-leaf resets, and verifying exact taxonomy + `describe` parity.
    #[test]
    fn four_thread_unstepped_shoi042_records_helper_activity_and_split_checkpoints() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
        let shoi042 =
            crate::ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap())
                .unwrap()
                .axioms;
        let mut valid_inds: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for a in &shoi042 {
            crate::model::Signature::visit_classes_and_individuals(
                &a.kind,
                &mut |_| {},
                &mut |n| {
                    valid_inds.insert(n.as_str().to_owned());
                },
            );
        }
        let lim_ref = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            dl_steps: true,
            ..Limits::default()
        };
        let ref_pairs = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<(
            usize,
            usize,
            u8,
            String,
            String,
        )>::new()));
        let rp_cb = ref_pairs.clone();
        let mut k_ref = Classifier::of(&shoi042);
        let out_ref = admission_test::with_split_pair_probe(
            move |splits, depth, prio, u, v| {
                rp_cb
                    .lock()
                    .push((splits, depth, prio, u.to_owned(), v.to_owned()));
            },
            || k_ref.classify(&lim_ref).unwrap(),
        );
        let ref_pairs = ref_pairs.lock().clone();
        assert!(
            !ref_pairs.is_empty(),
            "reference 1-thread shoi-042 must emit at least one split pair"
        );
        let c0 = Class::new("http://example.org/d#C0");
        let ref_c0 = k_ref.describe(&c0).unwrap().instances;

        for expansion in [Expansion::Cautious, Expansion::Eager] {
            let mut saw_split_with_helper_work = false;
            let mut saw_fork_pairs = false;
            let mut saw_post_leaf_reset = false;
            // Retry up to 6 full runs if OS scheduler jitter keeps all work on worker 0
            // during the split-producing saturation of one run; never accumulate across runs.
            for _attempt in 0..6 {
                let events = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
                let ev_cb = events.clone();
                let run_pairs = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<(
                    usize,
                    usize,
                    u8,
                    String,
                    String,
                )>::new(
                )));
                let pair_cb = run_pairs.clone();
                let mut k = Classifier::of(&shoi042);
                let out = admission_test::with_split_pair_probe(
                    move |splits, depth, prio, u, v| {
                        pair_cb
                            .lock()
                            .push((splits, depth, prio, u.to_owned(), v.to_owned()));
                    },
                    || {
                        admission_test::with_merge_probe(
                            move |p| ev_cb.lock().push(p),
                            || {
                                k.classify(&Limits {
                                    threads: 4,
                                    expansion,
                                    rooted: true,
                                    rooted_merge: true,
                                    rooted_eq_split: true,
                                    dl_steps: false,
                                    ..Limits::default()
                                })
                                .unwrap()
                            },
                        )
                    },
                );
                assert!(out.complete && !out.stats.dl_rooted_over && !out.stats.dl_rooted_declined);
                assert_eq!(out.taxonomy, out_ref.taxonomy);
                assert_eq!(k.describe(&c0).unwrap().instances, ref_c0);

                let pairs = run_pairs.lock().clone();
                assert!(
                    !pairs.is_empty(),
                    "4-thread {expansion:?} must capture selected split pairs"
                );
                for (splits, depth, prio, u, v) in &pairs {
                    assert!(*splits >= 1 && *depth >= 1 && *prio >= 1);
                    assert!(
                        u < v,
                        "selected fork pair must be canonically ordered: ({u}, {v})"
                    );
                    assert!(
                        valid_inds.contains(u) && valid_inds.contains(v),
                        "selected fork pair must name original individuals: ({u}, {v})"
                    );
                }
                assert!(
                    pairs.iter().any(|(_, _, _, u, v)| ref_pairs
                        .iter()
                        .any(|(_, _, _, ru, rv)| ru == u && rv == v)),
                    "4-thread {expansion:?} fork pairs {pairs:?} should overlap stepped reference pairs {ref_pairs:?}"
                );
                saw_fork_pairs = true;

                let mut this_run_split_with_helper = false;
                let mut this_run_fork = false;
                let mut this_run_reset = false;
                let ev = events.lock();
                for p in ev.iter() {
                    match *p {
                        admission_test::MergePhase::SatStop {
                            stop,
                            helper_worked,
                            ..
                        } => {
                            if stop == saturate::STOP_SPLIT && helper_worked {
                                this_run_split_with_helper = true;
                            }
                        }
                        admission_test::MergePhase::SplitFork { splits, depth, .. } => {
                            assert!(splits >= 1 && depth >= 1);
                            this_run_fork = true;
                        }
                        admission_test::MergePhase::SplitPostLeafReset {
                            canon_want_none,
                            leaf_inds_cap,
                            interner_bytes,
                            residual_bytes,
                        } => {
                            assert!(canon_want_none, "leaf reset must drop canon_want");
                            assert_eq!(leaf_inds_cap, 0, "leaf reset must free leaf_individuals");
                            assert_eq!(
                                interner_bytes, 0,
                                "leaf reset must leave zero replacement interner bytes"
                            );
                            assert_eq!(
                                residual_bytes, 0,
                                "leaf reset must leave zero residual outer bytes"
                            );
                            this_run_reset = true;
                        }
                        _ => {}
                    }
                }
                assert!(
                    this_run_fork && this_run_reset,
                    "every 4-thread {expansion:?} run must fork and reset cleanly"
                );
                saw_post_leaf_reset = true;
                if this_run_split_with_helper {
                    saw_split_with_helper_work = true;
                    break;
                }
            }
            assert!(
                saw_split_with_helper_work && saw_fork_pairs && saw_post_leaf_reset,
                "4-thread unstepped {expansion:?} must observe helper work inside a STOP_SPLIT saturation, captured fork pairs, and zero-residual leaf resets"
            );
        }
    }
}

#[cfg(test)]
#[path = "a1_probe_alloc.rs"]
mod a1_probe_alloc;
