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
use std::time::Instant;

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
    let (_, triples) = sparkles_shaclc::parse_ordered(text, Some(SHACLC_BASE)).map_err(|e| {
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
    Ok(triples
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
        self.equivalent_by(r, output, None)
    }

    fn equivalent_by(
        &self,
        r: &ShaclcReference,
        output: &str,
        deadline: Option<Instant>,
    ) -> Result<(), FormatError> {
        let unsafe_ = FormatError::Unsafe {
            check: Check::Graph,
        };
        let out = parse_reference(output).map_err(|_| unsafe_.clone())?;
        match isomorphic_bounded(&r.quads, &out, deadline)? {
            Some(true) => Ok(()),
            Some(false) => Err(unsafe_),
            // never accept what was not verified
            None => Err(FormatError::Unsupported {
                message: "could not verify the formatted output: comparing its shapes graph \
                          with the input's takes more work than the budget allows; input \
                          left unchanged"
                    .to_string(),
                line: 1,
                column: 1,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(text: &str) -> ShaclcReference {
        ShaclcReference {
            quads: parse_reference(text).unwrap(),
        }
    }

    /// Two properties with `in` lists that share 30 entries and end in the given values.
    fn in_lists(ends: [&str; 2]) -> String {
        let shared: String = (0..30).map(|i| format!("<v{i}> ")).collect();
        format!(
            "shape <s> {{ <p0> in=[{shared}<{}>] . <p1> in=[{shared}<{}>] . }}",
            ends[0], ends[1]
        )
    }

    /// `n` identical property shapes.
    fn identical(n: usize) -> String {
        format!("shape <s> {{ {}}}", "<p> IRI . ".repeat(n))
    }

    #[test]
    fn exchanged_list_tails_are_a_different_graph() {
        let input = reference(&in_lists(["x", "y"]));
        let err = Shaclc
            .equivalent(&input, &in_lists(["y", "x"]))
            .unwrap_err();
        assert_eq!(
            err,
            FormatError::Unsafe {
                check: Check::Graph
            }
        );
        // the same text in another layout is the same graph
        assert_eq!(
            Shaclc.equivalent(&input, &in_lists(["x", "y"]).replace(" . ", " .\n")),
            Ok(())
        );
    }

    #[test]
    fn ten_identical_property_shapes_are_not_nine() {
        let err = Shaclc
            .equivalent(&reference(&identical(10)), &identical(9))
            .unwrap_err();
        assert_eq!(
            err,
            FormatError::Unsafe {
                check: Check::Graph
            }
        );
        assert_eq!(
            Shaclc.equivalent(&reference(&identical(10)), &identical(10)),
            Ok(())
        );
    }

    #[test]
    fn a_large_comparison_watches_the_deadline() {
        // two `in` lists that share 120 copies of an 8 KiB literal, with exchanged tails
        let big = "x".repeat(8192);
        let doc = |ends: [&str; 2]| {
            let shared = format!("\"{big}\" ").repeat(120);
            format!(
                "shape <s> {{ <p0> in=[{shared}<{}>] . <p1> in=[{shared}<{}>] . }}",
                ends[0], ends[1]
            )
        };
        let input = reference(&doc(["x", "y"]));
        let output = doc(["y", "x"]);
        // an expired deadline is a timeout, which is not a refusal of the output
        let past = Instant::now() - std::time::Duration::from_secs(1);
        assert_eq!(
            Shaclc.equivalent_by(&input, &output, Some(past)),
            Err(FormatError::Timeout)
        );
        // within a deadline, the comparison ends in a verdict well before it
        let start = Instant::now();
        let err = Shaclc
            .equivalent_by(
                &input,
                &output,
                Some(start + std::time::Duration::from_secs(10)),
            )
            .unwrap_err();
        assert_eq!(
            err,
            FormatError::Unsafe {
                check: Check::Graph
            }
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }
}
