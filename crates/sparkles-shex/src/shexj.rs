//! ShExJ: the JSON-LD syntax of ShEx 2.1, read and written by hand through
//! `serde_json::Value` (string-or-object value expressions, `max: -1`, literals as
//! objects and IRIs as strings, `shapes` with `id`s, 2.next `ShapeDecl` wrappers).

use crate::ParseError;
use crate::ast::Schema;
use crate::error::{NOT_IMPLEMENTED, parse_todo};

/// The JSON-LD context of ShExJ.
pub const CONTEXT: &str = "http://www.w3.org/ns/shex.jsonld";

/// Is `text` a ShExJ schema (a JSON object with `"type": "Schema"`)? Tells a schema
/// posted as `application/json` from a request envelope.
pub fn is_shexj(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .is_ok_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("Schema"))
}

/// Parse ShExJ.
pub fn from_shexj(json: &str) -> Result<Schema, ParseError> {
    let _ = json;
    Err(parse_todo("ShExJ"))
}

/// Write ShExJ.
pub fn to_shexj(schema: &Schema) -> serde_json::Value {
    let _ = schema;
    unimplemented!("ShExJ writer: {NOT_IMPLEMENTED}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_schemas() {
        assert!(is_shexj(
            r#"{"@context": "http://www.w3.org/ns/shex.jsonld", "type": "Schema"}"#
        ));
        assert!(!is_shexj(r#"{"schema": "<S> {}", "map": "<n>@<S>"}"#));
        assert!(!is_shexj("PREFIX ex: <http://ex.org/> ex:S {}"));
    }
}
