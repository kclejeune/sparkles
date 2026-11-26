//! SPIN (`sp:`, `spin:`, `spl:`) vocabulary constants and operator dispatch tables.

use oxrdf::NamedNodeRef;

/// The SPIN Modeling Vocabulary namespace.
pub const SPIN_NS: &str = "http://spinrdf.org/spin#";
/// The SPIN SPARQL Syntax namespace.
pub const SP_NS: &str = "http://spinrdf.org/sp#";
/// The SPIN Standard Modules Library namespace.
pub const SPL_NS: &str = "http://spinrdf.org/spl#";

macro_rules! terms {
    ($ns:literal; $($name:ident = $local:literal),* $(,)?) => {
        $(pub const $name: NamedNodeRef<'static> = NamedNodeRef::new_unchecked(concat!($ns, $local));)*
    };
}

pub mod sp {
    use super::NamedNodeRef;
    terms! { "http://spinrdf.org/sp#";
        // Query forms
        CONSTRUCT = "Construct",
        SELECT = "Select",
        ASK = "Ask",
        DESCRIBE = "Describe",
        // Query properties
        TEXT = "text",
        TEMPLATES = "templates",
        WHERE = "where",
        RESULT_VARIABLES = "resultVariables",
        RESULT_NODES = "resultNodes",
        DISTINCT = "distinct",
        REDUCED = "reduced",
        ORDER_BY = "orderBy",
        GROUP_BY = "groupBy",
        HAVING = "having",
        LIMIT = "limit",
        OFFSET = "offset",
        FROM = "from",
        FROM_NAMED = "fromNamed",
        VALUES = "values",
        // Graph pattern element classes
        TRIPLE_PATTERN = "TriplePattern",
        TRIPLE_TEMPLATE = "TripleTemplate",
        TRIPLE_PATH = "TriplePath",
        FILTER = "Filter",
        BIND = "Bind",
        LET = "Let",
        OPTIONAL = "Optional",
        UNION = "Union",
        SUB_QUERY = "SubQuery",
        MINUS = "Minus",
        NAMED_GRAPH = "NamedGraph",
        SERVICE = "Service",
        VALUES_CLS = "Values",
        EXISTS = "Exists",
        NOT_EXISTS = "NotExists",
        ELEMENT_GROUP = "ElementGroup",
        // Graph pattern element properties
        SUBJECT = "subject",
        PREDICATE = "predicate",
        OBJECT = "object",
        PATH = "path",
        EXPRESSION = "expression",
        VARIABLE = "variable",
        ELEMENTS = "elements",
        QUERY_PROP = "query",
        GRAPH_NAME_NODE = "graphNameNode",
        SERVICE_URI = "serviceURI",
        SILENT = "silent",
        VAR_NAMES = "varNames",
        VARIABLES = "variables",
        BINDINGS = "bindings",
        UNDEF = "undef",
        // Property paths
        SEQ_PATH = "SeqPath",
        ALT_PATH = "AltPath",
        REVERSE_PATH = "ReversePath",
        REVERSE_LINK_PATH = "ReverseLinkPath",
        MOD_PATH = "ModPath",
        PATH1 = "path1",
        PATH2 = "path2",
        SUB_PATH = "subPath",
        NODE = "node",
        MOD_MIN = "modMin",
        MOD_MAX = "modMax",
        // Order & aggregates
        ASC = "Asc",
        DESC = "Desc",
        COUNT = "Count",
        GROUP_CONCAT = "GroupConcat",
        SEPARATOR = "separator",
        // Variables & expressions
        VARIABLE_CLS = "Variable",
        VAR_NAME = "varName",
        // Membership & bound
        IN = "in",
        NOT_IN = "notIn",
        BOUND = "bound",
    }
}

pub mod spin {
    use super::NamedNodeRef;
    terms! { "http://spinrdf.org/spin#";
        RULE = "rule",
        TEMPLATE = "Template",
        CONSTRUCT_TEMPLATE = "ConstructTemplate",
        SELECT_TEMPLATE = "SelectTemplate",
        ASK_TEMPLATE = "AskTemplate",
        BODY = "body",
        CONSTRAINT = "constraint",
        THIS = "_this",
    }
}

pub mod spl {
    use super::NamedNodeRef;
    terms! { "http://spinrdf.org/spl#";
        ARGUMENT = "Argument",
        PREDICATE = "predicate",
        DEFAULT_VALUE = "defaultValue",
        OPTIONAL = "optional",
        VALUE_TYPE = "valueType",
    }
}

/// Binary infix SPARQL operator corresponding to a `sp:` IRI.
pub(crate) fn binary_op(iri: &str) -> Option<&'static str> {
    let local = iri.strip_prefix(SP_NS)?;
    Some(match local {
        "or" => "||",
        "and" => "&&",
        "eq" => "=",
        "ne" => "!=",
        "lt" => "<",
        "le" => "<=",
        "gt" => ">",
        "ge" => ">=",
        "add" => "+",
        "sub" => "-",
        "mul" => "*",
        "divide" => "/",
        _ => return None,
    })
}

/// Unary prefix SPARQL operator corresponding to a `sp:` IRI.
pub(crate) fn unary_op(iri: &str) -> Option<&'static str> {
    let local = iri.strip_prefix(SP_NS)?;
    Some(match local {
        "not" => "!",
        "unaryPlus" => "+",
        "unaryMinus" => "-",
        _ => return None,
    })
}

/// SPARQL 1.1 built-in function corresponding to a `sp:` IRI, with `(keyword, min_args, max_args)`.
pub(crate) fn builtin_fn(iri: &str) -> Option<(&'static str, usize, Option<usize>)> {
    let local = iri.strip_prefix(SP_NS)?;
    Some(match local {
        // Functional forms (sp:bound is handled specially for template substitution)
        "if" => ("IF", 3, Some(3)),
        "coalesce" => ("COALESCE", 0, None),
        "sameTerm" => ("sameTerm", 2, Some(2)),
        "isIRI" => ("isIRI", 1, Some(1)),
        "isURI" => ("isURI", 1, Some(1)),
        "isBlank" => ("isBlank", 1, Some(1)),
        "isLiteral" => ("isLiteral", 1, Some(1)),
        "isNumeric" => ("isNumeric", 1, Some(1)),
        "str" => ("STR", 1, Some(1)),
        "lang" => ("LANG", 1, Some(1)),
        "datatype" => ("DATATYPE", 1, Some(1)),
        "iri" => ("IRI", 1, Some(1)),
        "uri" => ("URI", 1, Some(1)),
        "bnode" => ("BNODE", 0, Some(1)),
        "strdt" => ("STRDT", 2, Some(2)),
        "strlang" => ("STRLANG", 2, Some(2)),
        "uuid" => ("UUID", 0, Some(0)),
        "struuid" => ("STRUUID", 0, Some(0)),
        // String functions
        "strlen" => ("STRLEN", 1, Some(1)),
        "substr" => ("SUBSTR", 2, Some(3)),
        "ucase" => ("UCASE", 1, Some(1)),
        "lcase" => ("LCASE", 1, Some(1)),
        "strstarts" => ("STRSTARTS", 2, Some(2)),
        "strends" => ("STRENDS", 2, Some(2)),
        "contains" => ("CONTAINS", 2, Some(2)),
        "strbefore" => ("STRBEFORE", 2, Some(2)),
        "strafter" => ("STRAFTER", 2, Some(2)),
        "encode_for_uri" => ("ENCODE_FOR_URI", 1, Some(1)),
        "concat" => ("CONCAT", 0, None),
        "langMatches" => ("langMatches", 2, Some(2)),
        "regex" => ("REGEX", 2, Some(3)),
        "replace" => ("REPLACE", 3, Some(4)),
        // Numeric functions
        "abs" => ("ABS", 1, Some(1)),
        "round" => ("ROUND", 1, Some(1)),
        "ceil" => ("CEIL", 1, Some(1)),
        "floor" => ("FLOOR", 1, Some(1)),
        "rand" => ("RAND", 0, Some(0)),
        // Date/time functions
        "now" => ("NOW", 0, Some(0)),
        "year" => ("YEAR", 1, Some(1)),
        "month" => ("MONTH", 1, Some(1)),
        "day" => ("DAY", 1, Some(1)),
        "hours" => ("HOURS", 1, Some(1)),
        "minutes" => ("MINUTES", 1, Some(1)),
        "seconds" => ("SECONDS", 1, Some(1)),
        "timezone" => ("TIMEZONE", 1, Some(1)),
        "tz" => ("TZ", 1, Some(1)),
        // Hash functions
        "md5" => ("MD5", 1, Some(1)),
        "sha1" => ("SHA1", 1, Some(1)),
        "sha256" => ("SHA256", 1, Some(1)),
        "sha384" => ("SHA384", 1, Some(1)),
        "sha512" => ("SHA512", 1, Some(1)),
        _ => return None,
    })
}

/// SPARQL 1.1 aggregate keyword corresponding to a `sp:Aggregation` subclass IRI.
pub(crate) fn aggregate_fn(iri: &str) -> Option<&'static str> {
    let local = iri.strip_prefix(SP_NS)?;
    Some(match local {
        "Count" => "COUNT",
        "Sum" => "SUM",
        "Avg" => "AVG",
        "Min" => "MIN",
        "Max" => "MAX",
        "Sample" => "SAMPLE",
        "GroupConcat" => "GROUP_CONCAT",
        _ => return None,
    })
}

/// Whether `iri` is a known `sp:` expression operator, built-in, aggregate, or `EXISTS`/`NOT EXISTS`.
pub(crate) fn is_sp_expression_type(iri: &str) -> bool {
    binary_op(iri).is_some()
        || unary_op(iri).is_some()
        || builtin_fn(iri).is_some()
        || aggregate_fn(iri).is_some()
        || iri == sp::IN.as_str()
        || iri == sp::NOT_IN.as_str()
        || iri == sp::BOUND.as_str()
        || iri == sp::EXISTS.as_str()
        || iri == sp::NOT_EXISTS.as_str()
}

/// Structural `sp:` classes that cannot be called as custom IRI functions in an expression.
pub(crate) fn is_structural_sp_class(iri: &str) -> bool {
    let Some(local) = iri.strip_prefix(SP_NS) else {
        return false;
    };
    matches!(
        local,
        "Query"
            | "Construct"
            | "Select"
            | "Ask"
            | "Describe"
            | "TriplePattern"
            | "TripleTemplate"
            | "TriplePath"
            | "Filter"
            | "Bind"
            | "Let"
            | "Optional"
            | "Union"
            | "SubQuery"
            | "Minus"
            | "NamedGraph"
            | "Service"
            | "Values"
            | "ElementGroup"
            | "ElementList"
            | "SeqPath"
            | "AltPath"
            | "ReversePath"
            | "ReverseLinkPath"
            | "ModPath"
            | "Asc"
            | "Desc"
            | "Variable"
            | "Expression"
            | "Function"
            | "Aggregation"
    )
}
