//! The structural conditions on axioms that the types cannot express: the least number
//! of operands of each n-ary construct (structural specification §6–§9), which the
//! forward mapping needs to write an axiom.

use crate::model::*;

fn need(n: usize, least: usize, what: &str) -> Result<(), ModelError> {
    if n < least {
        Err(ModelError(format!(
            "{what} needs at least {least} operands, not {n}"
        )))
    } else {
        Ok(())
    }
}

pub fn data_range(r: &DataRange) -> Result<(), ModelError> {
    match r {
        DataRange::Datatype(_) => Ok(()),
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
            need(
                rs.len(),
                2,
                if matches!(r, DataRange::IntersectionOf(_)) {
                    "DataIntersectionOf"
                } else {
                    "DataUnionOf"
                },
            )?;
            rs.iter().try_for_each(data_range)
        }
        DataRange::ComplementOf(r) => data_range(r),
        DataRange::OneOf(ls) => need(ls.len(), 1, "DataOneOf"),
        DataRange::DatatypeRestriction(_, fs) => need(fs.len(), 1, "DatatypeRestriction"),
    }
}

pub fn class_expression(c: &ClassExpression) -> Result<(), ModelError> {
    use ClassExpression as C;
    match c {
        C::Class(_) | C::ObjectHasValue(..) | C::ObjectHasSelf(_) | C::DataHasValue(..) => Ok(()),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            need(cs.len(), 2, c.type_name())?;
            cs.iter().try_for_each(class_expression)
        }
        C::ObjectComplementOf(c) | C::ObjectSomeValuesFrom(_, c) | C::ObjectAllValuesFrom(_, c) => {
            class_expression(c)
        }
        C::ObjectOneOf(is) => need(is.len(), 1, "ObjectOneOf"),
        C::ObjectMinCardinality(_, _, f)
        | C::ObjectMaxCardinality(_, _, f)
        | C::ObjectExactCardinality(_, _, f) => f.as_deref().map_or(Ok(()), class_expression),
        C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
            need(ps.len(), 1, c.type_name())?;
            data_range(r)
        }
        C::DataMinCardinality(_, _, r)
        | C::DataMaxCardinality(_, _, r)
        | C::DataExactCardinality(_, _, r) => r.as_ref().map_or(Ok(()), data_range),
    }
}

/// Check that an axiom can be written.
pub fn axiom(ax: &Axiom) -> Result<(), ModelError> {
    use AxiomKind as K;
    let name = ax.axiom_type().name();
    match &ax.kind {
        K::SubClassOf(a, b) => {
            class_expression(a)?;
            class_expression(b)
        }
        // an equivalence of one operand, such as EquivalentClasses(C C), is legal and
        // trivial; a disjointness of one would claim that its operand is empty, which
        // a set of operands cannot express
        K::EquivalentClasses(cs) => {
            need(cs.len(), 1, name)?;
            cs.iter().try_for_each(class_expression)
        }
        K::DisjointClasses(cs) => {
            need(cs.len(), 2, name)?;
            cs.iter().try_for_each(class_expression)
        }
        K::DisjointUnion(_, cs) => {
            need(cs.len(), 2, name)?;
            cs.iter().try_for_each(class_expression)
        }
        K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), _) => {
            need(ps.len(), 2, "ObjectPropertyChain")
        }
        K::EquivalentObjectProperties(ps) => need(ps.len(), 1, name),
        K::EquivalentDataProperties(ps) => need(ps.len(), 1, name),
        K::DisjointObjectProperties(ps) => need(ps.len(), 2, name),
        K::DisjointDataProperties(ps) => need(ps.len(), 2, name),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::ClassAssertion(c, _) => class_expression(c),
        K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => data_range(r),
        K::HasKey(c, ops, dps) => {
            need(ops.len() + dps.len(), 1, name)?;
            class_expression(c)
        }
        K::SameIndividual(is) => need(is.len(), 1, name),
        K::DifferentIndividuals(is) => need(is.len(), 2, name),
        _ => Ok(()),
    }
}
