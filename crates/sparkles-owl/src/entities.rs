//! Entity accessors over a read ontology (spec F21 §2.1): the entities by kind, an
//! entity's labels and annotations, the class hierarchy, the domains, ranges and
//! hierarchy of properties, and the types of individuals.
//!
//! Each accessor answers from the axioms the read holds. For an ontology read from a
//! store, those are the terminology and the assertions that a read keeps eagerly, such
//! as annotated assertions and class assertions of class expressions; the assertions
//! that stay in the store come from the store handle's streaming accessors. An accessor
//! goes over the axioms once per call, and its answer is sorted and lists each value
//! once. With an inferred read, the answers include the materialized closure.
//!
//! ```
//! use sparkles_owl::{read, model::*};
//! use oxrdf::{NamedNode, Triple};
//!
//! let n = |s: &str| NamedNode::new_unchecked(s);
//! let ex = |s: &str| n(&format!("http://ex.org/{s}"));
//! let triples = vec![
//!     Triple::new(ex("Professor"), n(sparkles_owl::vocab::RDFS_SUB_CLASS_OF), ex("Person")),
//! ];
//! let o = read::read_graph(triples, read::ReadOptions::default());
//! let prof = o.class("http://ex.org/Professor").unwrap();
//! assert_eq!(o.super_classes(&prof), vec![ClassExpression::from(Class::new("http://ex.org/Person"))]);
//! assert_eq!(o.classes().len(), 2);
//! ```

use crate::model::*;
use crate::read::Ontology;
use crate::vocab;
use std::collections::BTreeSet;

type Ope = ObjectPropertyExpression;

fn sorted<T: Ord>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    items
        .into_iter()
        .collect::<BTreeSet<T>>()
        .into_iter()
        .collect()
}

/// The characteristics an object property can have, by the type of their axiom.
pub const OBJECT_PROPERTY_CHARACTERISTICS: [AxiomType; 7] = [
    AxiomType::FunctionalObjectProperty,
    AxiomType::InverseFunctionalObjectProperty,
    AxiomType::ReflexiveObjectProperty,
    AxiomType::IrreflexiveObjectProperty,
    AxiomType::SymmetricObjectProperty,
    AxiomType::AsymmetricObjectProperty,
    AxiomType::TransitiveObjectProperty,
];

impl Ontology {
    /// The kinds `iri` has: declared, built in, or in a lenient read typed by its use.
    pub fn kinds(&self, iri: &str) -> Vec<EntityKind> {
        self.typing.kinds(iri).kinds().collect()
    }

    /// The entity of kind `kind` that `iri` names, when the ontology declares it, it is
    /// built in, or a lenient read typed it so by its use.
    pub fn entity(&self, kind: EntityKind, iri: &str) -> Option<Entity> {
        self.typing
            .is(iri, kind)
            .then(|| Entity::of_kind(kind, crate::model::iri(iri)))
    }

    /// The entities of `kind` the ontology declares or, in a lenient read, types by use,
    /// in IRI order. Built-in entities are listed when the ontology declares them.
    pub fn entities(&self, kind: EntityKind) -> Vec<Entity> {
        let declared = self.typing.declared.iter();
        let inferred = self
            .typing
            .inferred
            .iter()
            .filter(|_| self.mode == crate::read::Mode::Lenient);
        sorted(
            declared
                .chain(inferred)
                .filter(|(_, k)| k.has(kind))
                .map(|(i, _)| Entity::of_kind(kind, crate::model::iri(i.as_str()))),
        )
    }

    /// The classes of [`entities`](Self::entities).
    pub fn classes(&self) -> Vec<Class> {
        self.entities(EntityKind::Class)
            .into_iter()
            .filter_map(|e| match e {
                Entity::Class(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    /// The object properties of [`entities`](Self::entities).
    pub fn object_properties(&self) -> Vec<ObjectProperty> {
        self.entities(EntityKind::ObjectProperty)
            .into_iter()
            .filter_map(|e| match e {
                Entity::ObjectProperty(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// The data properties of [`entities`](Self::entities).
    pub fn data_properties(&self) -> Vec<DataProperty> {
        self.entities(EntityKind::DataProperty)
            .into_iter()
            .filter_map(|e| match e {
                Entity::DataProperty(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// The annotation properties of [`entities`](Self::entities).
    pub fn annotation_properties(&self) -> Vec<AnnotationProperty> {
        self.entities(EntityKind::AnnotationProperty)
            .into_iter()
            .filter_map(|e| match e {
                Entity::AnnotationProperty(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// The datatypes of [`entities`](Self::entities).
    pub fn datatypes(&self) -> Vec<Datatype> {
        self.entities(EntityKind::Datatype)
            .into_iter()
            .filter_map(|e| match e {
                Entity::Datatype(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    /// The named individuals of [`entities`](Self::entities). A store read lists those
    /// its terminology declares or types; the individuals of the assertions left in the
    /// store come from the store handle's streams.
    pub fn named_individuals(&self) -> Vec<NamedIndividual> {
        self.entities(EntityKind::NamedIndividual)
            .into_iter()
            .filter_map(|e| match e {
                Entity::NamedIndividual(i) => Some(i),
                _ => None,
            })
            .collect()
    }

    /// The class `iri` names, as [`entity`](Self::entity) finds it.
    pub fn class(&self, iri: &str) -> Option<Class> {
        self.entity(EntityKind::Class, iri).map(|_| Class::new(iri))
    }

    /// The object property `iri` names.
    pub fn object_property(&self, iri: &str) -> Option<ObjectProperty> {
        self.entity(EntityKind::ObjectProperty, iri)
            .map(|_| ObjectProperty::new(iri))
    }

    /// The data property `iri` names.
    pub fn data_property(&self, iri: &str) -> Option<DataProperty> {
        self.entity(EntityKind::DataProperty, iri)
            .map(|_| DataProperty::new(iri))
    }

    /// The annotation property `iri` names.
    pub fn annotation_property(&self, iri: &str) -> Option<AnnotationProperty> {
        self.entity(EntityKind::AnnotationProperty, iri)
            .map(|_| AnnotationProperty::new(iri))
    }

    /// The datatype `iri` names.
    pub fn datatype(&self, iri: &str) -> Option<Datatype> {
        self.entity(EntityKind::Datatype, iri)
            .map(|_| Datatype::new(iri))
    }

    /// The named individual `iri` names.
    pub fn named_individual(&self, iri: &str) -> Option<NamedIndividual> {
        self.entity(EntityKind::NamedIndividual, iri)
            .map(|_| NamedIndividual::new(iri))
    }

    // ------------------------------------------------------------ annotations ------

    /// The annotations of the annotation assertions about `iri`, each with the
    /// annotations of its assertion.
    pub fn annotations(&self, iri: &str) -> Vec<Annotation> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::AnnotationAssertion(p, AnnotationSubject::Iri(s), v)
                if s.as_str() == iri =>
            {
                Some(Annotation {
                    property: p.clone(),
                    value: v.clone(),
                    annotations: ax.annotations.clone(),
                })
            }
            _ => None,
        }))
    }

    /// The `rdfs:label` literals of `iri`.
    pub fn labels(&self, iri: &str) -> Vec<Literal> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::AnnotationAssertion(
                p,
                AnnotationSubject::Iri(s),
                AnnotationValue::Literal(l),
            ) if s.as_str() == iri && p.as_str() == vocab::RDFS_LABEL => Some(l.clone()),
            _ => None,
        }))
    }

    // ---------------------------------------------------------------- classes ------

    /// The superclasses `SubClassOf` axioms give `c`.
    pub fn super_classes(&self, c: impl Into<ClassExpression>) -> Vec<ClassExpression> {
        let c = c.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubClassOf(a, b) if *a == c => Some(b.clone()),
            _ => None,
        }))
    }

    /// The subclasses `SubClassOf` axioms give `c`.
    pub fn sub_classes(&self, c: impl Into<ClassExpression>) -> Vec<ClassExpression> {
        let c = c.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubClassOf(a, b) if *b == c => Some(a.clone()),
            _ => None,
        }))
    }

    /// The other operands of the `EquivalentClasses` axioms that hold `c`.
    pub fn equivalent_classes(&self, c: impl Into<ClassExpression>) -> Vec<ClassExpression> {
        let c = c.into();
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::EquivalentClasses(cs) if cs.contains(&c) => {
                cs.iter().filter(|x| **x != c).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The other operands of the `DisjointClasses` and `DisjointUnion` axioms that hold
    /// `c`.
    pub fn disjoint_classes(&self, c: impl Into<ClassExpression>) -> Vec<ClassExpression> {
        let c = c.into();
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::DisjointClasses(cs) | AxiomKind::DisjointUnion(_, cs) if cs.contains(&c) => {
                cs.iter().filter(|x| **x != c).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The operands of the `DisjointUnion` axioms that define the class `c`.
    pub fn disjoint_unions(&self, c: &Class) -> Vec<BTreeSet<ClassExpression>> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::DisjointUnion(x, cs) if x == c => Some(cs.clone()),
            _ => None,
        }))
    }

    /// The keys of `HasKey` axioms on `c`: their object and data properties.
    pub fn keys(
        &self,
        c: impl Into<ClassExpression>,
    ) -> Vec<(BTreeSet<Ope>, BTreeSet<DataProperty>)> {
        let c = c.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::HasKey(x, o, d) if *x == c => Some((o.clone(), d.clone())),
            _ => None,
        }))
    }

    // -------------------------------------------------------- object properties ------

    /// The domains `ObjectPropertyDomain` axioms give `p`.
    pub fn object_property_domains(&self, p: impl Into<Ope>) -> Vec<ClassExpression> {
        let p = p.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::ObjectPropertyDomain(x, c) if *x == p => Some(c.clone()),
            _ => None,
        }))
    }

    /// The ranges `ObjectPropertyRange` axioms give `p`.
    pub fn object_property_ranges(&self, p: impl Into<Ope>) -> Vec<ClassExpression> {
        let p = p.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::ObjectPropertyRange(x, c) if *x == p => Some(c.clone()),
            _ => None,
        }))
    }

    /// The superproperties `SubObjectPropertyOf` axioms give `p`, without chains.
    pub fn super_object_properties(&self, p: impl Into<Ope>) -> Vec<Ope> {
        let p = p.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(x), b)
                if *x == p =>
            {
                Some(b.clone())
            }
            _ => None,
        }))
    }

    /// The subproperties `SubObjectPropertyOf` axioms give `p`: expressions and chains.
    pub fn sub_object_properties(&self, p: impl Into<Ope>) -> Vec<SubObjectPropertyExpression> {
        let p = p.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubObjectPropertyOf(a, b) if *b == p => Some(a.clone()),
            _ => None,
        }))
    }

    /// The other operands of the `EquivalentObjectProperties` axioms that hold `p`.
    pub fn equivalent_object_properties(&self, p: impl Into<Ope>) -> Vec<Ope> {
        let p = p.into();
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::EquivalentObjectProperties(ps) if ps.contains(&p) => {
                ps.iter().filter(|x| **x != p).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The other operands of the `DisjointObjectProperties` axioms that hold `p`.
    pub fn disjoint_object_properties(&self, p: impl Into<Ope>) -> Vec<Ope> {
        let p = p.into();
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::DisjointObjectProperties(ps) if ps.contains(&p) => {
                ps.iter().filter(|x| **x != p).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The properties that `InverseObjectProperties` axioms make inverse to `p`.
    pub fn inverse_object_properties(&self, p: impl Into<Ope>) -> Vec<Ope> {
        let p = p.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::InverseObjectProperties(a, b) if *a == p => Some(b.clone()),
            AxiomKind::InverseObjectProperties(a, b) if *b == p => Some(a.clone()),
            _ => None,
        }))
    }

    /// The characteristics of `p`, by the types of the axioms that state them, such as
    /// [`AxiomType::TransitiveObjectProperty`].
    pub fn object_property_characteristics(&self, p: impl Into<Ope>) -> Vec<AxiomType> {
        use AxiomKind as K;
        let p = p.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            K::FunctionalObjectProperty(x)
            | K::InverseFunctionalObjectProperty(x)
            | K::ReflexiveObjectProperty(x)
            | K::IrreflexiveObjectProperty(x)
            | K::SymmetricObjectProperty(x)
            | K::AsymmetricObjectProperty(x)
            | K::TransitiveObjectProperty(x)
                if *x == p =>
            {
                Some(ax.axiom_type())
            }
            _ => None,
        }))
    }

    // ---------------------------------------------------------- data properties ------

    /// The domains `DataPropertyDomain` axioms give `p`.
    pub fn data_property_domains(&self, p: &DataProperty) -> Vec<ClassExpression> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::DataPropertyDomain(x, c) if x == p => Some(c.clone()),
            _ => None,
        }))
    }

    /// The ranges `DataPropertyRange` axioms give `p`.
    pub fn data_property_ranges(&self, p: &DataProperty) -> Vec<DataRange> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::DataPropertyRange(x, r) if x == p => Some(r.clone()),
            _ => None,
        }))
    }

    /// The superproperties `SubDataPropertyOf` axioms give `p`.
    pub fn super_data_properties(&self, p: &DataProperty) -> Vec<DataProperty> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubDataPropertyOf(a, b) if a == p => Some(b.clone()),
            _ => None,
        }))
    }

    /// The subproperties `SubDataPropertyOf` axioms give `p`.
    pub fn sub_data_properties(&self, p: &DataProperty) -> Vec<DataProperty> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubDataPropertyOf(a, b) if b == p => Some(a.clone()),
            _ => None,
        }))
    }

    /// The other operands of the `EquivalentDataProperties` axioms that hold `p`.
    pub fn equivalent_data_properties(&self, p: &DataProperty) -> Vec<DataProperty> {
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::EquivalentDataProperties(ps) if ps.contains(p) => {
                ps.iter().filter(|x| *x != p).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The other operands of the `DisjointDataProperties` axioms that hold `p`.
    pub fn disjoint_data_properties(&self, p: &DataProperty) -> Vec<DataProperty> {
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::DisjointDataProperties(ps) if ps.contains(p) => {
                ps.iter().filter(|x| *x != p).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// Whether a `FunctionalDataProperty` axiom states that `p` is functional.
    pub fn is_functional_data_property(&self, p: &DataProperty) -> bool {
        self.axiom_set()
            .any(|ax| matches!(&ax.kind, AxiomKind::FunctionalDataProperty(x) if x == p))
    }

    // ---------------------------------------------------- annotation properties ------

    /// The superproperties `SubAnnotationPropertyOf` axioms give `p`.
    pub fn super_annotation_properties(&self, p: &AnnotationProperty) -> Vec<AnnotationProperty> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubAnnotationPropertyOf(a, b) if a == p => Some(b.clone()),
            _ => None,
        }))
    }

    /// The subproperties `SubAnnotationPropertyOf` axioms give `p`.
    pub fn sub_annotation_properties(&self, p: &AnnotationProperty) -> Vec<AnnotationProperty> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::SubAnnotationPropertyOf(a, b) if b == p => Some(a.clone()),
            _ => None,
        }))
    }

    /// The domains `AnnotationPropertyDomain` axioms give `p`.
    pub fn annotation_property_domains(&self, p: &AnnotationProperty) -> Vec<Iri> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::AnnotationPropertyDomain(x, i) if x == p => Some(i.clone()),
            _ => None,
        }))
    }

    /// The ranges `AnnotationPropertyRange` axioms give `p`.
    pub fn annotation_property_ranges(&self, p: &AnnotationProperty) -> Vec<Iri> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::AnnotationPropertyRange(x, i) if x == p => Some(i.clone()),
            _ => None,
        }))
    }

    // -------------------------------------------------------------- datatypes ------

    /// The data ranges `DatatypeDefinition` axioms define `d` as.
    pub fn datatype_definitions(&self, d: &Datatype) -> Vec<DataRange> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::DatatypeDefinition(x, r) if x == d => Some(r.clone()),
            _ => None,
        }))
    }

    // ------------------------------------------------------------ individuals ------

    /// The classes of the `ClassAssertion` axioms about `i` that the read holds. A
    /// store read holds class assertions of class expressions and annotated ones; the
    /// store handle streams the others.
    pub fn types(&self, i: &Individual) -> Vec<ClassExpression> {
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::ClassAssertion(c, x) if x == i => Some(c.clone()),
            _ => None,
        }))
    }

    /// The individuals of the `ClassAssertion` axioms of `c` that the read holds.
    pub fn instances(&self, c: impl Into<ClassExpression>) -> Vec<Individual> {
        let c = c.into();
        sorted(self.axiom_set().filter_map(|ax| match &ax.kind {
            AxiomKind::ClassAssertion(x, i) if *x == c => Some(i.clone()),
            _ => None,
        }))
    }

    /// The other operands of the `SameIndividual` axioms that hold `i`.
    pub fn same_individuals(&self, i: &Individual) -> Vec<Individual> {
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::SameIndividual(is) if is.contains(i) => {
                is.iter().filter(|x| *x != i).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The other operands of the `DifferentIndividuals` axioms that hold `i`.
    pub fn different_individuals(&self, i: &Individual) -> Vec<Individual> {
        sorted(self.axiom_set().flat_map(|ax| match &ax.kind {
            AxiomKind::DifferentIndividuals(is) if is.contains(i) => {
                is.iter().filter(|x| *x != i).cloned().collect()
            }
            _ => Vec::new(),
        }))
    }

    /// The object and data property assertions about `i` as their subject, positive
    /// and negative, that the read holds.
    pub fn property_assertions(&self, i: &Individual) -> Vec<Axiom> {
        use AxiomKind as K;
        sorted(
            self.axiom_set()
                .filter(|ax| match &ax.kind {
                    K::ObjectPropertyAssertion(_, s, _)
                    | K::NegativeObjectPropertyAssertion(_, s, _)
                    | K::DataPropertyAssertion(_, s, _)
                    | K::NegativeDataPropertyAssertion(_, s, _) => s == i,
                    _ => false,
                })
                .cloned(),
        )
    }
}
