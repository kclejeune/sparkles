//! The lossless SHACLC parser, on the event parser of [`crate::sparql::parse`]
//! (`Parser`, `Marker`, `Completed`, `build`): recursive descent over the significant
//! tokens, one function per rule of the grammar the reader of `sparkles-shaclc` follows.
//! Like the other parsers it does not recover, and an error is
//! [`FormatError::Unsupported`], because the reference parser has accepted the input.
//!
//! The shapes it builds: a [`NodeKind::ShaclcDoc`] root holding `BASE` and `PREFIX`
//! (as [`NodeKind::BaseDecl`] and [`NodeKind::PrefixDecl`], their keywords re-kinded to
//! `Kw(Base)` and `Kw(Prefix)`), `IMPORTS` and the shapes. A shape holds its keyword, its
//! IRI, the `->` target classes and a [`NodeKind::ShaclcBody`] of constraints, each of
//! them a node with its final `.`. A property shape holds a [`NodeKind::ShaclcPath`] (tokens
//! only) and then atoms, counts and `|` chains. An atom is an optional `!` and a
//! parameter `name=value`, a type, a node kind, a shape reference or a nested body. The
//! keywords `shape`, `shapeClass` and `IMPORTS` and every parameter name stay
//! [`TokenKind::Word`] tokens.
//!
//! It is a little more lenient than the grammar where that costs nothing (any word is a
//! parameter name), since the reader has rejected what is not SHACLC before this parser
//! runs.

use crate::FormatError;
use crate::lex::{Token, TokenKind};
use crate::sparql::keywords::Kw;
use crate::sparql::parse::{Completed, Parser, build};
use crate::syntax::NodeKind;
use crate::tree::Tree;
use TokenKind as T;

/// Parse a whole SHACLC document.
pub fn parse<'s>(src: &'s str, tokens: Vec<Token>) -> Result<Tree<'s>, FormatError> {
    let events = {
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::ShaclcDoc);
        // after an error every lookahead is the end
        while !p.at(T::Eof) {
            item(&mut p);
        }
        root.complete(&mut p);
        p.finish()?
    };
    Ok(build(src, tokens, events))
}

/// Whether the current token is the word `word`, in any case.
fn at_word(p: &Parser<'_>, word: &str) -> bool {
    p.at(T::Word) && p.nth_text(0).eq_ignore_ascii_case(word)
}

/// A directive or a shape.
fn item(p: &mut Parser<'_>) {
    if at_word(p, "BASE") {
        let m = p.start(NodeKind::BaseDecl);
        p.bump_as(T::Kw(Kw::Base));
        p.expect(T::IriRef);
        m.complete(p);
    } else if at_word(p, "PREFIX") {
        let m = p.start(NodeKind::PrefixDecl);
        p.bump_as(T::Kw(Kw::Prefix));
        p.expect(T::PnameNs);
        p.expect(T::IriRef);
        m.complete(p);
    } else if at_word(p, "IMPORTS") {
        let m = p.start(NodeKind::ShaclcImports);
        p.bump();
        iri(p);
        m.complete(p);
    } else if at_word(p, "shapeClass") || at_word(p, "shape") {
        let class = at_word(p, "shapeClass");
        let m = p.start(NodeKind::ShaclcShape);
        p.bump();
        iri(p);
        if !class && p.eat(T::Arrow) {
            iri(p);
            while at_iri(p) {
                iri(p);
            }
        }
        body(p);
        m.complete(p);
    } else {
        p.error("expected BASE, IMPORTS, PREFIX, `shape` or `shapeClass`");
    }
}

/// An IRI reference or a prefixed name is next.
fn at_iri(p: &Parser<'_>) -> bool {
    matches!(p.current(), T::IriRef | T::PnameLn | T::PnameNs)
}

fn iri(p: &mut Parser<'_>) {
    if at_iri(p) {
        p.bump();
    } else {
        p.error("expected an IRI");
    }
}

/// `{ constraint* }`
fn body(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ShaclcBody);
    p.expect(T::LBrace);
    while !p.at(T::RBrace) && !p.at(T::Eof) {
        constraint(p);
    }
    p.expect(T::RBrace);
    m.complete(p);
}

/// A node constraint, a shape reference or a property shape, with its `.`.
fn constraint(p: &mut Parser<'_>) {
    match p.current() {
        T::Word | T::Bang => {
            let m = p.start(NodeKind::ShaclcNodeConstraint);
            loop {
                node_or(p);
                if p.at(T::Dot) || p.has_error() {
                    break;
                }
                if p.at(T::Eof) || p.at(T::RBrace) {
                    p.error("expected `.`");
                    break;
                }
            }
            p.expect(T::Dot);
            m.complete(p);
        }
        T::AtPname | T::At => {
            let m = p.start(NodeKind::ShaclcShapeRef);
            shape_ref(p);
            p.expect(T::Dot);
            m.complete(p);
        }
        T::IriRef | T::PnameLn | T::PnameNs | T::LParen | T::Hat => {
            let m = p.start(NodeKind::ShaclcPropertyShape);
            path(p);
            loop {
                if p.at(T::LBracket) {
                    count(p);
                } else if p.at(T::Dot) || p.at(T::Eof) {
                    break;
                } else {
                    property_or(p);
                }
            }
            p.expect(T::Dot);
            m.complete(p);
        }
        _ => p.error("expected a constraint or `}`"),
    }
}

/// `nodeNot ( | nodeNot )*`: a [`NodeKind::ShaclcOr`] only when there is a `|`.
fn node_or(p: &mut Parser<'_>) {
    let first = atom(p, false);
    chain(p, first, |p| {
        atom(p, false);
    });
}

/// `propertyNot ( | propertyNot )*`
fn property_or(p: &mut Parser<'_>) {
    let first = atom(p, true);
    chain(p, first, |p| {
        atom(p, true);
    });
}

/// The `| operand` tail of a chain whose first operand is `first`.
fn chain(p: &mut Parser<'_>, first: Completed, operand: impl Fn(&mut Parser<'_>)) {
    if !p.at(T::Pipe) {
        return;
    }
    let m = first.precede(p, NodeKind::ShaclcOr);
    while p.eat(T::Pipe) {
        operand(p);
    }
    m.complete(p);
}

/// `!`? and `name=value` (a node parameter, `property` false) or, in a property shape, a
/// shape reference, a type, a node kind, `name=value` or a nested body.
fn atom(p: &mut Parser<'_>, property: bool) -> Completed {
    let m = p.start(NodeKind::ShaclcAtom);
    p.eat(T::Bang);
    if property && matches!(p.current(), T::AtPname | T::At) {
        shape_ref(p);
    } else if property && at_iri(p) {
        p.bump();
    } else if property && p.at(T::LBrace) {
        body(p);
    } else if p.at(T::Word) {
        p.bump();
        if p.eat(T::Eq) {
            value(p);
        } else if !property {
            p.error("expected `=`");
        }
    } else {
        p.error("expected a parameter");
    }
    m.complete(p)
}

/// `@ex:Shape` or `@` and an IRI reference.
fn shape_ref(p: &mut Parser<'_>) {
    if p.eat(T::AtPname) {
        return;
    }
    p.expect(T::At);
    p.expect(T::IriRef);
}

/// `[min..max]`
fn count(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ShaclcCount);
    p.expect(T::LBracket);
    p.expect(T::Integer);
    p.expect(T::DotDot);
    if !p.eat(T::Integer) {
        p.expect(T::Star);
    }
    p.expect(T::RBracket);
    m.complete(p);
}

/// An IRI, a literal or an array.
fn value(p: &mut Parser<'_>) {
    if p.at(T::LBracket) {
        let m = p.start(NodeKind::ShaclcArray);
        p.bump();
        while !p.at(T::RBracket) && !p.at(T::Eof) {
            let item = p.start(NodeKind::ShaclcArrayItem);
            term(p);
            item.complete(p);
        }
        p.expect(T::RBracket);
        m.complete(p);
    } else {
        term(p);
    }
}

/// An IRI or a literal.
fn term(p: &mut Parser<'_>) {
    match p.current() {
        T::IriRef | T::PnameLn | T::PnameNs | T::Integer | T::Decimal | T::Double => p.bump(),
        T::Word if p.nth_text(0).eq_ignore_ascii_case("true") => p.bump(),
        T::Word if p.nth_text(0).eq_ignore_ascii_case("false") => p.bump(),
        k if k.is_string() => {
            let m = p.start(NodeKind::ShaclcLiteral);
            p.bump();
            if p.at(T::LangDir) {
                p.bump();
            } else if p.eat(T::HatHat) {
                iri(p);
            }
            m.complete(p);
        }
        _ => p.error("expected an IRI or a literal"),
    }
}

/// A property path: tokens only, in one [`NodeKind::ShaclcPath`].
fn path(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ShaclcPath);
    path_alternative(p);
    m.complete(p);
}

/// Whether a `|` followed by the token `after` continues a path (the reader's rule).
fn continues_path(after: TokenKind) -> bool {
    matches!(
        after,
        T::IriRef | T::PnameLn | T::PnameNs | T::LParen | T::Hat
    )
}

fn path_alternative(p: &mut Parser<'_>) {
    path_sequence(p);
    while p.at(T::Pipe) && continues_path(p.nth(1)) {
        p.bump();
        path_sequence(p);
    }
}

fn path_sequence(p: &mut Parser<'_>) {
    path_element(p);
    while p.eat(T::Slash) {
        path_element(p);
    }
}

/// `^`? and a primary with its `?`, `*` or `+`.
fn path_element(p: &mut Parser<'_>) {
    p.eat(T::Hat);
    if p.eat(T::LParen) {
        path_alternative(p);
        p.expect(T::RParen);
    } else {
        iri(p);
    }
    if !p.eat(T::Question) && !p.eat(T::Star) {
        p.eat(T::Plus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};

    fn dump(src: &str) -> String {
        parse(src, lex(src, LexMode::Shaclc)).unwrap().dump()
    }

    #[test]
    fn a_shape_with_each_kind_of_constraint() {
        let d = dump(
            "PREFIX ex: <http://e/>\nshape ex:S -> ex:C {\n closed=true .\n ex:p xsd:string|xsd:int [0..*] .\n @ex:T .\n}",
        );
        for kind in [
            "ShaclcDoc",
            "PrefixDecl",
            "ShaclcShape",
            "ShaclcBody",
            "ShaclcNodeConstraint",
            "ShaclcPropertyShape",
            "ShaclcPath",
            "ShaclcOr",
            "ShaclcCount",
            "ShaclcShapeRef",
        ] {
            assert!(d.contains(kind), "{kind} in\n{d}");
        }
        assert_eq!(d.matches("ShaclcAtom").count(), 3, "{d}");
    }

    #[test]
    fn paths_alternatives_and_nested_shapes() {
        let d =
            dump("shape <s> { ^<a>/(<b>|<c>)* IRI | Literal { <z> !in=[<x> \"y\"@en 1] . } . }");
        assert_eq!(d.matches("ShaclcBody").count(), 2, "{d}");
        assert_eq!(d.matches("ShaclcArrayItem").count(), 3, "{d}");
        assert_eq!(d.matches("ShaclcLiteral").count(), 1, "{d}");
        // `|` after the first path element continues the path, else it is an atom chain
        let path_alt = dump("shape <s> { <a>|<b> <t> . }");
        assert!(!path_alt.contains("ShaclcOr"), "{path_alt}");
        let atom_alt = dump("shape <s> { <a> <t>|<u> . }");
        assert!(atom_alt.contains("ShaclcOr"), "{atom_alt}");
    }

    #[test]
    fn keywords_are_rekinded_and_the_tree_is_lossless() {
        let src = "# h\nbase <b>\nPrefix ex: <http://e/> # c\nshape ex:S { }\n";
        let tree = parse(src, lex(src, LexMode::Shaclc)).unwrap();
        let text: String = tree.tokens.iter().map(|t| t.text(src)).collect();
        assert_eq!(text, src);
        let d = tree.dump();
        assert!(d.contains("Kw(Base)") && d.contains("Kw(Prefix)"), "{d}");
    }

    #[test]
    fn errors_are_the_formatters_not_the_readers() {
        let src = "shape ex:S { ex:p ";
        let e = parse(src, lex(src, LexMode::Shaclc)).unwrap_err();
        assert!(
            matches!(e, FormatError::Unsupported { line: 1, .. }),
            "{e:?}"
        );
    }
}
