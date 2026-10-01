//! The lossless JSON parser, on the event parser of [`crate::sparql::parse`]: a
//! [`NodeKind::JsonDocument`] root holding one value; [`NodeKind::JsonObject`] with
//! [`NodeKind::JsonMember`] children (the key string, `:`, the value, the `,` after it),
//! [`NodeKind::JsonArray`] with its values and their `,`, and [`NodeKind::JsonScalar`]
//! around strings, numbers, `true`, `false` and `null`.
//!
//! Not written yet.

use crate::FormatError;
use crate::lex::Token;
use crate::tree::Tree;

#[allow(unused_imports)]
use crate::syntax::NodeKind;

/// Parse a whole JSON document.
pub fn parse<'s>(src: &'s str, tokens: Vec<Token>) -> Result<Tree<'s>, FormatError> {
    let _ = (src, tokens);
    Err(FormatError::Unsupported {
        message: "the JSON parser is not written yet".to_string(),
        line: 1,
        column: 1,
    })
}
