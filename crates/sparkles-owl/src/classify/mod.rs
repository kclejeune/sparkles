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
//! [`Classifier`] holds the axioms and the saturated contexts between runs. A run after
//! a change saturates again only the contexts that the changed axioms can reach, and a
//! change to the role hierarchy, chains, reflexivity or ranges saturates everything.
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
mod saturate;
mod taxonomy;

pub use index::Gap;
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
    /// the estimated bytes of the index and the contexts
    pub memory: usize,
    pub threads: usize,
    pub index: Duration,
    pub saturation: Duration,
    pub taxonomy: Duration,
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

/// One axiom the classifier holds: how many times it was added and what of it the
/// calculus uses.
struct Held {
    count: u32,
    /// the profile violation that leaves it out, if any
    outside: Option<(String, String)>,
}

/// An incremental OWL 2 EL classifier. See the [module documentation](self).
pub struct Classifier {
    axioms: FxHashMap<AxiomKind, Held>,
    /// the classes of the axioms' signature, with the number of axioms naming each
    classes: FxHashMap<Class, u32>,
    /// changes since the last run, as signed counts
    pending: FxHashMap<AxiomKind, i64>,
    interner: Interner,
    index: Option<Index>,
    ctx: Contexts,
    /// the partial axioms of the last normalization
    partial: BTreeMap<AxiomKind, &'static str>,
    /// chains left out by the range restriction of OWL 2 EL
    chain_ranges: BTreeMap<AxiomKind, String>,
    /// set when a run stopped part way, so that the next run starts again
    dirty: bool,
}

impl Default for Classifier {
    fn default() -> Classifier {
        Classifier::new()
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
    )
}

/// Whether the axiom has logical content for classification.
fn logical(ax: &AxiomKind) -> bool {
    !matches!(
        ax,
        AxiomKind::AnnotationAssertion(..)
            | AxiomKind::SubAnnotationPropertyOf(..)
            | AxiomKind::AnnotationPropertyDomain(..)
            | AxiomKind::AnnotationPropertyRange(..)
    )
}

fn classes_of(ax: &AxiomKind) -> impl Iterator<Item = Class> {
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    sig.0.into_iter().filter_map(|e| match e {
        Entity::Class(c)
            if !matches!(
                c.as_str(),
                crate::vocab::OWL_THING | crate::vocab::OWL_NOTHING
            ) =>
        {
            Some(c)
        }
        _ => None,
    })
}

impl Classifier {
    pub fn new() -> Classifier {
        Classifier {
            axioms: FxHashMap::default(),
            classes: FxHashMap::default(),
            pending: FxHashMap::default(),
            interner: Interner::new(),
            index: None,
            ctx: Vec::new(),
            partial: BTreeMap::new(),
            chain_ranges: BTreeMap::new(),
            dirty: true,
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
        for ax in axioms {
            if logical(&ax.kind) {
                *self.pending.entry(ax.kind.clone()).or_default() += 1;
            }
        }
    }

    /// Removes axioms. Removing an axiom the classifier does not hold does nothing.
    pub fn remove<'a>(&mut self, axioms: impl IntoIterator<Item = &'a Axiom>) {
        for ax in axioms {
            if logical(&ax.kind) {
                *self.pending.entry(ax.kind.clone()).or_default() -= 1;
            }
        }
    }

    /// The number of distinct axioms held, changes not yet classified included.
    pub fn len(&self) -> usize {
        let mut n = self.axioms.len() as i64;
        for (ax, d) in &self.pending {
            let have = self.axioms.get(ax).map_or(0, |h| h.count as i64);
            match (have > 0, have + d > 0) {
                (false, true) => n += 1,
                (true, false) => n -= 1,
                _ => {}
            }
        }
        n as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The axioms of the last run that it left out or used only in part, in order.
    pub fn excluded(&self) -> Vec<Excluded> {
        let mut out: Vec<Excluded> = Vec::new();
        for (ax, h) in &self.axioms {
            if let Some((rule, message)) = &h.outside {
                out.push(Excluded {
                    axiom: ax.clone(),
                    gap: Gap::Outside {
                        rule: rule.clone(),
                        message: message.clone(),
                    },
                });
            }
        }
        for (ax, message) in &self.chain_ranges {
            out.push(Excluded {
                axiom: ax.clone(),
                gap: Gap::Outside {
                    rule: "OWL 2 Profiles §2.2.6".to_string(),
                    message: message.clone(),
                },
            });
        }
        for (ax, why) in &self.partial {
            out.push(Excluded {
                axiom: ax.clone(),
                gap: Gap::Partial(why),
            });
        }
        out.sort();
        out
    }

    /// Applies the pending changes to the held axioms. The answer is the axioms that
    /// came to be held and those that stopped being held.
    fn apply_pending(&mut self) -> (Vec<AxiomKind>, Vec<AxiomKind>) {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        for (ax, d) in std::mem::take(&mut self.pending) {
            if d == 0 {
                continue;
            }
            let have = self.axioms.get(&ax).map_or(0, |h| h.count as i64);
            let now = (have + d).max(0);
            if have == 0 && now > 0 {
                let broken = profiles::el_axiom(&ax);
                let outside = broken
                    .into_iter()
                    .next()
                    .map(|b| (b.rule.to_string(), b.message));
                for c in classes_of(&ax) {
                    *self.classes.entry(c).or_default() += 1;
                }
                self.axioms.insert(
                    ax.clone(),
                    Held {
                        count: now as u32,
                        outside,
                    },
                );
                added.push(ax);
            } else if have > 0 && now == 0 {
                self.axioms.remove(&ax);
                for c in classes_of(&ax) {
                    if let Some(n) = self.classes.get_mut(&c) {
                        *n -= 1;
                        if *n == 0 {
                            self.classes.remove(&c);
                        }
                    }
                }
                removed.push(ax);
            } else if let Some(h) = self.axioms.get_mut(&ax) {
                h.count = now as u32;
            }
        }
        (added, removed)
    }

    /// Normalizes the held axioms inside OWL 2 EL and builds the index.
    fn build(&mut self) -> (Axioms, Index) {
        let mut ax = Axioms::default();
        let mut partial = BTreeMap::new();
        let mut chains: Vec<&AxiomKind> = Vec::new();
        let kinds = self
            .axioms
            .iter()
            .filter(|(_, h)| h.outside.is_none())
            .map(|(k, _)| k);
        for k in kinds {
            if matches!(
                k,
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _)
            ) {
                chains.push(k);
                continue;
            }
            if let Some(why) = index::normalize(k, &mut self.interner, &mut ax) {
                partial.insert(k.clone(), why);
            }
        }
        // the range restriction of OWL 2 EL (Profiles §2.2.6): a chain's superproperty
        // has no range that its last property lacks
        self.chain_ranges.clear();
        if !chains.is_empty() {
            let ranges = range_table(&self.axioms);
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
                        self.chain_ranges.insert(
                            k.clone(),
                            format!(
                                "the range {missing} of <{}> is not a range of the chain's last property <{}> in OWL 2 EL",
                                sup.as_str(),
                                last.named().as_str()
                            ),
                        );
                        continue;
                    }
                }
                if let Some(why) = index::normalize(k, &mut self.interner, &mut ax) {
                    partial.insert(k.clone(), why);
                }
            }
        }
        self.partial = partial;
        for c in self.classes.keys() {
            let e = self.interner.class(c);
            ax.classes.push(e);
        }
        let ix = Index::build(&ax, &mut self.interner);
        (ax, ix)
    }

    /// Classifies the axioms held, saturating again only what changed since the last
    /// run.
    pub fn classify(&mut self, limits: &Limits) -> Result<Classification, Error> {
        let mut stats = Stats::default();
        let start = Instant::now();
        let (added, removed) = self.apply_pending();
        stats.added = added.len();
        stats.removed = removed.len();
        let full =
            self.dirty || self.index.is_none() || added.iter().chain(&removed).any(role_axiom);
        if full {
            self.interner = Interner::new();
            self.ctx = Vec::new();
        }
        // the premises of the changed axioms, read from the interner before the index of
        // the new axioms is built
        let premises = if full {
            None
        } else {
            let mut scratch = Axioms::default();
            for k in added.iter().chain(&removed) {
                index::normalize(k, &mut self.interner, &mut scratch);
            }
            Some(scratch)
        };
        let (ax, ix) = self.build();
        let n = self.interner.len();
        self.ctx.resize_with(n, OnceLock::new);
        if let Some(scratch) = premises {
            stats.incremental = true;
            stats.cleared = self.invalidate(&scratch, &ix);
        }
        let mut roots: Vec<Ex> = ax.classes.clone();
        roots.push(TOP);
        roots.extend(ax.individuals.iter().copied());
        roots.sort_unstable();
        roots.dedup();
        stats.index = start.elapsed();
        let threads = if limits.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            limits.threads
        };
        stats.threads = threads;
        let base = self.index_bytes(&ix) + self.context_bytes();
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
            self.dirty = true;
            self.index = None;
            return Err(Error::Memory {
                limit: limits.max_memory.unwrap_or(base),
                used: base,
            });
        }
        let t = Instant::now();
        let totals = saturate::saturate(&ix, &self.interner.nodes, &self.ctx, &roots, &bounds);
        stats.saturation = t.elapsed();
        let totals = match totals {
            Ok(t) => t,
            Err(stop) => {
                self.dirty = true;
                self.index = None;
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
        stats.memory = totals.memory;
        self.dirty = false;
        let t = Instant::now();
        let consistent = self.consistent(&ax);
        let taxonomy = taxonomy::build(self, consistent);
        stats.taxonomy = t.elapsed();
        stats.contexts = self.ctx.iter().filter(|c| c.get().is_some()).count();
        self.index = Some(ix);
        Ok(Classification {
            consistent,
            taxonomy,
            stats,
        })
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
        let vecs = |v: &Vec<Vec<Ex>>| v.iter().map(|x| 24 + x.len() * 4).sum::<usize>();
        let pairs = |v: &Vec<Vec<(u32, u32)>>| v.iter().map(|x| 24 + x.len() * 8).sum::<usize>();
        self.interner.len() * 40
            + vecs(&ix.told)
            + pairs(&ix.conj)
            + pairs(&ix.exist)
            + ix.disjoint.iter().map(|x| 24 + x.len() * 4).sum::<usize>()
            + ix.succ.len() * 12
            + self.axioms.len() * 64
    }

    fn context_bytes(&self) -> usize {
        self.ctx
            .iter()
            .filter_map(|c| c.get())
            .map(|c| 64 + c.st.lock().bytes())
            .sum()
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
                    for (&r, srcs) in &st.back {
                        if ix.roles.sub(r, s) {
                            affected.extend(srcs.iter().copied());
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
            for srcs in st.back.values() {
                for &s in srcs {
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
    axioms: &FxHashMap<AxiomKind, Held>,
) -> FxHashMap<String, std::collections::BTreeSet<ClassExpression>> {
    let mut sup: FxHashMap<&str, Vec<&str>> = FxHashMap::default();
    let mut direct: FxHashMap<&str, Vec<&ClassExpression>> = FxHashMap::default();
    for (k, h) in axioms {
        if h.outside.is_some() {
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
