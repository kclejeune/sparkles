//! The tokens of the Manchester syntax, with their byte spans.

use super::{Error, Span};

/// A name as written: a full IRI, a bare word (a prefixed name, a simple name or a
/// label without spaces) or a quoted name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Name {
    Iri(String),
    Word(String),
    Quoted(String),
}

impl Name {
    /// The name as the text wrote it, for messages.
    pub fn shown(&self) -> String {
        match self {
            Name::Iri(i) => format!("<{i}>"),
            Name::Word(w) => w.clone(),
            Name::Quoted(q) => format!("'{q}'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    /// `<=`, `<`, `>=` or `>`
    Op(&'static str),
    Name(Name),
    /// a quoted literal with its language tag or its datatype
    Str {
        value: String,
        lang: Option<String>,
        datatype: Option<(Name, Span)>,
    },
    /// an integer, decimal or floating-point literal, as written
    Number(String),
}

impl Tok {
    /// The token as the text wrote it, for messages.
    pub fn shown(&self) -> String {
        match self {
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::LBrace => "{".into(),
            Tok::RBrace => "}".into(),
            Tok::LBracket => "[".into(),
            Tok::RBracket => "]".into(),
            Tok::Comma => ",".into(),
            Tok::Op(o) => (*o).into(),
            Tok::Name(n) => n.shown(),
            Tok::Str { value, .. } => format!("{value:?}"),
            Tok::Number(n) => n.clone(),
        }
    }

    /// The bare word, when the token is one.
    pub fn word(&self) -> Option<&str> {
        match self {
            Tok::Name(Name::Word(w)) => Some(w),
            _ => None,
        }
    }
}

/// The characters that end a bare word.
pub fn ends_word(c: char) -> bool {
    c.is_whitespace() || "()[]{},'\"<>".contains(c)
}

pub fn tokens(text: &str) -> Result<Vec<(Tok, Span)>, Error> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    let at = |i: usize| text[i..].chars().next();
    while let Some(c) = at(i) {
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let single = match c {
            '(' => Some(Tok::LParen),
            ')' => Some(Tok::RParen),
            '{' => Some(Tok::LBrace),
            '}' => Some(Tok::RBrace),
            '[' => Some(Tok::LBracket),
            ']' => Some(Tok::RBracket),
            ',' => Some(Tok::Comma),
            _ => None,
        };
        if let Some(t) = single {
            i += 1;
            out.push((t, Span::new(start, i)));
            continue;
        }
        match c {
            '<' | '>' => {
                // `<` starts a full IRI when a scheme's letter follows and the IRI closes
                // before any space; otherwise it is a facet operator
                if c == '<'
                    && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic)
                    && let Some(end) = text[i + 1..].find(|c: char| c == '>' || c.is_whitespace())
                    && b.get(i + 1 + end) == Some(&b'>')
                {
                    let iri = &text[i + 1..i + 1 + end];
                    i += end + 2;
                    out.push((Tok::Name(Name::Iri(iri.to_string())), Span::new(start, i)));
                    continue;
                }
                let eq = b.get(i + 1) == Some(&b'=');
                let op = match (c, eq) {
                    ('<', true) => "<=",
                    ('<', false) => "<",
                    ('>', true) => ">=",
                    _ => ">",
                };
                i += if eq { 2 } else { 1 };
                out.push((Tok::Op(op), Span::new(start, i)));
            }
            '\'' => {
                let (s, end) = quoted(text, i, '\'')?;
                i = end;
                out.push((Tok::Name(Name::Quoted(s)), Span::new(start, i)));
            }
            '"' => {
                let (value, end) = quoted(text, i, '"')?;
                i = end;
                let mut lang = None;
                let mut datatype = None;
                if b.get(i) == Some(&b'@') {
                    let n = text[i + 1..]
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                        .unwrap_or(text.len() - i - 1);
                    if n == 0 {
                        return Err(Error::syntax(
                            Span::new(i, i + 1),
                            "a language tag follows '@'",
                        ));
                    }
                    lang = Some(text[i + 1..i + 1 + n].to_string());
                    i += 1 + n;
                } else if text[i..].starts_with("^^") {
                    let ds = i + 2;
                    let (name, end) = match at(ds) {
                        Some('<') => match text[ds + 1..].find('>') {
                            Some(n) => {
                                (Name::Iri(text[ds + 1..ds + 1 + n].to_string()), ds + n + 2)
                            }
                            None => {
                                return Err(Error::syntax(
                                    Span::new(ds, text.len()),
                                    "the IRI has no closing '>'",
                                ));
                            }
                        },
                        Some('\'') => {
                            let (s, end) = quoted(text, ds, '\'')?;
                            (Name::Quoted(s), end)
                        }
                        Some(c) if !ends_word(c) => {
                            let n = text[ds..].find(ends_word).unwrap_or(text.len() - ds);
                            (Name::Word(text[ds..ds + n].to_string()), ds + n)
                        }
                        _ => {
                            return Err(Error::syntax(
                                Span::new(i, (ds + 1).min(text.len())),
                                "a datatype follows '^^'",
                            ));
                        }
                    };
                    datatype = Some((name, Span::new(ds, end)));
                    i = end;
                }
                out.push((
                    Tok::Str {
                        value,
                        lang,
                        datatype,
                    },
                    Span::new(start, i),
                ));
            }
            _ => {
                let n = text[i..].find(ends_word).unwrap_or(text.len() - i);
                let w = &text[i..i + n];
                i += n;
                let t = if number_like(w) {
                    Tok::Number(w.to_string())
                } else {
                    Tok::Name(Name::Word(w.to_string()))
                };
                out.push((t, Span::new(start, i)));
            }
        }
    }
    Ok(out)
}

/// Whether a word is written as a number: it starts with a digit, or with a sign or a
/// point and a digit.
pub fn number_like(w: &str) -> bool {
    let mut cs = w.chars();
    match cs.next() {
        Some(c) if c.is_ascii_digit() => true,
        Some('+' | '-' | '.') => matches!(cs.next(), Some(c) if c.is_ascii_digit() || c == '.'),
        _ => false,
    }
}

/// A quoted string from `start`, which holds the quote, with its escapes read: `\\`,
/// the quote itself, and the JSON escapes `\n`, `\t`, `\r`, `\b`, `\f`, `\/` and
/// `\uXXXX`. The answer is the string and the byte after the closing quote.
fn quoted(text: &str, start: usize, q: char) -> Result<(String, usize), Error> {
    let mut s = String::new();
    let mut it = text[start + 1..].char_indices();
    while let Some((k, c)) = it.next() {
        let here = start + 1 + k;
        if c == q {
            return Ok((s, here + 1));
        }
        if c != '\\' {
            s.push(c);
            continue;
        }
        let Some((_, e)) = it.next() else { break };
        match e {
            'n' => s.push('\n'),
            't' => s.push('\t'),
            'r' => s.push('\r'),
            'b' => s.push('\u{8}'),
            'f' => s.push('\u{c}'),
            'u' => {
                let hex: String = it.by_ref().take(4).map(|(_, c)| c).collect();
                let mut code = u32::from_str_radix(&hex, 16).ok();
                // a surrogate pair, as JSON writes characters outside the BMP
                if let Some(hi) = code
                    && (0xD800..0xDC00).contains(&hi)
                {
                    let rest = &text[start + 1..];
                    let pos = it.clone().next().map_or(rest.len(), |(k, _)| k);
                    if rest[pos..].starts_with("\\u") {
                        it.next();
                        it.next();
                        let lo: String = it.by_ref().take(4).map(|(_, c)| c).collect();
                        code = u32::from_str_radix(&lo, 16)
                            .ok()
                            .filter(|lo| (0xDC00..0xE000).contains(lo))
                            .map(|lo| 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                    } else {
                        code = None;
                    }
                }
                match code.and_then(char::from_u32) {
                    Some(c) => s.push(c),
                    None => {
                        return Err(Error::syntax(
                            Span::new(here, here + 2 + hex.len()),
                            "the \\u escape names no character",
                        ));
                    }
                }
            }
            e if e == q || e == '\\' || e == '/' => s.push(e),
            e => {
                return Err(Error::syntax(
                    Span::new(here, here + 1 + e.len_utf8()),
                    format!("unknown escape '\\{e}'"),
                ));
            }
        }
    }
    Err(Error::syntax(
        Span::new(start, text.len()),
        format!(
            "the quoted {} has no closing {q}",
            if q == '"' { "literal" } else { "name" }
        ),
    ))
}
