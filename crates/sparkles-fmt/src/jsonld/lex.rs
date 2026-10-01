//! The JSON lexer ([`crate::lex::LexMode::Json`]): lossless like the RDF lexer, with
//! strings as [`TokenKind::String2`] (escapes as written), numbers as the integer,
//! decimal and double kinds (the `Negative` ones with a `-`; lexemes as written), `true`,
//! `false` and `null` as [`TokenKind::Word`], `{` `}` `[` `]` `,` and
//! [`TokenKind::Colon`]. Whitespace is RFC 8259's: space, tab, CR and LF.
//!
//! JSON has no comments, so there are no [`TokenKind::Comment`] tokens: a `/` is an
//! [`TokenKind::Unknown`] token (the reference parser has rejected such input already).

use crate::lex::{Token, TokenKind};

/// Tokenize JSON text (at most `u32::MAX` bytes). A leading BOM belongs to no token; the
/// last token is an empty [`TokenKind::Eof`].
pub fn lex(src: &str) -> Vec<Token> {
    let mut out = Vec::with_capacity(src.len() / 4 + 1);
    let mut pos = if src.starts_with('\u{feff}') { 3 } else { 0 };
    while pos < src.len() {
        let (kind, len) = next(&src[pos..]);
        debug_assert!(len > 0 && src.is_char_boundary(pos + len));
        out.push(Token {
            kind,
            start: pos as u32,
            len: len as u32,
        });
        pos += len;
    }
    out.push(Token {
        kind: TokenKind::Eof,
        start: src.len() as u32,
        len: 0,
    });
    out
}

/// The kind and byte length of the token at the start of `rest` (not empty).
fn next(rest: &str) -> (TokenKind, usize) {
    use TokenKind::*;
    let b = rest.as_bytes();
    match b[0] {
        b' ' | b'\t' | b'\r' | b'\n' => (
            Whitespace,
            b.iter()
                .position(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
                .unwrap_or(b.len()),
        ),
        b'{' => (LBrace, 1),
        b'}' => (RBrace, 1),
        b'[' => (LBracket, 1),
        b']' => (RBracket, 1),
        b',' => (Comma, 1),
        b':' => (Colon, 1),
        b'"' => (String2, string(b)),
        b'-' | b'0'..=b'9' => number(b).unwrap_or((Unknown, 1)),
        c if c.is_ascii_alphabetic() => (
            Word,
            b.iter()
                .position(|c| !c.is_ascii_alphanumeric())
                .unwrap_or(b.len()),
        ),
        _ => (Unknown, rest.chars().next().map_or(1, char::len_utf8)),
    }
}

/// The length of the string at the start of `b` (which starts with `"`): up to the
/// closing quote, or the end of the input when there is none. A backslash escapes the
/// byte after it; bytes of non-ASCII characters are never `"` or `\`, so the token ends
/// on a character boundary.
fn string(b: &[u8]) -> usize {
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'"' => return i + 1,
            b'\\' if b.get(i + 1).is_some_and(u8::is_ascii) => i += 2,
            _ => i += 1,
        }
    }
    b.len()
}

/// A number at the start of `b`: `-`? digits (`.` digits)? ([eE] [+-]? digits)?, with
/// its kind. `None` for a `-` without a digit after it.
fn number(b: &[u8]) -> Option<(TokenKind, usize)> {
    use TokenKind::*;
    let digits = |from: usize| {
        b[from..]
            .iter()
            .position(|c| !c.is_ascii_digit())
            .unwrap_or(b.len() - from)
    };
    let negative = b[0] == b'-';
    let mut i = usize::from(negative);
    let n = digits(i);
    if n == 0 {
        return None;
    }
    i += n;
    let mut decimal = false;
    if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        decimal = true;
        i += 1 + digits(i + 1);
    }
    let mut double = false;
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
        let n = digits(i + 1 + sign);
        if n > 0 {
            double = true;
            i += 1 + sign + n;
        }
    }
    let kind = match (negative, double, decimal) {
        (false, true, _) => Double,
        (false, false, true) => Decimal,
        (false, false, false) => Integer,
        (true, true, _) => DoubleNegative,
        (true, false, true) => DecimalNegative,
        (true, false, false) => IntegerNegative,
    };
    Some((kind, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        lex(src)
            .iter()
            .map(|t| (t.kind, t.text(src)))
            .filter(|(k, _)| *k != TokenKind::Eof)
            .collect()
    }

    #[test]
    fn tokens() {
        use TokenKind::*;
        assert_eq!(
            kinds("{\"a\\\"b\": [1, -2.5, 3e+4, -0E1, true, null]}"),
            [
                (LBrace, "{"),
                (String2, "\"a\\\"b\""),
                (Colon, ":"),
                (Whitespace, " "),
                (LBracket, "["),
                (Integer, "1"),
                (Comma, ","),
                (Whitespace, " "),
                (DecimalNegative, "-2.5"),
                (Comma, ","),
                (Whitespace, " "),
                (Double, "3e+4"),
                (Comma, ","),
                (Whitespace, " "),
                (DoubleNegative, "-0E1"),
                (Comma, ","),
                (Whitespace, " "),
                (Word, "true"),
                (Comma, ","),
                (Whitespace, " "),
                (Word, "null"),
                (RBracket, "]"),
                (RBrace, "}"),
            ]
        );
        // a comment is not JSON: unknown tokens, never trivia
        assert_eq!(
            kinds("// x\n"),
            [
                (Unknown, "/"),
                (Unknown, "/"),
                (Whitespace, " "),
                (Word, "x"),
                (Whitespace, "\n")
            ]
        );
        assert_eq!(kinds("-x"), [(Unknown, "-"), (Word, "x")]);
        assert_eq!(kinds("1."), [(Integer, "1"), (Unknown, ".")]);
        assert_eq!(kinds("\"\\é\""), [(String2, "\"\\é\"")]);
        assert_eq!(kinds("\"open"), [(String2, "\"open")]);
        assert_eq!(kinds("é"), [(Unknown, "é")]);
    }

    #[test]
    fn lossless() {
        for src in [
            "\u{feff}{ \"a\" : [ 1 , 2 ] }\r\n",
            "  \"x\\u00e9\"  ",
            "/* c */ {}",
            "[1e, 2E-, -]",
            "",
        ] {
            let toks = lex(src);
            let bom = if src.starts_with('\u{feff}') { 3 } else { 0 };
            let joined: String = toks.iter().map(|t| t.text(src)).collect();
            assert_eq!(joined, &src[bom..]);
            assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
        }
    }
}
