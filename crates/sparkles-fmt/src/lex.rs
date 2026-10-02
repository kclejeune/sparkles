//! The shared lossless lexer. Every byte of the input after a leading BOM belongs to
//! exactly one token, whitespace runs and `#` comments included, so the concatenated
//! token texts give back the input. Terminals match longest first (SPARQL 1.1 §19.8,
//! Turtle §6.5): `?x-1` is `?x` and `-1`, `ex:a.` is `ex:a` and `.`.
//!
//! The lexer never fails: a character no terminal starts with is an [`TokenKind::Unknown`]
//! token (the reference parser has rejected such input already). Keywords are plain
//! [`TokenKind::Word`]s; the parser re-kinds them to [`TokenKind::Kw`].

use crate::sparql::keywords::Kw;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // trivia
    /// spaces, tabs, CR and LF
    Whitespace,
    /// `#` to the end of the line (without the line break)
    Comment,
    // terms
    IriRef,
    /// `ex:` (also the empty prefix `:`)
    PnameNs,
    /// `ex:local`
    PnameLn,
    BlankNodeLabel,
    /// `?x`
    Var1,
    /// `$x`
    Var2,
    /// `@en`, `@en-GB`, `@ar--rtl`
    LangDir,
    Integer,
    Decimal,
    Double,
    IntegerPositive,
    DecimalPositive,
    DoublePositive,
    IntegerNegative,
    DecimalNegative,
    DoubleNegative,
    /// `'…'`
    String1,
    /// `"…"`
    String2,
    /// `'''…'''`
    StringLong1,
    /// `"""…"""`
    StringLong2,
    /// `[` and `]` with only whitespace between (a comment between makes them two tokens)
    Anon,
    /// `(` and `)` with only whitespace between (a comment between makes them two tokens)
    Nil,
    /// a keyword, built-in name, `a`, `true` or `false` before the parser re-kinds it
    Word,
    /// a [`TokenKind::Word`] the parser recognized
    Kw(Kw),
    // punctuation
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    Dot,
    /// `|`
    Pipe,
    OrOr,
    AndAnd,
    /// `!`
    Bang,
    NotEq,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    /// `^`
    Hat,
    /// `^^`
    HatHat,
    /// `?` (a path modifier)
    Question,
    Tilde,
    /// `<<`
    LtLt,
    /// `>>`
    GtGt,
    /// `<<(`
    LtLtParen,
    /// `)>>`
    ParenGtGt,
    /// `{|`
    LBracePipe,
    /// `|}`
    PipeRBrace,
    /// `:` between a JSON key and its value (JSON only)
    Colon,
    /// one character no terminal starts with
    Unknown,
    /// the end of the input (empty)
    Eof,
}

impl TokenKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::Whitespace | TokenKind::Comment)
    }

    /// The numeric literal kinds, signed ones included.
    pub fn is_number(self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Integer
                | Decimal
                | Double
                | IntegerPositive
                | DecimalPositive
                | DoublePositive
                | IntegerNegative
                | DecimalNegative
                | DoubleNegative
        )
    }

    /// The four string kinds.
    pub fn is_string(self) -> bool {
        use TokenKind::*;
        matches!(self, String1 | String2 | StringLong1 | StringLong2)
    }
}

/// A token: a kind and a byte range of the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: u32,
    pub len: u32,
}

impl Token {
    pub fn end(&self) -> usize {
        (self.start + self.len) as usize
    }

    pub fn range(&self) -> Range<usize> {
        self.start as usize..self.end()
    }

    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.range()]
    }
}

/// Which grammar's terminals to recognize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexMode {
    /// variables, operators, keywords
    Sparql,
    /// Turtle, TriG, N-Triples and N-Quads. Their terminals are a subset of SPARQL's, so
    /// the tokens are the same: `@prefix`, `@base` and `@version` match `LANG_DIR` and are
    /// [`TokenKind::LangDir`] tokens the Turtle parser takes for directives (Turtle §6.5
    /// leaves `"x"@prefix` undefined; oxttl rejects what is not RDF).
    Turtle,
    /// JSON ([`crate::jsonld::lex`])
    Json,
}

/// Tokenize `src` (at most `u32::MAX` bytes). A leading BOM belongs to no token; the
/// last token is an empty [`TokenKind::Eof`].
pub fn lex(src: &str, mode: LexMode) -> Vec<Token> {
    if mode == LexMode::Json {
        return crate::jsonld::lex::lex(src);
    }
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
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let starts = |p: &str| rest.starts_with(p);
    match b[0] {
        b' ' | b'\t' | b'\r' | b'\n' => (
            Whitespace,
            b.iter()
                .position(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
                .unwrap_or(b.len()),
        ),
        b'#' => (
            Comment,
            b.iter()
                .position(|&c| c == b'\n' || c == b'\r')
                .unwrap_or(b.len()),
        ),
        b'<' => {
            if starts("<<(") {
                (LtLtParen, 3)
            } else if starts("<<") {
                (LtLt, 2)
            } else if let Some(n) = iriref(rest) {
                (IriRef, n)
            } else if starts("<=") {
                (Le, 2)
            } else {
                (Lt, 1)
            }
        }
        b'>' if starts(">>") => (GtGt, 2),
        b'>' if starts(">=") => (Ge, 2),
        b'>' => (Gt, 1),
        b'(' => match closed_by(b, b')') {
            Some(n) => (Nil, n),
            None => (LParen, 1),
        },
        b')' if starts(")>>") => (ParenGtGt, 3),
        b')' => (RParen, 1),
        b'[' => match closed_by(b, b']') {
            Some(n) => (Anon, n),
            None => (LBracket, 1),
        },
        b']' => (RBracket, 1),
        b'{' if starts("{|") => (LBracePipe, 2),
        b'{' => (LBrace, 1),
        b'}' => (RBrace, 1),
        b'|' if starts("||") => (OrOr, 2),
        b'|' if starts("|}") => (PipeRBrace, 2),
        b'|' => (Pipe, 1),
        b'&' if starts("&&") => (AndAnd, 2),
        b'!' if starts("!=") => (NotEq, 2),
        b'!' => (Bang, 1),
        b'=' => (Eq, 1),
        b',' => (Comma, 1),
        b';' => (Semicolon, 1),
        b'*' => (Star, 1),
        b'/' => (Slash, 1),
        b'~' => (Tilde, 1),
        b'^' if starts("^^") => (HatHat, 2),
        b'^' => (Hat, 1),
        b'0'..=b'9' => number(b).expect("a digit starts a number"),
        b'.' => number(b).unwrap_or((Dot, 1)),
        b'+' | b'-' => match number(&b[1..]) {
            Some((kind, n)) => (signed(kind, b[0] == b'-'), n + 1),
            None if b[0] == b'+' => (Plus, 1),
            None => (Minus, 1),
        },
        b'?' | b'$' => match varname(&rest[1..]) {
            0 if b[0] == b'?' => (Question, 1),
            0 => (Unknown, 1),
            n => (if b[0] == b'?' { Var1 } else { Var2 }, n + 1),
        },
        b'@' => match langdir(b) {
            0 => (Unknown, 1),
            n => (LangDir, n),
        },
        b'"' | b'\'' => string(rest).unwrap_or((Unknown, 1)),
        b'_' if at(1) == b':' => match blank_node_label(&rest[2..]) {
            0 => (Unknown, 1),
            n => (BlankNodeLabel, n + 2),
        },
        b':' => pname(rest, 0),
        _ => {
            let c = rest.chars().next().expect("not empty");
            if !is_pn_chars_base(c) {
                return (Unknown, c.len_utf8());
            }
            let prefix = pn_prefix(rest);
            if rest[prefix..].starts_with(':') {
                return pname(rest, prefix);
            }
            // a word: letters, digits and `_`
            let n = rest
                .char_indices()
                .find(|&(_, c)| !(is_pn_chars_u(c) || c.is_ascii_digit()))
                .map_or(rest.len(), |(i, _)| i);
            (Word, n)
        }
    }
}

/// `<` … `>` as the grammar defines `IRIREF` ([139]: no `<>"{}|^`\` and nothing up to
/// U+0020), with `\u`/`\U` escapes.
fn iriref(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let mut i = 1;
    loop {
        match *b.get(i)? {
            b'>' => return Some(i + 1),
            b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | 0..=0x20 => return None,
            b'\\' => i += uchar(&b[i..])?,
            _ => i += 1,
        }
    }
}

/// `\uXXXX` or `\UXXXXXXXX` at the start of `b`: its length.
fn uchar(b: &[u8]) -> Option<usize> {
    let n = match b.get(1)? {
        b'u' => 4,
        b'U' => 8,
        _ => return None,
    };
    let hex = b.get(2..2 + n)?;
    hex.iter().all(u8::is_ascii_hexdigit).then_some(n + 2)
}

/// `(` or `[` followed by whitespace only and the closing bracket.
fn closed_by(b: &[u8], close: u8) -> Option<usize> {
    let ws = b[1..]
        .iter()
        .position(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'))?;
    (b[1 + ws] == close).then_some(ws + 2)
}

/// `INTEGER`, `DECIMAL` or `DOUBLE` at the start of `b`, longest match.
fn number(b: &[u8]) -> Option<(TokenKind, usize)> {
    let digits = |from: usize| {
        b.get(from..)
            .map_or(0, |s| s.iter().take_while(|c| c.is_ascii_digit()).count())
    };
    let exponent = |from: usize| {
        if !matches!(b.get(from), Some(b'e' | b'E')) {
            return 0;
        }
        let sign = usize::from(matches!(b.get(from + 1), Some(b'+' | b'-')));
        match digits(from + 1 + sign) {
            0 => 0,
            n => 1 + sign + n,
        }
    };
    let int = digits(0);
    if b.get(int) == Some(&b'.') {
        let frac = digits(int + 1);
        let end = int + 1 + frac;
        if int + frac > 0 {
            let e = exponent(end);
            if e > 0 {
                return Some((TokenKind::Double, end + e));
            }
        }
        if frac > 0 {
            return Some((TokenKind::Decimal, end));
        }
    }
    if int == 0 {
        return None;
    }
    match exponent(int) {
        0 => Some((TokenKind::Integer, int)),
        e => Some((TokenKind::Double, int + e)),
    }
}

fn signed(kind: TokenKind, negative: bool) -> TokenKind {
    use TokenKind::*;
    match (kind, negative) {
        (Integer, false) => IntegerPositive,
        (Decimal, false) => DecimalPositive,
        (Double, false) => DoublePositive,
        (Integer, true) => IntegerNegative,
        (Decimal, true) => DecimalNegative,
        _ => DoubleNegative,
    }
}

/// `VARNAME` at the start of `rest`: its length (0: none).
fn varname(rest: &str) -> usize {
    let mut chars = rest.char_indices();
    match chars.next() {
        Some((_, c)) if is_pn_chars_u(c) || c.is_ascii_digit() => {}
        _ => return 0,
    }
    chars
        .find(|&(_, c)| {
            !(is_pn_chars_u(c)
                || c.is_ascii_digit()
                || c == '\u{B7}'
                || ('\u{300}'..='\u{36F}').contains(&c)
                || ('\u{203F}'..='\u{2040}').contains(&c))
        })
        .map_or(rest.len(), |(i, _)| i)
}

/// `LANG_DIR`: `@` letters, `-` alphanumeric subtags, an optional `--` direction.
fn langdir(b: &[u8]) -> usize {
    let alpha = |from: usize| {
        b.get(from..).map_or(0, |s| {
            s.iter().take_while(|c| c.is_ascii_alphabetic()).count()
        })
    };
    let alnum = |from: usize| {
        b.get(from..).map_or(0, |s| {
            s.iter().take_while(|c| c.is_ascii_alphanumeric()).count()
        })
    };
    let mut i = 1 + alpha(1);
    if i == 1 {
        return 0;
    }
    while b.get(i) == Some(&b'-') && b.get(i + 1) != Some(&b'-') {
        match alnum(i + 1) {
            0 => break,
            n => i += 1 + n,
        }
    }
    if b.get(i) == Some(&b'-') && b.get(i + 1) == Some(&b'-') {
        let n = alpha(i + 2);
        if n > 0 {
            i += 2 + n;
        }
    }
    i
}

/// One of the four string forms, escapes skipped; `None` when unterminated.
fn string(rest: &str) -> Option<(TokenKind, usize)> {
    let q = rest.as_bytes()[0];
    let long = rest.as_bytes().get(..3) == Some(&[q, q, q]);
    let kind = match (q, long) {
        (b'\'', false) => TokenKind::String1,
        (b'"', false) => TokenKind::String2,
        (b'\'', true) => TokenKind::StringLong1,
        _ => TokenKind::StringLong2,
    };
    let open = if long { 3 } else { 1 };
    let mut chars = rest[open..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '\n' | '\r' if !long => return None,
            c if c as u32 == u32::from(q) => {
                if !long {
                    return Some((kind, open + i + 1));
                }
                if rest.as_bytes().get(open + i..open + i + 3) == Some(&[q, q, q]) {
                    return Some((kind, open + i + 3));
                }
            }
            _ => {}
        }
    }
    // an unterminated `"""` is no long string: the longest match is the empty `""`
    long.then_some((
        match q {
            b'\'' => TokenKind::String1,
            _ => TokenKind::String2,
        },
        2,
    ))
}

/// After `_:`: `(PN_CHARS_U | [0-9]) ((PN_CHARS | '.')* PN_CHARS)?`.
fn blank_node_label(rest: &str) -> usize {
    match rest.chars().next() {
        Some(c) if is_pn_chars_u(c) || c.is_ascii_digit() => {}
        _ => return 0,
    }
    dotted(rest, |c| is_pn_chars(c) || c == '.')
}

/// The longest prefix of `rest` whose characters satisfy `ok`, not ending in `.`.
fn dotted(rest: &str, ok: impl Fn(char) -> bool) -> usize {
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        if i > 0 && !ok(c) {
            break;
        }
        if c != '.' {
            end = i + c.len_utf8();
        }
    }
    end
}

/// `PN_PREFIX` at the start of `rest` (which starts with a `PN_CHARS_BASE`): its length.
fn pn_prefix(rest: &str) -> usize {
    dotted(rest, |c| is_pn_chars(c) || c == '.')
}

/// A prefixed name whose `:` is at byte `colon`: `PNAME_NS`, or `PNAME_LN` when a
/// `PN_LOCAL` follows.
fn pname(rest: &str, colon: usize) -> (TokenKind, usize) {
    let local = pn_local(&rest[colon + 1..]);
    if local == 0 {
        (TokenKind::PnameNs, colon + 1)
    } else {
        (TokenKind::PnameLn, colon + 1 + local)
    }
}

/// `PN_LOCAL`: `(PN_CHARS_U | ':' | [0-9] | PLX) ((PN_CHARS | '.' | ':' | PLX)* (PN_CHARS
/// | ':' | PLX))?`, where `PLX` is `%HH` or a `\` escape.
fn pn_local(rest: &str) -> usize {
    let b = rest.as_bytes();
    let plx = |i: usize| -> usize {
        match b.get(i) {
            Some(b'%')
                if b.get(i + 1..i + 3)
                    .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) =>
            {
                3
            }
            Some(b'\\')
                if b.get(i + 1)
                    .is_some_and(|c| b"_~.-!$&'()*+,;=/?#@%".contains(c)) =>
            {
                2
            }
            _ => 0,
        }
    };
    let mut i = 0;
    let mut end = 0;
    while i < rest.len() {
        let n = plx(i);
        if n > 0 {
            i += n;
            end = i;
            continue;
        }
        let c = rest[i..].chars().next().expect("on a char boundary");
        let ok = if i == 0 {
            is_pn_chars_u(c) || c == ':' || c.is_ascii_digit()
        } else {
            is_pn_chars(c) || c == ':' || c == '.'
        };
        if !ok {
            break;
        }
        i += c.len_utf8();
        if c != '.' {
            end = i;
        }
    }
    end
}

/// `PN_CHARS_BASE`
pub fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// `PN_CHARS_U`: `PN_CHARS_BASE` or `_`
pub fn is_pn_chars_u(c: char) -> bool {
    c == '_' || is_pn_chars_base(c)
}

/// `PN_CHARS`
pub fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || ('\u{300}'..='\u{36F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// Whether `s` is a whole `PN_PREFIX` (a non-empty prefix label).
pub fn is_pn_prefix(s: &str) -> bool {
    s.chars().next().is_some_and(is_pn_chars_base) && pn_prefix(s) == s.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        let toks = lex(src, LexMode::Sparql);
        let joined: String = toks.iter().map(|t| t.text(src)).collect();
        assert_eq!(joined, src.trim_start_matches('\u{feff}'), "lossless");
        toks.iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != Eof)
            .map(|t| (t.kind, t.text(src)))
            .collect()
    }

    #[test]
    fn longest_match() {
        assert_eq!(kinds("?x-1"), [(Var1, "?x"), (IntegerNegative, "-1")]);
        assert_eq!(kinds("ex:a."), [(PnameLn, "ex:a"), (Dot, ".")]);
        assert_eq!(kinds("ex:a.b."), [(PnameLn, "ex:a.b"), (Dot, ".")]);
        assert_eq!(kinds("?v+1"), [(Var1, "?v"), (IntegerPositive, "+1")]);
        assert_eq!(kinds("?v + 1"), [(Var1, "?v"), (Plus, "+"), (Integer, "1")]);
        assert_eq!(kinds("1."), [(Integer, "1"), (Dot, ".")]);
        assert_eq!(
            kinds("1.5 .5 1.e3 1e-3 .5E2"),
            [
                (Decimal, "1.5"),
                (Decimal, ".5"),
                (Double, "1.e3"),
                (Double, "1e-3"),
                (Double, ".5E2"),
            ]
        );
        assert_eq!(kinds("1e"), [(Integer, "1"), (Word, "e")]);
        assert_eq!(kinds("-.5"), [(DecimalNegative, "-.5")]);
    }

    #[test]
    fn angle_brackets() {
        assert_eq!(kinds("<http://a/b#c>"), [(IriRef, "<http://a/b#c>")]);
        assert_eq!(kinds("<\\u00e9>"), [(IriRef, "<\\u00e9>")]);
        assert_eq!(kinds("?x<?y"), [(Var1, "?x"), (Lt, "<"), (Var1, "?y")]);
        assert_eq!(
            kinds("?x<?a&&?b>?y"),
            [(Var1, "?x"), (IriRef, "<?a&&?b>"), (Var1, "?y")]
        );
        assert_eq!(kinds("?x <= 2"), [(Var1, "?x"), (Le, "<="), (Integer, "2")]);
        assert_eq!(
            kinds("<< <<( )>> >> >= >"),
            [
                (LtLt, "<<"),
                (LtLtParen, "<<("),
                (ParenGtGt, ")>>"),
                (GtGt, ">>"),
                (Ge, ">="),
                (Gt, ">")
            ]
        );
        assert_eq!(
            kinds("{| |} || | { }"),
            [
                (LBracePipe, "{|"),
                (PipeRBrace, "|}"),
                (OrOr, "||"),
                (Pipe, "|"),
                (LBrace, "{"),
                (RBrace, "}")
            ]
        );
    }

    #[test]
    fn terms() {
        assert_eq!(
            kinds(":a : ex: ex:b:c ex:%41\\.x"),
            [
                (PnameLn, ":a"),
                (PnameNs, ":"),
                (PnameNs, "ex:"),
                (PnameLn, "ex:b:c"),
                (PnameLn, "ex:%41\\.x")
            ]
        );
        assert_eq!(
            kinds("_:b1 _:a.b. $this ?é"),
            [
                (BlankNodeLabel, "_:b1"),
                (BlankNodeLabel, "_:a.b"),
                (Dot, "."),
                (Var2, "$this"),
                (Var1, "?é")
            ]
        );
        assert_eq!(
            kinds("\"x\"@en-GB \"y\"@ar--rtl"),
            [
                (String2, "\"x\""),
                (LangDir, "@en-GB"),
                (String2, "\"y\""),
                (LangDir, "@ar--rtl")
            ]
        );
        assert_eq!(
            kinds("'a\\'b' \"\" '''x''y''' \"\"\"a\n\"b\"\"\"\""),
            [
                (String1, "'a\\'b'"),
                (String2, "\"\""),
                (StringLong1, "'''x''y'''"),
                (StringLong2, "\"\"\"a\n\"b\"\"\""),
                (Unknown, "\""),
            ]
        );
        // an unterminated long quote is an empty string and what follows
        assert_eq!(
            kinds("\"\"\"a\"@en '''b'"),
            [
                (String2, "\"\""),
                (String2, "\"a\""),
                (LangDir, "@en"),
                (String1, "''"),
                (String1, "'b'"),
            ]
        );
        assert_eq!(
            kinds("( ) [\n] (# c\n) [] ()"),
            [
                (Nil, "( )"),
                (Anon, "[\n]"),
                (LParen, "("),
                (RParen, ")"),
                (Anon, "[]"),
                (Nil, "()")
            ]
        );
        assert_eq!(
            kinds("ex:p? ?"),
            [(PnameLn, "ex:p"), (Question, "?"), (Question, "?")]
        );
    }

    #[test]
    fn words_and_trivia() {
        let src = "\u{feff}SELECT * WHERE { ?s a ex:C } # done\r\n";
        assert_eq!(
            kinds(src),
            [
                (Word, "SELECT"),
                (Star, "*"),
                (Word, "WHERE"),
                (LBrace, "{"),
                (Var1, "?s"),
                (Word, "a"),
                (PnameLn, "ex:C"),
                (RBrace, "}")
            ]
        );
        let toks = lex(src, LexMode::Sparql);
        assert_eq!(toks[0].start, 3);
        let comment = toks.iter().find(|t| t.kind == Comment).unwrap();
        assert_eq!(comment.text(src), "# done");
        assert_eq!(toks.last().unwrap().kind, Eof);
        assert_eq!(
            kinds("GROUP_CONCAT isIRI"),
            [(Word, "GROUP_CONCAT"), (Word, "isIRI")]
        );
        assert_eq!(kinds("a-b"), [(Word, "a"), (Minus, "-"), (Word, "b")]);
        // never fails
        assert_eq!(
            kinds("§ & $ @ _"),
            [
                (Unknown, "§"),
                (Unknown, "&"),
                (Unknown, "$"),
                (Unknown, "@"),
                (Unknown, "_")
            ]
        );
        assert_eq!(
            kinds("'open\n'"),
            [(Unknown, "'"), (Word, "open"), (Unknown, "'")]
        );
    }

    #[test]
    fn prefix_labels() {
        for ok in ["a", "ex", "a.b", "a-1", "é", "foaf"] {
            assert!(is_pn_prefix(ok), "{ok}");
        }
        for bad in ["", "1a", "_a", "a.", ".a", "a:b", "a b", "-a"] {
            assert!(!is_pn_prefix(bad), "{bad}");
        }
    }
}
