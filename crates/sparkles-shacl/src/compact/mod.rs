//! The SHACL Compact Syntax (SHACLC, `text/shaclc`): a reader that builds the shapes
//! graph by the production rules of the SHACL 1.2 Compact Syntax (and the SHACL 1.0
//! Working Group Note), and a writer that turns a shapes graph back into SHACLC or
//! names the triples it cannot express. See spec G03.
//!
//! The reader also accepts what Apache Jena's reader accepts beyond the grammar: a shape
//! reference alone in a node shape body, `targetClass` as a node parameter, and
//! `group`, `order`, `name`, `description` and `defaultValue` as property parameters.
//! It adds the SHACL 1.2 list parameters `memberShape`, `minListLength`,
//! `maxListLength` and `uniqueMembers` to both parameter lists.
//!
//! ```
//! let doc = sparkles_shacl::compact::parse(
//!     "PREFIX ex: <http://ex.org/>
//!      shape ex:PersonShape -> ex:Person {
//!          ex:name xsd:string [1..1] .
//!      }",
//!     None,
//! )
//! .unwrap();
//! assert_eq!(doc.graph.len(), 7);
//! let text = sparkles_shacl::compact::write(&doc.graph, &doc.prefixes).unwrap();
//! assert!(text.contains("ex:name xsd:string [1..1] ."));
//! ```

mod lex;
mod read;
mod write;

use oxrdf::{Graph, Triple};
use std::fmt;

/// The media type of SHACLC.
pub const MEDIA_TYPE: &str = "text/shaclc";

/// The file extensions of SHACLC documents.
pub const EXTENSIONS: [&str; 2] = ["shaclc", "shc"];

/// A parsed SHACLC document.
#[derive(Clone, Debug)]
pub struct Document {
    /// the shapes graph (fresh blank nodes)
    pub graph: Graph,
    /// the prefixes the document declares, in order
    pub prefixes: Vec<(String, String)>,
    /// the base at the end of the document (`BASE`, or the base given to the parser)
    pub base: Option<String>,
}

/// A SHACLC syntax error, with the line and column (1-based) where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SHACLC syntax error at line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for SyntaxError {}

/// A shapes graph with triples that SHACLC cannot express.
#[derive(Clone, Debug)]
pub struct NotCompact {
    /// the first triples that have no compact form (at most ten)
    pub triples: Vec<Triple>,
    /// how many triples have no compact form
    pub total: usize,
}

impl fmt::Display for NotCompact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the shapes graph cannot be written in SHACLC: {} triple{} ha{} no compact form, such as",
            self.total,
            if self.total == 1 { "" } else { "s" },
            if self.total == 1 { "s" } else { "ve" },
        )?;
        for t in &self.triples {
            write!(f, "\n  {t}")?;
        }
        Ok(())
    }
}

impl std::error::Error for NotCompact {}

/// Parse a SHACLC document. `base` resolves relative IRIs until a `BASE` directive, and
/// names the ontology (`<base> a owl:Ontology`) when the document has no `BASE`.
pub fn parse(text: &str, base: Option<&str>) -> Result<Document, SyntaxError> {
    read::parse(text, base)
}

/// Write a shapes graph as SHACLC, with prefixed names from `prefixes` (and `rdf`,
/// `rdfs`, `sh` and `xsd`). Fails when some triple has no compact form; the result then
/// names them.
pub fn write(graph: &Graph, prefixes: &[(String, String)]) -> Result<String, NotCompact> {
    write::write(graph, prefixes)
}

/// The node parameters: `name=value` in a node shape body, the predicate being
/// `sh:name`.
pub const NODE_PARAMS: [&str; 29] = [
    "targetNode",
    "targetObjectsOf",
    "targetSubjectsOf",
    "targetClass",
    "deactivated",
    "severity",
    "message",
    "class",
    "datatype",
    "nodeKind",
    "minExclusive",
    "minInclusive",
    "maxExclusive",
    "maxInclusive",
    "minLength",
    "maxLength",
    "pattern",
    "flags",
    "languageIn",
    "equals",
    "disjoint",
    "closed",
    "ignoredProperties",
    "hasValue",
    "in",
    "memberShape",
    "minListLength",
    "maxListLength",
    "uniqueMembers",
];

/// The property parameters: `name=value` in a property shape.
pub const PROPERTY_PARAMS: [&str; 37] = [
    "deactivated",
    "severity",
    "message",
    "class",
    "datatype",
    "nodeKind",
    "minExclusive",
    "minInclusive",
    "maxExclusive",
    "maxInclusive",
    "minLength",
    "maxLength",
    "pattern",
    "flags",
    "languageIn",
    "uniqueLang",
    "equals",
    "disjoint",
    "lessThan",
    "lessThanOrEquals",
    "qualifiedValueShape",
    "qualifiedMinCount",
    "qualifiedMaxCount",
    "qualifiedValueShapesDisjoint",
    "closed",
    "ignoredProperties",
    "hasValue",
    "in",
    "group",
    "order",
    "name",
    "description",
    "defaultValue",
    "memberShape",
    "minListLength",
    "maxListLength",
    "uniqueMembers",
];

pub(crate) fn node_param(name: &str) -> bool {
    NODE_PARAMS.contains(&name)
}

pub(crate) fn property_param(name: &str) -> bool {
    PROPERTY_PARAMS.contains(&name)
}

/// Whether a property type IRI means `sh:datatype` rather than `sh:class`: an `xsd:`
/// IRI, or one of the RDF datatypes (Jena's reading of "the RDF datatypes supported by
/// SPARQL 1.1").
pub fn is_datatype_iri(iri: &str) -> bool {
    iri.starts_with("http://www.w3.org/2001/XMLSchema#")
        || matches!(
            iri,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#HTML"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral"
        )
}

#[cfg(test)]
mod tests;
