//! ShExR: schemas as RDF graphs in the ShEx vocabulary (`http://www.w3.org/ns/shex#`,
//! the vocabulary behind ShExJ's JSON-LD context). A schema is the one node of type
//! `sx:Schema`; shape declarations are its `sx:shapes` list, triple expressions and
//! shape expressions are typed nodes (`sx:Shape`, `sx:EachOf`, `sx:TripleConstraint`,
//! …), and labels are the nodes' IRIs (or blank nodes).

use crate::ast::Schema;
use crate::error::ParseError;
use oxrdf::Graph;
use sparkles::io::RdfFormat;

/// The ShEx vocabulary namespace.
pub const SX: &str = "http://www.w3.org/ns/shex#";

fn not_implemented() -> ParseError {
    ParseError::new("ShExR schemas are not supported yet", 0, 0)
}

/// Read a schema from a graph in the ShEx vocabulary. Errors that are not at a place in
/// a text have line and column 0.
pub fn from_graph(graph: &Graph, base: Option<&str>) -> Result<Schema, ParseError> {
    let _ = (graph, base);
    Err(not_implemented())
}

/// Read a schema from RDF text in `format` (any syntax Sparkles reads). Syntax errors of
/// the RDF carry their line and column.
pub fn from_text(text: &str, format: RdfFormat, base: Option<&str>) -> Result<Schema, ParseError> {
    let _ = (text, format, base);
    Err(not_implemented())
}

/// The schema as a graph in the ShEx vocabulary (labels are the declarations' IRIs or
/// blank nodes; other nodes are fresh blank nodes).
pub fn to_graph(schema: &Schema) -> Graph {
    let _ = schema;
    Graph::new()
}

/// The schema as RDF text in `format`, with the schema's prefixes and `sx:`.
pub fn to_text(schema: &Schema, format: RdfFormat) -> String {
    let _ = (schema, format);
    String::new()
}
