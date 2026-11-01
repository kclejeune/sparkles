//! OWL 2 QL (OWL 2 Profiles §3).

use super::{Broken, EL_QL_FORBIDDEN_DATATYPES, broken, datatypes, individuals};
use crate::model::*;

const ENTITIES: &str = "OWL 2 Profiles §3.2.1";
const CLASSES: &str = "OWL 2 Profiles §3.2.3";
const RANGES: &str = "OWL 2 Profiles §3.2.4";
const AXIOMS: &str = "OWL 2 Profiles §3.2.5";

// IrreflexiveObjectProperty is not allowed: the ObjectPropertyAxiom production of
// §3.2.5 omits it, although the feature list of §3 names it. The W3C test case
// New-Feature-IrreflexiveProperty-001 follows the grammar.

fn is_thing(c: &ClassExpression) -> bool {
    matches!(c, ClassExpression::Class(c) if c.as_str() == crate::vocab::OWL_THING)
}

/// subClassExpression := Class | ObjectSomeValuesFrom(OPE owl:Thing) |
/// DataSomeValuesFrom(DPE DataRange)
fn sub(c: &ClassExpression, out: &mut Vec<Broken>) {
    use ClassExpression as C;
    match c {
        C::Class(_) => {}
        C::ObjectSomeValuesFrom(_, f) if is_thing(f) => {}
        C::ObjectSomeValuesFrom(..) => out.push(broken(
            CLASSES,
            "ObjectSomeValuesFrom with a filler other than owl:Thing is not allowed as a subclass expression",
        )),
        C::DataSomeValuesFrom(ps, r) => data_some(ps, r, out),
        other => out.push(broken(
            CLASSES,
            format!("{} is not allowed as a subclass expression", other.type_name()),
        )),
    }
}

/// superClassExpression := Class | ObjectIntersectionOf(super…) |
/// ObjectComplementOf(sub) | ObjectSomeValuesFrom(OPE Class) | DataSomeValuesFrom
fn sup(c: &ClassExpression, out: &mut Vec<Broken>) {
    use ClassExpression as C;
    match c {
        C::Class(_) => {}
        C::ObjectIntersectionOf(cs) => cs.iter().for_each(|c| sup(c, out)),
        C::ObjectComplementOf(c) => sub(c, out),
        C::ObjectSomeValuesFrom(_, f) if matches!(**f, C::Class(_)) => {}
        C::ObjectSomeValuesFrom(..) => out.push(broken(
            CLASSES,
            "ObjectSomeValuesFrom with a filler other than a class is not allowed as a superclass expression",
        )),
        C::DataSomeValuesFrom(ps, r) => data_some(ps, r, out),
        other => out.push(broken(
            CLASSES,
            format!("{} is not allowed as a superclass expression", other.type_name()),
        )),
    }
}

fn data_some(ps: &[DataProperty], r: &DataRange, out: &mut Vec<Broken>) {
    if ps.len() != 1 {
        out.push(broken(
            CLASSES,
            "DataSomeValuesFrom with several data properties is not allowed",
        ));
    }
    dr(r, out);
}

fn dr(r: &DataRange, out: &mut Vec<Broken>) {
    match r {
        DataRange::Datatype(_) => {}
        DataRange::IntersectionOf(rs) => rs.iter().for_each(|r| dr(r, out)),
        other => out.push(broken(
            RANGES,
            format!(
                "{} is not allowed",
                match other {
                    DataRange::UnionOf(_) => "DataUnionOf",
                    DataRange::ComplementOf(_) => "DataComplementOf",
                    DataRange::OneOf(_) => "DataOneOf",
                    _ => "DatatypeRestriction",
                }
            ),
        )),
    }
}

pub(super) fn axiom(ax: &Axiom) -> Vec<Broken> {
    use AxiomKind as K;
    let mut out = Vec::new();
    match &ax.kind {
        K::SubClassOf(a, b) => {
            sub(a, &mut out);
            sup(b, &mut out);
        }
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) => {
            cs.iter().for_each(|c| sub(c, &mut out))
        }
        K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _) => out.push(broken(
            AXIOMS,
            "SubObjectPropertyOf with a property chain is not allowed",
        )),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c) => sup(c, &mut out),
        K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => dr(r, &mut out),
        K::ClassAssertion(c, _) => {
            if !matches!(c, ClassExpression::Class(_)) {
                out.push(broken(
                    AXIOMS,
                    "ClassAssertion with a class expression other than a class is not allowed",
                ));
            }
        }
        K::DisjointUnion(..)
        | K::FunctionalObjectProperty(_)
        | K::InverseFunctionalObjectProperty(_)
        | K::IrreflexiveObjectProperty(_)
        | K::TransitiveObjectProperty(_)
        | K::FunctionalDataProperty(_)
        | K::HasKey(..)
        | K::SameIndividual(_)
        | K::NegativeObjectPropertyAssertion(..)
        | K::NegativeDataPropertyAssertion(..) => out.push(broken(
            AXIOMS,
            format!("{} is not allowed", ax.axiom_type().name()),
        )),
        _ => {}
    }
    for d in datatypes(&ax.kind) {
        if EL_QL_FORBIDDEN_DATATYPES.contains(&d.as_str()) {
            out.push(broken(
                ENTITIES,
                format!("the datatype <{d}> is not allowed"),
            ));
        }
    }
    if individuals(&ax.kind)
        .iter()
        .any(|i| matches!(i, Individual::Anonymous(_)))
    {
        out.push(broken(ENTITIES, "anonymous individuals are not allowed"));
    }
    out
}
