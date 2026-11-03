//! The IRIs of the RDF, RDFS, XSD and OWL vocabularies that the mapping to RDF graphs
//! and the profiles use.

pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
pub const OWL: &str = "http://www.w3.org/2002/07/owl#";

macro_rules! iris {
    ($($name:ident = $ns:ident $local:literal;)*) => {
        $(pub const $name: &str = concat_ns!($ns, $local);)*
    };
}

macro_rules! concat_ns {
    (RDF, $l:literal) => {
        concat!("http://www.w3.org/1999/02/22-rdf-syntax-ns#", $l)
    };
    (RDFS, $l:literal) => {
        concat!("http://www.w3.org/2000/01/rdf-schema#", $l)
    };
    (XSD, $l:literal) => {
        concat!("http://www.w3.org/2001/XMLSchema#", $l)
    };
    (OWL, $l:literal) => {
        concat!("http://www.w3.org/2002/07/owl#", $l)
    };
}

iris! {
    RDF_TYPE = RDF "type";
    RDF_FIRST = RDF "first";
    RDF_REST = RDF "rest";
    RDF_NIL = RDF "nil";
    RDF_LIST = RDF "List";
    RDF_PROPERTY = RDF "Property";
    RDF_PLAIN_LITERAL = RDF "PlainLiteral";
    RDF_XML_LITERAL = RDF "XMLLiteral";
    RDF_LANG_STRING = RDF "langString";
    RDF_LANG_RANGE = RDF "langRange";

    RDFS_CLASS = RDFS "Class";
    RDFS_DATATYPE = RDFS "Datatype";
    RDFS_LITERAL = RDFS "Literal";
    RDFS_SUB_CLASS_OF = RDFS "subClassOf";
    RDFS_SUB_PROPERTY_OF = RDFS "subPropertyOf";
    RDFS_DOMAIN = RDFS "domain";
    RDFS_RANGE = RDFS "range";
    RDFS_LABEL = RDFS "label";
    RDFS_COMMENT = RDFS "comment";
    RDFS_SEE_ALSO = RDFS "seeAlso";
    RDFS_IS_DEFINED_BY = RDFS "isDefinedBy";

    XSD_STRING = XSD "string";
    XSD_BOOLEAN = XSD "boolean";
    XSD_INTEGER = XSD "integer";
    XSD_DECIMAL = XSD "decimal";
    XSD_FLOAT = XSD "float";
    XSD_NON_NEGATIVE_INTEGER = XSD "nonNegativeInteger";

    OWL_ONTOLOGY = OWL "Ontology";
    OWL_IMPORTS = OWL "imports";
    OWL_VERSION_IRI = OWL "versionIRI";
    OWL_ONTOLOGY_PROPERTY = OWL "OntologyProperty";
    OWL_CLASS = OWL "Class";
    OWL_THING = OWL "Thing";
    OWL_NOTHING = OWL "Nothing";
    OWL_OBJECT_PROPERTY = OWL "ObjectProperty";
    OWL_DATATYPE_PROPERTY = OWL "DatatypeProperty";
    OWL_ANNOTATION_PROPERTY = OWL "AnnotationProperty";
    OWL_NAMED_INDIVIDUAL = OWL "NamedIndividual";
    OWL_TOP_OBJECT_PROPERTY = OWL "topObjectProperty";
    OWL_BOTTOM_OBJECT_PROPERTY = OWL "bottomObjectProperty";
    OWL_TOP_DATA_PROPERTY = OWL "topDataProperty";
    OWL_BOTTOM_DATA_PROPERTY = OWL "bottomDataProperty";
    OWL_REAL = OWL "real";
    OWL_RATIONAL = OWL "rational";
    OWL_DATA_RANGE = OWL "DataRange";
    OWL_RESTRICTION = OWL "Restriction";
    OWL_DEPRECATED_CLASS = OWL "DeprecatedClass";
    OWL_DEPRECATED_PROPERTY = OWL "DeprecatedProperty";
    OWL_DEPRECATED = OWL "deprecated";
    OWL_VERSION_INFO = OWL "versionInfo";
    OWL_PRIOR_VERSION = OWL "priorVersion";
    OWL_BACKWARD_COMPATIBLE_WITH = OWL "backwardCompatibleWith";
    OWL_INCOMPATIBLE_WITH = OWL "incompatibleWith";

    OWL_INTERSECTION_OF = OWL "intersectionOf";
    OWL_UNION_OF = OWL "unionOf";
    OWL_COMPLEMENT_OF = OWL "complementOf";
    OWL_DATATYPE_COMPLEMENT_OF = OWL "datatypeComplementOf";
    OWL_ONE_OF = OWL "oneOf";
    OWL_ON_DATATYPE = OWL "onDatatype";
    OWL_WITH_RESTRICTIONS = OWL "withRestrictions";
    OWL_INVERSE_OF = OWL "inverseOf";
    OWL_ON_PROPERTY = OWL "onProperty";
    OWL_ON_PROPERTIES = OWL "onProperties";
    OWL_SOME_VALUES_FROM = OWL "someValuesFrom";
    OWL_ALL_VALUES_FROM = OWL "allValuesFrom";
    OWL_HAS_VALUE = OWL "hasValue";
    OWL_HAS_SELF = OWL "hasSelf";
    OWL_MIN_CARDINALITY = OWL "minCardinality";
    OWL_MAX_CARDINALITY = OWL "maxCardinality";
    OWL_CARDINALITY = OWL "cardinality";
    OWL_MIN_QUALIFIED_CARDINALITY = OWL "minQualifiedCardinality";
    OWL_MAX_QUALIFIED_CARDINALITY = OWL "maxQualifiedCardinality";
    OWL_QUALIFIED_CARDINALITY = OWL "qualifiedCardinality";
    OWL_ON_CLASS = OWL "onClass";
    OWL_ON_DATA_RANGE = OWL "onDataRange";

    OWL_EQUIVALENT_CLASS = OWL "equivalentClass";
    OWL_DISJOINT_WITH = OWL "disjointWith";
    OWL_ALL_DISJOINT_CLASSES = OWL "AllDisjointClasses";
    OWL_MEMBERS = OWL "members";
    OWL_DISJOINT_UNION_OF = OWL "disjointUnionOf";
    OWL_PROPERTY_CHAIN_AXIOM = OWL "propertyChainAxiom";
    OWL_EQUIVALENT_PROPERTY = OWL "equivalentProperty";
    OWL_PROPERTY_DISJOINT_WITH = OWL "propertyDisjointWith";
    OWL_ALL_DISJOINT_PROPERTIES = OWL "AllDisjointProperties";
    OWL_FUNCTIONAL_PROPERTY = OWL "FunctionalProperty";
    OWL_INVERSE_FUNCTIONAL_PROPERTY = OWL "InverseFunctionalProperty";
    OWL_REFLEXIVE_PROPERTY = OWL "ReflexiveProperty";
    OWL_IRREFLEXIVE_PROPERTY = OWL "IrreflexiveProperty";
    OWL_SYMMETRIC_PROPERTY = OWL "SymmetricProperty";
    OWL_ASYMMETRIC_PROPERTY = OWL "AsymmetricProperty";
    OWL_TRANSITIVE_PROPERTY = OWL "TransitiveProperty";
    OWL_HAS_KEY = OWL "hasKey";
    OWL_SAME_AS = OWL "sameAs";
    OWL_DIFFERENT_FROM = OWL "differentFrom";
    OWL_ALL_DIFFERENT = OWL "AllDifferent";
    OWL_DISTINCT_MEMBERS = OWL "distinctMembers";
    OWL_NEGATIVE_PROPERTY_ASSERTION = OWL "NegativePropertyAssertion";
    OWL_SOURCE_INDIVIDUAL = OWL "sourceIndividual";
    OWL_ASSERTION_PROPERTY = OWL "assertionProperty";
    OWL_TARGET_INDIVIDUAL = OWL "targetIndividual";
    OWL_TARGET_VALUE = OWL "targetValue";

    OWL_AXIOM = OWL "Axiom";
    OWL_ANNOTATION = OWL "Annotation";
    OWL_ANNOTATED_SOURCE = OWL "annotatedSource";
    OWL_ANNOTATED_PROPERTY = OWL "annotatedProperty";
    OWL_ANNOTATED_TARGET = OWL "annotatedTarget";
}

/// The built-in annotation properties of the structural specification's §5.5.
pub const BUILTIN_ANNOTATION_PROPERTIES: [&str; 9] = [
    RDFS_LABEL,
    RDFS_COMMENT,
    RDFS_SEE_ALSO,
    RDFS_IS_DEFINED_BY,
    OWL_DEPRECATED,
    OWL_VERSION_INFO,
    OWL_PRIOR_VERSION,
    OWL_BACKWARD_COMPATIBLE_WITH,
    OWL_INCOMPATIBLE_WITH,
];

/// The datatypes of the OWL 2 datatype map (structural specification §4), without
/// `rdfs:Literal`, which is built in but not in the map.
pub const DATATYPE_MAP: [&str; 34] = [
    concat_ns!(OWL, "real"),
    concat_ns!(OWL, "rational"),
    concat_ns!(XSD, "decimal"),
    concat_ns!(XSD, "integer"),
    concat_ns!(XSD, "nonNegativeInteger"),
    concat_ns!(XSD, "nonPositiveInteger"),
    concat_ns!(XSD, "positiveInteger"),
    concat_ns!(XSD, "negativeInteger"),
    concat_ns!(XSD, "long"),
    concat_ns!(XSD, "int"),
    concat_ns!(XSD, "short"),
    concat_ns!(XSD, "byte"),
    concat_ns!(XSD, "unsignedLong"),
    concat_ns!(XSD, "unsignedInt"),
    concat_ns!(XSD, "unsignedShort"),
    concat_ns!(XSD, "unsignedByte"),
    concat_ns!(XSD, "double"),
    concat_ns!(XSD, "float"),
    concat_ns!(XSD, "string"),
    concat_ns!(XSD, "normalizedString"),
    concat_ns!(XSD, "token"),
    concat_ns!(XSD, "language"),
    concat_ns!(XSD, "Name"),
    concat_ns!(XSD, "NCName"),
    concat_ns!(XSD, "NMTOKEN"),
    concat_ns!(XSD, "boolean"),
    concat_ns!(XSD, "hexBinary"),
    concat_ns!(XSD, "base64Binary"),
    concat_ns!(XSD, "anyURI"),
    concat_ns!(XSD, "dateTime"),
    concat_ns!(XSD, "dateTimeStamp"),
    concat_ns!(RDF, "PlainLiteral"),
    concat_ns!(RDF, "XMLLiteral"),
    // RDF 1.1 names language-tagged strings rdf:langString, which OWL 2 reads as
    // rdf:PlainLiteral; it is accepted wherever rdf:PlainLiteral is
    concat_ns!(RDF, "langString"),
];

/// The constraining facets of the OWL 2 datatype map.
pub const FACETS: [&str; 13] = [
    concat_ns!(XSD, "minInclusive"),
    concat_ns!(XSD, "maxInclusive"),
    concat_ns!(XSD, "minExclusive"),
    concat_ns!(XSD, "maxExclusive"),
    concat_ns!(XSD, "length"),
    concat_ns!(XSD, "minLength"),
    concat_ns!(XSD, "maxLength"),
    concat_ns!(XSD, "pattern"),
    concat_ns!(XSD, "totalDigits"),
    concat_ns!(XSD, "fractionDigits"),
    concat_ns!(XSD, "maxScale"),
    concat_ns!(XSD, "explicitTimezone"),
    concat_ns!(RDF, "langRange"),
];

/// Whether `iri` is in the reserved vocabulary of OWL 2 (structural specification §2.4).
pub fn is_reserved(iri: &str) -> bool {
    [RDF, RDFS, XSD, OWL].iter().any(|ns| iri.starts_with(ns))
}

/// Whether `iri` is `rdfs:Literal` or a datatype of the OWL 2 datatype map.
pub fn is_builtin_datatype(iri: &str) -> bool {
    iri == RDFS_LITERAL || DATATYPE_MAP.contains(&iri)
}

/// Whether `iri` is a built-in annotation property.
pub fn is_builtin_annotation_property(iri: &str) -> bool {
    BUILTIN_ANNOTATION_PROPERTIES.contains(&iri)
}

/// The predicates whose triples belong to the terminology: every predicate of the
/// mapping's class, property and data range structures, its lists, its reification and
/// its terminological axioms. Assertions (`owl:sameAs`, `owl:differentFrom` and the
/// triples of user properties) are not among them.
pub const STRUCTURAL_PREDICATES: [&str; 46] = [
    RDF_FIRST,
    RDF_REST,
    RDFS_SUB_CLASS_OF,
    RDFS_SUB_PROPERTY_OF,
    RDFS_DOMAIN,
    RDFS_RANGE,
    OWL_IMPORTS,
    OWL_VERSION_IRI,
    OWL_INTERSECTION_OF,
    OWL_UNION_OF,
    OWL_COMPLEMENT_OF,
    OWL_DATATYPE_COMPLEMENT_OF,
    OWL_ONE_OF,
    OWL_ON_DATATYPE,
    OWL_WITH_RESTRICTIONS,
    OWL_INVERSE_OF,
    OWL_ON_PROPERTY,
    OWL_ON_PROPERTIES,
    OWL_SOME_VALUES_FROM,
    OWL_ALL_VALUES_FROM,
    OWL_HAS_VALUE,
    OWL_HAS_SELF,
    OWL_MIN_CARDINALITY,
    OWL_MAX_CARDINALITY,
    OWL_CARDINALITY,
    OWL_MIN_QUALIFIED_CARDINALITY,
    OWL_MAX_QUALIFIED_CARDINALITY,
    OWL_QUALIFIED_CARDINALITY,
    OWL_ON_CLASS,
    OWL_ON_DATA_RANGE,
    OWL_EQUIVALENT_CLASS,
    OWL_DISJOINT_WITH,
    OWL_MEMBERS,
    OWL_DISJOINT_UNION_OF,
    OWL_PROPERTY_CHAIN_AXIOM,
    OWL_EQUIVALENT_PROPERTY,
    OWL_PROPERTY_DISJOINT_WITH,
    OWL_HAS_KEY,
    OWL_DISTINCT_MEMBERS,
    OWL_SOURCE_INDIVIDUAL,
    OWL_ASSERTION_PROPERTY,
    OWL_TARGET_INDIVIDUAL,
    OWL_TARGET_VALUE,
    OWL_ANNOTATED_SOURCE,
    OWL_ANNOTATED_PROPERTY,
    OWL_ANNOTATED_TARGET,
];

/// The classes whose `rdf:type` triples belong to the terminology: the declaration
/// types, the types of the mapping's structures and of property characteristics, and
/// the OWL 1 types the mapping reads for compatibility.
pub const STRUCTURAL_TYPES: [&str; 28] = [
    OWL_ONTOLOGY,
    OWL_ONTOLOGY_PROPERTY,
    OWL_CLASS,
    RDFS_DATATYPE,
    OWL_OBJECT_PROPERTY,
    OWL_DATATYPE_PROPERTY,
    OWL_ANNOTATION_PROPERTY,
    OWL_NAMED_INDIVIDUAL,
    OWL_RESTRICTION,
    OWL_DATA_RANGE,
    OWL_AXIOM,
    OWL_ANNOTATION,
    OWL_ALL_DISJOINT_CLASSES,
    OWL_ALL_DISJOINT_PROPERTIES,
    OWL_ALL_DIFFERENT,
    OWL_NEGATIVE_PROPERTY_ASSERTION,
    OWL_FUNCTIONAL_PROPERTY,
    OWL_INVERSE_FUNCTIONAL_PROPERTY,
    OWL_REFLEXIVE_PROPERTY,
    OWL_IRREFLEXIVE_PROPERTY,
    OWL_SYMMETRIC_PROPERTY,
    OWL_ASYMMETRIC_PROPERTY,
    OWL_TRANSITIVE_PROPERTY,
    OWL_DEPRECATED_CLASS,
    OWL_DEPRECATED_PROPERTY,
    RDFS_CLASS,
    RDF_PROPERTY,
    RDF_LIST,
];
