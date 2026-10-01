//! The JSON lexer ([`crate::lex::LexMode::Json`]): lossless like the RDF lexer, with
//! strings as [`TokenKind::String2`] (escapes as written), numbers as the integer,
//! decimal and double kinds (lexemes as written), `true`, `false` and `null` as
//! [`TokenKind::Word`], `{` `}` `[` `]` `,` and [`TokenKind::Colon`].
//!
//! Not written yet: until it is, the RDF lexer stands in (lossless, but its tokens are not
//! JSON's).

#[allow(unused_imports)]
use crate::lex::TokenKind;
use crate::lex::{LexMode, Token};

/// Tokenize JSON text (at most `u32::MAX` bytes); the last token is an empty
/// [`TokenKind::Eof`].
pub fn lex(src: &str) -> Vec<Token> {
    crate::lex::lex(src, LexMode::Turtle)
}
