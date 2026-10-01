//! The lossless JSON parser, on the event parser of [`crate::sparql::parse`]: a
//! [`NodeKind::JsonDocument`] root holding one value; [`NodeKind::JsonObject`] with
//! [`NodeKind::JsonMember`] children (the key string, `:`, the value, the `,` after it),
//! [`NodeKind::JsonArray`] with its values and their `,`, and [`NodeKind::JsonScalar`]
//! around strings, numbers, `true`, `false` and `null`.
//!
//! The reference parser (json-event-parser) has accepted the input and bounded its
//! nesting ([`crate::check::json::MAX_DEPTH`]), so the recursion here is bounded too.

use crate::FormatError;
use crate::lex::{Token, TokenKind};
use crate::sparql::parse::{Parser, build};
use crate::syntax::NodeKind;
use crate::tree::Tree;

/// Parse a whole JSON document.
pub fn parse<'s>(src: &'s str, tokens: Vec<Token>) -> Result<Tree<'s>, FormatError> {
    let events = {
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::JsonDocument);
        value(&mut p);
        if !p.at(TokenKind::Eof) {
            p.error("expected the end of the input");
        }
        root.complete(&mut p);
        p.finish()?
    };
    Ok(build(src, tokens, events))
}

/// An object, an array or a scalar.
fn value(p: &mut Parser<'_>) {
    match p.current() {
        TokenKind::LBrace => object(p),
        TokenKind::LBracket => array(p),
        TokenKind::String2 => scalar(p),
        k if k.is_number() => scalar(p),
        TokenKind::Word if matches!(p.nth_text(0), "true" | "false" | "null") => scalar(p),
        _ => p.error("expected a JSON value"),
    }
}

fn scalar(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::JsonScalar);
    p.bump();
    m.complete(p);
}

/// `{ "key": value, … }`
fn object(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::JsonObject);
    p.bump();
    if !p.at(TokenKind::RBrace) {
        loop {
            let member = p.start(NodeKind::JsonMember);
            p.expect(TokenKind::String2);
            p.expect(TokenKind::Colon);
            value(p);
            let more = p.eat(TokenKind::Comma);
            member.complete(p);
            if !more || p.has_error() {
                break;
            }
        }
    }
    p.expect(TokenKind::RBrace);
    m.complete(p);
}

/// `[ value, … ]`
fn array(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::JsonArray);
    p.bump();
    if !p.at(TokenKind::RBracket) {
        loop {
            value(p);
            if !p.eat(TokenKind::Comma) || p.has_error() {
                break;
            }
        }
    }
    p.expect(TokenKind::RBracket);
    m.complete(p);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonld::lex::lex;

    fn dump(src: &str) -> String {
        parse(src, lex(src)).unwrap().dump()
    }

    #[test]
    fn tree_shape() {
        assert_eq!(
            dump("\u{feff} {\"a\": [1, {}], \"b\": null}\n"),
            "JsonDocument
  JsonObject
    LBrace \"{\"
    JsonMember
      String2 \"\\\"a\\\"\"
      Colon \":\"
      JsonArray
        LBracket \"[\"
        JsonScalar
          Integer \"1\"
        Comma \",\"
        JsonObject
          LBrace \"{\"
          RBrace \"}\"
        RBracket \"]\"
      Comma \",\"
    JsonMember
      String2 \"\\\"b\\\"\"
      Colon \":\"
      JsonScalar
        Word \"null\"
    RBrace \"}\"
"
        );
        let src = "\u{feff} 1 ";
        let t = parse(src, lex(src)).unwrap();
        assert_eq!(t.range(t.root()), 3..src.len());
    }

    #[test]
    fn rejects_what_is_not_json() {
        for src in [
            "",
            "{\"a\" 1}",
            "[1,]",
            "{\"a\": 1,}",
            "[1 2]",
            "nul",
            "{} {}",
            "{a: 1}",
        ] {
            let e = parse(src, lex(src));
            assert!(
                matches!(e, Err(FormatError::Unsupported { .. })),
                "{src:?}: {e:?}"
            );
        }
    }
}
