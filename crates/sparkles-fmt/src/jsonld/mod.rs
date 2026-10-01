//! JSON-LD 1.1: a strict JSON lexer ([`lex`]) and lossless tree ([`parse`]), and the
//! printer ([`print`]): Prettier's JSON layout with keyword-aware key order, string
//! escapes and number lexemes verbatim, arrays never reordered. The reference parser is
//! json-event-parser, and the output must be the same JSON document
//! ([`crate::check::json`]). JSON has no comments, so the comment check and the pragmas
//! have nothing to do.

pub mod lex;
pub mod parse;
pub mod print;

use crate::check::LangImpl;
use crate::check::json::{JsonReference, json_equivalent, json_reference};
use crate::doc::Printed;
use crate::lex::{LexMode, Token};
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Language, Options, Warning};

/// Whether JSON-LD formats.
pub const IMPLEMENTED: bool = true;
/// Whether `sort` orders JSON-LD terms.
pub const SORT_IMPLEMENTED: bool = true;

/// JSON-LD in the formatting pipeline.
pub struct JsonLd;

impl LangImpl for JsonLd {
    type Reference = JsonReference;

    fn language(&self) -> Language {
        Language::JsonLd
    }

    fn lex_mode(&self) -> LexMode {
        LexMode::Json
    }

    fn reference(&self, text: &str, _tokens: &[Token]) -> Result<JsonReference, FormatError> {
        json_reference(text)
    }

    fn warnings(&self, _r: &JsonReference) -> Vec<Warning> {
        Vec::new()
    }

    fn cst<'s>(
        &self,
        text: &'s str,
        tokens: Vec<Token>,
        _r: &JsonReference,
    ) -> Result<Tree<'s>, FormatError> {
        parse::parse(text, tokens)
    }

    fn rules(&self) -> &dyn CommentRules {
        &print::RULES
    }

    fn print(
        &self,
        tree: &Tree<'_>,
        comments: &Comments,
        opts: &Options,
    ) -> Result<Printed, FormatError> {
        print::print(tree, comments, opts)
    }

    fn equivalent(&self, r: &JsonReference, output: &str) -> Result<(), FormatError> {
        json_equivalent(r, output)
    }
}
