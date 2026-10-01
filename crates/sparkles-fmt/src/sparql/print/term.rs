//! RDF terms: literals with their language tag or datatype (N9 shorthand, N10
//! quotes), IRIs (N7), keywords in the grammar's spelling. Variables keep their sigil and
//! language tags their case: every other token prints as written.

use super::Ctx;
use crate::QuoteStyle;
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::normalize::{self, PrefixScope};
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
    {
        if let Some(iri) = datatype_iri(cx, *datatype)
            && let Some(short) = normalize::literal_shorthand(cx.tree.token_text(*string), &iri)
        {
            return cx.tok_as(*string, short.to_string());
        }
        let s = token(cx, *string);
        let h = cx.tok(*hathat);
        let d = token(cx, *datatype);
        return cx.concat([s, h, d]);
    }
    let mut parts = vec![token(cx, *string)];
    parts.extend(rest.iter().map(|&t| cx.tok(t)));
    cx.concat(parts)
}

/// A term token: an IRI compacted to a prefixed name (N7), a string with double quotes
/// (N10), a keyword in the grammar's spelling (`a`, `true`, `UNDEF`), `()` and `[]`
/// without the whitespace inside, anything else as written.
pub fn token(cx: &mut Ctx<'_, '_>, t: TokenId) -> DocId {
    let text = cx.tree.token_text(t);
    let printed = match cx.tree.token_kind(t) {
        TokenKind::IriRef if cx.opts.compact_iris => normalize::compact_iri(text, &scope(cx), t),
        k if k.is_string() && cx.opts.quote_style == QuoteStyle::Double => {
            normalize::requote(text, k)
        }
        TokenKind::Kw(_) => return cx.kw(t),
        // the whitespace inside `( )` and `[ ]` goes
        TokenKind::Nil => Some("()".to_string()),
        TokenKind::Anon => Some("[]".to_string()),
        _ => None,
    };
    match printed {
        Some(p) => cx.tok_as(t, p),
        None => cx.tok(t),
    }
}

/// A child element: a node with [`Ctx::node`], a token with [`token`].
pub fn element(cx: &mut Ctx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Node(c) => cx.node(c),
        Element::Token(t) => token(cx, t),
    }
}

/// A verb token: `a` for `rdf:type` (N8) unless `type-shorthand` is off, otherwise as
/// [`token`].
pub fn verb(cx: &mut Ctx<'_, '_>, t: TokenId) -> DocId {
    let may_be_type = match cx.tree.token_kind(t) {
        TokenKind::IriRef => true,
        TokenKind::PnameLn => cx.tree.token_text(t).ends_with(":type"),
        _ => false,
    };
    if cx.opts.type_shorthand && may_be_type && normalize::is_rdf_type(cx.tree, t, &scope(cx)) {
        return cx.tok_as(t, "a");
    }
    token(cx, t)
}

/// The full IRI of a datatype token, when it is written plainly: an `IRIREF`'s content,
/// or a prefixed name without `\` escapes whose prefix is declared.
fn datatype_iri(cx: &Ctx<'_, '_>, t: TokenId) -> Option<String> {
    let text = cx.tree.token_text(t);
    match cx.tree.token_kind(t) {
        TokenKind::IriRef => Some(text[1..text.len() - 1].to_string()),
        TokenKind::PnameLn | TokenKind::PnameNs if !text.contains('\\') => {
            let (label, local) = text.split_once(':')?;
            let ns = scope(cx).resolve(label, t)?.to_string();
            Some(ns + local)
        }
        _ => None,
    }
}

/// The prefixes in scope.
///
/// Built per call, and only where a normalization needs it; the printing context could
/// hold it once per tree.
fn scope(cx: &Ctx<'_, '_>) -> PrefixScope {
    PrefixScope::from_tree(cx.tree)
}
