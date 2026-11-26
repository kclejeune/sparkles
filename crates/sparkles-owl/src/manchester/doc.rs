//! Whole-document OWL 2 Manchester Syntax (`.omn`) writer and reader (W3C OWL 2
//! Manchester Syntax Working Group Note, Second Edition, 2012).
//!
//! [`write_document`] formats an ontology header and axioms as standard Manchester
//! frames (`Prefix:`, `Ontology:`, `Import:`, `Annotations:`, `AnnotationProperty:`,
//! `Datatype:`, `ObjectProperty:`, `DataProperty:`, `Class:`, `Individual:` and the
//! six misc frames `EquivalentClasses:`, `DisjointClasses:`, `EquivalentProperties:`,
//! `DisjointProperties:`, `SameIndividual:` and `DifferentIndividuals:`). Entity frames
//! are emitted in sorted IRI order while entries within frame sections and miscellaneous
//! frames preserve input order, so output is repeatable for a given input order.
//!
//! Axioms that Manchester frame syntax cannot represent cleanly—such as general class
//! inclusions with an anonymous subclass, `HasKey` on an anonymous class expression,
//! property axioms whose subject is `ObjectInverseOf`, n-ary data restrictions with
//! more than one data property, annotated `Declaration` axioms, or annotation
//! assertions on IRIs that have no frame—are either refused ([`WriteOptions::strict`])
//! or skipped and recorded in [`WriteReport`].
//!
//! [`parse_document`] reads a Manchester ontology document back into a
//! [`crate::ofn_read::Document`], reusing the crate's Manchester expression lexer and
//! parser.

use super::lex::{self, Name, Tok};
use super::parse::Parser;
use super::{Renderer, Span, Vocabulary};
use crate::model::*;
use crate::ofn::{Renderer as OfnRenderer, STANDARD_PREFIXES};
use crate::ofn_read::{Document, ParseError};
use crate::read::MAX_NESTING;
use crate::validate;
use crate::vocab;
use crate::write_common::{
    WriteError, WriteOptions, WriteReport, check_axiom_depth, check_header_depth,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

// ------------------------------------------------------------------ writing ------

#[derive(Default)]
struct ClassFrame<'a> {
    annotations: Vec<&'a Axiom>,
    sub_class_of: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    equivalent_to: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    disjoint_with: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    disjoint_union_of: Vec<(&'a BTreeSet<Annotation>, &'a Multiset<ClassExpression>)>,
    has_key: Vec<(
        &'a BTreeSet<Annotation>,
        &'a BTreeSet<ObjectPropertyExpression>,
        &'a BTreeSet<DataProperty>,
    )>,
}

#[derive(Default)]
struct DatatypeFrame<'a> {
    annotations: Vec<&'a Axiom>,
    equivalent_to: Vec<(&'a BTreeSet<Annotation>, &'a DataRange)>,
}

#[derive(Default)]
struct ObjectPropertyFrame<'a> {
    annotations: Vec<&'a Axiom>,
    domain: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    range: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    characteristics: Vec<(&'a BTreeSet<Annotation>, &'static str)>,
    sub_property_of: Vec<(&'a BTreeSet<Annotation>, &'a ObjectPropertyExpression)>,
    equivalent_to: Vec<(&'a BTreeSet<Annotation>, &'a ObjectPropertyExpression)>,
    disjoint_with: Vec<(&'a BTreeSet<Annotation>, &'a ObjectPropertyExpression)>,
    inverse_of: Vec<(&'a BTreeSet<Annotation>, &'a ObjectProperty)>,
    sub_property_chain: Vec<(&'a BTreeSet<Annotation>, &'a [ObjectPropertyExpression])>,
}

#[derive(Default)]
struct DataPropertyFrame<'a> {
    annotations: Vec<&'a Axiom>,
    domain: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    range: Vec<(&'a BTreeSet<Annotation>, &'a DataRange)>,
    functional: Vec<&'a BTreeSet<Annotation>>,
    sub_property_of: Vec<(&'a BTreeSet<Annotation>, &'a DataProperty)>,
    equivalent_to: Vec<(&'a BTreeSet<Annotation>, &'a DataProperty)>,
    disjoint_with: Vec<(&'a BTreeSet<Annotation>, &'a DataProperty)>,
}

#[derive(Default)]
struct AnnotationPropertyFrame<'a> {
    declared: bool,
    annotations: Vec<&'a Axiom>,
    domain: Vec<(&'a BTreeSet<Annotation>, &'a Iri)>,
    range: Vec<(&'a BTreeSet<Annotation>, &'a Iri)>,
    sub_property_of: Vec<(&'a BTreeSet<Annotation>, &'a AnnotationProperty)>,
}

#[derive(Default)]
struct IndividualFrame<'a> {
    annotations: Vec<&'a Axiom>,
    types: Vec<(&'a BTreeSet<Annotation>, &'a ClassExpression)>,
    facts: Vec<Fact<'a>>,
    same_as: Vec<(&'a BTreeSet<Annotation>, &'a Individual)>,
    different_from: Vec<(&'a BTreeSet<Annotation>, &'a Individual)>,
}

enum Fact<'a> {
    Obj(
        &'a BTreeSet<Annotation>,
        bool,
        &'a ObjectProperty,
        &'a Individual,
    ),
    Data(
        &'a BTreeSet<Annotation>,
        bool,
        &'a DataProperty,
        &'a Literal,
    ),
}

enum MiscFrame<'a> {
    EquivalentClasses(&'a BTreeSet<Annotation>, Vec<&'a ClassExpression>),
    DisjointClasses(&'a BTreeSet<Annotation>, Vec<&'a ClassExpression>),
    EquivalentObjectProperties(&'a BTreeSet<Annotation>, Vec<&'a ObjectPropertyExpression>),
    DisjointObjectProperties(&'a BTreeSet<Annotation>, Vec<&'a ObjectPropertyExpression>),
    EquivalentDataProperties(&'a BTreeSet<Annotation>, Vec<&'a DataProperty>),
    DisjointDataProperties(&'a BTreeSet<Annotation>, Vec<&'a DataProperty>),
    SameIndividual(&'a BTreeSet<Annotation>, Vec<&'a Individual>),
    DifferentIndividuals(&'a BTreeSet<Annotation>, Vec<&'a Individual>),
}

/// Whether `c` contains an n-ary data restriction (`DataSomeValuesFrom`/`DataAllValuesFrom`
/// with more than one data property), which Manchester syntax cannot represent.
fn has_nary_data_restriction(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::Class(_)
        | C::ObjectOneOf(_)
        | C::ObjectHasValue(..)
        | C::ObjectHasSelf(_)
        | C::DataHasValue(..)
        | C::DataMinCardinality(..)
        | C::DataMaxCardinality(..)
        | C::DataExactCardinality(..) => false,
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            cs.iter().any(has_nary_data_restriction)
        }
        C::ObjectComplementOf(x) | C::ObjectSomeValuesFrom(_, x) | C::ObjectAllValuesFrom(_, x) => {
            has_nary_data_restriction(x)
        }
        C::ObjectMinCardinality(_, _, f)
        | C::ObjectMaxCardinality(_, _, f)
        | C::ObjectExactCardinality(_, _, f) => f.as_deref().is_some_and(has_nary_data_restriction),
        C::DataSomeValuesFrom(ps, _) | C::DataAllValuesFrom(ps, _) => ps.len() != 1,
    }
}

fn axiom_has_nary_data(ax: &AxiomKind) -> bool {
    use AxiomKind as K;
    match ax {
        K::SubClassOf(a, b) => has_nary_data_restriction(a) || has_nary_data_restriction(b),
        K::EquivalentClasses(cs) => cs.iter().any(has_nary_data_restriction),
        K::DisjointClasses(cs) => cs.iter().any(has_nary_data_restriction),
        K::DisjointUnion(_, cs) => cs.iter().any(has_nary_data_restriction),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::HasKey(c, _, _)
        | K::ClassAssertion(c, _) => has_nary_data_restriction(c),
        _ => false,
    }
}

/// Whether `s` is a valid Manchester prefix name (`""` or a W3C `PN_PREFIX`).
fn is_manchester_prefix(s: &str) -> bool {
    s.is_empty() || crate::write_common::is_pn_prefix(s)
}

/// Whether `label` satisfies W3C SPARQL 1.1 `BLANK_NODE_LABEL` (after `_:`).
fn is_valid_anon_label(label: &str) -> bool {
    crate::write_common::is_blank_node_label(label)
}

fn ind_has_invalid_anon(i: &Individual) -> bool {
    matches!(i, Individual::Anonymous(a) if !is_valid_anon_label(a.label()))
}

fn ann_has_invalid_anon(a: &Annotation) -> bool {
    let val_bad =
        matches!(&a.value, AnnotationValue::Anonymous(an) if !is_valid_anon_label(an.label()));
    val_bad || a.annotations.iter().any(ann_has_invalid_anon)
}

fn ce_has_invalid_anon(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::Class(_)
        | C::ObjectHasSelf(_)
        | C::DataSomeValuesFrom(..)
        | C::DataAllValuesFrom(..)
        | C::DataHasValue(..)
        | C::DataMinCardinality(..)
        | C::DataMaxCardinality(..)
        | C::DataExactCardinality(..) => false,
        C::ObjectOneOf(is) => is.iter().any(ind_has_invalid_anon),
        C::ObjectHasValue(_, i) => ind_has_invalid_anon(i),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().any(ce_has_invalid_anon),
        C::ObjectComplementOf(x) | C::ObjectSomeValuesFrom(_, x) | C::ObjectAllValuesFrom(_, x) => {
            ce_has_invalid_anon(x)
        }
        C::ObjectMinCardinality(_, _, f)
        | C::ObjectMaxCardinality(_, _, f)
        | C::ObjectExactCardinality(_, _, f) => f.as_deref().is_some_and(ce_has_invalid_anon),
    }
}

fn axiom_has_invalid_anon(ax: &Axiom) -> bool {
    if ax.annotations.iter().any(ann_has_invalid_anon) {
        return true;
    }
    use AxiomKind as K;
    match &ax.kind {
        K::SubClassOf(a, b) => ce_has_invalid_anon(a) || ce_has_invalid_anon(b),
        K::EquivalentClasses(cs) => cs.iter().any(ce_has_invalid_anon),
        K::DisjointClasses(cs) => cs.iter().any(ce_has_invalid_anon),
        K::DisjointUnion(_, cs) => cs.iter().any(ce_has_invalid_anon),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::HasKey(c, _, _) => ce_has_invalid_anon(c),
        K::ClassAssertion(c, i) => ce_has_invalid_anon(c) || ind_has_invalid_anon(i),
        K::SameIndividual(is) => is.iter().any(ind_has_invalid_anon),
        K::DifferentIndividuals(is) => is.iter().any(ind_has_invalid_anon),
        K::ObjectPropertyAssertion(_, a, b) | K::NegativeObjectPropertyAssertion(_, a, b) => {
            ind_has_invalid_anon(a) || ind_has_invalid_anon(b)
        }
        K::DataPropertyAssertion(_, a, _) | K::NegativeDataPropertyAssertion(_, a, _) => {
            ind_has_invalid_anon(a)
        }
        K::AnnotationAssertion(_, s, v) => {
            matches!(s, AnnotationSubject::Anonymous(a) if !is_valid_anon_label(a.label()))
                || matches!(v, AnnotationValue::Anonymous(a) if !is_valid_anon_label(a.label()))
        }
        _ => false,
    }
}

fn is_builtin_manchester_datatype(iri: &str) -> bool {
    iri.starts_with(vocab::XSD)
        || matches!(
            iri,
            vocab::RDFS_LITERAL
                | vocab::RDF_PLAIN_LITERAL
                | vocab::RDF_XML_LITERAL
                | vocab::RDF_LANG_STRING
                | vocab::OWL_REAL
                | vocab::OWL_RATIONAL
        )
}

fn collect_dr_datatypes(r: &DataRange, dts: &mut BTreeSet<Datatype>) {
    match r {
        DataRange::Datatype(d) | DataRange::DatatypeRestriction(d, _) => {
            if !is_builtin_manchester_datatype(d.as_str()) {
                dts.insert(d.clone());
            }
        }
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
            for sub in rs {
                collect_dr_datatypes(sub, dts);
            }
        }
        DataRange::ComplementOf(sub) => collect_dr_datatypes(sub, dts),
        DataRange::OneOf(_) => {}
    }
}

#[derive(Default)]
struct AxiomEntityRefs {
    classes: BTreeSet<Class>,
    obj_props: BTreeSet<ObjectProperty>,
    data_props: BTreeSet<DataProperty>,
    datatypes: BTreeSet<Datatype>,
    individuals: BTreeSet<NamedIndividual>,
}

fn collect_ce_entity_refs(c: &ClassExpression, refs: &mut AxiomEntityRefs) {
    use ClassExpression as C;
    match c {
        C::Class(cl) => {
            if !Entity::Class(cl.clone()).is_builtin() {
                refs.classes.insert(cl.clone());
            }
        }
        C::ObjectOneOf(is) => {
            for i in is {
                if let Individual::Named(ni) = i {
                    refs.individuals.insert(ni.clone());
                }
            }
        }
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            for x in cs {
                collect_ce_entity_refs(x, refs);
            }
        }
        C::ObjectComplementOf(x) => collect_ce_entity_refs(x, refs),
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => {
            let np = p.named();
            if !Entity::ObjectProperty(np.clone()).is_builtin() {
                refs.obj_props.insert(np.clone());
            }
            collect_ce_entity_refs(f, refs);
        }
        C::ObjectHasValue(p, ind) => {
            let np = p.named();
            if !Entity::ObjectProperty(np.clone()).is_builtin() {
                refs.obj_props.insert(np.clone());
            }
            if let Individual::Named(ni) = ind {
                refs.individuals.insert(ni.clone());
            }
        }
        C::ObjectHasSelf(p) => {
            let np = p.named();
            if !Entity::ObjectProperty(np.clone()).is_builtin() {
                refs.obj_props.insert(np.clone());
            }
        }
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => {
            let np = p.named();
            if !Entity::ObjectProperty(np.clone()).is_builtin() {
                refs.obj_props.insert(np.clone());
            }
            if let Some(f) = f {
                collect_ce_entity_refs(f, refs);
            }
        }
        C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
            for p in ps {
                if !Entity::DataProperty(p.clone()).is_builtin() {
                    refs.data_props.insert(p.clone());
                }
            }
            collect_dr_datatypes(r, &mut refs.datatypes);
        }
        C::DataHasValue(p, _) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
        }
        C::DataMinCardinality(_, p, r)
        | C::DataMaxCardinality(_, p, r)
        | C::DataExactCardinality(_, p, r) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
            if let Some(r) = r {
                collect_dr_datatypes(r, &mut refs.datatypes);
            }
        }
    }
}

fn add_obj_prop_ref(p: &ObjectPropertyExpression, ops: &mut BTreeSet<ObjectProperty>) {
    let np = p.named();
    if !Entity::ObjectProperty(np.clone()).is_builtin() {
        ops.insert(np.clone());
    }
}

fn collect_axiom_entity_refs(ax: &AxiomKind, refs: &mut AxiomEntityRefs) {
    use AxiomKind as K;
    match ax {
        K::Declaration(Entity::Class(c)) => {
            if !Entity::Class(c.clone()).is_builtin() {
                refs.classes.insert(c.clone());
            }
        }
        K::Declaration(Entity::ObjectProperty(p)) => {
            if !Entity::ObjectProperty(p.clone()).is_builtin() {
                refs.obj_props.insert(p.clone());
            }
        }
        K::Declaration(Entity::DataProperty(p)) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
        }
        K::Declaration(Entity::Datatype(d)) => {
            if !is_builtin_manchester_datatype(d.as_str()) {
                refs.datatypes.insert(d.clone());
            }
        }
        K::Declaration(Entity::NamedIndividual(i)) => {
            refs.individuals.insert(i.clone());
        }
        K::Declaration(Entity::AnnotationProperty(_)) => {}
        K::SubClassOf(a, b) => {
            collect_ce_entity_refs(a, refs);
            collect_ce_entity_refs(b, refs);
        }
        K::EquivalentClasses(cs) => {
            for c in cs {
                collect_ce_entity_refs(c, refs);
            }
        }
        K::DisjointClasses(cs) => {
            for c in cs {
                collect_ce_entity_refs(c, refs);
            }
        }
        K::DisjointUnion(c, cs) => {
            if !Entity::Class(c.clone()).is_builtin() {
                refs.classes.insert(c.clone());
            }
            for x in cs {
                collect_ce_entity_refs(x, refs);
            }
        }
        K::SubObjectPropertyOf(sub, sup) => {
            match sub {
                SubObjectPropertyExpression::Expression(p) => {
                    add_obj_prop_ref(p, &mut refs.obj_props);
                }
                SubObjectPropertyExpression::Chain(chain) => {
                    for p in chain {
                        add_obj_prop_ref(p, &mut refs.obj_props);
                    }
                }
            }
            add_obj_prop_ref(sup, &mut refs.obj_props);
        }
        K::EquivalentObjectProperties(ps) => {
            for p in ps {
                add_obj_prop_ref(p, &mut refs.obj_props);
            }
        }
        K::DisjointObjectProperties(ps) => {
            for p in ps {
                add_obj_prop_ref(p, &mut refs.obj_props);
            }
        }
        K::InverseObjectProperties(a, b) => {
            add_obj_prop_ref(a, &mut refs.obj_props);
            add_obj_prop_ref(b, &mut refs.obj_props);
        }
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            add_obj_prop_ref(p, &mut refs.obj_props);
            collect_ce_entity_refs(c, refs);
        }
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => add_obj_prop_ref(p, &mut refs.obj_props),
        K::SubDataPropertyOf(a, b) => {
            if !Entity::DataProperty(a.clone()).is_builtin() {
                refs.data_props.insert(a.clone());
            }
            if !Entity::DataProperty(b.clone()).is_builtin() {
                refs.data_props.insert(b.clone());
            }
        }
        K::EquivalentDataProperties(ps) => {
            for p in ps {
                if !Entity::DataProperty(p.clone()).is_builtin() {
                    refs.data_props.insert(p.clone());
                }
            }
        }
        K::DisjointDataProperties(ps) => {
            for p in ps {
                if !Entity::DataProperty(p.clone()).is_builtin() {
                    refs.data_props.insert(p.clone());
                }
            }
        }
        K::DataPropertyDomain(p, c) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
            collect_ce_entity_refs(c, refs);
        }
        K::DataPropertyRange(p, dr) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
            collect_dr_datatypes(dr, &mut refs.datatypes);
        }
        K::FunctionalDataProperty(p) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
        }
        K::DatatypeDefinition(_, dr) => collect_dr_datatypes(dr, &mut refs.datatypes),
        K::HasKey(c, key_ops, key_dps) => {
            collect_ce_entity_refs(c, refs);
            for p in key_ops {
                add_obj_prop_ref(p, &mut refs.obj_props);
            }
            for p in key_dps {
                if !Entity::DataProperty(p.clone()).is_builtin() {
                    refs.data_props.insert(p.clone());
                }
            }
        }
        K::ClassAssertion(c, i) => {
            collect_ce_entity_refs(c, refs);
            if let Individual::Named(ni) = i {
                refs.individuals.insert(ni.clone());
            }
        }
        K::SameIndividual(is) => {
            for i in is {
                if let Individual::Named(ni) = i {
                    refs.individuals.insert(ni.clone());
                }
            }
        }
        K::DifferentIndividuals(is) => {
            for i in is {
                if let Individual::Named(ni) = i {
                    refs.individuals.insert(ni.clone());
                }
            }
        }
        K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
            add_obj_prop_ref(p, &mut refs.obj_props);
            if let Individual::Named(ni) = a {
                refs.individuals.insert(ni.clone());
            }
            if let Individual::Named(ni) = b {
                refs.individuals.insert(ni.clone());
            }
        }
        K::DataPropertyAssertion(p, a, _) | K::NegativeDataPropertyAssertion(p, a, _) => {
            if !Entity::DataProperty(p.clone()).is_builtin() {
                refs.data_props.insert(p.clone());
            }
            if let Individual::Named(ni) = a {
                refs.individuals.insert(ni.clone());
            }
        }
        _ => {}
    }
}

fn dr_leading_datatype_punned_with_class(r: &DataRange, class_iris: &BTreeSet<Iri>) -> bool {
    match r {
        DataRange::Datatype(d) => class_iris.contains(&d.0),
        DataRange::ComplementOf(inner) => dr_leading_datatype_punned_with_class(inner, class_iris),
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => rs
            .iter()
            .next()
            .is_some_and(|first| dr_leading_datatype_punned_with_class(first, class_iris)),
        DataRange::OneOf(_) | DataRange::DatatypeRestriction(..) => false,
    }
}

fn ce_has_punned_property_ambiguity(
    c: &ClassExpression,
    obj_iris: &BTreeSet<Iri>,
    data_iris: &BTreeSet<Iri>,
    class_iris: &BTreeSet<Iri>,
) -> bool {
    use ClassExpression as C;
    match c {
        C::Class(_)
        | C::ObjectOneOf(_)
        | C::ObjectHasValue(..)
        | C::ObjectHasSelf(_)
        | C::DataHasValue(..) => false,
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs
            .iter()
            .any(|x| ce_has_punned_property_ambiguity(x, obj_iris, data_iris, class_iris)),
        C::ObjectComplementOf(x) | C::ObjectSomeValuesFrom(_, x) | C::ObjectAllValuesFrom(_, x) => {
            ce_has_punned_property_ambiguity(x, obj_iris, data_iris, class_iris)
        }
        C::ObjectMinCardinality(_, p, None)
        | C::ObjectMaxCardinality(_, p, None)
        | C::ObjectExactCardinality(_, p, None) => {
            matches!(p, ObjectPropertyExpression::Property(op) if data_iris.contains(&op.0))
        }
        C::ObjectMinCardinality(_, _, Some(f))
        | C::ObjectMaxCardinality(_, _, Some(f))
        | C::ObjectExactCardinality(_, _, Some(f)) => {
            ce_has_punned_property_ambiguity(f, obj_iris, data_iris, class_iris)
        }
        C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
            ps.iter().any(|p| obj_iris.contains(&p.0))
                && dr_leading_datatype_punned_with_class(r, class_iris)
        }
        C::DataMinCardinality(_, p, None)
        | C::DataMaxCardinality(_, p, None)
        | C::DataExactCardinality(_, p, None) => obj_iris.contains(&p.0),
        C::DataMinCardinality(_, p, Some(r))
        | C::DataMaxCardinality(_, p, Some(r))
        | C::DataExactCardinality(_, p, Some(r)) => {
            obj_iris.contains(&p.0) && dr_leading_datatype_punned_with_class(r, class_iris)
        }
    }
}

fn axiom_has_punned_property_ambiguity(
    ax: &AxiomKind,
    obj_iris: &BTreeSet<Iri>,
    data_iris: &BTreeSet<Iri>,
    class_iris: &BTreeSet<Iri>,
) -> bool {
    use AxiomKind as K;
    match ax {
        K::SubClassOf(a, b) => {
            ce_has_punned_property_ambiguity(a, obj_iris, data_iris, class_iris)
                || ce_has_punned_property_ambiguity(b, obj_iris, data_iris, class_iris)
        }
        K::EquivalentClasses(cs) => cs
            .iter()
            .any(|c| ce_has_punned_property_ambiguity(c, obj_iris, data_iris, class_iris)),
        K::DisjointClasses(cs) => cs
            .iter()
            .any(|c| ce_has_punned_property_ambiguity(c, obj_iris, data_iris, class_iris)),
        K::DisjointUnion(_, cs) => cs
            .iter()
            .any(|c| ce_has_punned_property_ambiguity(c, obj_iris, data_iris, class_iris)),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::ClassAssertion(c, _) => {
            ce_has_punned_property_ambiguity(c, obj_iris, data_iris, class_iris)
        }
        K::HasKey(c, ops, dps) => {
            ce_has_punned_property_ambiguity(c, obj_iris, data_iris, class_iris)
                || dps.iter().any(|dp| obj_iris.contains(&dp.0))
                || ops
                    .iter()
                    .any(|op| matches!(op, ObjectPropertyExpression::Property(p) if data_iris.contains(&p.0)))
        }
        K::EquivalentDataProperties(ps) if ps.len() > 2 => {
            ps.iter().any(|dp| obj_iris.contains(&dp.0))
        }
        K::DisjointDataProperties(ps) if ps.len() > 2 || ps.has_duplicates() => {
            ps.iter().any(|dp| obj_iris.contains(&dp.0))
        }
        K::EquivalentObjectProperties(ps) if ps.len() > 2 => ps
            .iter()
            .all(|op| matches!(op, ObjectPropertyExpression::Property(p) if data_iris.contains(&p.0))),
        K::DisjointObjectProperties(ps) if ps.len() > 2 || ps.has_duplicates() => ps
            .iter()
            .all(|op| matches!(op, ObjectPropertyExpression::Property(p) if data_iris.contains(&p.0))),
        _ => false,
    }
}

/// Write an ontology header and axioms as a Manchester OWL Syntax (`.omn`) document.
pub fn write_document<'a>(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
    options: WriteOptions,
) -> Result<WriteReport, WriteError> {
    let axioms: Vec<&'a Axiom> = axioms.into_iter().collect();
    check_header_depth(header, "Manchester")?;

    // Filter and deduplicate prefix declarations to valid Manchester PN_PREFIX names and
    // retain the W3C Manchester reserved standard bindings (rdf, rdfs, xsd, owl).
    let mut valid_prefixes: BTreeMap<String, String> = BTreeMap::new();
    for (p, n) in ofn.prefixes() {
        if is_manchester_prefix(p)
            && !n.is_empty()
            && !STANDARD_PREFIXES
                .iter()
                .any(|(sp, sn)| *sp == p && *sn != n)
        {
            valid_prefixes.insert(p.to_string(), n.to_string());
        }
    }
    let clean_ofn = OfnRenderer::new(valid_prefixes.iter().map(|(p, n)| (p.as_str(), n.as_str())));
    let r = Renderer::new(valid_prefixes.iter().map(|(p, n)| (p.as_str(), n.as_str())));
    let ofn = &clean_ofn;
    let mut report = WriteReport::default();

    let mut kept_header_annotations: Vec<&Annotation> = Vec::new();
    for a in &header.annotations {
        if ann_has_invalid_anon(a) {
            report.skip_header_or_err(
                options,
                ofn,
                a,
                "Manchester syntax cannot represent invalid anonymous individual labels in header annotations",
            )?;
        } else {
            kept_header_annotations.push(a);
        }
    }

    let mut classes: BTreeMap<Class, ClassFrame<'a>> = BTreeMap::new();
    let mut datatypes: BTreeMap<Datatype, DatatypeFrame<'a>> = BTreeMap::new();
    let mut obj_props: BTreeMap<ObjectProperty, ObjectPropertyFrame<'a>> = BTreeMap::new();
    let mut data_props: BTreeMap<DataProperty, DataPropertyFrame<'a>> = BTreeMap::new();
    let mut ann_props: BTreeMap<AnnotationProperty, AnnotationPropertyFrame<'a>> = BTreeMap::new();
    let mut individuals: BTreeMap<Individual, IndividualFrame<'a>> = BTreeMap::new();
    let mut misc: Vec<MiscFrame<'a>> = Vec::new();
    let mut pending_ann_assertions: Vec<&'a Axiom> = Vec::new();
    let mut all_refs = AxiomEntityRefs::default();

    // First pass: check depth, record all entity kind IRIs, and collect declarations.
    for &ax in &axioms {
        check_axiom_depth(ax, "Manchester")?;
        collect_axiom_entity_refs(&ax.kind, &mut all_refs);
        if let AxiomKind::Declaration(e) = &ax.kind {
            if !ax.annotations.is_empty() {
                if options.strict {
                    return Err(WriteError::Unsupported(format!(
                        "Manchester syntax cannot represent annotations on Declaration: {}",
                        ofn.axiom(ax)
                    )));
                }
                report.record_skipped(
                    ax,
                    "Manchester syntax cannot attach annotations to Declaration axioms",
                );
            }
            match e {
                Entity::Class(c) => {
                    classes.entry(c.clone()).or_default();
                }
                Entity::Datatype(d) => {
                    datatypes.entry(d.clone()).or_default();
                }
                Entity::ObjectProperty(p) => {
                    obj_props.entry(p.clone()).or_default();
                }
                Entity::DataProperty(p) => {
                    data_props.entry(p.clone()).or_default();
                }
                Entity::AnnotationProperty(p) => {
                    ann_props.entry(p.clone()).or_default().declared = true;
                }
                Entity::NamedIndividual(i) => {
                    individuals.entry(Individual::Named(i.clone())).or_default();
                }
            }
            if ax.annotations.is_empty() {
                report.written_axioms += 1;
            }
        }
    }

    let all_obj_prop_iris: BTreeSet<Iri> = all_refs.obj_props.iter().map(|p| p.0.clone()).collect();
    let all_data_prop_iris: BTreeSet<Iri> =
        all_refs.data_props.iter().map(|p| p.0.clone()).collect();
    let all_class_iris: BTreeSet<Iri> = all_refs.classes.iter().map(|c| c.0.clone()).collect();

    // Second pass: classify remaining axioms into entity frames or misc frames.
    let mut written_refs = AxiomEntityRefs::default();
    for &ax in &axioms {
        if matches!(ax.kind, AxiomKind::Declaration(_)) {
            continue;
        }
        if axiom_has_invalid_anon(ax) {
            report.skip_or_err(
                options,
                ofn,
                ax,
                "Manchester syntax cannot represent invalid anonymous individual labels",
            )?;
            continue;
        }
        if axiom_has_nary_data(&ax.kind) {
            report.skip_or_err(
                options,
                ofn,
                ax,
                "Manchester syntax does not support n-ary data restrictions",
            )?;
            continue;
        }
        if axiom_has_punned_property_ambiguity(
            &ax.kind,
            &all_obj_prop_iris,
            &all_data_prop_iris,
            &all_class_iris,
        ) {
            report.skip_or_err(
                options,
                ofn,
                ax,
                "Manchester syntax cannot disambiguate an IRI punned as both ObjectProperty and DataProperty in this axiom",
            )?;
            continue;
        }
        let written_before = report.written_axioms;
        use AxiomKind as K;
        match &ax.kind {
            K::Declaration(_) => unreachable!(),
            K::SubClassOf(ClassExpression::Class(c), sup) => {
                classes
                    .entry(c.clone())
                    .or_default()
                    .sub_class_of
                    .push((&ax.annotations, sup));
                report.written_axioms += 1;
            }
            K::SubClassOf(_, _) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester frame syntax cannot represent general class inclusions with an anonymous subclass",
                )?;
            }
            K::EquivalentClasses(cs) => {
                if cs.len() == 2 {
                    let items: Vec<&ClassExpression> = cs.iter().collect();
                    if let ClassExpression::Class(c) = items[0] {
                        classes
                            .entry(c.clone())
                            .or_default()
                            .equivalent_to
                            .push((&ax.annotations, items[1]));
                    } else if let ClassExpression::Class(c) = items[1] {
                        classes
                            .entry(c.clone())
                            .or_default()
                            .equivalent_to
                            .push((&ax.annotations, items[0]));
                    } else {
                        misc.push(MiscFrame::EquivalentClasses(&ax.annotations, items));
                    }
                    report.written_axioms += 1;
                } else if cs.len() == 1 {
                    let item = cs.iter().next().expect("len 1");
                    if let ClassExpression::Class(c) = item {
                        classes
                            .entry(c.clone())
                            .or_default()
                            .equivalent_to
                            .push((&ax.annotations, item));
                    } else {
                        misc.push(MiscFrame::EquivalentClasses(
                            &ax.annotations,
                            vec![item, item],
                        ));
                    }
                    report.written_axioms += 1;
                } else {
                    misc.push(MiscFrame::EquivalentClasses(
                        &ax.annotations,
                        cs.iter().collect(),
                    ));
                    report.written_axioms += 1;
                }
            }
            K::DisjointClasses(cs) => {
                if cs.len() == 2 && !cs.has_duplicates() {
                    let items: Vec<&ClassExpression> = cs.iter().collect();
                    if let ClassExpression::Class(c) = items[0] {
                        classes
                            .entry(c.clone())
                            .or_default()
                            .disjoint_with
                            .push((&ax.annotations, items[1]));
                    } else if let ClassExpression::Class(c) = items[1] {
                        classes
                            .entry(c.clone())
                            .or_default()
                            .disjoint_with
                            .push((&ax.annotations, items[0]));
                    } else {
                        misc.push(MiscFrame::DisjointClasses(&ax.annotations, items));
                    }
                } else {
                    misc.push(MiscFrame::DisjointClasses(
                        &ax.annotations,
                        cs.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::DisjointUnion(c, cs) => {
                classes
                    .entry(c.clone())
                    .or_default()
                    .disjoint_union_of
                    .push((&ax.annotations, cs));
                report.written_axioms += 1;
            }
            K::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ObjectPropertyExpression::Property(sub)),
                sup,
            ) => {
                obj_props
                    .entry(sub.clone())
                    .or_default()
                    .sub_property_of
                    .push((&ax.annotations, sup));
                report.written_axioms += 1;
            }
            K::SubObjectPropertyOf(
                SubObjectPropertyExpression::Chain(chain),
                ObjectPropertyExpression::Property(sup),
            ) => {
                obj_props
                    .entry(sup.clone())
                    .or_default()
                    .sub_property_chain
                    .push((&ax.annotations, chain.as_slice()));
                report.written_axioms += 1;
            }
            K::SubObjectPropertyOf(_, _) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester syntax cannot represent SubObjectPropertyOf with an inverse property head",
                )?;
            }
            K::EquivalentObjectProperties(ps) => {
                if ps.len() == 2 {
                    let items: Vec<&ObjectPropertyExpression> = ps.iter().collect();
                    if let ObjectPropertyExpression::Property(p) = items[0] {
                        obj_props
                            .entry(p.clone())
                            .or_default()
                            .equivalent_to
                            .push((&ax.annotations, items[1]));
                    } else if let ObjectPropertyExpression::Property(p) = items[1] {
                        obj_props
                            .entry(p.clone())
                            .or_default()
                            .equivalent_to
                            .push((&ax.annotations, items[0]));
                    } else {
                        misc.push(MiscFrame::EquivalentObjectProperties(
                            &ax.annotations,
                            items,
                        ));
                    }
                } else if ps.len() == 1 {
                    let item = ps.iter().next().expect("len 1");
                    if let ObjectPropertyExpression::Property(p) = item {
                        obj_props
                            .entry(p.clone())
                            .or_default()
                            .equivalent_to
                            .push((&ax.annotations, item));
                    } else {
                        misc.push(MiscFrame::EquivalentObjectProperties(
                            &ax.annotations,
                            vec![item, item],
                        ));
                    }
                } else {
                    misc.push(MiscFrame::EquivalentObjectProperties(
                        &ax.annotations,
                        ps.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::DisjointObjectProperties(ps) => {
                if ps.len() == 2 && !ps.has_duplicates() {
                    let items: Vec<&ObjectPropertyExpression> = ps.iter().collect();
                    if let ObjectPropertyExpression::Property(p) = items[0] {
                        obj_props
                            .entry(p.clone())
                            .or_default()
                            .disjoint_with
                            .push((&ax.annotations, items[1]));
                    } else if let ObjectPropertyExpression::Property(p) = items[1] {
                        obj_props
                            .entry(p.clone())
                            .or_default()
                            .disjoint_with
                            .push((&ax.annotations, items[0]));
                    } else {
                        misc.push(MiscFrame::DisjointObjectProperties(&ax.annotations, items));
                    }
                } else {
                    misc.push(MiscFrame::DisjointObjectProperties(
                        &ax.annotations,
                        ps.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::InverseObjectProperties(
                ObjectPropertyExpression::Property(a),
                ObjectPropertyExpression::Property(b),
            ) => {
                obj_props
                    .entry(a.clone())
                    .or_default()
                    .inverse_of
                    .push((&ax.annotations, b));
                report.written_axioms += 1;
            }
            K::InverseObjectProperties(_, _) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester syntax cannot represent InverseObjectProperties over ObjectInverseOf",
                )?;
            }
            K::ObjectPropertyDomain(ObjectPropertyExpression::Property(p), c) => {
                obj_props
                    .entry(p.clone())
                    .or_default()
                    .domain
                    .push((&ax.annotations, c));
                report.written_axioms += 1;
            }
            K::ObjectPropertyRange(ObjectPropertyExpression::Property(p), c) => {
                obj_props
                    .entry(p.clone())
                    .or_default()
                    .range
                    .push((&ax.annotations, c));
                report.written_axioms += 1;
            }
            K::ObjectPropertyDomain(_, _) | K::ObjectPropertyRange(_, _) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester syntax cannot represent Domain/Range on an inverse object property",
                )?;
            }
            K::FunctionalObjectProperty(ObjectPropertyExpression::Property(p))
            | K::InverseFunctionalObjectProperty(ObjectPropertyExpression::Property(p))
            | K::ReflexiveObjectProperty(ObjectPropertyExpression::Property(p))
            | K::IrreflexiveObjectProperty(ObjectPropertyExpression::Property(p))
            | K::SymmetricObjectProperty(ObjectPropertyExpression::Property(p))
            | K::AsymmetricObjectProperty(ObjectPropertyExpression::Property(p))
            | K::TransitiveObjectProperty(ObjectPropertyExpression::Property(p)) => {
                let word = match &ax.kind {
                    K::FunctionalObjectProperty(_) => "Functional",
                    K::InverseFunctionalObjectProperty(_) => "InverseFunctional",
                    K::ReflexiveObjectProperty(_) => "Reflexive",
                    K::IrreflexiveObjectProperty(_) => "Irreflexive",
                    K::SymmetricObjectProperty(_) => "Symmetric",
                    K::AsymmetricObjectProperty(_) => "Asymmetric",
                    K::TransitiveObjectProperty(_) => "Transitive",
                    _ => unreachable!(),
                };
                obj_props
                    .entry(p.clone())
                    .or_default()
                    .characteristics
                    .push((&ax.annotations, word));
                report.written_axioms += 1;
            }
            K::FunctionalObjectProperty(_)
            | K::InverseFunctionalObjectProperty(_)
            | K::ReflexiveObjectProperty(_)
            | K::IrreflexiveObjectProperty(_)
            | K::SymmetricObjectProperty(_)
            | K::AsymmetricObjectProperty(_)
            | K::TransitiveObjectProperty(_) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester syntax cannot represent property characteristics on an inverse property",
                )?;
            }
            K::SubDataPropertyOf(a, b) => {
                data_props
                    .entry(a.clone())
                    .or_default()
                    .sub_property_of
                    .push((&ax.annotations, b));
                report.written_axioms += 1;
            }
            K::EquivalentDataProperties(ps) => {
                if ps.len() == 2 {
                    let mut it = ps.iter();
                    let a = it.next().expect("first");
                    let b = it.next().expect("second");
                    data_props
                        .entry(a.clone())
                        .or_default()
                        .equivalent_to
                        .push((&ax.annotations, b));
                } else if ps.len() == 1 {
                    let a = ps.iter().next().expect("first");
                    data_props
                        .entry(a.clone())
                        .or_default()
                        .equivalent_to
                        .push((&ax.annotations, a));
                } else {
                    misc.push(MiscFrame::EquivalentDataProperties(
                        &ax.annotations,
                        ps.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::DisjointDataProperties(ps) => {
                if ps.len() == 2 && !ps.has_duplicates() {
                    let mut it = ps.iter();
                    let a = it.next().expect("first");
                    let b = it.next().expect("second");
                    data_props
                        .entry(a.clone())
                        .or_default()
                        .disjoint_with
                        .push((&ax.annotations, b));
                } else {
                    misc.push(MiscFrame::DisjointDataProperties(
                        &ax.annotations,
                        ps.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::DataPropertyDomain(p, c) => {
                data_props
                    .entry(p.clone())
                    .or_default()
                    .domain
                    .push((&ax.annotations, c));
                report.written_axioms += 1;
            }
            K::DataPropertyRange(p, dr) => {
                data_props
                    .entry(p.clone())
                    .or_default()
                    .range
                    .push((&ax.annotations, dr));
                report.written_axioms += 1;
            }
            K::FunctionalDataProperty(p) => {
                data_props
                    .entry(p.clone())
                    .or_default()
                    .functional
                    .push(&ax.annotations);
                report.written_axioms += 1;
            }
            K::DatatypeDefinition(d, dr) => {
                datatypes
                    .entry(d.clone())
                    .or_default()
                    .equivalent_to
                    .push((&ax.annotations, dr));
                report.written_axioms += 1;
            }
            K::HasKey(ClassExpression::Class(c), ops, dps) => {
                classes
                    .entry(c.clone())
                    .or_default()
                    .has_key
                    .push((&ax.annotations, ops, dps));
                report.written_axioms += 1;
            }
            K::HasKey(_, _, _) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester syntax cannot represent HasKey on an anonymous class expression",
                )?;
            }
            K::SameIndividual(is) => {
                if is.len() == 2 {
                    let mut it = is.iter();
                    let a = it.next().expect("first");
                    let b = it.next().expect("second");
                    individuals
                        .entry(a.clone())
                        .or_default()
                        .same_as
                        .push((&ax.annotations, b));
                } else if is.len() == 1 {
                    let a = is.iter().next().expect("first");
                    individuals
                        .entry(a.clone())
                        .or_default()
                        .same_as
                        .push((&ax.annotations, a));
                } else {
                    misc.push(MiscFrame::SameIndividual(
                        &ax.annotations,
                        is.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::DifferentIndividuals(is) => {
                if is.len() == 2 && !is.has_duplicates() {
                    let mut it = is.iter();
                    let a = it.next().expect("first");
                    let b = it.next().expect("second");
                    individuals
                        .entry(a.clone())
                        .or_default()
                        .different_from
                        .push((&ax.annotations, b));
                } else {
                    misc.push(MiscFrame::DifferentIndividuals(
                        &ax.annotations,
                        is.iter().collect(),
                    ));
                }
                report.written_axioms += 1;
            }
            K::ClassAssertion(c, i) => {
                individuals
                    .entry(i.clone())
                    .or_default()
                    .types
                    .push((&ax.annotations, c));
                report.written_axioms += 1;
            }
            K::ObjectPropertyAssertion(ObjectPropertyExpression::Property(p), a, b) => {
                individuals
                    .entry(a.clone())
                    .or_default()
                    .facts
                    .push(Fact::Obj(&ax.annotations, false, p, b));
                report.written_axioms += 1;
            }
            K::NegativeObjectPropertyAssertion(ObjectPropertyExpression::Property(p), a, b) => {
                individuals
                    .entry(a.clone())
                    .or_default()
                    .facts
                    .push(Fact::Obj(&ax.annotations, true, p, b));
                report.written_axioms += 1;
            }
            K::ObjectPropertyAssertion(_, _, _) | K::NegativeObjectPropertyAssertion(_, _, _) => {
                report.skip_or_err(
                    options,
                    ofn,
                    ax,
                    "Manchester syntax cannot represent object property facts on an inverse property",
                )?;
            }
            K::DataPropertyAssertion(p, a, l) => {
                individuals
                    .entry(a.clone())
                    .or_default()
                    .facts
                    .push(Fact::Data(&ax.annotations, false, p, l));
                report.written_axioms += 1;
            }
            K::NegativeDataPropertyAssertion(p, a, l) => {
                individuals
                    .entry(a.clone())
                    .or_default()
                    .facts
                    .push(Fact::Data(&ax.annotations, true, p, l));
                report.written_axioms += 1;
            }
            K::AnnotationAssertion(..) => {
                pending_ann_assertions.push(ax);
            }
            K::SubAnnotationPropertyOf(a, b) => {
                ann_props
                    .entry(a.clone())
                    .or_default()
                    .sub_property_of
                    .push((&ax.annotations, b));
                report.written_axioms += 1;
            }
            K::AnnotationPropertyDomain(p, i) => {
                ann_props
                    .entry(p.clone())
                    .or_default()
                    .domain
                    .push((&ax.annotations, i));
                report.written_axioms += 1;
            }
            K::AnnotationPropertyRange(p, i) => {
                ann_props
                    .entry(p.clone())
                    .or_default()
                    .range
                    .push((&ax.annotations, i));
                report.written_axioms += 1;
            }
        }
        if report.written_axioms > written_before {
            collect_axiom_entity_refs(&ax.kind, &mut written_refs);
        }
    }

    // Ensure every referenced DataProperty and non-builtin Datatype has an entity frame
    // so Manchester consumers never misclassify undeclared data properties as object
    // properties (e.g. in unqualified data cardinalities, HasKey, or n-ary misc frames).
    for dp in written_refs.data_props {
        data_props.entry(dp).or_default();
    }
    for dt in written_refs.datatypes {
        datatypes.entry(dt).or_default();
    }
    // Synthesize matching frames for referenced ObjectProperties, Classes and
    // NamedIndividuals whenever their IRI collides with another emitted frame kind so
    // the reader's vocabulary never shadows or rejects unambiguous punned expressions.
    for op in written_refs.obj_props {
        let iri = &op.0;
        let collides = data_props.contains_key(&DataProperty(iri.clone()))
            || classes.contains_key(&Class(iri.clone()))
            || datatypes.contains_key(&Datatype(iri.clone()))
            || ann_props.contains_key(&AnnotationProperty(iri.clone()))
            || individuals.contains_key(&Individual::Named(NamedIndividual(iri.clone())));
        if collides {
            obj_props.entry(op).or_default();
        }
    }
    for c in written_refs.classes {
        let iri = &c.0;
        let collides = datatypes.contains_key(&Datatype(iri.clone()))
            || obj_props.contains_key(&ObjectProperty(iri.clone()))
            || data_props.contains_key(&DataProperty(iri.clone()))
            || ann_props.contains_key(&AnnotationProperty(iri.clone()))
            || individuals.contains_key(&Individual::Named(NamedIndividual(iri.clone())));
        if collides {
            classes.entry(c).or_default();
        }
    }
    for ind in written_refs.individuals {
        let iri = &ind.0;
        let collides = classes.contains_key(&Class(iri.clone()))
            || datatypes.contains_key(&Datatype(iri.clone()))
            || obj_props.contains_key(&ObjectProperty(iri.clone()))
            || data_props.contains_key(&DataProperty(iri.clone()))
            || ann_props.contains_key(&AnnotationProperty(iri.clone()));
        if collides {
            individuals.entry(Individual::Named(ind)).or_default();
        }
    }

    // Attach AnnotationAssertions to an existing frame for their subject, or to an
    // anonymous Individual frame (which does not emit a Declaration).
    for ax in pending_ann_assertions {
        let AxiomKind::AnnotationAssertion(_, subj, _) = &ax.kind else {
            unreachable!()
        };
        match subj {
            AnnotationSubject::Anonymous(a) => {
                individuals
                    .entry(Individual::Anonymous(a.clone()))
                    .or_default()
                    .annotations
                    .push(ax);
                report.written_axioms += 1;
            }
            AnnotationSubject::Iri(iri) => {
                if let Some(f) = classes.get_mut(&Class(iri.clone())) {
                    f.annotations.push(ax);
                    report.written_axioms += 1;
                } else if let Some(f) = obj_props.get_mut(&ObjectProperty(iri.clone())) {
                    f.annotations.push(ax);
                    report.written_axioms += 1;
                } else if let Some(f) = data_props.get_mut(&DataProperty(iri.clone())) {
                    f.annotations.push(ax);
                    report.written_axioms += 1;
                } else if let Some(f) = ann_props.get_mut(&AnnotationProperty(iri.clone())) {
                    f.annotations.push(ax);
                    report.written_axioms += 1;
                } else if let Some(f) = datatypes.get_mut(&Datatype(iri.clone())) {
                    f.annotations.push(ax);
                    report.written_axioms += 1;
                } else if let Some(f) =
                    individuals.get_mut(&Individual::Named(NamedIndividual(iri.clone())))
                {
                    f.annotations.push(ax);
                    report.written_axioms += 1;
                } else if options.strict {
                    return Err(WriteError::Unsupported(format!(
                        "Manchester syntax has no frame for undeclared annotation subject <{}>: {}",
                        iri.as_str(),
                        ofn.axiom(ax)
                    )));
                } else {
                    report.record_skipped(
                        ax,
                        "Manchester syntax cannot represent AnnotationAssertion on an IRI without an entity frame",
                    );
                }
            }
        }
    }

    // Stream the document out in deterministic order.
    for (p, n) in ofn.prefixes() {
        writeln!(w, "Prefix: {p}: <{n}>")?;
    }
    writeln!(w)?;
    write!(w, "Ontology:")?;
    if let Some(iri) = &header.iri {
        write!(w, " <{}>", iri.as_str())?;
        if let Some(ver) = &header.version_iri {
            write!(w, " <{}>", ver.as_str())?;
        }
    }
    writeln!(w)?;
    for imp in &header.imports {
        writeln!(w, "Import: <{}>", imp.as_str())?;
    }
    if !kept_header_annotations.is_empty() {
        writeln!(w, "\nAnnotations:")?;
        write_annotation_list(w, ofn, &r, &kept_header_annotations, 4)?;
        writeln!(w)?;
    }

    for (p, f) in &ann_props {
        if !f.declared
            && f.annotations.is_empty()
            && f.domain.is_empty()
            && f.range.is_empty()
            && f.sub_property_of.is_empty()
        {
            continue;
        }
        writeln!(w, "\nAnnotationProperty: {}", ofn.iri(p.as_str()))?;
        write_frame_annotations(w, ofn, &r, &f.annotations)?;
        for (anns, d) in &f.domain {
            write_section_item(w, ofn, &r, "Domain:", anns, &ofn.iri(d.as_str()))?;
        }
        for (anns, rng) in &f.range {
            write_section_item(w, ofn, &r, "Range:", anns, &ofn.iri(rng.as_str()))?;
        }
        for (anns, sup) in &f.sub_property_of {
            write_section_item(w, ofn, &r, "SubPropertyOf:", anns, &ofn.iri(sup.as_str()))?;
        }
    }

    for (d, f) in &datatypes {
        writeln!(w, "\nDatatype: {}", ofn.iri(d.as_str()))?;
        write_frame_annotations(w, ofn, &r, &f.annotations)?;
        for (anns, dr) in &f.equivalent_to {
            write_section_item(w, ofn, &r, "EquivalentTo:", anns, &r.data_range(dr))?;
        }
    }

    for (p, f) in &obj_props {
        writeln!(w, "\nObjectProperty: {}", ofn.iri(p.as_str()))?;
        write_frame_annotations(w, ofn, &r, &f.annotations)?;
        for (anns, c) in &f.domain {
            write_section_item(w, ofn, &r, "Domain:", anns, &r.class_expression(c))?;
        }
        for (anns, c) in &f.range {
            write_section_item(w, ofn, &r, "Range:", anns, &r.class_expression(c))?;
        }
        for (anns, ch) in &f.characteristics {
            write_section_item(w, ofn, &r, "Characteristics:", anns, ch)?;
        }
        for (anns, sup) in &f.sub_property_of {
            write_section_item(w, ofn, &r, "SubPropertyOf:", anns, &r.ope(sup))?;
        }
        for (anns, eq) in &f.equivalent_to {
            write_section_item(w, ofn, &r, "EquivalentTo:", anns, &r.ope(eq))?;
        }
        for (anns, dj) in &f.disjoint_with {
            write_section_item(w, ofn, &r, "DisjointWith:", anns, &r.ope(dj))?;
        }
        for (anns, inv) in &f.inverse_of {
            write_section_item(w, ofn, &r, "InverseOf:", anns, &ofn.iri(inv.as_str()))?;
        }
        for (anns, chain) in &f.sub_property_chain {
            let expr = chain
                .iter()
                .map(|x| r.ope(x))
                .collect::<Vec<_>>()
                .join(" o ");
            write_section_item(w, ofn, &r, "SubPropertyChain:", anns, &expr)?;
        }
    }

    for (p, f) in &data_props {
        writeln!(w, "\nDataProperty: {}", ofn.iri(p.as_str()))?;
        write_frame_annotations(w, ofn, &r, &f.annotations)?;
        for (anns, c) in &f.domain {
            write_section_item(w, ofn, &r, "Domain:", anns, &r.class_expression(c))?;
        }
        for (anns, dr) in &f.range {
            write_section_item(w, ofn, &r, "Range:", anns, &r.data_range(dr))?;
        }
        for anns in &f.functional {
            write_section_item(w, ofn, &r, "Characteristics:", anns, "Functional")?;
        }
        for (anns, sup) in &f.sub_property_of {
            write_section_item(w, ofn, &r, "SubPropertyOf:", anns, &ofn.iri(sup.as_str()))?;
        }
        for (anns, eq) in &f.equivalent_to {
            write_section_item(w, ofn, &r, "EquivalentTo:", anns, &ofn.iri(eq.as_str()))?;
        }
        for (anns, dj) in &f.disjoint_with {
            write_section_item(w, ofn, &r, "DisjointWith:", anns, &ofn.iri(dj.as_str()))?;
        }
    }

    for (c, f) in &classes {
        writeln!(w, "\nClass: {}", ofn.iri(c.as_str()))?;
        write_frame_annotations(w, ofn, &r, &f.annotations)?;
        for (anns, sup) in &f.sub_class_of {
            write_section_item(w, ofn, &r, "SubClassOf:", anns, &r.class_expression(sup))?;
        }
        for (anns, eq) in &f.equivalent_to {
            write_section_item(w, ofn, &r, "EquivalentTo:", anns, &r.class_expression(eq))?;
        }
        for (anns, dj) in &f.disjoint_with {
            write_section_item(w, ofn, &r, "DisjointWith:", anns, &r.class_expression(dj))?;
        }
        for (anns, cs) in &f.disjoint_union_of {
            let list = cs
                .iter()
                .map(|x| r.class_expression(x))
                .collect::<Vec<_>>()
                .join(", ");
            write_section_item(w, ofn, &r, "DisjointUnionOf:", anns, &list)?;
        }
        for (anns, ops, dps) in &f.has_key {
            let mut parts: Vec<String> = ops.iter().map(|p| r.ope(p)).collect();
            parts.extend(dps.iter().map(|p| ofn.iri(p.as_str())));
            write_section_item(w, ofn, &r, "HasKey:", anns, &parts.join(" "))?;
        }
    }

    for (ind, f) in &individuals {
        writeln!(w, "\nIndividual: {}", r.individual(ind))?;
        write_frame_annotations(w, ofn, &r, &f.annotations)?;
        for (anns, c) in &f.types {
            write_section_item(w, ofn, &r, "Types:", anns, &r.class_expression(c))?;
        }
        for fact in &f.facts {
            match fact {
                Fact::Obj(anns, neg, p, o) => {
                    let prefix = if *neg { "not " } else { "" };
                    let expr = format!("{prefix}{} {}", ofn.iri(p.as_str()), r.individual(o));
                    write_section_item(w, ofn, &r, "Facts:", anns, &expr)?;
                }
                Fact::Data(anns, neg, p, l) => {
                    let prefix = if *neg { "not " } else { "" };
                    let expr = format!("{prefix}{} {}", ofn.iri(p.as_str()), r.literal(l));
                    write_section_item(w, ofn, &r, "Facts:", anns, &expr)?;
                }
            }
        }
        for (anns, o) in &f.same_as {
            write_section_item(w, ofn, &r, "SameAs:", anns, &r.individual(o))?;
        }
        for (anns, o) in &f.different_from {
            write_section_item(w, ofn, &r, "DifferentFrom:", anns, &r.individual(o))?;
        }
    }

    for m in &misc {
        writeln!(w)?;
        match m {
            MiscFrame::EquivalentClasses(anns, cs) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "EquivalentClasses:",
                    anns,
                    cs.iter().map(|c| r.class_expression(c)),
                )?;
            }
            MiscFrame::DisjointClasses(anns, cs) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "DisjointClasses:",
                    anns,
                    cs.iter().map(|c| r.class_expression(c)),
                )?;
            }
            MiscFrame::EquivalentObjectProperties(anns, ps) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "EquivalentProperties:",
                    anns,
                    ps.iter().map(|p| r.ope(p)),
                )?;
            }
            MiscFrame::DisjointObjectProperties(anns, ps) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "DisjointProperties:",
                    anns,
                    ps.iter().map(|p| r.ope(p)),
                )?;
            }
            MiscFrame::EquivalentDataProperties(anns, ps) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "EquivalentProperties:",
                    anns,
                    ps.iter().map(|p| ofn.iri(p.as_str())),
                )?;
            }
            MiscFrame::DisjointDataProperties(anns, ps) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "DisjointProperties:",
                    anns,
                    ps.iter().map(|p| ofn.iri(p.as_str())),
                )?;
            }
            MiscFrame::SameIndividual(anns, is) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "SameIndividual:",
                    anns,
                    is.iter().map(|i| r.individual(i)),
                )?;
            }
            MiscFrame::DifferentIndividuals(anns, is) => {
                write_misc_frame(
                    w,
                    ofn,
                    &r,
                    "DifferentIndividuals:",
                    anns,
                    is.iter().map(|i| r.individual(i)),
                )?;
            }
        }
    }

    Ok(report)
}

/// Format an ontology header and axioms as a Manchester OWL Syntax string.
pub fn document<'a>(
    ofn: &OfnRenderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
    options: WriteOptions,
) -> Result<(String, WriteReport), WriteError> {
    let mut buf = Vec::new();
    let report = write_document(&mut buf, ofn, header, axioms, options)?;
    let s = String::from_utf8(buf).map_err(|e| WriteError::Unsupported(e.to_string()))?;
    Ok((s, report))
}

fn format_annotation_value(ofn: &OfnRenderer, r: &Renderer<'_>, v: &AnnotationValue) -> String {
    match v {
        AnnotationValue::Iri(i) => ofn.iri(i.as_str()),
        AnnotationValue::Literal(l) => r.literal(l),
        AnnotationValue::Anonymous(a) => format!("_:{}", a.label()),
    }
}

fn write_single_annotation(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    r: &Renderer<'_>,
    a: &Annotation,
    indent: usize,
) -> Result<(), WriteError> {
    if !a.annotations.is_empty() {
        write!(w, "{:indent$}Annotations: ", "")?;
        write_inline_annotations(w, ofn, r, &a.annotations)?;
        write!(w, " ")?;
    } else {
        write!(w, "{:indent$}", "")?;
    }
    write!(
        w,
        "{} {}",
        ofn.iri(a.property.as_str()),
        format_annotation_value(ofn, r, &a.value)
    )?;
    Ok(())
}

fn write_inline_annotations(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    r: &Renderer<'_>,
    anns: &BTreeSet<Annotation>,
) -> Result<(), WriteError> {
    for (i, a) in anns.iter().enumerate() {
        if i > 0 {
            w.write_all(b", ")?;
        }
        if !a.annotations.is_empty() {
            w.write_all(b"Annotations: ")?;
            write_inline_annotations(w, ofn, r, &a.annotations)?;
            w.write_all(b" ")?;
        }
        write!(
            w,
            "{} {}",
            ofn.iri(a.property.as_str()),
            format_annotation_value(ofn, r, &a.value)
        )?;
    }
    Ok(())
}

fn write_annotation_list(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    r: &Renderer<'_>,
    anns: &[&Annotation],
    indent: usize,
) -> Result<(), WriteError> {
    for (i, a) in anns.iter().enumerate() {
        if i > 0 {
            writeln!(w, ",")?;
        }
        write_single_annotation(w, ofn, r, a, indent)?;
    }
    Ok(())
}

fn write_frame_annotations(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    r: &Renderer<'_>,
    anns: &[&Axiom],
) -> Result<(), WriteError> {
    if anns.is_empty() {
        return Ok(());
    }
    writeln!(w, "    Annotations:")?;
    for (i, ax) in anns.iter().enumerate() {
        if i > 0 {
            writeln!(w, ",")?;
        }
        let AxiomKind::AnnotationAssertion(p, _, v) = &ax.kind else {
            unreachable!()
        };
        if !ax.annotations.is_empty() {
            write!(w, "        Annotations: ")?;
            write_inline_annotations(w, ofn, r, &ax.annotations)?;
            write!(w, " ")?;
        } else {
            write!(w, "        ")?;
        }
        write!(
            w,
            "{} {}",
            ofn.iri(p.as_str()),
            format_annotation_value(ofn, r, v)
        )?;
    }
    writeln!(w)?;
    Ok(())
}

fn write_section_item(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    r: &Renderer<'_>,
    keyword: &str,
    anns: &BTreeSet<Annotation>,
    body: &str,
) -> Result<(), WriteError> {
    writeln!(w, "    {keyword}")?;
    if !anns.is_empty() {
        write!(w, "        Annotations: ")?;
        write_inline_annotations(w, ofn, r, anns)?;
        writeln!(w)?;
    }
    writeln!(w, "        {body}")?;
    Ok(())
}

fn write_misc_frame(
    w: &mut dyn Write,
    ofn: &OfnRenderer,
    r: &Renderer<'_>,
    keyword: &str,
    anns: &BTreeSet<Annotation>,
    items: impl IntoIterator<Item = String>,
) -> Result<(), WriteError> {
    writeln!(w, "{keyword}")?;
    if !anns.is_empty() {
        write!(w, "    Annotations: ")?;
        write_inline_annotations(w, ofn, r, anns)?;
        writeln!(w)?;
    }
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            writeln!(w, ",")?;
        }
        write!(w, "    {item}")?;
    }
    writeln!(w)?;
    Ok(())
}

// ------------------------------------------------------------------ reading ------

const ALL_FRAME_HEADERS: [&str; 15] = [
    "Prefix:",
    "Ontology:",
    "Import:",
    "AnnotationProperty:",
    "Datatype:",
    "ObjectProperty:",
    "DataProperty:",
    "Class:",
    "Individual:",
    "EquivalentClasses:",
    "DisjointClasses:",
    "EquivalentProperties:",
    "DisjointProperties:",
    "SameIndividual:",
    "DifferentIndividuals:",
];

const SECTION_KEYWORDS: [&str; 17] = [
    "Annotations:",
    "SubClassOf:",
    "EquivalentTo:",
    "DisjointWith:",
    "DisjointUnionOf:",
    "HasKey:",
    "Domain:",
    "Range:",
    "Characteristics:",
    "SubPropertyOf:",
    "InverseOf:",
    "SubPropertyChain:",
    "Types:",
    "Facts:",
    "SameAs:",
    "DifferentFrom:",
    "Import:",
];

/// Strip `# ...` comments outside `<...>`, `"..."`, and `'...'`, preserving byte offsets
/// by replacing comment bytes with ASCII spaces so token spans remain accurate.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_quote: Option<char> = None;
    let mut in_iri = false;
    while let Some(c) = chars.next() {
        if let Some(q) = in_quote {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if c == q {
                in_quote = None;
            }
            continue;
        }
        if in_iri {
            out.push(c);
            if c == '>' || c.is_whitespace() {
                in_iri = false;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                in_quote = Some(c);
                out.push(c);
            }
            '<' => {
                if chars.peek().is_some_and(char::is_ascii_alphabetic) {
                    in_iri = true;
                }
                out.push(c);
            }
            '#' => {
                out.push(' ');
                while let Some(&nc) = chars.peek() {
                    if nc == '\n' {
                        break;
                    }
                    chars.next();
                    for _ in 0..nc.len_utf8() {
                        out.push(' ');
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn line_col(text: &str, byte: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    let limit = byte.min(text.len());
    for (i, c) in text.char_indices() {
        if i >= limit {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Read a Manchester OWL Syntax (`.omn`) document into a [`Document`].
pub fn parse_document(text: &str) -> Result<Document, ParseError> {
    let cleaned = strip_comments(text);
    let toks = lex::tokens(&cleaned).map_err(|e| {
        let (line, column) = line_col(text, e.span.start);
        ParseError {
            line,
            column,
            message: e.message,
        }
    })?;
    let mut p = DocParser {
        text,
        toks,
        pos: 0,
        vocab: Vocabulary::new(std::iter::empty::<(String, String)>()),
        declared_prefixes: Vec::new(),
    };
    p.parse()
}

struct DocParser<'a> {
    text: &'a str,
    toks: Vec<(Tok, Span)>,
    pos: usize,
    vocab: Vocabulary,
    declared_prefixes: Vec<(String, String)>,
}

impl<'a> DocParser<'a> {
    fn err_at(&self, span: Span, msg: impl Into<String>) -> ParseError {
        let (line, column) = line_col(self.text, span.start);
        ParseError {
            line,
            column,
            message: msg.into(),
        }
    }

    fn err_here(&self, msg: impl Into<String>) -> ParseError {
        let span = self
            .toks
            .get(self.pos)
            .map_or(Span::new(self.text.len(), self.text.len()), |(_, s)| *s);
        self.err_at(span, msg)
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }

    fn peek_word(&self) -> Option<&str> {
        self.peek().and_then(Tok::word)
    }

    fn bump(&mut self) -> Option<(Tok, Span)> {
        if self.pos < self.toks.len() {
            let item = self.toks[self.pos].clone();
            self.pos += 1;
            Some(item)
        } else {
            None
        }
    }

    fn is_frame_header(&self, idx: usize) -> bool {
        self.toks
            .get(idx)
            .and_then(|(t, _)| t.word())
            .is_some_and(|w| ALL_FRAME_HEADERS.contains(&w))
    }

    fn is_section_or_frame(&self, idx: usize) -> bool {
        self.toks
            .get(idx)
            .and_then(|(t, _)| t.word())
            .is_some_and(|w| ALL_FRAME_HEADERS.contains(&w) || SECTION_KEYWORDS.contains(&w))
    }

    fn parse(&mut self) -> Result<Document, ParseError> {
        let mut prefixes: Vec<(String, String)> = Vec::new();
        // 1. Read leading Prefix: declarations.
        while self.peek_word() == Some("Prefix:") {
            self.bump();
            let (pname_tok, pspan) = self
                .bump()
                .ok_or_else(|| self.err_here("expected prefix name after Prefix:"))?;
            let Some(raw_prefix) = pname_tok.word() else {
                return Err(self.err_at(pspan, "expected prefix name ending in ':'"));
            };
            let Some(pname) = raw_prefix.strip_suffix(':') else {
                return Err(self.err_at(pspan, format!("prefix '{raw_prefix}' must end with ':'")));
            };
            let (iri_tok, ispan) = self
                .bump()
                .ok_or_else(|| self.err_here("expected <namespace> after prefix name"))?;
            let Tok::Name(Name::Iri(ns)) = iri_tok else {
                return Err(self.err_at(ispan, "expected <namespace> IRI in Prefix:"));
            };
            prefixes.push((pname.to_string(), ns.clone()));
            if !STANDARD_PREFIXES
                .iter()
                .any(|(sp, sn)| *sp == pname && *sn == ns.as_str())
            {
                self.declared_prefixes.push((pname.to_string(), ns));
            }
        }
        self.vocab = Vocabulary::new(prefixes);

        // 2. Parse Ontology: header (if present).
        let mut header = Header::default();
        if self.peek_word() == Some("Ontology:") {
            self.bump();
            if let Some(Tok::Name(Name::Iri(_))) = self.peek() {
                let (Tok::Name(Name::Iri(iri)), span) = self.bump().expect("peeked") else {
                    unreachable!()
                };
                header.iri = Some(parse_iri(&iri).map_err(|e| self.err_at(span, e.0))?);
                if let Some(Tok::Name(Name::Iri(_))) = self.peek() {
                    let (Tok::Name(Name::Iri(ver)), vspan) = self.bump().expect("peeked") else {
                        unreachable!()
                    };
                    header.version_iri =
                        Some(parse_iri(&ver).map_err(|e| self.err_at(vspan, e.0))?);
                }
            }
            loop {
                match self.peek_word() {
                    Some("Import:") => {
                        self.bump();
                        let iri = self.read_iri()?;
                        header.imports.insert(iri);
                    }
                    Some("Annotations:") => {
                        self.bump();
                        let anns = self.parse_annotations_list(0)?;
                        header.annotations.extend(anns);
                    }
                    _ => break,
                }
            }
        }

        // 3. Pre-scan all entity frame headers so forward references resolve cleanly.
        let body_start = self.pos;
        let mut i = body_start;
        let mut depth = 0i32;
        while i < self.toks.len() {
            match &self.toks[i].0 {
                Tok::LParen | Tok::LBrace | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBrace | Tok::RBracket => depth -= 1,
                Tok::Name(Name::Word(w)) if depth == 0 => {
                    let kind = match w.as_str() {
                        "Class:" => Some(EntityKind::Class),
                        "Datatype:" => Some(EntityKind::Datatype),
                        "ObjectProperty:" => Some(EntityKind::ObjectProperty),
                        "DataProperty:" => Some(EntityKind::DataProperty),
                        "AnnotationProperty:" => Some(EntityKind::AnnotationProperty),
                        "Individual:" => Some(EntityKind::NamedIndividual),
                        _ => None,
                    };
                    if let Some(k) = kind
                        && let Some((tok, _)) = self.toks.get(i + 1)
                        && let Some(iri) = self.try_token_iri(tok)
                        && let Ok(parsed) = parse_iri(&iri)
                    {
                        self.vocab.add_entity(Entity::of_kind(k, parsed));
                    }
                }
                _ => {}
            }
            i += 1;
        }

        // 4. Parse all frames in document order.
        let mut axioms = Vec::new();
        while self.pos < self.toks.len() {
            let Some(kw) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected Manchester frame keyword"));
            };
            match kw.as_str() {
                "AnnotationProperty:" => self.parse_ann_prop_frame(&mut axioms)?,
                "Datatype:" => self.parse_datatype_frame(&mut axioms)?,
                "ObjectProperty:" => self.parse_obj_prop_frame(&mut axioms)?,
                "DataProperty:" => self.parse_data_prop_frame(&mut axioms)?,
                "Class:" => self.parse_class_frame(&mut axioms)?,
                "Individual:" => self.parse_individual_frame(&mut axioms)?,
                "EquivalentClasses:" => self.parse_misc_classes(true, &mut axioms)?,
                "DisjointClasses:" => self.parse_misc_classes(false, &mut axioms)?,
                "EquivalentProperties:" => self.parse_misc_properties(true, &mut axioms)?,
                "DisjointProperties:" => self.parse_misc_properties(false, &mut axioms)?,
                "SameIndividual:" => self.parse_misc_individuals(true, &mut axioms)?,
                "DifferentIndividuals:" => self.parse_misc_individuals(false, &mut axioms)?,
                other => {
                    return Err(self.err_here(format!("unexpected Manchester frame '{other}'")));
                }
            }
        }

        for ax in &axioms {
            validate::axiom(ax).map_err(|e| self.err_here(e.0))?;
        }

        Ok(Document {
            prefixes: self.declared_prefixes.clone(),
            header,
            axioms,
            rules: 0,
        })
    }

    fn try_token_iri(&self, tok: &Tok) -> Option<String> {
        match tok {
            Tok::Name(Name::Iri(i)) => Some(i.clone()),
            Tok::Name(Name::Word(w)) if !w.starts_with("_:") => self.vocab.expand(w),
            _ => None,
        }
    }

    fn read_iri(&mut self) -> Result<Iri, ParseError> {
        let (tok, span) = self
            .bump()
            .ok_or_else(|| self.err_here("expected an IRI or prefixed name"))?;
        match tok {
            Tok::Name(Name::Iri(i)) => parse_iri(&i).map_err(|e| self.err_at(span, e.0)),
            Tok::Name(Name::Word(w)) => {
                if let Some(i) = self.vocab.expand(&w) {
                    parse_iri(&i).map_err(|e| self.err_at(span, e.0))
                } else {
                    Err(self.err_at(span, format!("cannot expand IRI or prefixed name '{w}'")))
                }
            }
            other => Err(self.err_at(span, format!("expected IRI, got '{}'", other.shown()))),
        }
    }

    fn read_individual(&mut self) -> Result<Individual, ParseError> {
        let (tok, span) = self
            .bump()
            .ok_or_else(|| self.err_here("expected an individual"))?;
        if let Tok::Name(Name::Word(w)) = &tok
            && let Some(label) = w.strip_prefix("_:")
        {
            if label.is_empty() {
                return Err(self.err_at(span, "empty anonymous individual label"));
            }
            return Ok(Individual::anonymous(label));
        }
        let iri = match tok {
            Tok::Name(Name::Iri(i)) => parse_iri(&i).map_err(|e| self.err_at(span, e.0))?,
            Tok::Name(Name::Word(w)) => {
                let Some(i) = self.vocab.expand(&w) else {
                    return Err(self.err_at(span, format!("unknown individual '{w}'")));
                };
                parse_iri(&i).map_err(|e| self.err_at(span, e.0))?
            }
            other => {
                return Err(self.err_at(
                    span,
                    format!("expected individual, got '{}'", other.shown()),
                ));
            }
        };
        Ok(Individual::Named(NamedIndividual(iri)))
    }

    fn read_annotation_value(&mut self) -> Result<AnnotationValue, ParseError> {
        let Some((tok, span)) = self.toks.get(self.pos).cloned() else {
            return Err(self.err_here("expected annotation value"));
        };
        match &tok {
            Tok::Str { .. } | Tok::Number(_) => {
                self.bump();
                let lit = Parser::from_tokens(vec![(tok, span)], self.text.len(), &self.vocab)
                    .whole(Parser::literal)
                    .map_err(|e| self.err_at(e.span, e.message))?;
                Ok(AnnotationValue::Literal(lit))
            }
            Tok::Name(Name::Word(w)) if w == "true" || w == "false" => {
                self.bump();
                Ok(AnnotationValue::Literal(Literal::typed(
                    w,
                    crate::vocab::XSD_BOOLEAN,
                )))
            }
            Tok::Name(Name::Word(w)) if w.starts_with("_:") => {
                self.bump();
                let label = w.strip_prefix("_:").expect("checked");
                if label.is_empty() {
                    return Err(self.err_at(span, "empty anonymous individual label"));
                }
                Ok(AnnotationValue::Anonymous(AnonymousIndividual::new(label)))
            }
            _ => Ok(AnnotationValue::Iri(self.read_iri()?)),
        }
    }

    fn parse_annotations_list(&mut self, depth: usize) -> Result<BTreeSet<Annotation>, ParseError> {
        if depth >= MAX_NESTING {
            return Err(self.err_here(format!(
                "Manchester annotation nests deeper than {MAX_NESTING} levels"
            )));
        }
        let mut out = BTreeSet::new();
        loop {
            let nested = if self.peek_word() == Some("Annotations:") {
                self.bump();
                self.parse_annotations_list(depth + 1)?
            } else {
                BTreeSet::new()
            };
            let prop = AnnotationProperty(self.read_iri()?);
            let value = self.read_annotation_value()?;
            out.insert(Annotation {
                property: prop,
                value,
                annotations: nested,
            });
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
                continue;
            }
            break;
        }
        Ok(out)
    }

    fn maybe_annotations(&mut self) -> Result<BTreeSet<Annotation>, ParseError> {
        if self.peek_word() == Some("Annotations:") {
            self.bump();
            self.parse_annotations_list(0)
        } else {
            Ok(BTreeSet::new())
        }
    }

    /// Collect tokens for one comma-separated expression item, respecting parentheses,
    /// braces and brackets, stopping before top-level `,` or the next section/frame keyword.
    fn collect_item_tokens(&mut self) -> Vec<(Tok, Span)> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        while self.pos < self.toks.len() {
            if depth == 0
                && (self.peek() == Some(&Tok::Comma) || self.is_section_or_frame(self.pos))
            {
                break;
            }
            let (t, s) = self.bump().expect("in bounds");
            match t {
                Tok::LParen | Tok::LBrace | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBrace | Tok::RBracket => depth -= 1,
                _ => {}
            }
            out.push((t, s));
        }
        out
    }

    fn parse_ce_item(&mut self) -> Result<ClassExpression, ParseError> {
        let toks = self.collect_item_tokens();
        if toks.is_empty() {
            return Err(self.err_here("expected class expression"));
        }
        Parser::from_tokens(toks, self.text.len(), &self.vocab)
            .whole(Parser::description)
            .map_err(|e| self.err_at(e.span, e.message))
    }

    fn parse_dr_item(&mut self) -> Result<DataRange, ParseError> {
        let toks = self.collect_item_tokens();
        if toks.is_empty() {
            return Err(self.err_here("expected data range"));
        }
        Parser::from_tokens(toks, self.text.len(), &self.vocab)
            .whole(Parser::data_range)
            .map_err(|e| self.err_at(e.span, e.message))
    }

    fn parse_ope_item(&mut self) -> Result<ObjectPropertyExpression, ParseError> {
        let toks = self.collect_item_tokens();
        if toks.is_empty() {
            return Err(self.err_here("expected object property expression"));
        }
        Parser::from_tokens(toks, self.text.len(), &self.vocab)
            .whole(Parser::ope)
            .map_err(|e| self.err_at(e.span, e.message))
    }

    fn parse_frame_annotations_section(
        &mut self,
        subj: AnnotationSubject,
        axioms: &mut Vec<Axiom>,
    ) -> Result<(), ParseError> {
        let anns = self.parse_annotations_list(0)?;
        for a in anns {
            axioms.push(Axiom {
                kind: AxiomKind::AnnotationAssertion(a.property, subj.clone(), a.value),
                annotations: a.annotations,
            });
        }
        Ok(())
    }

    fn parse_ann_prop_frame(&mut self, axioms: &mut Vec<Axiom>) -> Result<(), ParseError> {
        self.bump();
        let p = AnnotationProperty(self.read_iri()?);
        axioms.push(Axiom::declaration(Entity::AnnotationProperty(p.clone())));
        while !self.is_frame_header(self.pos) && self.pos < self.toks.len() {
            let Some(sec) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected section keyword in AnnotationProperty frame"));
            };
            self.bump();
            match sec.as_str() {
                "Annotations:" => self
                    .parse_frame_annotations_section(AnnotationSubject::Iri(p.0.clone()), axioms)?,
                "Domain:" | "Range:" | "SubPropertyOf:" => loop {
                    let anns = self.maybe_annotations()?;
                    let target = self.read_iri()?;
                    let kind = match sec.as_str() {
                        "Domain:" => AxiomKind::AnnotationPropertyDomain(p.clone(), target),
                        "Range:" => AxiomKind::AnnotationPropertyRange(p.clone(), target),
                        _ => AxiomKind::SubAnnotationPropertyOf(
                            p.clone(),
                            AnnotationProperty(target),
                        ),
                    };
                    axioms.push(Axiom {
                        kind,
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                other => {
                    return Err(self.err_here(format!(
                        "unexpected section '{other}' in AnnotationProperty frame"
                    )));
                }
            }
        }
        Ok(())
    }

    fn parse_datatype_frame(&mut self, axioms: &mut Vec<Axiom>) -> Result<(), ParseError> {
        self.bump();
        let d = Datatype(self.read_iri()?);
        axioms.push(Axiom::declaration(Entity::Datatype(d.clone())));
        while !self.is_frame_header(self.pos) && self.pos < self.toks.len() {
            let Some(sec) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected section keyword in Datatype frame"));
            };
            self.bump();
            match sec.as_str() {
                "Annotations:" => self
                    .parse_frame_annotations_section(AnnotationSubject::Iri(d.0.clone()), axioms)?,
                "EquivalentTo:" => loop {
                    let anns = self.maybe_annotations()?;
                    let dr = self.parse_dr_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::DatatypeDefinition(d.clone(), dr),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                other => {
                    return Err(
                        self.err_here(format!("unexpected section '{other}' in Datatype frame"))
                    );
                }
            }
        }
        Ok(())
    }

    fn parse_obj_prop_frame(&mut self, axioms: &mut Vec<Axiom>) -> Result<(), ParseError> {
        self.bump();
        let p = ObjectProperty(self.read_iri()?);
        axioms.push(Axiom::declaration(Entity::ObjectProperty(p.clone())));
        while !self.is_frame_header(self.pos) && self.pos < self.toks.len() {
            let Some(sec) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected section keyword in ObjectProperty frame"));
            };
            self.bump();
            match sec.as_str() {
                "Annotations:" => self
                    .parse_frame_annotations_section(AnnotationSubject::Iri(p.0.clone()), axioms)?,
                "Domain:" | "Range:" => loop {
                    let anns = self.maybe_annotations()?;
                    let ce = self.parse_ce_item()?;
                    let kind = if sec == "Domain:" {
                        AxiomKind::ObjectPropertyDomain(p.clone().into(), ce)
                    } else {
                        AxiomKind::ObjectPropertyRange(p.clone().into(), ce)
                    };
                    axioms.push(Axiom {
                        kind,
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "Characteristics:" => loop {
                    let anns = self.maybe_annotations()?;
                    let (tok, span) = self
                        .bump()
                        .ok_or_else(|| self.err_here("expected property characteristic"))?;
                    let kind = match tok.word() {
                        Some("Functional") => AxiomKind::FunctionalObjectProperty(p.clone().into()),
                        Some("InverseFunctional") => {
                            AxiomKind::InverseFunctionalObjectProperty(p.clone().into())
                        }
                        Some("Reflexive") => AxiomKind::ReflexiveObjectProperty(p.clone().into()),
                        Some("Irreflexive") => {
                            AxiomKind::IrreflexiveObjectProperty(p.clone().into())
                        }
                        Some("Symmetric") => AxiomKind::SymmetricObjectProperty(p.clone().into()),
                        Some("Asymmetric") => AxiomKind::AsymmetricObjectProperty(p.clone().into()),
                        Some("Transitive") => AxiomKind::TransitiveObjectProperty(p.clone().into()),
                        _ => {
                            return Err(self.err_at(
                                span,
                                format!("unknown object property characteristic '{}'", tok.shown()),
                            ));
                        }
                    };
                    axioms.push(Axiom {
                        kind,
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "SubPropertyOf:" => loop {
                    let anns = self.maybe_annotations()?;
                    let sup = self.parse_ope_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::SubObjectPropertyOf(
                            SubObjectPropertyExpression::Expression(p.clone().into()),
                            sup,
                        ),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "EquivalentTo:" => loop {
                    let anns = self.maybe_annotations()?;
                    let other = self.parse_ope_item()?;
                    let mut set = BTreeSet::new();
                    set.insert(ObjectPropertyExpression::Property(p.clone()));
                    set.insert(other);
                    axioms.push(Axiom {
                        kind: AxiomKind::EquivalentObjectProperties(set),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "DisjointWith:" => loop {
                    let anns = self.maybe_annotations()?;
                    let other = self.parse_ope_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::DisjointObjectProperties(
                            vec![ObjectPropertyExpression::Property(p.clone()), other].into(),
                        ),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "InverseOf:" => loop {
                    let anns = self.maybe_annotations()?;
                    let other = self.parse_ope_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::InverseObjectProperties(p.clone().into(), other),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "SubPropertyChain:" => {
                    let anns = self.maybe_annotations()?;
                    let toks = self.collect_item_tokens();
                    if toks.is_empty() {
                        return Err(
                            self.err_here("expected property chain after SubPropertyChain:")
                        );
                    }
                    let mut chain = Vec::new();
                    let mut part = Vec::new();
                    for (t, s) in toks {
                        if t.word() == Some("o") {
                            if part.is_empty() {
                                return Err(self
                                    .err_at(s, "expected object property expression before 'o'"));
                            }
                            let ope = Parser::from_tokens(
                                std::mem::take(&mut part),
                                self.text.len(),
                                &self.vocab,
                            )
                            .whole(Parser::ope)
                            .map_err(|e| self.err_at(e.span, e.message))?;
                            chain.push(ope);
                        } else {
                            part.push((t, s));
                        }
                    }
                    if part.is_empty() {
                        return Err(self.err_here(
                            "expected object property expression after trailing 'o' in SubPropertyChain:",
                        ));
                    }
                    let ope = Parser::from_tokens(part, self.text.len(), &self.vocab)
                        .whole(Parser::ope)
                        .map_err(|e| self.err_at(e.span, e.message))?;
                    chain.push(ope);
                    axioms.push(Axiom {
                        kind: AxiomKind::SubObjectPropertyOf(
                            SubObjectPropertyExpression::Chain(chain),
                            p.clone().into(),
                        ),
                        annotations: anns,
                    });
                }
                other => {
                    return Err(self.err_here(format!(
                        "unexpected section '{other}' in ObjectProperty frame"
                    )));
                }
            }
        }
        Ok(())
    }

    fn parse_data_prop_frame(&mut self, axioms: &mut Vec<Axiom>) -> Result<(), ParseError> {
        self.bump();
        let p = DataProperty(self.read_iri()?);
        axioms.push(Axiom::declaration(Entity::DataProperty(p.clone())));
        while !self.is_frame_header(self.pos) && self.pos < self.toks.len() {
            let Some(sec) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected section keyword in DataProperty frame"));
            };
            self.bump();
            match sec.as_str() {
                "Annotations:" => self
                    .parse_frame_annotations_section(AnnotationSubject::Iri(p.0.clone()), axioms)?,
                "Domain:" => loop {
                    let anns = self.maybe_annotations()?;
                    let ce = self.parse_ce_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::DataPropertyDomain(p.clone(), ce),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "Range:" => loop {
                    let anns = self.maybe_annotations()?;
                    let dr = self.parse_dr_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::DataPropertyRange(p.clone(), dr),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "Characteristics:" => loop {
                    let anns = self.maybe_annotations()?;
                    let (tok, span) = self
                        .bump()
                        .ok_or_else(|| self.err_here("expected Functional"))?;
                    if tok.word() != Some("Functional") {
                        return Err(self
                            .err_at(span, format!("expected Functional, got '{}'", tok.shown())));
                    }
                    axioms.push(Axiom {
                        kind: AxiomKind::FunctionalDataProperty(p.clone()),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "SubPropertyOf:" | "EquivalentTo:" | "DisjointWith:" => loop {
                    let anns = self.maybe_annotations()?;
                    let other = DataProperty(self.read_iri()?);
                    let kind = match sec.as_str() {
                        "SubPropertyOf:" => AxiomKind::SubDataPropertyOf(p.clone(), other),
                        "EquivalentTo:" => {
                            let mut set = BTreeSet::new();
                            set.insert(p.clone());
                            set.insert(other);
                            AxiomKind::EquivalentDataProperties(set)
                        }
                        _ => AxiomKind::DisjointDataProperties(vec![p.clone(), other].into()),
                    };
                    axioms.push(Axiom {
                        kind,
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                other => {
                    return Err(self.err_here(format!(
                        "unexpected section '{other}' in DataProperty frame"
                    )));
                }
            }
        }
        Ok(())
    }

    fn parse_class_frame(&mut self, axioms: &mut Vec<Axiom>) -> Result<(), ParseError> {
        self.bump();
        let c = Class(self.read_iri()?);
        axioms.push(Axiom::declaration(Entity::Class(c.clone())));
        while !self.is_frame_header(self.pos) && self.pos < self.toks.len() {
            let Some(sec) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected section keyword in Class frame"));
            };
            self.bump();
            match sec.as_str() {
                "Annotations:" => self
                    .parse_frame_annotations_section(AnnotationSubject::Iri(c.0.clone()), axioms)?,
                "SubClassOf:" => loop {
                    let anns = self.maybe_annotations()?;
                    let sup = self.parse_ce_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::SubClassOf(ClassExpression::Class(c.clone()), sup),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "EquivalentTo:" => loop {
                    let anns = self.maybe_annotations()?;
                    let eq = self.parse_ce_item()?;
                    let mut set = BTreeSet::new();
                    set.insert(ClassExpression::Class(c.clone()));
                    set.insert(eq);
                    axioms.push(Axiom {
                        kind: AxiomKind::EquivalentClasses(set),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "DisjointWith:" => loop {
                    let anns = self.maybe_annotations()?;
                    let dj = self.parse_ce_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::DisjointClasses(
                            vec![ClassExpression::Class(c.clone()), dj].into(),
                        ),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "DisjointUnionOf:" => {
                    let anns = self.maybe_annotations()?;
                    let mut members = Vec::new();
                    loop {
                        members.push(self.parse_ce_item()?);
                        if self.peek() == Some(&Tok::Comma) {
                            self.bump();
                            continue;
                        }
                        break;
                    }
                    axioms.push(Axiom {
                        kind: AxiomKind::DisjointUnion(c.clone(), members.into()),
                        annotations: anns,
                    });
                }
                "HasKey:" => {
                    let anns = self.maybe_annotations()?;
                    let mut ops = BTreeSet::new();
                    let mut dps = BTreeSet::new();
                    while self.pos < self.toks.len() && !self.is_section_or_frame(self.pos) {
                        if self.peek() == Some(&Tok::Comma) {
                            self.bump();
                            continue;
                        }
                        if self.peek_word() == Some("inverse") {
                            let inv = self.bump().expect("peeked");
                            let mut toks = vec![inv];
                            if self.peek() == Some(&Tok::LParen) {
                                toks.push(self.bump().expect("peeked"));
                                let Some(prop_tok) = self.bump() else {
                                    return Err(
                                        self.err_here("expected property after 'inverse ('")
                                    );
                                };
                                toks.push(prop_tok);
                                let Some(rp) = self.bump() else {
                                    return Err(
                                        self.err_here("expected ')' after inverse property")
                                    );
                                };
                                toks.push(rp);
                            } else {
                                let Some(prop_tok) = self.bump() else {
                                    return Err(self.err_here("expected property after 'inverse'"));
                                };
                                toks.push(prop_tok);
                            }
                            let ope = Parser::from_tokens(toks, self.text.len(), &self.vocab)
                                .whole(Parser::ope)
                                .map_err(|e| self.err_at(e.span, e.message))?;
                            ops.insert(ope);
                        } else {
                            let Some((tok, span)) = self.bump() else {
                                break;
                            };
                            let is_dp = self.try_token_iri(&tok).is_some_and(|iri| {
                                let kinds = self.vocab.kinds(&iri);
                                kinds.contains(&EntityKind::DataProperty)
                                    && !kinds.contains(&EntityKind::ObjectProperty)
                            });
                            if is_dp {
                                let dp = Parser::from_tokens(
                                    vec![(tok, span)],
                                    self.text.len(),
                                    &self.vocab,
                                )
                                .whole(Parser::data_property)
                                .map_err(|e| self.err_at(e.span, e.message))?;
                                dps.insert(dp);
                            } else {
                                let ope = Parser::from_tokens(
                                    vec![(tok, span)],
                                    self.text.len(),
                                    &self.vocab,
                                )
                                .whole(Parser::ope)
                                .map_err(|e| self.err_at(e.span, e.message))?;
                                ops.insert(ope);
                            }
                        }
                    }
                    if ops.is_empty() && dps.is_empty() {
                        return Err(self.err_here("expected property in HasKey:"));
                    }
                    axioms.push(Axiom {
                        kind: AxiomKind::HasKey(ClassExpression::Class(c.clone()), ops, dps),
                        annotations: anns,
                    });
                }
                other => {
                    return Err(
                        self.err_here(format!("unexpected section '{other}' in Class frame"))
                    );
                }
            }
        }
        Ok(())
    }

    fn parse_individual_frame(&mut self, axioms: &mut Vec<Axiom>) -> Result<(), ParseError> {
        self.bump();
        let ind = self.read_individual()?;
        if let Individual::Named(n) = &ind {
            axioms.push(Axiom::declaration(Entity::NamedIndividual(n.clone())));
        }
        let ann_subj = match &ind {
            Individual::Named(n) => AnnotationSubject::Iri(n.0.clone()),
            Individual::Anonymous(a) => AnnotationSubject::Anonymous(a.clone()),
        };
        while !self.is_frame_header(self.pos) && self.pos < self.toks.len() {
            let Some(sec) = self.peek_word().map(str::to_string) else {
                return Err(self.err_here("expected section keyword in Individual frame"));
            };
            self.bump();
            match sec.as_str() {
                "Annotations:" => self.parse_frame_annotations_section(ann_subj.clone(), axioms)?,
                "Types:" => loop {
                    let anns = self.maybe_annotations()?;
                    let ce = self.parse_ce_item()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::ClassAssertion(ce, ind.clone()),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "Facts:" => loop {
                    let anns = self.maybe_annotations()?;
                    let neg = if self.peek_word() == Some("not") {
                        self.bump();
                        true
                    } else {
                        false
                    };
                    let prop_iri = self.read_iri()?;
                    let is_lit = matches!(self.peek(), Some(Tok::Str { .. } | Tok::Number(_)))
                        || matches!(self.peek_word(), Some("true" | "false"));
                    let kind = if is_lit {
                        let (tok, span) = self.bump().expect("peeked");
                        let lit =
                            Parser::from_tokens(vec![(tok, span)], self.text.len(), &self.vocab)
                                .whole(Parser::literal)
                                .map_err(|e| self.err_at(e.span, e.message))?;
                        if neg {
                            AxiomKind::NegativeDataPropertyAssertion(
                                DataProperty(prop_iri),
                                ind.clone(),
                                lit,
                            )
                        } else {
                            AxiomKind::DataPropertyAssertion(
                                DataProperty(prop_iri),
                                ind.clone(),
                                lit,
                            )
                        }
                    } else {
                        let target = self.read_individual()?;
                        let ope = ObjectPropertyExpression::Property(ObjectProperty(prop_iri));
                        if neg {
                            AxiomKind::NegativeObjectPropertyAssertion(ope, ind.clone(), target)
                        } else {
                            AxiomKind::ObjectPropertyAssertion(ope, ind.clone(), target)
                        }
                    };
                    axioms.push(Axiom {
                        kind,
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "SameAs:" => loop {
                    let anns = self.maybe_annotations()?;
                    let other = self.read_individual()?;
                    let mut set = BTreeSet::new();
                    set.insert(ind.clone());
                    set.insert(other);
                    axioms.push(Axiom {
                        kind: AxiomKind::SameIndividual(set),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                "DifferentFrom:" => loop {
                    let anns = self.maybe_annotations()?;
                    let other = self.read_individual()?;
                    axioms.push(Axiom {
                        kind: AxiomKind::DifferentIndividuals(vec![ind.clone(), other].into()),
                        annotations: anns,
                    });
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    break;
                },
                other => {
                    return Err(
                        self.err_here(format!("unexpected section '{other}' in Individual frame"))
                    );
                }
            }
        }
        Ok(())
    }

    fn parse_misc_classes(
        &mut self,
        equiv: bool,
        axioms: &mut Vec<Axiom>,
    ) -> Result<(), ParseError> {
        self.bump();
        let anns = self.maybe_annotations()?;
        let mut items = Vec::new();
        loop {
            items.push(self.parse_ce_item()?);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
                continue;
            }
            break;
        }
        let kind = if equiv {
            AxiomKind::EquivalentClasses(items.into_iter().collect())
        } else {
            AxiomKind::DisjointClasses(items.into())
        };
        axioms.push(Axiom {
            kind,
            annotations: anns,
        });
        Ok(())
    }

    fn parse_misc_properties(
        &mut self,
        equiv: bool,
        axioms: &mut Vec<Axiom>,
    ) -> Result<(), ParseError> {
        self.bump();
        let anns = self.maybe_annotations()?;
        let mut raw_items = Vec::new();
        loop {
            let toks = self.collect_item_tokens();
            if toks.is_empty() {
                return Err(self.err_here("expected property in misc frame"));
            }
            raw_items.push(toks);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
                continue;
            }
            break;
        }
        let is_data = raw_items.iter().all(|toks| {
            toks.len() == 1
                && self.try_token_iri(&toks[0].0).is_some_and(|iri| {
                    let kinds = self.vocab.kinds(&iri);
                    kinds.contains(&EntityKind::DataProperty)
                        && !kinds.contains(&EntityKind::ObjectProperty)
                })
        });
        let kind = if is_data {
            let mut dps = Vec::new();
            for toks in raw_items {
                let dp = Parser::from_tokens(toks, self.text.len(), &self.vocab)
                    .whole(Parser::data_property)
                    .map_err(|e| self.err_at(e.span, e.message))?;
                dps.push(dp);
            }
            if equiv {
                AxiomKind::EquivalentDataProperties(dps.into_iter().collect())
            } else {
                AxiomKind::DisjointDataProperties(dps.into())
            }
        } else {
            let mut ops = Vec::new();
            for toks in raw_items {
                let ope = Parser::from_tokens(toks, self.text.len(), &self.vocab)
                    .whole(Parser::ope)
                    .map_err(|e| self.err_at(e.span, e.message))?;
                ops.push(ope);
            }
            if equiv {
                AxiomKind::EquivalentObjectProperties(ops.into_iter().collect())
            } else {
                AxiomKind::DisjointObjectProperties(ops.into())
            }
        };
        axioms.push(Axiom {
            kind,
            annotations: anns,
        });
        Ok(())
    }

    fn parse_misc_individuals(
        &mut self,
        same: bool,
        axioms: &mut Vec<Axiom>,
    ) -> Result<(), ParseError> {
        self.bump();
        let anns = self.maybe_annotations()?;
        let mut items = Vec::new();
        loop {
            items.push(self.read_individual()?);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
                continue;
            }
            break;
        }
        let kind = if same {
            AxiomKind::SameIndividual(items.into_iter().collect())
        } else {
            AxiomKind::DifferentIndividuals(items.into())
        };
        axioms.push(Axiom {
            kind,
            annotations: anns,
        });
        Ok(())
    }
}
