//! The syntactic bottom-locality module of a signature (Cuenca Grau, Horrocks, Kazakov
//! and Sattler, JAIR 2008), which `reason --dump-unsatisfiable` writes for the
//! unsatisfiable classes, as ROBOT writes the `BOT` module that its `extract` computes.
//! The module holds every justification of each entailment over the signature.

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
    let mut sig: FxHashSet<String> = seed.into_iter().map(|c| c.as_str().to_string()).collect();
    // the logical axioms by each IRI of their signature
    let mut by_entity: FxHashMap<String, Vec<usize>> = FxHashMap::default();
    let mut signatures: Vec<Vec<String>> = vec![Vec::new(); axioms.len()];
    for (i, a) in axioms.iter().enumerate() {
        if !crate::classify::logical(&a.kind) || matches!(a.kind, AxiomKind::Declaration(_)) {
            continue;
        }
        let mut s = Signature::default();
        s.axiom(&a.kind);
        for e in s.0 {
            let iri = e.iri().as_str().to_string();
            by_entity.entry(iri.clone()).or_default().push(i);
            signatures[i].push(iri);
        }
    }
    let mut in_module = vec![false; axioms.len()];
    let mut queue: Vec<String> = sig.iter().cloned().collect();
    // axioms with an empty signature are never local to a non-empty one
    for (i, s) in signatures.iter().enumerate() {
        if s.is_empty()
            && crate::classify::logical(&axioms[i].kind)
            && !matches!(axioms[i].kind, AxiomKind::Declaration(_))
            && !local(&axioms[i].kind, &sig)
        {
            in_module[i] = true;
        }
    }
    while let Some(iri) = queue.pop() {
        let Some(list) = by_entity.get(&iri) else {
            continue;
        };
        for &i in list {
            if in_module[i] || local(&axioms[i].kind, &sig) {
                continue;
            }
            in_module[i] = true;
            for e in &signatures[i] {
                if sig.insert(e.clone()) {
                    queue.push(e.clone());
                }
            }
        }
    }
    axioms
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            in_module[*i]
                || matches!(&a.kind, AxiomKind::Declaration(e) if sig.contains(e.iri().as_str()))
        })
        .map(|(_, a)| (*a).clone())
        .collect()
}

struct Sig<'s>(&'s FxHashSet<String>);

impl Sig<'_> {
    fn has(&self, iri: &str) -> bool {
        self.0.contains(iri)
    }

    fn outside_ope(&self, p: &ObjectPropertyExpression) -> bool {
        let n = p.named().as_str();
        n != vocab::OWL_TOP_OBJECT_PROPERTY && !self.has(n)
    }

    fn outside_dp(&self, p: &DataProperty) -> bool {
        p.as_str() != vocab::OWL_TOP_DATA_PROPERTY && !self.has(p.as_str())
    }

    /// Whether the expression is empty once the entities outside the signature are.
    fn bottom(&self, c: &ClassExpression) -> bool {
        use ClassExpression as C;
        match c {
            C::Class(c) => {
                c.as_str() == vocab::OWL_NOTHING
                    || c.as_str() != vocab::OWL_THING && !self.has(c.as_str())
            }
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

    /// Whether the expression is everything once the entities outside the signature are
    /// empty.
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
}

/// Whether the axiom is bottom-local for the signature: a tautology once every entity
/// outside it is empty.
fn local(k: &AxiomKind, sig: &FxHashSet<String>) -> bool {
    use AxiomKind as K;
    let s = Sig(sig);
    let sub_ope = |p: &SubObjectPropertyExpression| match p {
        SubObjectPropertyExpression::Expression(p) => s.outside_ope(p),
        SubObjectPropertyExpression::Chain(ps) => ps.iter().any(|p| s.outside_ope(p)),
    };
    match k {
        K::SubClassOf(a, b) => s.bottom(a) || s.top(b),
        K::EquivalentClasses(cs) => cs.iter().all(|c| s.bottom(c)) || cs.iter().all(|c| s.top(c)),
        K::DisjointClasses(cs) => cs.iter().filter(|c| !s.bottom(c)).count() <= 1,
        K::DisjointUnion(c, cs) => !s.has(c.as_str()) && cs.iter().all(|c| s.bottom(c)),
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
        K::HasKey(..) | K::Declaration(_) => true,
        K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => true,
        _ => false,
    }
}
