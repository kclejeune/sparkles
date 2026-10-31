//! Linting N-Triples and N-Quads: the term rules of spec X04 §3.4 over each term of the
//! document. Prefix and variable rules do not apply, because the line formats have
//! neither. The reference parser is oxttl, which also decides what a syntax error is.
//!
//! The Turtle lexer reads the line formats' terms, so the rules run on a flat tree that
//! holds every significant token under one root, with a [`NodeKind::Literal`] around each
//! string that has a language tag or a datatype. That is the shape the term rules read in
//! Turtle, so they run unchanged.

use super::{Cst, LintError, Out, terms};
use crate::Language;
use crate::lex::{LexMode, Token, TokenKind, lex};
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeData, NodeId, TokenId, Tree};

pub(super) fn run(text: &str, lang: Language, out: &mut Out<'_>) -> Result<(), LintError> {
    let reference = crate::check::graph::parse(text, lang);
    out.deadline()?;
    if let Err(e) = reference {
        if !terms::iri_space_in_error(text, &e, out) {
            out.syntax(&e);
        }
        return Ok(());
    }
    let tree = flat_tree(text, lex(text, LexMode::Turtle));
    let c = Cst::new(&tree);
    terms::run(&c, out);
    Ok(())
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
