//! Linting SHACLC (spec G03, X04): the prefix rules, which also know the four prefixes a
//! SHACLC document starts with, and the term rules over its literals and IRIs. The
//! reference parser is the reader of `sparkles-shaclc`. It refuses an undeclared prefix,
//! so the lint declares those for the reference parse and reports `undefined-prefix`
//! itself, as it does in Turtle.

use super::{Cst, LintError, Out, prefixes, terms};
use crate::lex::{LexMode, Token, TokenKind, lex};

/// The prefixes a SHACLC document starts with.
pub(crate) const STANDARD_PREFIXES: [(&str, &str); 4] = [
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("sh", "http://www.w3.org/ns/shacl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
];

pub(super) fn run(text: &str, out: &mut Out<'_>) -> Result<(), LintError> {
    let tokens = lex(text, LexMode::Shaclc);
    if let Err(e) = crate::sparql::nesting(text, &tokens) {
        out.syntax(&e);
        return Ok(());
    }
    let undeclared = undeclared_labels(text, &tokens);
    let reference = reference(text, &undeclared);
    out.deadline()?;
    if let Err(e) = reference {
        if !terms::iri_space_in_error(text, &e, out) {
            out.syntax(&e);
        }
        return Ok(());
    }
    match crate::shaclc::parse::parse(text, tokens) {
        Ok(tree) => {
            let c = Cst::new(&tree);
            prefixes::run(&c, out, &Default::default());
            terms::run(&c, out)?;
        }
        Err(e) => out.syntax(&e),
    }
    Ok(())
}

/// The prefix labels a document uses without declaring them first, each once. The four
/// standard prefixes are declared from the start.
pub(super) fn undeclared_labels(text: &str, tokens: &[Token]) -> Vec<String> {
    let sig: Vec<&Token> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
    let mut declared: std::collections::HashSet<String> = STANDARD_PREFIXES
        .iter()
        .map(|(l, _)| (*l).to_string())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for (i, t) in sig.iter().enumerate() {
        let s = t.text(text);
        let label = match t.kind {
            TokenKind::PnameNs | TokenKind::PnameLn => &s[..s.find(':').unwrap_or(s.len())],
            TokenKind::AtPname => {
                let s = &s[1..];
                &s[..s.find(':').unwrap_or(s.len())]
            }
            _ => continue,
        };
        let directive = t.kind == TokenKind::PnameNs
            && i > 0
            && sig[i - 1].kind == TokenKind::Word
            && sig[i - 1].text(text).eq_ignore_ascii_case("prefix");
        if directive {
            declared.insert(label.to_string());
        } else if !declared.contains(label) && !out.iter().any(|l| l == label) {
            out.push(label.to_string());
        }
    }
    out
}

/// The reader's parse of `text`, with the `undeclared` prefixes declared in front of it.
/// An error's offset is that of `text`.
fn reference(text: &str, undeclared: &[String]) -> Result<(), crate::FormatError> {
    quads(text, undeclared).map(|_| ())
}

/// The shapes graph of `text`, with the `undeclared` prefixes declared in front of it.
pub(super) fn quads(
    text: &str,
    undeclared: &[String],
) -> Result<Vec<oxrdf::Quad>, crate::FormatError> {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut prelude = String::new();
    for l in undeclared {
        prelude.push_str(&format!(
            "PREFIX {l}: <{}> ",
            crate::check::undeclared_namespace(l)
        ));
    }
    if prelude.is_empty() {
        return crate::shaclc::parse_reference(text);
    }
    let joined = format!("{prelude}{}", &text[bom..]);
    crate::shaclc::parse_reference(&joined).map_err(|e| match e {
        crate::FormatError::Syntax {
            message, offset, ..
        } => {
            let offset = (bom + offset.saturating_sub(prelude.len())).min(text.len());
            let (line, column) = crate::line_col(text, offset);
            crate::FormatError::Syntax {
                message,
                line,
                column,
                offset,
            }
        }
        other => other,
    })
}
