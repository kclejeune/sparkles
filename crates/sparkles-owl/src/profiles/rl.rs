//! OWL 2 RL (OWL 2 Profiles §4).

use super::{Broken, broken, datatypes};
use crate::model::*;
use crate::vocab;

const ENTITIES: &str = "OWL 2 Profiles §4.2.1";
const CLASSES: &str = "OWL 2 Profiles §4.2.3";
const RANGES: &str = "OWL 2 Profiles §4.2.4";
const AXIOMS: &str = "OWL 2 Profiles §4.2.5";

fn is_thing(c: &ClassExpression) -> bool {
    matches!(c, ClassExpression::Class(c) if c.as_str() == vocab::OWL_THING)
}

fn thing(out: &mut Vec<Broken>, position: &str) {
    out.push(broken(
        CLASSES,
        format!("owl:Thing is not allowed as a {position} expression"),
    ));
}

/// subClassExpression of §4.2.3.
fn sub(c: &ClassExpression, out: &mut Vec<Broken>) {
    use ClassExpression as C;
    match c {
        C::Class(_) if is_thing(c) => thing(out, "subclass"),
        C::Class(_) | C::ObjectOneOf(_) | C::ObjectHasValue(..) | C::DataHasValue(..) => {}
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().for_each(|c| sub(c, out)),
        C::ObjectSomeValuesFrom(_, f) => {
            if !is_thing(f) {
                sub(f, out);
            }
        }
        C::DataSomeValuesFrom(_, r) => dr(r, out),
        other => out.push(broken(
            CLASSES,
            format!(
                "{} is not allowed as a subclass expression",
                other.type_name()
            ),
        )),
    }
}

/// superClassExpression of §4.2.3.
fn sup(c: &ClassExpression, out: &mut Vec<Broken>) {
    use ClassExpression as C;
    match c {
        C::Class(_) if is_thing(c) => thing(out, "superclass"),
        C::Class(_) | C::ObjectHasValue(..) | C::DataHasValue(..) => {}
        C::ObjectIntersectionOf(cs) => cs.iter().for_each(|c| sup(c, out)),
        C::ObjectComplementOf(c) => sub(c, out),
        C::ObjectAllValuesFrom(_, c) => sup(c, out),
        C::ObjectMaxCardinality(n, _, f) => {
            if *n > 1 {
                out.push(broken(
                    CLASSES,
                    "ObjectMaxCardinality above 1 is not allowed",
                ));
            }
            if let Some(f) = f
                && !is_thing(f)
            {
                sub(f, out);
            }
        }
        C::DataAllValuesFrom(_, r) => dr(r, out),
        C::DataMaxCardinality(n, _, r) => {
            if *n > 1 {
                out.push(broken(CLASSES, "DataMaxCardinality above 1 is not allowed"));
            }
            if let Some(r) = r {
                dr(r, out);
            }
        }
        other => out.push(broken(
            CLASSES,
            format!(
                "{} is not allowed as a superclass expression",
                other.type_name()
            ),
        )),
    }
}

/// equivClassExpression of §4.2.3.
fn equiv(c: &ClassExpression, out: &mut Vec<Broken>) {
    use ClassExpression as C;
    match c {
        C::Class(_) if is_thing(c) => thing(out, "equivalent class"),
        C::Class(_) | C::ObjectHasValue(..) | C::DataHasValue(..) => {}
        C::ObjectIntersectionOf(cs) => cs.iter().for_each(|c| equiv(c, out)),
        other => out.push(broken(
            CLASSES,
            format!("{} is not allowed in EquivalentClasses", other.type_name()),
        )),
    }
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
        K::EquivalentClasses(cs) => cs.iter().for_each(|c| equiv(c, &mut out)),
        K::DisjointClasses(cs) => cs.iter().for_each(|c| sub(c, &mut out)),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c) => sup(c, &mut out),
        K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => dr(r, &mut out),
        K::HasKey(c, ..) => sub(c, &mut out),
        K::ClassAssertion(c, _) => sup(c, &mut out),
        K::DisjointUnion(..) | K::ReflexiveObjectProperty(_) => out.push(broken(
            AXIOMS,
            format!("{} is not allowed", ax.axiom_type().name()),
        )),
        _ => {}
    }
    for d in datatypes(&ax.kind) {
        if d == vocab::OWL_REAL || d == vocab::OWL_RATIONAL {
            out.push(broken(
                ENTITIES,
                format!("the datatype <{d}> is not allowed"),
            ));
        }
    }
    if !matches!(ax.kind, K::Declaration(_)) {
        let mut s = crate::model::Signature::default();
        s.axiom(&ax.kind);
        for e in &s.0 {
            let i = e.iri().as_str();
            if matches!(e, Entity::ObjectProperty(_) | Entity::DataProperty(_))
                && [
                    vocab::OWL_TOP_OBJECT_PROPERTY,
                    vocab::OWL_BOTTOM_OBJECT_PROPERTY,
                    vocab::OWL_TOP_DATA_PROPERTY,
                    vocab::OWL_BOTTOM_DATA_PROPERTY,
                ]
                .contains(&i)
            {
                out.push(broken(
                    ENTITIES,
                    format!("the property <{i}> is not allowed"),
                ));
            }
        }
    }
    out
}
