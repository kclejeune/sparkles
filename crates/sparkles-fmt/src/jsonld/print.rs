//! JSON-LD printing: Prettier's JSON layout (`"key": value`, `{ "a": 1 }`, no trailing
//! commas; an object with two or more members expanded, one-member objects and arrays of
//! scalars inline when they fit, arrays holding an object or an array one element per
//! line), and the key order: keywords first in their fixed order (node and value
//! objects, contexts, expanded term definitions), unknown `@` keys after them in source
//! order, then the terms in source order (by codepoint with `sort`), `@graph` last.
//!
//! Not written yet: [`print`] refuses every document.

use crate::doc::Printed;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Language, Options};

/// Print a JSON-LD tree.
pub fn print(tree: &Tree<'_>, comments: &Comments, opts: &Options) -> Result<Printed, FormatError> {
    let _ = (tree, comments, opts);
    Err(FormatError::unsupported_language(Language::JsonLd))
}

/// JSON has no comments: nothing attaches.
pub struct JsonRules;

pub static RULES: JsonRules = JsonRules;

impl CommentRules for JsonRules {
    fn is_attachment(&self, kind: NodeKind) -> bool {
        matches!(kind, NodeKind::JsonMember | NodeKind::JsonScalar)
    }

    fn is_container(&self, kind: NodeKind) -> bool {
        matches!(kind, NodeKind::JsonObject | NodeKind::JsonArray)
    }

    fn is_separator(&self, kind: TokenKind) -> bool {
        kind == TokenKind::Comma
    }

    fn is_closer(&self, kind: TokenKind) -> bool {
        matches!(kind, TokenKind::RBrace | TokenKind::RBracket)
    }
}
