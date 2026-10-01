//! The JSON check of JSON-LD: the reference parse with json-event-parser (strict JSON;
//! its errors are the user's syntax errors) into an order-insensitive model, and the
//! comparison of the input's and the output's models. Objects are maps from keys to
//! values (a duplicate key is an error), arrays are ordered, strings are compared after
//! unescaping and numbers by lexeme. Equal models are the same JSON document, so they
//! denote the same RDF under any context, remote ones included. A difference refuses the
//! output as [`Check::Graph`](crate::Check::Graph).
//!
//! Not written yet: [`crate::jsonld::IMPLEMENTED`] is off, so nothing reaches it.

use crate::{FormatError, Language};

/// The reference parse of a JSON-LD document.
#[derive(Clone, Debug)]
pub struct JsonReference {}

/// Parse `text` (a BOM dropped) as strict JSON.
pub fn json_reference(text: &str) -> Result<JsonReference, FormatError> {
    let _ = text;
    Err(FormatError::unsupported_language(Language::JsonLd))
}

/// `Ok` when `output` is the same JSON document as the input.
pub fn json_equivalent(r: &JsonReference, output: &str) -> Result<(), FormatError> {
    let _ = (r, output);
    Err(FormatError::unsupported_language(Language::JsonLd))
}
