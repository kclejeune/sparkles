//! Turtle 1.2 and TriG 1.2 (SHACL shapes graphs included): the lossless parser
//! ([`parse`]), the printing rules ([`print`]), the opt-in sorting ([`sort`]), their
//! place in the pipeline ([`Turtle`]) and streaming ([`stream`]). The reference parser is oxttl, and the output must
//! parse to an isomorphic graph or dataset ([`crate::check::graph`]).
//!
//! The tree reuses SPARQL's node kinds for what the grammars share (directives, triples
//! statements, property list entries, objects, blank node property lists, collections,
//! RDF 1.2 terms, literals), so the Turtle and SPARQL printers can share their term and
//! triples helpers; the document roots and TriG's graph blocks have kinds of their own.

pub mod parse;
pub mod print;
pub mod sort;
pub mod stream;

use crate::check::LangImpl;
use crate::check::graph::{RdfReference, rdf_equivalent, rdf_reference};
use crate::doc::Printed;
use crate::lex::{LexMode, Token};
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Language, Options, Warning};

/// Whether Turtle and TriG format.
pub const IMPLEMENTED: bool = true;

/// Turtle (`trig: false`) or TriG in the formatting pipeline.
pub struct Turtle {
    pub trig: bool,
}

impl LangImpl for Turtle {
    type Reference = RdfReference;

    fn language(&self) -> Language {
        match self.trig {
            false => Language::Turtle,
            true => Language::TriG,
        }
    }

    fn lex_mode(&self) -> LexMode {
        LexMode::Turtle
    }

    fn reference(&self, text: &str, tokens: &[Token]) -> Result<RdfReference, FormatError> {
        crate::sparql::nesting(text, tokens)?;
        rdf_reference(text, self.language())
    }

    fn warnings(&self, _r: &RdfReference) -> Vec<Warning> {
        Vec::new()
    }

    fn cst<'s>(
        &self,
        text: &'s str,
        tokens: Vec<Token>,
        _r: &RdfReference,
    ) -> Result<Tree<'s>, FormatError> {
        parse::parse(text, tokens, self.trig)
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
        print::print(tree, comments, opts, self.trig)
    }

    fn equivalent(&self, r: &RdfReference, output: &str) -> Result<(), FormatError> {
        rdf_equivalent(r, output)
    }
}
