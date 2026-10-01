//! RDF terms: literals with their language tag or datatype (N9 shorthand, N10
//! quotes), IRIs (N7), keywords in the grammar's spelling. Variables keep their sigil and
//! language tags their case: every other token prints as written.

use super::Ctx;
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::tree::{Element, NodeId, TokenId};

/// `Literal`: a string and its language tag (`"x"@en`, the tag as written), or its
/// datatype (`"x"^^xsd:string`). A typed literal whose content is the numeric or
/// boolean token of its datatype prints as that token (`"1"^^xsd:integer` → `1`).
pub fn literal(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let tokens: Vec<TokenId> = cx
        .children(n)
        .into_iter()
        .filter_map(|e| match e {
            Element::Token(t) => Some(t),
            Element::Node(_) => None,
        })
        .collect();
    let [string, rest @ ..] = tokens.as_slice() else {
        return cx.verbatim(n);
    };
    if let [hathat, datatype] = rest
        && cx.tree.token_kind(*hathat) == TokenKind::HatHat
        && let Some(short) = cx.literal_shorthand(*string, *datatype)
    {
        return short;
    }
    let mut parts = vec![token(cx, *string)];
    parts.extend(rest.iter().map(|&t| token(cx, t)));
    cx.concat(parts)
}

/// A term token as [`Ctx::term`] prints it, and `()` and `[]` without the whitespace
/// inside.
pub fn token(cx: &mut Ctx<'_, '_>, t: TokenId) -> DocId {
    match cx.tree.token_kind(t) {
        TokenKind::Nil => cx.tok_as(t, "()"),
        TokenKind::Anon => cx.tok_as(t, "[]"),
        _ => cx.term(t),
    }
}

/// A child element: a node with [`Ctx::node`], a token with [`token`].
pub fn element(cx: &mut Ctx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Node(c) => cx.node(c),
        Element::Token(t) => token(cx, t),
    }
}
