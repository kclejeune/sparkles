//! The OWL 2 structural specification as Rust values: entities, property and class
//! expressions, data ranges, axioms, annotations and the ontology header.
//!
//! Abstract classes of the specification are enums and n-ary operands are sets, so two
//! values are equal exactly when the specification calls them structurally equivalent.
//! Sets are `BTreeSet`s, which also gives every value a stable order for output.

use crate::vocab;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

/// An IRI.
pub type Iri = oxrdf::NamedNode;

/// An error for a value that the model cannot hold, such as an invalid IRI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError(pub String);

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}

/// An IRI from a string the caller vouches for. [`parse_iri`] checks the syntax.
pub fn iri(s: impl Into<String>) -> Iri {
    oxrdf::NamedNode::new_unchecked(s)
}

/// An IRI, checked for syntax.
pub fn parse_iri(s: &str) -> Result<Iri, ModelError> {
    oxrdf::NamedNode::new(s).map_err(|e| ModelError(format!("invalid IRI <{s}>: {e}")))
}

macro_rules! entity {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub Iri);

        impl $name {
            /// The entity with this IRI, which the caller vouches for. `parse` checks it.
            pub fn new(iri: impl Into<String>) -> $name {
                $name(iri_unchecked(iri.into()))
            }

            /// The entity with this IRI, checked for syntax.
            pub fn parse(iri: &str) -> Result<$name, ModelError> {
                parse_iri(iri).map($name)
            }

            pub fn iri(&self) -> &Iri {
                &self.0
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl From<Iri> for $name {
            fn from(i: Iri) -> $name {
                $name(i)
            }
        }
    };
}

fn iri_unchecked(s: String) -> Iri {
    oxrdf::NamedNode::new_unchecked(s)
}

entity!(
    /// A class (structural specification §5.1).
    Class
);
entity!(
    /// A datatype (§5.2).
    Datatype
);
entity!(
    /// An object property (§5.3).
    ObjectProperty
);
entity!(
    /// A data property (§5.4).
    DataProperty
);
entity!(
    /// An annotation property (§5.5).
    AnnotationProperty
);
entity!(
    /// A named individual (§5.6.1).
    NamedIndividual
);

impl Class {
    pub fn thing() -> Class {
        Class::new(vocab::OWL_THING)
    }
    pub fn nothing() -> Class {
        Class::new(vocab::OWL_NOTHING)
    }
}

impl Datatype {
    pub fn literal() -> Datatype {
        Datatype::new(vocab::RDFS_LITERAL)
    }
}

impl ObjectProperty {
    pub fn top() -> ObjectProperty {
        ObjectProperty::new(vocab::OWL_TOP_OBJECT_PROPERTY)
    }
    pub fn bottom() -> ObjectProperty {
        ObjectProperty::new(vocab::OWL_BOTTOM_OBJECT_PROPERTY)
    }
}

impl DataProperty {
    pub fn top() -> DataProperty {
        DataProperty::new(vocab::OWL_TOP_DATA_PROPERTY)
    }
    pub fn bottom() -> DataProperty {
        DataProperty::new(vocab::OWL_BOTTOM_DATA_PROPERTY)
    }
}

/// An anonymous individual (§5.6.2): a blank node label local to the ontology read.
/// Read from a store, the label is the store's own label for the node, so a later
/// write can name the same node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnonymousIndividual(pub String);

impl AnonymousIndividual {
    pub fn new(label: impl Into<String>) -> AnonymousIndividual {
        AnonymousIndividual(label.into())
    }
    pub fn label(&self) -> &str {
        &self.0
    }
}

/// An individual: named or anonymous.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Individual {
    Named(NamedIndividual),
    Anonymous(AnonymousIndividual),
}

impl Individual {
    pub fn named(iri: impl Into<String>) -> Individual {
        Individual::Named(NamedIndividual::new(iri))
    }
    pub fn anonymous(label: impl Into<String>) -> Individual {
        Individual::Anonymous(AnonymousIndividual::new(label))
    }
}

/// A literal (§5.7). OWL 2 compares literals by lexical form and datatype, which is
/// what `oxrdf::Literal`'s equality does; this wrapper adds the order the model's sets
/// need.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Literal(pub oxrdf::Literal);

impl Literal {
    pub fn simple(value: impl Into<String>) -> Literal {
        Literal(oxrdf::Literal::new_simple_literal(value))
    }
    pub fn typed(value: impl Into<String>, datatype: &str) -> Literal {
        Literal(oxrdf::Literal::new_typed_literal(value, iri(datatype)))
    }
    pub fn lang(value: impl Into<String>, lang: &str) -> Result<Literal, ModelError> {
        oxrdf::Literal::new_language_tagged_literal(value, lang)
            .map(Literal)
            .map_err(|e| ModelError(format!("invalid language tag '{lang}': {e}")))
    }
    pub fn value(&self) -> &str {
        self.0.value()
    }
    pub fn datatype(&self) -> &str {
        self.0.datatype().as_str()
    }
    pub fn language(&self) -> Option<&str> {
        self.0.language()
    }
    pub fn as_rdf(&self) -> &oxrdf::Literal {
        &self.0
    }
}

impl PartialOrd for Literal {
    fn partial_cmp(&self, other: &Literal) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Literal {
    fn cmp(&self, other: &Literal) -> Ordering {
        (self.value(), self.datatype(), self.language()).cmp(&(
            other.value(),
            other.datatype(),
            other.language(),
        ))
    }
}

impl From<oxrdf::Literal> for Literal {
    fn from(l: oxrdf::Literal) -> Literal {
        Literal(l)
    }
}

/// An entity with its kind, as a declaration names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Entity {
    Class(Class),
    Datatype(Datatype),
    ObjectProperty(ObjectProperty),
    DataProperty(DataProperty),
    AnnotationProperty(AnnotationProperty),
    NamedIndividual(NamedIndividual),
}

impl Entity {
    pub fn iri(&self) -> &Iri {
        match self {
            Entity::Class(e) => &e.0,
            Entity::Datatype(e) => &e.0,
            Entity::ObjectProperty(e) => &e.0,
            Entity::DataProperty(e) => &e.0,
            Entity::AnnotationProperty(e) => &e.0,
            Entity::NamedIndividual(e) => &e.0,
        }
    }

    pub fn kind(&self) -> EntityKind {
        match self {
            Entity::Class(_) => EntityKind::Class,
            Entity::Datatype(_) => EntityKind::Datatype,
            Entity::ObjectProperty(_) => EntityKind::ObjectProperty,
            Entity::DataProperty(_) => EntityKind::DataProperty,
            Entity::AnnotationProperty(_) => EntityKind::AnnotationProperty,
            Entity::NamedIndividual(_) => EntityKind::NamedIndividual,
        }
    }

    pub fn of_kind(kind: EntityKind, iri: Iri) -> Entity {
        match kind {
            EntityKind::Class => Entity::Class(Class(iri)),
            EntityKind::Datatype => Entity::Datatype(Datatype(iri)),
            EntityKind::ObjectProperty => Entity::ObjectProperty(ObjectProperty(iri)),
            EntityKind::DataProperty => Entity::DataProperty(DataProperty(iri)),
            EntityKind::AnnotationProperty => Entity::AnnotationProperty(AnnotationProperty(iri)),
            EntityKind::NamedIndividual => Entity::NamedIndividual(NamedIndividual(iri)),
        }
    }

    /// Whether the structural specification declares this entity implicitly (§5.8,
    /// Table 5).
    pub fn is_builtin(&self) -> bool {
        let i = self.iri().as_str();
        match self {
            Entity::Class(_) => i == vocab::OWL_THING || i == vocab::OWL_NOTHING,
            Entity::Datatype(_) => vocab::is_builtin_datatype(i),
            Entity::ObjectProperty(_) => {
                i == vocab::OWL_TOP_OBJECT_PROPERTY || i == vocab::OWL_BOTTOM_OBJECT_PROPERTY
            }
            Entity::DataProperty(_) => {
                i == vocab::OWL_TOP_DATA_PROPERTY || i == vocab::OWL_BOTTOM_DATA_PROPERTY
            }
            Entity::AnnotationProperty(_) => vocab::is_builtin_annotation_property(i),
            Entity::NamedIndividual(_) => false,
        }
    }
}

/// The six kinds of entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityKind {
    Class,
    Datatype,
    ObjectProperty,
    DataProperty,
    AnnotationProperty,
    NamedIndividual,
}

impl EntityKind {
    pub const ALL: [EntityKind; 6] = [
        EntityKind::Class,
        EntityKind::Datatype,
        EntityKind::ObjectProperty,
        EntityKind::DataProperty,
        EntityKind::AnnotationProperty,
        EntityKind::NamedIndividual,
    ];

    /// The functional-style name: `Class`, `Datatype`, `ObjectProperty`, …
    pub fn name(self) -> &'static str {
        match self {
            EntityKind::Class => "Class",
            EntityKind::Datatype => "Datatype",
            EntityKind::ObjectProperty => "ObjectProperty",
            EntityKind::DataProperty => "DataProperty",
            EntityKind::AnnotationProperty => "AnnotationProperty",
            EntityKind::NamedIndividual => "NamedIndividual",
        }
    }

    pub fn parse(s: &str) -> Option<EntityKind> {
        EntityKind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// The type of the declaration's triple in RDF.
    pub fn rdf_type(self) -> &'static str {
        match self {
            EntityKind::Class => vocab::OWL_CLASS,
            EntityKind::Datatype => vocab::RDFS_DATATYPE,
            EntityKind::ObjectProperty => vocab::OWL_OBJECT_PROPERTY,
            EntityKind::DataProperty => vocab::OWL_DATATYPE_PROPERTY,
            EntityKind::AnnotationProperty => vocab::OWL_ANNOTATION_PROPERTY,
            EntityKind::NamedIndividual => vocab::OWL_NAMED_INDIVIDUAL,
        }
    }

    pub fn of_rdf_type(t: &str) -> Option<EntityKind> {
        EntityKind::ALL.into_iter().find(|k| k.rdf_type() == t)
    }
}

/// An object property expression (§6.1): a property or its inverse.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectPropertyExpression {
    Property(ObjectProperty),
    InverseOf(ObjectProperty),
}

impl ObjectPropertyExpression {
    /// The named property the expression is built on.
    pub fn named(&self) -> &ObjectProperty {
        match self {
            ObjectPropertyExpression::Property(p) | ObjectPropertyExpression::InverseOf(p) => p,
        }
    }

    /// INV of the structural specification's §11.1.
    pub fn inverse(&self) -> ObjectPropertyExpression {
        match self {
            ObjectPropertyExpression::Property(p) => ObjectPropertyExpression::InverseOf(p.clone()),
            ObjectPropertyExpression::InverseOf(p) => ObjectPropertyExpression::Property(p.clone()),
        }
    }
}

impl From<ObjectProperty> for ObjectPropertyExpression {
    fn from(p: ObjectProperty) -> ObjectPropertyExpression {
        ObjectPropertyExpression::Property(p)
    }
}

impl From<&ObjectProperty> for ObjectPropertyExpression {
    fn from(p: &ObjectProperty) -> ObjectPropertyExpression {
        ObjectPropertyExpression::Property(p.clone())
    }
}

impl From<&ObjectPropertyExpression> for ObjectPropertyExpression {
    fn from(p: &ObjectPropertyExpression) -> ObjectPropertyExpression {
        p.clone()
    }
}

/// A facet restriction of a datatype restriction.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FacetRestriction {
    pub facet: Iri,
    pub value: Literal,
}

/// A data range (§7).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataRange {
    Datatype(Datatype),
    IntersectionOf(BTreeSet<DataRange>),
    UnionOf(BTreeSet<DataRange>),
    ComplementOf(Box<DataRange>),
    OneOf(BTreeSet<Literal>),
    DatatypeRestriction(Datatype, BTreeSet<FacetRestriction>),
}

impl From<Datatype> for DataRange {
    fn from(d: Datatype) -> DataRange {
        DataRange::Datatype(d)
    }
}

/// A class expression (§8).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClassExpression {
    Class(Class),
    ObjectIntersectionOf(BTreeSet<ClassExpression>),
    ObjectUnionOf(BTreeSet<ClassExpression>),
    ObjectComplementOf(Box<ClassExpression>),
    ObjectOneOf(BTreeSet<Individual>),
    ObjectSomeValuesFrom(ObjectPropertyExpression, Box<ClassExpression>),
    ObjectAllValuesFrom(ObjectPropertyExpression, Box<ClassExpression>),
    ObjectHasValue(ObjectPropertyExpression, Individual),
    ObjectHasSelf(ObjectPropertyExpression),
    ObjectMinCardinality(u32, ObjectPropertyExpression, Option<Box<ClassExpression>>),
    ObjectMaxCardinality(u32, ObjectPropertyExpression, Option<Box<ClassExpression>>),
    ObjectExactCardinality(u32, ObjectPropertyExpression, Option<Box<ClassExpression>>),
    /// The data properties are a list: their number is the data range's arity.
    DataSomeValuesFrom(Vec<DataProperty>, DataRange),
    DataAllValuesFrom(Vec<DataProperty>, DataRange),
    DataHasValue(DataProperty, Literal),
    DataMinCardinality(u32, DataProperty, Option<DataRange>),
    DataMaxCardinality(u32, DataProperty, Option<DataRange>),
    DataExactCardinality(u32, DataProperty, Option<DataRange>),
}

impl ClassExpression {
    /// `ObjectSomeValuesFrom(p c)`.
    pub fn some(p: impl Into<ObjectPropertyExpression>, c: impl Into<ClassExpression>) -> Self {
        ClassExpression::ObjectSomeValuesFrom(p.into(), Box::new(c.into()))
    }
    /// `ObjectAllValuesFrom(p c)`.
    pub fn all(p: impl Into<ObjectPropertyExpression>, c: impl Into<ClassExpression>) -> Self {
        ClassExpression::ObjectAllValuesFrom(p.into(), Box::new(c.into()))
    }
    /// `ObjectIntersectionOf(…)`.
    pub fn and(cs: impl IntoIterator<Item = ClassExpression>) -> Self {
        ClassExpression::ObjectIntersectionOf(cs.into_iter().collect())
    }
    /// `ObjectUnionOf(…)`.
    pub fn or(cs: impl IntoIterator<Item = ClassExpression>) -> Self {
        ClassExpression::ObjectUnionOf(cs.into_iter().collect())
    }
    /// `ObjectComplementOf(c)`.
    pub fn not(c: impl Into<ClassExpression>) -> Self {
        ClassExpression::ObjectComplementOf(Box::new(c.into()))
    }

    /// The class, when the expression is a class.
    pub fn as_class(&self) -> Option<&Class> {
        match self {
            ClassExpression::Class(c) => Some(c),
            _ => None,
        }
    }

    /// The functional-style name of the expression's type.
    pub fn type_name(&self) -> &'static str {
        match self {
            ClassExpression::Class(_) => "Class",
            ClassExpression::ObjectIntersectionOf(_) => "ObjectIntersectionOf",
            ClassExpression::ObjectUnionOf(_) => "ObjectUnionOf",
            ClassExpression::ObjectComplementOf(_) => "ObjectComplementOf",
            ClassExpression::ObjectOneOf(_) => "ObjectOneOf",
            ClassExpression::ObjectSomeValuesFrom(..) => "ObjectSomeValuesFrom",
            ClassExpression::ObjectAllValuesFrom(..) => "ObjectAllValuesFrom",
            ClassExpression::ObjectHasValue(..) => "ObjectHasValue",
            ClassExpression::ObjectHasSelf(_) => "ObjectHasSelf",
            ClassExpression::ObjectMinCardinality(..) => "ObjectMinCardinality",
            ClassExpression::ObjectMaxCardinality(..) => "ObjectMaxCardinality",
            ClassExpression::ObjectExactCardinality(..) => "ObjectExactCardinality",
            ClassExpression::DataSomeValuesFrom(..) => "DataSomeValuesFrom",
            ClassExpression::DataAllValuesFrom(..) => "DataAllValuesFrom",
            ClassExpression::DataHasValue(..) => "DataHasValue",
            ClassExpression::DataMinCardinality(..) => "DataMinCardinality",
            ClassExpression::DataMaxCardinality(..) => "DataMaxCardinality",
            ClassExpression::DataExactCardinality(..) => "DataExactCardinality",
        }
    }
}

impl From<Class> for ClassExpression {
    fn from(c: Class) -> ClassExpression {
        ClassExpression::Class(c)
    }
}

impl From<&Class> for ClassExpression {
    fn from(c: &Class) -> ClassExpression {
        ClassExpression::Class(c.clone())
    }
}

impl From<&ClassExpression> for ClassExpression {
    fn from(c: &ClassExpression) -> ClassExpression {
        c.clone()
    }
}

/// The subproperty side of `SubObjectPropertyOf`: an expression or a chain.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubObjectPropertyExpression {
    Expression(ObjectPropertyExpression),
    Chain(Vec<ObjectPropertyExpression>),
}

/// The subject of an annotation assertion: an IRI or an anonymous individual.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnnotationSubject {
    Iri(Iri),
    Anonymous(AnonymousIndividual),
}

/// The value of an annotation: an IRI, a literal or an anonymous individual.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnnotationValue {
    Iri(Iri),
    Literal(Literal),
    Anonymous(AnonymousIndividual),
}

/// An annotation (§10.1), which may itself be annotated.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Annotation {
    pub property: AnnotationProperty,
    pub value: AnnotationValue,
    pub annotations: BTreeSet<Annotation>,
}

impl Annotation {
    pub fn new(property: AnnotationProperty, value: AnnotationValue) -> Annotation {
        Annotation {
            property,
            value,
            annotations: BTreeSet::new(),
        }
    }

    /// `rdfs:comment` with a plain string.
    pub fn comment(text: &str) -> Annotation {
        Annotation::new(
            AnnotationProperty::new(vocab::RDFS_COMMENT),
            AnnotationValue::Literal(Literal::simple(text)),
        )
    }

    /// `rdfs:label` with a plain string.
    pub fn label(text: &str) -> Annotation {
        Annotation::new(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationValue::Literal(Literal::simple(text)),
        )
    }
}

/// The kind and operands of an axiom (§9), without its annotations.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AxiomKind {
    Declaration(Entity),

    SubClassOf(ClassExpression, ClassExpression),
    EquivalentClasses(BTreeSet<ClassExpression>),
    DisjointClasses(BTreeSet<ClassExpression>),
    DisjointUnion(Class, BTreeSet<ClassExpression>),

    SubObjectPropertyOf(SubObjectPropertyExpression, ObjectPropertyExpression),
    EquivalentObjectProperties(BTreeSet<ObjectPropertyExpression>),
    DisjointObjectProperties(BTreeSet<ObjectPropertyExpression>),
    InverseObjectProperties(ObjectPropertyExpression, ObjectPropertyExpression),
    ObjectPropertyDomain(ObjectPropertyExpression, ClassExpression),
    ObjectPropertyRange(ObjectPropertyExpression, ClassExpression),
    FunctionalObjectProperty(ObjectPropertyExpression),
    InverseFunctionalObjectProperty(ObjectPropertyExpression),
    ReflexiveObjectProperty(ObjectPropertyExpression),
    IrreflexiveObjectProperty(ObjectPropertyExpression),
    SymmetricObjectProperty(ObjectPropertyExpression),
    AsymmetricObjectProperty(ObjectPropertyExpression),
    TransitiveObjectProperty(ObjectPropertyExpression),

    SubDataPropertyOf(DataProperty, DataProperty),
    EquivalentDataProperties(BTreeSet<DataProperty>),
    DisjointDataProperties(BTreeSet<DataProperty>),
    DataPropertyDomain(DataProperty, ClassExpression),
    DataPropertyRange(DataProperty, DataRange),
    FunctionalDataProperty(DataProperty),

    DatatypeDefinition(Datatype, DataRange),
    HasKey(
        ClassExpression,
        BTreeSet<ObjectPropertyExpression>,
        BTreeSet<DataProperty>,
    ),

    SameIndividual(BTreeSet<Individual>),
    DifferentIndividuals(BTreeSet<Individual>),
    ClassAssertion(ClassExpression, Individual),
    ObjectPropertyAssertion(ObjectPropertyExpression, Individual, Individual),
    NegativeObjectPropertyAssertion(ObjectPropertyExpression, Individual, Individual),
    DataPropertyAssertion(DataProperty, Individual, Literal),
    NegativeDataPropertyAssertion(DataProperty, Individual, Literal),

    AnnotationAssertion(AnnotationProperty, AnnotationSubject, AnnotationValue),
    SubAnnotationPropertyOf(AnnotationProperty, AnnotationProperty),
    AnnotationPropertyDomain(AnnotationProperty, Iri),
    AnnotationPropertyRange(AnnotationProperty, Iri),
}

/// The type of an axiom, as its functional-style name gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AxiomType {
    Declaration,
    SubClassOf,
    EquivalentClasses,
    DisjointClasses,
    DisjointUnion,
    SubObjectPropertyOf,
    EquivalentObjectProperties,
    DisjointObjectProperties,
    InverseObjectProperties,
    ObjectPropertyDomain,
    ObjectPropertyRange,
    FunctionalObjectProperty,
    InverseFunctionalObjectProperty,
    ReflexiveObjectProperty,
    IrreflexiveObjectProperty,
    SymmetricObjectProperty,
    AsymmetricObjectProperty,
    TransitiveObjectProperty,
    SubDataPropertyOf,
    EquivalentDataProperties,
    DisjointDataProperties,
    DataPropertyDomain,
    DataPropertyRange,
    FunctionalDataProperty,
    DatatypeDefinition,
    HasKey,
    SameIndividual,
    DifferentIndividuals,
    ClassAssertion,
    ObjectPropertyAssertion,
    NegativeObjectPropertyAssertion,
    DataPropertyAssertion,
    NegativeDataPropertyAssertion,
    AnnotationAssertion,
    SubAnnotationPropertyOf,
    AnnotationPropertyDomain,
    AnnotationPropertyRange,
}

impl AxiomType {
    pub const ALL: [AxiomType; 37] = [
        AxiomType::Declaration,
        AxiomType::SubClassOf,
        AxiomType::EquivalentClasses,
        AxiomType::DisjointClasses,
        AxiomType::DisjointUnion,
        AxiomType::SubObjectPropertyOf,
        AxiomType::EquivalentObjectProperties,
        AxiomType::DisjointObjectProperties,
        AxiomType::InverseObjectProperties,
        AxiomType::ObjectPropertyDomain,
        AxiomType::ObjectPropertyRange,
        AxiomType::FunctionalObjectProperty,
        AxiomType::InverseFunctionalObjectProperty,
        AxiomType::ReflexiveObjectProperty,
        AxiomType::IrreflexiveObjectProperty,
        AxiomType::SymmetricObjectProperty,
        AxiomType::AsymmetricObjectProperty,
        AxiomType::TransitiveObjectProperty,
        AxiomType::SubDataPropertyOf,
        AxiomType::EquivalentDataProperties,
        AxiomType::DisjointDataProperties,
        AxiomType::DataPropertyDomain,
        AxiomType::DataPropertyRange,
        AxiomType::FunctionalDataProperty,
        AxiomType::DatatypeDefinition,
        AxiomType::HasKey,
        AxiomType::SameIndividual,
        AxiomType::DifferentIndividuals,
        AxiomType::ClassAssertion,
        AxiomType::ObjectPropertyAssertion,
        AxiomType::NegativeObjectPropertyAssertion,
        AxiomType::DataPropertyAssertion,
        AxiomType::NegativeDataPropertyAssertion,
        AxiomType::AnnotationAssertion,
        AxiomType::SubAnnotationPropertyOf,
        AxiomType::AnnotationPropertyDomain,
        AxiomType::AnnotationPropertyRange,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AxiomType::Declaration => "Declaration",
            AxiomType::SubClassOf => "SubClassOf",
            AxiomType::EquivalentClasses => "EquivalentClasses",
            AxiomType::DisjointClasses => "DisjointClasses",
            AxiomType::DisjointUnion => "DisjointUnion",
            AxiomType::SubObjectPropertyOf => "SubObjectPropertyOf",
            AxiomType::EquivalentObjectProperties => "EquivalentObjectProperties",
            AxiomType::DisjointObjectProperties => "DisjointObjectProperties",
            AxiomType::InverseObjectProperties => "InverseObjectProperties",
            AxiomType::ObjectPropertyDomain => "ObjectPropertyDomain",
            AxiomType::ObjectPropertyRange => "ObjectPropertyRange",
            AxiomType::FunctionalObjectProperty => "FunctionalObjectProperty",
            AxiomType::InverseFunctionalObjectProperty => "InverseFunctionalObjectProperty",
            AxiomType::ReflexiveObjectProperty => "ReflexiveObjectProperty",
            AxiomType::IrreflexiveObjectProperty => "IrreflexiveObjectProperty",
            AxiomType::SymmetricObjectProperty => "SymmetricObjectProperty",
            AxiomType::AsymmetricObjectProperty => "AsymmetricObjectProperty",
            AxiomType::TransitiveObjectProperty => "TransitiveObjectProperty",
            AxiomType::SubDataPropertyOf => "SubDataPropertyOf",
            AxiomType::EquivalentDataProperties => "EquivalentDataProperties",
            AxiomType::DisjointDataProperties => "DisjointDataProperties",
            AxiomType::DataPropertyDomain => "DataPropertyDomain",
            AxiomType::DataPropertyRange => "DataPropertyRange",
            AxiomType::FunctionalDataProperty => "FunctionalDataProperty",
            AxiomType::DatatypeDefinition => "DatatypeDefinition",
            AxiomType::HasKey => "HasKey",
            AxiomType::SameIndividual => "SameIndividual",
            AxiomType::DifferentIndividuals => "DifferentIndividuals",
            AxiomType::ClassAssertion => "ClassAssertion",
            AxiomType::ObjectPropertyAssertion => "ObjectPropertyAssertion",
            AxiomType::NegativeObjectPropertyAssertion => "NegativeObjectPropertyAssertion",
            AxiomType::DataPropertyAssertion => "DataPropertyAssertion",
            AxiomType::NegativeDataPropertyAssertion => "NegativeDataPropertyAssertion",
            AxiomType::AnnotationAssertion => "AnnotationAssertion",
            AxiomType::SubAnnotationPropertyOf => "SubAnnotationPropertyOf",
            AxiomType::AnnotationPropertyDomain => "AnnotationPropertyDomain",
            AxiomType::AnnotationPropertyRange => "AnnotationPropertyRange",
        }
    }

    pub fn parse(s: &str) -> Option<AxiomType> {
        AxiomType::ALL.into_iter().find(|t| t.name() == s)
    }

    /// Whether axioms of this type are assertions about individuals (§9.6), which a
    /// store ontology reads lazily.
    pub fn is_assertion(self) -> bool {
        matches!(
            self,
            AxiomType::SameIndividual
                | AxiomType::DifferentIndividuals
                | AxiomType::ClassAssertion
                | AxiomType::ObjectPropertyAssertion
                | AxiomType::NegativeObjectPropertyAssertion
                | AxiomType::DataPropertyAssertion
                | AxiomType::NegativeDataPropertyAssertion
        )
    }
}

impl AxiomKind {
    pub fn axiom_type(&self) -> AxiomType {
        use AxiomKind as K;
        match self {
            K::Declaration(_) => AxiomType::Declaration,
            K::SubClassOf(..) => AxiomType::SubClassOf,
            K::EquivalentClasses(_) => AxiomType::EquivalentClasses,
            K::DisjointClasses(_) => AxiomType::DisjointClasses,
            K::DisjointUnion(..) => AxiomType::DisjointUnion,
            K::SubObjectPropertyOf(..) => AxiomType::SubObjectPropertyOf,
            K::EquivalentObjectProperties(_) => AxiomType::EquivalentObjectProperties,
            K::DisjointObjectProperties(_) => AxiomType::DisjointObjectProperties,
            K::InverseObjectProperties(..) => AxiomType::InverseObjectProperties,
            K::ObjectPropertyDomain(..) => AxiomType::ObjectPropertyDomain,
            K::ObjectPropertyRange(..) => AxiomType::ObjectPropertyRange,
            K::FunctionalObjectProperty(_) => AxiomType::FunctionalObjectProperty,
            K::InverseFunctionalObjectProperty(_) => AxiomType::InverseFunctionalObjectProperty,
            K::ReflexiveObjectProperty(_) => AxiomType::ReflexiveObjectProperty,
            K::IrreflexiveObjectProperty(_) => AxiomType::IrreflexiveObjectProperty,
            K::SymmetricObjectProperty(_) => AxiomType::SymmetricObjectProperty,
            K::AsymmetricObjectProperty(_) => AxiomType::AsymmetricObjectProperty,
            K::TransitiveObjectProperty(_) => AxiomType::TransitiveObjectProperty,
            K::SubDataPropertyOf(..) => AxiomType::SubDataPropertyOf,
            K::EquivalentDataProperties(_) => AxiomType::EquivalentDataProperties,
            K::DisjointDataProperties(_) => AxiomType::DisjointDataProperties,
            K::DataPropertyDomain(..) => AxiomType::DataPropertyDomain,
            K::DataPropertyRange(..) => AxiomType::DataPropertyRange,
            K::FunctionalDataProperty(_) => AxiomType::FunctionalDataProperty,
            K::DatatypeDefinition(..) => AxiomType::DatatypeDefinition,
            K::HasKey(..) => AxiomType::HasKey,
            K::SameIndividual(_) => AxiomType::SameIndividual,
            K::DifferentIndividuals(_) => AxiomType::DifferentIndividuals,
            K::ClassAssertion(..) => AxiomType::ClassAssertion,
            K::ObjectPropertyAssertion(..) => AxiomType::ObjectPropertyAssertion,
            K::NegativeObjectPropertyAssertion(..) => AxiomType::NegativeObjectPropertyAssertion,
            K::DataPropertyAssertion(..) => AxiomType::DataPropertyAssertion,
            K::NegativeDataPropertyAssertion(..) => AxiomType::NegativeDataPropertyAssertion,
            K::AnnotationAssertion(..) => AxiomType::AnnotationAssertion,
            K::SubAnnotationPropertyOf(..) => AxiomType::SubAnnotationPropertyOf,
            K::AnnotationPropertyDomain(..) => AxiomType::AnnotationPropertyDomain,
            K::AnnotationPropertyRange(..) => AxiomType::AnnotationPropertyRange,
        }
    }
}

/// An axiom with its annotations. Two axioms are equal when their structure and their
/// annotations are equal (§9).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Axiom {
    pub kind: AxiomKind,
    pub annotations: BTreeSet<Annotation>,
}

impl From<AxiomKind> for Axiom {
    fn from(kind: AxiomKind) -> Axiom {
        Axiom {
            kind,
            annotations: BTreeSet::new(),
        }
    }
}

impl Axiom {
    pub fn new(kind: AxiomKind) -> Axiom {
        kind.into()
    }

    /// The axiom with an annotation added.
    pub fn annotated(mut self, a: Annotation) -> Axiom {
        self.annotations.insert(a);
        self
    }

    pub fn axiom_type(&self) -> AxiomType {
        self.kind.axiom_type()
    }

    /// The axiom without its annotations.
    pub fn without_annotations(&self) -> Axiom {
        Axiom::new(self.kind.clone())
    }

    pub fn declaration(e: Entity) -> Axiom {
        AxiomKind::Declaration(e).into()
    }

    pub fn sub_class_of(sub: impl Into<ClassExpression>, sup: impl Into<ClassExpression>) -> Axiom {
        AxiomKind::SubClassOf(sub.into(), sup.into()).into()
    }

    pub fn equivalent_classes(cs: impl IntoIterator<Item = ClassExpression>) -> Axiom {
        AxiomKind::EquivalentClasses(cs.into_iter().collect()).into()
    }

    pub fn disjoint_classes(cs: impl IntoIterator<Item = ClassExpression>) -> Axiom {
        AxiomKind::DisjointClasses(cs.into_iter().collect()).into()
    }

    pub fn class_assertion(c: impl Into<ClassExpression>, i: Individual) -> Axiom {
        AxiomKind::ClassAssertion(c.into(), i).into()
    }

    /// The entities of the axiom's signature, as their positions type them. Built-in
    /// entities are included.
    pub fn signature(&self) -> BTreeSet<Entity> {
        let mut s = Signature::default();
        s.axiom(&self.kind);
        for a in &self.annotations {
            s.annotation(a);
        }
        s.0
    }

    /// The entities of the axiom's signature, as [`Axiom::signature`] gives them, and the
    /// axiom's [`Shape`].
    pub fn signature_and_shape(&self) -> (BTreeSet<Entity>, Shape) {
        let mut s = Signature::default();
        s.axiom(&self.kind);
        for a in &self.annotations {
            s.annotation(a);
        }
        (s.0, s.1)
    }

    /// The IRIs the axiom mentions in any position, annotations and their values
    /// included.
    pub fn mentions(&self, iri: &str) -> bool {
        fn value(v: &AnnotationValue, iri: &str) -> bool {
            matches!(v, AnnotationValue::Iri(v) if v.as_str() == iri)
        }
        fn annotation(a: &Annotation, iri: &str) -> bool {
            value(&a.value, iri) || a.annotations.iter().any(|n| annotation(n, iri))
        }
        self.signature().iter().any(|e| e.iri().as_str() == iri)
            || self.annotations.iter().any(|a| annotation(a, iri))
            || match &self.kind {
                AxiomKind::AnnotationAssertion(_, s, v) => {
                    matches!(s, AnnotationSubject::Iri(s) if s.as_str() == iri) || value(v, iri)
                }
                AxiomKind::AnnotationPropertyDomain(_, d)
                | AxiomKind::AnnotationPropertyRange(_, d) => d.as_str() == iri,
                _ => false,
            }
    }

    /// The axioms the reverse mapping reads from this axiom's triples: an equivalence
    /// or `SameIndividual` of more than two operands is written as pairwise triples and
    /// read as one binary axiom per pair, and an object property assertion of an inverse
    /// property is written and read as the assertion of the named property with the
    /// individuals swapped. Every other axiom is its own normal form.
    pub fn normal_forms(&self) -> Vec<Axiom> {
        fn pairs<T: Clone + Ord>(
            set: &BTreeSet<T>,
            annotations: &BTreeSet<Annotation>,
            f: impl Fn(BTreeSet<T>) -> AxiomKind,
        ) -> Vec<Axiom> {
            let items: Vec<&T> = set.iter().collect();
            items
                .windows(2)
                .map(|w| Axiom {
                    kind: f([w[0].clone(), w[1].clone()].into_iter().collect()),
                    annotations: annotations.clone(),
                })
                .collect()
        }
        let a = &self.annotations;
        match &self.kind {
            AxiomKind::EquivalentClasses(s) if s.len() > 2 => {
                pairs(s, a, AxiomKind::EquivalentClasses)
            }
            AxiomKind::EquivalentObjectProperties(s) if s.len() > 2 => {
                pairs(s, a, AxiomKind::EquivalentObjectProperties)
            }
            AxiomKind::EquivalentDataProperties(s) if s.len() > 2 => {
                pairs(s, a, AxiomKind::EquivalentDataProperties)
            }
            AxiomKind::SameIndividual(s) if s.len() > 2 => pairs(s, a, AxiomKind::SameIndividual),
            AxiomKind::ObjectPropertyAssertion(ObjectPropertyExpression::InverseOf(p), x, y) => {
                vec![Axiom {
                    kind: AxiomKind::ObjectPropertyAssertion(
                        ObjectPropertyExpression::Property(p.clone()),
                        y.clone(),
                        x.clone(),
                    ),
                    annotations: a.clone(),
                }]
            }
            _ => vec![self.clone()],
        }
    }
}

/// The signature of a structure, collected by walking it, and its [`Shape`].
#[derive(Default)]
pub(crate) struct Signature(pub(crate) BTreeSet<Entity>, pub(crate) Shape);

/// What a walk of a structure found besides its signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// an `ObjectInverseOf` anywhere
    pub inverses: bool,
    /// a facet of a datatype restriction that is not one of the OWL 2 facets
    pub other_facets: bool,
    /// the deepest nesting of class expressions and data ranges, 1 for a class
    pub depth: usize,
    /// the deepest nesting of annotations, 1 for an annotation without annotations
    pub annotation_depth: usize,
    /// the nesting at the point of the walk
    at: usize,
}

impl Shape {
    fn enter(&mut self) {
        self.at += 1;
        self.depth = self.depth.max(self.at);
    }
}

impl Signature {
    /// Whether an axiom names an individual in any position but its annotations.
    pub(crate) fn names_individual(k: &AxiomKind) -> bool {
        let mut s = Signature::default();
        s.axiom(k);
        s.0.iter().any(|e| matches!(e, Entity::NamedIndividual(_)))
    }

    fn class(&mut self, c: &Class) {
        self.0.insert(Entity::Class(c.clone()));
    }
    fn ope(&mut self, p: &ObjectPropertyExpression) {
        self.1.inverses |= matches!(p, ObjectPropertyExpression::InverseOf(_));
        self.0.insert(Entity::ObjectProperty(p.named().clone()));
    }
    fn dp(&mut self, p: &DataProperty) {
        self.0.insert(Entity::DataProperty(p.clone()));
    }
    fn ind(&mut self, i: &Individual) {
        if let Individual::Named(n) = i {
            self.0.insert(Entity::NamedIndividual(n.clone()));
        }
    }
    fn lit(&mut self, l: &Literal) {
        // a literal's datatype is part of the signature (§5.8.1 types it)
        if l.language().is_none() && l.datatype() != vocab::XSD_STRING {
            self.0.insert(Entity::Datatype(Datatype::new(l.datatype())));
        }
    }

    pub(crate) fn ce(&mut self, c: &ClassExpression) {
        self.1.enter();
        self.ce_inner(c);
        self.1.at -= 1;
    }

    fn ce_inner(&mut self, c: &ClassExpression) {
        use ClassExpression as C;
        match c {
            C::Class(c) => self.class(c),
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                cs.iter().for_each(|c| self.ce(c))
            }
            C::ObjectComplementOf(c) => self.ce(c),
            C::ObjectOneOf(is) => is.iter().for_each(|i| self.ind(i)),
            C::ObjectSomeValuesFrom(p, c) | C::ObjectAllValuesFrom(p, c) => {
                self.ope(p);
                self.ce(c);
            }
            C::ObjectHasValue(p, i) => {
                self.ope(p);
                self.ind(i);
            }
            C::ObjectHasSelf(p) => self.ope(p),
            C::ObjectMinCardinality(_, p, c)
            | C::ObjectMaxCardinality(_, p, c)
            | C::ObjectExactCardinality(_, p, c) => {
                self.ope(p);
                if let Some(c) = c {
                    self.ce(c);
                }
            }
            C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
                ps.iter().for_each(|p| self.dp(p));
                self.dr(r);
            }
            C::DataHasValue(p, l) => {
                self.dp(p);
                self.lit(l);
            }
            C::DataMinCardinality(_, p, r)
            | C::DataMaxCardinality(_, p, r)
            | C::DataExactCardinality(_, p, r) => {
                self.dp(p);
                if let Some(r) = r {
                    self.dr(r);
                }
            }
        }
    }

    pub(crate) fn dr(&mut self, r: &DataRange) {
        self.1.enter();
        self.dr_inner(r);
        self.1.at -= 1;
    }

    fn dr_inner(&mut self, r: &DataRange) {
        match r {
            DataRange::Datatype(d) => {
                self.0.insert(Entity::Datatype(d.clone()));
            }
            DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
                rs.iter().for_each(|r| self.dr(r))
            }
            DataRange::ComplementOf(r) => self.dr(r),
            DataRange::OneOf(ls) => ls.iter().for_each(|l| self.lit(l)),
            DataRange::DatatypeRestriction(d, fs) => {
                self.0.insert(Entity::Datatype(d.clone()));
                for f in fs {
                    self.1.other_facets |= !crate::vocab::FACETS.contains(&f.facet.as_str());
                    self.lit(&f.value);
                }
            }
        }
    }

    fn annotation(&mut self, a: &Annotation) {
        self.annotation_at(a, 1);
    }

    fn annotation_at(&mut self, a: &Annotation, depth: usize) {
        self.1.annotation_depth = self.1.annotation_depth.max(depth);
        self.0
            .insert(Entity::AnnotationProperty(a.property.clone()));
        if let AnnotationValue::Literal(l) = &a.value {
            self.lit(l);
        }
        for a in &a.annotations {
            self.annotation_at(a, depth + 1);
        }
    }

    pub(crate) fn axiom(&mut self, k: &AxiomKind) {
        use AxiomKind as K;
        match k {
            K::Declaration(e) => {
                self.0.insert(e.clone());
            }
            K::SubClassOf(a, b) => {
                self.ce(a);
                self.ce(b);
            }
            K::EquivalentClasses(cs) | K::DisjointClasses(cs) => cs.iter().for_each(|c| self.ce(c)),
            K::DisjointUnion(c, cs) => {
                self.class(c);
                cs.iter().for_each(|c| self.ce(c));
            }
            K::SubObjectPropertyOf(sub, sup) => {
                match sub {
                    SubObjectPropertyExpression::Expression(p) => self.ope(p),
                    SubObjectPropertyExpression::Chain(ps) => ps.iter().for_each(|p| self.ope(p)),
                }
                self.ope(sup);
            }
            K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
                ps.iter().for_each(|p| self.ope(p))
            }
            K::InverseObjectProperties(a, b) => {
                self.ope(a);
                self.ope(b);
            }
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
                self.ope(p);
                self.ce(c);
            }
            K::FunctionalObjectProperty(p)
            | K::InverseFunctionalObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
            | K::IrreflexiveObjectProperty(p)
            | K::SymmetricObjectProperty(p)
            | K::AsymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p) => self.ope(p),
            K::SubDataPropertyOf(a, b) => {
                self.dp(a);
                self.dp(b);
            }
            K::EquivalentDataProperties(ps) | K::DisjointDataProperties(ps) => {
                ps.iter().for_each(|p| self.dp(p))
            }
            K::DataPropertyDomain(p, c) => {
                self.dp(p);
                self.ce(c);
            }
            K::DataPropertyRange(p, r) => {
                self.dp(p);
                self.dr(r);
            }
            K::FunctionalDataProperty(p) => self.dp(p),
            K::DatatypeDefinition(d, r) => {
                self.0.insert(Entity::Datatype(d.clone()));
                self.dr(r);
            }
            K::HasKey(c, ops, dps) => {
                self.ce(c);
                ops.iter().for_each(|p| self.ope(p));
                dps.iter().for_each(|p| self.dp(p));
            }
            K::SameIndividual(is) | K::DifferentIndividuals(is) => {
                is.iter().for_each(|i| self.ind(i))
            }
            K::ClassAssertion(c, i) => {
                self.ce(c);
                self.ind(i);
            }
            K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
                self.ope(p);
                self.ind(a);
                self.ind(b);
            }
            K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
                self.dp(p);
                self.ind(a);
                self.lit(l);
            }
            K::AnnotationAssertion(p, _, v) => {
                self.0.insert(Entity::AnnotationProperty(p.clone()));
                if let AnnotationValue::Literal(l) = v {
                    self.lit(l);
                }
            }
            K::SubAnnotationPropertyOf(a, b) => {
                self.0.insert(Entity::AnnotationProperty(a.clone()));
                self.0.insert(Entity::AnnotationProperty(b.clone()));
            }
            K::AnnotationPropertyDomain(p, _) | K::AnnotationPropertyRange(p, _) => {
                self.0.insert(Entity::AnnotationProperty(p.clone()));
            }
        }
    }
}

/// The ontology header (structural specification §3): IRI, version IRI, direct imports
/// and ontology annotations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub iri: Option<Iri>,
    pub version_iri: Option<Iri>,
    pub imports: BTreeSet<Iri>,
    pub annotations: BTreeSet<Annotation>,
}
