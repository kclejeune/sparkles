//! The ShExC 2.1 parser: recursive descent over the lexer's tokens, with errors that
//! give the 1-based line and column and the tokens that were expected. ShEx 2.2 syntax
//! (`EXTENDS`, `ABSTRACT`, `RESTRICTS`) is an error that names the feature.

use crate::ParseError;
use crate::ast::Schema;
use crate::error::parse_todo;

/// Parse a ShExC schema (see [`Schema::parse_shexc`]).
pub fn parse(text: &str, base: Option<&str>) -> Result<Schema, ParseError> {
    let _ = (text, base);
    Err(parse_todo("the ShExC parser"))
}
