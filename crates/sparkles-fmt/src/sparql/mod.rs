//! SPARQL 1.2 queries and updates: the lossless parser ([`parse`]), the printing rules
//! ([`print`]) and their place in the pipeline ([`Sparql`]).

pub mod keywords;
pub mod parse;
pub mod print;

use crate::check::{self, LangImpl, SparqlReference};
use crate::doc::Printed;
use crate::lex::{LexMode, Token};
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Language, Options, Warning};

/// A SPARQL document is a query or an update request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Query,
    Update,
}

/// SPARQL in the formatting pipeline.
pub struct Sparql;

impl LangImpl for Sparql {
    type Reference = SparqlReference;

    fn language(&self) -> Language {
        Language::Sparql
    }

    fn lex_mode(&self) -> LexMode {
        LexMode::Sparql
    }

    fn reference(&self, text: &str, tokens: &[Token]) -> Result<SparqlReference, FormatError> {
        check::sparql_reference(text, tokens)
    }

    fn warnings(&self, r: &SparqlReference) -> Vec<Warning> {
        r.warnings.clone()
    }

    fn cst<'s>(
        &self,
        text: &'s str,
        tokens: Vec<Token>,
        r: &SparqlReference,
    ) -> Result<Tree<'s>, FormatError> {
        parse::parse(text, tokens, r.unit)
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

    fn equivalent(&self, r: &SparqlReference, output: &str) -> Result<(), FormatError> {
        check::sparql_equivalent(r, output)
    }
}
