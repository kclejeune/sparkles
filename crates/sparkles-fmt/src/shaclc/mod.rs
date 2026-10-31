//! SHACL Compact Syntax (SHACLC, `text/shaclc`): the lexer ([`lex`]), the lossless parser
//! ([`parse`]), the printing rules ([`print`]) and their place in the pipeline
//! ([`Shaclc`]). The reference parser is the reader of `sparkles-shaclc`, and the output
//! must give a shapes graph isomorphic to the input's.

pub mod lex;
pub mod parse;
pub mod print;

use crate::check::LangImpl;
use crate::check::graph::isomorphic_bounded;
use crate::doc::Printed;
use crate::lex::{LexMode, Token};
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{Check, FormatError, Language, Options, Warning};
use oxrdf::{GraphName, Quad};

/// Whether SHACLC formats.
pub const IMPLEMENTED: bool = true;

/// The base IRI of both parses, so relative IRIs resolve and `IMPORTS` has a base (a
/// `BASE` in the text wins).
pub const SHACLC_BASE: &str = crate::check::SPARQL_BASE;

/// SHACLC in the formatting pipeline.
pub struct Shaclc;

/// The reference parse of a SHACLC document: its shapes graph as quads of the default
/// graph.
#[derive(Clone, Debug)]
pub struct ShaclcReference {
    pub quads: Vec<Quad>,
}

/// Parse `text` as SHACLC with the reader of `sparkles-shaclc` and the synthetic base.
pub fn parse_reference(text: &str) -> Result<Vec<Quad>, FormatError> {
    let doc = sparkles_shaclc::parse(text, Some(SHACLC_BASE)).map_err(|e| {
        let offset = crate::offset_of(text, e.line as u32, e.column as u32);
        FormatError::Syntax {
            message: e.message,
            line: e.line as u32,
            column: e.column as u32,
            offset,
        }
    })?;
    // in the reader's order, so equal documents number their blank nodes alike and the
    // comparison takes its fast path
    Ok(doc
        .triples
        .into_iter()
        .map(|t| t.in_graph(GraphName::DefaultGraph))
        .collect::<Vec<Quad>>())
}

impl LangImpl for Shaclc {
    type Reference = ShaclcReference;

    fn language(&self) -> Language {
        Language::Shaclc
    }

    fn lex_mode(&self) -> LexMode {
        LexMode::Shaclc
    }

    fn reference(&self, text: &str, tokens: &[Token]) -> Result<ShaclcReference, FormatError> {
        crate::sparql::nesting(text, tokens)?;
        Ok(ShaclcReference {
            quads: parse_reference(text)?,
        })
    }

    fn warnings(&self, _r: &ShaclcReference) -> Vec<Warning> {
        Vec::new()
    }

    fn cst<'s>(
        &self,
        text: &'s str,
        tokens: Vec<Token>,
        _r: &ShaclcReference,
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

    fn equivalent(&self, r: &ShaclcReference, output: &str) -> Result<(), FormatError> {
        let unsafe_ = FormatError::Unsafe {
            check: Check::Graph,
        };
        let out = parse_reference(output).map_err(|_| unsafe_.clone())?;
        match isomorphic_bounded(&r.quads, &out) {
            true => Ok(()),
            false => Err(unsafe_),
        }
    }
}
