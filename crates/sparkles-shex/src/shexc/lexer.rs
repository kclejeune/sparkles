//! The ShExC lexer: lossless (whitespace and `#` and `/* */` comments are tokens, so the
//! token texts give back the input), with IRIs, prefixed names, blank-node labels,
//! language tags, the four string forms, numbers, regular expressions (`/…/flags`),
//! semantic-action code (`%iri{…%}`), repeat ranges (`{m,n}`) and punctuation.
//! Terminals match longest first: `@ex:a` is one token, `ex:a.` is `ex:a` and `.`.
//!
//! The lexer never fails: a character no terminal starts with is an
//! [`TokenKind::Unknown`] token, and the parser reports it. Escapes are recognized but
//! not checked here (a string with `\z` is still a string); the parser checks them when
//! it decodes the token. Keywords are plain [`TokenKind::Word`]s, matched by the parser
//! without regard to case.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // trivia
    /// spaces, tabs, form feeds, CR and LF
    Whitespace,
    /// `#` to the end of the line (without the line break), or `/* … */`
    Comment,
    // terms
    /// `<…>`
    IriRef,
    /// `ex:` (also the empty prefix `:`)
    PnameNs,
    /// `ex:local`
    PnameLn,
    /// `@ex:`: a shape reference
    AtPnameNs,
    /// `@ex:local`: a shape reference
    AtPnameLn,
    /// `_:label`
    BlankNodeLabel,
    /// `@en`, `@en-GB`
    LangTag,
    /// `[+-]?[0-9]+`
    Integer,
    /// `[+-]?[0-9]*.[0-9]+`
    Decimal,
    /// a number with an exponent
    Double,
    /// `'…'`
    String1,
    /// `"…"`
    String2,
    /// `'''…'''`
    StringLong1,
    /// `"""…"""`
    StringLong2,
    /// `/…/flags`
    Regexp,
    /// `%iri{ … %}` or `%iri%`: a whole semantic action
    Code,
    /// `{m}`, `{m,}`, `{m,n}`, `{m,*}`
    RepeatRange,
    /// a keyword, `a`, `true` or `false`, or another run of letters and digits
    Word,
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
    Star,
    Plus,
    Minus,
    Question,
    /// `^`
    Hat,
    /// `^^`
    HatHat,
    Tilde,
    /// `&`
    Amp,
    /// `$`
    Dollar,
    /// `@` not followed by a prefixed name or a language tag
    At,
    /// `=`
    Eq,
    /// `//`: an annotation
    SlashSlash,
    /// one character no terminal starts with
    Unknown,
    /// the end of the input (empty)
    Eof,
}

impl TokenKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::Whitespace | TokenKind::Comment)
    }

    /// The numeric literal kinds.
    pub fn is_number(self) -> bool {
        matches!(
            self,
            TokenKind::Integer | TokenKind::Decimal | TokenKind::Double
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

/// Tokenize `src` (at most `u32::MAX` bytes). A leading BOM belongs to no token; the
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

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c')
}

/// The kind and byte length of the token at the start of `rest` (not empty).
fn next(rest: &str) -> (TokenKind, usize) {
    use TokenKind::*;
    let b = rest.as_bytes();
    let starts = |p: &str| rest.starts_with(p);
    match b[0] {
        c if is_space(c) => (
            Whitespace,
            b.iter().position(|&c| !is_space(c)).unwrap_or(b.len()),
        ),
        b'#' => (
            Comment,
            b.iter()
                .position(|&c| c == b'\n' || c == b'\r')
                .unwrap_or(b.len()),
        ),
        b'/' if starts("/*") => match rest[2..].find("*/") {
            Some(i) => (Comment, i + 4),
            None => (Unknown, 1),
        },
        b'/' if starts("//") => (SlashSlash, 2),
        b'/' => regexp(rest).map_or((Unknown, 1), |n| (Regexp, n)),
        b'<' => iriref(rest).map_or((Unknown, 1), |n| (IriRef, n)),
        b'{' => repeat_range(b).map_or((LBrace, 1), |n| (RepeatRange, n)),
        b'}' => (RBrace, 1),
        b'(' => (LParen, 1),
        b')' => (RParen, 1),
        b'[' => (LBracket, 1),
        b']' => (RBracket, 1),
        b',' => (Comma, 1),
        b';' => (Semicolon, 1),
        b'|' => (Pipe, 1),
        b'*' => (Star, 1),
        b'?' => (Question, 1),
        b'~' => (Tilde, 1),
        b'&' => (Amp, 1),
        b'$' => (Dollar, 1),
        b'=' => (Eq, 1),
        b'^' if starts("^^") => (HatHat, 2),
        b'^' => (Hat, 1),
        b'%' => code(rest).map_or((Unknown, 1), |n| (Code, n)),
        b'0'..=b'9' => number(b).expect("a digit starts a number"),
        b'.' => number(b).unwrap_or((Dot, 1)),
        b'+' | b'-' => match number(&b[1..]) {
            Some((kind, n)) => (kind, n + 1),
            None if b[0] == b'+' => (Plus, 1),
            None => (Minus, 1),
        },
        b'@' => at(rest),
        b'"' | b'\'' => string(rest).unwrap_or((Unknown, 1)),
        b'_' if b.get(1) == Some(&b':') => match blank_node_label(&rest[2..]) {
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

/// `<` … `>` as the grammar defines `IRIREF` (no `<>"{}|^`\` and nothing up to U+0020),
/// with `\u`/`\U` escapes.
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
pub(crate) fn uchar(b: &[u8]) -> Option<usize> {
    let n = match b.get(1)? {
        b'u' => 4,
        b'U' => 8,
        _ => return None,
    };
    let hex = b.get(2..2 + n)?;
    hex.iter().all(u8::is_ascii_hexdigit).then_some(n + 2)
}

/// `REGEXP`: `/` (`[^/\\\n\r]` | `\` one of `nrt\|.?*+(){}$-[]^/` | `UCHAR`)+ `/`
/// `[smix]*`.
fn regexp(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let mut i = 1;
    loop {
        match *b.get(i)? {
            b'/' if i > 1 => break,
            b'/' | b'\n' | b'\r' => return None,
            b'\\' => match b.get(i + 1)? {
                b'n' | b'r' | b't' | b'\\' | b'|' | b'.' | b'?' | b'*' | b'+' | b'(' | b')'
                | b'{' | b'}' | b'$' | b'-' | b'[' | b']' | b'^' | b'/' => i += 2,
                _ => i += uchar(&b[i..])?,
            },
            _ => i += 1,
        }
    }
    i += 1;
    i += b[i..]
        .iter()
        .take_while(|c| matches!(c, b's' | b'm' | b'i' | b'x'))
        .count();
    Some(i)
}

/// `{` INTEGER (`,` (INTEGER | `*`)?)? `}`, without spaces.
fn repeat_range(b: &[u8]) -> Option<usize> {
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut i = 1;
    match digits(i) {
        0 => return None,
        n => i += n,
    }
    if b.get(i) == Some(&b',') {
        i += 1;
        if b.get(i) == Some(&b'*') {
            i += 1;
        } else {
            i += digits(i);
        }
    }
    (b.get(i) == Some(&b'}')).then_some(i + 1)
}

/// A semantic action: `%`, an IRI or prefixed name, then `{` code `%}` or `%`, with
/// optional whitespace between the parts. Code is `[^%\\]`, `\%`, `\\` or `UCHAR`; other
/// backslash pairs are kept too.
fn code(rest: &str) -> Option<usize> {
    let ws = |i: usize| {
        rest.as_bytes()[i..]
            .iter()
            .take_while(|&&c| is_space(c))
            .count()
    };
    let mut i = 1 + ws(1);
    if i >= rest.len() {
        return None;
    }
    let name = match next(&rest[i..]) {
        (TokenKind::IriRef | TokenKind::PnameNs | TokenKind::PnameLn, n) => n,
        _ => return None,
    };
    i += name;
    i += ws(i);
    let b = rest.as_bytes();
    match b.get(i)? {
        b'%' => Some(i + 1),
        b'{' => {
            i += 1;
            loop {
                match *b.get(i)? {
                    b'%' if b.get(i + 1) == Some(&b'}') => return Some(i + 2),
                    b'\\' => {
                        // one escaped character (UTF-8 continuation bytes follow as
                        // plain characters)
                        b.get(i + 1)?;
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
        }
        _ => None,
    }
}

/// `INTEGER`, `DECIMAL` or `DOUBLE` (unsigned) at the start of `b`, longest match.
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

/// After `@`: a prefixed name (a shape reference) or a language tag, whichever is
/// longer; `@` alone otherwise.
fn at(rest: &str) -> (TokenKind, usize) {
    let after = &rest[1..];
    let pname_len = match after.chars().next() {
        Some(':') => Some(pname(after, 0)),
        Some(c) if is_pn_chars_base(c) => {
            let prefix = pn_prefix(after);
            after[prefix..]
                .starts_with(':')
                .then(|| pname(after, prefix))
        }
        _ => None,
    };
    let lang = langtag(rest.as_bytes());
    match pname_len {
        Some((kind, n)) if n + 1 >= lang => (
            if kind == TokenKind::PnameNs {
                TokenKind::AtPnameNs
            } else {
                TokenKind::AtPnameLn
            },
            n + 1,
        ),
        _ if lang > 0 => (TokenKind::LangTag, lang),
        _ => (TokenKind::At, 1),
    }
}

/// `LANGTAG`: `@` letters, then `-` alphanumeric subtags (0: none).
fn langtag(b: &[u8]) -> usize {
    let run = |from: usize, ok: fn(&u8) -> bool| {
        b.get(from..)
            .map_or(0, |s| s.iter().take_while(|c| ok(c)).count())
    };
    let mut i = 1 + run(1, u8::is_ascii_alphabetic);
    if i == 1 {
        return 0;
    }
    while b.get(i) == Some(&b'-') {
        match run(i + 1, u8::is_ascii_alphanumeric) {
            0 => break,
            n => i += 1 + n,
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
    None
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
            Some(b'\\') if b.get(i + 1).is_some_and(|c| is_local_escape(*c)) => 2,
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

/// The characters `PN_LOCAL_ESC` may escape.
pub(crate) fn is_local_escape(c: u8) -> bool {
    b"_~.-!$&'()*+,;=/?#@%".contains(&c)
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

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        let toks = lex(src);
        let joined: String = toks.iter().map(|t| t.text(src)).collect();
        assert_eq!(joined, src.trim_start_matches('\u{feff}'), "lossless");
        toks.iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != Eof)
            .map(|t| (t.kind, t.text(src)))
            .collect()
    }

    #[test]
    fn terms() {
        assert_eq!(
            kinds("<http://a/b#c> <\\u00e9> ex: ex:a :b ex:a. _:b1 _:a.b."),
            [
                (IriRef, "<http://a/b#c>"),
                (IriRef, "<\\u00e9>"),
                (PnameNs, "ex:"),
                (PnameLn, "ex:a"),
                (PnameLn, ":b"),
                (PnameLn, "ex:a"),
                (Dot, "."),
                (BlankNodeLabel, "_:b1"),
                (BlankNodeLabel, "_:a.b"),
                (Dot, "."),
            ]
        );
        assert_eq!(
            kinds("ex:p%41\\.x ex:p%1 ex:p\\u0031"),
            [
                (PnameLn, "ex:p%41\\.x"),
                (PnameLn, "ex:p"),
                (Unknown, "%"),
                (Integer, "1"),
                (PnameLn, "ex:p"),
                (Unknown, "\\"),
                (Word, "u0031"),
            ]
        );
        assert_eq!(
            kinds("<a b> <a\\n>"),
            [
                (Unknown, "<"),
                (Word, "a"),
                (Word, "b"),
                (Unknown, ">"),
                (Unknown, "<"),
                (Word, "a"),
                (Unknown, "\\"),
                (Word, "n"),
                (Unknown, ">"),
            ]
        );
    }

    #[test]
    fn at_forms() {
        assert_eq!(
            kinds("@ex:S @ex: @:S @en @en-GB @en:x @<S> @_:b @~ @start"),
            [
                (AtPnameLn, "@ex:S"),
                (AtPnameNs, "@ex:"),
                (AtPnameLn, "@:S"),
                (LangTag, "@en"),
                (LangTag, "@en-GB"),
                (AtPnameLn, "@en:x"),
                (At, "@"),
                (IriRef, "<S>"),
                (At, "@"),
                (BlankNodeLabel, "_:b"),
                (At, "@"),
                (Tilde, "~"),
                (LangTag, "@start"),
            ]
        );
        assert_eq!(
            kinds("\"x\"@en~ \"y\"@1"),
            [
                (String2, "\"x\""),
                (LangTag, "@en"),
                (Tilde, "~"),
                (String2, "\"y\""),
                (At, "@"),
                (Integer, "1"),
            ]
        );
    }

    #[test]
    fn numbers_and_cardinalities() {
        assert_eq!(
            kinds("1 -1 +1.5 .5 1e3 -1.5E-2 123abc +-1"),
            [
                (Integer, "1"),
                (Integer, "-1"),
                (Decimal, "+1.5"),
                (Decimal, ".5"),
                (Double, "1e3"),
                (Double, "-1.5E-2"),
                (Integer, "123"),
                (Word, "abc"),
                (Plus, "+"),
                (Integer, "-1"),
            ]
        );
        assert_eq!(
            kinds(". * + ? {2} {2,} {2,3} {2,*} { 2 } {a"),
            [
                (Dot, "."),
                (Star, "*"),
                (Plus, "+"),
                (Question, "?"),
                (RepeatRange, "{2}"),
                (RepeatRange, "{2,}"),
                (RepeatRange, "{2,3}"),
                (RepeatRange, "{2,*}"),
                (LBrace, "{"),
                (Integer, "2"),
                (RBrace, "}"),
                (LBrace, "{"),
                (Word, "a"),
            ]
        );
        assert_eq!(kinds(".{2,}"), [(Dot, "."), (RepeatRange, "{2,}")]);
    }

    #[test]
    fn strings() {
        assert_eq!(
            kinds("'a\\'b' \"\" '''x''y''' \"\"\"a\n\"b\"\"\"\" 'open\n'"),
            [
                (String1, "'a\\'b'"),
                (String2, "\"\""),
                (StringLong1, "'''x''y'''"),
                (StringLong2, "\"\"\"a\n\"b\"\"\""),
                (Unknown, "\""),
                (Unknown, "'"),
                (Word, "open"),
                (Unknown, "'"),
            ]
        );
    }

    #[test]
    fn regexps_comments_annotations() {
        assert_eq!(
            kinds("/^a\\/b$/i // ex:p /* c\n */ # d\n"),
            [
                (Regexp, "/^a\\/b$/i"),
                (SlashSlash, "//"),
                (PnameLn, "ex:p"),
            ]
        );
        assert_eq!(
            kinds("/\\1/"),
            [
                (Unknown, "/"),
                (Unknown, "\\"),
                (Integer, "1"),
                (Unknown, "/")
            ]
        );
        assert_eq!(
            kinds("/x\n/"),
            [(Unknown, "/"), (Word, "x"), (Unknown, "/")]
        );
        assert_eq!(
            kinds("/\\u0061\\\\\\./smix"),
            [(Regexp, "/\\u0061\\\\\\./smix")]
        );
        let src = "a /* c */ b # d\n";
        let toks = lex(src);
        let comments: Vec<_> = toks
            .iter()
            .filter(|t| t.kind == Comment)
            .map(|t| t.text(src))
            .collect();
        assert_eq!(comments, ["/* c */", "# d"]);
    }

    #[test]
    fn semantic_actions() {
        assert_eq!(
            kinds("%<http://e/>{ x \\%} y %} % ex:t % %ex:t{%} %{ x %}"),
            [
                (Code, "%<http://e/>{ x \\%} y %}"),
                (Code, "% ex:t %"),
                (Code, "%ex:t{%}"),
                (Unknown, "%"),
                (LBrace, "{"),
                (Word, "x"),
                (Unknown, "%"),
                (RBrace, "}"),
            ]
        );
    }

    #[test]
    fn punctuation_and_unknown() {
        assert_eq!(
            kinds("\u{feff}start = ^^ ^ ~ & $ | ; , ( ) [ ] { } ! §"),
            [
                (Word, "start"),
                (Eq, "="),
                (HatHat, "^^"),
                (Hat, "^"),
                (Tilde, "~"),
                (Amp, "&"),
                (Dollar, "$"),
                (Pipe, "|"),
                (Semicolon, ";"),
                (Comma, ","),
                (LParen, "("),
                (RParen, ")"),
                (LBracket, "["),
                (RBracket, "]"),
                (LBrace, "{"),
                (RBrace, "}"),
                (Unknown, "!"),
                (Unknown, "§"),
            ]
        );
        assert_eq!(lex("").len(), 1);
        assert_eq!(
            kinds("/* open"),
            [(Unknown, "/"), (Star, "*"), (Word, "open")]
        );
    }
}
