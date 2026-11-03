//! `materialize`: the ontology with its direct superclasses of the form
//! `ObjectSomeValuesFrom(p B)` asserted for chosen properties, as ROBOT 1.9.11's
//! `MaterializeOperation` asserts them with the expression materializing reasoner over
//! ELK.
//!
//! The classifier derives an existential superclass of a class in its saturation, as a
//! link to a context whose subsumers hold the filler, but answers with named classes
//! only. ROBOT asserts the existential superclasses that are direct in a taxonomy with a
//! node for every `ObjectSomeValuesFrom(p B)`, and whether one is direct depends on what
//! that expression is itself subsumed by. So each expression is named by a fresh class
//! equivalent to it, as the expression materializing reasoner names them, and one
//! classification answers both: the taxonomy is the one ROBOT reads, with each fresh
//! class read back as its expression.

use super::reason::Probes;
use super::{BOTTOM, Classify, Document, Error, Report, TOP, fresh_class};
use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

/// The options of `materialize`, with ROBOT's defaults.
#[derive(Clone, Debug, Default)]
pub struct MaterializeOptions {
    /// the properties whose existential superclasses are asserted; none takes every
    /// object property of the ontology's signature, as ROBOT does
    pub properties: BTreeSet<ObjectProperty>,
    /// ROBOT's `--create-new-ontology`, with which its command saves the ontology without
    /// the axioms it found, so that only the validation has an effect
    pub create_new_ontology: bool,
}

/// The fresh classes that name the existential restrictions.
const EXPRESSION: &str = "urn:sparkles:materialize:";

/// Assert the direct superclasses `ObjectSomeValuesFrom(p B)` of the ontology's classes for
/// the chosen properties, and the direct named superclasses beside them that are not
/// asserted, as ROBOT's `materialize` does. The ontology is validated first, as ROBOT's
/// `ReasonerHelper.validate` does, and is changed only when the run succeeds.
pub fn materialize<E>(
    doc: &mut Document,
    o: &MaterializeOptions,
    classify: &mut Classify<'_, E>,
) -> Result<Report, Error<E>> {
    materialize_with(doc, o, classify, true)
}

/// [`materialize`], naming every class of the signature's expressions as ROBOT does
/// when `prune` is false.
pub(crate) fn materialize_with<E>(
    doc: &mut Document,
    o: &MaterializeOptions,
    classify: &mut Classify<'_, E>,
    prune: bool,
) -> Result<Report, Error<E>> {
    let mut report = Report::default();
    let sig = doc.signature();
    let mut fresh: FxHashSet<Class> = FxHashSet::default();
    let probes = Probes::new(&sig, &mut fresh);
    let properties: Vec<&ObjectProperty> = if o.properties.is_empty() {
        sig.object_properties.iter().collect()
    } else {
        o.properties.iter().collect()
    };
    let classes: Vec<&Class> = sig.classes.iter().collect();
    // ROBOT names `ObjectSomeValuesFrom(p B)` for every class B of the signature. Only the
    // fillers that some class can have a successor in are named here, which changes no
    // answer (see `fillers`).
    let fillers: Vec<&Class> = match fillers(doc).filter(|_| prune) {
        Some(f) => classes
            .iter()
            .copied()
            .filter(|c| c.as_str() == vocab::OWL_THING || f.contains(c))
            .collect(),
        None => classes.clone(),
    };

    // the expression materializing reasoner's classes: one equivalent to
    // `ObjectSomeValuesFrom(p B)` for each property p and each filler B
    let n = properties.len() * fillers.len();
    let mut names: Vec<Class> = Vec::with_capacity(n);
    for i in 0..n {
        names.push(fresh_class(EXPRESSION, i, &sig.classes));
    }
    let expression = |i: usize| -> ClassExpression {
        ClassExpression::some(
            properties[i / fillers.len()].clone(),
            fillers[i % fillers.len()].clone(),
        )
    };
    let definitions: Vec<AxiomKind> = names
        .iter()
        .enumerate()
        .map(|(i, x)| {
            AxiomKind::EquivalentClasses([ClassExpression::Class(x.clone()), expression(i)].into())
        })
        .collect();
    let named: FxHashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, x)| (x.as_str(), i))
        .collect();

    let mut kinds = doc.logical_kinds();
    kinds.extend(probes.axioms.iter());
    kinds.extend(definitions.iter());
    let (classification, excluded) = classify(&kinds).map_err(Error::Classifier)?;
    drop(kinds);
    drop(definitions);
    report.excluded = excluded;
    // ROBOT validates before it adds the expressions' classes, so an expression that is
    // unsatisfiable, such as one whose filler meets the property's range in nothing, does
    // not fail
    probes.validate(
        &classification,
        |c| fresh.contains(c) || named.contains_key(c.as_str()),
        || None,
    )?;

    // the classes OWL API's `getAxioms(OWLClass)` gives axioms for: the subclass of a
    // subclass axiom, an operand of an equivalence or a disjointness, the class of a
    // disjoint union
    let mut described: FxHashSet<&Class> = FxHashSet::default();
    for a in &doc.axioms {
        match &a.kind {
            AxiomKind::SubClassOf(ClassExpression::Class(c), _)
            | AxiomKind::DisjointUnion(c, _) => {
                described.insert(c);
            }
            AxiomKind::EquivalentClasses(cs) | AxiomKind::DisjointClasses(cs) => {
                for x in cs {
                    if let ClassExpression::Class(c) = x {
                        described.insert(c);
                    }
                }
            }
            _ => {}
        }
    }

    let tax = &classification.taxonomy;
    let mut found: BTreeSet<Axiom> = BTreeSet::new();
    let mut by_property: FxHashMap<&ObjectProperty, usize> = FxHashMap::default();
    for &c in &classes {
        if c.as_str() == vocab::OWL_NOTHING || !described.contains(c) {
            continue;
        }
        // ELK's `getSuperClasses(c, true)`, flattened: the classes of the parents of c's
        // node, the top node for a class the taxonomy does not hold
        let parents: &[usize] = match tax.node_of(c) {
            Some(BOTTOM) => &[],
            Some(at) => &tax.node(at).parents,
            None => &[TOP],
        };
        for &at in parents {
            for d in &tax.node(at).classes {
                if fresh.contains(d) {
                    continue;
                }
                let (sup, property) = match named.get(d.as_str()) {
                    Some(&i) => (expression(i), Some(properties[i / fillers.len()])),
                    None => (ClassExpression::Class(d.clone()), None),
                };
                let filler = match &sup {
                    ClassExpression::ObjectSomeValuesFrom(_, f) => f.as_ref(),
                    x => x,
                };
                // no axiom with owl:Thing, which is trivial, and none whose superclass
                // names the class, as a reflexive property gives every class
                if matches!(filler, ClassExpression::Class(f) if f.as_str() == vocab::OWL_THING || f == c)
                {
                    continue;
                }
                let ax = Axiom::sub_class_of(c.clone(), sup);
                if doc.axioms.contains(&ax) {
                    continue;
                }
                if found.insert(ax)
                    && let Some(p) = property
                {
                    *by_property.entry(p).or_default() += 1;
                }
            }
        }
    }
    drop(classification);

    let mut counts: Vec<(&ObjectProperty, usize)> = by_property.into_iter().collect();
    counts.sort();
    for (p, k) in counts {
        report.warnings.push(format!(
            "{} gives {k} materialized superclasses",
            p.as_str()
        ));
    }
    if o.create_new_ontology {
        // ROBOT's operation adds the axioms to a new ontology that its command does not
        // save
        report.warnings.push(format!(
            "--create-new-ontology true: the {} materialized superclasses go to a new ontology that is not saved, as in ROBOT",
            found.len()
        ));
        return Ok(report);
    }
    for a in found {
        if doc.axioms.insert(a) {
            report.added += 1;
        }
    }
    Ok(report)
}

/// The classes B for which `ObjectSomeValuesFrom(p B)` can be a superclass of a satisfiable
/// class of the ontology, or `None` when every class can be one.
///
/// A class that occurs only where it can be made smaller without making an axiom false,
/// such as the subclass of a subclass axiom, an operand of a disjointness or the class of
/// a key, can be given no instances in any model of the ontology. No other class then
/// needs an instance with a successor in it, so `ObjectSomeValuesFrom(p B)` is a
/// superclass of no class but B itself, and of B only when an instance of B can be its
/// own successor. That takes a reflexive property, `ObjectHasSelf` or an individual, and
/// an ontology with any of them names every class. The expressions left unnamed are thus
/// in no node above a class of the ontology and change none of the direct superclasses
/// that ROBOT reads, and the classifier is sound, so it derives none of them either. The
/// classes that occur where they could grow, owl:Thing among them, are the fillers. An
/// expression this does not know, such as a complement, counts its classes both ways.
pub(crate) fn fillers(doc: &Document) -> Option<FxHashSet<&Class>> {
    let mut out: FxHashSet<&Class> = FxHashSet::default();
    for a in &doc.axioms {
        let ok = match &a.kind {
            AxiomKind::SubClassOf(sub, sup) => {
                walk(sub, false, false, &mut out) && walk(sup, true, false, &mut out)
            }
            AxiomKind::EquivalentClasses(cs) => cs.iter().all(|x| walk(x, true, true, &mut out)),
            AxiomKind::DisjointClasses(cs) => cs.iter().all(|x| walk(x, false, false, &mut out)),
            AxiomKind::DisjointUnion(c, cs) => {
                out.insert(c);
                cs.iter().all(|x| walk(x, true, true, &mut out))
            }
            AxiomKind::ObjectPropertyDomain(p, x) | AxiomKind::ObjectPropertyRange(p, x) => {
                !top(p) && walk(x, true, false, &mut out)
            }
            AxiomKind::DataPropertyDomain(_, x) => walk(x, true, false, &mut out),
            AxiomKind::HasKey(x, ..) => walk(x, false, false, &mut out),
            AxiomKind::ReflexiveObjectProperty(_)
            | AxiomKind::SameIndividual(_)
            | AxiomKind::DifferentIndividuals(_)
            | AxiomKind::ClassAssertion(..)
            | AxiomKind::ObjectPropertyAssertion(..)
            | AxiomKind::NegativeObjectPropertyAssertion(..)
            | AxiomKind::DataPropertyAssertion(..)
            | AxiomKind::NegativeDataPropertyAssertion(..) => false,
            AxiomKind::SubObjectPropertyOf(sub, sup) => {
                !top(sup)
                    && match sub {
                        SubObjectPropertyExpression::Expression(p) => !top(p),
                        SubObjectPropertyExpression::Chain(ps) => !ps.iter().any(top),
                    }
            }
            AxiomKind::EquivalentObjectProperties(ps) => !ps.iter().any(top),
            _ => true,
        };
        if !ok {
            return None;
        }
    }
    Some(out)
}

/// Whether a property expression is the top object property, which relates every pair.
fn top(p: &ObjectPropertyExpression) -> bool {
    p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
}

/// Adds the classes of `x` that occur where they could grow, `x` itself occurring that
/// way when `positive` holds and both ways when `both` does. The answer is false for an
/// expression with an individual, `ObjectHasSelf` or the top property, for which every
/// class is a filler.
fn walk<'a>(
    x: &'a ClassExpression,
    positive: bool,
    both: bool,
    out: &mut FxHashSet<&'a Class>,
) -> bool {
    use ClassExpression as X;
    match x {
        X::Class(c) => {
            if positive || both {
                out.insert(c);
            }
            true
        }
        X::ObjectIntersectionOf(xs) | X::ObjectUnionOf(xs) => {
            xs.iter().all(|y| walk(y, positive, both, out))
        }
        X::ObjectComplementOf(y) => walk(y, !positive, both, out),
        X::ObjectSomeValuesFrom(p, y) | X::ObjectAllValuesFrom(p, y) => {
            !top(p) && walk(y, positive, both, out)
        }
        X::ObjectMinCardinality(_, p, y) => {
            !top(p) && y.as_deref().is_none_or(|y| walk(y, positive, both, out))
        }
        X::ObjectMaxCardinality(_, p, y) => {
            !top(p) && y.as_deref().is_none_or(|y| walk(y, !positive, both, out))
        }
        X::ObjectExactCardinality(_, p, y) => {
            !top(p) && y.as_deref().is_none_or(|y| walk(y, true, true, out))
        }
        X::ObjectOneOf(_) | X::ObjectHasValue(..) | X::ObjectHasSelf(_) => false,
        X::DataSomeValuesFrom(..)
        | X::DataAllValuesFrom(..)
        | X::DataHasValue(..)
        | X::DataMinCardinality(..)
        | X::DataMaxCardinality(..)
        | X::DataExactCardinality(..) => true,
    }
}
