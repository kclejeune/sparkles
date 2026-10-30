//! The SHACLC lexer ([`crate::lex::LexMode::Shaclc`]): the lossless mode of the reader's
//! own scanner ([`sparkles_shaclc::lex::lex_lossless_partial`]) with its lexemes mapped to
//! the formatter's token kinds. Because the formatter and the reader share the scanner,
//! they cannot disagree about where a token ends.
//!
//! The mapping: prefixed names are [`TokenKind::PnameNs`] (no local part) or
//! [`TokenKind::PnameLn`], `@ex:S` is [`TokenKind::AtPname`], the `@` before an IRI is
//! [`TokenKind::At`], language tags are [`TokenKind::LangDir`], strings are the four
//! string kinds by their quotes, numbers are the unsigned kinds whatever their sign,
//! punctuation has the kinds of the other languages, and bare words are
//! [`TokenKind::Word`]. A lexical error leaves the rest of the text as one
//! [`TokenKind::Unknown`] token (the reference parser has rejected the input by then).

use crate::lex::{Token, TokenKind};
use sparkles_shaclc::lex::{LexKind, lex_lossless_partial};

/// Tokenize SHACLC text (at most `u32::MAX` bytes). A leading BOM belongs to no token; the
/// last token is an empty [`TokenKind::Eof`].
pub fn lex(src: &str) -> Vec<Token> {
    let (lexemes, error) = lex_lossless_partial(src);
    let mut out = Vec::with_capacity(lexemes.len() + 2);
    let mut end = if src.starts_with('\u{feff}') { 3 } else { 0 };
    for l in &lexemes {
        out.push(Token {
            kind: kind(l.kind, &src[l.start..l.end]),
            start: l.start as u32,
            len: (l.end - l.start) as u32,
        });
        end = l.end;
    }
    if error.is_some() && end < src.len() {
        out.push(Token {
            kind: TokenKind::Unknown,
            start: end as u32,
            len: (src.len() - end) as u32,
        });
    }
    out.push(Token {
        kind: TokenKind::Eof,
        start: src.len() as u32,
        len: 0,
    });
    out
}

fn kind(kind: LexKind, text: &str) -> TokenKind {
    use TokenKind as T;
    match kind {
        LexKind::Whitespace => T::Whitespace,
        LexKind::Comment => T::Comment,
        LexKind::IriRef => T::IriRef,
        LexKind::PName if text.ends_with(':') => T::PnameNs,
        LexKind::PName => T::PnameLn,
        LexKind::AtPName => T::AtPname,
        LexKind::At => T::At,
        LexKind::LangTag => T::LangDir,
        LexKind::String => match (text.starts_with("'''"), text.starts_with("\"\"\"")) {
            // the empty strings `''` and `""` are short
            (true, _) if text.len() >= 6 => T::StringLong1,
            (_, true) if text.len() >= 6 => T::StringLong2,
            _ if text.starts_with('\'') => T::String1,
            _ => T::String2,
        },
        LexKind::Integer => T::Integer,
        LexKind::Decimal => T::Decimal,
        LexKind::Double => T::Double,
        LexKind::Word => T::Word,
        LexKind::Punct => match text {
            "{" => T::LBrace,
            "}" => T::RBrace,
            "[" => T::LBracket,
            "]" => T::RBracket,
            "(" => T::LParen,
            ")" => T::RParen,
            "|" => T::Pipe,
            "/" => T::Slash,
            "^" => T::Hat,
            "^^" => T::HatHat,
            "*" => T::Star,
            "+" => T::Plus,
            "?" => T::Question,
            "!" => T::Bang,
            "=" => T::Eq,
            "." => T::Dot,
            ".." => T::DotDot,
            "->" => T::Arrow,
            _ => T::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind as T;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        lex(src)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .map(|t| (t.kind, &src[t.range()]))
            .collect()
    }

    #[test]
    fn maps_every_lexeme_kind() {
        assert_eq!(
            kinds("shape ex:S -> :C {")
                .iter()
                .map(|k| k.0)
                .collect::<Vec<_>>(),
            [T::Word, T::PnameLn, T::Arrow, T::PnameLn, T::LBrace, T::Eof]
        );
        let k = kinds("ex: ex:a @ex:T @<i> 'a'@en \"\"\"b\"\"\" '''c''' '' \"\" -1 .5 1e3 ^^ ..");
        let kinds: Vec<TokenKind> = k.iter().map(|k| k.0).collect();
        assert_eq!(
            kinds,
            [
                T::PnameNs,
                T::PnameLn,
                T::AtPname,
                T::At,
                T::IriRef,
                T::String1,
                T::LangDir,
                T::StringLong2,
                T::StringLong1,
                T::String1,
                T::String2,
                T::Integer,
                T::Decimal,
                T::Double,
                T::HatHat,
                T::DotDot,
                T::Eof,
            ]
        );
    }

    #[test]
    fn every_byte_is_in_a_token_and_an_error_leaves_the_rest() {
        for src in [
            "\u{feff}# c\nshape ex:S { ex:p xsd:string . }\n",
            "shape ex:S { # keep\n ex:p \"open",
            "",
        ] {
            let toks = lex(src);
            let bom = if src.starts_with('\u{feff}') { 3 } else { 0 };
            let mut at = bom;
            for t in &toks {
                assert_eq!(t.start as usize, at, "{src:?}");
                at = t.end();
            }
            assert_eq!(at, src.len());
            assert_eq!(toks.last().unwrap().kind, T::Eof);
        }
        let toks = lex("# keep\nshape ex:S { ex:p \"open");
        assert_eq!(toks.iter().filter(|t| t.kind == T::Comment).count(), 1);
        assert_eq!(toks[toks.len() - 2].kind, T::Unknown);
    }
}
