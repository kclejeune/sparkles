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
//! index does not hold, and only when the EL axioms need no rules of individuals,
//! universal roles or functional data roles; otherwise the Horn axioms are left out too.
//! A run with the Horn rules saturates everything again on its next run. An axiom the
//! classifier does not take is left out and reported, with the rule of the OWL 2 Profiles
//! recommendation it breaks, and the run's [`Stats`] count the axioms outside OWL 2 EL by
//! whether they are Horn, give the [`Tier`] of rules the ontology needs and count the
//! axioms the Horn rules take. An axiom with a literal or a
//! datatype that the classifier cannot compare, or with a property chain through the top
//! property, is used as far as the calculus goes and reported as partial. Every
//! subsumption the classifier answers is entailed. It derives every entailed subsumption
//! between named classes when no axiom is reported as partial or left out.
//!
//! [`Saturation`] keeps the saturated contexts between runs over axioms that the caller
//! keeps, such as those of an open ontology, and reads them in place on each run.
//! [`Classifier`] holds its own copy of the axioms on top of one. A run after a change
//! saturates again only the contexts that the changed axioms can reach, and a change to
//! the role hierarchy, chains, reflexivity or ranges, to an axiom with an individual, a
//! data property, a datatype or a key, and any change after a run that merged a context
//! with an individual saturates everything.
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

/// Whether Tier D takes ontologies with individuals and nominals through the root of a
/// rooted run (F24, "Nominals in the disjunctive rules"). It stays off until the rooted
/// rules are complete, so that such an ontology takes the paths it took before.
const ROOTED_RUNS: bool = false;

/// The most conclusions a run of Tier D with counting derives, kept or redundant, before
/// it passes its bound and the run starts again without counting.
/// Number restrictions can make a context derive millions of conclusions that subsumption
/// discards, which the memory budget does not see. A run that counts over a real ontology
/// either finishes in a few thousand conclusions or runs on past millions, so the bound
/// is the time a run spends before it gives up counting, about 0.2 s on one thread for a
/// small ontology. Of the 150 counting fixtures all but three derive at most 190,000
/// conclusions on one thread, one derives 1.0 million, which [`Limits::dl_work`] lets a
/// caller allow, and two pass any bound.
const DL_WORK_BOUND: usize = 1 << 18;

/// The bound on the conclusions of the run that starts again without counting once the run
/// that counts has passed its bound. The ontology has shown that its case analysis is
/// heavy, and without a bound such a run can go on for minutes where the Horn rules answer
/// in seconds. Four million conclusions take a few seconds.
const DL_RETRY_WORK: usize = 1 << 22;

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
/// check, and the rules it may use.
#[derive(Clone, Default)]
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
    /// before the run starts again without counting. `None` takes a bound of
    /// [`DL_WORK_BOUND`] conclusions. It applies only with [`counting`](Limits::counting).
    /// A run of Tier D without counting has no such bound, except the run that starts again
    /// after this one passed it, whose bound is 4 million.
    pub dl_work: Option<usize>,
    /// whether Tier D counts, which is off by default. A run that counts takes number
    /// restrictions, functional and inverse functional properties, disjoint properties and
    /// asymmetric properties into the case analysis, and it is complete for them when it
    /// finishes within [`dl_work`](Limits::dl_work). On real ontologies it rarely does, so
    /// a run that does not count leaves those axioms out and reports them, as the
    /// classifier did before it counted, and costs what that run costs.
    pub counting: bool,
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
    /// the candidate subsumers that Tier D tested in contexts of their own
    pub dl_tests: usize,
    /// the tested candidates that hold
    pub dl_tests_held: usize,
    /// the part of [`saturation`](Stats::saturation) that the tests of Tier D took
    pub dl_testing: Duration,
    /// Tier D passed its memory budget, so the run classified with the Horn rules and
    /// left the disjunctive axioms out, or Tier D with counting passed its bound on
    /// derived conclusions and the run classified without counting
    /// ([`dl_counting_over`](Stats::dl_counting_over))
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
            consistent: true,
            dynamic: None,
            horn_tried: false,
            dl_tried: false,
            skip_dl: false,
            no_counting: false,
            dl_worked: Default::default(),
            partial_over: false,
            numeric_halt: false,
            partial: None,
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
        let mut r = self.run(axioms, changes, held_bytes, first, with_taxonomy);
        if let Err(Error::Memory { .. }) = r
            && self.dl_tried
            && limits.threads != 1
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
            && self.dl_worked.load(Ordering::Relaxed)
        {
            // Tier D passed its bound on derived conclusions, and how many it derives
            // depends on the order in which the threads delivered the contexts'
            // conclusions: the run starts again on one thread, whose order is fixed, before
            // it falls back to the Horn rules. A schedule that stays within the bound where
            // one thread does not still answers in full, which the roadmap's item on a
            // deterministic bound covers. Its charges draw first on what the pool admitted
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
        }
        let mut counting_over = None;
        if let Err(Error::Memory { used: counted, .. }) = r
            && self.dl_tried
            && !limits.el_only
            && !self.partial_over
            && !self.numeric_halt
            && self.dl_worked.load(Ordering::Relaxed)
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
            self.no_counting = false;
            counting_over = Some(counted);
        }
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
        r
    }

    /// Drops the state of a run that stopped part way, since the next run starts again.
    fn reset(&mut self) {
        self.index = None;
        self.interner = Interner::new();
        self.ctx = Vec::new();
        self.dynamic = None;
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
        // The Horn rules take what they can of these (item 603) when the EL part needs
        // no rules of individuals, universal roles or functional data roles.
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
        if !candidates.is_empty()
            && !limits.el_only
            && !self.skip_dl
            && (self.horn_allowed(&ax) || ROOTED_RUNS && self.rooted_allowed(&ax))
            && candidates.iter().any(|k| !dl::shape(k).horn)
            && dl_norm::eligible(&self.interner, &ax, ROOTED_RUNS)
        {
            self.dl_tried = true;
            if let Some((flags, d)) = dl_norm::normalize(
                &candidates,
                &mut self.interner,
                &mut ax,
                limits.max_memory,
                &stop,
                limits.counting && !self.no_counting,
                ROOTED_RUNS,
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
        if !dl_taken && !candidates.is_empty() && !limits.el_only && self.horn_allowed(&ax) {
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
        ax.classes.extend(self.interner.met_classes());
        if let Some(b) = &self.interner.ranges.budget {
            b.baseline(self.interner.bytes() + ax.bytes())?;
        }
        let ix = Index::build(&ax, &mut self.interner, limits.max_memory, &stop).map_err(halt)?;
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
    /// when the EL part has no keys, top properties or functional data roles. Keys wait on
    /// their reduction to class axioms and the universal property on its replacement, as
    /// F24 describes.
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

    /// Whether the Horn rules may take axioms outside OWL 2 EL: when the EL part has no
    /// individuals, top properties or functional data roles, whose rules the Horn
    /// instantiation leaves out.
    fn horn_allowed(&self, ax: &Axioms) -> bool {
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

    fn run(
        &mut self,
        axioms: &[&AxiomKind],
        changes: Option<Changes<'_>>,
        held_bytes: usize,
        limits: &Limits,
        with_taxonomy: bool,
    ) -> Result<(bool, Option<Taxonomy>, Stats), Error> {
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
        let changes = changes.filter(|_| !again);
        let full = self.entangled
            || changes.is_none_or(|c| c.added.iter().chain(c.removed).any(|k| needs_full_run(k)));
        self.worlds.clear();
        if full {
            self.interner = Interner::new();
            self.ctx = Vec::new();
        }
        // the premises of the changed axioms, read from the interner before the index of
        // the new axioms is built
        let premises = match changes {
            Some(c) if !full => {
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
        let (ax, mut ix, normalized_peak, outside_el) =
            self.build(axioms, &outside, threads, held_bytes, limits)?;
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
        self.dynamic = None;
        let premises = if horn || disj {
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
        let dynamic = match &ix.horn {
            Some(h) => {
                debug_assert_eq!(h.base as usize, n);
                Some(saturate::Dynamic::new(n as Ex))
            }
            // the test contexts of Tier D get ids above the index's
            None if disj => Some(saturate::Dynamic::new(n as Ex)),
            None => None,
        };
        let dl_run = ix.dl.as_ref().map(|_| {
            dl_rules::DlRun::new(limits.expansion == Expansion::Eager, self.dl_worked.clone())
        });
        if let Some(scratch) = premises {
            stats.incremental = true;
            stats.cleared = self.invalidate(&scratch, &ix);
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
            + self.context_bytes();
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
        if let Some(d) = &dl_run {
            // a run that counts has the bound of the limits, and the run that starts again
            // without counting after it passed that bound has the larger one of a run that
            // cannot expect to finish soon, before the Horn rules take over
            if self.no_counting {
                d.set_work_limit(DL_RETRY_WORK);
            } else if ax.dl.as_ref().is_some_and(|d| d.counting) {
                d.set_work_limit(limits.dl_work.unwrap_or(DL_WORK_BOUND).max(1));
            }
        }
        let max_memory = if horn || disj {
            let own = HORN_MEMORY_FLOOR.max(base.saturating_mul(HORN_MEMORY_FACTOR));
            Some(limits.max_memory.map_or(own, |m| m.min(own)))
        } else {
            limits.max_memory
        };
        let bounds = Bounds {
            threads,
            deadline: limits.deadline,
            max_memory,
            cancelled: cancel_fn,
            charge: charge_fn,
            base_memory: base,
            peak_memory: normalized_peak,
            dynamic: dynamic.as_ref(),
            dl: dl_run.as_ref(),
            partial: limits.partial && disj && with_taxonomy,
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
            Stop::Memory { used } => Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            },
        };
        // a run that answers in part keeps the reason it stopped and goes on to the answer
        let mut halted: Option<Error> = None;
        let partial = bounds.partial;
        let mut totals = match saturate::saturate_counted(&ix, &self.ctx, &roots, &[], &[], &bounds)
        {
            (t, None) => t,
            (t, Some(s)) if partial => {
                halted = Some(stopped(s));
                t
            }
            (_, Some(s)) => return Err(stopped(s)),
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
                stop.map_or(Ok(()), |s| Err(stopped(s)))
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
                tests.extend(singles.drain(..).map(|(x, b)| (x, vec![b])));
                if tests.is_empty() {
                    break;
                }
                let ids: Vec<Ex> = tests
                    .iter()
                    .map(|(x, bs)| dl_rules::test(d, *x, bs))
                    .collect();
                if let Err(e) = round(&[], &ids, &mut totals) {
                    halted = Some(e);
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
                if let Err(e) = round(&seeds, &[], &mut totals) {
                    halted = Some(e);
                    break;
                }
            }
            stats.dl_testing = tests_start.elapsed();
        }
        if !partial && let Some(h) = halted.take() {
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
        let entangled = ix.has_individuals()
            && (totals.merged || !ix.universal.is_empty() || !ix.keys.is_empty());
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
                let view = world::Global(&self.ctx);
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
            let witnesses = world::witnesses(&ix, &self.ctx, &universal);
            if !witnesses.is_empty() {
                let classes = world::reaching(&ix, &self.ctx, &witnesses);
                let real: Vec<Ex> = (0..self.ctx.len() as Ex)
                    .filter(|&e| self.ctx[e as usize].get().is_some_and(|c| c.st.lock().real))
                    .collect();
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
                        let w = world::World::new(&ix, &self.ctx, &universal, &real, &bound);
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
        // again
        self.entangled |= horn || disj;
        if let Some(d) = &dl_run {
            for c in self.ctx.iter().filter_map(|c| c.get()) {
                if let Some(n) = c.st.lock().extra.as_ref().and_then(|x| x.dl.as_ref()) {
                    stats.dl_contexts += 1;
                    stats.dl_stored += n.store.clauses.len();
                }
            }
            stats.dl_work = d.work();
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
        drop(dl_run);
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
            return Ok((consistent, None, stats));
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
                .collect();
            classes.sort();
            self.partial = Some((classes, consistency));
            return Ok((consistent, Some(taxonomy), stats));
        }
        poll(limits)?;
        Ok((consistent, Some(taxonomy), stats))
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
    }

    fn retained_bytes(&self, held_bytes: usize, root_bytes: usize, world_bytes: usize) -> usize {
        self.index_bytes(self.index.as_ref().expect("completed index"))
            + held_bytes
            + root_bytes
            + self.context_bytes()
            + self.dynamic.as_ref().map_or(0, |d| d.retained_bytes())
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
    /// the record while they run. The steps outside the saturation run on the calling
    /// thread for so small an ontology.
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
        let hook = {
            let seen = seen.clone();
            move || {
                if std::thread::current().id() != caller
                    && started.load(std::sync::atomic::Ordering::Relaxed) > 0
                {
                    seen.store(true, std::sync::atomic::Ordering::Relaxed);
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
}

#[cfg(test)]
#[path = "a1_probe_alloc.rs"]
mod a1_probe_alloc;
