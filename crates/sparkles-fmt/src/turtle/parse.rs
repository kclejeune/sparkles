//! The lossless Turtle 1.2 and TriG 1.2 parser, on the event parser of
//! [`crate::sparql::parse`] (`Parser`, `Marker`, `Completed`, `build`): recursive descent
//! over the significant tokens, one function per grammar rule. Like the SPARQL parser it
//! does not recover, and an error is [`FormatError::Unsupported`] (oxttl accepted the
//! input, so the error is the formatter's).
//!
//! The shapes it builds: a [`NodeKind::TurtleDoc`] or [`NodeKind::TrigDoc`] root; the
//! directives as `PrefixDecl`, `BaseDecl` and `VersionDecl` (the keyword re-kinded to
//! `Kw(Prefix)` and the like for the SPARQL spelling, `@prefix`, `@base` and `@version`
//! staying [`TokenKind::LangDir`] tokens, the final `.` of the `@` forms inside the
//! node); statements as `TriplesStmt` with `PropertyListEntry` and `Object` children as
//! in SPARQL; TriG's `GRAPH g { … }`, `g { … }` and `{ … }` as [`NodeKind::GraphBlock`]
//! (the `GRAPH` keyword, the label, the braces and the statements).
//!
//! It is a little more lenient than the grammar where that costs nothing (any RDF term
//! in any position, a reified triple's reifier anywhere oxttl allows one): the reference
//! parser has rejected what is not Turtle or TriG before this parser runs.

use crate::FormatError;
use crate::lex::{Token, TokenKind};
use crate::sparql::keywords::Kw;
use crate::sparql::parse::{Parser, build, term};
use crate::syntax::NodeKind;
use crate::tree::Tree;

/// Parse a whole Turtle (`trig: false`) or TriG document.
pub fn parse<'s>(src: &'s str, tokens: Vec<Token>, trig: bool) -> Result<Tree<'s>, FormatError> {
    let events = {
        let mut p = Parser::new(src, &tokens);
        let root = p.start(match trig {
            false => NodeKind::TurtleDoc,
            true => NodeKind::TrigDoc,
        });
        // after an error every lookahead is the end
        while !p.at(TokenKind::Eof) {
            if at_directive(&p) {
                directive(&mut p);
            } else if trig && at_graph_block(&p) {
                graph_block(&mut p);
            } else if !triples(&mut p) {
                p.error("expected `.` after the triples");
            }
        }
        root.complete(&mut p);
        p.finish()?
    };
    Ok(build(src, tokens, events))
}

/// `@prefix`, `@base`, `@version` (case-sensitive) and their SPARQL spellings `PREFIX`,
/// `BASE`, `VERSION` (case-insensitive).
fn at_directive(p: &Parser<'_>) -> bool {
    turtle_directive(p).is_some()
        || p.at_kw(Kw::Prefix)
        || p.at_kw(Kw::Base)
        || p.at_kw(Kw::Version)
}

/// The directive an `@` token spells.
fn turtle_directive(p: &Parser<'_>) -> Option<Kw> {
    if !p.at(TokenKind::LangDir) {
        return None;
    }
    match p.nth_text(0) {
        "@prefix" => Some(Kw::Prefix),
        "@base" => Some(Kw::Base),
        "@version" => Some(Kw::Version),
        _ => None,
    }
}

/// `prefixID | base | version | sparqlPrefix | sparqlBase | sparqlVersion`: a
/// `PrefixDecl`, `BaseDecl` or `VersionDecl` with its keyword, its terms and, in the `@`
/// forms, its `.`.
fn directive(p: &mut Parser<'_>) {
    let (kw, at_form) = match turtle_directive(p) {
        Some(kw) => (kw, true),
        None => (p.current_kw().expect("at a directive"), false),
    };
    let m = p.start(match kw {
        Kw::Prefix => NodeKind::PrefixDecl,
        Kw::Base => NodeKind::BaseDecl,
        _ => NodeKind::VersionDecl,
    });
    match at_form {
        true => p.bump(),
        false => p.bump_as(TokenKind::Kw(kw)),
    }
    match kw {
        Kw::Prefix => {
            p.expect(TokenKind::PnameNs);
            p.expect(TokenKind::IriRef);
        }
        Kw::Base => {
            p.expect(TokenKind::IriRef);
        }
        _ => {
            if !(p.eat(TokenKind::String1) || p.eat(TokenKind::String2)) {
                p.error("expected a version string");
            }
        }
    }
    if at_form {
        p.expect(TokenKind::Dot);
    }
    m.complete(p);
}

/// A TriG graph block starts here: `GRAPH`, `{`, or a graph label before `{`.
fn at_graph_block(p: &Parser<'_>) -> bool {
    p.at_kw(Kw::Graph) || p.at(TokenKind::LBrace) || (at_label(p) && p.nth(1) == TokenKind::LBrace)
}

/// `labelOrSubject ::= iri | BlankNode`
fn at_label(p: &Parser<'_>) -> bool {
    term::at_iri(p) || term::at_blank_node(p)
}

/// `"GRAPH" labelOrSubject wrappedGraph | labelOrSubject wrappedGraph | wrappedGraph`: a
/// `GraphBlock` holding the keyword, the label, `{`, the statements and `}`.
fn graph_block(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::GraphBlock);
    let keyword = p.eat_kw(Kw::Graph);
    if keyword || !p.at(TokenKind::LBrace) {
        if term::at_iri(p) {
            term::iri(p);
        } else if term::at_blank_node(p) {
            p.bump();
        } else {
            p.error("expected a graph name");
        }
    }
    p.expect(TokenKind::LBrace);
    // `triplesBlock ::= triples ( '.' triplesBlock? )?`: the last `.` is optional
    while !p.at(TokenKind::RBrace) && !p.at(TokenKind::Eof) {
        if !triples(p) {
            break;
        }
    }
    p.expect(TokenKind::RBrace);
    m.complete(p);
}

/// `triples '.'`: one `TriplesStmt` with its `.` when there is one. Returns whether it
/// ended with a `.`.
fn triples(p: &mut Parser<'_>) -> bool {
    let m = p.start(NodeKind::TriplesStmt);
    // a reified triple and a blank node property list can stand alone
    let alone = match p.current() {
        TokenKind::LtLt => {
            reified_triple(p);
            true
        }
        TokenKind::LBracket => {
            bnode_property_list(p);
            true
        }
        _ => {
            graph_node(p);
            false
        }
    };
    if !alone || at_verb(p) {
        property_list(p);
    }
    let dot = p.eat(TokenKind::Dot);
    m.complete(p);
    dot
}

/// `verb ::= iri | 'a'`
fn at_verb(p: &Parser<'_>) -> bool {
    term::at_iri(p) || p.at_kw(Kw::A)
}

/// `predicateObjectList ::= verb objectList (';' (verb objectList)?)*`: one
/// `PropertyListEntry` per verb, holding the `;`s after it.
fn property_list(p: &mut Parser<'_>) {
    loop {
        let m = p.start(NodeKind::PropertyListEntry);
        if !term::verb(p) {
            m.complete(p);
            return;
        }
        object_list(p);
        let mut semicolons = false;
        while p.eat(TokenKind::Semicolon) {
            semicolons = true;
        }
        m.complete(p);
        if !(semicolons && at_verb(p)) {
            return;
        }
    }
}

/// `objectList ::= object annotation (',' object annotation)*`: one `Object` per object,
/// holding its reifiers, annotation blocks and the `,` after it.
fn object_list(p: &mut Parser<'_>) {
    loop {
        let m = p.start(NodeKind::Object);
        graph_node(p);
        annotation(p);
        let comma = p.eat(TokenKind::Comma);
        m.complete(p);
        if !comma || p.has_error() {
            return;
        }
    }
}

/// A subject or an object: an IRI, a blank node, a literal, a collection, a blank node
/// property list, a triple term or a reified triple.
fn graph_node(p: &mut Parser<'_>) {
    match p.current() {
        TokenKind::LtLt => reified_triple(p),
        TokenKind::LtLtParen => triple_term(p),
        TokenKind::LBracket => bnode_property_list(p),
        TokenKind::LParen => collection(p),
        TokenKind::BlankNodeLabel | TokenKind::Anon | TokenKind::Nil => p.bump(),
        _ => {
            term::iri_or_literal(p);
        }
    }
}

/// `annotation ::= (reifier | annotationBlock)*`
fn annotation(p: &mut Parser<'_>) {
    loop {
        match p.current() {
            TokenKind::Tilde => reifier(p),
            TokenKind::LBracePipe => {
                let m = p.start(NodeKind::AnnotationBlock);
                p.bump();
                property_list(p);
                p.expect(TokenKind::PipeRBrace);
                m.complete(p);
            }
            _ => return,
        }
    }
}

/// `reifier ::= '~' (iri | BlankNode)?`
fn reifier(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::Reifier);
    p.expect(TokenKind::Tilde);
    if term::at_iri(p) {
        term::iri(p);
    } else if term::at_blank_node(p) {
        p.bump();
    }
    m.complete(p);
}

/// `blankNodePropertyList ::= '[' predicateObjectList ']'`, and `[` `]` with a comment
/// between (an anonymous blank node: the comment makes the brackets two tokens).
fn bnode_property_list(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::BNodePropertyList);
    p.expect(TokenKind::LBracket);
    if !p.at(TokenKind::RBracket) {
        property_list(p);
    }
    p.expect(TokenKind::RBracket);
    m.complete(p);
}

/// `collection ::= '(' object* ')'`, each item a `CollectionItem` (empty with a comment
/// between the parentheses, which makes them two tokens).
fn collection(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::Collection);
    p.expect(TokenKind::LParen);
    while !p.at(TokenKind::RParen) && !p.at(TokenKind::Eof) {
        let item = p.start(NodeKind::CollectionItem);
        graph_node(p);
        item.complete(p);
    }
    p.expect(TokenKind::RParen);
    m.complete(p);
}

/// `reifiedTriple ::= '<<' rtSubject verb rtObject reifier? '>>'`
fn reified_triple(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ReifiedTriple);
    p.expect(TokenKind::LtLt);
    graph_node(p);
    term::verb(p);
    graph_node(p);
    if p.at(TokenKind::Tilde) {
        reifier(p);
    }
    p.expect(TokenKind::GtGt);
    m.complete(p);
}

/// `tripleTerm ::= '<<(' ttSubject verb ttObject ')>>'`
fn triple_term(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::TripleTerm);
    p.expect(TokenKind::LtLtParen);
    graph_node(p);
    term::verb(p);
    graph_node(p);
    p.expect(TokenKind::ParenGtGt);
    m.complete(p);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};

    fn tree(src: &str, trig: bool) -> String {
        parse(src, lex(src, LexMode::Turtle), trig).unwrap().dump()
    }

    #[test]
    fn directives_in_both_spellings() {
        let t = tree(
            "@prefix ex: <http://e/> .\nprefix x: <http://x/>\n@base <b> . BASE <c> VERSION '1.2' @version \"1.2\" .",
            false,
        );
        assert_eq!(
            t,
            "TurtleDoc\n  PrefixDecl\n    LangDir \"@prefix\"\n    PnameNs \"ex:\"\n    IriRef \"<http://e/>\"\n    Dot \".\"\n  PrefixDecl\n    Kw(Prefix) \"prefix\"\n    PnameNs \"x:\"\n    IriRef \"<http://x/>\"\n  BaseDecl\n    LangDir \"@base\"\n    IriRef \"<b>\"\n    Dot \".\"\n  BaseDecl\n    Kw(Base) \"BASE\"\n    IriRef \"<c>\"\n  VersionDecl\n    Kw(Version) \"VERSION\"\n    String1 \"'1.2'\"\n  VersionDecl\n    LangDir \"@version\"\n    String2 \"\\\"1.2\\\"\"\n    Dot \".\"\n"
        );
    }

    #[test]
    fn graph_blocks() {
        let t = tree(
            "GRAPH <g> { <a> <b> <c> } <h> { <a> <b> <c> . } { } <a> <b> <c> .",
            true,
        );
        let kinds: Vec<&str> = t
            .lines()
            .filter(|l| !l.starts_with("      "))
            .map(str::trim)
            .collect();
        assert_eq!(
            kinds,
            [
                "TrigDoc",
                "GraphBlock",
                "Kw(Graph) \"GRAPH\"",
                "IriRef \"<g>\"",
                "LBrace \"{\"",
                "TriplesStmt",
                "RBrace \"}\"",
                "GraphBlock",
                "IriRef \"<h>\"",
                "LBrace \"{\"",
                "TriplesStmt",
                "RBrace \"}\"",
                "GraphBlock",
                "LBrace \"{\"",
                "RBrace \"}\"",
                "TriplesStmt",
                "IriRef \"<a>\"",
                "PropertyListEntry",
                "Dot \".\"",
            ]
        );
    }

    #[test]
    fn empty_brackets_with_comments() {
        let t = tree("<a> <b> [ # c\n ], ( # d\n ) .", false);
        assert!(t.contains("BNodePropertyList\n          LBracket"), "{t}");
        assert!(t.contains("Collection\n          LParen"), "{t}");
    }

    #[test]
    fn unsupported_is_the_formatters_error() {
        let src = "<a> <b> <c>";
        let e = parse(src, lex(src, LexMode::Turtle), false).unwrap_err();
        assert!(matches!(e, FormatError::Unsupported { .. }), "{e:?}");
    }
}
