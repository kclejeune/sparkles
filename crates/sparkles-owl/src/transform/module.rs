//! Syntactic locality modules (Cuenca Grau, Horrocks, Kazakov and Sattler, JAIR 2008).
//!
//! `reason --dump-unsatisfiable` writes the bottom-locality module of the unsatisfiable
//! classes, as ROBOT writes the `BOT` module that its `extract` computes. The module
//! holds every justification of each entailment over the signature.
//!
//! [`coverage`] decides, for a classification that left axioms out, which classes still
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
//! be.
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
    let mut x = Extraction::new(&ix, Mode::Bottom, None);
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
}

/// Which of `classes` keep their complete answer when a run over `axioms` left out or
/// used only in part the axioms `left_out`, and found the rest consistent or not. See the
/// [module documentation](self) for the argument. `owl:Thing` and `owl:Nothing` are never
/// listed. A left-out axiom that is not among `axioms` leaves the consistency unknown.
pub(crate) fn coverage<'c>(
    axioms: &[&AxiomKind],
    left_out: &[&AxiomKind],
    consistent: bool,
    classes: impl IntoIterator<Item = &'c Class>,
) -> Coverage {
    let classes: Vec<&Class> = classes
        .into_iter()
        .filter(|c| c.as_str() != vocab::OWL_THING && c.as_str() != vocab::OWL_NOTHING)
        .collect();
    let all = |classes: &[&Class]| {
        let mut v: Vec<Class> = classes.iter().map(|c| (*c).clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    if left_out.is_empty() || !consistent {
        // nothing left out, or an inconsistency, which is sound and makes every class
        // unsatisfiable whatever was left out
        return Coverage {
            consistency_known: true,
            incomplete: Vec::new(),
        };
    }
    let ix = Index::new(axioms);
    let wanted: FxHashSet<&AxiomKind> = left_out.iter().copied().collect();
    let mut left = vec![false; ix.axioms.len()];
    let mut found: FxHashSet<&AxiomKind> = FxHashSet::default();
    for (j, a) in ix.axioms.iter().enumerate() {
        if wanted.contains(a) {
            left[j] = true;
            found.insert(a);
        }
    }
    // a left-out declaration or annotation is no logical content
    let missing = wanted
        .iter()
        .any(|a| !found.contains(a) && Index::indexed(a));
    let unknown = Coverage {
        consistency_known: false,
        incomplete: all(&classes),
    };
    if missing {
        return unknown;
    }
    let hits = |x: &mut Extraction<'_>| {
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
        hit
    };
    // layer 1's base, which is also the first test of layer 2
    let mut bottom = Extraction::new(&ix, Mode::Bottom, None);
    let bottom_hit = hits(&mut bottom);
    let consistency_known = !bottom_hit || !hits(&mut Extraction::new(&ix, Mode::Top, None));
    if !consistency_known {
        return unknown;
    }
    // layer 3 when its gates hold, which decides every class layer 1 decides and more
    let terminology = ix.terminology_gate();
    let (mut base, base_hit) = match &terminology {
        Some(mask) => {
            let mut t = Extraction::new(&ix, Mode::Bottom, Some(mask));
            let hit = hits(&mut t);
            (t, hit)
        }
        None => (bottom, bottom_hit),
    };
    if base_hit {
        return Coverage {
            consistency_known,
            incomplete: all(&classes),
        };
    }
    base.freeze();
    // what is known of each entity's module: 1 holds no left-out axiom, 2 holds one
    let mut status = vec![0u8; ix.names.len()];
    for e in base.signature_ids() {
        status[e as usize] = 1;
    }
    let mut incomplete: Vec<Class> = Vec::new();
    for c in &classes {
        let Some(id) = ix.id(c.as_str()) else {
            // named by no axiom: its module is the base's
            continue;
        };
        match status[id as usize] {
            1 => continue,
            2 => {
                incomplete.push((*c).clone());
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
        if dirty.get() {
            status[id as usize] = 2;
            incomplete.push((*c).clone());
        } else {
            // the module of each entity of a clean module is inside it, so clean too
            for e in base.added() {
                status[e as usize] = 1;
            }
            status[id as usize] = 1;
        }
    }
    incomplete.sort();
    incomplete.dedup();
    Coverage {
        consistency_known,
        incomplete,
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
            let assertion = matches!(
                k,
                AxiomKind::ClassAssertion(..)
                    | AxiomKind::ObjectPropertyAssertion(..)
                    | AxiomKind::NegativeObjectPropertyAssertion(..)
                    | AxiomKind::DataPropertyAssertion(..)
                    | AxiomKind::NegativeDataPropertyAssertion(..)
                    | AxiomKind::SameIndividual(..)
                    | AxiomKind::DifferentIndividuals(..)
                    | AxiomKind::HasKey(..)
            );
            if !assertion && names_nominal(k) {
                return None;
            }
            mask.push(!assertion);
        }
        Some(mask)
    }
}

/// Whether an axiom holds `ObjectOneOf` or `ObjectHasValue`, the class expressions that
/// name individuals, named or anonymous.
fn names_nominal(k: &AxiomKind) -> bool {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => nominal(a) || nominal(b),
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            cs.iter().any(nominal)
        }
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
    fn new(ix: &'i Index<'i>, mode: Mode, include: Option<&'i Vec<bool>>) -> Extraction<'i> {
        Extraction {
            ix,
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
        while let Some(e) = queue.pop() {
            let list = &self.ix.by_entity[e as usize];
            for i in 0..list.len() {
                let j = self.ix.by_entity[e as usize][i] as usize;
                if !self.take(j, axiom, entity, &mut queue) {
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
        if self.include.is_some_and(|m| !m[j]) || self.in_module(j) || self.local(j) {
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

/// Whether the axiom is bottom-local for the signature: a tautology once every entity
/// outside it is empty.
fn bottom_local(k: &AxiomKind, s: &Sig<'_>) -> bool {
    use AxiomKind as K;
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
