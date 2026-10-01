//! The lossless Turtle 1.2 and TriG 1.2 parser, on the event parser of
//! [`crate::sparql::parse`] (`Parser`, `Marker`, `Completed`, `build`): recursive descent
//! over the significant tokens, one function per grammar rule. Like the SPARQL parser it
//! does not recover, and an error is [`FormatError::Unsupported`] (oxttl accepted the
//! input, so the error is the formatter's).
//!
//! The shapes it builds: a [`NodeKind::TurtleDoc`] or [`NodeKind::TrigDoc`] root; the
//! directives as `PrefixDecl`, `BaseDecl` and `VersionDecl` (the keyword re-kinded to
//! `Kw(Prefix)` and the like for the SPARQL spelling, `@prefix`, `@base` and `@version`
//! staying [`crate::lex::TokenKind::LangDir`] tokens); statements as `TriplesStmt` with
//! `PropertyListEntry` and `Object` children as in SPARQL; TriG's `GRAPH g { … }`,
//! `g { … }` and `{ … }` as [`NodeKind::GraphBlock`].
//!
//! Not written yet.

use crate::FormatError;
use crate::lex::Token;
use crate::tree::Tree;

#[allow(unused_imports)]
use crate::syntax::NodeKind;

/// Parse a whole Turtle (`trig: false`) or TriG document.
pub fn parse<'s>(src: &'s str, tokens: Vec<Token>, trig: bool) -> Result<Tree<'s>, FormatError> {
    let _ = (src, tokens, trig);
    Err(FormatError::Unsupported {
        message: "the Turtle and TriG parser is not written yet".to_string(),
        line: 1,
        column: 1,
    })
}
