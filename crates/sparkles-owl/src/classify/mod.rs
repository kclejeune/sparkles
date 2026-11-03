//! OWL 2 EL classification (spec F24): the inferred class hierarchy and the
//! unsatisfiable classes of an ontology, computed by a consequence-based, concurrent
//! calculus in the style of ELK (Kazakov, Krötzsch and Simančík, *The Incredible ELK*,
//! JAR 2014), and kept up to date incrementally as axioms are added and removed.
//!
//! The classifier reads the F21 axiom model only. An axiom outside OWL 2 EL is left out
//! and reported, with the rule of the OWL 2 Profiles recommendation it breaks. An axiom
//! inside the profile whose consequences the calculus may not all derive, such as one with
//! an individual in a class expression, a data range or a negative property assertion,
//! is used as far as the calculus goes and reported as partial. Every subsumption the
//! classifier answers is entailed. It derives every entailed subsumption between named
//! classes when no axiom is reported as partial or left out.
//!
//! [`Saturation`] keeps the saturated contexts between runs over axioms that the caller
//! keeps, such as those of an open ontology, and reads them in place on each run.
//! [`Classifier`] holds its own copy of the axioms on top of one. A run after a change
//! saturates again only the contexts that the changed axioms can reach, and a change to
//! the role hierarchy, chains, reflexivity or ranges saturates everything.
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

mod index;
mod query;
mod saturate;
mod taxonomy;

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
use std::time::{Duration, Instant};

/// What bounds a run: the worker threads, a deadline, a memory budget and a cancellation
/// check.
#[derive(Clone, Default)]
pub struct Limits {
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
    /// whether the last run whose saturation ended found the ontology consistent
    consistent: bool,
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
/// role table, which a run builds once, or one with data, whose generated rules for
/// literals and data ranges the invalidation does not follow.
fn needs_full_run(ax: &AxiomKind) -> bool {
    if role_axiom(ax) {
        return true;
    }
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    sig.0
        .iter()
        .any(|e| matches!(e, Entity::DataProperty(_) | Entity::Datatype(_)))
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

/// The threads a run takes.
fn threads_of(limits: &Limits) -> usize {
    if limits.threads == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        limits.threads
    }
}

/// The items below which a parallel step runs on one thread.
const MIN_CHUNK: usize = 1024;

/// Runs `f` on chunks of `items`, on up to `threads` scoped threads.
pub(crate) fn par_chunks<T: Send>(items: &mut [T], threads: usize, f: impl Fn(&mut [T]) + Sync) {
    let threads = threads.clamp(1, items.len().div_ceil(MIN_CHUNK).max(1));
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

/// The answers of `f` on chunks of `items`, in order, computed on up to `threads` scoped
/// threads. `f` takes the offset of its chunk.
pub(crate) fn par_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    f: impl Fn(usize, &[T]) -> R + Sync,
) -> Vec<R> {
    let threads = threads.clamp(1, items.len().div_ceil(MIN_CHUNK).max(1));
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
        self.run_held(limits, true)
            .map(|(consistent, taxonomy, stats)| Classification {
                consistent,
                taxonomy: taxonomy.expect("a taxonomy was asked for"),
                stats,
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
            par_map(xs, threads, |_, c| {
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
            consistent: true,
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
        self.run_with(axioms, changes, 0, limits, true)
            .map(|(consistent, taxonomy, stats)| Classification {
                consistent,
                taxonomy: taxonomy.expect("a taxonomy was asked for"),
                stats,
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
        let r = self.run(axioms, changes, held_bytes, limits, with_taxonomy);
        if r.is_err() && self.dirty {
            // the next run starts again, so the state of this one is of no use
            self.index = None;
            self.interner = Interner::new();
            self.ctx = Vec::new();
        }
        r
    }

    /// Normalizes the axioms inside OWL 2 EL and builds the index. `outside` names the
    /// axioms the profile check leaves out.
    fn build(
        &mut self,
        axioms: &[&AxiomKind],
        outside: &[(usize, (String, String))],
        limits: &Limits,
    ) -> Result<(Axioms, Index), Error> {
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
        for (i, &k) in axioms.iter().enumerate() {
            if i % POLL_AXIOMS == 0 {
                poll(limits)?;
            }
            if !logical(k) {
                continue;
            }
            if is_outside[i] {
                index::intern_signature(k, &mut self.interner);
                continue;
            }
            if matches!(
                k,
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _)
            ) {
                chains.push(k);
                continue;
            }
            if let Some(why) = index::normalize(k, &mut self.interner, &mut ax) {
                partial.insert(k, why);
            }
        }
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
            }
        }
        ax.classes.extend(self.interner.met_classes());
        let stop = || poll(limits).is_err();
        let ix =
            Index::build(&ax, &mut self.interner, limits.max_memory, &stop).map_err(
                |h| match h {
                    index::Halt::Memory(used) => Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used,
                    },
                    index::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                },
            )?;
        // an individual that fills a property with a range has a context for each range,
        // which nominal reasoning would merge
        for (i, &k) in axioms.iter().enumerate() {
            if let AxiomKind::ObjectPropertyAssertion(ObjectPropertyExpression::Property(p), ..) = k
                && !is_outside[i]
                && let Some(r) = self.interner.role_id(&index::RoleName::Object(p.clone()))
                && ix.roles.range.get(r as usize).is_some_and(|c| c.is_some())
            {
                partial
                    .entry(k)
                    .or_insert("individuals that fill a property with a range");
            }
        }
        let mut excluded: Vec<Excluded> = Vec::new();
        for (i, (rule, message)) in outside {
            excluded.push(Excluded {
                axiom: axioms[*i].clone(),
                gap: Gap::Outside {
                    rule: rule.clone(),
                    message: message.clone(),
                },
            });
        }
        for (k, message) in chain_ranges {
            excluded.push(Excluded {
                axiom: k.clone(),
                gap: Gap::Outside {
                    rule: "OWL 2 Profiles §2.2.6".to_string(),
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
        Ok((ax, ix))
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
        let changes = changes.filter(|_| !again);
        let full =
            changes.is_none_or(|c| c.added.iter().chain(c.removed).any(|k| needs_full_run(k)));
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
        // the profile check of each axiom, which depends on it alone
        let outside: Vec<(usize, (String, String))> = par_map(axioms, threads, |at, chunk| {
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
        let (ax, ix) = self.build(axioms, &outside, limits)?;
        drop(outside);
        poll(limits)?;
        let n = self.interner.len();
        self.ctx.resize_with(n, OnceLock::new);
        if let Some(scratch) = premises {
            stats.incremental = true;
            stats.cleared = self.invalidate(&scratch, &ix);
            poll(limits)?;
        }
        let mut roots: Vec<Ex> = ax.classes.clone();
        roots.push(TOP);
        roots.extend(ax.individuals.iter().copied());
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
        let bounds = Bounds {
            threads,
            deadline: limits.deadline,
            max_memory: limits.max_memory,
            cancelled: cancel_fn,
            charge: charge_fn,
            base_memory: base,
        };
        if limits.max_memory.is_some_and(|m| base > m) || charge_fn.is_some_and(|f| !f(base)) {
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(base),
                used: base,
            });
        }
        let t = Instant::now();
        let totals = saturate::saturate(&ix, &self.ctx, &roots, &bounds);
        stats.saturation = t.elapsed();
        let totals = match totals {
            Ok(t) => t,
            Err(stop) => {
                return Err(match stop {
                    Stop::Cancelled => Error::Cancelled,
                    Stop::Timeout => Error::Timeout,
                    Stop::Memory { used } => Error::Memory {
                        limit: limits.max_memory.unwrap_or(usize::MAX),
                        used,
                    },
                });
            }
        };
        stats.conclusions = totals.conclusions;
        // the saturated state is complete: a stop from here on keeps it for the next run
        self.dirty = false;
        self.index = Some(ix);
        let t = Instant::now();
        let consistent = self.consistent(&ax);
        self.consistent = consistent;
        let held = totals.memory - ax.bytes();
        drop(ax);
        if !with_taxonomy {
            stats.memory = held;
            stats.peak_memory = totals.peak.max(held);
            stats.contexts = self.ctx.iter().filter(|c| c.get().is_some()).count();
            poll(limits)?;
            return Ok((consistent, None, stats));
        }
        let stop = || poll(limits).is_err();
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
        let (taxonomy, tax_bytes) = taxonomy::build(self, consistent, threads, &stop, &budget)
            .map_err(|h| match h {
                taxonomy::Halt::Stopped => poll(limits).err().unwrap_or(Error::Cancelled),
                taxonomy::Halt::Memory(bytes) => Error::Memory {
                    limit: limits.max_memory.unwrap_or(usize::MAX),
                    used: held + bytes,
                },
            })?;
        stats.memory = held + tax_bytes;
        stats.taxonomy_memory = tax_bytes;
        stats.peak_memory = totals.peak.max(stats.memory);
        stats.taxonomy = t.elapsed();
        stats.contexts = self.ctx.iter().filter(|c| c.get().is_some()).count();
        poll(limits)?;
        Ok((consistent, Some(taxonomy), stats))
    }

    fn consistent(&self, ax: &Axioms) -> bool {
        let bottom = |e: Ex| {
            self.ctx
                .get(e as usize)
                .and_then(|c| c.get())
                .is_some_and(|c| c.st.lock().bottom)
        };
        !bottom(TOP) && !ax.individuals.iter().any(|&i| bottom(i))
    }

    fn index_bytes(&self, ix: &Index) -> usize {
        let iris = |len: usize| len + 56;
        self.interner.len() * 40
            + self
                .interner
                .classes
                .iter()
                .map(|c| 2 * iris(c.as_str().len()))
                .sum::<usize>()
            + ix.bytes()
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
fn poll(limits: &Limits) -> Result<(), Error> {
    if limits.deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(Error::Timeout);
    }
    if limits.cancelled.as_ref().is_some_and(|f| f()) {
        return Err(Error::Cancelled);
    }
    Ok(())
}
