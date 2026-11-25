//! The reductions of item 805 (F24, "Nominals in the disjunctive rules"): a rooted run
//! takes keys over object properties and the universal property by rewriting the axioms
//! before anything reads them.
//!
//! The universal property `owl:topObjectProperty` becomes a fresh property `U` that is
//! symmetric and transitive, with `⊤ ⊑ ∃U.{h}` for a fresh individual `h`, so that every
//! element reaches `h` and back and `U` relates every pair of elements. An inclusion or a
//! chain whose superproperty is the universal property, and a symmetry, transitivity or
//! reflexivity of it, hold of every ontology and are dropped. A domain or range of the
//! property holds of every element, and a number restriction, functionality, asymmetry,
//! irreflexivity or disjointness of it has no replacement, which leaves the ontology to the
//! run without the reductions.
//!
//! A key `HasKey(C (r_1 … r_m) ())` holds only of named individuals, and its values are
//! named too. For each named individual `b` other than the first and each key property
//! `r_j`, the axiom `{b} ⊓ C ⊑ ∀r_j.V_jb` marks the values of `b` when `b` is a `C`. For
//! each pair of distinct named individuals `a` and `b`, each `r_j` and each named individual
//! `c`, the axiom `{a} ⊓ ∃r_j.({c} ⊓ V_jb) ⊑ M_jab` marks that `a` has a named value of `b`,
//! and `{a} ⊓ C ⊓ M_1ab ⊓ … ⊓ M_mab ⊑ {b}` makes them equal. A model of the ontology
//! extends to the fresh classes, with `V_jb` the `r_j`-values of `b` when `b` is a `C` and
//! empty otherwise and `M_jab` the set `{a}` when `a` has a named `r_j`-value in `V_jb`, and
//! a model of the rewritten axioms satisfies the key. The marks stay on the values, so no
//! axiom links every element to the individuals, which the reduction through a fresh
//! property `W_b` with `⊤ ⊑ ∃W_b.{b}` did and which took most generated ontologies with
//! keys past the rooted run's bound. The reduction has about `m·n·n(n-1)/2` axioms for `n`
//! named individuals, and an ontology whose keys pass the bound is left to the run without
//! the reductions.
//!
//! Every fresh name has the IRI prefix [`PREFIX`], and an ontology that names anything with
//! it is left to the run without the reductions. The run hides the classes among them from
//! the taxonomy, a partial answer and a description, and the hub individual from a
//! description's instances. The axioms the rewriting makes stand for the original axioms in
//! the run's report.

use super::Excluded;
use super::dl;
use super::index::Gap;
use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{BTreeMap, BTreeSet};

/// The IRI prefix of the names that the reductions make up.
pub const PREFIX: &str = "urn:sparkles:internal:";

/// The most axioms that the keys' reductions make, whatever the memory budget.
const KEY_AXIOMS: usize = 1 << 17;

/// The bytes of memory budget that one reduction axiom may take.
const AXIOM_BYTES: usize = 2048;

/// Key-expansion steps between cooperative cancellation/deadline polls.
const POLL_KEY: usize = if cfg!(test) { 64 } else { 1024 };

/// Outcome of item 805 reduction planning.
pub enum Plan {
    /// No reduction is needed, or the ontology is structurally ineligible or Horn-only.
    NotNeeded,
    /// Reduction was needed and structurally eligible, but declined by key/memory budget.
    Declined,
    /// Reduction succeeded and produced rewritten axioms.
    Ready(Reduction),
}

/// The axioms of a rewritten ontology.
pub struct Reduction {
    /// the axioms that the rewriting makes, each standing for the original axiom at the
    /// same place of `origin`
    pub axioms: Vec<AxiomKind>,
    pub origin: Vec<usize>,
    /// the original axioms that the rewriting replaces or drops
    pub replaced: Vec<bool>,
    /// the generated hub individual if this reduction introduced `owl:topObjectProperty`
    /// via `⊤ ⊑ ∃U.{hub}`, identifying the generated hub by provenance rather than prefix
    pub hub: Option<NamedIndividual>,
    /// accumulated heap bytes owned inside `self.axioms` elements (strings, `BTreeSet`s,
    /// `Box`ed expressions, and chain vectors), excluding `self.axioms`'s own `Vec` buffer
    axiom_heap_bytes: usize,
}

impl Reduction {
    /// Retained heap bytes of this reduction's containers, AST payloads, and hub IRI.
    pub fn retained_bytes(&self) -> usize {
        self.axioms
            .capacity()
            .saturating_mul(std::mem::size_of::<AxiomKind>())
            .saturating_add(self.axiom_heap_bytes)
            .saturating_add(
                self.origin
                    .capacity()
                    .saturating_mul(std::mem::size_of::<usize>()),
            )
            .saturating_add(self.replaced.capacity())
            .saturating_add(self.hub.as_ref().map_or(0, |h| h.as_str().len()))
    }

    /// The number of axiom references that [`Reduction::all`] will collect.
    pub fn all_len(&self, axioms: &[&AxiomKind]) -> usize {
        let kept = self.replaced.iter().filter(|r| !**r).count();
        debug_assert_eq!(self.replaced.len(), axioms.len());
        kept.saturating_add(self.axioms.len())
    }

    /// The axioms of the rewritten ontology.
    pub fn all<'a>(&'a self, axioms: &[&'a AxiomKind]) -> Vec<&'a AxiomKind> {
        let mut all: Vec<&AxiomKind> = Vec::with_capacity(self.all_len(axioms));
        all.extend(
            axioms
                .iter()
                .zip(&self.replaced)
                .filter(|(_, r)| !**r)
                .map(|(k, _)| *k),
        );
        all.extend(self.axioms.iter());
        all
    }

    /// Puts the original axioms in place of the rewritten ones that the run's report
    /// holds. A rewritten axiom stands for every original axiom that made it, and for
    /// itself as well when the ontology states it unchanged, so one entry of the report
    /// can become several. The lookup holds only the report's axioms, so it costs memory
    /// in proportion to the report and not to the rewriting.
    pub fn restore(&self, axioms: &[&AxiomKind], excluded: &mut Vec<Excluded>) {
        if excluded.is_empty() {
            return;
        }
        // the origins of each axiom of the report, and whether the ontology states it as
        // it is
        let mut origin: FxHashMap<AxiomKind, (Vec<usize>, bool)> = excluded
            .iter()
            .map(|e| (e.axiom.clone(), (Vec::new(), false)))
            .collect();
        for (k, &i) in self.axioms.iter().zip(&self.origin) {
            if let Some((from, _)) = origin.get_mut(k)
                && from.last() != Some(&i)
            {
                from.push(i);
            }
        }
        for (k, r) in axioms.iter().zip(&self.replaced) {
            if !*r && let Some((_, stated)) = origin.get_mut(*k) {
                *stated = true;
            }
        }
        let mut out = Vec::with_capacity(excluded.len());
        // an original axiom whose rewritten axioms are left out for several reasons of
        // one kind is reported once for that kind, by the first of them
        let mut told: FxHashSet<(usize, std::mem::Discriminant<Gap>)> = FxHashSet::default();
        excluded.sort();
        for e in excluded.drain(..) {
            match origin.get(&e.axiom) {
                Some((from, stated)) if !from.is_empty() => {
                    let kind = std::mem::discriminant(&e.gap);
                    out.extend(from.iter().filter(|&&i| told.insert((i, kind))).map(|&i| {
                        Excluded {
                            axiom: axioms[i].clone(),
                            gap: e.gap.clone(),
                        }
                    }));
                    if *stated {
                        out.push(e);
                    }
                }
                _ => out.push(e),
            }
        }
        out.sort();
        out.dedup();
        *excluded = out;
    }

    /// What the original axioms outside OWL 2 EL need, for the run's statistics, with the
    /// report of the axioms the run left out.
    /// `chains` are the original chains outside OWL 2 EL for their ranges alone, which are
    /// Horn, as the run without the rewriting counts them.
    pub fn outside_el(
        &self,
        axioms: &[&AxiomKind],
        chains: &[&AxiomKind],
        excluded: &[Excluded],
    ) -> dl::OutsideEl {
        let mut out = dl::OutsideEl::default();
        let mut seen: BTreeSet<&AxiomKind> = BTreeSet::new();
        for k in axioms {
            if super::logical(k) && super::el_break(k).is_some() && seen.insert(k) {
                out.add(dl::shape(k));
            }
        }
        for k in chains {
            if seen.insert(k) {
                out.horn += 1;
            }
        }
        let left: BTreeSet<&AxiomKind> = excluded
            .iter()
            .filter(|e| matches!(e.gap, Gap::Outside { .. }))
            .map(|e| &e.axiom)
            .filter(|k| seen.contains(k))
            .collect();
        out.taken = seen.len() - left.len();
        out
    }
}

fn is_top(p: &ObjectPropertyExpression) -> bool {
    p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
}

fn u() -> ObjectProperty {
    ObjectProperty::new(format!("{PREFIX}u"))
}

fn hub() -> Individual {
    Individual::named(format!("{PREFIX}hub"))
}

fn ce_has_top(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().any(ce_has_top),
        C::ObjectComplementOf(f) => ce_has_top(f),
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => is_top(p) || ce_has_top(f),
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => is_top(p),
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => is_top(p) || f.as_deref().is_some_and(ce_has_top),
        _ => false,
    }
}

fn ax_has_top(k: &AxiomKind) -> bool {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => ce_has_top(a) || ce_has_top(b),
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            cs.iter().any(ce_has_top)
        }
        K::ClassAssertion(c, _) => ce_has_top(c),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => is_top(p) || ce_has_top(c),
        K::SubObjectPropertyOf(s, p) => {
            is_top(p)
                || match s {
                    SubObjectPropertyExpression::Expression(q) => is_top(q),
                    SubObjectPropertyExpression::Chain(qs) => qs.iter().any(is_top),
                }
        }
        K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
            ps.iter().any(is_top)
        }
        K::InverseObjectProperties(p, q) => is_top(p) || is_top(q),
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => is_top(p),
        K::ObjectPropertyAssertion(p, ..) | K::NegativeObjectPropertyAssertion(p, ..) => is_top(p),
        K::HasKey(c, ops, _) => ce_has_top(c) || ops.iter().any(is_top),
        _ => false,
    }
}

/// What the rewriting makes of one axiom.
enum Rt {
    Drop,
    New(Vec<AxiomKind>),
}

/// The state of a rewriting: whether the fresh property `U` is in use.
#[derive(Default)]
struct Cx {
    used: bool,
}

impl Cx {
    fn ope(&mut self, p: &ObjectPropertyExpression) -> ObjectPropertyExpression {
        if !is_top(p) {
            return p.clone();
        }
        self.used = true;
        match p {
            ObjectPropertyExpression::Property(_) => ObjectPropertyExpression::Property(u()),
            ObjectPropertyExpression::InverseOf(_) => ObjectPropertyExpression::InverseOf(u()),
        }
    }

    fn ces(&mut self, cs: &BTreeSet<ClassExpression>) -> Result<BTreeSet<ClassExpression>, ()> {
        cs.iter().map(|c| self.ce(c)).collect()
    }

    /// The images of the members of a disjointness, and an axiom `D ⊑ ⊥` for each image
    /// `D` of more than one member. Two members with one image are equivalent, so their
    /// disjointness makes the image empty, which the set of images alone would lose.
    fn disjoint(
        &mut self,
        cs: &BTreeSet<ClassExpression>,
    ) -> Result<(BTreeSet<ClassExpression>, Vec<AxiomKind>), ()> {
        let mut images = BTreeSet::new();
        let mut empty = BTreeSet::new();
        for c in cs {
            let d = self.ce(c)?;
            if images.contains(&d) {
                empty.insert(d);
            } else {
                images.insert(d);
            }
        }
        let empty = empty
            .into_iter()
            .map(|d| AxiomKind::SubClassOf(d, ClassExpression::Class(Class::nothing())))
            .collect();
        Ok((images, empty))
    }

    fn ce(&mut self, c: &ClassExpression) -> Result<ClassExpression, ()> {
        use ClassExpression as C;
        let filler = |s: &mut Cx, f: &Option<Box<ClassExpression>>| match f {
            Some(f) => s.ce(f).map(|f| Some(Box::new(f))),
            None => Ok(None),
        };
        Ok(match c {
            C::ObjectIntersectionOf(cs) => C::ObjectIntersectionOf(self.ces(cs)?),
            C::ObjectUnionOf(cs) => C::ObjectUnionOf(self.ces(cs)?),
            C::ObjectComplementOf(f) => C::ObjectComplementOf(Box::new(self.ce(f)?)),
            C::ObjectSomeValuesFrom(p, f) => {
                C::ObjectSomeValuesFrom(self.ope(p), Box::new(self.ce(f)?))
            }
            C::ObjectAllValuesFrom(p, f) => {
                C::ObjectAllValuesFrom(self.ope(p), Box::new(self.ce(f)?))
            }
            C::ObjectHasValue(p, i) => C::ObjectHasValue(self.ope(p), i.clone()),
            C::ObjectHasSelf(p) if is_top(p) => return Err(()),
            // at least none is every element, and at least one is an existential
            C::ObjectMinCardinality(0, p, _) if is_top(p) => C::Class(Class::thing()),
            C::ObjectMinCardinality(1, p, f) if is_top(p) => C::ObjectSomeValuesFrom(
                self.ope(p),
                Box::new(match f {
                    Some(f) => self.ce(f)?,
                    None => C::Class(Class::thing()),
                }),
            ),
            C::ObjectMinCardinality(_, p, _) if is_top(p) => return Err(()),
            C::ObjectMinCardinality(n, p, f) => {
                C::ObjectMinCardinality(*n, p.clone(), filler(self, f)?)
            }
            C::ObjectMaxCardinality(_, p, _) | C::ObjectExactCardinality(_, p, _) if is_top(p) => {
                return Err(());
            }
            C::ObjectMaxCardinality(n, p, f) => {
                C::ObjectMaxCardinality(*n, p.clone(), filler(self, f)?)
            }
            C::ObjectExactCardinality(n, p, f) => {
                C::ObjectExactCardinality(*n, p.clone(), filler(self, f)?)
            }
            _ => c.clone(),
        })
    }

    fn retarget(&mut self, k: &AxiomKind) -> Result<Rt, ()> {
        use AxiomKind as K;
        let sub = |p: ObjectPropertyExpression, q: &ObjectPropertyExpression| {
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(p), q.clone())
        };
        Ok(Rt::New(vec![match k {
            K::SubClassOf(a, b) => K::SubClassOf(self.ce(a)?, self.ce(b)?),
            K::EquivalentClasses(cs) => K::EquivalentClasses(self.ces(cs)?),
            K::DisjointClasses(cs) => {
                let (images, mut out) = self.disjoint(cs)?;
                if images.len() >= 2 {
                    out.push(K::DisjointClasses(images));
                }
                return Ok(Rt::New(out));
            }
            // the union keeps the set of images, and the disjointness of its members
            // follows the rule of `DisjointClasses`
            K::DisjointUnion(c, cs) => {
                let (images, mut out) = self.disjoint(cs)?;
                if images.len() >= 2 {
                    out.push(K::DisjointUnion(c.clone(), images));
                } else if let Some(d) = images.into_iter().next() {
                    let c = ClassExpression::Class(c.clone());
                    if c != d {
                        out.push(K::EquivalentClasses([c, d].into()));
                    }
                }
                return Ok(Rt::New(out));
            }
            K::ClassAssertion(c, i) => K::ClassAssertion(self.ce(c)?, i.clone()),
            // every element is in the domain and in the range of the universal property
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) if is_top(p) => {
                K::SubClassOf(ClassExpression::Class(Class::thing()), self.ce(c)?)
            }
            K::ObjectPropertyDomain(p, c) => K::ObjectPropertyDomain(p.clone(), self.ce(c)?),
            K::ObjectPropertyRange(p, c) => K::ObjectPropertyRange(p.clone(), self.ce(c)?),
            K::SubObjectPropertyOf(_, sup) if is_top(sup) => return Ok(Rt::Drop),
            K::SubObjectPropertyOf(s, sup) => K::SubObjectPropertyOf(
                match s {
                    SubObjectPropertyExpression::Expression(p) => {
                        SubObjectPropertyExpression::Expression(self.ope(p))
                    }
                    SubObjectPropertyExpression::Chain(ps) => {
                        SubObjectPropertyExpression::Chain(ps.iter().map(|p| self.ope(p)).collect())
                    }
                },
                sup.clone(),
            ),
            // the properties equivalent to the universal property are below it
            K::EquivalentObjectProperties(ps) => {
                let rest: Vec<&ObjectPropertyExpression> =
                    ps.iter().filter(|p| !is_top(p)).collect();
                if rest.is_empty() {
                    return Ok(Rt::Drop);
                }
                return Ok(Rt::New(
                    rest.into_iter()
                        .map(|p| {
                            let top = self
                                .ope(&ObjectPropertyExpression::Property(ObjectProperty::top()));
                            sub(top, p)
                        })
                        .collect(),
                ));
            }
            K::InverseObjectProperties(p, q) => match (is_top(p), is_top(q)) {
                (true, true) => return Ok(Rt::Drop),
                (true, false) | (false, true) => {
                    let other = if is_top(p) { q } else { p };
                    let top = self.ope(&ObjectPropertyExpression::Property(ObjectProperty::top()));
                    sub(top, other)
                }
                _ => return Err(()),
            },
            K::SymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
                if is_top(p) =>
            {
                return Ok(Rt::Drop);
            }
            K::ObjectPropertyAssertion(p, ..) if is_top(p) => return Ok(Rt::Drop),
            K::NegativeObjectPropertyAssertion(p, a, b) => {
                K::NegativeObjectPropertyAssertion(self.ope(p), a.clone(), b.clone())
            }
            _ => return Err(()),
        }]))
    }
}

#[inline]
fn dec_len(mut n: usize) -> usize {
    let mut d = 1usize;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Safe upper bound on heap bytes owned by a `BTreeSet<T>` of `len` elements.
#[inline]
fn btree_set_bytes<T>(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let elem = std::mem::size_of::<T>();
    let leaf = 32usize.saturating_add(11usize.saturating_mul(elem));
    if len <= 11 {
        return leaf;
    }
    let leaves = len.div_ceil(5);
    let internals = leaves.div_ceil(5);
    leaves.saturating_mul(leaf).saturating_add(
        internals.saturating_mul(128usize.saturating_add(11usize.saturating_mul(elem))),
    )
}

/// Safe upper bound on heap bytes owned by a `BTreeMap<K, V>` of `len` entries.
#[inline]
fn btree_map_bytes<K, V>(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let entry = std::mem::size_of::<K>().saturating_add(std::mem::size_of::<V>());
    let leaf = 32usize.saturating_add(11usize.saturating_mul(entry));
    if len <= 11 {
        return leaf;
    }
    let leaves = len.div_ceil(5);
    let internals = leaves.div_ceil(5);
    leaves.saturating_mul(leaf).saturating_add(
        internals.saturating_mul(128usize.saturating_add(11usize.saturating_mul(entry))),
    )
}

#[inline]
fn ind_heap_bytes(i: &Individual) -> usize {
    match i {
        Individual::Named(n) => n.as_str().len(),
        Individual::Anonymous(a) => a.label().len(),
    }
}

fn ce_heap_bytes(c: &ClassExpression) -> usize {
    use ClassExpression as C;
    match c {
        C::Class(cl) => cl.as_str().len(),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            btree_set_bytes::<ClassExpression>(cs.len())
                .saturating_add(cs.iter().map(ce_heap_bytes).sum::<usize>())
        }
        C::ObjectComplementOf(f) => {
            std::mem::size_of::<ClassExpression>().saturating_add(ce_heap_bytes(f))
        }
        C::ObjectOneOf(is) => btree_set_bytes::<Individual>(is.len())
            .saturating_add(is.iter().map(ind_heap_bytes).sum::<usize>()),
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => p
            .named()
            .as_str()
            .len()
            .saturating_add(std::mem::size_of::<ClassExpression>())
            .saturating_add(ce_heap_bytes(f)),
        C::ObjectHasValue(p, i) => p.named().as_str().len().saturating_add(ind_heap_bytes(i)),
        C::ObjectHasSelf(p) => p.named().as_str().len(),
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => {
            p.named()
                .as_str()
                .len()
                .saturating_add(f.as_deref().map_or(0, |inner| {
                    std::mem::size_of::<ClassExpression>().saturating_add(ce_heap_bytes(inner))
                }))
        }
        C::DataSomeValuesFrom(ps, _) | C::DataAllValuesFrom(ps, _) => ps
            .capacity()
            .saturating_mul(std::mem::size_of::<DataProperty>())
            .saturating_add(ps.iter().map(|p| p.as_str().len()).sum::<usize>()),
        C::DataHasValue(p, _)
        | C::DataMinCardinality(_, p, _)
        | C::DataMaxCardinality(_, p, _)
        | C::DataExactCardinality(_, p, _) => p.as_str().len(),
    }
}

fn axiom_heap_bytes(k: &AxiomKind) -> usize {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => ce_heap_bytes(a).saturating_add(ce_heap_bytes(b)),
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) => {
            btree_set_bytes::<ClassExpression>(cs.len())
                .saturating_add(cs.iter().map(ce_heap_bytes).sum::<usize>())
        }
        K::DisjointUnion(c, cs) => c
            .as_str()
            .len()
            .saturating_add(btree_set_bytes::<ClassExpression>(cs.len()))
            .saturating_add(cs.iter().map(ce_heap_bytes).sum::<usize>()),
        K::ClassAssertion(c, i) => ce_heap_bytes(c).saturating_add(ind_heap_bytes(i)),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            p.named().as_str().len().saturating_add(ce_heap_bytes(c))
        }
        K::SubObjectPropertyOf(s, sup) => sup.named().as_str().len().saturating_add(match s {
            SubObjectPropertyExpression::Expression(p) => p.named().as_str().len(),
            SubObjectPropertyExpression::Chain(ps) => ps
                .capacity()
                .saturating_mul(std::mem::size_of::<ObjectPropertyExpression>())
                .saturating_add(ps.iter().map(|p| p.named().as_str().len()).sum::<usize>()),
        }),
        K::SymmetricObjectProperty(p) | K::TransitiveObjectProperty(p) => p.named().as_str().len(),
        K::NegativeObjectPropertyAssertion(p, a, b) => p
            .named()
            .as_str()
            .len()
            .saturating_add(ind_heap_bytes(a))
            .saturating_add(ind_heap_bytes(b)),
        K::Declaration(e) => e.iri().as_str().len(),
        _ => 0,
    }
}

struct PlanBudget<'a, F: FnMut(usize) -> bool> {
    live_base: usize,
    held: usize,
    admit: &'a mut F,
}

impl<F: FnMut(usize) -> bool> PlanBudget<'_, F> {
    fn try_temp(&mut self, extra: usize) -> bool {
        let Some(peak) = self
            .live_base
            .checked_add(self.held)
            .and_then(|b| b.checked_add(extra))
        else {
            return false;
        };
        (self.admit)(peak)
    }

    fn try_add(&mut self, bytes: usize) -> bool {
        let Some(next) = self.held.checked_add(bytes) else {
            return false;
        };
        let Some(peak) = self.live_base.checked_add(next) else {
            return false;
        };
        if !(self.admit)(peak) {
            return false;
        }
        self.held = next;
        true
    }

    fn release(&mut self, bytes: usize) {
        self.held = self.held.saturating_sub(bytes);
    }
}

fn push_axiom<F: FnMut(usize) -> bool>(
    out: &mut Reduction,
    budget: &mut PlanBudget<'_, F>,
    origin_i: usize,
    ax_heap: usize,
    make: impl FnOnce() -> AxiomKind,
) -> bool {
    let ax_grow = if out.axioms.len() == out.axioms.capacity() {
        let Some(new_cap) =
            super::dl_clause::next_vec_cap(out.axioms.capacity(), std::mem::size_of::<AxiomKind>())
        else {
            return false;
        };
        let Some(bytes) = new_cap.checked_mul(std::mem::size_of::<AxiomKind>()) else {
            return false;
        };
        bytes
    } else {
        0
    };
    let orig_grow = if out.origin.len() == out.origin.capacity() {
        let Some(new_cap) =
            super::dl_clause::next_vec_cap(out.origin.capacity(), std::mem::size_of::<usize>())
        else {
            return false;
        };
        let Some(bytes) = new_cap.checked_mul(std::mem::size_of::<usize>()) else {
            return false;
        };
        bytes
    } else {
        0
    };
    let Some(peak_add) = ax_heap
        .checked_add(ax_grow)
        .and_then(|b| b.checked_add(orig_grow))
    else {
        return false;
    };
    if !budget.try_temp(peak_add) {
        return false;
    }
    let old_ax_cap = out.axioms.capacity();
    let old_orig_cap = out.origin.capacity();
    out.axioms.push(make());
    out.origin.push(origin_i);
    let ax_delta = out
        .axioms
        .capacity()
        .saturating_sub(old_ax_cap)
        .saturating_mul(std::mem::size_of::<AxiomKind>());
    let orig_delta = out
        .origin
        .capacity()
        .saturating_sub(old_orig_cap)
        .saturating_mul(std::mem::size_of::<usize>());
    budget.held = budget
        .held
        .saturating_add(ax_heap)
        .saturating_add(ax_delta)
        .saturating_add(orig_delta);
    out.axiom_heap_bytes = out.axiom_heap_bytes.saturating_add(ax_heap);
    true
}

/// Outcome of scanning one axiom during `plan_budgeted`.
enum Blocked {
    Ok,
    Ineligible,
    BudgetDeclined,
}

/// Whether the axioms hold something that keeps a rooted run of Tier D from them whatever
/// the rewriting does, data, the bottom property or a reflexive property, or a name with
/// the prefix of the made-up names, admitting each newly added canonical individual into
/// `inds` against `budget`.
fn blocked<F: FnMut(usize) -> bool>(
    k: &AxiomKind,
    inds: &mut BTreeSet<NamedIndividual>,
    canon: Option<&super::index::CanonMap>,
    budget: &mut PlanBudget<'_, F>,
) -> Blocked {
    let mut s = Signature::default();
    s.axiom(k);
    let mut bad = matches!(k, AxiomKind::ReflexiveObjectProperty(p) if !is_top(p));
    for e in &s.0 {
        // a name of the ontology's own that looks like a made-up one would meet them
        if e.iri().as_str().starts_with(PREFIX) {
            bad = true;
        }
        match e {
            Entity::DataProperty(_) | Entity::Datatype(_) => bad = true,
            Entity::ObjectProperty(p) if p.as_str() == vocab::OWL_BOTTOM_OBJECT_PROPERTY => {
                bad = true
            }
            Entity::NamedIndividual(i) if !bad => {
                let rep = canon.and_then(|m| m.get(i)).unwrap_or(i);
                if !inds.contains(rep) {
                    let old_tree = btree_set_bytes::<NamedIndividual>(inds.len());
                    let new_tree = btree_set_bytes::<NamedIndividual>(inds.len() + 1);
                    let add = rep
                        .as_str()
                        .len()
                        .saturating_add(new_tree.saturating_sub(old_tree));
                    if !budget.try_add(add) {
                        return Blocked::BudgetDeclined;
                    }
                    inds.insert(super::index::exact_individual(rep));
                }
            }
            _ => {}
        }
    }
    if bad {
        Blocked::Ineligible
    } else {
        Blocked::Ok
    }
}

/// The rewriting of the axioms for a rooted run, or `None` when no axiom needs it, when
/// the ontology has no axiom that needs case analysis, which the Horn rules take, when
/// something keeps Tier D from it anyway, or when an axiom has no reduction or the keys'
/// reductions pass the bound. `memory` is the run's memory budget. When `canon` is given,
/// key subjects and object-key values range over the distinct canonical images of the
/// original named signature.
#[cfg_attr(not(test), allow(dead_code))]
pub fn plan(
    axioms: &[&AxiomKind],
    memory: Option<usize>,
    canon: Option<&super::index::CanonMap>,
) -> Option<Reduction> {
    match plan_budgeted(
        axioms,
        memory,
        canon,
        0,
        &mut || Ok::<(), ()>(()),
        &mut |_| true,
    ) {
        Ok(Plan::Ready(red)) => Some(red),
        _ => None,
    }
}

/// Plans item 805 reductions with cooperative polling and exact memory pre-admission
/// against `live_base` (including any live merge partition and canonical map).
pub fn plan_budgeted<E>(
    axioms: &[&AxiomKind],
    memory: Option<usize>,
    canon: Option<&super::index::CanonMap>,
    live_base: usize,
    poll: &mut impl FnMut() -> Result<(), E>,
    admit: &mut impl FnMut(usize) -> bool,
) -> Result<Plan, E> {
    // an inclusion in the top property alone needs no rewriting, since the run takes it
    // as an axiom that adds nothing (roadmap item 1282)
    let wants = |k: &AxiomKind| {
        ax_has_top(k) && !super::index::top_inclusion(k)
            || matches!(k, AxiomKind::HasKey(_, ops, dps) if !ops.is_empty() && dps.is_empty())
    };
    let mut needed = false;
    for (i, k) in axioms.iter().enumerate() {
        if i.is_multiple_of(super::POLL_AXIOMS) {
            poll()?;
        }
        if wants(k) {
            needed = true;
            break;
        }
    }
    if !needed {
        return Ok(Plan::NotNeeded);
    }
    let mut budget = PlanBudget {
        live_base,
        held: 0,
        admit,
    };
    let mut inds = BTreeSet::new();
    for (i, k) in axioms.iter().enumerate() {
        if i.is_multiple_of(super::POLL_AXIOMS) {
            poll()?;
        }
        // an inclusion in a top property is dropped below, and its subproperty, such as
        // a data property, keeps nothing from the rewriting
        if super::index::top_inclusion(k) {
            continue;
        }
        match blocked(k, &mut inds, canon, &mut budget) {
            Blocked::Ok => {}
            Blocked::Ineligible => return Ok(Plan::NotNeeded),
            Blocked::BudgetDeclined => return Ok(Plan::Declined),
        }
        if matches!(k, AxiomKind::HasKey(_, _, dps) if !dps.is_empty()) {
            return Ok(Plan::NotNeeded);
        }
    }
    let mut has_disj = false;
    for (i, k) in axioms.iter().enumerate() {
        if i.is_multiple_of(super::POLL_AXIOMS) {
            poll()?;
        }
        if !dl::shape(k).horn {
            has_disj = true;
            break;
        }
    }
    if !has_disj {
        return Ok(Plan::NotNeeded);
    }
    let inds_tree_bytes = btree_set_bytes::<NamedIndividual>(inds.len());
    let Some(inds_vec_bytes) = inds
        .len()
        .checked_mul(std::mem::size_of::<NamedIndividual>())
    else {
        return Ok(Plan::Declined);
    };
    if !budget.try_add(inds_vec_bytes) {
        return Ok(Plan::Declined);
    }
    let inds: Vec<NamedIndividual> = inds.into_iter().collect();
    budget.release(inds_tree_bytes);

    let n = inds.len();
    let pairs = n * n.saturating_sub(1) / 2;
    let limit = memory.map_or(KEY_AXIOMS, |m| (m / AXIOM_BYTES).min(KEY_AXIOMS));
    if !budget.try_add(axioms.len()) {
        return Ok(Plan::Declined);
    }
    let mut cx = Cx::default();
    let mut out = Reduction {
        axioms: Vec::new(),
        origin: Vec::new(),
        replaced: vec![false; axioms.len()],
        hub: None,
        axiom_heap_bytes: 0,
    };
    let mut first_use = None;
    let mut keys = 0usize;
    let mut total = 0usize;
    for (i, &k) in axioms.iter().enumerate() {
        if i.is_multiple_of(super::POLL_AXIOMS) {
            poll()?;
        }
        if let AxiomKind::HasKey(c, ops, _) = k
            && !ops.is_empty()
        {
            let m = ops.len();
            total = total.saturating_add(m.saturating_mul(n).saturating_mul(pairs + 1) + pairs);
            if total > limit {
                return Ok(Plan::Declined);
            }
            let before = cx.used;
            let Ok(c) = cx.ce(c) else {
                return Ok(Plan::NotNeeded);
            };
            let rs: Vec<ObjectPropertyExpression> = ops.iter().map(|p| cx.ope(p)).collect();
            if cx.used && !before {
                first_use.get_or_insert(i);
            }
            // a class expression other than a name is named once, so that the many
            // axioms of the key hold a name and not a copy of the expression
            let c = match c {
                ClassExpression::Class(_) => c,
                c => {
                    let name_len = PREFIX.len() + 1 + dec_len(keys);
                    let eq_heap = btree_set_bytes::<ClassExpression>(2)
                        .saturating_add(name_len)
                        .saturating_add(ce_heap_bytes(&c));
                    let name = ClassExpression::Class(Class::new(format!("{PREFIX}k{keys}")));
                    if !push_axiom(&mut out, &mut budget, i, eq_heap, || {
                        AxiomKind::EquivalentClasses([name.clone(), c].into())
                    }) {
                        return Ok(Plan::Declined);
                    }
                    name
                }
            };
            if !key_budgeted(keys, &c, &rs, &inds, i, &mut out, &mut budget, poll)? {
                return Ok(Plan::Declined);
            }
            keys += 1;
            out.replaced[i] = true;
            continue;
        }
        if !ax_has_top(k) {
            continue;
        }
        if !budget.try_temp(axiom_heap_bytes(k)) {
            return Ok(Plan::Declined);
        }
        let before = cx.used;
        let Ok(rt) = cx.retarget(k) else {
            return Ok(Plan::NotNeeded);
        };
        match rt {
            Rt::Drop => {}
            Rt::New(v) => {
                for ax in v {
                    let ax_heap = axiom_heap_bytes(&ax);
                    if !push_axiom(&mut out, &mut budget, i, ax_heap, || ax) {
                        return Ok(Plan::Declined);
                    }
                }
            }
        }
        if cx.used && !before {
            first_use.get_or_insert(i);
        }
        out.replaced[i] = true;
    }
    if let Some(i) = first_use {
        let hub_str = format!("{PREFIX}hub");
        if !budget.try_add(hub_str.len()) {
            return Ok(Plan::Declined);
        }
        out.hub = Some(NamedIndividual::new(hub_str));
        let up = ObjectPropertyExpression::Property(u());
        let thing = ClassExpression::Class(Class::thing());
        for ax in [
            AxiomKind::SubClassOf(
                thing,
                ClassExpression::ObjectSomeValuesFrom(
                    up.clone(),
                    Box::new(ClassExpression::ObjectOneOf([hub()].into())),
                ),
            ),
            AxiomKind::SymmetricObjectProperty(up.clone()),
            AxiomKind::TransitiveObjectProperty(up),
        ] {
            let ax_heap = axiom_heap_bytes(&ax);
            if !push_axiom(&mut out, &mut budget, i, ax_heap, || ax) {
                return Ok(Plan::Declined);
            }
        }
    }
    // the classes and individuals of the replaced axioms stay in the signature, though the
    // rewriting may drop them
    let mut sig_at: BTreeMap<Entity, usize> = BTreeMap::new();
    let mut sig_str_bytes = 0usize;
    for (i, k) in axioms.iter().enumerate() {
        if i.is_multiple_of(super::POLL_AXIOMS) {
            poll()?;
        }
        if out.replaced[i] {
            let mut one = Signature::default();
            one.axiom(k);
            for e in one.0 {
                if !matches!(e, Entity::Class(_) | Entity::NamedIndividual(_)) {
                    continue;
                }
                if !sig_at.contains_key(&e) {
                    let old_tree = btree_map_bytes::<Entity, usize>(sig_at.len());
                    let new_tree = btree_map_bytes::<Entity, usize>(sig_at.len() + 1);
                    let str_len = e.iri().as_str().len();
                    let add = str_len.saturating_add(new_tree.saturating_sub(old_tree));
                    if !budget.try_add(add) {
                        return Ok(Plan::Declined);
                    }
                    sig_str_bytes = sig_str_bytes.saturating_add(str_len);
                    sig_at.insert(e, i);
                }
            }
        }
    }
    let sig_tree_bytes = btree_map_bytes::<Entity, usize>(sig_at.len());
    for (step, (e, origin_i)) in sig_at.into_iter().enumerate() {
        if step.is_multiple_of(super::POLL_AXIOMS) {
            poll()?;
        }
        let (ax, ax_heap) = match e {
            Entity::Class(c) => {
                let h = c.as_str().len();
                (AxiomKind::Declaration(Entity::Class(c)), h)
            }
            Entity::NamedIndividual(n) => {
                let h = vocab::OWL_THING.len().saturating_add(n.as_str().len());
                (
                    AxiomKind::ClassAssertion(
                        ClassExpression::Class(Class::thing()),
                        Individual::Named(n),
                    ),
                    h,
                )
            }
            _ => continue,
        };
        if !push_axiom(&mut out, &mut budget, origin_i, ax_heap, || ax) {
            return Ok(Plan::Declined);
        }
    }
    budget.release(sig_tree_bytes.saturating_add(sig_str_bytes));
    if out.replaced.iter().any(|r| *r) {
        Ok(Plan::Ready(out))
    } else {
        Ok(Plan::NotNeeded)
    }
}

/// Emits the class axioms that stand for the key `HasKey(c (rs) ())`, number `k`, over the
/// named individuals `inds`, admitting every axiom against `budget` and polling `poll`
/// cooperatively. Returns `Ok(false)` when `budget` declines.
#[allow(clippy::too_many_arguments)]
fn key_budgeted<E, F: FnMut(usize) -> bool>(
    k: usize,
    c: &ClassExpression,
    rs: &[ObjectPropertyExpression],
    inds: &[NamedIndividual],
    origin_i: usize,
    out: &mut Reduction,
    budget: &mut PlanBudget<'_, F>,
    poll: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    use ClassExpression as C;
    if inds.len() < 2 {
        return Ok(true);
    }
    let one = |i: &NamedIndividual| C::ObjectOneOf([Individual::Named(i.clone())].into());
    let one_node = btree_set_bytes::<Individual>(1);
    let inter2_node = btree_set_bytes::<ClassExpression>(2);
    let box_ce = std::mem::size_of::<ClassExpression>();
    let c_heap = ce_heap_bytes(c);
    let v_prefix = PREFIX.len() + 1 + dec_len(k) + 1;
    let m_prefix = PREFIX.len() + 1 + dec_len(k) + 1;
    // V_jb holds the r_j-values of b when b is a C
    let v = |j: usize, b: usize| Class::new(format!("{PREFIX}v{k}-{j}-{b}"));
    let v_len = |j: usize, b: usize| v_prefix + dec_len(j) + 1 + dec_len(b);
    let m_cls = |j: usize, a: usize, b: usize| Class::new(format!("{PREFIX}m{k}-{j}-{a}-{b}"));
    let m_len =
        |j: usize, a: usize, b: usize| m_prefix + dec_len(j) + 1 + dec_len(a) + 1 + dec_len(b);
    let mut steps = 0usize;
    for (b, ib) in inds.iter().enumerate().skip(1) {
        let ib_len = ib.as_str().len();
        for (j, r) in rs.iter().enumerate() {
            if steps.is_multiple_of(POLL_KEY) {
                poll()?;
            }
            steps += 1;
            let ax_heap = inter2_node
                .saturating_add(one_node)
                .saturating_add(ib_len)
                .saturating_add(c_heap)
                .saturating_add(r.named().as_str().len())
                .saturating_add(box_ce)
                .saturating_add(v_len(j, b));
            if !push_axiom(out, budget, origin_i, ax_heap, || {
                AxiomKind::SubClassOf(
                    C::ObjectIntersectionOf([one(ib), c.clone()].into()),
                    C::ObjectAllValuesFrom(r.clone(), Box::new(C::Class(v(j, b)))),
                )
            }) {
                return Ok(false);
            }
        }
    }
    let body_node = btree_set_bytes::<ClassExpression>(2usize.saturating_add(rs.len()));
    for (a, ia) in inds.iter().enumerate() {
        let ia_len = ia.as_str().len();
        for (b, ib) in inds.iter().enumerate().skip(a + 1) {
            let mut m_total_len = 0usize;
            for (j, r) in rs.iter().enumerate() {
                let r_len = r.named().as_str().len();
                let vj_len = v_len(j, b);
                let mj_len = m_len(j, a, b);
                m_total_len = m_total_len.saturating_add(mj_len);
                // M_jab: a has a named r_j-value that b, a C, has too
                let m = m_cls(j, a, b);
                let vj = v(j, b);
                for ic in inds {
                    if steps.is_multiple_of(POLL_KEY) {
                        poll()?;
                    }
                    steps += 1;
                    let ax_heap = inter2_node
                        .saturating_add(one_node)
                        .saturating_add(ia_len)
                        .saturating_add(r_len)
                        .saturating_add(box_ce)
                        .saturating_add(inter2_node)
                        .saturating_add(one_node)
                        .saturating_add(ic.as_str().len())
                        .saturating_add(vj_len)
                        .saturating_add(mj_len);
                    if !push_axiom(out, budget, origin_i, ax_heap, || {
                        AxiomKind::SubClassOf(
                            C::ObjectIntersectionOf(
                                [
                                    one(ia),
                                    C::ObjectSomeValuesFrom(
                                        r.clone(),
                                        Box::new(C::ObjectIntersectionOf(
                                            [one(ic), C::Class(vj.clone())].into(),
                                        )),
                                    ),
                                ]
                                .into(),
                            ),
                            C::Class(m.clone()),
                        )
                    }) {
                        return Ok(false);
                    }
                }
            }
            if steps.is_multiple_of(POLL_KEY) {
                poll()?;
            }
            steps += 1;
            let final_heap = body_node
                .saturating_add(one_node)
                .saturating_add(ia_len)
                .saturating_add(c_heap)
                .saturating_add(m_total_len)
                .saturating_add(one_node)
                .saturating_add(ib.as_str().len());
            if !push_axiom(out, budget, origin_i, final_heap, || {
                let mut body = BTreeSet::new();
                body.insert(one(ia));
                body.insert(c.clone());
                for j in 0..rs.len() {
                    body.insert(C::Class(m_cls(j, a, b)));
                }
                AxiomKind::SubClassOf(C::ObjectIntersectionOf(body), one(ib))
            }) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
