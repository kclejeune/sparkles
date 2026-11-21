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

mod data;
mod dl;
mod dl_clause;
mod dl_eq;
mod dl_norm;
mod dl_reduce;
mod dl_root;
mod dl_rules;
mod horn;
mod horn_rules;
mod index;
mod numeric;
mod query;
#[cfg(test)]
mod retained_accounting_tests;
mod saturate;
mod taxonomy;
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
    /// [`complete`](Classification::complete) is false, instead of failing. Runs of the
    /// other tiers fail as before.
    pub partial: bool,
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
    pub taxonomy: Duration,
    /// the axioms outside OWL 2 EL, by what classifying them needs, with those the Horn
    /// rules of roadmap item 603 take; the others are left out and reported
    pub outside_el: OutsideEl,
    /// the rules the run's axioms need
    pub tier: Tier,
    /// the contexts of conjunctions that the Horn rules created, which
    /// [`contexts`](Stats::contexts) includes
    pub conjunctions: usize,
    /// the Horn rules passed their memory budget, so the run classified by the OWL 2 EL
    /// rules alone, as [`Limits::el_only`] does, and left the axioms outside OWL 2 EL out
    pub horn_over_budget: bool,
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
    /// false for the partial answer of a run that stopped ([`Limits::partial`]): its
    /// subsumptions are entailed, and those of the classes not in
    /// [`incomplete`](Classification::incomplete) are all that hold
    pub complete: bool,
    /// the classes whose subsumers a partial answer may miss, in IRI order
    pub incomplete: Vec<Class>,
    /// whether the ontology has a model, as far as the run knows: a partial answer can
    /// leave it unknown, and then no class is complete
    pub consistency: Consistency,
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
            incomplete,
            consistency,
        }
    }
}

/// An axiom the classifier leaves out or uses only in part, and why.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Excluded {
    pub axiom: AxiomKind,
    pub gap: Gap,
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
    /// whether the current run keeps contexts of a run with the Horn rules or Tier D,
    /// so that a stop on its memory budget starts again in full before a lower tier
    /// takes over
    reused: bool,
    /// the clause table of the last run of Tier D whose contexts the next run may keep,
    /// which name its clauses by id
    dl_run: Option<dl_rules::DlRun>,
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
/// derives from every data axiom at once, a key, or one with an individual, whose
/// merges the invalidation does not follow.
fn needs_full_run(ax: &AxiomKind) -> bool {
    if role_axiom(ax) || matches!(ax, AxiomKind::HasKey(..)) {
        return true;
    }
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    sig.0.iter().any(|e| {
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
                    Node::Nominal(_) | Node::SelfLoop(_) | Node::DataSome(..) | Node::AtMost1(..)
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
        r.map(|(consistent, taxonomy, stats)| {
            Classification::new(consistent, taxonomy, stats, partial)
        })
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
            excluded: Vec::new(),
            dirty: true,
            worlds: FxHashMap::default(),
            entangled: false,
            stale_tier: false,
            consistent: true,
            dynamic: None,
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
        r.map(|(consistent, taxonomy, stats)| {
            Classification::new(consistent, taxonomy, stats, partial)
        })
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
        // what a pool admitted for a run that may fall back, so that the EL run charges
        // only what it needs beyond that
        let admitted = std::sync::Arc::new(AtomicUsize::new(0));
        let counted;
        let first = match &limits.charge {
            Some(f) if !limits.el_only => {
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
            _ => limits,
        };
        self.dl_worked.store(false, Ordering::Relaxed);
        self.rooted_declined = false;
        let mut r = self.run(axioms, changes, held_bytes, first, with_taxonomy);
        if let Err(Error::Memory { .. }) = r
            && self.reused
        {
            // A run that kept contexts of a run with the Horn rules or Tier D passed its
            // budget, which a run that starts again may not: the last index, which it
            // holds until the new one is built, and what it keeps of the contexts that
            // nothing reaches any more count against the budget too. The run starts
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
            r = self.run(axioms, None, held_bytes, &again, with_taxonomy);
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
            r = self.run(axioms, None, held_bytes, &one, with_taxonomy);
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
            r = self.run(axioms, None, held_bytes, &without, with_taxonomy);
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
            r = self.run(axioms, None, held_bytes, &unrooted, with_taxonomy);
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
            r = self.run(axioms, None, held_bytes, &horn, with_taxonomy);
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
            r = self.run(axioms, None, held_bytes, &el, with_taxonomy).map(
                |(consistent, taxonomy, mut stats)| {
                    stats.horn_over_budget = true;
                    if let Some(d) = dl_over {
                        stats.dl_over_budget = true;
                        stats.dl_attempt_over = attempt_over;
                        stats.peak_memory = stats.peak_memory.max(d);
                    }
                    // the Horn run's high-water mark, which the run reached first
                    stats.peak_memory = stats.peak_memory.max(used);
                    (consistent, taxonomy, stats)
                },
            );
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
        *r = self.run(axioms, None, held_bytes, &unrooted, with_taxonomy);
        declined.get_or_insert(used);
    }

    /// Drops the state of a run that stopped part way, since the next run starts again.
    fn reset(&mut self) {
        self.index = None;
        self.interner = Interner::new();
        self.stale_tier = false;
        self.ctx = Vec::new();
        self.dynamic = None;
        self.dl_run = None;
    }

    /// Normalizes the axioms inside OWL 2 EL and builds the index. `outside` names the
    /// axioms the profile check leaves out.
    /// The answer's last part is the estimated peak of a parallel normalization, with
    /// `held_bytes`, which the pool has been charged for, or 0.
    fn build(
        &mut self,
        axioms: &[&AxiomKind],
        outside: &[(usize, (String, String))],
        threads: usize,
        held_bytes: usize,
        limits: &Limits,
    ) -> Result<(Axioms, Index, usize, OutsideEl), Error> {
        let budget = numeric::Budget::new(
            limits,
            held_bytes + self.context_bytes(),
            self.interner.ranges.ledger.clone(),
        );
        budget.baseline(self.interner.bytes())?;
        self.interner.ranges.begin(budget.clone());
        let result = self.build_inner(axioms, outside, threads, held_bytes, limits);
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
        // Chunks of the axioms normalize on the run's threads into interners of their
        // own, which merge in order into the run's, so that every id is the one a
        // normalization on one thread gives. Data ranges and literals have ids inside
        // their values, which the merge does not map, so a run that has them normalizes
        // on one thread. So does a run whose chunks pass the memory budget or the pool's
        // grant, since each chunk holds the names it meets and the merge copies them.
        let mut merged = false;
        let mut peak = 0;
        if threads > 1 && axioms.len() >= PARALLEL_AXIOMS {
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
        let rooted = limits.rooted && !self.no_rooted;
        if !candidates.is_empty()
            && !limits.el_only
            && !self.skip_dl
            && (self.disj_allowed(&ax) || rooted && self.rooted_allowed(&ax))
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
            }
        }
        if !dl_taken && !candidates.is_empty() && !limits.el_only {
            self.horn_tried = true;
            let flags = horn::normalize(
                &candidates,
                &mut self.interner,
                &mut ax,
                limits.max_memory,
                &stop,
            )
            .map_err(halt)?;
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
        // A fresh run decides its tier before these normalizations, so what they intern
        // that the decision reads would keep a later run over this interner from Tier D.
        // A run whose rules take it starts again anyway, as `entangled` says.
        self.stale_tier |= mark.passed(&self.interner);
        if self.hide_internal {
            self.interner.hide_prefix(dl_reduce::PREFIX);
        }
        ax.classes.extend(self.interner.met_classes());
        if let Some(b) = &self.interner.ranges.budget {
            b.baseline(self.interner.bytes() + ax.bytes())?;
        }
        let ix = Index::build(&ax, &mut self.interner, limits.max_memory, &stop).map_err(halt)?;
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

    /// Whether a rooted run of Tier D may take axioms outside OWL 2 EL with individuals:
    /// when the EL part has no keys, top properties or functional data roles. A key over
    /// object properties and the universal object property reach this check only when
    /// item 805's rewriting in [`Saturation::run`] declined them.
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
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
        if limits.rooted
            && !self.no_rooted
            && !limits.el_only
            && !self.skip_dl
            && let Some(red) = dl_reduce::plan(axioms, limits.max_memory)
        {
            // item 805: keys over object properties and the universal property go through
            // class axioms over fresh names, which a run that does not take Tier D leaves
            // to the run without them
            let all = red.all(axioms);
            // the interner of an earlier run holds names and individuals that decide the
            // tiers, so the rewritten axioms start from nothing
            self.reset();
            // the rewritten axioms are held beside the caller's for the run
            let red_bytes: usize = red.axioms.iter().map(held_bytes_of).sum();
            self.hide_internal = true;
            let r = self.run_once(&all, None, held_bytes + red_bytes, limits, with_taxonomy);
            self.hide_internal = false;
            match r {
                Ok(Some(mut r)) if self.dl_tried => {
                    red.restore(axioms, &mut self.excluded);
                    r.2.added = axioms.len();
                    let tier = r.2.tier;
                    let mut chains: Vec<Excluded> = std::mem::take(&mut self.chain_ranged)
                        .into_iter()
                        .map(|axiom| Excluded {
                            axiom,
                            gap: index::Gap::Partial("chain range"),
                        })
                        .collect();
                    red.restore(axioms, &mut chains);
                    let chains: Vec<&AxiomKind> = chains.iter().map(|e| &e.axiom).collect();
                    r.2.outside_el = red.outside_el(axioms, &chains, &self.excluded);
                    r.2.tier = tier;
                    self.reduced = true;
                    return Ok(r);
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
        if let Some(r) = self.run_once(axioms, changes, held_bytes, limits, with_taxonomy)? {
            return Ok(r);
        }
        self.reset();
        let r = self.run_once(axioms, None, held_bytes, limits, with_taxonomy)?;
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
            None => stats.added = axioms.len(),
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
                        && let Some(b) = profiles::el_axiom(k).into_iter().next()
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
                    || c.added.iter().any(|k| {
                        !tier_neutral(k) && profiles::el_axiom(k).into_iter().next().is_some()
                    })
            });
        let full = self.entangled
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
        // that starts again does not hold
        self.reused = old_ix.is_some();
        // the premises of the changed axioms, read from the interner before the index of
        // the new axioms is built
        let premises = match changes {
            Some(c) if !full && old_ix.is_none() => {
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
        let (ax, mut ix, mut normalized_peak, outside_el) =
            self.build(axioms, &outside, threads, held_bytes + old_bytes, limits)?;
        drop(outside);
        stats.outside_el = outside_el;
        stats.tier = stats.outside_el.tier();
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
            stats.cleared = self.invalidate(&scratch, &ix);
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
            peak_memory: normalized_peak,
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
                let b = Bounds {
                    base_memory: totals.memory,
                    peak_memory: totals.peak,
                    ..bounds
                };
                let (more, stop) =
                    saturate::saturate_counted(&ix, &self.ctx, tests, seeds, &[], &b);
                *totals = saturate::Totals {
                    conclusions: totals.conclusions + more.conclusions,
                    memory: more.memory,
                    peak: totals.peak.max(more.peak),
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
        // Which contexts are real matters only when a context derived an individual, for
        // keys and for universal roles. Then owl:Thing and the individuals become real,
        // and so does what they reach, between rounds of the saturation, with the facts
        // of universal roles and the merges of keys, until none is new.
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
                let mut woken: Vec<Ex> = Vec::with_capacity(seeds.len());
                for (e, c) in seeds.drain(..) {
                    if let Some(x) = self.ctx.get(e as usize).and_then(|x| x.get()) {
                        x.todo.lock().push(c);
                        woken.push(e);
                    }
                }
                woken.sort_unstable();
                woken.dedup();
                let round = Bounds {
                    base_memory: totals.memory,
                    // the pool has been charged for the run's peak so far
                    peak_memory: totals.peak,
                    ..bounds
                };
                let more = saturate::saturate(&ix, &self.ctx, &[], &woken, &universal, &round)
                    .map_err(stopped)?;
                totals = saturate::Totals {
                    conclusions: totals.conclusions + more.conclusions,
                    memory: more.memory,
                    peak: totals.peak.max(more.peak),
                    merged: totals.merged || more.merged,
                };
                let view = world::GlobalAll {
                    ctx: &self.ctx,
                    dynamic: dynamic.as_ref(),
                };
                let facts = world::universal_facts(&ix, &view, &universal);
                let key_budget = WorldBudget {
                    limits,
                    charge: charge_fn,
                    base: totals.memory,
                    held: AtomicIsize::new(0),
                    peak: AtomicUsize::new(totals.peak),
                    halt: AtomicU8::new(0),
                    halted_at: AtomicUsize::new(0),
                };
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
                totals.peak = totals.peak.max(key_budget.peak.load(Ordering::Relaxed));
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
                let budget = WorldBudget {
                    limits,
                    charge: charge_fn,
                    base: totals.memory,
                    held: AtomicIsize::new(0),
                    peak: AtomicUsize::new(totals.peak),
                    halt: AtomicU8::new(0),
                    halted_at: AtomicUsize::new(0),
                };
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
                totals.peak = budget.peak.load(Ordering::Relaxed);
                for (d, f, kept) in found.into_iter().flatten() {
                    world_bytes += kept;
                    self.worlds.insert(d, (f.bottom, f.subs));
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
                let held = totals.memory + d.bytes() + dynamic.as_ref().map_or(0, |dy| dy.bytes());
                #[cfg(test)]
                let mut scan = SCAN_EXTRA.with(|x| x.get()) as isize;
                #[cfg(not(test))]
                let mut scan = 0isize;
                let mut refused = None;
                let peak = &mut totals.peak;
                let mut admit = |delta: isize| {
                    scan += delta;
                    if delta <= 0 {
                        return true;
                    }
                    let used = held + scan.max(0) as usize;
                    let grown = used.saturating_sub(*peak);
                    *peak = (*peak).max(used);
                    if max_memory.is_some_and(|m| used > m)
                        || (grown > 0 && charge_fn.is_some_and(|f| !f(grown)))
                    {
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
        self.dynamic = dynamic;
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
        let held = self.retained_bytes(held_bytes, roots.capacity() * 4, world_bytes);
        drop(ax);
        totals.peak = Self::admit_retained(held, totals.peak, limits).inspect_err(|_| {
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
        // the taxonomy's bytes join those held, and the budget and the pool's grant cover
        // what passes the saturation's peak. The build asks again as its estimate grows,
        // and the pool is charged only for the growth.
        let charged = std::cell::Cell::new(0usize);
        let budget = |bytes: usize| {
            let used = held + bytes;
            if limits.max_memory.is_some_and(|m| used > m) {
                return false;
            }
            let over = used.saturating_sub(totals.peak);
            let more = over.saturating_sub(charged.get());
            if more == 0 {
                return true;
            }
            let ok = charge_fn.is_none_or(|f| f(more));
            if ok {
                charged.set(over);
            }
            ok
        };
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
        stats.peak_memory = totals.peak.max(stats.memory);
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

    fn admit_retained(held: usize, peak: usize, limits: &Limits) -> Result<usize, Error> {
        let more = held.saturating_sub(peak);
        if limits.max_memory.is_some_and(|limit| held > limit)
            || limits
                .charge
                .as_ref()
                .is_some_and(|charge| more > 0 && !charge(more))
        {
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used: held,
            });
        }
        Ok(peak.max(held))
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
            .filter(|a| logical(a) && profiles::el_axiom(a).into_iter().next().is_some())
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
    /// left side of a changed subsumption or part of it, those with a link that a
    /// changed existential restriction can use, and every context with a link to one of
    /// them. The other contexts drop their links from cleared contexts and recompute
    /// what they take from the index. The answer is the number cleared.
    fn invalidate(&mut self, changed: &Axioms, ix: &Index) -> usize {
        let nodes = &self.interner.nodes;
        let mut subs: FxHashSet<Ex> = FxHashSet::default();
        let mut links: Vec<(index::Role, Ex)> = Vec::new();
        let mut stack: Vec<Ex> = Vec::new();
        stack.extend(changed.subs.iter().map(|&(a, _)| a));
        stack.extend(changed.disjoint.iter().flatten().copied());
        while let Some(e) = stack.pop() {
            if !subs.insert(e) {
                continue;
            }
            match nodes[e as usize] {
                Node::Conj(a, b) => {
                    stack.push(a);
                    stack.push(b);
                }
                Node::Exist(r, f) => {
                    links.push((r, f));
                    stack.push(f);
                }
                _ => {}
            }
        }
        let mut affected: FxHashSet<Ex> = FxHashSet::default();
        for (e, c) in self.ctx.iter().enumerate() {
            let Some(c) = c.get() else { continue };
            let st = c.st.lock();
            if !st.init {
                continue;
            }
            if st.subs.iter().any(|s| subs.contains(s)) {
                affected.insert(e as Ex);
            }
            for &(s, f) in &links {
                if st.subs.contains(&f) {
                    for (r, srcs) in &st.back {
                        if ix.roles.sub(*r, s) {
                            affected.extend(srcs.list.iter().copied());
                        }
                    }
                }
            }
        }
        // every context with a link into an affected context is affected
        let mut queue: Vec<Ex> = affected.iter().copied().collect();
        while let Some(e) = queue.pop() {
            let Some(c) = self.ctx.get(e as usize).and_then(|c| c.get()) else {
                continue;
            };
            let st = c.st.lock();
            for (_, srcs) in &st.back {
                for &s in &srcs.list {
                    if affected.insert(s) {
                        queue.push(s);
                    }
                }
            }
        }
        let mut cleared = 0;
        for (e, c) in self.ctx.iter_mut().enumerate() {
            let Some(c) = c.get_mut() else { continue };
            let st = c.st.get_mut();
            if affected.contains(&(e as Ex)) {
                if st.init {
                    cleared += 1;
                }
                *st = saturate::State::default();
                *c.started.get_mut() = false;
            } else if st.init {
                st.drop_links_from(&affected);
                st.reindex(ix);
            }
        }
        cleared
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
        if matches!(
            k,
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _)
        ) {
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
    }
    tell(it, out)
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

/// The bound the worlds of a run share: what they hold counts against the memory budget
/// beside the saturated state, the pool is charged for what passes the run's peak, and
/// every report polls the deadline and the cancellation.
struct WorldBudget<'a> {
    limits: &'a Limits,
    charge: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
    /// the bytes of the saturated state
    base: usize,
    /// the bytes the worlds hold now
    held: AtomicIsize,
    peak: AtomicUsize,
    halt: AtomicU8,
    halted_at: AtomicUsize,
}

impl WorldBudget<'_> {
    /// Takes a change of what a world holds, and answers whether the worlds go on.
    fn tell(&self, delta: isize) -> bool {
        let now = self.held.fetch_add(delta, Ordering::Relaxed) + delta;
        if self.halt.load(Ordering::Relaxed) != 0 {
            return false;
        }
        let limits = self.limits;
        let mut over = false;
        if delta > 0 {
            let used = self.base + now.max(0) as usize;
            let before = self.peak.fetch_max(used, Ordering::Relaxed);
            let grown = used.saturating_sub(before);
            over = limits.max_memory.is_some_and(|m| used > m)
                || self.charge.is_some_and(|f| grown > 0 && !f(grown));
            if over {
                self.halted_at.store(used, Ordering::Relaxed);
            }
        }
        let why = if over {
            WORLD_MEMORY
        } else if limits.deadline.is_some_and(|d| Instant::now() >= d) {
            WORLD_TIMEOUT
        } else if limits.cancelled.as_ref().is_some_and(|f| f()) {
            WORLD_CANCEL
        } else {
            return true;
        };
        // the first reason stays
        let _ = self
            .halt
            .compare_exchange(0, why, Ordering::Relaxed, Ordering::Relaxed);
        false
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
                            !out.complete,
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
                    Ok(out) if out.complete => assert_eq!(outcome(&out), first),
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
            assert!(out.complete && !out.stats.dl_over_budget);
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
                let b = profiles::el_axiom(k).into_iter().next()?;
                Some((i, (b.rule.to_string(), b.message)))
            })
            .collect();
        let mut s = Saturation::default();
        let (_, ix, _, _) = s
            .build(&axioms, &outside, 1, 0, &Limits::default())
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
    fn the_horn_normalization_opens_no_scope_for_an_assertion_about_an_anonymous_individual() {
        // roadmap item 869: the normalization refuses the assertions without reading
        // them, so they take no scope and no copy, and the Horn axiom beside them is
        // taken as before
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
        let run = |axioms: &[&AxiomKind]| {
            let mut it = Interner::new();
            let mut ax = index::Axioms::default();
            let taken = horn::normalize(axioms, &mut it, &mut ax, None, &|| false).unwrap();
            (taken, it.scope_bytes())
        };
        let (alone, bytes) = run(&[&all]);
        assert_eq!(alone, vec![true]);
        let mut with: Vec<&AxiomKind> = assertions.iter().collect();
        with.push(&all);
        let (taken, with_bytes) = run(&with);
        assert_eq!(taken, vec![false, false, false, false, true]);
        assert_eq!(with_bytes, bytes, "the assertions hold no scope");
    }
}

#[cfg(test)]
#[path = "a1_probe_alloc.rs"]
mod a1_probe_alloc;
