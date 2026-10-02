//! SHACL, RDF and RDFS vocabulary constants.

use oxrdf::NamedNodeRef;

pub const SH_NS: &str = "http://www.w3.org/ns/shacl#";

macro_rules! terms {
    ($ns:literal; $($name:ident = $local:literal),* $(,)?) => {
        $(pub const $name: NamedNodeRef<'static> = NamedNodeRef::new_unchecked(concat!($ns, $local));)*
    };
}

pub mod sh {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/ns/shacl#";
        NODE_SHAPE = "NodeShape",
        PROPERTY_SHAPE = "PropertyShape",
        SHAPE = "Shape",
        TARGET_NODE = "targetNode",
        TARGET_CLASS = "targetClass",
        TARGET_SUBJECTS_OF = "targetSubjectsOf",
        TARGET_OBJECTS_OF = "targetObjectsOf",
        TARGET_WHERE = "targetWhere",
        DEACTIVATED = "deactivated",
        SEVERITY = "severity",
        MESSAGE = "message",
        PATH = "path",
        INVERSE_PATH = "inversePath",
        ALTERNATIVE_PATH = "alternativePath",
        ZERO_OR_MORE_PATH = "zeroOrMorePath",
        ONE_OR_MORE_PATH = "oneOrMorePath",
        ZERO_OR_ONE_PATH = "zeroOrOnePath",
        VIOLATION = "Violation",
        WARNING = "Warning",
        INFO = "Info",
        // parameters
        CLASS = "class",
        DATATYPE = "datatype",
        NODE_KIND = "nodeKind",
        MIN_COUNT = "minCount",
        MAX_COUNT = "maxCount",
        MIN_EXCLUSIVE = "minExclusive",
        MIN_INCLUSIVE = "minInclusive",
        MAX_EXCLUSIVE = "maxExclusive",
        MAX_INCLUSIVE = "maxInclusive",
        MIN_LENGTH = "minLength",
        MAX_LENGTH = "maxLength",
        PATTERN = "pattern",
        FLAGS = "flags",
        LANGUAGE_IN = "languageIn",
        UNIQUE_LANG = "uniqueLang",
        EQUALS = "equals",
        DISJOINT = "disjoint",
        LESS_THAN = "lessThan",
        LESS_THAN_OR_EQUALS = "lessThanOrEquals",
        NOT = "not",
        AND = "and",
        OR = "or",
        XONE = "xone",
        NODE = "node",
        PROPERTY = "property",
        QUALIFIED_VALUE_SHAPE = "qualifiedValueShape",
        QUALIFIED_VALUE_SHAPES_DISJOINT = "qualifiedValueShapesDisjoint",
        QUALIFIED_MIN_COUNT = "qualifiedMinCount",
        QUALIFIED_MAX_COUNT = "qualifiedMaxCount",
        CLOSED = "closed",
        IGNORED_PROPERTIES = "ignoredProperties",
        HAS_VALUE = "hasValue",
        IN = "in",
        // node kinds
        BLANK_NODE = "BlankNode",
        IRI = "IRI",
        LITERAL = "Literal",
        BLANK_NODE_OR_IRI = "BlankNodeOrIRI",
        BLANK_NODE_OR_LITERAL = "BlankNodeOrLiteral",
        IRI_OR_LITERAL = "IRIOrLiteral",
        // SHACL-SPARQL
        SPARQL = "sparql",
        SELECT = "select",
        ASK = "ask",
        PREFIXES = "prefixes",
        DECLARE = "declare",
        PREFIX = "prefix",
        NAMESPACE = "namespace",
        CONSTRAINT_COMPONENT = "ConstraintComponent",
        PARAMETER = "parameter",
        OPTIONAL = "optional",
        VALIDATOR = "validator",
        NODE_VALIDATOR = "nodeValidator",
        PROPERTY_VALIDATOR = "propertyValidator",
        SPARQL_ASK_VALIDATOR = "SPARQLAskValidator",
        SPARQL_SELECT_VALIDATOR = "SPARQLSelectValidator",
        // report
        VALIDATION_REPORT = "ValidationReport",
        VALIDATION_RESULT = "ValidationResult",
        CONFORMS = "conforms",
        RESULT = "result",
        FOCUS_NODE = "focusNode",
        RESULT_PATH = "resultPath",
        VALUE = "value",
        SOURCE_SHAPE = "sourceShape",
        SOURCE_CONSTRAINT = "sourceConstraint",
        SOURCE_CONSTRAINT_COMPONENT = "sourceConstraintComponent",
        RESULT_SEVERITY = "resultSeverity",
        RESULT_MESSAGE = "resultMessage",
        // components
        CLASS_CC = "ClassConstraintComponent",
        DATATYPE_CC = "DatatypeConstraintComponent",
        NODE_KIND_CC = "NodeKindConstraintComponent",
        MIN_COUNT_CC = "MinCountConstraintComponent",
        MAX_COUNT_CC = "MaxCountConstraintComponent",
        MIN_EXCLUSIVE_CC = "MinExclusiveConstraintComponent",
        MIN_INCLUSIVE_CC = "MinInclusiveConstraintComponent",
        MAX_EXCLUSIVE_CC = "MaxExclusiveConstraintComponent",
        MAX_INCLUSIVE_CC = "MaxInclusiveConstraintComponent",
        MIN_LENGTH_CC = "MinLengthConstraintComponent",
        MAX_LENGTH_CC = "MaxLengthConstraintComponent",
        PATTERN_CC = "PatternConstraintComponent",
        LANGUAGE_IN_CC = "LanguageInConstraintComponent",
        UNIQUE_LANG_CC = "UniqueLangConstraintComponent",
        EQUALS_CC = "EqualsConstraintComponent",
        DISJOINT_CC = "DisjointConstraintComponent",
        LESS_THAN_CC = "LessThanConstraintComponent",
        LESS_THAN_OR_EQUALS_CC = "LessThanOrEqualsConstraintComponent",
        NOT_CC = "NotConstraintComponent",
        AND_CC = "AndConstraintComponent",
        OR_CC = "OrConstraintComponent",
        XONE_CC = "XoneConstraintComponent",
        NODE_CC = "NodeConstraintComponent",
        PROPERTY_CC = "PropertyConstraintComponent",
        QUALIFIED_MIN_COUNT_CC = "QualifiedMinCountConstraintComponent",
        QUALIFIED_MAX_COUNT_CC = "QualifiedMaxCountConstraintComponent",
        CLOSED_CC = "ClosedConstraintComponent",
        HAS_VALUE_CC = "HasValueConstraintComponent",
        IN_CC = "InConstraintComponent",
        SPARQL_CC = "SPARQLConstraintComponent",
    }
}

pub mod rdf {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
        TYPE = "type",
        FIRST = "first",
        REST = "rest",
        NIL = "nil",
        LANG_STRING = "langString",
    }
}

pub mod rdfs {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/2000/01/rdf-schema#";
        CLASS = "Class",
        SUB_CLASS_OF = "subClassOf",
    }
}

pub mod owl {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/2002/07/owl#";
        IMPORTS = "imports",
    }
}
