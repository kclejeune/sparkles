//! The forward mapping: axioms to triples, as the OWL 2 Mapping to RDF Graphs §2
//! defines it.
//!
//! Every blank node the mapping generates is fresh, taken from the caller's allocator;
//! an anonymous individual is written as the blank node with its label. RDF lists get a
//! fresh node for each cell. An annotated axiom is written with its main triple and an
//! `owl:Axiom` node (§2.3.1), the axioms that become several triples repeat the
//! annotations on each (§2.3.2), and the axioms represented by a blank node carry them
//! on that node (§2.3.3).

use crate::model::*;
use crate::vocab;
use oxrdf::{BlankNode, NamedNode, NamedOrBlankNode, Term, Triple};

/// Collects the triples of a translation.
pub struct Writer<'f> {
    fresh: &'f mut dyn FnMut() -> BlankNode,
    pub triples: Vec<Triple>,
}

fn nn(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn nonneg(n: u32) -> Term {
    Term::Literal(oxrdf::Literal::new_typed_literal(
        n.to_string(),
        nn(vocab::XSD_NON_NEGATIVE_INTEGER),
    ))
}

/// The term of an individual.
pub fn individual_term(i: &Individual) -> NamedOrBlankNode {
    match i {
        Individual::Named(n) => n.0.clone().into(),
        Individual::Anonymous(a) => BlankNode::new_unchecked(a.label()).into(),
    }
}

impl<'f> Writer<'f> {
    pub fn new(fresh: &'f mut dyn FnMut() -> BlankNode) -> Writer<'f> {
        Writer {
            fresh,
            triples: Vec::new(),
        }
    }

    fn blank(&mut self) -> BlankNode {
        (self.fresh)()
    }

    fn add(&mut self, s: impl Into<NamedOrBlankNode>, p: &str, o: impl Into<Term>) {
        self.triples.push(Triple::new(s.into(), nn(p), o.into()));
    }

    fn typed(&mut self, t: &str) -> BlankNode {
        let x = self.blank();
        self.add(x.clone(), vocab::RDF_TYPE, nn(t));
        x
    }

    /// T(SEQ y1 … yn): a list with a fresh node per cell, `rdf:nil` when empty.
    pub fn list(&mut self, items: Vec<Term>) -> Term {
        let mut head: Term = nn(vocab::RDF_NIL).into();
        let cells: Vec<BlankNode> = items.iter().map(|_| self.blank()).collect();
        for (i, item) in items.into_iter().enumerate() {
            self.add(cells[i].clone(), vocab::RDF_FIRST, item);
            let rest: Term = match cells.get(i + 1) {
                Some(next) => next.clone().into(),
                None => nn(vocab::RDF_NIL).into(),
            };
            self.add(cells[i].clone(), vocab::RDF_REST, rest);
        }
        if let Some(first) = cells.first() {
            head = first.clone().into();
        }
        head
    }

    pub fn ope(&mut self, p: &ObjectPropertyExpression) -> NamedOrBlankNode {
        match p {
            ObjectPropertyExpression::Property(p) => p.0.clone().into(),
            ObjectPropertyExpression::InverseOf(p) => {
                let x = self.blank();
                self.add(x.clone(), vocab::OWL_INVERSE_OF, p.0.clone());
                x.into()
            }
        }
    }

    pub fn data_range(&mut self, r: &DataRange) -> NamedOrBlankNode {
        match r {
            DataRange::Datatype(d) => d.0.clone().into(),
            DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
                let items = rs.iter().map(|r| self.data_range(r).into()).collect();
                let x = self.typed(vocab::RDFS_DATATYPE);
                let l = self.list(items);
                let p = if matches!(r, DataRange::IntersectionOf(_)) {
                    vocab::OWL_INTERSECTION_OF
                } else {
                    vocab::OWL_UNION_OF
                };
                self.add(x.clone(), p, l);
                x.into()
            }
            DataRange::ComplementOf(inner) => {
                let i = self.data_range(inner);
                let x = self.typed(vocab::RDFS_DATATYPE);
                self.add(x.clone(), vocab::OWL_DATATYPE_COMPLEMENT_OF, i);
                x.into()
            }
            DataRange::OneOf(ls) => {
                let items = ls.iter().map(|l| l.0.clone().into()).collect();
                let x = self.typed(vocab::RDFS_DATATYPE);
                let l = self.list(items);
                self.add(x.clone(), vocab::OWL_ONE_OF, l);
                x.into()
            }
            DataRange::DatatypeRestriction(d, fs) => {
                let x = self.typed(vocab::RDFS_DATATYPE);
                self.add(x.clone(), vocab::OWL_ON_DATATYPE, d.0.clone());
                let mut ys = Vec::new();
                for f in fs {
                    let y = self.blank();
                    self.add(y.clone(), f.facet.as_str(), f.value.0.clone());
                    ys.push(y.into());
                }
                let l = self.list(ys);
                self.add(x.clone(), vocab::OWL_WITH_RESTRICTIONS, l);
                x.into()
            }
        }
    }

    pub fn class_expression(&mut self, c: &ClassExpression) -> NamedOrBlankNode {
        use ClassExpression as C;
        match c {
            C::Class(c) => c.0.clone().into(),
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                let items = cs.iter().map(|c| self.class_expression(c).into()).collect();
                let x = self.typed(vocab::OWL_CLASS);
                let l = self.list(items);
                let p = if matches!(c, C::ObjectIntersectionOf(_)) {
                    vocab::OWL_INTERSECTION_OF
                } else {
                    vocab::OWL_UNION_OF
                };
                self.add(x.clone(), p, l);
                x.into()
            }
            C::ObjectComplementOf(inner) => {
                let i = self.class_expression(inner);
                let x = self.typed(vocab::OWL_CLASS);
                self.add(x.clone(), vocab::OWL_COMPLEMENT_OF, i);
                x.into()
            }
            C::ObjectOneOf(is) => {
                let items = is.iter().map(|i| individual_term(i).into()).collect();
                let x = self.typed(vocab::OWL_CLASS);
                let l = self.list(items);
                self.add(x.clone(), vocab::OWL_ONE_OF, l);
                x.into()
            }
            C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => {
                let pn = self.ope(p);
                let fv = self.class_expression(f);
                let x = self.typed(vocab::OWL_RESTRICTION);
                self.add(x.clone(), vocab::OWL_ON_PROPERTY, pn);
                let q = if matches!(c, C::ObjectSomeValuesFrom(..)) {
                    vocab::OWL_SOME_VALUES_FROM
                } else {
                    vocab::OWL_ALL_VALUES_FROM
                };
                self.add(x.clone(), q, fv);
                x.into()
            }
            C::ObjectHasValue(p, i) => {
                let pn = self.ope(p);
                let x = self.typed(vocab::OWL_RESTRICTION);
                self.add(x.clone(), vocab::OWL_ON_PROPERTY, pn);
                self.add(x.clone(), vocab::OWL_HAS_VALUE, individual_term(i));
                x.into()
            }
            C::ObjectHasSelf(p) => {
                let pn = self.ope(p);
                let x = self.typed(vocab::OWL_RESTRICTION);
                self.add(x.clone(), vocab::OWL_ON_PROPERTY, pn);
                self.add(
                    x.clone(),
                    vocab::OWL_HAS_SELF,
                    oxrdf::Literal::new_typed_literal("true", nn(vocab::XSD_BOOLEAN)),
                );
                x.into()
            }
            C::ObjectMinCardinality(n, p, f)
            | C::ObjectMaxCardinality(n, p, f)
            | C::ObjectExactCardinality(n, p, f) => {
                let pn = self.ope(p);
                let fv = f.as_ref().map(|f| self.class_expression(f));
                let x = self.typed(vocab::OWL_RESTRICTION);
                self.add(x.clone(), vocab::OWL_ON_PROPERTY, pn);
                let (plain, qualified) = match c {
                    C::ObjectMinCardinality(..) => (
                        vocab::OWL_MIN_CARDINALITY,
                        vocab::OWL_MIN_QUALIFIED_CARDINALITY,
                    ),
                    C::ObjectMaxCardinality(..) => (
                        vocab::OWL_MAX_CARDINALITY,
                        vocab::OWL_MAX_QUALIFIED_CARDINALITY,
                    ),
                    _ => (vocab::OWL_CARDINALITY, vocab::OWL_QUALIFIED_CARDINALITY),
                };
                match fv {
                    Some(fv) => {
                        self.add(x.clone(), qualified, nonneg(*n));
                        self.add(x.clone(), vocab::OWL_ON_CLASS, fv);
                    }
                    None => self.add(x.clone(), plain, nonneg(*n)),
                }
                x.into()
            }
            C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
                let rv = self.data_range(r);
                let x = self.typed(vocab::OWL_RESTRICTION);
                if ps.len() == 1 {
                    self.add(x.clone(), vocab::OWL_ON_PROPERTY, ps[0].0.clone());
                } else {
                    let items = ps.iter().map(|p| p.0.clone().into()).collect();
                    let l = self.list(items);
                    self.add(x.clone(), vocab::OWL_ON_PROPERTIES, l);
                }
                let q = if matches!(c, C::DataSomeValuesFrom(..)) {
                    vocab::OWL_SOME_VALUES_FROM
                } else {
                    vocab::OWL_ALL_VALUES_FROM
                };
                self.add(x.clone(), q, rv);
                x.into()
            }
            C::DataHasValue(p, l) => {
                let x = self.typed(vocab::OWL_RESTRICTION);
                self.add(x.clone(), vocab::OWL_ON_PROPERTY, p.0.clone());
                self.add(x.clone(), vocab::OWL_HAS_VALUE, l.0.clone());
                x.into()
            }
            C::DataMinCardinality(n, p, r)
            | C::DataMaxCardinality(n, p, r)
            | C::DataExactCardinality(n, p, r) => {
                let rv = r.as_ref().map(|r| self.data_range(r));
                let x = self.typed(vocab::OWL_RESTRICTION);
                self.add(x.clone(), vocab::OWL_ON_PROPERTY, p.0.clone());
                let (plain, qualified) = match c {
                    C::DataMinCardinality(..) => (
                        vocab::OWL_MIN_CARDINALITY,
                        vocab::OWL_MIN_QUALIFIED_CARDINALITY,
                    ),
                    C::DataMaxCardinality(..) => (
                        vocab::OWL_MAX_CARDINALITY,
                        vocab::OWL_MAX_QUALIFIED_CARDINALITY,
                    ),
                    _ => (vocab::OWL_CARDINALITY, vocab::OWL_QUALIFIED_CARDINALITY),
                };
                match rv {
                    Some(rv) => {
                        self.add(x.clone(), qualified, nonneg(*n));
                        self.add(x.clone(), vocab::OWL_ON_DATA_RANGE, rv);
                    }
                    None => self.add(x.clone(), plain, nonneg(*n)),
                }
                x.into()
            }
        }
    }

    fn annotation_value(v: &AnnotationValue) -> Term {
        match v {
            AnnotationValue::Iri(i) => i.clone().into(),
            AnnotationValue::Literal(l) => l.0.clone().into(),
            AnnotationValue::Anonymous(a) => BlankNode::new_unchecked(a.label()).into(),
        }
    }

    /// TANN(ann, y) of the mapping's Table 2.
    pub fn annotation(&mut self, a: &Annotation, y: &NamedOrBlankNode) {
        let v = Self::annotation_value(&a.value);
        self.add(y.clone(), a.property.as_str(), v.clone());
        if !a.annotations.is_empty() {
            let x = self.typed(vocab::OWL_ANNOTATION);
            self.add(x.clone(), vocab::OWL_ANNOTATED_SOURCE, y.clone());
            self.add(
                x.clone(),
                vocab::OWL_ANNOTATED_PROPERTY,
                nn(a.property.as_str()),
            );
            self.add(x.clone(), vocab::OWL_ANNOTATED_TARGET, v);
            let xn: NamedOrBlankNode = x.into();
            for inner in &a.annotations {
                self.annotation(inner, &xn);
            }
        }
    }

    /// The reification of a main triple with the axiom's annotations (§2.3.1).
    fn reify(&mut self, main: &Triple, annotations: &std::collections::BTreeSet<Annotation>) {
        if annotations.is_empty() {
            return;
        }
        let x = self.typed(vocab::OWL_AXIOM);
        self.add(x.clone(), vocab::OWL_ANNOTATED_SOURCE, main.subject.clone());
        self.add(
            x.clone(),
            vocab::OWL_ANNOTATED_PROPERTY,
            main.predicate.clone(),
        );
        self.add(x.clone(), vocab::OWL_ANNOTATED_TARGET, main.object.clone());
        let xn: NamedOrBlankNode = x.into();
        for a in annotations {
            self.annotation(a, &xn);
        }
    }

    /// One main triple, reified when the axiom is annotated.
    fn main(
        &mut self,
        s: impl Into<NamedOrBlankNode>,
        p: &str,
        o: impl Into<Term>,
        annotations: &std::collections::BTreeSet<Annotation>,
    ) {
        let t = Triple::new(s.into(), nn(p), o.into());
        self.triples.push(t.clone());
        self.reify(&t, annotations);
    }

    /// An axiom represented by a blank node of type `t`, with `members` as its list
    /// (§2.3.3).
    fn blank_axiom(
        &mut self,
        t: &str,
        members: Vec<Term>,
        annotations: &std::collections::BTreeSet<Annotation>,
    ) {
        let x = self.typed(t);
        let l = self.list(members);
        self.add(x.clone(), vocab::OWL_MEMBERS, l);
        let xn: NamedOrBlankNode = x.into();
        for a in annotations {
            self.annotation(a, &xn);
        }
    }

    /// Pairwise main triples, each reified with the annotations (§2.3.2).
    fn pairwise(
        &mut self,
        p: &str,
        nodes: Vec<NamedOrBlankNode>,
        annotations: &std::collections::BTreeSet<Annotation>,
    ) {
        if let [only] = nodes.as_slice() {
            self.main(only.clone(), p, only.clone(), annotations);
        }
        for w in nodes.windows(2) {
            self.main(w[0].clone(), p, w[1].clone(), annotations);
        }
    }

    /// The triples of one axiom.
    pub fn axiom(&mut self, ax: &Axiom) {
        use AxiomKind as K;
        let ann = &ax.annotations;
        match &ax.kind {
            K::Declaration(e) => self.main(
                e.iri().clone(),
                vocab::RDF_TYPE,
                nn(e.kind().rdf_type()),
                ann,
            ),
            K::SubClassOf(a, b) => {
                let (a, b) = (self.class_expression(a), self.class_expression(b));
                self.main(a, vocab::RDFS_SUB_CLASS_OF, b, ann);
            }
            K::EquivalentClasses(cs) => {
                let nodes = cs.iter().map(|c| self.class_expression(c)).collect();
                self.pairwise(vocab::OWL_EQUIVALENT_CLASS, nodes, ann);
            }
            K::DisjointClasses(cs) => {
                if cs.len() == 2 {
                    let mut it = cs.iter();
                    let a = self.class_expression(it.next().unwrap());
                    let b = self.class_expression(it.next().unwrap());
                    self.main(a, vocab::OWL_DISJOINT_WITH, b, ann);
                } else {
                    let members = cs.iter().map(|c| self.class_expression(c).into()).collect();
                    self.blank_axiom(vocab::OWL_ALL_DISJOINT_CLASSES, members, ann);
                }
            }
            K::DisjointUnion(c, cs) => {
                let items = cs.iter().map(|c| self.class_expression(c).into()).collect();
                let l = self.list(items);
                self.main(c.0.clone(), vocab::OWL_DISJOINT_UNION_OF, l, ann);
            }
            K::SubObjectPropertyOf(sub, sup) => match sub {
                SubObjectPropertyExpression::Expression(p) => {
                    let (a, b) = (self.ope(p), self.ope(sup));
                    self.main(a, vocab::RDFS_SUB_PROPERTY_OF, b, ann);
                }
                SubObjectPropertyExpression::Chain(ps) => {
                    let s = self.ope(sup);
                    let items = ps.iter().map(|p| self.ope(p).into()).collect();
                    let l = self.list(items);
                    self.main(s, vocab::OWL_PROPERTY_CHAIN_AXIOM, l, ann);
                }
            },
            K::EquivalentObjectProperties(ps) => {
                let nodes = ps.iter().map(|p| self.ope(p)).collect();
                self.pairwise(vocab::OWL_EQUIVALENT_PROPERTY, nodes, ann);
            }
            K::DisjointObjectProperties(ps) => {
                if ps.len() == 2 {
                    let mut it = ps.iter();
                    let a = self.ope(it.next().unwrap());
                    let b = self.ope(it.next().unwrap());
                    self.main(a, vocab::OWL_PROPERTY_DISJOINT_WITH, b, ann);
                } else {
                    let members = ps.iter().map(|p| self.ope(p).into()).collect();
                    self.blank_axiom(vocab::OWL_ALL_DISJOINT_PROPERTIES, members, ann);
                }
            }
            K::InverseObjectProperties(a, b) => {
                let (a, b) = (self.ope(a), self.ope(b));
                self.main(a, vocab::OWL_INVERSE_OF, b, ann);
            }
            K::ObjectPropertyDomain(p, c) => {
                let (p, c) = (self.ope(p), self.class_expression(c));
                self.main(p, vocab::RDFS_DOMAIN, c, ann);
            }
            K::ObjectPropertyRange(p, c) => {
                let (p, c) = (self.ope(p), self.class_expression(c));
                self.main(p, vocab::RDFS_RANGE, c, ann);
            }
            K::FunctionalObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_FUNCTIONAL_PROPERTY, ann)
            }
            K::InverseFunctionalObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_INVERSE_FUNCTIONAL_PROPERTY, ann)
            }
            K::ReflexiveObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_REFLEXIVE_PROPERTY, ann)
            }
            K::IrreflexiveObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_IRREFLEXIVE_PROPERTY, ann)
            }
            K::SymmetricObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_SYMMETRIC_PROPERTY, ann)
            }
            K::AsymmetricObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_ASYMMETRIC_PROPERTY, ann)
            }
            K::TransitiveObjectProperty(p) => {
                self.characteristic(p, vocab::OWL_TRANSITIVE_PROPERTY, ann)
            }
            K::SubDataPropertyOf(a, b) => {
                self.main(a.0.clone(), vocab::RDFS_SUB_PROPERTY_OF, b.0.clone(), ann)
            }
            K::EquivalentDataProperties(ps) => {
                let nodes = ps.iter().map(|p| p.0.clone().into()).collect();
                self.pairwise(vocab::OWL_EQUIVALENT_PROPERTY, nodes, ann);
            }
            K::DisjointDataProperties(ps) => {
                if ps.len() == 2 {
                    let mut it = ps.iter();
                    let a = it.next().unwrap().0.clone();
                    let b = it.next().unwrap().0.clone();
                    self.main(a, vocab::OWL_PROPERTY_DISJOINT_WITH, b, ann);
                } else {
                    let members = ps.iter().map(|p| p.0.clone().into()).collect();
                    self.blank_axiom(vocab::OWL_ALL_DISJOINT_PROPERTIES, members, ann);
                }
            }
            K::DataPropertyDomain(p, c) => {
                let c = self.class_expression(c);
                self.main(p.0.clone(), vocab::RDFS_DOMAIN, c, ann);
            }
            K::DataPropertyRange(p, r) => {
                let r = self.data_range(r);
                self.main(p.0.clone(), vocab::RDFS_RANGE, r, ann);
            }
            K::FunctionalDataProperty(p) => self.main(
                p.0.clone(),
                vocab::RDF_TYPE,
                nn(vocab::OWL_FUNCTIONAL_PROPERTY),
                ann,
            ),
            K::DatatypeDefinition(d, r) => {
                let r = self.data_range(r);
                self.main(d.0.clone(), vocab::OWL_EQUIVALENT_CLASS, r, ann);
            }
            K::HasKey(c, ops, dps) => {
                let c = self.class_expression(c);
                let mut items: Vec<Term> = ops.iter().map(|p| self.ope(p).into()).collect();
                items.extend(dps.iter().map(|p| Term::from(p.0.clone())));
                let l = self.list(items);
                self.main(c, vocab::OWL_HAS_KEY, l, ann);
            }
            K::SameIndividual(is) => {
                let nodes = is.iter().map(individual_term).collect();
                self.pairwise(vocab::OWL_SAME_AS, nodes, ann);
            }
            K::DifferentIndividuals(is) => {
                if is.len() == 2 {
                    let mut it = is.iter();
                    let a = individual_term(it.next().unwrap());
                    let b = individual_term(it.next().unwrap());
                    self.main(a, vocab::OWL_DIFFERENT_FROM, b, ann);
                } else {
                    let members = is.iter().map(|i| individual_term(i).into()).collect();
                    self.blank_axiom(vocab::OWL_ALL_DIFFERENT, members, ann);
                }
            }
            K::ClassAssertion(c, i) => {
                let c = self.class_expression(c);
                self.main(individual_term(i), vocab::RDF_TYPE, c, ann);
            }
            K::ObjectPropertyAssertion(p, a, b) => match p {
                ObjectPropertyExpression::Property(p) => {
                    self.main(individual_term(a), p.as_str(), individual_term(b), ann)
                }
                ObjectPropertyExpression::InverseOf(p) => {
                    self.main(individual_term(b), p.as_str(), individual_term(a), ann)
                }
            },
            K::NegativeObjectPropertyAssertion(p, a, b) => {
                let pn = self.ope(p);
                let x = self.typed(vocab::OWL_NEGATIVE_PROPERTY_ASSERTION);
                self.add(x.clone(), vocab::OWL_SOURCE_INDIVIDUAL, individual_term(a));
                self.add(x.clone(), vocab::OWL_ASSERTION_PROPERTY, pn);
                self.add(x.clone(), vocab::OWL_TARGET_INDIVIDUAL, individual_term(b));
                let xn: NamedOrBlankNode = x.into();
                for a in ann {
                    self.annotation(a, &xn);
                }
            }
            K::DataPropertyAssertion(p, a, l) => {
                self.main(individual_term(a), p.as_str(), l.0.clone(), ann)
            }
            K::NegativeDataPropertyAssertion(p, a, l) => {
                let x = self.typed(vocab::OWL_NEGATIVE_PROPERTY_ASSERTION);
                self.add(x.clone(), vocab::OWL_SOURCE_INDIVIDUAL, individual_term(a));
                self.add(x.clone(), vocab::OWL_ASSERTION_PROPERTY, p.0.clone());
                self.add(x.clone(), vocab::OWL_TARGET_VALUE, l.0.clone());
                let xn: NamedOrBlankNode = x.into();
                for a in ann {
                    self.annotation(a, &xn);
                }
            }
            K::AnnotationAssertion(p, s, v) => {
                let s: NamedOrBlankNode = match s {
                    AnnotationSubject::Iri(i) => i.clone().into(),
                    AnnotationSubject::Anonymous(a) => BlankNode::new_unchecked(a.label()).into(),
                };
                let v = Self::annotation_value(v);
                self.main(s, p.as_str(), v, ann);
            }
            K::SubAnnotationPropertyOf(a, b) => {
                self.main(a.0.clone(), vocab::RDFS_SUB_PROPERTY_OF, b.0.clone(), ann)
            }
            K::AnnotationPropertyDomain(p, i) => {
                self.main(p.0.clone(), vocab::RDFS_DOMAIN, i.clone(), ann)
            }
            K::AnnotationPropertyRange(p, i) => {
                self.main(p.0.clone(), vocab::RDFS_RANGE, i.clone(), ann)
            }
        }
    }

    fn characteristic(
        &mut self,
        p: &ObjectPropertyExpression,
        t: &str,
        ann: &std::collections::BTreeSet<Annotation>,
    ) {
        let p = self.ope(p);
        self.main(p, vocab::RDF_TYPE, nn(t), ann);
    }

    /// The header's triples, with `node` as the ontology node.
    pub fn header(&mut self, h: &Header, node: &NamedOrBlankNode) {
        self.add(node.clone(), vocab::RDF_TYPE, nn(vocab::OWL_ONTOLOGY));
        if let Some(v) = &h.version_iri {
            self.add(node.clone(), vocab::OWL_VERSION_IRI, v.clone());
        }
        for i in &h.imports {
            self.add(node.clone(), vocab::OWL_IMPORTS, i.clone());
        }
        for a in &h.annotations {
            self.annotation(a, node);
        }
    }
}

/// A blank-node allocator that labels its nodes `{prefix}{n}`.
pub fn counter(prefix: &str) -> impl FnMut() -> BlankNode + use<> {
    let prefix = prefix.to_string();
    let mut n = 0u64;
    move || {
        n += 1;
        BlankNode::new_unchecked(format!("{prefix}{n}"))
    }
}

/// The triples of one axiom with blank nodes from `fresh`.
pub fn axiom_triples(ax: &Axiom, fresh: &mut dyn FnMut() -> BlankNode) -> Vec<Triple> {
    let mut w = Writer::new(fresh);
    w.axiom(ax);
    w.triples
}

/// The triples of a whole ontology: its header (on its IRI, or a blank node for an
/// anonymous ontology) and its axioms.
pub fn ontology_triples<'a>(
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
    fresh: &mut dyn FnMut() -> BlankNode,
) -> Vec<Triple> {
    let mut w = Writer::new(fresh);
    let node: NamedOrBlankNode = match &header.iri {
        Some(i) => i.clone().into(),
        None => w.blank().into(),
    };
    w.header(header, &node);
    for ax in axioms {
        w.axiom(ax);
    }
    w.triples
}
