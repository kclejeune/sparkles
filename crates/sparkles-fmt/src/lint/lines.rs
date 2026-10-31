//! Linting N-Triples and N-Quads: the term rules of spec X04 §3.4 over each term of the
//! document. Prefix and variable rules do not apply, because the line formats have
//! neither. The reference parser is oxttl, which also decides what a syntax error is. The
//! parse streams, and both it and the rules look at the deadline every few steps.
//!
//! The Turtle lexer reads the line formats' terms, so the rules run on a flat tree that
//! holds every significant token under one root, with a [`NodeKind::Literal`] around each
//! string that has a language tag or a datatype. That is the shape the term rules read in
//! Turtle, so they run unchanged.

use super::{Cst, LintError, Out, terms};
use crate::lex::{LexMode, Token, TokenKind, lex};
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeData, NodeId, TokenId, Tree};
use crate::{FormatError, Language};

pub(super) fn run(text: &str, lang: Language, out: &mut Out<'_>) -> Result<(), LintError> {
    out.deadline()?;
    if let Err(e) = reference(text, lang, out)? {
        // a syntax error is reported only within the deadline
        out.deadline()?;
        if !terms::iri_space_in_error(text, &e, out) {
            out.syntax(&e);
        }
        return Ok(());
    }
    terms_pass(text, out)
}

/// oxttl's parse of the document, streamed without keeping the quads, with the deadline
/// checked as it goes. The outer error is the deadline, the inner one a syntax error.
fn reference(
    text: &str,
    lang: Language,
    out: &mut Out<'_>,
) -> Result<Result<(), FormatError>, LintError> {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let body = &text[bom..];
    let positioned = |e: oxttl::TurtleSyntaxError| {
        let offset = bom + e.location().start.offset as usize;
        let (line, column) = crate::line_col(text, offset);
        FormatError::Syntax {
            message: e.message().to_string(),
            line,
            column,
            offset,
        }
    };
    if lang == Language::NQuads {
        for q in oxttl::NQuadsParser::new().for_slice(body) {
            out.tick()?;
            if let Err(e) = q {
                return Ok(Err(positioned(e)));
            }
        }
    } else {
        for t in oxttl::NTriplesParser::new().for_slice(body) {
            out.tick()?;
            if let Err(e) = t {
                return Ok(Err(positioned(e)));
            }
        }
    }
    out.deadline()?;
    Ok(Ok(()))
}

/// The term rules over a document that parses, with the deadline checked between the
/// stages and every few steps of the rules.
pub(super) fn terms_pass(text: &str, out: &mut Out<'_>) -> Result<(), LintError> {
    let tokens = lex(text, LexMode::Turtle);
    out.deadline()?;
    let tree = flat_tree(text, tokens);
    out.deadline()?;
    let c = Cst::new(&tree);
    terms::run(&c, out)
}

/// A tree of one root over all the significant tokens of `src`, with a `Literal` node
/// around each string and the language tag or `^^` and datatype that follow it.
fn flat_tree(src: &str, tokens: Vec<Token>) -> Tree<'_> {
    let bom = if src.starts_with('\u{feff}') { 3 } else { 0 };
    let mut nodes = vec![NodeData {
        kind: NodeKind::TurtleDoc,
        parent: None,
        children: Vec::new(),
        range: bom..src.len() as u32,
    }];
    let sig: Vec<usize> = (0..tokens.len())
        .filter(|&i| !tokens[i].kind.is_trivia())
        .collect();
    let mut at = 0;
    while at < sig.len() {
        let i = sig[at];
        let kind = tokens[i].kind;
        if kind == TokenKind::Eof {
            at += 1;
            continue;
        }
        let tail = |n: usize| (at + 1..=at + n).map(|k| sig.get(k).map(|&j| tokens[j].kind));
        let len = if !kind.is_string() {
            0
        } else {
            match tail(2).collect::<Vec<_>>().as_slice() {
                [Some(TokenKind::LangDir), ..] => 1,
                [
                    Some(TokenKind::HatHat),
                    Some(TokenKind::IriRef | TokenKind::PnameLn),
                ] => 2,
                _ => 0,
            }
        };
        if len == 0 {
            nodes[0].children.push(Element::Token(TokenId(i as u32)));
            at += 1;
            continue;
        }
        let id = NodeId(nodes.len() as u32);
        let last = sig[at + len];
        nodes.push(NodeData {
            kind: NodeKind::Literal,
            parent: Some(NodeId(0)),
            children: sig[at..=at + len]
                .iter()
                .map(|&j| Element::Token(TokenId(j as u32)))
                .collect(),
            range: tokens[i].start..tokens[last].start + tokens[last].len,
        });
        nodes[0].children.push(Element::Node(id));
        at += len + 1;
    }
    Tree { src, tokens, nodes }
}
