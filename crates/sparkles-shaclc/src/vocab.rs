//! The vocabulary the reader and the writer produce and recognize.

use oxrdf::NamedNodeRef;

pub(crate) const SH_NS: &str = "http://www.w3.org/ns/shacl#";

macro_rules! terms {
    ($ns:literal; $($name:ident = $local:literal),* $(,)?) => {
        $(pub(crate) const $name: NamedNodeRef<'static> = NamedNodeRef::new_unchecked(concat!($ns, $local));)*
    };
}

pub(crate) mod sh {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/ns/shacl#";
        NODE_SHAPE = "NodeShape",
        TARGET_CLASS = "targetClass",
        PATH = "path",
        INVERSE_PATH = "inversePath",
        ALTERNATIVE_PATH = "alternativePath",
        ZERO_OR_MORE_PATH = "zeroOrMorePath",
        ONE_OR_MORE_PATH = "oneOrMorePath",
        ZERO_OR_ONE_PATH = "zeroOrOnePath",
        CLASS = "class",
        DATATYPE = "datatype",
        NODE_KIND = "nodeKind",
        MIN_COUNT = "minCount",
        MAX_COUNT = "maxCount",
        NOT = "not",
        OR = "or",
        NODE = "node",
        PROPERTY = "property",
    }
}

pub(crate) mod rdf {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
        TYPE = "type",
        FIRST = "first",
        REST = "rest",
        NIL = "nil",
    }
}

pub(crate) mod rdfs {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/2000/01/rdf-schema#";
        CLASS = "Class",
    }
}

pub(crate) mod owl {
    use super::NamedNodeRef;
    terms! { "http://www.w3.org/2002/07/owl#";
        IMPORTS = "imports",
        ONTOLOGY = "Ontology",
    }
}
