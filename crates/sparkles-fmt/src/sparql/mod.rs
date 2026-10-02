//! SPARQL 1.2 queries and updates: the lossless parser ([`parse`]), the printing rules
//! ([`print`]) and their place in the pipeline ([`Sparql`]).

pub mod keywords;
pub mod parse;
pub mod print;

use crate::check::{self, LangImpl, SparqlReference};
use crate::doc::Printed;
use crate::lex::{LexMode, Token, TokenKind};
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
        nesting(text, tokens)?;
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

/// The deepest nesting of brackets the formatter takes: the same bound as JSON-LD's
/// ([`check::json::MAX_DEPTH`]).
pub const MAX_DEPTH: usize = check::json::MAX_DEPTH;

/// Refuse brackets (`(`, `[`, `{`, `<<`, `<<(`, `{|`) nested deeper than [`MAX_DEPTH`],
/// before anything recurses over them: the reference parser, the formatter's parser and
/// the printer all do, and a few thousand levels overflow the stack of a server's worker
/// thread (a few hundred, of WebAssembly's). An unbalanced closer counts as nothing; the
/// parsers report it.
pub fn nesting(text: &str, tokens: &[Token]) -> Result<(), FormatError> {
    let mut depth = 0usize;
    for t in tokens {
        match t.kind {
            TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::LtLt
            | TokenKind::LtLtParen
            | TokenKind::LBracePipe => {
                depth += 1;
                if depth > MAX_DEPTH {
                    let (line, column) = crate::line_col(text, t.start as usize);
                    return Err(FormatError::Unsupported {
                        message: format!("nesting deeper than {MAX_DEPTH} levels"),
                        line,
                        column,
                    });
                }
            }
            TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::GtGt
            | TokenKind::ParenGtGt
            | TokenKind::PipeRBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::lex;

    fn deep(open: &str, close: &str, n: usize) -> String {
        format!(
            "SELECT * {{ FILTER({}1{}) }}",
            open.repeat(n),
            close.repeat(n)
        )
    }

    #[test]
    fn nesting_is_bounded() {
        // `{` and `FILTER(` are two levels
        let ok = deep("(", ")", MAX_DEPTH - 2);
        assert!(nesting(&ok, &lex(&ok, LexMode::Sparql)).is_ok());
        let text = deep("(", ")", MAX_DEPTH - 1);
        assert_eq!(
            nesting(&text, &lex(&text, LexMode::Sparql)),
            Err(FormatError::Unsupported {
                message: format!("nesting deeper than {MAX_DEPTH} levels"),
                line: 1,
                column: 19 + MAX_DEPTH as u32 - 2,
            })
        );
        // closed brackets give their levels back
        let wide = deep("(1)+", "", 3 * MAX_DEPTH);
        assert!(nesting(&wide, &lex(&wide, LexMode::Sparql)).is_ok());
    }

    #[test]
    fn deep_nesting_is_refused_before_it_overflows_the_stack() {
        for (open, close) in [("(", ")"), ("{", "}"), ("<<( <a:s> <a:p> ", " )>>")] {
            let text = deep(open, close, 5000);
            let e = crate::format(&text, Language::Sparql, &Options::default());
            assert!(
                matches!(&e, Err(FormatError::Unsupported { message, .. })
                    if message.starts_with("nesting deeper")),
                "{open}: {e:?}"
            );
        }
    }
}
