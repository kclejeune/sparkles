//! Shape maps: the compact syntax (ShapeMap draft plus Jena's `BASE`/`PREFIX`
//! directives, commas, a trailing `.` and `a`), the JSON syntax, and the expansion of a
//! query map into a fixed map over the data graph.

use crate::error::{parse_todo, todo};
use crate::ir::PairKind;
use crate::{CompiledSchema, ParseError, PrefixMap, ShapeLabel, ShapeMap};
use oxrdf::Term;
use sparkles::id::Id;
use sparkles::validation::DataGraph;

/// Parse the compact syntax (see [`ShapeMap::parse`]).
pub fn parse(text: &str, prefixes: &PrefixMap, base: Option<&str>) -> Result<ShapeMap, ParseError> {
    let _ = (text, prefixes, base);
    Err(parse_todo("compact shape maps"))
}

/// Parse the JSON syntax (see [`ShapeMap::from_json`]).
pub fn from_json(json: &str) -> Result<ShapeMap, ParseError> {
    let _ = json;
    Err(parse_todo("JSON shape maps"))
}

/// An association of the fixed map.
#[derive(Clone, Debug, PartialEq)]
pub struct FixedEntry {
    pub node: Term,
    /// the node's store id (`None`: not in the store, so its neighbourhood is empty)
    pub id: Option<Id>,
    pub shape: ShapeLabel,
    pub kind: PairKind,
}

/// The fixed map of a query map: each selector expanded over the data graph, the
/// associations deduplicated per (node, label) in first-seen order; and the warnings
/// (blank-node labels that select nothing). A label the schema does not define, or
/// START without a start shape, is a [`crate::SchemaError`].
pub fn expand(
    map: &ShapeMap,
    data: &DataGraph,
    schema: &CompiledSchema,
) -> anyhow::Result<(Vec<FixedEntry>, Vec<String>)> {
    let _ = (map, data, schema);
    Err(todo("shape map expansion"))
}
