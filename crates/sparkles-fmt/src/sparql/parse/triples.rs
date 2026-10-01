//! Triples: subjects with property lists, object lists, blank node property lists,
//! collections, and the RDF 1.2 reified triples, reifiers and annotations.
//!
//! The shapes: a `TriplesStmt` holds its subject (a term token or node), its
//! `PropertyListEntry`s and its `.`. An entry holds its verb (a token, or a path node),
//! its `Object`s and the `;`s after it. An `Object` holds its term, its `Reifier`s and
//! `AnnotationBlock`s, and the `,` after it.

use super::term::{self, TripleTermCtx};
use super::{Parser, path};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// Which triples grammar: graph patterns allow property paths as verbs
/// (`TriplesSameSubjectPath`); templates and quads do not (`TriplesSameSubject`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Path,
    Template,
}

/// `TriplesSameSubject(Path)` and the `.` after it, if any: one `TriplesStmt`. Returns
/// whether it ended with a `.`.
pub fn triples_stmt(p: &mut Parser<'_>, mode: Mode) -> bool {
    let m = p.start(NodeKind::TriplesStmt);
    // a reified triple, a blank node property list or a collection can stand alone
    let alone = match p.current() {
        TokenKind::LtLt => {
            reified_triple(p);
            true
        }
        TokenKind::LBracket => {
            bnode_property_list(p, mode);
            true
        }
        TokenKind::LParen => {
            collection(p, mode);
            true
        }
        _ => {
            term::var_or_term(p);
            false
        }
    };
    if !alone || at_verb(p, mode) {
        property_list(p, mode);
    }
    let dot = p.eat(TokenKind::Dot);
    m.complete(p);
    dot
}

/// Whether the current token can start a verb.
pub fn at_verb(p: &Parser<'_>, mode: Mode) -> bool {
    term::at_var(p)
        || term::at_iri(p)
        || p.at_kw(Kw::A)
        || (mode == Mode::Path
            && matches!(
                p.current(),
                TokenKind::Hat | TokenKind::Bang | TokenKind::LParen
            ))
}

/// `PropertyListNotEmpty` (`PropertyListPathNotEmpty`): entries separated by `;`, a `;`
/// at the end or doubled allowed.
pub fn property_list(p: &mut Parser<'_>, mode: Mode) {
    loop {
        let m = p.start(NodeKind::PropertyListEntry);
        let plain = if mode == Mode::Path && !term::at_var(p) {
            path::path(p)
        } else {
            term::verb(p)
        };
        if object_list(p, mode) && !plain {
            p.error("reifiers and annotations are not allowed after a property path");
        }
        let mut semicolons = false;
        while p.eat(TokenKind::Semicolon) {
            semicolons = true;
        }
        m.complete(p);
        if !(semicolons && at_verb(p, mode)) {
            break;
        }
    }
}

/// `ObjectList(Path)`: `Object`s separated by `,`. Returns whether any has a reifier
/// or an annotation.
fn object_list(p: &mut Parser<'_>, mode: Mode) -> bool {
    let mut annotated = false;
    loop {
        let m = p.start(NodeKind::Object);
        graph_node(p, mode);
        annotated |= annotations(p, mode);
        let comma = p.eat(TokenKind::Comma);
        m.complete(p);
        if !comma || p.has_error() {
            return annotated;
        }
    }
}

/// `GraphNode(Path)`: a term, a triple term, a reified triple, a collection or a blank
/// node property list.
pub fn graph_node(p: &mut Parser<'_>, mode: Mode) {
    match p.current() {
        TokenKind::LtLt => reified_triple(p),
        TokenKind::LBracket => bnode_property_list(p, mode),
        TokenKind::LParen => collection(p, mode),
        _ => {
            term::var_or_term(p);
        }
    }
}

/// `Annotation(Path)`: reifiers and annotation blocks, in any order. Returns whether
/// there is any.
fn annotations(p: &mut Parser<'_>, mode: Mode) -> bool {
    let mut any = false;
    // an annotation block right after a reifier annotates it; any other block has an
    // implicit blank node reifier
    let mut after_reifier = false;
    loop {
        match p.current() {
            TokenKind::Tilde => {
                reifier(p);
                after_reifier = true;
            }
            TokenKind::LBracePipe => {
                if !after_reifier {
                    p.blank_node_here();
                }
                let m = p.start(NodeKind::AnnotationBlock);
                p.bump();
                property_list(p, mode);
                p.expect(TokenKind::PipeRBrace);
                m.complete(p);
                after_reifier = false;
            }
            _ => return any,
        }
        any = true;
    }
}

/// `Reifier ::= '~' VarOrReifierId?`
fn reifier(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::Reifier);
    p.expect(TokenKind::Tilde);
    if term::at_var(p) || term::at_blank_node(p) {
        p.bump();
    } else if term::at_iri(p) {
        term::iri(p);
    } else {
        // an implicit blank node
        p.blank_node_here();
    }
    m.complete(p);
}

/// `BlankNodePropertyList(Path) ::= '[' PropertyListNotEmpty ']'`
fn bnode_property_list(p: &mut Parser<'_>, mode: Mode) {
    p.blank_node_here();
    let m = p.start(NodeKind::BNodePropertyList);
    p.expect(TokenKind::LBracket);
    property_list(p, mode);
    p.expect(TokenKind::RBracket);
    m.complete(p);
}

/// `Collection(Path) ::= '(' GraphNode+ ')'`, each item a `CollectionItem`.
fn collection(p: &mut Parser<'_>, mode: Mode) {
    p.blank_node_here();
    let m = p.start(NodeKind::Collection);
    p.expect(TokenKind::LParen);
    loop {
        let item = p.start(NodeKind::CollectionItem);
        graph_node(p, mode);
        item.complete(p);
        if p.at(TokenKind::RParen) || p.has_error() {
            break;
        }
    }
    p.expect(TokenKind::RParen);
    m.complete(p);
}

/// `ReifiedTriple ::= '<<' ReifiedTripleSubject Verb ReifiedTripleObject Reifier? '>>'`
fn reified_triple(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ReifiedTriple);
    p.expect(TokenKind::LtLt);
    reified_triple_term(p);
    term::verb(p);
    reified_triple_term(p);
    if p.at(TokenKind::Tilde) {
        reifier(p);
    } else {
        // an implicit blank node reifier
        p.blank_node_here();
    }
    p.expect(TokenKind::GtGt);
    m.complete(p);
}

/// `ReifiedTripleSubject` and `ReifiedTripleObject`: a variable, an IRI, a literal, a
/// blank node, a triple term or a reified triple.
fn reified_triple_term(p: &mut Parser<'_>) {
    match p.current() {
        TokenKind::LtLt => reified_triple(p),
        TokenKind::LtLtParen => term::triple_term(p, TripleTermCtx::Pattern),
        TokenKind::Nil => p.error("expected the subject or object of a reified triple"),
        _ => {
            term::var_or_term(p);
        }
    }
}
