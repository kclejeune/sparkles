//! OWL 2 EL (OWL 2 Profiles §2).

use super::{Broken, EL_QL_FORBIDDEN_DATATYPES, broken, datatypes, individuals};
use crate::model::*;
use std::collections::{BTreeMap, BTreeSet};

const ENTITIES: &str = "OWL 2 Profiles §2.2.1";
const PROPERTIES: &str = "OWL 2 Profiles §2.2.2";
const CLASSES: &str = "OWL 2 Profiles §2.2.3";
const RANGES: &str = "OWL 2 Profiles §2.2.4";
const AXIOMS: &str = "OWL 2 Profiles §2.2.5";
const GLOBAL: &str = "OWL 2 Profiles §2.2.6";

fn ope(p: &ObjectPropertyExpression, out: &mut Vec<Broken>) {
    if let ObjectPropertyExpression::InverseOf(_) = p {
        out.push(broken(PROPERTIES, "ObjectInverseOf is not allowed"));
    }
}

fn ce(c: &ClassExpression, out: &mut Vec<Broken>) {
    use ClassExpression as C;
    match c {
        C::Class(_) | C::DataHasValue(..) => {}
        C::ObjectIntersectionOf(cs) => cs.iter().for_each(|c| ce(c, out)),
        C::ObjectOneOf(is) => {
            if is.len() != 1 {
                out.push(broken(
                    CLASSES,
                    "ObjectOneOf with more than one individual is not allowed",
                ));
            }
        }
        C::ObjectSomeValuesFrom(p, c) => {
            ope(p, out);
            ce(c, out);
        }
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => ope(p, out),
        C::DataSomeValuesFrom(_, r) => dr(r, out),
        other => out.push(broken(
            CLASSES,
            format!("{} is not allowed", other.type_name()),
        )),
    }
}

fn dr(r: &DataRange, out: &mut Vec<Broken>) {
    match r {
        DataRange::Datatype(_) => {}
        DataRange::IntersectionOf(rs) => rs.iter().for_each(|r| dr(r, out)),
        DataRange::OneOf(ls) if ls.len() == 1 => {}
        DataRange::OneOf(_) => out.push(broken(
            RANGES,
            "DataOneOf with more than one literal is not allowed",
        )),
        DataRange::UnionOf(_) => out.push(broken(RANGES, "DataUnionOf is not allowed")),
        DataRange::ComplementOf(_) => out.push(broken(RANGES, "DataComplementOf is not allowed")),
        DataRange::DatatypeRestriction(..) => {
            out.push(broken(RANGES, "DatatypeRestriction is not allowed"))
        }
    }
}

/// The rules of one axiom.
pub(super) fn axiom(ax: &Axiom) -> Vec<Broken> {
    use AxiomKind as K;
    let mut out = Vec::new();
    match &ax.kind {
        K::SubClassOf(a, b) => {
            ce(a, &mut out);
            ce(b, &mut out);
        }
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) => {
            cs.iter().for_each(|c| ce(c, &mut out))
        }
        K::SubObjectPropertyOf(sub, sup) => {
            match sub {
                SubObjectPropertyExpression::Expression(p) => ope(p, &mut out),
                SubObjectPropertyExpression::Chain(ps) => ps.iter().for_each(|p| ope(p, &mut out)),
            }
            ope(sup, &mut out);
        }
        K::EquivalentObjectProperties(ps) => ps.iter().for_each(|p| ope(p, &mut out)),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            ope(p, &mut out);
            ce(c, &mut out);
        }
        K::ReflexiveObjectProperty(p) | K::TransitiveObjectProperty(p) => ope(p, &mut out),
        K::DataPropertyDomain(_, c) => ce(c, &mut out),
        K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => dr(r, &mut out),
        K::HasKey(c, ops, _) => {
            ce(c, &mut out);
            ops.iter().for_each(|p| ope(p, &mut out));
        }
        K::ClassAssertion(c, _) => ce(c, &mut out),
        K::ObjectPropertyAssertion(p, ..) | K::NegativeObjectPropertyAssertion(p, ..) => {
            ope(p, &mut out)
        }
        K::DisjointUnion(..)
        | K::DisjointObjectProperties(_)
        | K::InverseObjectProperties(..)
        | K::FunctionalObjectProperty(_)
        | K::InverseFunctionalObjectProperty(_)
        | K::IrreflexiveObjectProperty(_)
        | K::SymmetricObjectProperty(_)
        | K::AsymmetricObjectProperty(_)
        | K::DisjointDataProperties(_) => out.push(broken(
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

/// The global restriction of §2.2.6 on the ranges of property chains.
pub(super) struct El {
    /// the ranges each object property has, through its superproperties
    ranges: BTreeMap<ObjectProperty, BTreeSet<ClassExpression>>,
}

impl El {
    pub(super) fn new<'a>(axioms: impl Iterator<Item = &'a Axiom>) -> El {
        let mut sup: BTreeMap<ObjectProperty, BTreeSet<ObjectProperty>> = BTreeMap::new();
        let mut direct: BTreeMap<ObjectProperty, BTreeSet<ClassExpression>> = BTreeMap::new();
        for ax in axioms {
            match &ax.kind {
                AxiomKind::SubObjectPropertyOf(
                    SubObjectPropertyExpression::Expression(ObjectPropertyExpression::Property(a)),
                    ObjectPropertyExpression::Property(b),
                ) => {
                    sup.entry(a.clone()).or_default().insert(b.clone());
                }
                AxiomKind::ObjectPropertyRange(ObjectPropertyExpression::Property(p), c) => {
                    direct.entry(p.clone()).or_default().insert(c.clone());
                }
                _ => {}
            }
        }
        let mut props: BTreeSet<ObjectProperty> = sup.keys().cloned().collect();
        props.extend(direct.keys().cloned());
        let mut ranges = BTreeMap::new();
        for p in props {
            let mut seen = BTreeSet::new();
            let mut stack = vec![p.clone()];
            let mut rs = BTreeSet::new();
            while let Some(q) = stack.pop() {
                if !seen.insert(q.clone()) {
                    continue;
                }
                if let Some(d) = direct.get(&q) {
                    rs.extend(d.iter().cloned());
                }
                if let Some(s) = sup.get(&q) {
                    stack.extend(s.iter().cloned());
                }
            }
            ranges.insert(p, rs);
        }
        El { ranges }
    }

    pub(super) fn chain_ranges(&self, ax: &Axiom) -> Vec<Broken> {
        let AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(ps),
            ObjectPropertyExpression::Property(p),
        ) = &ax.kind
        else {
            return Vec::new();
        };
        let Some(ObjectPropertyExpression::Property(last)) = ps.last() else {
            return Vec::new();
        };
        let empty = BTreeSet::new();
        let want = self.ranges.get(p).unwrap_or(&empty);
        let have = self.ranges.get(last).unwrap_or(&empty);
        want.iter()
            .filter(|c| !have.contains(*c))
            .map(|c| {
                broken(
                    GLOBAL,
                    format!(
                        "the range {c} of <{}> is not a range of the chain's last property <{}>",
                        p.as_str(),
                        last.as_str()
                    ),
                )
            })
            .collect()
    }
}
