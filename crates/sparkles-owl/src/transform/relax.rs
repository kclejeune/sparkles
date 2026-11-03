//! `relax`: subclass axioms for the existential restrictions and named conjuncts of
//! equivalences, as ROBOT 1.9.11's `RelaxOperation` adds them.

use super::{Document, Report, thing};
use crate::model::*;
use std::collections::BTreeSet;

/// The options of `relax`, with the defaults of ROBOT's `relax` command.
#[derive(Clone, Copy, Debug)]
pub struct RelaxOptions {
    /// also relax `SubClassOf(C X)` with a named C and an anonymous X
    pub include_subclass_of: bool,
    /// skip the named classes of an equivalence (the command's default is true)
    pub exclude_named_classes: bool,
    /// only `ObjectSomeValuesFrom(p D)` with a named property and a named class
    pub enforce_obo_format: bool,
}

impl Default for RelaxOptions {
    fn default() -> RelaxOptions {
        RelaxOptions {
            include_subclass_of: false,
            exclude_named_classes: true,
            enforce_obo_format: false,
        }
    }
}

/// Add the relaxed subclass axioms to the document. For each named class C of each
/// `EquivalentClasses` axiom and each other class expression Y of it, and with
/// `include_subclass_of` for each `SubClassOf(C Y)` with an anonymous Y, it adds
/// `SubClassOf(C E)` for each existential E that Y is or has as a conjunct, at any depth
/// of intersections, and `SubClassOf(C D)` for each named class D that Y is or has as a
/// conjunct. A cardinality restriction of more than zero counts as the existential of its
/// property and filler, as OWL API's `OWLObjectCardinalityRestriction` covers the
/// minimum, maximum and exact ones. The added axioms have no annotations.
pub fn relax(doc: &mut Document, o: &RelaxOptions) -> Report {
    let mut new: BTreeSet<Axiom> = BTreeSet::new();
    for a in &doc.axioms {
        match &a.kind {
            AxiomKind::EquivalentClasses(cs) => {
                for x in cs {
                    let ClassExpression::Class(c) = x else {
                        continue;
                    };
                    for y in cs.iter().filter(|y| *y != x) {
                        if !matches!(y, ClassExpression::Class(_)) || !o.exclude_named_classes {
                            relax_expression(c, y, o.enforce_obo_format, &mut new);
                        }
                    }
                }
            }
            AxiomKind::SubClassOf(ClassExpression::Class(c), y)
                if o.include_subclass_of && !matches!(y, ClassExpression::Class(_)) =>
            {
                relax_expression(c, y, o.enforce_obo_format, &mut new);
            }
            _ => {}
        }
    }
    let mut report = Report::default();
    for a in new {
        if doc.axioms.insert(a) {
            report.added += 1;
        }
    }
    report
}

fn relax_expression(c: &Class, y: &ClassExpression, obo: bool, out: &mut BTreeSet<Axiom>) {
    let mut svfs = Vec::new();
    some_values_from(y, obo, &mut svfs);
    for s in svfs {
        out.insert(Axiom::sub_class_of(c.clone(), s));
    }
    let mut named = Vec::new();
    named_ancestors(y, &mut named);
    for d in named {
        out.insert(Axiom::sub_class_of(c.clone(), d.clone()));
    }
}

/// ROBOT's `getSomeValuesFromAncestor`.
fn some_values_from(x: &ClassExpression, obo: bool, out: &mut Vec<ClassExpression>) {
    use ClassExpression as C;
    let conformant = |p: &ObjectPropertyExpression, f: &ClassExpression| {
        !obo || (matches!(p, ObjectPropertyExpression::Property(_))
            && matches!(f, ClassExpression::Class(_)))
    };
    match x {
        C::ObjectSomeValuesFrom(p, f) => {
            if conformant(p, f) {
                out.push(x.clone());
            }
        }
        C::ObjectMinCardinality(n, p, f)
        | C::ObjectMaxCardinality(n, p, f)
        | C::ObjectExactCardinality(n, p, f) => {
            if *n > 0 {
                let f = f
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(|| ClassExpression::Class(thing()));
                if conformant(p, &f) {
                    out.push(ClassExpression::some(p.clone(), f));
                }
            }
        }
        C::ObjectIntersectionOf(cs) => {
            for c in cs {
                some_values_from(c, obo, out);
            }
        }
        _ => {}
    }
}

/// ROBOT's `getNamedAncestors`.
fn named_ancestors<'a>(x: &'a ClassExpression, out: &mut Vec<&'a Class>) {
    match x {
        ClassExpression::Class(c) => out.push(c),
        ClassExpression::ObjectIntersectionOf(cs) => {
            for c in cs {
                named_ancestors(c, out);
            }
        }
        _ => {}
    }
}
