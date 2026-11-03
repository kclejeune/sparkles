//! `reduce`: the ontology without the subclass axioms that other subclass axioms entail,
//! as ROBOT 1.9.11's `ReduceOperation` removes them with ELK.

use super::{Classify, Document, Error, Report, TOP, View, fresh_class};
use crate::model::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

/// The options of `reduce`, with ROBOT's defaults.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReduceOptions {
    /// keep the subclass axioms that have annotations
    pub preserve_annotated_axioms: bool,
    /// only the axioms between named classes, by the classification of the whole
    /// ontology
    pub named_classes_only: bool,
    /// also reason with the subproperty axioms and property chains
    pub include_subproperties: bool,
}

/// The fresh classes that name the class expressions of subclass axioms.
const EXPRESSION: &str = "urn:sparkles:reduce:expression:";

/// Remove the redundant subclass axioms, as ROBOT's `reduce` does. An inconsistent
/// ontology is left as it is, with a warning, as ROBOT leaves it.
pub fn reduce<E>(
    doc: &mut Document,
    o: &ReduceOptions,
    classify: &mut Classify<'_, E>,
) -> Result<Report, Error<E>> {
    if o.named_classes_only {
        named_only(doc, o, classify)
    } else {
        all_expressions(doc, o, classify)
    }
}

/// ROBOT's `reduceAllClassExpressions`: the subclass axioms and the object property
/// characteristics, and with `include_subproperties` the subproperty axioms and chains,
/// are classified on their own, with each anonymous class expression of a subclass axiom
/// named by a fresh class equivalent to it. `SubClassOf(A B)` is redundant when an
/// asserted superclass of A, B itself included, has B among its strict superclasses, or,
/// when A is anonymous, when a strict superclass of A that has asserted superclasses has
/// B among its strict superclasses.
fn all_expressions<E>(
    doc: &mut Document,
    o: &ReduceOptions,
    classify: &mut Classify<'_, E>,
) -> Result<Report, Error<E>> {
    let mut report = Report::default();
    let taken: BTreeSet<Class> = doc.signature().classes;
    let mut name: FxHashMap<&ClassExpression, Class> = FxHashMap::default();
    let mut fresh_n = 0usize;
    let mut asserted: FxHashMap<Class, FxHashSet<Class>> = FxHashMap::default();
    let mut kinds: Vec<&AxiomKind> = Vec::new();
    for a in &doc.axioms {
        match &a.kind {
            AxiomKind::SubClassOf(sub, sup) => {
                for x in [sub, sup] {
                    if !name.contains_key(x) {
                        let c = match x {
                            ClassExpression::Class(c) => c.clone(),
                            _ => {
                                fresh_n += 1;
                                fresh_class(EXPRESSION, fresh_n - 1, &taken)
                            }
                        };
                        name.insert(x, c);
                    }
                }
                asserted
                    .entry(name[sub].clone())
                    .or_default()
                    .insert(name[sup].clone());
                if kinds.last() != Some(&&a.kind) {
                    kinds.push(&a.kind);
                }
            }
            AxiomKind::FunctionalObjectProperty(_)
            | AxiomKind::InverseFunctionalObjectProperty(_)
            | AxiomKind::ReflexiveObjectProperty(_)
            | AxiomKind::IrreflexiveObjectProperty(_)
            | AxiomKind::SymmetricObjectProperty(_)
            | AxiomKind::AsymmetricObjectProperty(_)
            | AxiomKind::TransitiveObjectProperty(_) => kinds.push(&a.kind),
            AxiomKind::SubObjectPropertyOf(..) if o.include_subproperties => kinds.push(&a.kind),
            _ => {}
        }
    }
    let mut definitions: Vec<AxiomKind> = name
        .iter()
        .filter(|(x, _)| !matches!(x, ClassExpression::Class(_)))
        .map(|(x, c)| {
            AxiomKind::EquivalentClasses([ClassExpression::Class(c.clone()), (*x).clone()].into())
        })
        .collect();
    definitions.sort_unstable();
    kinds.extend(definitions.iter());
    let (classification, excluded) = classify(&kinds).map_err(Error::Classifier)?;
    drop(kinds);
    report.excluded = excluded;
    if !classification.consistent {
        report.warnings.push("Ontology is not consistent!".into());
        return Ok(report);
    }
    // ROBOT's reasoner sees the fresh classes as classes
    let none = FxHashSet::default();
    let view = View::new(&classification, &none);
    note_unsatisfiable(&view, &mut report);
    let mut above: FxHashMap<Option<usize>, FxHashSet<usize>> = FxHashMap::default();
    let mut strictly_above = |x: &Class, y: &Class| -> bool {
        let Some(ny) = view.node_of(y) else {
            return false;
        };
        let nx = view.node_of(x);
        above
            .entry(nx)
            .or_insert_with(|| view.strict_super_nodes(x).into_iter().collect())
            .contains(&ny)
    };
    let mut remove: Vec<Axiom> = Vec::new();
    for a in &doc.axioms {
        let AxiomKind::SubClassOf(sub_x, sup_x) = &a.kind else {
            continue;
        };
        if o.preserve_annotated_axioms && !a.annotations.is_empty() {
            continue;
        }
        let sub = &name[sub_x];
        let sup = &name[sup_x];
        let mut redundant = asserted[sub].iter().any(|x| strictly_above(x, sup));
        if !redundant && !matches!(sub_x, ClassExpression::Class(_)) {
            // the special case of a general class inclusion
            let parents: Vec<Class> = view
                .strict_super_nodes(sub)
                .into_iter()
                .flat_map(|n| view.members(n).cloned().collect::<Vec<_>>())
                .collect();
            redundant = parents
                .iter()
                .any(|p| asserted.contains_key(p) && strictly_above(p, sup));
        }
        if redundant {
            remove.push(a.clone());
        }
    }
    report.removed = remove.len();
    for a in remove {
        doc.axioms.remove(&a);
    }
    Ok(report)
}

/// ROBOT's `reduceNamedOnly`: the whole ontology is classified, and a subclass axiom
/// between named classes stays when its superclass is a class of a node and its subclass
/// a class of a direct subclass node of it, the bottom node being the direct subclass
/// node of a node without others.
fn named_only<E>(
    doc: &mut Document,
    o: &ReduceOptions,
    classify: &mut Classify<'_, E>,
) -> Result<Report, Error<E>> {
    let mut report = Report::default();
    let kinds = doc.logical_kinds();
    let (classification, excluded) = classify(&kinds).map_err(Error::Classifier)?;
    drop(kinds);
    report.excluded = excluded;
    if !classification.consistent {
        report.warnings.push("Ontology is not consistent!".into());
        return Ok(report);
    }
    let none = FxHashSet::default();
    let view = View::new(&classification, &none);
    note_unsatisfiable(&view, &mut report);
    let pairs: FxHashSet<(&Class, &Class)> = doc
        .axioms
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::SubClassOf(ClassExpression::Class(c), ClassExpression::Class(d)) => {
                Some((c, d))
            }
            _ => None,
        })
        .collect();
    let mut keep: FxHashSet<(&Class, &Class)> = FxHashSet::default();
    let mut seen = vec![false; classification.taxonomy.nodes().len()];
    let mut stack = vec![TOP];
    while let Some(n) = stack.pop() {
        if std::mem::replace(&mut seen[n], true) {
            continue;
        }
        let subs = view.direct_sub_nodes(n);
        for d in view.members(n) {
            for &s in &subs {
                for c in view.members(s) {
                    if let Some(p) = pairs.get(&(c, d)) {
                        keep.insert(*p);
                    }
                }
            }
        }
        stack.extend(subs.into_iter().filter(|&s| !seen[s]));
    }
    let remove: Vec<Axiom> = doc
        .axioms
        .iter()
        .filter(|a| match &a.kind {
            AxiomKind::SubClassOf(ClassExpression::Class(c), ClassExpression::Class(d)) => {
                !(o.preserve_annotated_axioms && !a.annotations.is_empty())
                    && !keep.contains(&(c, d))
            }
            _ => false,
        })
        .cloned()
        .collect();
    drop((keep, pairs));
    report.removed = remove.len();
    for a in remove {
        doc.axioms.remove(&a);
    }
    Ok(report)
}

/// ROBOT's log of the unsatisfiable classes, which `reduce` does not refuse.
fn note_unsatisfiable(view: &View<'_>, report: &mut Report) {
    let unsat: Vec<&Class> = view
        .members(super::BOTTOM)
        .filter(|c| c.as_str() != crate::vocab::OWL_NOTHING)
        .collect();
    if !unsat.is_empty() {
        report.warnings.push(format!(
            "There are {} unsatisfiable classes in the ontology.",
            unsat.len() + 1
        ));
        for c in unsat {
            report
                .warnings
                .push(format!("    unsatisfiable: {}", c.as_str()));
        }
    }
}
