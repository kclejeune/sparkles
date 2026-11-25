//! Syntactic locality modules (Cuenca Grau, Horrocks, Kazakov and Sattler, JAIR 2008).
//!
//! `reason --dump-unsatisfiable` writes the bottom-locality module of the unsatisfiable
//! classes, as ROBOT writes the `BOT` module that its `extract` computes. The module
//! holds every justification of each entailment over the signature.
//!
//! [`coverage_refs`] decides, for a classification that left axioms out, which classes still
//! have their complete answer and whether the run's consistency holds for the whole
//! ontology. Its soundness rests on the locality rules below, so a rule that is unsure
//! calls an axiom non-local, which only makes a module larger.
//!
//! # Why the coverage is sound
//!
//! Let `O` be the ontology, `L` the axioms the run left out or used only in part, and
//! `T = O \ L`. The run is sound for `O` and complete for `T`, with the caveats of the
//! rules it ran. For a set of axioms `S`, let `S(C)` be the named superclasses of `C` that
//! `S` entails, `owl:Nothing` included.
//!
//! An axiom is bottom-local for a signature `Σ` when every interpretation, with every
//! class and property outside `Σ` made empty, satisfies it. The bottom module `M` of `Σ`
//! in a set `S` is the least subset of `S` whose complement is bottom-local for
//! `Σ ∪ sig(M)`. A model `I` of `M` then extends to a model of `S` that agrees with `I`
//! on `Σ ∪ sig(M)` and makes every other class empty.
//!
//! *Layer 1.* Let `M_C` be the bottom module of `{C}` in `O`. When `L` and `M_C` are
//! disjoint, `M_C ⊆ T`, and `M_C(C) ⊆ T(C) ⊆ answer(C) ⊆ O(C)`. For a class `D` with
//! `O ⊨ C ⊑ D`, take a model `I` of `M_C` and its extension `J` to `O`. If `D` is outside
//! `sig(M_C) ∪ {C}`, `D` is empty in `J`, so `C` is empty in `J` and in `I`, and
//! `M_C ⊨ C ⊑ ⊥`. Otherwise `D^I = D^J ⊇ C^J = C^I`. So `O(C) = M_C(C)`, and the answer
//! for `C` is exact.
//!
//! *Equivalents.* A satisfiable class `D` equivalent to such a `C` is in `sig(M_C)` by the
//! same argument. The module of `{D}` is then contained in `M_C`, so `D` is complete too
//! and the run finds `D ⊑ C`. Every superclass of a satisfiable complete class is complete
//! for the same reason, so its direct superclasses are exact. An unsatisfiable complete
//! class is exact, but the other unsatisfiable classes, which share its node, need not
//! be, so when any class is incomplete
//! [`Classification::cover`](crate::classify::Classification::cover) lists every
//! unsatisfiable class as incomplete too.
//!
//! *Layer 2, consistency.* The run's verdict of inconsistent is sound for `O`. When it
//! finds `T` consistent, `O` is consistent if `L` misses the bottom module of the empty
//! signature in `O`, whose models extend to models of `O`, or the top module of the empty
//! signature in `O`. An axiom is top-local for `Σ` when every interpretation, with every
//! class outside `Σ` made the whole domain and every property outside `Σ` every pair, satisfies
//! it, and a model of the top module extends to a model of `O` in the same way. When
//! neither holds, the consistency is unknown and every class is incomplete, since an
//! inconsistent ontology makes every class unsatisfiable.
//!
//! *Layer 3, the terminology alone.* Let `A` be the assertions and keys of `O`, which
//! constrain only named individuals, and `M'_C` the bottom module of `{C}` in `O \ A`.
//! Suppose `O` is known consistent, no axiom of `O \ A` holds `ObjectOneOf` or
//! `ObjectHasValue`, and no axiom of `O` holds `owl:topObjectProperty`. Then the axioms
//! of `O \ A` are preserved by disjoint unions of interpretations. Take a model `K` of
//! `O` and a model `I` of `M'_C` with an element of `C` outside `D`, extend `I` to a model
//! `J` of `O \ A`, and interpret the individuals as `K` does. The disjoint union of `K`
//! and `J` is a model of `O`, since `K` satisfies `A` and every axiom of `O \ A` holds in
//! each part. It keeps the element of `C` outside `D`. So `O(C) = M'_C(C)`, and when `L`
//! misses `M'_C` the answer for `C` is exact. `M'_C ⊆ M_C`, so this layer decides more
//! classes, such as those of an ontology whose assertions reach a left-out axiom.

use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};

/// The logical axioms of the bottom-locality module of `seed` among `axioms`, and the
/// declarations of the entities of its signature, in the axioms' order. An axiom of a
/// kind the rules below do not cover counts as non-local, which only makes the module
/// larger.
pub fn bottom_module<'a>(
    axioms: impl IntoIterator<Item = &'a Axiom>,
    seed: impl IntoIterator<Item = &'a Class>,
) -> Vec<Axiom> {
    let axioms: Vec<&Axiom> = axioms.into_iter().collect();
    let kinds: Vec<&AxiomKind> = axioms.iter().map(|a| &a.kind).collect();
    let ix = Index::new(&kinds);
    let mut x = Extraction::new(&ix, Mode::Bottom, None, None);
    let seed: Vec<&Class> = seed.into_iter().collect();
    let ids: Vec<u32> = seed.iter().filter_map(|c| ix.id(c.as_str())).collect();
    x.extend(&ids, &mut |_| Step::Go, &mut |_| Step::Go);
    // the seed's own declarations too, which no axiom may name
    let mut names: FxHashSet<&str> = x.signature().collect();
    names.extend(seed.iter().map(|c| c.as_str()));
    axioms
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            ix.axiom_of(*i).is_some_and(|j| x.in_module(j))
                || matches!(&a.kind, AxiomKind::Declaration(e) if names.contains(e.iri().as_str()))
        })
        .map(|(_, a)| (*a).clone())
        .collect()
}

/// What a classification that left axioms out still answers in full.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// whether the run's verdict on consistency holds for the whole ontology
    pub consistency_known: bool,
    /// the classes whose answer may miss entailments, in IRI order: every class when the
    /// consistency is not known
    pub incomplete: Vec<Class>,
    /// the extraction passed its work bound, and the classes it had not decided are
    /// incomplete
    pub over_budget: bool,
}

/// A stream of the axioms of an ontology that a run was not given at all, for
/// [`Classification::cover`](crate::classify::Classification::cover): it calls its
/// argument on each until that answers false, and answers false itself only when it
/// could not finish, such as at its caller's deadline. Each call starts again from the
/// first axiom, as a scan of a store's or a file's assertions does. The stream polls its
/// own deadline.
pub type Dropped<'d> = dyn FnMut(&mut dyn FnMut(&AxiomKind) -> bool) -> bool + 'd;

/// Why [`coverage_refs`] stopped before an answer: its caller's stop said so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stopped;

/// How often an extraction asks its caller whether to stop, in axioms it looks at.
const POLL_EVERY: usize = 4096;

/// Borrowed result of [`coverage_refs`], allowing callers to admit the retained
/// `Vec<Class>` bytes before cloning any class IRI strings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CoverageRefs<'c> {
    /// whether the run's verdict on consistency holds for the whole ontology
    pub consistency_known: bool,
    /// the classes whose answer may miss entailments, in IRI order: every class when the
    /// consistency is not known
    pub incomplete: Vec<&'c Class>,
    /// the extraction passed its work bound, and the classes it had not decided are
    /// incomplete
    pub over_budget: bool,
}

/// Which of `classes` keep their complete answer when a run over `axioms` left out or
/// used only in part the axioms `left_out`, and found the rest consistent or not. See the
/// [module documentation](self) for the argument. `owl:Thing` and `owl:Nothing` are never
/// listed. A left-out axiom that is not among `axioms` leaves the consistency unknown.
///
/// The extractions look at no more than `budget` axioms in all, after which the classes
/// not yet decided are incomplete, so that the answer depends on the input alone. `stop`
/// is asked every few thousand axioms, and true ends the pass with [`Stopped`].
///
/// Returns borrowed references into `classes` without cloning class IRIs so callers can
/// enforce memory admission before materializing an owned `Vec<Class>`.
pub(crate) fn coverage_refs<'c>(
    axioms: &[&AxiomKind],
    left_out: &[&AxiomKind],
    consistent: bool,
    classes: impl IntoIterator<Item = &'c Class>,
    budget: usize,
    stop: &dyn Fn() -> bool,
) -> Result<CoverageRefs<'c>, Stopped> {
    let classes: Vec<&'c Class> = classes
        .into_iter()
        .filter(|c| c.as_str() != vocab::OWL_THING && c.as_str() != vocab::OWL_NOTHING)
        .collect();
    let all = |classes: &[&'c Class]| {
        let mut v: Vec<&'c Class> = classes.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };
    if !consistent || left_out.is_empty() {
        // nothing left out, or an inconsistency, which is sound and makes every class
        // unsatisfiable whatever was left out
        return Ok(CoverageRefs {
            consistency_known: true,
            ..CoverageRefs::default()
        });
    }
    let mut ix = Index::new(axioms);
    let wanted: FxHashSet<&AxiomKind> = left_out.iter().copied().collect();
    let mut left = vec![false; ix.axioms.len()];
    let mut found: FxHashSet<&AxiomKind> = FxHashSet::default();
    for (j, a) in ix.axioms.iter().enumerate() {
        if wanted.contains(a) {
            left[j] = true;
            found.insert(a);
        }
    }
    // the left-out axioms first in each list, so that an extension that reaches one
    // stops before it checks the others
    for list in ix.by_trigger.iter_mut().chain(ix.by_entity.iter_mut()) {
        list.sort_by_key(|&j| !left[j as usize]);
    }
    let ix = ix;
    let work = Work {
        done: std::cell::Cell::new(0),
        budget,
        stop,
        halted: std::cell::Cell::new(None),
    };
    // the answer when the consistency is not known: every class incomplete
    let unknown = |over_budget| CoverageRefs {
        consistency_known: false,
        incomplete: all(&classes),
        over_budget,
    };
    // a left-out declaration or annotation is no logical content
    let missing = wanted
        .iter()
        .any(|a| !found.contains(a) && Index::indexed(a));
    if missing {
        return Ok(unknown(false));
    }
    // whether the extraction reached a left-out axiom, or None when it halted
    let hits = |x: &mut Extraction<'_>| -> Option<bool> {
        let mut hit = false;
        x.extend(
            &[],
            &mut |j| {
                if left[j] {
                    hit = true;
                    Step::Stop
                } else {
                    Step::Go
                }
            },
            &mut |_| Step::Go,
        );
        if !hit && work.halted.get().is_some() {
            None
        } else {
            Some(hit)
        }
    };
    // the answer of a pass that halted: every class incomplete, after the caller's stop
    // an error
    let halted = || match work.halted.get() {
        Some(Halt::Stopped) => Err(Stopped),
        _ => Ok(unknown(true)),
    };
    // layer 1's base, which is also the first test of layer 2
    let mut bottom = Extraction::new(&ix, Mode::Bottom, None, &work);
    let Some(bottom_hit) = hits(&mut bottom) else {
        return halted();
    };
    if bottom_hit {
        let mut t = Extraction::new(&ix, Mode::Top, None, &work);
        let Some(top_hit) = hits(&mut t) else {
            return halted();
        };
        if top_hit {
            return Ok(unknown(false));
        }
    }
    let consistency_known = true;
    // layer 3 when its gates hold, which decides every class layer 1 decides and more
    let terminology = ix.terminology_gate();
    let (mut base, base_hit) = match &terminology {
        Some(mask) => {
            let mut t = Extraction::new(&ix, Mode::Bottom, Some(mask), &work);
            let Some(hit) = hits(&mut t) else {
                return halted();
            };
            (t, hit)
        }
        None => (bottom, bottom_hit),
    };
    if base_hit {
        return Ok(CoverageRefs {
            consistency_known,
            incomplete: all(&classes),
            ..CoverageRefs::default()
        });
    }
    base.freeze();
    // what is known of each entity's module: 1 holds no left-out axiom, 2 holds one
    let mut status = vec![0u8; ix.names.len()];
    for e in base.signature_ids() {
        status[e as usize] = 1;
    }
    let mut incomplete: Vec<&'c Class> = Vec::new();
    let mut over_budget = false;
    for &c in &classes {
        if over_budget {
            // past the bound, a class no clean module covered is incomplete
            if ix.id(c.as_str()).is_some_and(|id| status[id as usize] != 1) {
                incomplete.push(c);
            }
            continue;
        }
        let Some(id) = ix.id(c.as_str()) else {
            // named by no axiom: its module is the base's
            continue;
        };
        match status[id as usize] {
            1 => continue,
            2 => {
                incomplete.push(c);
                continue;
            }
            _ => {}
        }
        let dirty = std::cell::Cell::new(false);
        base.begin();
        base.extend(
            &[id],
            &mut |j| {
                if left[j] {
                    dirty.set(true);
                    Step::Stop
                } else {
                    Step::Go
                }
            },
            &mut |e| {
                if status[e as usize] == 2 {
                    dirty.set(true);
                    Step::Stop
                } else {
                    Step::Go
                }
            },
        );
        if let Some(h) = work.halted.get()
            && !dirty.get()
        {
            if h == Halt::Stopped {
                return Err(Stopped);
            }
            over_budget = true;
            incomplete.push(c);
        } else if dirty.get() {
            status[id as usize] = 2;
            incomplete.push(c);
        } else {
            // the module of each entity of a clean module is inside it, so clean too
            for e in base.added() {
                status[e as usize] = 1;
            }
            status[id as usize] = 1;
        }
    }
    incomplete.sort_unstable();
    incomplete.dedup();
    Ok(CoverageRefs {
        consistency_known,
        incomplete,
        over_budget,
    })
}

/// Owned wrapper over [`coverage_refs`] used by module coverage tests.
#[cfg(test)]
pub(crate) fn coverage<'c>(
    axioms: &[&AxiomKind],
    left_out: &[&AxiomKind],
    consistent: bool,
    classes: impl IntoIterator<Item = &'c Class>,
    budget: usize,
    stop: &dyn Fn() -> bool,
) -> Result<Coverage, Stopped> {
    let c = coverage_refs(axioms, left_out, consistent, classes, budget, stop)?;
    Ok(Coverage {
        consistency_known: c.consistency_known,
        incomplete: c.incomplete.into_iter().cloned().collect(),
        over_budget: c.over_budget,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Halt {
    Budget,
    Stopped,
}

/// The work of a pass: the axioms its extractions looked at, the bound on them and the
/// caller's stop, which every extraction of the pass shares.
struct Work<'s> {
    done: std::cell::Cell<usize>,
    budget: usize,
    stop: &'s dyn Fn() -> bool,
    halted: std::cell::Cell<Option<Halt>>,
}

impl Work<'_> {
    /// Counts one axiom looked at; false when the pass halts.
    fn step(&self) -> bool {
        if self.halted.get().is_some() {
            return false;
        }
        let n = self.done.get() + 1;
        self.done.set(n);
        if n > self.budget {
            self.halted.set(Some(Halt::Budget));
            return false;
        }
        if n.is_multiple_of(POLL_EVERY) && (self.stop)() {
            self.halted.set(Some(Halt::Stopped));
            return false;
        }
        true
    }
}

/// The logical axioms with the entities of each, by numbers.
struct Index<'a> {
    /// the logical axioms other than declarations
    axioms: Vec<&'a AxiomKind>,
    /// the place in `axioms` of each axiom given
    place: Vec<Option<u32>>,
    /// the entities of each axiom
    sigs: Vec<Vec<u32>>,
    /// the axioms of each entity
    by_entity: Vec<Vec<u32>>,
    /// the axioms whose bottom-locality each entity can change ([`bottom_deps`]): an
    /// entity outside this list enters the signature without changing it
    by_trigger: Vec<Vec<u32>>,
    /// for each axiom, the sets of entities one of which must lie in the signature for
    /// the axiom not to be bottom-local ([`bottom_needs`]), or None when there are none
    needs: Vec<Option<Vec<Vec<u32>>>>,
    ids: FxHashMap<String, u32>,
    names: Vec<String>,
}

impl<'a> Index<'a> {
    fn indexed(k: &AxiomKind) -> bool {
        crate::classify::logical(k) && !matches!(k, AxiomKind::Declaration(_))
    }

    fn new(given: &[&'a AxiomKind]) -> Index<'a> {
        let mut ix = Index {
            axioms: Vec::new(),
            place: Vec::with_capacity(given.len()),
            sigs: Vec::new(),
            by_entity: Vec::new(),
            by_trigger: Vec::new(),
            needs: Vec::new(),
            ids: FxHashMap::default(),
            names: Vec::new(),
        };
        for k in given {
            if !Index::indexed(k) {
                ix.place.push(None);
                continue;
            }
            let j = ix.axioms.len() as u32;
            ix.place.push(Some(j));
            ix.axioms.push(k);
            let mut s = Signature::default();
            s.axiom(k);
            let mut sig = Vec::with_capacity(s.0.len());
            for e in s.0 {
                let iri = e.iri().as_str();
                let id = match ix.ids.get(iri) {
                    Some(&id) => id,
                    None => {
                        let id = ix.names.len() as u32;
                        ix.ids.insert(iri.to_string(), id);
                        ix.names.push(iri.to_string());
                        ix.by_entity.push(Vec::new());
                        ix.by_trigger.push(Vec::new());
                        id
                    }
                };
                // an IRI that two kinds of entity share counts once
                if !sig.contains(&id) {
                    sig.push(id);
                    ix.by_entity[id as usize].push(j);
                }
            }
            ix.sigs.push(sig);
            // an IRI without a number is in no signature, so a set holding it is never
            // inside one and drops out
            // an axiom with an ill-typed literal is never local, so nothing it needs
            let needs = bottom_needs(k).filter(|_| !ill_literal(k)).map(|alts| {
                alts.into_iter()
                    .filter_map(|set| {
                        set.into_iter()
                            .map(|iri| ix.ids.get(iri).copied())
                            .collect::<Option<Vec<u32>>>()
                    })
                    .collect()
            });
            ix.needs.push(needs);
            let mut deps: Vec<&str> = Vec::new();
            bottom_deps(k, &mut deps);
            deps.sort_unstable();
            deps.dedup();
            for iri in deps {
                // an entity the signature walk does not name, such as owl:Thing, is never
                // in a signature that the extraction grows
                if let Some(&id) = ix.ids.get(iri) {
                    ix.by_trigger[id as usize].push(j);
                }
            }
        }
        ix
    }

    fn id(&self, iri: &str) -> Option<u32> {
        self.ids.get(iri).copied()
    }

    fn axiom_of(&self, given: usize) -> Option<usize> {
        self.place[given].map(|j| j as usize)
    }

    /// The axioms of the terminology, when layer 3's syntactic gates hold: no axiom
    /// outside the assertions and keys holds a nominal, and no axiom holds the universal
    /// object property.
    fn terminology_gate(&self) -> Option<Vec<bool>> {
        if self.ids.contains_key(vocab::OWL_TOP_OBJECT_PROPERTY) {
            return None;
        }
        let mut mask = Vec::with_capacity(self.axioms.len());
        for k in &self.axioms {
            let assertion = assertion_or_key(k);
            if !assertion && names_nominal(k) {
                return None;
            }
            mask.push(!assertion);
        }
        Some(mask)
    }
}

/// Whether an axiom is an assertion or a key, the axioms that constrain only named
/// individuals and that layer 3 leaves out.
fn assertion_or_key(k: &AxiomKind) -> bool {
    matches!(
        k,
        AxiomKind::ClassAssertion(..)
            | AxiomKind::ObjectPropertyAssertion(..)
            | AxiomKind::NegativeObjectPropertyAssertion(..)
            | AxiomKind::DataPropertyAssertion(..)
            | AxiomKind::NegativeDataPropertyAssertion(..)
            | AxiomKind::SameIndividual(..)
            | AxiomKind::DifferentIndividuals(..)
            | AxiomKind::HasKey(..)
    )
}

/// Whether an axiom holds `ObjectOneOf` or `ObjectHasValue`, the class expressions that
/// name individuals, named or anonymous.
fn names_nominal(k: &AxiomKind) -> bool {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => nominal(a) || nominal(b),
        K::EquivalentClasses(cs) => cs.iter().any(nominal),
        K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => cs.iter().any(nominal),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::ClassAssertion(c, _)
        | K::HasKey(c, _, _) => nominal(c),
        _ => false,
    }
}

fn nominal(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::ObjectOneOf(_) | C::ObjectHasValue(..) => true,
        C::Class(_) | C::ObjectHasSelf(_) => false,
        C::ObjectComplementOf(c) => nominal(c),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().any(nominal),
        C::ObjectSomeValuesFrom(_, c) | C::ObjectAllValuesFrom(_, c) => nominal(c),
        C::ObjectMinCardinality(_, _, c)
        | C::ObjectMaxCardinality(_, _, c)
        | C::ObjectExactCardinality(_, _, c) => c.as_deref().is_some_and(nominal),
        C::DataSomeValuesFrom(..)
        | C::DataAllValuesFrom(..)
        | C::DataHasValue(..)
        | C::DataMinCardinality(..)
        | C::DataMaxCardinality(..)
        | C::DataExactCardinality(..) => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Bottom,
    Top,
}

enum Step {
    Go,
    Stop,
}

/// A module being extracted, which can be kept as the base of several extensions: the
/// state of the base is frozen, and each extension after [`begin`](Extraction::begin)
/// adds to it by stamps that the next one forgets.
struct Extraction<'i> {
    ix: &'i Index<'i>,
    work: Option<&'i Work<'i>>,
    mode: Mode,
    include: Option<&'i [bool]>,
    base_sig: Vec<bool>,
    base_mod: Vec<bool>,
    sig_stamp: Vec<u32>,
    mod_stamp: Vec<u32>,
    stamp: u32,
    frozen: bool,
    added: Vec<u32>,
}

impl<'i> Extraction<'i> {
    fn new(
        ix: &'i Index<'i>,
        mode: Mode,
        include: Option<&'i Vec<bool>>,
        work: impl Into<Option<&'i Work<'i>>>,
    ) -> Extraction<'i> {
        Extraction {
            ix,
            work: work.into(),
            mode,
            include: include.map(|v| v.as_slice()),
            base_sig: vec![false; ix.names.len()],
            base_mod: vec![false; ix.axioms.len()],
            sig_stamp: Vec::new(),
            mod_stamp: Vec::new(),
            stamp: 0,
            frozen: false,
            added: Vec::new(),
        }
    }

    fn freeze(&mut self) {
        self.frozen = true;
        // no stamp is current until the first extension begins
        self.stamp = 1;
        self.sig_stamp = vec![0; self.ix.names.len()];
        self.mod_stamp = vec![0; self.ix.axioms.len()];
    }

    /// Starts an extension of the frozen base.
    fn begin(&mut self) {
        self.stamp += 1;
        self.added.clear();
    }

    /// The entities an extension added to the base.
    fn added(&self) -> impl Iterator<Item = u32> + '_ {
        self.added.iter().copied()
    }

    fn signature_ids(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.ix.names.len() as u32).filter(|&e| self.has_id(e))
    }

    fn signature(&self) -> impl Iterator<Item = &str> + '_ {
        self.signature_ids()
            .map(|e| self.ix.names[e as usize].as_str())
    }

    fn has_id(&self, e: u32) -> bool {
        self.base_sig[e as usize] || (self.frozen && self.sig_stamp[e as usize] == self.stamp)
    }

    fn in_module(&self, j: usize) -> bool {
        self.base_mod[j] || (self.frozen && self.mod_stamp[j] == self.stamp)
    }

    fn add_entity(&mut self, e: u32) -> bool {
        if self.has_id(e) {
            return false;
        }
        if self.frozen {
            self.sig_stamp[e as usize] = self.stamp;
            self.added.push(e);
        } else {
            self.base_sig[e as usize] = true;
        }
        true
    }

    fn add_axiom(&mut self, j: usize) {
        if self.frozen {
            self.mod_stamp[j] = self.stamp;
        } else {
            self.base_mod[j] = true;
        }
    }

    fn local(&self, j: usize) -> bool {
        let has = |iri: &str| self.ix.id(iri).is_some_and(|e| self.has_id(e));
        let k = self.ix.axioms[j];
        match self.mode {
            Mode::Bottom => bottom_local(k, &Sig(&has)),
            Mode::Top => top_local(k, &Sig(&has)),
        }
    }

    /// Adds `seed` to the signature and the axioms that are then not local, until none
    /// is. An unfrozen extraction first takes the axioms that are not local for the
    /// empty signature. `axiom` sees each axiom before it enters, and `entity` each
    /// entity, and either can stop the extraction.
    fn extend(
        &mut self,
        seed: &[u32],
        axiom: &mut dyn FnMut(usize) -> Step,
        entity: &mut dyn FnMut(u32) -> Step,
    ) {
        let mut queue: Vec<u32> = Vec::new();
        for &e in seed {
            if let Step::Stop = entity(e) {
                return;
            }
            if self.add_entity(e) {
                queue.push(e);
            }
        }
        if !self.frozen {
            for j in 0..self.ix.axioms.len() {
                if !self.take(j, axiom, entity, &mut queue) {
                    return;
                }
            }
        }
        // an axiom whose locality the new entity cannot change needs no check
        let ix: &'i Index<'i> = self.ix;
        let lists = match self.mode {
            Mode::Bottom => &ix.by_trigger,
            Mode::Top => &ix.by_entity,
        };
        while let Some(e) = queue.pop() {
            for &j in &lists[e as usize] {
                if !self.take(j as usize, axiom, entity, &mut queue) {
                    return;
                }
            }
        }
    }

    /// Takes the axiom `j` into the module when it is included and not local; false when
    /// a callback stopped the extraction.
    fn take(
        &mut self,
        j: usize,
        axiom: &mut dyn FnMut(usize) -> Step,
        entity: &mut dyn FnMut(u32) -> Step,
        queue: &mut Vec<u32>,
    ) -> bool {
        if self.include.is_some_and(|m| !m[j]) || self.in_module(j) {
            return true;
        }
        if let Some(w) = self.work
            && !w.step()
        {
            return false;
        }
        // the cheap necessary condition first, then the rules
        if self.mode == Mode::Bottom
            && let Some(alts) = &self.ix.needs[j]
            && !alts.iter().any(|set| set.iter().all(|&e| self.has_id(e)))
        {
            return true;
        }
        if self.local(j) {
            return true;
        }
        if let Step::Stop = axiom(j) {
            return false;
        }
        self.add_axiom(j);
        for i in 0..self.ix.sigs[j].len() {
            let e = self.ix.sigs[j][i];
            if self.has_id(e) {
                continue;
            }
            if let Step::Stop = entity(e) {
                return false;
            }
            self.add_entity(e);
            queue.push(e);
        }
        true
    }
}

struct Sig<'s>(&'s dyn Fn(&str) -> bool);

impl Sig<'_> {
    fn has(&self, iri: &str) -> bool {
        (self.0)(iri)
    }

    /// Bottom mode: whether the property is empty once the entities outside the
    /// signature are. The universal property never is.
    fn outside_ope(&self, p: &ObjectPropertyExpression) -> bool {
        let n = p.named().as_str();
        n != vocab::OWL_TOP_OBJECT_PROPERTY && !self.has(n)
    }

    fn outside_dp(&self, p: &DataProperty) -> bool {
        p.as_str() != vocab::OWL_TOP_DATA_PROPERTY && !self.has(p.as_str())
    }

    /// Bottom mode: whether the class is empty once the entities outside the signature
    /// are.
    fn bottom_class(&self, c: &Class) -> bool {
        c.as_str() == vocab::OWL_NOTHING || c.as_str() != vocab::OWL_THING && !self.has(c.as_str())
    }

    /// Bottom mode: whether the expression is empty once the entities outside the
    /// signature are.
    fn bottom(&self, c: &ClassExpression) -> bool {
        use ClassExpression as C;
        match c {
            C::Class(c) => self.bottom_class(c),
            C::ObjectComplementOf(c) => self.top(c),
            C::ObjectIntersectionOf(cs) => cs.iter().any(|c| self.bottom(c)),
            C::ObjectUnionOf(cs) => cs.iter().all(|c| self.bottom(c)),
            C::ObjectSomeValuesFrom(p, c) => self.outside_ope(p) || self.bottom(c),
            C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => self.outside_ope(p),
            C::ObjectMinCardinality(n, p, c) | C::ObjectExactCardinality(n, p, c) => {
                *n > 0 && (self.outside_ope(p) || c.as_deref().is_some_and(|c| self.bottom(c)))
            }
            C::DataSomeValuesFrom(ps, _) => ps.iter().any(|p| self.outside_dp(p)),
            C::DataHasValue(p, _) => self.outside_dp(p),
            C::DataMinCardinality(n, p, _) | C::DataExactCardinality(n, p, _) => {
                *n > 0 && self.outside_dp(p)
            }
            C::ObjectOneOf(is) => is.is_empty(),
            C::ObjectAllValuesFrom(..)
            | C::ObjectMaxCardinality(..)
            | C::DataAllValuesFrom(..)
            | C::DataMaxCardinality(..) => false,
        }
    }

    /// Bottom mode: whether the expression is everything once the entities outside the
    /// signature are empty.
    fn top(&self, c: &ClassExpression) -> bool {
        use ClassExpression as C;
        match c {
            C::Class(c) => c.as_str() == vocab::OWL_THING,
            C::ObjectComplementOf(c) => self.bottom(c),
            C::ObjectIntersectionOf(cs) => cs.iter().all(|c| self.top(c)),
            C::ObjectUnionOf(cs) => cs.iter().any(|c| self.top(c)),
            C::ObjectAllValuesFrom(p, c) => self.outside_ope(p) || self.top(c),
            C::ObjectMaxCardinality(_, p, c) => {
                self.outside_ope(p) || c.as_deref().is_some_and(|c| self.bottom(c))
            }
            C::ObjectMinCardinality(0, ..) | C::DataMinCardinality(0, ..) => true,
            C::DataAllValuesFrom(ps, _) => ps.iter().any(|p| self.outside_dp(p)),
            C::DataMaxCardinality(_, p, _) => self.outside_dp(p),
            _ => false,
        }
    }

    /// Top mode: whether the property is every pair once the entities outside the
    /// signature are. The empty property never is.
    fn full_ope(&self, p: &ObjectPropertyExpression) -> bool {
        let n = p.named().as_str();
        n == vocab::OWL_TOP_OBJECT_PROPERTY
            || n != vocab::OWL_BOTTOM_OBJECT_PROPERTY && !self.has(n)
    }

    fn full_dp(&self, p: &DataProperty) -> bool {
        let n = p.as_str();
        n == vocab::OWL_TOP_DATA_PROPERTY || n != vocab::OWL_BOTTOM_DATA_PROPERTY && !self.has(n)
    }

    /// Top mode: whether the expression is everything once the classes outside the
    /// signature are everything and the properties outside it every pair.
    fn t_top(&self, c: &ClassExpression) -> bool {
        use ClassExpression as C;
        match c {
            C::Class(c) => {
                c.as_str() == vocab::OWL_THING
                    || c.as_str() != vocab::OWL_NOTHING && !self.has(c.as_str())
            }
            C::ObjectComplementOf(c) => self.t_bottom(c),
            C::ObjectIntersectionOf(cs) => cs.iter().all(|c| self.t_top(c)),
            C::ObjectUnionOf(cs) => cs.iter().any(|c| self.t_top(c)),
            // every element has every element as a successor, and the domain is not empty
            C::ObjectSomeValuesFrom(p, c) => self.full_ope(p) && self.t_top(c),
            C::ObjectAllValuesFrom(_, c) => self.t_top(c),
            C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => self.full_ope(p),
            C::ObjectMinCardinality(0, ..) | C::DataMinCardinality(0, ..) => true,
            C::ObjectMinCardinality(1, p, c) => {
                self.full_ope(p) && c.as_deref().is_none_or(|c| self.t_top(c))
            }
            C::ObjectMaxCardinality(_, _, c) => c.as_deref().is_some_and(|c| self.t_bottom(c)),
            // a data property every pair would need every literal well typed, so the data
            // expressions are not decided
            _ => false,
        }
    }

    /// Top mode: whether the expression is empty once the classes outside the signature
    /// are everything and the properties outside it every pair.
    fn t_bottom(&self, c: &ClassExpression) -> bool {
        use ClassExpression as C;
        match c {
            C::Class(c) => c.as_str() == vocab::OWL_NOTHING,
            C::ObjectComplementOf(c) => self.t_top(c),
            C::ObjectIntersectionOf(cs) => cs.iter().any(|c| self.t_bottom(c)),
            C::ObjectUnionOf(cs) => cs.iter().all(|c| self.t_bottom(c)),
            C::ObjectSomeValuesFrom(_, c) => self.t_bottom(c),
            C::ObjectMinCardinality(n, _, Some(c)) | C::ObjectExactCardinality(n, _, Some(c)) => {
                *n > 0 && self.t_bottom(c)
            }
            // every element has a successor, none of them in the filler
            C::ObjectAllValuesFrom(p, c) => self.full_ope(p) && self.t_bottom(c),
            C::ObjectOneOf(is) => is.is_empty(),
            _ => false,
        }
    }
}

/// The IRIs whose presence in the signature [`bottom_local`] can read for the axiom, in
/// any branch: its answer is a function of them alone, so an entity outside them enters
/// the signature without changing it. Each case mirrors one of [`bottom_local`].
fn bottom_deps<'k>(k: &'k AxiomKind, out: &mut Vec<&'k str>) {
    use AxiomKind as K;
    let ope =
        |p: &'k ObjectPropertyExpression, out: &mut Vec<&'k str>| out.push(p.named().as_str());
    match k {
        K::SubClassOf(a, b) => {
            ce_bottom_deps(a, out);
            ce_top_deps(b, out);
        }
        K::EquivalentClasses(cs) => {
            for c in cs {
                ce_bottom_deps(c, out);
                ce_top_deps(c, out);
            }
        }
        K::DisjointClasses(cs) => cs.iter().for_each(|c| ce_bottom_deps(c, out)),
        K::DisjointUnion(c, cs) => {
            out.push(c.as_str());
            cs.iter().for_each(|c| ce_bottom_deps(c, out));
        }
        K::SubObjectPropertyOf(p, _) => match p {
            SubObjectPropertyExpression::Expression(p) => ope(p, out),
            SubObjectPropertyExpression::Chain(ps) => ps.iter().for_each(|p| ope(p, out)),
        },
        K::EquivalentObjectProperties(ps) => ps.iter().for_each(|p| ope(p, out)),
        K::DisjointObjectProperties(ps) => ps.iter().for_each(|p| ope(p, out)),
        K::InverseObjectProperties(p, q) => {
            ope(p, out);
            ope(q, out);
        }
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            ope(p, out);
            ce_top_deps(c, out);
        }
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p)
        | K::NegativeObjectPropertyAssertion(p, _, _) => ope(p, out),
        K::SubDataPropertyOf(p, _)
        | K::DataPropertyRange(p, _)
        | K::FunctionalDataProperty(p)
        | K::NegativeDataPropertyAssertion(p, _, _) => out.push(p.as_str()),
        K::EquivalentDataProperties(ps) => out.extend(ps.iter().map(|p| p.as_str())),
        K::DisjointDataProperties(ps) => out.extend(ps.iter().map(|p| p.as_str())),
        K::DataPropertyDomain(p, c) => {
            out.push(p.as_str());
            ce_top_deps(c, out);
        }
        K::ClassAssertion(c, _) => ce_top_deps(c, out),
        K::HasKey(c, ps, ds) => {
            ce_bottom_deps(c, out);
            ps.iter().for_each(|p| ope(p, out));
            out.extend(ds.iter().map(|p| p.as_str()));
        }
        // local for every signature, or for none
        _ => {}
    }
}

/// Sets of IRIs one of which must lie wholly in the signature for the axiom not to be
/// bottom-local, or None when [`bottom_local`] can call it non-local whatever the
/// signature holds. Each case follows from one of [`bottom_local`]: an axiom about classes
/// needs a class expression that is not empty, and one about properties a property that
/// is not. An empty list means the axiom is local for every signature.
fn bottom_needs(k: &AxiomKind) -> Option<Vec<Vec<&str>>> {
    use AxiomKind as K;
    fn ope(p: &ObjectPropertyExpression) -> Vec<&str> {
        named_ope(p).into_iter().collect()
    }
    fn dp(p: &DataProperty) -> Vec<&str> {
        named_dp(p).into_iter().collect()
    }
    fn each(sets: Vec<Option<Vec<&str>>>) -> Vec<Vec<&str>> {
        sets.into_iter().flatten().collect()
    }
    Some(match k {
        K::SubClassOf(a, _) => each(vec![nonempty_needs(a)]),
        K::EquivalentClasses(cs) => each(cs.iter().map(nonempty_needs).collect()),
        K::DisjointClasses(cs) => each(cs.iter().map(nonempty_needs).collect()),
        K::DisjointUnion(c, cs) => {
            let mut sets = vec![class_needs(c)];
            sets.extend(cs.iter().map(nonempty_needs));
            each(sets)
        }
        K::SubObjectPropertyOf(p, _) => vec![match p {
            SubObjectPropertyExpression::Expression(p) => ope(p),
            SubObjectPropertyExpression::Chain(ps) => ps.iter().flat_map(ope).collect(),
        }],
        K::EquivalentObjectProperties(ps) => ps.iter().map(ope).collect(),
        K::DisjointObjectProperties(ps) => ps.iter().map(ope).collect(),
        K::InverseObjectProperties(p, q) => vec![ope(p), ope(q)],
        K::ObjectPropertyDomain(p, _)
        | K::ObjectPropertyRange(p, _)
        | K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p)
        | K::NegativeObjectPropertyAssertion(p, _, _) => vec![ope(p)],
        K::SubDataPropertyOf(p, _)
        | K::DataPropertyDomain(p, _)
        | K::DataPropertyRange(p, _)
        | K::FunctionalDataProperty(p)
        | K::NegativeDataPropertyAssertion(p, _, _) => vec![dp(p)],
        K::EquivalentDataProperties(ps) => ps.iter().map(dp).collect(),
        K::DisjointDataProperties(ps) => ps.iter().map(dp).collect(),
        K::HasKey(c, ps, ds) => each(vec![nonempty_needs(c).map(|mut set| {
            set.extend(ps.iter().flat_map(ope));
            set.extend(ds.iter().flat_map(dp));
            set
        })]),
        K::Declaration(_)
        | K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => Vec::new(),
        _ => return None,
    })
}

/// The property's IRI, which a signature must hold for the property not to be empty, or
/// None for the universal property, which never is.
fn named_ope(p: &ObjectPropertyExpression) -> Option<&str> {
    let n = p.named().as_str();
    (n != vocab::OWL_TOP_OBJECT_PROPERTY).then_some(n)
}

fn named_dp(p: &DataProperty) -> Option<&str> {
    let n = p.as_str();
    (n != vocab::OWL_TOP_DATA_PROPERTY).then_some(n)
}

/// The IRIs that must all lie in the signature for [`Sig::bottom`] to be false of the
/// expression, or None when it is true whatever the signature, as for owl:Nothing.
fn nonempty_needs(c: &ClassExpression) -> Option<Vec<&str>> {
    use ClassExpression as C;
    Some(match c {
        C::Class(c) => return class_needs(c),
        C::ObjectIntersectionOf(cs) => {
            let mut set = Vec::new();
            for c in cs {
                set.extend(nonempty_needs(c)?);
            }
            set
        }
        C::ObjectSomeValuesFrom(p, c) => {
            let mut set = nonempty_needs(c)?;
            set.extend(named_ope(p));
            set
        }
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => named_ope(p).into_iter().collect(),
        C::ObjectMinCardinality(n, p, c) | C::ObjectExactCardinality(n, p, c) if *n > 0 => {
            let mut set = match c {
                Some(c) => nonempty_needs(c)?,
                None => Vec::new(),
            };
            set.extend(named_ope(p));
            set
        }
        C::DataSomeValuesFrom(ps, _) => ps.iter().filter_map(named_dp).collect(),
        C::DataHasValue(p, _) => named_dp(p).into_iter().collect(),
        C::DataMinCardinality(n, p, _) | C::DataExactCardinality(n, p, _) if *n > 0 => {
            named_dp(p).into_iter().collect()
        }
        C::ObjectOneOf(is) if is.is_empty() => return None,
        // a union, a complement, a universal or an upper bound can be non-empty with no
        // entity of its own in the signature
        _ => Vec::new(),
    })
}

/// [`nonempty_needs`] of a class.
fn class_needs(c: &Class) -> Option<Vec<&str>> {
    match c.as_str() {
        vocab::OWL_NOTHING => None,
        vocab::OWL_THING => Some(Vec::new()),
        iri => Some(vec![iri]),
    }
}

/// The IRIs [`Sig::bottom`] can read for the expression.
fn ce_bottom_deps<'k>(c: &'k ClassExpression, out: &mut Vec<&'k str>) {
    use ClassExpression as C;
    match c {
        C::Class(c) => out.push(c.as_str()),
        C::ObjectComplementOf(c) => ce_top_deps(c, out),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            cs.iter().for_each(|c| ce_bottom_deps(c, out))
        }
        C::ObjectSomeValuesFrom(p, c) => {
            out.push(p.named().as_str());
            ce_bottom_deps(c, out);
        }
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => out.push(p.named().as_str()),
        C::ObjectMinCardinality(_, p, c) | C::ObjectExactCardinality(_, p, c) => {
            out.push(p.named().as_str());
            if let Some(c) = c {
                ce_bottom_deps(c, out);
            }
        }
        C::DataSomeValuesFrom(ps, _) => out.extend(ps.iter().map(|p| p.as_str())),
        C::DataHasValue(p, _)
        | C::DataMinCardinality(_, p, _)
        | C::DataExactCardinality(_, p, _) => out.push(p.as_str()),
        C::ObjectOneOf(_)
        | C::ObjectAllValuesFrom(..)
        | C::ObjectMaxCardinality(..)
        | C::DataAllValuesFrom(..)
        | C::DataMaxCardinality(..) => {}
    }
}

/// The IRIs [`Sig::top`] can read for the expression.
fn ce_top_deps<'k>(c: &'k ClassExpression, out: &mut Vec<&'k str>) {
    use ClassExpression as C;
    match c {
        C::ObjectComplementOf(c) => ce_bottom_deps(c, out),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            cs.iter().for_each(|c| ce_top_deps(c, out))
        }
        C::ObjectAllValuesFrom(p, c) => {
            out.push(p.named().as_str());
            ce_top_deps(c, out);
        }
        C::ObjectMaxCardinality(_, p, c) => {
            out.push(p.named().as_str());
            if let Some(c) = c {
                ce_bottom_deps(c, out);
            }
        }
        C::DataAllValuesFrom(ps, _) => out.extend(ps.iter().map(|p| p.as_str())),
        C::DataMaxCardinality(_, p, _) => out.push(p.as_str()),
        _ => {}
    }
}

/// Whether a literal is outside the lexical space of its datatype, which no
/// interpretation gives a value, so that an axiom holding it is never local.
fn ill_typed(l: &Literal) -> bool {
    crate::datatypes::lexical(l.datatype(), l.value()) == Some(false)
}

fn ill_range(r: &DataRange) -> bool {
    match r {
        DataRange::Datatype(_) => false,
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => rs.iter().any(ill_range),
        DataRange::ComplementOf(r) => ill_range(r),
        DataRange::OneOf(ls) => ls.iter().any(ill_typed),
        DataRange::DatatypeRestriction(_, fs) => fs.iter().any(|f| ill_typed(&f.value)),
    }
}

fn ill_class(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().any(ill_class),
        C::ObjectComplementOf(c) => ill_class(c),
        C::ObjectSomeValuesFrom(_, c) | C::ObjectAllValuesFrom(_, c) => ill_class(c),
        C::ObjectMinCardinality(_, _, c)
        | C::ObjectMaxCardinality(_, _, c)
        | C::ObjectExactCardinality(_, _, c) => c.as_deref().is_some_and(ill_class),
        C::DataSomeValuesFrom(_, r) | C::DataAllValuesFrom(_, r) => ill_range(r),
        C::DataHasValue(_, l) => ill_typed(l),
        C::DataMinCardinality(_, _, r)
        | C::DataMaxCardinality(_, _, r)
        | C::DataExactCardinality(_, _, r) => r.as_ref().is_some_and(ill_range),
        _ => false,
    }
}

/// Whether an axiom holds an ill-typed literal ([`ill_typed`]).
fn ill_literal(k: &AxiomKind) -> bool {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => ill_class(a) || ill_class(b),
        K::EquivalentClasses(cs) => cs.iter().any(ill_class),
        K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => cs.iter().any(ill_class),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::ClassAssertion(c, _)
        | K::HasKey(c, _, _) => ill_class(c),
        K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => ill_range(r),
        K::DataPropertyAssertion(_, _, l) | K::NegativeDataPropertyAssertion(_, _, l) => {
            ill_typed(l)
        }
        _ => false,
    }
}

/// Whether the axiom is bottom-local for the signature: a tautology once every entity
/// outside it is empty.
fn bottom_local(k: &AxiomKind, s: &Sig<'_>) -> bool {
    use AxiomKind as K;
    if ill_literal(k) {
        return false;
    }
    let sub_ope = |p: &SubObjectPropertyExpression| match p {
        SubObjectPropertyExpression::Expression(p) => s.outside_ope(p),
        SubObjectPropertyExpression::Chain(ps) => ps.iter().any(|p| s.outside_ope(p)),
    };
    match k {
        K::SubClassOf(a, b) => s.bottom(a) || s.top(b),
        K::EquivalentClasses(cs) => cs.iter().all(|c| s.bottom(c)) || cs.iter().all(|c| s.top(c)),
        K::DisjointClasses(cs) => cs.iter().filter(|c| !s.bottom(c)).count() <= 1,
        // the class is the union of the others, which owl:Thing never is of empty ones
        K::DisjointUnion(c, cs) => s.bottom_class(c) && cs.iter().all(|c| s.bottom(c)),
        K::SubObjectPropertyOf(p, _) => sub_ope(p),
        K::EquivalentObjectProperties(ps) => ps.iter().all(|p| s.outside_ope(p)),
        K::DisjointObjectProperties(ps) => ps.iter().filter(|p| !s.outside_ope(p)).count() <= 1,
        K::InverseObjectProperties(p, q) => s.outside_ope(p) && s.outside_ope(q),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            s.outside_ope(p) || s.top(c)
        }
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => s.outside_ope(p),
        K::SubDataPropertyOf(p, _) => s.outside_dp(p),
        K::EquivalentDataProperties(ps) => ps.iter().all(|p| s.outside_dp(p)),
        K::DisjointDataProperties(ps) => ps.iter().filter(|p| !s.outside_dp(p)).count() <= 1,
        K::DataPropertyDomain(p, c) => s.outside_dp(p) || s.top(c),
        K::DataPropertyRange(p, _) | K::FunctionalDataProperty(p) => s.outside_dp(p),
        K::ClassAssertion(c, _) => s.top(c),
        K::NegativeObjectPropertyAssertion(p, _, _) => s.outside_ope(p),
        K::NegativeDataPropertyAssertion(p, _, _) => s.outside_dp(p),
        // a key holds vacuously when its class is empty or a property of it has no
        // values; otherwise it can make named individuals equal, even with no property
        K::HasKey(c, ps, ds) => {
            s.bottom(c) || ps.iter().any(|p| s.outside_ope(p)) || ds.iter().any(|p| s.outside_dp(p))
        }
        K::Declaration(_) => true,
        K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => true,
        _ => false,
    }
}

/// Whether the axiom is top-local for the signature: a tautology once every class
/// outside it is everything and every property outside it every pair.
fn top_local(k: &AxiomKind, s: &Sig<'_>) -> bool {
    use AxiomKind as K;
    if ill_literal(k) {
        return false;
    }
    match k {
        K::SubClassOf(a, b) => s.t_bottom(a) || s.t_top(b),
        K::EquivalentClasses(cs) => {
            cs.iter().all(|c| s.t_top(c)) || cs.iter().all(|c| s.t_bottom(c))
        }
        K::DisjointClasses(cs) => cs.iter().filter(|c| !s.t_bottom(c)).count() <= 1,
        K::SubObjectPropertyOf(_, sup) => s.full_ope(sup),
        K::EquivalentObjectProperties(ps) => ps.iter().all(|p| s.full_ope(p)),
        K::InverseObjectProperties(p, q) => s.full_ope(p) && s.full_ope(q),
        K::ObjectPropertyDomain(_, c) | K::ObjectPropertyRange(_, c) => s.t_top(c),
        K::ReflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => s.full_ope(p),
        K::SubDataPropertyOf(_, sup) => s.full_dp(sup),
        K::EquivalentDataProperties(ps) => ps.iter().all(|p| s.full_dp(p)),
        K::DataPropertyDomain(_, c) => s.t_top(c),
        K::ClassAssertion(c, _) => s.t_top(c),
        K::ObjectPropertyAssertion(p, _, _) => s.full_ope(p),
        // every element has every data value, so a literal that has a value
        K::DataPropertyAssertion(p, _, l) => {
            s.full_dp(p) && crate::datatypes::lexical(l.datatype(), l.value()) == Some(true)
        }
        K::Declaration(_) => true,
        K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => true,
        // functional, irreflexive, asymmetric and disjoint properties fail for every
        // pair, keys can make individuals equal, and the data axioms are not decided
        _ => false,
    }
}

#[cfg(test)]
mod tests;
