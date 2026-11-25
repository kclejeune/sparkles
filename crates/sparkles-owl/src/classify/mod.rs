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
mod polled;
mod query;
#[cfg(test)]
mod retained_accounting_tests;
mod saturate;
#[cfg(test)]
mod scratch_admission_tests;
#[cfg(test)]
mod shared_world_found_row_tests;
mod taxonomy;
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
    /// the high-water mark that the last completed [`Saturation::run_once`] paid to the
    /// memory pool, used to offset post-run original-axiom accounting on kept rewrites
    last_paid: usize,
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

/// Rough additional ownership of a retained copy of exclusions. This uses the
/// classifier's held-axiom estimate and is not a process RSS bound.
pub fn excluded_copy_bytes(excluded: &Vec<Excluded>) -> usize {
    use std::mem::size_of;
    excluded
        .capacity()
        .saturating_mul(size_of::<Excluded>())
        .saturating_add(
            excluded
                .iter()
                .map(|e| {
                    held_bytes_of(&e.axiom).saturating_add(match &e.gap {
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

type RunOutcome = ControlFlow<LateRestart, (bool, Option<Taxonomy>, Stats)>;

/// Distinguishes the caller's original default attempt from rewrites and fallbacks.
#[derive(Clone, Copy)]
enum AttemptProvenance {
    OriginalDefault,
    Ineligible,
}

/// Why an incremental attempt must start over after building its index.
enum LateRestart {
    Incompatible,
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
            last_paid: 0,
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
        let counted;
        let first = match &limits.charge {
            Some(f) => {
                let (f, a) = (f.clone(), admitted.clone());
                counted = Limits {
                    charge: Some(std::sync::Arc::new(move |bytes: usize| {
                        let ok = f(bytes);
                        if ok {
                            a.fetch_add(bytes, Ordering::Relaxed);
                        }
                        ok
                    })),
                    ..limits.clone()
                };
                &counted
            }
            None => limits,
        };
        self.dl_worked.store(false, Ordering::Relaxed);
        self.rooted_declined = false;
        let attempt = if limits.el_only || limits.rooted {
            AttemptProvenance::Ineligible
        } else {
            AttemptProvenance::OriginalDefault
        };
        let mut r = self.run(axioms, changes, held_bytes, first, with_taxonomy, attempt);
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
            self.dl_worked.store(false, Ordering::Relaxed);
            self.rooted_declined = false;
            let a = admitted.clone();
            let counting = limits.charge.clone().map(|f| {
                let f: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                    std::sync::Arc::new(move |bytes: usize| {
                        let ok = f(bytes);
                        if ok {
                            a.fetch_add(bytes, Ordering::Relaxed);
                        }
                        ok
                    });
                drawing_on(admitted.load(Ordering::Relaxed), f)
            });
            let again = Limits {
                charge: counting,
                ..limits.clone()
            };
            r = self.run(axioms, None, held_bytes, &again, with_taxonomy, attempt);
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
            // (`Limits::dl_steps`) derives the same on every schedule and does not repeat. Its charges draw first on what the pool admitted
            // for the first run.
            self.reset();
            self.dl_worked.store(false, Ordering::Relaxed);
            let a = admitted.clone();
            let counting = limits.charge.clone().map(|f| {
                let f: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                    std::sync::Arc::new(move |bytes: usize| {
                        let ok = f(bytes);
                        if ok {
                            a.fetch_add(bytes, Ordering::Relaxed);
                        }
                        ok
                    });
                drawing_on(admitted.load(Ordering::Relaxed), f)
            });
            let one = Limits {
                threads: 1,
                charge: counting,
                ..limits.clone()
            };
            r = self.run(
                axioms,
                None,
                held_bytes,
                &one,
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
            self.dl_worked.store(false, Ordering::Relaxed);
            self.no_counting = true;
            let a = admitted.clone();
            let counting = limits.charge.clone().map(|f| {
                let f: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                    std::sync::Arc::new(move |bytes: usize| {
                        let ok = f(bytes);
                        if ok {
                            a.fetch_add(bytes, Ordering::Relaxed);
                        }
                        ok
                    });
                drawing_on(admitted.load(Ordering::Relaxed), f)
            });
            let without = Limits {
                charge: counting,
                ..limits.clone()
            };
            r = self.run(
                axioms,
                None,
                held_bytes,
                &without,
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
            self.reset();
            self.dl_worked.store(false, Ordering::Relaxed);
            self.no_rooted = true;
            let a = admitted.clone();
            let counting = limits.charge.clone().map(|f| {
                let f: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                    std::sync::Arc::new(move |bytes: usize| {
                        let ok = f(bytes);
                        if ok {
                            a.fetch_add(bytes, Ordering::Relaxed);
                        }
                        ok
                    });
                drawing_on(admitted.load(Ordering::Relaxed), f)
            });
            let unrooted = Limits {
                charge: counting,
                ..limits.clone()
            };
            r = self.run(
                axioms,
                None,
                held_bytes,
                &unrooted,
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
            self.reset();
            self.skip_dl = true;
            let a = admitted.clone();
            let counting = limits.charge.clone().map(|f| {
                let f: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                    std::sync::Arc::new(move |bytes: usize| {
                        let ok = f(bytes);
                        if ok {
                            a.fetch_add(bytes, Ordering::Relaxed);
                        }
                        ok
                    });
                drawing_on(admitted.load(Ordering::Relaxed), f)
            });
            let horn = Limits {
                charge: counting,
                ..limits.clone()
            };
            r = self.run(
                axioms,
                None,
                held_bytes,
                &horn,
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
            self.reset();
            let el = Limits {
                el_only: true,
                charge: limits
                    .charge
                    .clone()
                    .map(|f| drawing_on(admitted.load(Ordering::Relaxed), f)),
                ..limits.clone()
            };
            r = self
                .run(
                    axioms,
                    None,
                    held_bytes,
                    &el,
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
        self.reset();
        self.dl_worked.store(false, Ordering::Relaxed);
        self.no_rooted = true;
        let a = admitted.clone();
        let counting = limits.charge.clone().map(|f| {
            let f: std::sync::Arc<dyn Fn(usize) -> bool + Send + Sync> =
                std::sync::Arc::new(move |bytes: usize| {
                    let ok = f(bytes);
                    if ok {
                        a.fetch_add(bytes, Ordering::Relaxed);
                    }
                    ok
                });
            drawing_on(admitted.load(Ordering::Relaxed), f)
        });
        let unrooted = Limits {
            charge: counting,
            ..limits.clone()
        };
        *r = self.run(
            axioms,
            None,
            held_bytes,
            &unrooted,
            with_taxonomy,
            AttemptProvenance::Ineligible,
        );
        declined.get_or_insert(used);
    }

    /// Drops the state of a run that stopped part way, since the next run starts again.
    fn reset(&mut self) {
        self.index = None;
        self.interner = Interner::new();
        self.stale_tier = false;
        self.ctx = Vec::new();
        self.disjoint = Vec::new();
        self.dynamic = None;
        self.dl_run = None;
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
    ) -> Result<(Axioms, Index, usize, OutsideEl), Error> {
        let budget = numeric::Budget::new(
            limits,
            held_bytes + kept_bytes,
            self.interner.ranges.ledger.clone(),
        );
        budget.baseline(self.interner.bytes())?;
        self.interner.ranges.begin(budget.clone());
        let result = self.build_inner(axioms, outside, threads, held_bytes, limits, attempt);
        // the run's components and definitions end with its normalization, on every exit
        self.interner.dt_run = None;
        self.numeric_halt |= self.interner.ranges.check().is_err();
        self.interner.ranges.end();
        result.map(|(ax, ix, peak, outside)| (ax, ix, peak.max(budget.peak()), outside))
    }

    fn build_inner(
        &mut self,
        axioms: &[&AxiomKind],
        outside: &[(usize, (String, String))],
        threads: usize,
        held_bytes: usize,
        limits: &Limits,
        attempt: AttemptProvenance,
    ) -> Result<(Axioms, Index, usize, OutsideEl), Error> {
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
                base: held_bytes + self.interner.bytes(),
                held: AtomicUsize::new(0),
                peak: parking_lot::Mutex::new(0),
                over: std::sync::atomic::AtomicBool::new(false),
            };
            let numeric = self.interner.ranges.budget.clone();
            let definitions = &self.interner.ranges.definitions;
            let is_outside = &is_outside;
            let parts = par_map(axioms, threads, MIN_AXIOMS, |at, chunk| {
                let mut it = Interner::chunk(definitions);
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
        ax.classes.extend(self.interner.met_classes());
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
        Ok((ax, ix, peak, outside_el))
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
    fn run(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        with_taxonomy: bool,
        attempt: AttemptProvenance,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        if limits.rooted
            && !self.no_rooted
            && !limits.el_only
            && !self.skip_dl
            && let Some(red) = dl_reduce::plan(axioms, limits.max_memory)
        {
            // item 805: keys over object properties and the universal property go through
            // class axioms over fresh names; keep the run when Tier D took it, or when
            // the rewriting turned the ontology Horn or EL and every rewritten axiom was
            // taken (item 1313)
            let all = red.all(axioms);
            // the interner of an earlier run holds names and individuals that decide the
            // tiers, so the rewritten axioms start from nothing
            self.reset();
            // the rewritten axioms are held beside the caller's for the run
            let red_bytes: usize = red.axioms.iter().map(held_bytes_of).sum();
            self.hide_internal = true;
            let r = self.run_once(
                &all,
                None,
                held_bytes + red_bytes,
                limits,
                with_taxonomy,
                AttemptProvenance::Ineligible,
            );
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
                    // Rewritten axioms are no longer live; keep the earlier rewrite peak
                    // and paid high-water mark intact while post-run scratch admits against
                    // the live retained baseline.
                    r.2.memory = r.2.memory.saturating_sub(red_bytes);
                    r.2.added = self.source_len.unwrap_or(axioms.len());
                    let charge = limits
                        .charge
                        .as_deref()
                        .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
                    let mut paid = self.last_paid;
                    let mut scratch = PostRunScratch {
                        base: r.2.memory,
                        peak: &mut r.2.peak_memory,
                        paid: &mut paid,
                        limits,
                        charge,
                        check_stop: self.partial.is_none(),
                        scratch: 0,
                    };
                    #[cfg(test)]
                    admission_test::hit(admission_test::Point::PostRunStart);
                    match dl_reduce::Reduction::outside_el(axioms, &self.excluded, &mut scratch) {
                        Ok(outside_el) => {
                            self.last_paid = paid;
                            r.2.outside_el = outside_el;
                            r.2.tier = r.2.outside_el.tier();
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
                    // the report of a run that stopped still names the original axioms
                    red.restore(axioms, &mut self.excluded);
                    self.reduced = true;
                    return Err(e);
                }
                _ => {
                    red.restore(axioms, &mut self.excluded);
                    self.reset();
                }
            }
        }
        // after a run of rewritten axioms, whose made-up names and hub individual the
        // interner still holds, the run starts from nothing
        let was = std::mem::take(&mut self.reduced);
        if was {
            self.reset();
        }
        let changes = changes.filter(|_| !was);
        let restart =
            match self.run_once(axioms, changes, held_bytes, limits, with_taxonomy, attempt)? {
                ControlFlow::Continue(r) => return Ok(r),
                ControlFlow::Break(restart) => restart,
            };
        self.reset();
        let credited;
        let (limits, discarded_peak) = match restart {
            LateRestart::Incompatible => (limits, 0),
            LateRestart::NamedDomain { normalized_peak } => match &limits.charge {
                Some(charge) => {
                    // The successful build's one numeric budget approved exactly this
                    // much growth at the incoming callback, including inherited credit.
                    credited = Limits {
                        charge: Some(drawing_on(normalized_peak, charge.clone())),
                        ..limits.clone()
                    };
                    (&credited, normalized_peak)
                }
                None => (limits, 0),
            },
        };
        let ControlFlow::Continue(mut r) =
            self.run_once(axioms, None, held_bytes, limits, with_taxonomy, attempt)?
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
        let had_top_chain = self.index.as_ref().is_some_and(|ix| ix.top_chain.is_some());
        let full = self.entangled
            || had_top_chain
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
        self.worlds.clear();
        if full {
            self.interner = Interner::new();
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
            + self.dl_run.as_ref().map_or(0, |d| d.bytes());
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
        let (mut ax, mut ix, mut normalized_peak, outside_el) = self.build(
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
            return Ok(ControlFlow::Break(LateRestart::Incompatible));
        }
        let reuse = match &old_ix {
            Some(old) => {
                if !ix.same_tables(old) {
                    return Ok(ControlFlow::Break(LateRestart::Incompatible));
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
        let dl_run = dl_run.map(|mut d| {
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
        if let Some(d) = &dl_run {
            // a run that counts has the bound of the limits, and the run that starts again
            // without counting after it passed that bound has the larger one of a run that
            // cannot expect to finish soon, before the Horn rules take over; a rooted run
            // without counting has its own bound, past which the run starts again without
            // the rooted rules
            // only an attempt has a limit on the choices of a product
            d.set_width_limit(0);
            if self.dl_rooted && !self.dl_counted {
                d.set_work_limit(limits.dl_work.unwrap_or(ROOTED_WORK_BOUND).max(1));
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
            let own = HORN_MEMORY_FLOOR.max(base.saturating_mul(HORN_MEMORY_FACTOR));
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
            Stop::Memory { used } | Stop::Work { used } => Error::Memory {
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

    fn index_bytes(&self, ix: &Index) -> usize {
        // each class's IRI is held once, beside its hash and its entry in the table
        self.interner.len() * 40
            + self.interner.classes.bytes()
            + self.interner.classes.name_bytes()
            + self.interner.data_bytes()
            + ix.numeric_ledger
                .as_ref()
                .or(self.interner.ranges.ledger.as_ref())
                .map_or(0, |l| l.bytes())
            + ix.bytes()
            + self.interner.scope_bytes()
            + self.disjoint.capacity() * 24
            + self
                .disjoint
                .iter()
                .map(|d| d.capacity() * 4)
                .sum::<usize>()
    }

    fn retained_bytes(&self, held_bytes: usize, root_bytes: usize, world_bytes: usize) -> usize {
        self.index_bytes(self.index.as_ref().expect("completed index"))
            + held_bytes
            + root_bytes
            + self.context_bytes()
            + self.dynamic.as_ref().map_or(0, |d| d.retained_bytes())
            + self.dl_run.as_ref().map_or(0, |d| d.table.bytes())
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
        let (_, ix, _, _) = s
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
        let (_, _, peak_no, _) = Saturation::new()
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
        let (_, _, peak_with, _) = Saturation::new()
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
        let (ax_miss, _, peak_miss, _) = admission_test::with_hook(
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
        let (_, _, peak_par, _) = admission_test::with_hook(
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
        let (_, _, peak_ser, _) = admission_test::with_hook(
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

        // Construct a kept-rewrite ontology with large `red_bytes` (many independent
        // rewritten axioms with linear saturation) and a smaller post-run range scan so
        // that `live_memory + scan_peak <= rewrite_peak < live_memory + red_bytes + scan_peak`.
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
        let mut sat_full = Saturation::new();

        let full = admission_test::with_hook(
            move |pt| {
                if pt == admission_test::Point::PostRunStart {
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

        // Explicitly establish that the live baseline + scan peak fits under `before_scan`,
        // while restoring `red_bytes` would exceed `before_scan`.
        let red = dl_reduce::plan(&kinds, None).expect("expected rewrite plan");
        let red_bytes: usize = red.axioms.iter().map(held_bytes_of).sum();
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
        assert!(
            before_scan < full.stats.memory + red_bytes + scan_peak,
            "cap ({before_scan}) must sit strictly below stale baseline + scan ({})",
            full.stats.memory + red_bytes + scan_peak
        );

        // A cap equal to the rewrite peak covers both the rewrite peak and live + scan,
        // while sitting strictly below live + red_bytes + scan.
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
        let (_, _, peak_with, _) = admission_test::with_hook(
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
        let (_, _, wide_peak_with, _) = admission_test::with_hook(
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
        let ((ax_accepted_no, _, _, _), sample_accepted_no) = a1_probe_alloc::measure(|| {
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
        let (_, _, acc_peak_with, _) = admission_test::with_hook(
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
}

#[cfg(test)]
#[path = "a1_probe_alloc.rs"]
mod a1_probe_alloc;
