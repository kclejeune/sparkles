//! Composite datatype literals, `cdt:List` and `cdt:Map`, as Jena 5's ARQ has them (the
//! SPARQL CDTs proposal from Amazon Neptune): the lexical forms, equality, the `<` order
//! and the ORDER BY order, and the `cdt:` function library.
//!
//! A literal keeps its lexical form in the store, and the value is parsed from it when an
//! operation needs it. Lists and maps that functions build are written in a canonical
//! form: elements separated by `", "`, terms written as Turtle writes them, and the
//! entries of a map in key order. Jena writes a map's entries in hash order, so its
//! lexical forms can differ from Sparkles' while the values are equal.

use super::ctx::Ctx;
use super::expr::{Expr, Row, Val, eval};
use super::value::{EvalResult, Num, TypeError, Value, compare, equals, order_cmp};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use std::cmp::Ordering;

/// The namespace of the datatypes and of the function library.
pub const NS: &str = "http://w3id.org/awslabs/neptune/SPARQL-CDTs/";
/// The `cdt:List` datatype.
pub const LIST: &str = "http://w3id.org/awslabs/neptune/SPARQL-CDTs/List";
/// The `cdt:Map` datatype.
pub const MAP: &str = "http://w3id.org/awslabs/neptune/SPARQL-CDTs/Map";

/// The functions of the `cdt:` library, by local name.
pub const FUNCTIONS: [&str; 16] = [
    "concat",
    "contains",
    "containsKey",
    "containsTerm",
    "get",
    "head",
    "keys",
    "List",
    "Map",
    "merge",
    "put",
    "remove",
    "reverse",
    "size",
    "subseq",
    "tail",
];

/// An element of a list or a value of a map: an RDF term or `null`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Elem {
    Null,
    Term(Term),
}

/// A map's entries, in key order ([`key_cmp`]), with distinct keys.
pub type Map = Vec<(Term, Elem)>;

/// Whether `dt` is one of the two composite datatypes.
pub fn is_cdt(dt: &str) -> bool {
    dt == LIST || dt == MAP
}

// ------------------------------------------------------------------ lexical forms ----

/// The elements of a `cdt:List` lexical form, or `None` when it is ill-formed.
pub fn parse_list(lex: &str) -> Option<Vec<Elem>> {
    let mut p = Parser { s: lex, i: 0 };
    p.ws();
    let l = p.list()?;
    p.ws();
    p.at_end().then_some(l)
}

/// The entries of a `cdt:Map` lexical form in key order, or `None` when it is
/// ill-formed (a repeated key included).
pub fn parse_map(lex: &str) -> Option<Map> {
    let mut p = Parser { s: lex, i: 0 };
    p.ws();
    let m = p.map()?;
    p.ws();
    p.at_end().then_some(m)
}

/// The canonical lexical form of a list.
pub fn list_lexical(l: &[Elem]) -> String {
    let mut s = String::from("[");
    for (i, e) in l.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        elem_ttl(e, &mut s);
    }
    s.push(']');
    s
}

/// The canonical lexical form of a map (entries in key order).
pub fn map_lexical(m: &[(Term, Elem)]) -> String {
    let mut s = String::from("{");
    for (i, (k, v)) in m.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        term_ttl(k, &mut s);
        s.push_str(" : ");
        elem_ttl(v, &mut s);
    }
    s.push('}');
    s
}

/// A list as a `cdt:List` literal value.
pub fn list_value(l: &[Elem]) -> Value {
    Value::Other {
        lex: list_lexical(l).into(),
        dt: LIST.into(),
    }
}

/// A map as a `cdt:Map` literal value.
pub fn map_value(m: &[(Term, Elem)]) -> Value {
    Value::Other {
        lex: map_lexical(m).into(),
        dt: MAP.into(),
    }
}

fn elem_ttl(e: &Elem, out: &mut String) {
    match e {
        Elem::Null => out.push_str("null"),
        Elem::Term(t) => term_ttl(t, out),
    }
}

/// A term as Turtle writes it without prefixes (Jena's `NodeFmtLib.strTTL`): numbers
/// and booleans in their short form where the lexical form allows it, and composite
/// literals as their bare lexical form.
pub fn term_ttl(t: &Term, out: &mut String) {
    match t {
        Term::NamedNode(n) => {
            out.push('<');
            out.push_str(n.as_str());
            out.push('>');
        }
        Term::BlankNode(b) => {
            out.push_str("_:");
            out.push_str(b.as_str());
        }
        Term::Literal(l) => {
            let dt = l.datatype();
            let lex = l.value();
            if is_cdt(dt.as_str()) {
                out.push_str(lex);
                return;
            }
            let bare = (dt == xsd::INTEGER && turtle_integer(lex))
                || (dt == xsd::DECIMAL && turtle_decimal(lex))
                || (dt == xsd::DOUBLE && turtle_double(lex))
                || (dt == xsd::BOOLEAN && (lex == "true" || lex == "false"));
            if bare {
                out.push_str(lex);
                return;
            }
            quote(lex, out);
            if let Some(lang) = l.language() {
                out.push('@');
                out.push_str(lang);
                if let Some(d) = l.direction() {
                    out.push_str(match d {
                        oxrdf::BaseDirection::Ltr => "--ltr",
                        oxrdf::BaseDirection::Rtl => "--rtl",
                    });
                }
            } else if dt != xsd::STRING {
                out.push_str("^^<");
                out.push_str(dt.as_str());
                out.push('>');
            }
        }
        // a triple term cannot be written in a composite literal; it is not an element
        Term::Triple(t) => {
            out.push_str("<<( ");
            term_ttl(&t.subject.clone().into(), out);
            out.push(' ');
            term_ttl(&Term::NamedNode(t.predicate.clone()), out);
            out.push(' ');
            term_ttl(&t.object, out);
            out.push_str(" )>>");
        }
    }
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn unsign(s: &str) -> &str {
    s.strip_prefix(['+', '-']).unwrap_or(s)
}

fn turtle_integer(s: &str) -> bool {
    digits(unsign(s))
}

fn turtle_decimal(s: &str) -> bool {
    match unsign(s).split_once('.') {
        Some((a, b)) => (a.is_empty() || digits(a)) && digits(b),
        None => false,
    }
}

fn turtle_double(s: &str) -> bool {
    let s = unsign(s);
    let Some(e) = s.find(['e', 'E']) else {
        return false;
    };
    let (m, x) = (&s[..e], unsign(&s[e + 1..]));
    let mantissa = match m.split_once('.') {
        Some((a, b)) => (digits(a) && (b.is_empty() || digits(b))) || (a.is_empty() && digits(b)),
        None => digits(m),
    };
    mantissa && digits(x)
}

/// A parser for the lexical forms (Jena's `cdt_literals.jj`).
struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }
    fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn ws(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                self.i += 1;
            } else {
                break;
            }
        }
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn list(&mut self) -> Option<Vec<Elem>> {
        if !self.eat('[') {
            return None;
        }
        let mut out = Vec::new();
        self.ws();
        if self.eat(']') {
            return Some(out);
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            if self.eat(']') {
                return Some(out);
            }
            if !self.eat(',') {
                return None;
            }
        }
    }

    fn map(&mut self) -> Option<Map> {
        if !self.eat('{') {
            return None;
        }
        let mut out: Map = Vec::new();
        self.ws();
        if self.eat('}') {
            return Some(out);
        }
        loop {
            self.ws();
            let key = match self.term()? {
                Elem::Term(t @ (Term::NamedNode(_) | Term::Literal(_))) => t,
                _ => return None,
            };
            self.ws();
            if !self.eat(':') {
                return None;
            }
            self.ws();
            let value = self.value()?;
            // a repeated key makes the literal ill-formed
            match out.binary_search_by(|(k, _)| key_cmp(k, &key)) {
                Ok(_) => return None,
                Err(at) => out.insert(at, (key, value)),
            }
            self.ws();
            if self.eat('}') {
                return Some(out);
            }
            if !self.eat(',') {
                return None;
            }
        }
    }

    /// A list element or map value: a term, `null`, or a nested list or map.
    fn value(&mut self) -> Option<Elem> {
        match self.peek()? {
            '[' => {
                let start = self.i;
                self.list()?;
                Some(Elem::Term(literal(&self.s[start..self.i], LIST)))
            }
            '{' => {
                let start = self.i;
                self.map()?;
                Some(Elem::Term(literal(&self.s[start..self.i], MAP)))
            }
            _ if self.rest().starts_with("null") && !self.word_continues(4) => {
                self.i += 4;
                Some(Elem::Null)
            }
            _ => self.term(),
        }
    }

    fn word_continues(&self, n: usize) -> bool {
        self.rest()[n..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    }

    fn term(&mut self) -> Option<Elem> {
        let c = self.peek()?;
        let t = match c {
            '<' => Term::NamedNode(NamedNode::new_unchecked(self.iri()?)),
            '_' => {
                let rest = self.rest().strip_prefix("_:")?;
                let n = pn_local_len(rest);
                if n == 0 {
                    return None;
                }
                let label = &rest[..n];
                self.i += 2 + n;
                Term::BlankNode(BlankNode::new(label).ok()?)
            }
            '"' | '\'' => {
                let lex = self.string()?;
                if self.eat('@') {
                    let tag = self.langtag()?;
                    Term::Literal(Literal::new_language_tagged_literal(lex, tag).ok()?)
                } else if self.rest().starts_with("^^") {
                    self.i += 2;
                    let dt = self.iri()?;
                    Term::Literal(Literal::new_typed_literal(
                        lex,
                        NamedNode::new_unchecked(dt),
                    ))
                } else {
                    Term::Literal(Literal::new_simple_literal(lex))
                }
            }
            '+' | '-' | '.' | '0'..='9' => self.number()?,
            't' | 'T' | 'f' | 'F' => {
                let word = self.rest().get(..5).unwrap_or(self.rest());
                if word.len() >= 4 && word[..4].eq_ignore_ascii_case("true") {
                    self.i += 4;
                    Term::Literal(Literal::new_typed_literal("true", xsd::BOOLEAN))
                } else if word.len() == 5 && word.eq_ignore_ascii_case("false") {
                    self.i += 5;
                    Term::Literal(Literal::new_typed_literal("false", xsd::BOOLEAN))
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        Some(Elem::Term(t))
    }

    fn iri(&mut self) -> Option<String> {
        let rest = self.rest().strip_prefix('<')?;
        let end = rest.find('>')?;
        let iri = &rest[..end];
        if iri
            .chars()
            .any(|c| matches!(c, '<' | '"' | '{' | '}' | '^' | '\\' | '|' | '`') || c <= ' ')
        {
            return None;
        }
        self.i += end + 2;
        Some(iri.to_string())
    }

    fn langtag(&mut self) -> Option<String> {
        let rest = self.rest();
        let mut n = 0;
        let b = rest.as_bytes();
        while n < b.len() && b[n].is_ascii_alphabetic() {
            n += 1;
        }
        if n == 0 {
            return None;
        }
        while n < b.len() && b[n] == b'-' {
            let mut m = n + 1;
            while m < b.len() && b[m].is_ascii_alphanumeric() {
                m += 1;
            }
            if m == n + 1 {
                break;
            }
            n = m;
        }
        self.i += n;
        Some(rest[..n].to_string())
    }

    fn string(&mut self) -> Option<String> {
        let rest = self.rest();
        let q = rest.chars().next()?;
        let triple: String = std::iter::repeat_n(q, 3).collect();
        let (body_start, long) = if rest.starts_with(&triple) {
            (3, true)
        } else {
            (1, false)
        };
        let mut out = String::new();
        let mut chars = rest[body_start..].char_indices();
        while let Some((j, c)) = chars.next() {
            if long {
                if rest[body_start + j..].starts_with(&triple) {
                    self.i += body_start + j + 3;
                    return Some(out);
                }
            } else if c == q {
                self.i += body_start + j + 1;
                return Some(out);
            } else if c == '\n' || c == '\r' {
                return None;
            }
            if c == '\\' {
                let (_, e) = chars.next()?;
                match e {
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    'f' => out.push('\u{c}'),
                    '\\' | '"' | '\'' => out.push(e),
                    'u' | 'U' => {
                        let n = if e == 'u' { 4 } else { 8 };
                        let mut hex = String::new();
                        for _ in 0..n {
                            hex.push(chars.next()?.1);
                        }
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    _ => return None,
                }
            } else {
                out.push(c);
            }
        }
        None
    }

    fn number(&mut self) -> Option<Term> {
        let b = self.rest().as_bytes();
        let mut n = 0;
        if n < b.len() && (b[n] == b'+' || b[n] == b'-') {
            n += 1;
        }
        let int_start = n;
        while n < b.len() && b[n].is_ascii_digit() {
            n += 1;
        }
        let int_digits = n - int_start;
        let mut frac_digits = 0;
        let mut dot = false;
        if n < b.len() && b[n] == b'.' {
            let mut m = n + 1;
            while m < b.len() && b[m].is_ascii_digit() {
                m += 1;
            }
            frac_digits = m - n - 1;
            // `1.` is a decimal; a lone `.` is not a number
            if int_digits > 0 || frac_digits > 0 {
                dot = true;
                n = m;
            }
        }
        if int_digits == 0 && frac_digits == 0 {
            return None;
        }
        let mut exp = false;
        if n < b.len() && (b[n] == b'e' || b[n] == b'E') {
            let mut m = n + 1;
            if m < b.len() && (b[m] == b'+' || b[m] == b'-') {
                m += 1;
            }
            let d = m;
            while m < b.len() && b[m].is_ascii_digit() {
                m += 1;
            }
            if m > d {
                exp = true;
                n = m;
            }
        }
        let lex = &self.rest()[..n];
        let dt = if exp {
            xsd::DOUBLE
        } else if dot {
            xsd::DECIMAL
        } else {
            xsd::INTEGER
        };
        let t = Term::Literal(Literal::new_typed_literal(lex, dt));
        self.i += n;
        Some(t)
    }
}

/// The length of the `PN_LOCAL` label at the start of `s` (blank node labels).
fn pn_local_len(s: &str) -> usize {
    let chars_u = |c: char| c == '_' || c.is_alphabetic() || c.is_ascii_digit();
    let chars = |c: char| chars_u(c) || c == '-' || c == '\u{b7}' || c.is_alphanumeric();
    let mut it = s.char_indices();
    match it.next() {
        Some((_, c)) if chars_u(c) => {}
        _ => return 0,
    }
    // dots may occur inside a label but not at its end
    let mut last_ok = s.chars().next().map_or(0, char::len_utf8);
    for (j, c) in it {
        if chars(c) {
            last_ok = j + c.len_utf8();
        } else if c != '.' {
            break;
        }
    }
    last_ok
}

fn literal(lex: &str, dt: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(
        lex,
        NamedNode::new_unchecked(dt),
    ))
}

// ------------------------------------------------------------ blank node labels ----
//
// A blank node label inside a composite literal is scoped like a label outside it. A
// loader gives the labels of a literal the nodes its file's labels name, and a query
// gives the labels of a literal written in its text nodes of its own. Both write the
// literal again with each label replaced by the label of the node it names, so a query
// that reads the literal later finds that node (see `Ctx::intern_term`).

/// Maximum nesting depth of *quoted* composite literals (`"..."^^<cdt:List>`
/// nested inside another composite literal). Bare `[` / `{` nesting is validated
/// and scanned iteratively using an inline zero-heap stack fast path with
/// memory-preflighted paged overflow and a bounded sliding-window fallback.
const MAX_QUOTED_CDT_DEPTH: usize = 64;

/// Inline stack bitset of bare `[` / `{` container kinds tracked in
/// `validate_cdt` (8 KiB on the stack, 0 heap). Nesting beyond the inline
/// window spills into a memory-preflighted heap bit vector for O(1) push/pop
/// without rescans, falling back to a bounded sliding window only when a tight
/// memory cap refuses the spill.
const CONTAINER_WINDOW_BITS: usize = 1024 * 64;
const CONTAINER_WINDOW_WORDS: usize = CONTAINER_WINDOW_BITS / 64;
const CONTAINER_WINDOW_HALF_WORDS: usize = CONTAINER_WINDOW_WORDS / 2;
const CONTAINER_WINDOW_HALF_BITS: usize = CONTAINER_WINDOW_HALF_WORDS * 64;

/// Inline stack frames and fixed key pool for O(1) zero-heap duplicate-key
/// checks on ordinary maps. Deeper map nesting spills frames into a memory-
/// preflighted heap vector; wider maps promote to a memory-preflighted
/// open-addressing hash table sized proportionally to their key count. When a
/// tight memory cap refuses heap spills, validation falls back to bounded
/// zero-heap sliding windows and batched scans capped linearly in input length.
const INLINE_MAP_FRAMES: usize = 256;
const INLINE_MAP_FRAMES_HALF: usize = INLINE_MAP_FRAMES / 2;
const INLINE_MAP_KEY_POOL: usize = 512;
const INLINE_MAP_KEY_BUCKETS: usize = 1024;
const ZERO_HEAP_FALLBACK_STEP_FACTOR: u64 = 8;
const MIN_ZERO_HEAP_FALLBACK_STEPS: u64 = 262_144;

/// The 26 RFC 5646 grandfathered tags accepted by `oxilangtag` 0.1.6
/// (maximum length 11 bytes: `"cel-gaulish"`).
const GRANDFATHERED_LANGTAGS: [&str; 26] = [
    "art-lojban",
    "cel-gaulish",
    "en-GB-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "no-bok",
    "no-nyn",
    "sgn-BE-FR",
    "sgn-BE-NL",
    "sgn-CH-DE",
    "zh-guoyu",
    "zh-hakka",
    "zh-min",
    "zh-min-nan",
    "zh-xiang",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CdtKind {
    List,
    Map,
}

#[derive(Clone, Copy)]
enum KeySuffix {
    Plain,
    Typed { start: usize, end: usize },
    Lang { start: usize, end: usize },
}

#[derive(Clone, Copy)]
enum KeyKind {
    Iri {
        start: usize,
        end: usize,
    },
    String {
        body_start: usize,
        body_end: usize,
        suffix: KeySuffix,
    },
    Number {
        start: usize,
        end: usize,
        dt: &'static str,
    },
    Bool(bool),
}

#[derive(Clone, Copy)]
enum CharSource {
    Static { s: &'static str, pos: usize },
    Raw { pos: usize, end: usize },
    Escaped { pos: usize, end: usize },
    Lower { pos: usize, end: usize },
}

impl CharSource {
    fn next(&mut self, stream: &mut CdtStream<'_>) -> Option<char> {
        match self {
            Self::Static { s, pos } => {
                let c = s[*pos..].chars().next()?;
                *pos += c.len_utf8();
                Some(c)
            }
            Self::Raw { pos, end } => {
                if *pos >= *end {
                    return None;
                }
                stream.pos = *pos;
                let c = stream.next_char()?;
                *pos = stream.pos;
                Some(c)
            }
            Self::Escaped { pos, end } => {
                if *pos >= *end {
                    return None;
                }
                stream.pos = *pos;
                let c = stream.next_char()?;
                let c = if c == '\\' {
                    stream.next_escape(stream.depth)?
                } else {
                    c
                };
                *pos = stream.pos;
                Some(c)
            }
            Self::Lower { pos, end } => {
                if *pos >= *end {
                    return None;
                }
                stream.pos = *pos;
                let c = stream.next_char()?;
                *pos = stream.pos;
                Some(c.to_ascii_lowercase())
            }
        }
    }
}

fn fnv1a_byte(h: u64, b: u8) -> u64 {
    (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
}

fn fnv1a_char(mut h: u64, c: char) -> u64 {
    let mut buf = [0u8; 4];
    for &b in c.encode_utf8(&mut buf).as_bytes() {
        h = fnv1a_byte(h, b);
    }
    h
}

#[inline]
fn mix_tag(mut h: u64) -> u32 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    (h ^ (h >> 32)) as u32
}

fn hash_source(
    stream: &mut CdtStream<'_>,
    mut src: CharSource,
    mut h: u64,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
) -> crate::error::Result<u64> {
    while let Some(c) = src.next(stream) {
        stream.tick(check)?;
        h = fnv1a_char(h, c);
    }
    Ok(h)
}

fn sources_eq(
    stream: &mut CdtStream<'_>,
    mut a: CharSource,
    mut b: CharSource,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
) -> crate::error::Result<bool> {
    loop {
        stream.tick(check)?;
        match (a.next(stream), b.next(stream)) {
            (Some(x), Some(y)) if x == y => {}
            (None, None) => return Ok(true),
            _ => return Ok(false),
        }
    }
}

fn literal_key_parts(key: KeyKind) -> Option<(CharSource, Option<CharSource>, CharSource)> {
    match key {
        KeyKind::Iri { .. } => None,
        KeyKind::String {
            body_start,
            body_end,
            suffix,
        } => {
            let (dt, lang) = match suffix {
                KeySuffix::Plain => (
                    CharSource::Static {
                        s: xsd::STRING.as_str(),
                        pos: 0,
                    },
                    None,
                ),
                KeySuffix::Typed { start, end } => (CharSource::Raw { pos: start, end }, None),
                KeySuffix::Lang { start, end } => (
                    CharSource::Static {
                        s: oxrdf::vocab::rdf::LANG_STRING.as_str(),
                        pos: 0,
                    },
                    Some(CharSource::Lower { pos: start, end }),
                ),
            };
            Some((
                dt,
                lang,
                CharSource::Escaped {
                    pos: body_start,
                    end: body_end,
                },
            ))
        }
        KeyKind::Number { start, end, dt } => Some((
            CharSource::Static { s: dt, pos: 0 },
            None,
            CharSource::Raw { pos: start, end },
        )),
        KeyKind::Bool(b) => Some((
            CharSource::Static {
                s: xsd::BOOLEAN.as_str(),
                pos: 0,
            },
            None,
            CharSource::Static {
                s: if b { "true" } else { "false" },
                pos: 0,
            },
        )),
    }
}

fn hash_map_key(
    stream: &mut CdtStream<'_>,
    key: KeyKind,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
) -> crate::error::Result<u64> {
    let saved = stream.pos;
    let res = (|| {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        match key {
            KeyKind::Iri { start, end } => {
                h = fnv1a_byte(h, 0);
                h = hash_source(stream, CharSource::Raw { pos: start, end }, h, check)?;
            }
            other => {
                let (dt, lang, val) = literal_key_parts(other).expect("non-IRI key is a literal");
                h = fnv1a_byte(h, 1);
                h = hash_source(stream, dt, h, check)?;
                h = fnv1a_byte(h, 0xff);
                if let Some(lang) = lang {
                    h = fnv1a_byte(h, 0xfd);
                    h = hash_source(stream, lang, h, check)?;
                } else {
                    h = fnv1a_byte(h, 0xfe);
                }
                h = fnv1a_byte(h, 0xff);
                h = hash_source(stream, val, h, check)?;
            }
        }
        Ok(h)
    })();
    stream.pos = saved;
    res
}

fn map_keys_equal(
    stream: &mut CdtStream<'_>,
    a: KeyKind,
    b: KeyKind,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
) -> crate::error::Result<bool> {
    let saved = stream.pos;
    let res = match (a, b) {
        (KeyKind::Iri { start: sa, end: ea }, KeyKind::Iri { start: sb, end: eb }) => sources_eq(
            stream,
            CharSource::Raw { pos: sa, end: ea },
            CharSource::Raw { pos: sb, end: eb },
            check,
        ),
        (KeyKind::Iri { .. }, _) | (_, KeyKind::Iri { .. }) => Ok(false),
        (la, lb) => (|| {
            let (dt_a, lang_a, val_a) = literal_key_parts(la).expect("literal key");
            let (dt_b, lang_b, val_b) = literal_key_parts(lb).expect("literal key");
            let lang_eq = match (lang_a, lang_b) {
                (None, None) => true,
                (Some(x), Some(y)) => sources_eq(stream, x, y, check)?,
                _ => false,
            };
            Ok(lang_eq
                && sources_eq(stream, dt_a, dt_b, check)?
                && sources_eq(stream, val_a, val_b, check)?)
        })(),
    };
    stream.pos = saved;
    res
}

#[inline]
fn is_oxrdf_bnode_start(c: char) -> bool {
    matches!(
        c,
        '0'..='9'
            | '_'
            | ':'
            | 'A'..='Z'
            | 'a'..='z'
            | '\u{00C0}'..='\u{00D6}'
            | '\u{00D8}'..='\u{00F6}'
            | '\u{00F8}'..='\u{02FF}'
            | '\u{0370}'..='\u{037D}'
            | '\u{037F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}'
            | '\u{10000}'..='\u{EFFFF}'
    )
}

#[inline]
fn is_oxrdf_bnode_char(c: char) -> bool {
    is_oxrdf_bnode_start(c)
        || matches!(
            c,
            '.' | '-' | '\u{00B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'
        )
}

struct CdtStream<'a> {
    s: &'a str,
    pos: usize,
    depth: usize,
    ends: [usize; MAX_QUOTED_CDT_DEPTH],
    steps: u64,
    fallback_steps: u64,
    fallback_limit: u64,
    in_fallback: bool,
}

impl<'a> CdtStream<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s,
            pos: 0,
            depth: 0,
            ends: [0; MAX_QUOTED_CDT_DEPTH],
            steps: 0,
            fallback_steps: 0,
            fallback_limit: u64::MAX,
            in_fallback: false,
        }
    }

    #[inline]
    fn reset_fallback_budget(&mut self, level_len: usize) {
        self.fallback_steps = 0;
        self.fallback_limit = (level_len as u64)
            .saturating_mul(ZERO_HEAP_FALLBACK_STEP_FACTOR)
            .max(MIN_ZERO_HEAP_FALLBACK_STEPS);
        self.in_fallback = false;
    }

    #[inline]
    fn tick(
        &mut self,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        self.steps = self.steps.wrapping_add(1);
        if self.steps & 1023 == 0 {
            check()?;
        }
        if self.in_fallback {
            self.fallback_steps = self.fallback_steps.saturating_add(1);
            if self.fallback_steps > self.fallback_limit {
                return Err(crate::error::Error::StorageFull(
                    "composite literal validation exceeds memory budget".into(),
                ));
            }
        }
        Ok(())
    }

    #[inline]
    fn limit(&self) -> usize {
        if self.depth == 0 {
            self.s.len()
        } else {
            self.ends[self.depth - 1]
        }
    }

    #[inline]
    fn next_char(&mut self) -> Option<char> {
        self.next_at(self.depth)
    }

    #[inline]
    fn peek_char(&mut self) -> Option<char> {
        let saved = self.pos;
        let c = self.next_char();
        self.pos = saved;
        c
    }

    fn next_at(&mut self, level: usize) -> Option<char> {
        if level == 0 {
            let limit = self.limit();
            if self.pos >= limit {
                return None;
            }
            let c = self.s[self.pos..limit].chars().next()?;
            self.pos += c.len_utf8();
            return Some(c);
        }
        let c = self.next_at(level - 1)?;
        if c == '\\' {
            self.next_escape(level - 1)
        } else {
            Some(c)
        }
    }

    fn next_escape(&mut self, level: usize) -> Option<char> {
        let e = self.next_at(level)?;
        match e {
            't' => Some('\t'),
            'b' => Some('\u{8}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            'f' => Some('\u{c}'),
            '\\' | '"' | '\'' => Some(e),
            'u' | 'U' => {
                let digits = if e == 'u' { 4 } else { 8 };
                let mut buf = [0u8; 8];
                for slot in &mut buf[..digits] {
                    let ch = self.next_at(level)?;
                    if !ch.is_ascii() {
                        return None;
                    }
                    *slot = ch as u8;
                }
                let hex = std::str::from_utf8(&buf[..digits]).ok()?;
                let code = u32::from_str_radix(hex, 16).ok()?;
                char::from_u32(code)
            }
            _ => None,
        }
    }

    fn skip_ws(
        &mut self,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        while let Some(c) = self.peek_char() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                self.tick(check)?;
                let _ = self.next_char();
            } else {
                break;
            }
        }
        Ok(())
    }

    fn eat(&mut self, expected: char) -> bool {
        let saved = self.pos;
        if self.next_char() == Some(expected) {
            true
        } else {
            self.pos = saved;
            false
        }
    }

    fn eat_double_quote(&mut self, q: char) -> bool {
        let saved = self.pos;
        if self.next_char() == Some(q) && self.next_char() == Some(q) {
            return true;
        }
        self.pos = saved;
        false
    }

    fn eat_str(&mut self, expected: &str) -> bool {
        let saved = self.pos;
        for want in expected.chars() {
            if self.next_char() != Some(want) {
                self.pos = saved;
                return false;
            }
        }
        true
    }

    fn eat_cdt_kind(&mut self) -> Option<CdtKind> {
        let saved = self.pos;
        if !self.eat_str("^^<") || !self.eat_str(NS) {
            self.pos = saved;
            return None;
        }
        if self.eat_str("List>") {
            return Some(CdtKind::List);
        }
        if self.eat_str("Map>") {
            return Some(CdtKind::Map);
        }
        self.pos = saved;
        None
    }

    fn eat_iri_span(
        &mut self,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<(usize, usize)>> {
        if !self.eat('<') {
            return Ok(None);
        }
        let start = self.pos;
        loop {
            self.tick(check)?;
            let before = self.pos;
            let Some(ch) = self.next_char() else {
                return Ok(None);
            };
            if ch == '>' {
                return Ok(Some((start, before)));
            }
            if matches!(ch, '<' | '"' | '{' | '}' | '^' | '\\' | '|' | '`') || ch <= ' ' {
                return Ok(None);
            }
        }
    }

    fn eat_string_after_quote(
        &mut self,
        q: char,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<(usize, usize)>> {
        let long = self.eat_double_quote(q);
        let body_start = self.pos;
        loop {
            self.tick(check)?;
            let before = self.pos;
            let Some(ch) = self.next_char() else {
                return Ok(None);
            };
            if long {
                if ch == q && self.eat_double_quote(q) {
                    return Ok(Some((body_start, before)));
                }
            } else if ch == q {
                return Ok(Some((body_start, before)));
            } else if ch == '\n' || ch == '\r' {
                return Ok(None);
            }
            if ch == '\\' && self.next_escape(self.depth).is_none() {
                return Ok(None);
            }
        }
    }

    /// Consume a language tag (`[a-zA-Z]+(-[a-zA-Z0-9]+)*`) and, when
    /// `validate_bcp47` is set, validate it in streaming O(1) stack and zero
    /// heap with exact parity to `oxilangtag` 0.1.6's `parse_language_tag`.
    fn eat_langtag_span(
        &mut self,
        validate_bcp47: bool,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<(usize, usize)>> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum LangState {
            AfterLanguage,
            AfterExtLang,
            AfterScript,
            AfterRegion,
            InExtension { expected: bool },
            InPrivateUse { expected: bool },
        }

        let start = self.pos;
        let mut prefix = [0u8; 11];
        let mut total_len = 0usize;

        let mut primary_len = 0usize;
        let mut first_primary = 0u8;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_alphabetic() {
                self.tick(check)?;
                let _ = self.next_char();
                let b = c as u8;
                if primary_len == 0 {
                    first_primary = b;
                }
                if total_len < prefix.len() {
                    prefix[total_len] = b;
                }
                total_len += 1;
                primary_len += 1;
            } else {
                break;
            }
        }
        if primary_len == 0 {
            return Ok(None);
        }

        let mut extlangs_count = 0u8;
        let (mut valid, mut state) =
            if primary_len == 1 && first_primary.eq_ignore_ascii_case(&b'x') {
                (true, LangState::InPrivateUse { expected: true })
            } else if (2..=8).contains(&primary_len) {
                if primary_len < 4 {
                    (true, LangState::AfterLanguage)
                } else {
                    (true, LangState::AfterExtLang)
                }
            } else {
                (false, LangState::AfterExtLang)
            };

        while self.peek_char() == Some('-') {
            self.tick(check)?;
            let saved = self.pos;
            let saved_total_len = total_len;
            let _ = self.next_char();
            let mut sub_len = 0usize;
            let mut first_sub = 0u8;
            let mut all_alpha = true;
            let mut all_digit = true;
            while let Some(c) = self.peek_char() {
                if c.is_ascii_alphanumeric() {
                    self.tick(check)?;
                    let _ = self.next_char();
                    let b = c as u8;
                    if sub_len == 0 {
                        if total_len < prefix.len() {
                            prefix[total_len] = b'-';
                        }
                        total_len += 1;
                        first_sub = b;
                    }
                    if total_len < prefix.len() {
                        prefix[total_len] = b;
                    }
                    total_len += 1;
                    all_alpha &= b.is_ascii_alphabetic();
                    all_digit &= b.is_ascii_digit();
                    sub_len += 1;
                } else {
                    break;
                }
            }
            if sub_len == 0 {
                self.pos = saved;
                total_len = saved_total_len;
                break;
            }
            if validate_bcp47 && valid {
                if sub_len > 8 {
                    valid = false;
                } else {
                    match state {
                        LangState::InPrivateUse { .. } => {
                            state = LangState::InPrivateUse { expected: false };
                        }
                        _ if sub_len == 1 && first_sub.eq_ignore_ascii_case(&b'x') => {
                            if matches!(state, LangState::InExtension { expected: true }) {
                                valid = false;
                            } else {
                                state = LangState::InPrivateUse { expected: true };
                            }
                        }
                        _ if sub_len == 1 => {
                            if matches!(state, LangState::InExtension { expected: true }) {
                                valid = false;
                            } else {
                                state = LangState::InExtension { expected: true };
                            }
                        }
                        LangState::InExtension { .. } => {
                            state = LangState::InExtension { expected: false };
                        }
                        LangState::AfterLanguage if sub_len == 3 && all_alpha => {
                            extlangs_count += 1;
                            if extlangs_count > 3 {
                                valid = false;
                            }
                        }
                        LangState::AfterLanguage | LangState::AfterExtLang
                            if sub_len == 4 && all_alpha =>
                        {
                            state = LangState::AfterScript;
                        }
                        LangState::AfterLanguage
                        | LangState::AfterExtLang
                        | LangState::AfterScript
                            if (sub_len == 2 && all_alpha) || (sub_len == 3 && all_digit) =>
                        {
                            state = LangState::AfterRegion;
                        }
                        LangState::AfterLanguage
                        | LangState::AfterExtLang
                        | LangState::AfterScript
                        | LangState::AfterRegion
                            if (sub_len >= 5 && first_sub.is_ascii_alphabetic())
                                || (sub_len >= 4 && first_sub.is_ascii_digit()) =>
                        {
                            state = LangState::AfterRegion;
                        }
                        _ => {
                            valid = false;
                        }
                    }
                }
            }
        }
        let end = self.pos;
        if validate_bcp47 {
            let is_grandfathered = (5..=11).contains(&total_len)
                && GRANDFATHERED_LANGTAGS.iter().any(|g| {
                    g.len() == total_len && g.as_bytes().eq_ignore_ascii_case(&prefix[..total_len])
                });
            let normal_ok = valid
                && !matches!(
                    state,
                    LangState::InExtension { expected: true }
                        | LangState::InPrivateUse { expected: true }
                );
            if !is_grandfathered && !normal_ok {
                return Ok(None);
            }
        }
        Ok(Some((start, end)))
    }

    fn eat_number_span(
        &mut self,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<(usize, usize, &'static str)>> {
        let start = self.pos;
        if matches!(self.peek_char(), Some('+' | '-')) {
            let _ = self.next_char();
        }
        let mut int_digits = 0usize;
        while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            self.tick(check)?;
            let _ = self.next_char();
            int_digits += 1;
        }
        let mut frac_digits = 0usize;
        let mut dot = false;
        if self.peek_char() == Some('.') {
            let saved = self.pos;
            let _ = self.next_char();
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.tick(check)?;
                let _ = self.next_char();
                frac_digits += 1;
            }
            if int_digits > 0 || frac_digits > 0 {
                dot = true;
            } else {
                self.pos = saved;
            }
        }
        if int_digits == 0 && frac_digits == 0 {
            self.pos = start;
            return Ok(None);
        }
        let mut exp = false;
        if matches!(self.peek_char(), Some('e' | 'E')) {
            let saved = self.pos;
            let _ = self.next_char();
            if matches!(self.peek_char(), Some('+' | '-')) {
                let _ = self.next_char();
            }
            let mut exp_digits = 0usize;
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.tick(check)?;
                let _ = self.next_char();
                exp_digits += 1;
            }
            if exp_digits > 0 {
                exp = true;
            } else {
                self.pos = saved;
            }
        }
        let dt = if exp {
            xsd::DOUBLE.as_str()
        } else if dot {
            xsd::DECIMAL.as_str()
        } else {
            xsd::INTEGER.as_str()
        };
        Ok(Some((start, self.pos, dt)))
    }

    fn eat_bool(&mut self) -> Option<bool> {
        let saved = self.pos;
        let mut buf = [0u8; 5];
        let mut n = 0usize;
        while n < 5 {
            let p = self.pos;
            let Some(c) = self.next_char() else {
                break;
            };
            if !c.is_ascii() {
                self.pos = p;
                break;
            }
            buf[n] = c as u8;
            n += 1;
            if n == 4 && buf[..4].eq_ignore_ascii_case(b"true") {
                return Some(true);
            }
        }
        if n == 5 && buf.eq_ignore_ascii_case(b"false") {
            return Some(false);
        }
        self.pos = saved;
        None
    }

    fn eat_null(&mut self) -> bool {
        let saved = self.pos;
        if self.eat_str("null")
            && !self
                .peek_char()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            true
        } else {
            self.pos = saved;
            false
        }
    }

    fn eat_bnode_after_underscore(
        &mut self,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        mut visit: Option<&mut dyn FnMut(crate::id::Id) -> crate::error::Result<()>>,
    ) -> crate::error::Result<bool> {
        if !self.eat(':') {
            return Ok(false);
        }
        let Some(first) = self.next_char() else {
            return Ok(false);
        };
        if !(first == '_' || first.is_alphabetic() || first.is_ascii_digit()) {
            return Ok(false);
        }
        let mut oxrdf_ok = is_oxrdf_bnode_start(first);
        let mut buf = [0u8; 16];
        let mut len = 0usize;
        let mut valid_stored = first.is_ascii();
        if valid_stored {
            buf[0] = first as u8;
            len = 1;
        }
        let mut last_ok_pos = self.pos;
        let mut last_ok_oxrdf = oxrdf_ok;
        let mut last_ok_len = len;
        let mut last_ok_valid = valid_stored;
        let mut saw_dot = false;
        while let Some(ch) = self.next_char() {
            self.tick(check)?;
            if ch == '_' || ch == '-' || ch == '\u{b7}' || ch.is_alphanumeric() {
                if saw_dot {
                    valid_stored = false;
                    saw_dot = false;
                }
                oxrdf_ok &= is_oxrdf_bnode_char(ch);
                if valid_stored {
                    if ch.is_ascii() && len < buf.len() {
                        buf[len] = ch as u8;
                        len += 1;
                    } else {
                        valid_stored = false;
                    }
                }
                last_ok_pos = self.pos;
                last_ok_oxrdf = oxrdf_ok;
                last_ok_len = len;
                last_ok_valid = valid_stored;
            } else if ch == '.' {
                saw_dot = true;
            } else {
                break;
            }
        }
        self.pos = last_ok_pos;
        if !last_ok_oxrdf {
            return Ok(false);
        }
        if let Some(visit) = visit.as_mut()
            && last_ok_valid
            && let Ok(label) = std::str::from_utf8(&buf[..last_ok_len])
            && let Some(payload) = crate::id::parse_bnode_payload(label)
            && payload & crate::id::Id::LOCAL_BNODE_BIT == 0
        {
            visit(crate::id::Id::bnode(payload))?;
        }
        Ok(true)
    }

    fn eat_map_key(
        &mut self,
        validate_bcp47: bool,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<KeyKind>> {
        match self.peek_char() {
            Some('<') => Ok(self
                .eat_iri_span(check)?
                .map(|(start, end)| KeyKind::Iri { start, end })),
            Some('"' | '\'') => {
                let q = self.next_char().expect("peeked quote");
                let Some((body_start, body_end)) = self.eat_string_after_quote(q, check)? else {
                    return Ok(None);
                };
                let suffix = if self.eat('@') {
                    let Some((start, end)) = self.eat_langtag_span(validate_bcp47, check)? else {
                        return Ok(None);
                    };
                    KeySuffix::Lang { start, end }
                } else if self.eat_str("^^") {
                    let Some((start, end)) = self.eat_iri_span(check)? else {
                        return Ok(None);
                    };
                    KeySuffix::Typed { start, end }
                } else {
                    KeySuffix::Plain
                };
                Ok(Some(KeyKind::String {
                    body_start,
                    body_end,
                    suffix,
                }))
            }
            Some('+' | '-' | '.' | '0'..='9') => Ok(self
                .eat_number_span(check)?
                .map(|(start, end, dt)| KeyKind::Number { start, end, dt })),
            Some('t' | 'T' | 'f' | 'F') => Ok(self.eat_bool().map(KeyKind::Bool)),
            _ => Ok(None),
        }
    }

    fn eat_term_any(
        &mut self,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        if self.peek_char() == Some('_') {
            let _ = self.next_char();
            self.eat_bnode_after_underscore(check, None)
        } else {
            Ok(self.eat_map_key(true, check)?.is_some())
        }
    }
}

#[derive(Clone, Copy)]
struct PoolEntry {
    start: usize,
    tag: u32,
    next: u16,
}

impl PoolEntry {
    const EMPTY: Self = Self {
        start: 0,
        tag: 0,
        next: 0,
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MapKeyMode {
    /// Zero keys seen so far in this map; holds zero slots in `key_pool`.
    Empty,
    /// Exactly one key seen so far, stored inline in the frame (zero slots in
    /// `key_pool` and zero heap allocation even when `key_pool` is full).
    Single { start: usize, tag: u32 },
    /// Two or more keys seen so far, all indexed in `key_pool`/`key_buckets`.
    Inline,
    /// All keys seen so far in this map are indexed in the matching
    /// `overflow_tables` entry. This map holds zero slots in `key_pool`.
    WideTable,
    /// Tight-cap fallback where all earlier keys are already verified pairwise
    /// distinct and no keys are currently buffered in `key_pool`.
    BatchedFallbackEmpty,
    /// Tight-cap fallback where keys before `batch_start` are pairwise distinct
    /// and keys from `batch_start` onward are buffered in `key_pool` to be
    /// checked against earlier entries when the batch flushes or closes.
    BatchedFallback { batch_start: usize },
}

#[derive(Clone, Copy)]
struct MapFrame {
    start: usize,
    key_base: u16,
    key_count: u16,
    mode: MapKeyMode,
    mem_refused: bool,
}

impl MapFrame {
    const EMPTY: Self = Self {
        start: 0,
        key_base: 0,
        key_count: 0,
        mode: MapKeyMode::Empty,
        mem_refused: false,
    };
}

/// Compact 16-byte representation of [`MapFrame`] for literals under 4 GiB,
/// allowing paged overflow frames to grow in fixed 2 KiB increments without
/// doubling spikes.
#[derive(Clone, Copy)]
struct CompactMapFrame {
    start: u32,
    extra: u32,
    tag: u32,
    meta: u32,
}

impl CompactMapFrame {
    const EMPTY: Self = Self {
        start: 0,
        extra: 0,
        tag: 0,
        meta: 0,
    };

    #[inline]
    fn from_frame(f: MapFrame) -> Self {
        debug_assert!(f.key_base <= INLINE_MAP_KEY_POOL as u16);
        debug_assert!(f.key_count <= INLINE_MAP_KEY_POOL as u16);
        let (mode_bits, extra, tag) = match f.mode {
            MapKeyMode::Empty => (0u32, 0u32, 0u32),
            MapKeyMode::Single { start, tag } => (1u32, start as u32, tag),
            MapKeyMode::Inline => (2u32, 0u32, 0u32),
            MapKeyMode::WideTable => (3u32, 0u32, 0u32),
            MapKeyMode::BatchedFallbackEmpty => (4u32, 0u32, 0u32),
            MapKeyMode::BatchedFallback { batch_start } => (5u32, batch_start as u32, 0u32),
        };
        let meta = u32::from(f.key_base)
            | (u32::from(f.key_count) << 10)
            | (mode_bits << 20)
            | (u32::from(f.mem_refused) << 23);
        Self {
            start: f.start as u32,
            extra,
            tag,
            meta,
        }
    }

    #[inline]
    fn into_frame(self) -> MapFrame {
        let key_base = (self.meta & 0x3ff) as u16;
        let key_count = ((self.meta >> 10) & 0x3ff) as u16;
        let mode = match (self.meta >> 20) & 0x7 {
            0 => MapKeyMode::Empty,
            1 => MapKeyMode::Single {
                start: self.extra as usize,
                tag: self.tag,
            },
            2 => MapKeyMode::Inline,
            3 => MapKeyMode::WideTable,
            4 => MapKeyMode::BatchedFallbackEmpty,
            _ => MapKeyMode::BatchedFallback {
                batch_start: self.extra as usize,
            },
        };
        let mem_refused = ((self.meta >> 23) & 1) != 0;
        MapFrame {
            start: self.start as usize,
            key_base,
            key_count,
            mode,
            mem_refused,
        }
    }
}

const OVERFLOW_BITS_PAGE_WORDS: usize = 128;
const OVERFLOW_MAP_PAGE_SMALL: usize = 128;
const OVERFLOW_MAP_PAGE_LARGE: usize = 64;

// Pages are boxed individually so growing the page directory reallocates only
// 8-byte pointers instead of doubling the full 2 KiB page buffers.
#[allow(clippy::vec_box)]
enum OverflowMapPages {
    Empty,
    Small {
        pages: Vec<Box<[CompactMapFrame; OVERFLOW_MAP_PAGE_SMALL]>>,
        vec_bytes: u64,
        len: usize,
    },
    Large {
        pages: Vec<Box<[MapFrame; OVERFLOW_MAP_PAGE_LARGE]>>,
        vec_bytes: u64,
        len: usize,
    },
}

// Keep one spare page on pop to reuse across a page boundary. After a
// refused allocation, discard even that page and any unused directory.
fn trim_spill_pages<T>(
    pages: &mut Vec<Box<T>>,
    live_pages: usize,
    keep_spare: bool,
    directory_bytes: &mut u64,
) -> u64 {
    let keep = live_pages + usize::from(keep_spare);
    if keep == 0 {
        let bytes = (pages.len() * std::mem::size_of::<T>()) as u64;
        *pages = Vec::new();
        return bytes + std::mem::take(directory_bytes);
    }
    let released = pages.len().saturating_sub(keep);
    pages.truncate(keep);
    (released * std::mem::size_of::<T>()) as u64
}

impl OverflowMapPages {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Small { len, .. } | Self::Large { len, .. } => *len,
        }
    }

    #[inline]
    fn get(&self, idx: usize) -> MapFrame {
        match self {
            Self::Empty => unreachable!("overflow map pages empty"),
            Self::Small { pages, .. } => {
                pages[idx / OVERFLOW_MAP_PAGE_SMALL][idx % OVERFLOW_MAP_PAGE_SMALL].into_frame()
            }
            Self::Large { pages, .. } => {
                pages[idx / OVERFLOW_MAP_PAGE_LARGE][idx % OVERFLOW_MAP_PAGE_LARGE]
            }
        }
    }

    #[inline]
    fn set(&mut self, idx: usize, frame: MapFrame) {
        match self {
            Self::Empty => unreachable!("overflow map pages empty"),
            Self::Small { pages, .. } => {
                pages[idx / OVERFLOW_MAP_PAGE_SMALL][idx % OVERFLOW_MAP_PAGE_SMALL] =
                    CompactMapFrame::from_frame(frame);
            }
            Self::Large { pages, .. } => {
                pages[idx / OVERFLOW_MAP_PAGE_LARGE][idx % OVERFLOW_MAP_PAGE_LARGE] = frame;
            }
        }
    }

    fn trim(&mut self, keep_spare: bool, active_heap: &mut u64, total_bytes: &mut u64) -> bool {
        let released = match self {
            Self::Empty => return false,
            Self::Small {
                pages,
                vec_bytes,
                len,
            } => trim_spill_pages(
                pages,
                (*len).div_ceil(OVERFLOW_MAP_PAGE_SMALL),
                keep_spare,
                vec_bytes,
            ),
            Self::Large {
                pages,
                vec_bytes,
                len,
            } => trim_spill_pages(
                pages,
                (*len).div_ceil(OVERFLOW_MAP_PAGE_LARGE),
                keep_spare,
                vec_bytes,
            ),
        };
        *active_heap -= released;
        *total_bytes -= released;
        released != 0
    }

    #[inline]
    fn pop(&mut self, active_heap: &mut u64, total_bytes: &mut u64) {
        let crossed_page = match self {
            Self::Empty => unreachable!("pop on empty overflow map pages"),
            Self::Small { len, .. } => {
                *len -= 1;
                (*len).is_multiple_of(OVERFLOW_MAP_PAGE_SMALL)
            }
            Self::Large { len, .. } => {
                *len -= 1;
                (*len).is_multiple_of(OVERFLOW_MAP_PAGE_LARGE)
            }
        };
        if crossed_page {
            self.trim(true, active_heap, total_bytes);
        }
    }

    fn try_push(
        &mut self,
        frame: MapFrame,
        large: bool,
        active_heap: &mut u64,
        total_bytes: &mut u64,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> bool {
        if matches!(self, Self::Empty) {
            *self = if large {
                Self::Large {
                    pages: Vec::new(),
                    vec_bytes: 0,
                    len: 0,
                }
            } else {
                Self::Small {
                    pages: Vec::new(),
                    vec_bytes: 0,
                    len: 0,
                }
            };
        }
        match self {
            Self::Empty => unreachable!(),
            Self::Small {
                pages,
                vec_bytes,
                len,
            } => {
                let page_idx = *len / OVERFLOW_MAP_PAGE_SMALL;
                if page_idx == pages.len() {
                    let page_bytes =
                        std::mem::size_of::<[CompactMapFrame; OVERFLOW_MAP_PAGE_SMALL]>() as u64;
                    let elem =
                        std::mem::size_of::<Box<[CompactMapFrame; OVERFLOW_MAP_PAGE_SMALL]>>()
                            as u64;
                    if pages.len() == pages.capacity() {
                        let old_cap = pages.capacity();
                        let Some(new_cap) = (if old_cap == 0 {
                            Some(8)
                        } else {
                            old_cap.checked_mul(2)
                        }) else {
                            return false;
                        };
                        let Some(new_vec_bytes) = (new_cap as u64).checked_mul(elem) else {
                            return false;
                        };
                        if !allow_mem(
                            active_heap
                                .saturating_add(new_vec_bytes)
                                .saturating_add(page_bytes),
                        ) {
                            return false;
                        }
                        pages.reserve_exact(new_cap - pages.len());
                        *active_heap = active_heap
                            .saturating_sub(*vec_bytes)
                            .saturating_add(new_vec_bytes);
                        *total_bytes = total_bytes
                            .saturating_sub(*vec_bytes)
                            .saturating_add(new_vec_bytes);
                        *vec_bytes = new_vec_bytes;
                    } else if !allow_mem(active_heap.saturating_add(page_bytes)) {
                        return false;
                    }
                    pages.push(Box::new([CompactMapFrame::EMPTY; OVERFLOW_MAP_PAGE_SMALL]));
                    *active_heap = active_heap.saturating_add(page_bytes);
                    *total_bytes = total_bytes.saturating_add(page_bytes);
                }
                pages[page_idx][*len % OVERFLOW_MAP_PAGE_SMALL] =
                    CompactMapFrame::from_frame(frame);
                *len += 1;
                true
            }
            Self::Large {
                pages,
                vec_bytes,
                len,
            } => {
                let page_idx = *len / OVERFLOW_MAP_PAGE_LARGE;
                if page_idx == pages.len() {
                    let page_bytes =
                        std::mem::size_of::<[MapFrame; OVERFLOW_MAP_PAGE_LARGE]>() as u64;
                    let elem =
                        std::mem::size_of::<Box<[MapFrame; OVERFLOW_MAP_PAGE_LARGE]>>() as u64;
                    if pages.len() == pages.capacity() {
                        let old_cap = pages.capacity();
                        let Some(new_cap) = (if old_cap == 0 {
                            Some(8)
                        } else {
                            old_cap.checked_mul(2)
                        }) else {
                            return false;
                        };
                        let Some(new_vec_bytes) = (new_cap as u64).checked_mul(elem) else {
                            return false;
                        };
                        if !allow_mem(
                            active_heap
                                .saturating_add(new_vec_bytes)
                                .saturating_add(page_bytes),
                        ) {
                            return false;
                        }
                        pages.reserve_exact(new_cap - pages.len());
                        *active_heap = active_heap
                            .saturating_sub(*vec_bytes)
                            .saturating_add(new_vec_bytes);
                        *total_bytes = total_bytes
                            .saturating_sub(*vec_bytes)
                            .saturating_add(new_vec_bytes);
                        *vec_bytes = new_vec_bytes;
                    } else if !allow_mem(active_heap.saturating_add(page_bytes)) {
                        return false;
                    }
                    pages.push(Box::new([MapFrame::EMPTY; OVERFLOW_MAP_PAGE_LARGE]));
                    *active_heap = active_heap.saturating_add(page_bytes);
                    *total_bytes = total_bytes.saturating_add(page_bytes);
                }
                pages[page_idx][*len % OVERFLOW_MAP_PAGE_LARGE] = frame;
                *len += 1;
                true
            }
        }
    }
}

enum WideSlots {
    Small(Vec<(u32, u32)>),
    Large(Vec<(u64, usize)>),
}

struct WideKeySet {
    slots: WideSlots,
    len: usize,
}

impl WideKeySet {
    const MIN_CAPACITY: usize = 4;

    fn bytes_for(cap: usize, large: bool) -> u64 {
        let elem = if large {
            std::mem::size_of::<(u64, usize)>()
        } else {
            std::mem::size_of::<(u32, u32)>()
        };
        (cap as u64).saturating_mul(elem as u64)
    }

    fn new_preflighted(
        cap: usize,
        large: bool,
        active_heap: &mut u64,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> Option<Self> {
        let cap = cap.checked_next_power_of_two()?.max(Self::MIN_CAPACITY);
        let bytes = Self::bytes_for(cap, large);
        if !allow_mem(active_heap.saturating_add(bytes)) {
            return None;
        }
        let slots = if large {
            WideSlots::Large(vec![(0u64, 0usize); cap])
        } else {
            WideSlots::Small(vec![(0u32, 0u32); cap])
        };
        *active_heap = active_heap.saturating_add(bytes);
        Some(Self { slots, len: 0 })
    }

    fn capacity(&self) -> usize {
        match &self.slots {
            WideSlots::Small(v) => v.len(),
            WideSlots::Large(v) => v.len(),
        }
    }

    fn is_large(&self) -> bool {
        matches!(self.slots, WideSlots::Large(_))
    }

    fn bytes(&self) -> u64 {
        Self::bytes_for(self.capacity(), self.is_large())
    }

    fn needs_grow(&self) -> bool {
        self.len.saturating_mul(4) >= self.capacity().saturating_mul(3)
    }

    fn contains(
        &self,
        stream: &mut CdtStream<'_>,
        key: KeyKind,
        hash: u64,
        tag: u32,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        match &self.slots {
            WideSlots::Small(slots) => {
                let mask = slots.len() - 1;
                let mut idx = (tag as usize) & mask;
                loop {
                    stream.tick(check)?;
                    let (slot_tag, pos1) = slots[idx];
                    if pos1 == 0 {
                        return Ok(false);
                    }
                    if slot_tag == tag {
                        let saved = stream.pos;
                        stream.pos = (pos1 - 1) as usize;
                        let prev_key = stream.eat_map_key(false, check);
                        stream.pos = saved;
                        if let Some(prev_key) = prev_key?
                            && map_keys_equal(stream, prev_key, key, check)?
                        {
                            return Ok(true);
                        }
                    }
                    idx = (idx + 1) & mask;
                }
            }
            WideSlots::Large(slots) => {
                let mask = slots.len() - 1;
                let mut idx = (tag as usize) & mask;
                loop {
                    stream.tick(check)?;
                    let (slot_hash, pos1) = slots[idx];
                    if pos1 == 0 {
                        return Ok(false);
                    }
                    if slot_hash == hash {
                        let saved = stream.pos;
                        stream.pos = pos1 - 1;
                        let prev_key = stream.eat_map_key(false, check);
                        stream.pos = saved;
                        if let Some(prev_key) = prev_key?
                            && map_keys_equal(stream, prev_key, key, check)?
                        {
                            return Ok(true);
                        }
                    }
                    idx = (idx + 1) & mask;
                }
            }
        }
    }

    fn grow_preflighted(
        &mut self,
        stream: &mut CdtStream<'_>,
        active_heap: &mut u64,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> crate::error::Result<bool> {
        let old_cap = self.capacity();
        let Some(new_cap) = old_cap.checked_mul(2) else {
            return Ok(false);
        };
        let old_bytes = self.bytes();
        let new_bytes = Self::bytes_for(new_cap, self.is_large());
        // Account for old and new buffers live simultaneously during rehash.
        if !allow_mem(active_heap.saturating_add(new_bytes)) {
            return Ok(false);
        }
        let new_mask = new_cap - 1;
        match &mut self.slots {
            WideSlots::Small(old) => {
                let mut next = vec![(0u32, 0u32); new_cap];
                for &(tag, pos1) in old.iter() {
                    stream.tick(check)?;
                    if pos1 != 0 {
                        let mut idx = (tag as usize) & new_mask;
                        while next[idx].1 != 0 {
                            stream.tick(check)?;
                            idx = (idx + 1) & new_mask;
                        }
                        next[idx] = (tag, pos1);
                    }
                }
                *old = next;
            }
            WideSlots::Large(old) => {
                let mut next = vec![(0u64, 0usize); new_cap];
                for &(hash, pos1) in old.iter() {
                    stream.tick(check)?;
                    if pos1 != 0 {
                        let mut idx = (mix_tag(hash) as usize) & new_mask;
                        while next[idx].1 != 0 {
                            stream.tick(check)?;
                            idx = (idx + 1) & new_mask;
                        }
                        next[idx] = (hash, pos1);
                    }
                }
                *old = next;
            }
        }
        *active_heap = active_heap
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
        Ok(true)
    }

    fn insert_unique(
        &mut self,
        stream: &mut CdtStream<'_>,
        hash: u64,
        tag: u32,
        pos: usize,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        stream.tick(check)?;
        match &mut self.slots {
            WideSlots::Small(slots) => {
                let mask = slots.len() - 1;
                let mut idx = (tag as usize) & mask;
                while slots[idx].1 != 0 {
                    stream.tick(check)?;
                    idx = (idx + 1) & mask;
                }
                slots[idx] = (tag, (pos as u32) + 1);
            }
            WideSlots::Large(slots) => {
                let mask = slots.len() - 1;
                let mut idx = (tag as usize) & mask;
                while slots[idx].1 != 0 {
                    stream.tick(check)?;
                    idx = (idx + 1) & mask;
                }
                slots[idx] = (hash, pos + 1);
            }
        }
        self.len += 1;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct MapKeyEntry {
    start: usize,
    hash: u64,
    tag: u32,
}

fn skip_map_entry_value(
    stream: &mut CdtStream<'_>,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
) -> crate::error::Result<bool> {
    stream.skip_ws(check)?;
    if !stream.eat(':') {
        return Ok(false);
    }
    let mut val_depth = 0usize;
    while let Some(ch) = stream.next_char() {
        stream.tick(check)?;
        match ch {
            '[' | '{' => val_depth += 1,
            ']' | '}' => {
                if val_depth == 0 {
                    break;
                }
                val_depth -= 1;
            }
            ',' if val_depth == 0 => break,
            '<' => {
                while let Some(c) = stream.next_char() {
                    stream.tick(check)?;
                    if c == '>' {
                        break;
                    }
                }
            }
            '"' | '\'' => {
                let _ = stream.eat_string_after_quote(ch, check)?;
            }
            _ => {}
        }
    }
    Ok(true)
}

/// Returns `Ok(true)` if no duplicate was found before `cur.start`, `Ok(false)`
/// on duplicate, or `Err(StorageFull | Cancelled)` if bounded fallback or
/// cancellation triggers.
fn scan_single_against_prior(
    stream: &mut CdtStream<'_>,
    map_start: usize,
    cur_key: KeyKind,
    cur: MapKeyEntry,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
) -> crate::error::Result<bool> {
    let saved = stream.pos;
    let prev_fallback = std::mem::replace(&mut stream.in_fallback, true);
    stream.pos = map_start;
    let res = (|| {
        loop {
            stream.skip_ws(check)?;
            if stream.pos >= cur.start {
                return Ok(true);
            }
            stream.tick(check)?;
            let Some(prev_key) = stream.eat_map_key(false, check)? else {
                return Ok(true);
            };
            if hash_map_key(stream, prev_key, check)? == cur.hash
                && map_keys_equal(stream, prev_key, cur_key, check)?
            {
                return Ok(false);
            }
            if !skip_map_entry_value(stream, check)? {
                return Ok(true);
            }
        }
    })();
    stream.in_fallback = prev_fallback;
    stream.pos = saved;
    res
}

struct ContainerStack {
    bits: [u64; CONTAINER_WINDOW_WORDS],
    // Pages are boxed individually so growing the page directory reallocates
    // only 8-byte pointers instead of doubling the full 1 KiB bit pages.
    #[allow(clippy::vec_box)]
    overflow_bits_pages: Vec<Box<[u64; OVERFLOW_BITS_PAGE_WORDS]>>,
    overflow_bits_vec_bytes: u64,
    overflow_bits_bytes: u64,
    overflow_bits_len: usize,
    bits_base: usize,
    depth: usize,
    map_frames: [MapFrame; INLINE_MAP_FRAMES],
    overflow_map_frames: OverflowMapPages,
    overflow_map_frames_bytes: u64,
    map_frame_base: usize,
    map_depth: usize,
    key_pool: [PoolEntry; INLINE_MAP_KEY_POOL],
    key_pool_len: usize,
    key_buckets: [u16; INLINE_MAP_KEY_BUCKETS],
    overflow_tables: Vec<(usize, WideKeySet)>,
    overflow_tables_vec_bytes: u64,
    active_heap_bytes: u64,
}

impl ContainerStack {
    fn new() -> Self {
        Self {
            bits: [0; CONTAINER_WINDOW_WORDS],
            overflow_bits_pages: Vec::new(),
            overflow_bits_vec_bytes: 0,
            overflow_bits_bytes: 0,
            overflow_bits_len: 0,
            bits_base: 0,
            depth: 0,
            map_frames: [MapFrame::EMPTY; INLINE_MAP_FRAMES],
            overflow_map_frames: OverflowMapPages::Empty,
            overflow_map_frames_bytes: 0,
            map_frame_base: 0,
            map_depth: 0,
            key_pool: [PoolEntry::EMPTY; INLINE_MAP_KEY_POOL],
            key_pool_len: 0,
            key_buckets: [0; INLINE_MAP_KEY_BUCKETS],
            overflow_tables: Vec::new(),
            overflow_tables_vec_bytes: 0,
            active_heap_bytes: 0,
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.depth == 0
    }

    #[inline]
    fn overflow_bit_word(&self, word_idx: usize) -> u64 {
        self.overflow_bits_pages[word_idx / OVERFLOW_BITS_PAGE_WORDS]
            [word_idx % OVERFLOW_BITS_PAGE_WORDS]
    }

    #[inline]
    fn overflow_bit_word_mut(&mut self, word_idx: usize) -> &mut u64 {
        &mut self.overflow_bits_pages[word_idx / OVERFLOW_BITS_PAGE_WORDS]
            [word_idx % OVERFLOW_BITS_PAGE_WORDS]
    }

    #[inline]
    fn top_is_map(&self) -> bool {
        let d = self.depth - 1;
        if self.bits_base == 0 {
            if d < CONTAINER_WINDOW_BITS {
                (self.bits[d / 64] >> (d % 64)) & 1 != 0
            } else {
                let r = d - CONTAINER_WINDOW_BITS;
                (self.overflow_bit_word(r / 64) >> (r % 64)) & 1 != 0
            }
        } else {
            let rel = d - self.bits_base;
            (self.bits[rel / 64] >> (rel % 64)) & 1 != 0
        }
    }

    #[inline]
    fn top_map_frame(&self) -> MapFrame {
        let idx = self.map_depth - 1;
        if self.map_frame_base == 0 {
            if idx < INLINE_MAP_FRAMES {
                self.map_frames[idx]
            } else {
                self.overflow_map_frames.get(idx - INLINE_MAP_FRAMES)
            }
        } else {
            self.map_frames[idx - self.map_frame_base]
        }
    }

    #[inline]
    fn set_top_map_frame(&mut self, frame: MapFrame) {
        let idx = self.map_depth - 1;
        if self.map_frame_base == 0 {
            if idx < INLINE_MAP_FRAMES {
                self.map_frames[idx] = frame;
            } else {
                self.overflow_map_frames.set(idx - INLINE_MAP_FRAMES, frame);
            }
        } else {
            self.map_frames[idx - self.map_frame_base] = frame;
        }
    }

    #[inline]
    fn update_top_map_frame(&mut self, f: impl FnOnce(&mut MapFrame)) {
        let mut frame = self.top_map_frame();
        f(&mut frame);
        self.set_top_map_frame(frame);
    }

    fn shift_bits_from_spill(&mut self) {
        let ov_len = self.overflow_bits_len;
        debug_assert_eq!(self.depth, CONTAINER_WINDOW_BITS + ov_len * 64);
        if ov_len >= CONTAINER_WINDOW_HALF_WORDS {
            let start = ov_len - CONTAINER_WINDOW_HALF_WORDS;
            for i in 0..CONTAINER_WINDOW_HALF_WORDS {
                self.bits[i] = self.overflow_bit_word(start + i);
            }
        } else {
            let from_bits = CONTAINER_WINDOW_HALF_WORDS - ov_len;
            let bits_start = CONTAINER_WINDOW_WORDS - from_bits;
            self.bits.copy_within(bits_start..CONTAINER_WINDOW_WORDS, 0);
            for i in 0..ov_len {
                self.bits[from_bits + i] = self.overflow_bit_word(i);
            }
        }
        self.bits[CONTAINER_WINDOW_HALF_WORDS..].fill(0);
        self.bits_base = self.depth - CONTAINER_WINDOW_HALF_BITS;
        self.active_heap_bytes = self
            .active_heap_bytes
            .saturating_sub(self.overflow_bits_bytes);
        self.overflow_bits_pages = Vec::new();
        self.overflow_bits_vec_bytes = 0;
        self.overflow_bits_bytes = 0;
        self.overflow_bits_len = 0;
    }

    fn trim_overflow_bits(&mut self, keep_spare: bool) -> bool {
        let released = trim_spill_pages(
            &mut self.overflow_bits_pages,
            self.overflow_bits_len.div_ceil(OVERFLOW_BITS_PAGE_WORDS),
            keep_spare,
            &mut self.overflow_bits_vec_bytes,
        );
        self.active_heap_bytes -= released;
        self.overflow_bits_bytes -= released;
        released != 0
    }

    #[inline]
    fn pop_bit_overflow(&mut self) {
        if self.overflow_bits_len != 0 && self.depth.is_multiple_of(64) {
            self.overflow_bits_len -= 1;
            if self
                .overflow_bits_len
                .is_multiple_of(OVERFLOW_BITS_PAGE_WORDS)
            {
                self.trim_overflow_bits(true);
            }
        }
    }

    fn reclaim_inactive_spill_pages(&mut self) -> bool {
        let bits = self.trim_overflow_bits(false);
        let maps = self.overflow_map_frames.trim(
            false,
            &mut self.active_heap_bytes,
            &mut self.overflow_map_frames_bytes,
        );
        bits || maps
    }

    fn reclaim_inactive_spill(&mut self) -> bool {
        let mut freed = self.reclaim_inactive_spill_pages();
        if self.overflow_tables.is_empty() && self.overflow_tables_vec_bytes > 0 {
            self.overflow_tables = Vec::new();
            self.active_heap_bytes = self
                .active_heap_bytes
                .saturating_sub(self.overflow_tables_vec_bytes);
            self.overflow_tables_vec_bytes = 0;
            freed = true;
        }
        freed
    }

    fn try_grow_overflow_bits(&mut self, allow_mem: &mut dyn FnMut(u64) -> bool) -> bool {
        let page_idx = self.overflow_bits_len / OVERFLOW_BITS_PAGE_WORDS;
        if page_idx < self.overflow_bits_pages.len() {
            return true;
        }
        let page_bytes = std::mem::size_of::<[u64; OVERFLOW_BITS_PAGE_WORDS]>() as u64;
        let elem = std::mem::size_of::<Box<[u64; OVERFLOW_BITS_PAGE_WORDS]>>() as u64;
        for attempt in 0..2 {
            if self.overflow_bits_pages.len() == self.overflow_bits_pages.capacity() {
                let old_cap = self.overflow_bits_pages.capacity();
                let Some(new_cap) = (if old_cap == 0 {
                    Some(8)
                } else {
                    old_cap.checked_mul(2)
                }) else {
                    return false;
                };
                let Some(new_vec_bytes) = (new_cap as u64).checked_mul(elem) else {
                    return false;
                };
                let needed = self
                    .active_heap_bytes
                    .saturating_add(new_vec_bytes)
                    .saturating_add(page_bytes);
                if !allow_mem(needed) {
                    if attempt == 0 && self.reclaim_inactive_spill() {
                        continue;
                    }
                    return false;
                }
                self.overflow_bits_pages
                    .reserve_exact(new_cap - self.overflow_bits_pages.len());
                self.active_heap_bytes = self
                    .active_heap_bytes
                    .saturating_sub(self.overflow_bits_vec_bytes)
                    .saturating_add(new_vec_bytes);
                self.overflow_bits_bytes = self
                    .overflow_bits_bytes
                    .saturating_sub(self.overflow_bits_vec_bytes)
                    .saturating_add(new_vec_bytes);
                self.overflow_bits_vec_bytes = new_vec_bytes;
            } else if !allow_mem(self.active_heap_bytes.saturating_add(page_bytes)) {
                if attempt == 0 && self.reclaim_inactive_spill() {
                    continue;
                }
                return false;
            }
            self.overflow_bits_pages
                .push(Box::new([0u64; OVERFLOW_BITS_PAGE_WORDS]));
            self.active_heap_bytes = self.active_heap_bytes.saturating_add(page_bytes);
            self.overflow_bits_bytes = self.overflow_bits_bytes.saturating_add(page_bytes);
            return true;
        }
        false
    }

    fn push_bit(&mut self, is_map: bool, allow_mem: &mut dyn FnMut(u64) -> bool) {
        if self.bits_base == 0 {
            if self.depth < CONTAINER_WINDOW_BITS {
                let d = self.depth;
                if is_map {
                    self.bits[d / 64] |= 1u64 << (d % 64);
                } else {
                    self.bits[d / 64] &= !(1u64 << (d % 64));
                }
                self.depth += 1;
                return;
            }
            let r = self.depth - CONTAINER_WINDOW_BITS;
            let word_idx = r / 64;
            if word_idx < self.overflow_bits_len || self.try_grow_overflow_bits(allow_mem) {
                if word_idx == self.overflow_bits_len {
                    *self.overflow_bit_word_mut(word_idx) = 0;
                    self.overflow_bits_len += 1;
                }
                let word = self.overflow_bit_word_mut(word_idx);
                if is_map {
                    *word |= 1u64 << (r % 64);
                } else {
                    *word &= !(1u64 << (r % 64));
                }
                self.depth += 1;
                return;
            }
            self.shift_bits_from_spill();
        } else if self.depth == self.bits_base + CONTAINER_WINDOW_BITS {
            self.bits.copy_within(CONTAINER_WINDOW_HALF_WORDS.., 0);
            self.bits[CONTAINER_WINDOW_HALF_WORDS..].fill(0);
            self.bits_base += CONTAINER_WINDOW_HALF_BITS;
        }
        let rel = self.depth - self.bits_base;
        if is_map {
            self.bits[rel / 64] |= 1u64 << (rel % 64);
        } else {
            self.bits[rel / 64] &= !(1u64 << (rel % 64));
        }
        self.depth += 1;
    }

    fn push_list(&mut self, allow_mem: &mut dyn FnMut(u64) -> bool) {
        self.push_bit(false, allow_mem);
    }

    fn push_pool_entry(&mut self, tag: u32, start: usize) {
        let idx = self.key_pool_len;
        let bucket = (tag as usize) & (INLINE_MAP_KEY_BUCKETS - 1);
        let prev_head = self.key_buckets[bucket];
        self.key_pool[idx] = PoolEntry {
            start,
            tag,
            next: prev_head,
        };
        self.key_buckets[bucket] = (idx + 1) as u16;
        self.key_pool_len += 1;
    }

    fn pop_pool_to(&mut self, base: usize) {
        while self.key_pool_len > base {
            self.key_pool_len -= 1;
            let entry = self.key_pool[self.key_pool_len];
            let bucket = (entry.tag as usize) & (INLINE_MAP_KEY_BUCKETS - 1);
            self.key_buckets[bucket] = entry.next;
        }
    }

    fn pool_contains(
        &self,
        stream: &mut CdtStream<'_>,
        frame_key_base: usize,
        key: KeyKind,
        tag: u32,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        let mut cur = self.key_buckets[(tag as usize) & (INLINE_MAP_KEY_BUCKETS - 1)];
        while cur != 0 {
            stream.tick(check)?;
            let idx = (cur - 1) as usize;
            if idx < frame_key_base {
                break;
            }
            let entry = self.key_pool[idx];
            if entry.tag == tag {
                let saved = stream.pos;
                stream.pos = entry.start;
                let prev_key = stream.eat_map_key(false, check);
                stream.pos = saved;
                if let Some(prev_key) = prev_key?
                    && map_keys_equal(stream, prev_key, key, check)?
                {
                    return Ok(true);
                }
            }
            cur = entry.next;
        }
        Ok(false)
    }

    fn ensure_overflow_tables_slot_with_extra(
        &mut self,
        extra_bytes: u64,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> bool {
        let elem = std::mem::size_of::<(usize, WideKeySet)>() as u64;
        for attempt in 0..2 {
            if self.overflow_tables.len() < self.overflow_tables.capacity() {
                if allow_mem(self.active_heap_bytes.saturating_add(extra_bytes)) {
                    return true;
                }
                if attempt == 0 && self.reclaim_inactive_spill() {
                    continue;
                }
                return false;
            }
            let old_cap = self.overflow_tables.capacity();
            let Some(new_cap) = (if old_cap == 0 {
                Some(4)
            } else {
                old_cap.checked_mul(2)
            }) else {
                return false;
            };
            let Some(new_bytes) = (new_cap as u64).checked_mul(elem) else {
                return false;
            };
            if !allow_mem(
                self.active_heap_bytes
                    .saturating_add(new_bytes)
                    .saturating_add(extra_bytes),
            ) {
                if attempt == 0 && self.reclaim_inactive_spill() {
                    continue;
                }
                return false;
            }
            self.overflow_tables
                .reserve_exact(new_cap - self.overflow_tables.len());
            self.active_heap_bytes = self
                .active_heap_bytes
                .saturating_sub(self.overflow_tables_vec_bytes)
                .saturating_add(new_bytes);
            self.overflow_tables_vec_bytes = new_bytes;
            return true;
        }
        false
    }

    /// Returns `Ok(true)` if no duplicate was found between `[map_start, batch_start)`
    /// and the buffered batch in `key_pool[frame_key_base..]`, `Ok(false)` on duplicate,
    /// or `Err(StorageFull | Cancelled)` on budget or cancellation.
    fn scan_batch_against_prior(
        &mut self,
        stream: &mut CdtStream<'_>,
        map_start: usize,
        batch_start: usize,
        frame_key_base: usize,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        let saved = stream.pos;
        let prev_fallback = std::mem::replace(&mut stream.in_fallback, true);
        stream.pos = map_start;
        let res = (|| {
            loop {
                stream.skip_ws(check)?;
                if stream.pos >= batch_start {
                    return Ok(true);
                }
                stream.tick(check)?;
                let Some(prev_key) = stream.eat_map_key(false, check)? else {
                    return Ok(true);
                };
                let prev_hash = hash_map_key(stream, prev_key, check)?;
                let prev_tag = mix_tag(prev_hash);
                let mut cur = self.key_buckets[(prev_tag as usize) & (INLINE_MAP_KEY_BUCKETS - 1)];
                while cur != 0 {
                    stream.tick(check)?;
                    let idx = (cur - 1) as usize;
                    if idx < frame_key_base {
                        break;
                    }
                    let entry = self.key_pool[idx];
                    if entry.tag == prev_tag {
                        let after_prev = stream.pos;
                        stream.pos = entry.start;
                        let batch_key = stream.eat_map_key(false, check);
                        stream.pos = after_prev;
                        if let Some(batch_key) = batch_key?
                            && map_keys_equal(stream, prev_key, batch_key, check)?
                        {
                            return Ok(false);
                        }
                    }
                    cur = entry.next;
                }
                if !skip_map_entry_value(stream, check)? {
                    return Ok(true);
                }
            }
        })();
        stream.in_fallback = prev_fallback;
        stream.pos = saved;
        res
    }

    fn flush_and_clear_pool_frame(
        &mut self,
        frame: MapFrame,
        stream: &mut CdtStream<'_>,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        stream.tick(check)?;
        match frame.mode {
            MapKeyMode::BatchedFallback { batch_start } => {
                if !self.scan_batch_against_prior(
                    stream,
                    frame.start,
                    batch_start,
                    frame.key_base as usize,
                    check,
                )? {
                    return Ok(false);
                }
                self.pop_pool_to(frame.key_base as usize);
            }
            MapKeyMode::Inline => {
                self.pop_pool_to(frame.key_base as usize);
            }
            MapKeyMode::Empty
            | MapKeyMode::Single { .. }
            | MapKeyMode::WideTable
            | MapKeyMode::BatchedFallbackEmpty => {}
        }
        Ok(true)
    }

    fn shift_map_frames_from_spill(
        &mut self,
        stream: &mut CdtStream<'_>,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        let ov_len = self.overflow_map_frames.len();
        debug_assert_eq!(self.map_depth, INLINE_MAP_FRAMES + ov_len);
        let prev_fallback = std::mem::replace(&mut stream.in_fallback, true);
        let flush_res: crate::error::Result<bool> = (|| {
            for d in (0..self.map_depth).rev() {
                let frame = if d < INLINE_MAP_FRAMES {
                    self.map_frames[d]
                } else {
                    self.overflow_map_frames.get(d - INLINE_MAP_FRAMES)
                };
                if !self.flush_and_clear_pool_frame(frame, stream, check)? {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        stream.in_fallback = prev_fallback;
        if !flush_res? {
            return Ok(false);
        }
        debug_assert_eq!(self.key_pool_len, 0);
        if ov_len >= INLINE_MAP_FRAMES_HALF {
            let start = ov_len - INLINE_MAP_FRAMES_HALF;
            for i in 0..INLINE_MAP_FRAMES_HALF {
                self.map_frames[i] = self.overflow_map_frames.get(start + i);
            }
        } else {
            let from_inline = INLINE_MAP_FRAMES_HALF - ov_len;
            let inline_start = INLINE_MAP_FRAMES - from_inline;
            self.map_frames
                .copy_within(inline_start..INLINE_MAP_FRAMES, 0);
            for i in 0..ov_len {
                self.map_frames[from_inline + i] = self.overflow_map_frames.get(i);
            }
        }
        for frame in &mut self.map_frames[..INLINE_MAP_FRAMES_HALF] {
            frame.key_base = 0;
            frame.key_count = 0;
            if matches!(
                frame.mode,
                MapKeyMode::Inline | MapKeyMode::BatchedFallback { .. }
            ) {
                frame.mode = MapKeyMode::BatchedFallbackEmpty;
            }
        }
        self.active_heap_bytes = self
            .active_heap_bytes
            .saturating_sub(self.overflow_map_frames_bytes);
        self.overflow_map_frames = OverflowMapPages::Empty;
        self.overflow_map_frames_bytes = 0;
        self.map_frame_base = self.map_depth - INLINE_MAP_FRAMES_HALF;
        Ok(true)
    }

    fn shift_map_frames(
        &mut self,
        stream: &mut CdtStream<'_>,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        // Flush any buffered batched-fallback frames from innermost to outermost
        // so the LIFO key_buckets chains stay exact while clearing key_pool.
        for i in (0..INLINE_MAP_FRAMES).rev() {
            let frame = self.map_frames[i];
            if !self.flush_and_clear_pool_frame(frame, stream, check)? {
                return Ok(false);
            }
        }
        debug_assert_eq!(self.key_pool_len, 0);
        self.map_frames.copy_within(INLINE_MAP_FRAMES_HALF.., 0);
        for frame in &mut self.map_frames[..INLINE_MAP_FRAMES_HALF] {
            frame.key_base = 0;
            frame.key_count = 0;
            if matches!(
                frame.mode,
                MapKeyMode::Inline | MapKeyMode::BatchedFallback { .. }
            ) {
                frame.mode = MapKeyMode::BatchedFallbackEmpty;
            }
        }
        self.map_frame_base += INLINE_MAP_FRAMES_HALF;
        Ok(true)
    }

    fn push_map(
        &mut self,
        start: usize,
        stream: &mut CdtStream<'_>,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> crate::error::Result<bool> {
        self.push_bit(true, allow_mem);
        let new_frame = MapFrame {
            start,
            key_base: self.key_pool_len as u16,
            key_count: 0,
            mode: MapKeyMode::Empty,
            mem_refused: false,
        };
        if self.map_frame_base == 0 {
            if self.map_depth < INLINE_MAP_FRAMES {
                self.map_frames[self.map_depth] = new_frame;
                self.map_depth += 1;
                return Ok(true);
            }
            let large = stream.s.len() >= u32::MAX as usize;
            if self.overflow_map_frames.try_push(
                new_frame,
                large,
                &mut self.active_heap_bytes,
                &mut self.overflow_map_frames_bytes,
                allow_mem,
            ) || (self.reclaim_inactive_spill()
                && self.overflow_map_frames.try_push(
                    new_frame,
                    large,
                    &mut self.active_heap_bytes,
                    &mut self.overflow_map_frames_bytes,
                    allow_mem,
                ))
            {
                self.map_depth += 1;
                return Ok(true);
            }
            if !self.shift_map_frames_from_spill(stream, check)? {
                return Ok(false);
            }
        } else if self.map_depth == self.map_frame_base + INLINE_MAP_FRAMES
            && !self.shift_map_frames(stream, check)?
        {
            return Ok(false);
        }
        let rel = self.map_depth - self.map_frame_base;
        self.map_frames[rel] = MapFrame {
            key_base: self.key_pool_len as u16,
            ..new_frame
        };
        self.map_depth += 1;
        Ok(true)
    }

    fn refill_bits(
        &mut self,
        stream: &mut CdtStream<'_>,
        level_start: usize,
        up_to: usize,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        let new_base = self.bits_base.saturating_sub(CONTAINER_WINDOW_HALF_BITS);
        let window_end = new_base + CONTAINER_WINDOW_BITS;
        self.bits.fill(0);
        let saved = stream.pos;
        let prev_fallback = std::mem::replace(&mut stream.in_fallback, true);
        stream.pos = level_start;
        let res: crate::error::Result<()> = (|| {
            let mut d = 0usize;
            while stream.pos < up_to {
                stream.tick(check)?;
                let Some(ch) = stream.next_char() else {
                    break;
                };
                match ch {
                    '[' => {
                        if d >= new_base && d < window_end {
                            let rel = d - new_base;
                            self.bits[rel / 64] &= !(1u64 << (rel % 64));
                        }
                        d += 1;
                    }
                    '{' => {
                        if d >= new_base && d < window_end {
                            let rel = d - new_base;
                            self.bits[rel / 64] |= 1u64 << (rel % 64);
                        }
                        d += 1;
                    }
                    ']' | '}' => d = d.saturating_sub(1),
                    '<' => {
                        while let Some(c) = stream.next_char() {
                            stream.tick(check)?;
                            if c == '>' {
                                break;
                            }
                        }
                    }
                    '"' | '\'' => {
                        let _ = stream.eat_string_after_quote(ch, check)?;
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        stream.in_fallback = prev_fallback;
        stream.pos = saved;
        res?;
        self.bits_base = new_base;
        Ok(())
    }

    fn refill_map_frames(
        &mut self,
        stream: &mut CdtStream<'_>,
        level_start: usize,
        up_to: usize,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        debug_assert_eq!(self.key_pool_len, 0);
        debug_assert_eq!(self.map_depth, self.map_frame_base);
        let new_base = self.map_frame_base.saturating_sub(INLINE_MAP_FRAMES_HALF);
        let window_end = new_base + INLINE_MAP_FRAMES;
        let saved = stream.pos;
        let prev_fallback = std::mem::replace(&mut stream.in_fallback, true);
        stream.pos = level_start;
        let res: crate::error::Result<()> = (|| {
            let mut m = 0usize;
            while stream.pos < up_to {
                stream.tick(check)?;
                let Some(ch) = stream.next_char() else {
                    break;
                };
                match ch {
                    '{' => {
                        if m >= new_base && m < window_end {
                            self.map_frames[m - new_base] = MapFrame {
                                start: stream.pos,
                                key_base: 0,
                                key_count: 0,
                                mode: MapKeyMode::BatchedFallbackEmpty,
                                mem_refused: false,
                            };
                        }
                        m += 1;
                    }
                    '}' => m = m.saturating_sub(1),
                    '<' => {
                        while let Some(c) = stream.next_char() {
                            stream.tick(check)?;
                            if c == '>' {
                                break;
                            }
                        }
                    }
                    '"' | '\'' => {
                        let _ = stream.eat_string_after_quote(ch, check)?;
                    }
                    _ => {}
                }
            }
            // Replay records the last opener at each depth, including slots that
            // may have held many closed siblings. Only restore live tables here.
            // The stack top is inside the restored window, so walk down to its base.
            for &(d, _) in self.overflow_tables.iter().rev() {
                stream.tick(check)?;
                if d <= new_base {
                    break;
                }
                self.map_frames[(d - 1) - new_base].mode = MapKeyMode::WideTable;
            }
            Ok(())
        })();
        stream.in_fallback = prev_fallback;
        stream.pos = saved;
        res?;
        self.map_frame_base = new_base;
        Ok(())
    }

    fn pop_list(
        &mut self,
        stream: &mut CdtStream<'_>,
        level_start: usize,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        debug_assert!(!self.top_is_map());
        self.depth -= 1;
        self.pop_bit_overflow();
        if self.bits_base > 0 && self.depth > 0 && self.depth - 1 < self.bits_base {
            self.refill_bits(stream, level_start, stream.pos, check)?;
        }
        Ok(())
    }

    fn finish_map_and_pop(
        &mut self,
        stream: &mut CdtStream<'_>,
        level_start: usize,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<bool> {
        debug_assert!(self.top_is_map());
        let frame = self.top_map_frame();
        match frame.mode {
            MapKeyMode::Empty | MapKeyMode::Single { .. } | MapKeyMode::BatchedFallbackEmpty => {}
            MapKeyMode::Inline => {
                self.pop_pool_to(frame.key_base as usize);
            }
            MapKeyMode::WideTable => {
                let (d, table) = self
                    .overflow_tables
                    .pop()
                    .expect("wide table matches active map frame");
                debug_assert_eq!(d, self.map_depth);
                self.active_heap_bytes = self.active_heap_bytes.saturating_sub(table.bytes());
                drop(table);
            }
            MapKeyMode::BatchedFallback { batch_start } => {
                if !self.scan_batch_against_prior(
                    stream,
                    frame.start,
                    batch_start,
                    frame.key_base as usize,
                    check,
                )? {
                    return Ok(false);
                }
                self.pop_pool_to(frame.key_base as usize);
            }
        }
        if self.map_frame_base == 0 && self.map_depth > INLINE_MAP_FRAMES {
            self.overflow_map_frames.pop(
                &mut self.active_heap_bytes,
                &mut self.overflow_map_frames_bytes,
            );
        }
        self.depth -= 1;
        self.pop_bit_overflow();
        self.map_depth -= 1;
        if self.bits_base > 0 && self.depth > 0 && self.depth - 1 < self.bits_base {
            self.refill_bits(stream, level_start, stream.pos, check)?;
        }
        if self.map_frame_base > 0 && self.map_depth > 0 && self.map_depth - 1 < self.map_frame_base
        {
            self.refill_map_frames(stream, level_start, stream.pos, check)?;
        }
        Ok(true)
    }

    fn try_promote_single_to_wide(
        &mut self,
        first_start: usize,
        first_tag: u32,
        stream: &mut CdtStream<'_>,
        cur: MapKeyEntry,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> crate::error::Result<bool> {
        let large = stream.s.len() >= u32::MAX as usize;
        let init_cap = WideKeySet::MIN_CAPACITY;
        let init_bytes = WideKeySet::bytes_for(init_cap, large);
        if !self.ensure_overflow_tables_slot_with_extra(init_bytes, allow_mem) {
            return Ok(false);
        }
        let Some(mut table) =
            WideKeySet::new_preflighted(init_cap, large, &mut self.active_heap_bytes, allow_mem)
        else {
            return Ok(false);
        };
        let insert_res = (|| -> crate::error::Result<()> {
            let first_hash = if large {
                let saved = stream.pos;
                stream.pos = first_start;
                let prev_key = stream.eat_map_key(false, check);
                stream.pos = saved;
                if let Some(prev_key) = prev_key? {
                    hash_map_key(stream, prev_key, check)?
                } else {
                    0
                }
            } else {
                0
            };
            table.insert_unique(stream, first_hash, first_tag, first_start, check)?;
            table.insert_unique(stream, cur.hash, cur.tag, cur.start, check)?;
            Ok(())
        })();
        if let Err(e) = insert_res {
            self.active_heap_bytes = self.active_heap_bytes.saturating_sub(table.bytes());
            drop(table);
            return Err(e);
        }
        self.overflow_tables.push((self.map_depth, table));
        self.update_top_map_frame(|frame| {
            frame.mode = MapKeyMode::WideTable;
            frame.key_count = 0;
        });
        Ok(true)
    }

    fn try_promote_inline_to_wide(
        &mut self,
        stream: &mut CdtStream<'_>,
        cur: MapKeyEntry,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> crate::error::Result<bool> {
        let frame = self.top_map_frame();
        let large = stream.s.len() >= u32::MAX as usize;
        let init_cap = ((frame.key_count as usize + 1) * 4 / 3 + 1)
            .next_power_of_two()
            .max(WideKeySet::MIN_CAPACITY);
        let init_bytes = WideKeySet::bytes_for(init_cap, large);
        if !self.ensure_overflow_tables_slot_with_extra(init_bytes, allow_mem) {
            return Ok(false);
        }
        let Some(mut table) =
            WideKeySet::new_preflighted(init_cap, large, &mut self.active_heap_bytes, allow_mem)
        else {
            return Ok(false);
        };
        let start_i = frame.key_base as usize;
        let end_i = start_i + frame.key_count as usize;
        let insert_res = (|| -> crate::error::Result<()> {
            for idx in start_i..end_i {
                let entry = self.key_pool[idx];
                let hash = if large {
                    let saved = stream.pos;
                    stream.pos = entry.start;
                    let prev_key = stream.eat_map_key(false, check);
                    stream.pos = saved;
                    if let Some(prev_key) = prev_key? {
                        hash_map_key(stream, prev_key, check)?
                    } else {
                        0
                    }
                } else {
                    0
                };
                table.insert_unique(stream, hash, entry.tag, entry.start, check)?;
            }
            table.insert_unique(stream, cur.hash, cur.tag, cur.start, check)?;
            Ok(())
        })();
        if let Err(e) = insert_res {
            self.active_heap_bytes = self.active_heap_bytes.saturating_sub(table.bytes());
            drop(table);
            return Err(e);
        }
        self.pop_pool_to(start_i);
        self.overflow_tables.push((self.map_depth, table));
        self.update_top_map_frame(|frame| {
            frame.mode = MapKeyMode::WideTable;
            frame.key_count = 0;
        });
        Ok(true)
    }

    /// Attempt to promote a refilled `BatchedFallback` map frame into a
    /// preflighted `WideKeySet` by scanning its earlier entries once.
    /// Returns `Ok(Some(true))` on promotion, `Ok(Some(false))` if a duplicate
    /// was found, and `Ok(None)` if `allow_mem` refused the table.
    fn try_promote_batched_to_wide(
        &mut self,
        stream: &mut CdtStream<'_>,
        prior_end: usize,
        cur: MapKeyEntry,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> crate::error::Result<Option<bool>> {
        let frame = self.top_map_frame();
        let large = stream.s.len() >= u32::MAX as usize;
        let init_cap = ((frame.key_count as usize + 1) * 4 / 3 + 1)
            .next_power_of_two()
            .max(WideKeySet::MIN_CAPACITY);
        let init_bytes = WideKeySet::bytes_for(init_cap, large);
        if !self.ensure_overflow_tables_slot_with_extra(init_bytes, allow_mem) {
            return Ok(None);
        }
        let Some(mut table) =
            WideKeySet::new_preflighted(init_cap, large, &mut self.active_heap_bytes, allow_mem)
        else {
            return Ok(None);
        };
        let start_i = frame.key_base as usize;
        let end_i = start_i + frame.key_count as usize;
        let saved = stream.pos;
        let prev_fallback = std::mem::replace(&mut stream.in_fallback, true);
        let scan_res: crate::error::Result<Option<bool>> = (|| {
            for idx in start_i..end_i {
                let entry = self.key_pool[idx];
                let hash = if large {
                    let saved_inner = stream.pos;
                    stream.pos = entry.start;
                    let prev_key = stream.eat_map_key(false, check);
                    stream.pos = saved_inner;
                    if let Some(prev_key) = prev_key? {
                        hash_map_key(stream, prev_key, check)?
                    } else {
                        0
                    }
                } else {
                    0
                };
                table.insert_unique(stream, hash, entry.tag, entry.start, check)?;
            }
            table.insert_unique(stream, cur.hash, cur.tag, cur.start, check)?;

            stream.pos = frame.start;
            loop {
                stream.skip_ws(check)?;
                if stream.pos >= prior_end {
                    return Ok(Some(true));
                }
                stream.tick(check)?;
                let prev_start = stream.pos;
                let Some(prev_key) = stream.eat_map_key(false, check)? else {
                    return Ok(Some(true));
                };
                let prev_hash = hash_map_key(stream, prev_key, check)?;
                let prev_tag = mix_tag(prev_hash);
                if table.contains(stream, prev_key, prev_hash, prev_tag, check)? {
                    return Ok(Some(false));
                }
                if table.needs_grow() {
                    let mut grew = table.grow_preflighted(
                        stream,
                        &mut self.active_heap_bytes,
                        check,
                        allow_mem,
                    )?;
                    if !grew && self.reclaim_inactive_spill_pages() {
                        grew = table.grow_preflighted(
                            stream,
                            &mut self.active_heap_bytes,
                            check,
                            allow_mem,
                        )?;
                    }
                    if !grew {
                        return Ok(None);
                    }
                }
                table.insert_unique(stream, prev_hash, prev_tag, prev_start, check)?;
                if !skip_map_entry_value(stream, check)? {
                    return Ok(Some(true));
                }
            }
        })();
        stream.in_fallback = prev_fallback;
        stream.pos = saved;
        match scan_res {
            Ok(Some(true)) => {
                self.pop_pool_to(start_i);
                self.overflow_tables.push((self.map_depth, table));
                self.update_top_map_frame(|frame| {
                    frame.mode = MapKeyMode::WideTable;
                    frame.key_count = 0;
                });
                Ok(Some(true))
            }
            Ok(Some(false)) => {
                self.active_heap_bytes = self.active_heap_bytes.saturating_sub(table.bytes());
                drop(table);
                Ok(Some(false))
            }
            Ok(None) => {
                self.active_heap_bytes = self.active_heap_bytes.saturating_sub(table.bytes());
                drop(table);
                Ok(None)
            }
            Err(e) => {
                self.active_heap_bytes = self.active_heap_bytes.saturating_sub(table.bytes());
                drop(table);
                Err(e)
            }
        }
    }

    /// Record `cur_key` in the current innermost map frame and return `Ok(false)`
    /// when it duplicates an earlier key of the same map.
    fn register_map_key(
        &mut self,
        stream: &mut CdtStream<'_>,
        key_start: usize,
        key: KeyKind,
        cur_hash: u64,
        check: &mut dyn FnMut() -> crate::error::Result<()>,
        allow_mem: &mut dyn FnMut(u64) -> bool,
    ) -> crate::error::Result<bool> {
        let cur = MapKeyEntry {
            start: key_start,
            hash: cur_hash,
            tag: mix_tag(cur_hash),
        };
        let frame = self.top_map_frame();
        match frame.mode {
            MapKeyMode::Empty => {
                self.update_top_map_frame(|frame| {
                    frame.mode = MapKeyMode::Single {
                        start: cur.start,
                        tag: cur.tag,
                    };
                });
                Ok(true)
            }
            MapKeyMode::Single {
                start: first_start,
                tag: first_tag,
            } => {
                if first_tag == cur.tag {
                    let saved = stream.pos;
                    stream.pos = first_start;
                    let prev_key = stream.eat_map_key(false, check);
                    stream.pos = saved;
                    if let Some(prev_key) = prev_key?
                        && map_keys_equal(stream, prev_key, key, check)?
                    {
                        return Ok(false);
                    }
                }
                if self.key_pool_len + 2 <= INLINE_MAP_KEY_POOL {
                    let base = self.key_pool_len as u16;
                    self.push_pool_entry(first_tag, first_start);
                    self.push_pool_entry(cur.tag, cur.start);
                    self.update_top_map_frame(|frame| {
                        frame.key_base = base;
                        frame.key_count = 2;
                        frame.mode = MapKeyMode::Inline;
                    });
                    return Ok(true);
                }
                if self.try_promote_single_to_wide(
                    first_start,
                    first_tag,
                    stream,
                    cur,
                    check,
                    allow_mem,
                )? {
                    return Ok(true);
                }
                // Tight memory cap refused WideTable; both keys are already
                // verified distinct.
                let base = self.key_pool_len as u16;
                self.update_top_map_frame(|frame| {
                    frame.key_base = base;
                    frame.key_count = 0;
                    frame.mem_refused = true;
                    frame.mode = MapKeyMode::BatchedFallbackEmpty;
                });
                Ok(true)
            }
            MapKeyMode::Inline => {
                if self.pool_contains(stream, frame.key_base as usize, key, cur.tag, check)? {
                    return Ok(false);
                }
                if self.key_pool_len < INLINE_MAP_KEY_POOL {
                    self.push_pool_entry(cur.tag, cur.start);
                    self.update_top_map_frame(|frame| frame.key_count += 1);
                    return Ok(true);
                }
                if self.try_promote_inline_to_wide(stream, cur, check, allow_mem)? {
                    return Ok(true);
                }
                // Tight memory cap refused WideTable; all prior keys in key_pool
                // were already checked against each other and against cur_key.
                self.pop_pool_to(frame.key_base as usize);
                self.update_top_map_frame(|frame| {
                    frame.key_count = 0;
                    frame.mem_refused = true;
                    frame.mode = MapKeyMode::BatchedFallbackEmpty;
                });
                Ok(true)
            }
            MapKeyMode::WideTable => {
                let (d, table) = self
                    .overflow_tables
                    .last_mut()
                    .expect("wide table matches active map frame");
                debug_assert_eq!(*d, self.map_depth);
                if table.contains(stream, key, cur.hash, cur.tag, check)? {
                    return Ok(false);
                }
                if table.needs_grow()
                    && !table.grow_preflighted(
                        stream,
                        &mut self.active_heap_bytes,
                        check,
                        allow_mem,
                    )?
                    && !(self.reclaim_inactive_spill_pages()
                        && self
                            .overflow_tables
                            .last_mut()
                            .expect("wide table matches active map frame")
                            .1
                            .grow_preflighted(
                                stream,
                                &mut self.active_heap_bytes,
                                check,
                                allow_mem,
                            )?)
                {
                    // All keys up to and including cur_key have been checked.
                    let (_, old) = self.overflow_tables.pop().expect("wide table exists");
                    self.active_heap_bytes = self.active_heap_bytes.saturating_sub(old.bytes());
                    drop(old);
                    let base = self.key_pool_len as u16;
                    self.update_top_map_frame(|frame| {
                        frame.key_base = base;
                        frame.key_count = 0;
                        frame.mem_refused = true;
                        frame.mode = MapKeyMode::BatchedFallbackEmpty;
                    });
                    return Ok(true);
                }
                self.overflow_tables
                    .last_mut()
                    .expect("wide table matches active map frame")
                    .1
                    .insert_unique(stream, cur.hash, cur.tag, cur.start, check)?;
                Ok(true)
            }
            MapKeyMode::BatchedFallbackEmpty | MapKeyMode::BatchedFallback { .. } => {
                let batch_start = match frame.mode {
                    MapKeyMode::BatchedFallback { batch_start } => Some(batch_start),
                    _ => None,
                };
                if batch_start.is_some()
                    && self.pool_contains(stream, frame.key_base as usize, key, cur.tag, check)?
                {
                    return Ok(false);
                }
                if self.key_pool_len < INLINE_MAP_KEY_POOL {
                    if batch_start.is_none() {
                        let base = self.key_pool_len as u16;
                        self.update_top_map_frame(|frame| {
                            frame.key_base = base;
                            frame.mode = MapKeyMode::BatchedFallback {
                                batch_start: cur.start,
                            };
                        });
                    }
                    self.push_pool_entry(cur.tag, cur.start);
                    self.update_top_map_frame(|frame| frame.key_count += 1);
                    return Ok(true);
                }
                // Key pool is full. If this frame entered BatchedFallback due to
                // map_frames shifting (not a memory refusal), try promoting to WideTable.
                let prior_end = batch_start.unwrap_or(cur.start);
                if !frame.mem_refused {
                    match self
                        .try_promote_batched_to_wide(stream, prior_end, cur, check, allow_mem)?
                    {
                        Some(ok) => return Ok(ok),
                        None => {
                            self.update_top_map_frame(|frame| frame.mem_refused = true);
                        }
                    }
                }
                if let Some(b_start) = batch_start {
                    if !self.scan_batch_against_prior(
                        stream,
                        frame.start,
                        b_start,
                        frame.key_base as usize,
                        check,
                    )? {
                        return Ok(false);
                    }
                    self.pop_pool_to(frame.key_base as usize);
                    self.push_pool_entry(cur.tag, cur.start);
                    self.update_top_map_frame(|frame| {
                        frame.mode = MapKeyMode::BatchedFallback {
                            batch_start: cur.start,
                        };
                        frame.key_count = 1;
                    });
                    Ok(true)
                } else {
                    scan_single_against_prior(stream, frame.start, key, cur, check)
                }
            }
        }
    }
}

/// Validate a single CDT lexical level (matching `parse_list` / `parse_map`,
/// including all bare nested lists/maps and map duplicate-key checks, without
/// descending into quoted nested CDT literals) iteratively in O(1) stack.
/// Uses zero heap for language tags, bare nesting up to 65,536 containers,
/// map nesting up to 256 levels, single-key maps at any pool occupancy, and
/// nested multi-key maps whose concurrent active keys fit in the 512-key stack
/// pool. Deeper containers and additional keys spill to memory-preflighted
/// proportional heap buffers (or bounded zero-heap fallback when memory is
/// tight) and check cancellation throughout.
fn validate_cdt(
    stream: &mut CdtStream<'_>,
    is_map: bool,
    check: &mut dyn FnMut() -> crate::error::Result<()>,
    allow_mem: &mut dyn FnMut(u64) -> bool,
) -> crate::error::Result<bool> {
    #[derive(Clone, Copy)]
    enum Expect {
        ListStartOrEnd,
        ListValue,
        MapStartOrEnd,
        MapKey,
        AfterValue,
    }

    let level_start = stream.pos;
    stream.reset_fallback_budget(stream.limit().saturating_sub(level_start));
    stream.skip_ws(check)?;
    let mut stack = ContainerStack::new();
    let mut expect = if is_map {
        if !stream.eat('{') {
            return Ok(false);
        }
        if stack.depth >= stream.limit().saturating_sub(stream.pos) {
            return Ok(false);
        }
        if !stack.push_map(stream.pos, stream, check, allow_mem)? {
            return Ok(false);
        }
        Expect::MapStartOrEnd
    } else {
        if !stream.eat('[') {
            return Ok(false);
        }
        if stack.depth >= stream.limit().saturating_sub(stream.pos) {
            return Ok(false);
        }
        stack.push_list(allow_mem);
        Expect::ListStartOrEnd
    };

    loop {
        stream.tick(check)?;
        stream.skip_ws(check)?;
        match expect {
            Expect::ListStartOrEnd => {
                if stream.eat(']') {
                    stack.pop_list(stream, level_start, check)?;
                    expect = Expect::AfterValue;
                    continue;
                }
                expect = Expect::ListValue;
            }
            Expect::ListValue => match stream.peek_char() {
                Some('[') => {
                    let _ = stream.next_char();
                    if stack.depth >= stream.limit().saturating_sub(stream.pos) {
                        return Ok(false);
                    }
                    stack.push_list(allow_mem);
                    expect = Expect::ListStartOrEnd;
                }
                Some('{') => {
                    let _ = stream.next_char();
                    if stack.depth >= stream.limit().saturating_sub(stream.pos) {
                        return Ok(false);
                    }
                    if !stack.push_map(stream.pos, stream, check, allow_mem)? {
                        return Ok(false);
                    }
                    expect = Expect::MapStartOrEnd;
                }
                Some(_) if stream.eat_null() || stream.eat_term_any(check)? => {
                    expect = Expect::AfterValue;
                }
                _ => return Ok(false),
            },
            Expect::MapStartOrEnd | Expect::MapKey => {
                let is_first_key = matches!(expect, Expect::MapStartOrEnd);
                if is_first_key && stream.eat('}') {
                    if !stack.finish_map_and_pop(stream, level_start, check)? {
                        return Ok(false);
                    }
                    expect = Expect::AfterValue;
                    continue;
                }
                let key_start = stream.pos;
                let Some(key) = stream.eat_map_key(true, check)? else {
                    return Ok(false);
                };
                let cur_hash = hash_map_key(stream, key, check)?;
                if !stack.register_map_key(stream, key_start, key, cur_hash, check, allow_mem)? {
                    return Ok(false);
                }
                stream.skip_ws(check)?;
                if !stream.eat(':') {
                    return Ok(false);
                }
                expect = Expect::ListValue;
            }
            Expect::AfterValue => {
                if stack.is_empty() {
                    break;
                }
                if stack.top_is_map() {
                    if stream.eat('}') {
                        if !stack.finish_map_and_pop(stream, level_start, check)? {
                            return Ok(false);
                        }
                    } else if stream.eat(',') {
                        expect = Expect::MapKey;
                    } else {
                        return Ok(false);
                    }
                } else if stream.eat(']') {
                    stack.pop_list(stream, level_start, check)?;
                } else if stream.eat(',') {
                    expect = Expect::ListValue;
                } else {
                    return Ok(false);
                }
            }
        }
    }

    stream.skip_ws(check)?;
    Ok(stream.peek_char().is_none())
}

/// Scan a composite literal's lexical form in O(1) stack without constructing
/// parsed elements or replacement strings, calling `visit` for every stored
/// blank-node id referenced as a CDT blank node (including inside arbitrarily
/// nested bare lists/maps and quoted nested `cdt:List` / `cdt:Map` literals).
/// Uses zero heap for language tags, bare nesting up to 65,536 containers,
/// map nesting up to 256 levels, single-key maps at any pool occupancy, and
/// nested multi-key maps whose concurrent active keys fit in the 512-key stack
/// pool; deeper containers or additional keys preflight proportional temporary
/// heap buffers through `allow_mem` (falling back to bounded zero-heap scans
/// under tight caps) and drop them before returning. `check` is polled
/// periodically for cancellation/deadlines. Ill-typed CDT literals (or
/// ill-typed quoted nested CDT literals) are skipped cleanly without calling
/// `visit`, matching `relabel_lexical`.
pub(crate) fn scan_bnode_ids(
    lex: &str,
    dt: &str,
    mut check: impl FnMut() -> crate::error::Result<()>,
    mut allow_mem: impl FnMut(u64) -> bool,
    mut visit: impl FnMut(crate::id::Id) -> crate::error::Result<()>,
) -> crate::error::Result<()> {
    if !is_cdt(dt) {
        return Ok(());
    }
    let mut stream = CdtStream::new(lex);
    let mut resumes = [0usize; MAX_QUOTED_CDT_DEPTH];
    let mut current_is_map = dt == MAP;
    let mut need_validate = true;
    loop {
        stream.tick(&mut check)?;
        if need_validate {
            let level_start = stream.pos;
            if !validate_cdt(&mut stream, current_is_map, &mut check, &mut allow_mem)? {
                if stream.depth == 0 {
                    return Ok(());
                }
                stream.depth -= 1;
                stream.pos = resumes[stream.depth];
                need_validate = false;
                continue;
            }
            stream.pos = level_start;
            need_validate = false;
        }
        let Some(c) = stream.next_char() else {
            if stream.depth == 0 {
                return Ok(());
            }
            stream.depth -= 1;
            stream.pos = resumes[stream.depth];
            continue;
        };
        match c {
            '<' => {
                while let Some(ch) = stream.next_char() {
                    stream.tick(&mut check)?;
                    if ch == '>' {
                        break;
                    }
                }
            }
            '"' | '\'' => {
                if let Some((body_start, body_end)) =
                    stream.eat_string_after_quote(c, &mut check)?
                {
                    if let Some(kind) = stream.eat_cdt_kind() {
                        let raw = &lex.as_bytes()[body_start..body_end];
                        if memchr::memmem::find(raw, b"_:").is_some()
                            || memchr::memchr(b'\\', raw).is_some()
                        {
                            if stream.depth >= MAX_QUOTED_CDT_DEPTH {
                                return Err(crate::error::Error::invalid(
                                    "composite literal exceeds quoted nesting ceiling",
                                ));
                            }
                            resumes[stream.depth] = stream.pos;
                            stream.ends[stream.depth] = body_end;
                            stream.depth += 1;
                            stream.pos = body_start;
                            current_is_map = matches!(kind, CdtKind::Map);
                            need_validate = true;
                        }
                    } else if stream.eat('@') {
                        let _ = stream.eat_langtag_span(false, &mut check)?;
                    } else if stream.eat_str("^^") {
                        let _ = stream.eat_iri_span(&mut check)?;
                    }
                }
            }
            '_' => {
                stream.eat_bnode_after_underscore(&mut check, Some(&mut visit))?;
            }
            _ => {}
        }
    }
}

/// Whether a literal is a composite literal whose lexical form may name blank nodes.
/// This is the only test that a literal without blank nodes pays.
#[inline]
pub fn may_name_bnodes(l: &Literal) -> bool {
    is_cdt(l.datatype().as_str()) && l.value().contains("_:")
}

/// A composite literal with each blank node label in it replaced by `f(label)`, also in
/// the lists, maps and composite literals nested in it, written in the canonical form.
/// `None` when the literal is not a composite literal, names no blank node or is
/// ill-formed, so that it is kept as it is.
pub fn relabel_literal(l: &Literal, f: &mut dyn FnMut(&str) -> String) -> Option<Literal> {
    if !may_name_bnodes(l) {
        return None;
    }
    let dt = l.datatype().as_str();
    relabel_lexical(l.value(), dt, f)
        .map(|lex| Literal::new_typed_literal(lex, NamedNode::new_unchecked(dt)))
}

/// [`relabel_literal`] for a term: a literal, or a triple term whose object holds one.
pub fn relabel_term(t: &Term, f: &mut dyn FnMut(&str) -> String) -> Option<Term> {
    match t {
        Term::Literal(l) => relabel_literal(l, f).map(Term::Literal),
        Term::Triple(tr) => {
            let o = relabel_term(&tr.object, f)?;
            Some(Term::Triple(Box::new(oxrdf::Triple::new(
                tr.subject.clone(),
                tr.predicate.clone(),
                o,
            ))))
        }
        _ => None,
    }
}

/// Callback-only parsing guard. Ordinary CDT operations retain their existing
/// path. Its deliberately conservative estimate covers vector capacity, owned
/// RDF values, datatype strings and nested lexical copies, rather than pretending
/// that the opaque literal's lexical size measures its materialized parser heap.
pub(crate) struct CallbackRelabel<'q> {
    pub term: Option<Term>,
    _charge: super::ctx::Charge<'q>,
}

pub(crate) fn callback_relabel_term<'q>(
    ctx: &'q Ctx,
    term: &Term,
    f: &mut dyn FnMut(&str) -> String,
) -> crate::error::Result<CallbackRelabel<'q>> {
    fn lexical_depth(ctx: &Ctx, lex: &str, inherited: usize) -> crate::error::Result<usize> {
        // The preflight only retains one decoded string at each nesting level.
        // Charge it before using the existing string decoder, including growth.
        let _preflight = ctx.charge((lex.len() as u64).saturating_mul(2) + 256)?;
        let mut parser = Parser { s: lex, i: 0 };
        let mut depth = inherited;
        let mut peak = depth;
        while let Some(c) = parser.peek() {
            ctx.check()?;
            match c {
                '[' | '{' => {
                    depth += 1;
                    if depth > 128 {
                        return Err(crate::error::Error::invalid(
                            "callback composite literal exceeds nesting ceiling",
                        ));
                    }
                    peak = peak.max(depth);
                    parser.i += 1;
                }
                ']' | '}' => {
                    depth = depth.saturating_sub(1).max(inherited);
                    parser.i += 1;
                }
                '"' | '\'' => {
                    let Some(value) = parser.string() else { break };
                    if parser.rest().starts_with("^^") {
                        parser.i += 2;
                        let Some(datatype) = parser.iri() else { break };
                        if is_cdt(&datatype) {
                            // Quoted nested CDTs are opaque to the outer parser's
                            // bracket depth; check their decoded forms as well.
                            peak = peak.max(lexical_depth(ctx, &value, depth)?);
                        }
                    }
                }
                '<' => {
                    let Some(end) = parser.rest().find('>') else {
                        break;
                    };
                    parser.i += end + 1;
                }
                _ => parser.i += c.len_utf8(),
            }
        }
        Ok(peak)
    }
    fn estimate(ctx: &Ctx, term: &Term) -> crate::error::Result<u64> {
        match term {
            Term::Literal(l) if may_name_bnodes(l) => {
                let depth = lexical_depth(ctx, l.value(), 0)?;
                // An element consumes at least one input byte. Four vector slots
                // per byte cover both parsed and rewritten list/map capacities;
                // 128 covers term metadata, implicit datatype and label growth.
                // Nested lexical copies add four bytes per byte and depth level.
                let per_byte = 4 * std::mem::size_of::<(Term, Elem)>()
                    + 4 * std::mem::size_of::<Elem>()
                    + 128
                    + 4 * depth;
                Ok((l.value().len() as u64)
                    .saturating_mul(per_byte as u64)
                    .saturating_add(1024))
            }
            Term::Triple(t) => estimate(ctx, &t.object),
            _ => Ok(0),
        }
    }
    ctx.check()?;
    let charge = ctx.charge(estimate(ctx, term)?)?;
    let term = relabel_term(term, f);
    ctx.check()?;
    Ok(CallbackRelabel {
        term,
        _charge: charge,
    })
}

fn relabel_lexical(lex: &str, dt: &str, f: &mut dyn FnMut(&str) -> String) -> Option<String> {
    let mut changed = false;
    if dt == LIST {
        let l: Vec<Elem> = parse_list(lex)?
            .into_iter()
            .map(|e| relabel_elem(e, f, &mut changed))
            .collect();
        changed.then(|| list_lexical(&l))
    } else {
        let mut m = Map::new();
        for (k, v) in parse_map(lex)? {
            let k = relabel_elem_term(k, f, &mut changed);
            let v = relabel_elem(v, f, &mut changed);
            map_put(&mut m, k, v);
        }
        changed.then(|| map_lexical(&m))
    }
}

fn relabel_elem(e: Elem, f: &mut dyn FnMut(&str) -> String, changed: &mut bool) -> Elem {
    match e {
        Elem::Term(t) => Elem::Term(relabel_elem_term(t, f, changed)),
        Elem::Null => Elem::Null,
    }
}

fn relabel_elem_term(t: Term, f: &mut dyn FnMut(&str) -> String, changed: &mut bool) -> Term {
    match t {
        Term::BlankNode(b) => {
            *changed = true;
            Term::BlankNode(BlankNode::new_unchecked(f(b.as_str())))
        }
        Term::Literal(l) => match relabel_literal(&l, f) {
            Some(l) => {
                *changed = true;
                Term::Literal(l)
            }
            None => Term::Literal(l),
        },
        t => t,
    }
}

// ----------------------------------------------------------- equality and order ----

/// Jena's order of map keys (`CDTKeySorter`): IRIs first, by IRI; then literals by
/// datatype IRI, lexical form and language tag.
pub fn key_cmp(a: &Term, b: &Term) -> Ordering {
    match (a, b) {
        (Term::NamedNode(x), Term::NamedNode(y)) => x.as_str().cmp(y.as_str()),
        (Term::NamedNode(_), _) => Ordering::Less,
        (_, Term::NamedNode(_)) => Ordering::Greater,
        (Term::Literal(x), Term::Literal(y)) => x
            .datatype()
            .as_str()
            .cmp(y.datatype().as_str())
            .then_with(|| x.value().cmp(y.value()))
            .then_with(|| x.language().cmp(&y.language())),
        _ => term_string(a).cmp(&term_string(b)),
    }
}

fn term_string(t: &Term) -> String {
    let mut s = String::new();
    term_ttl(t, &mut s);
    s
}

/// The value of an element, for comparisons.
fn val(t: &Term) -> Value {
    Value::from_term(t)
}

/// Jena's `Node.sameValueAs`, which list and map equality apply to their elements: the
/// same term, or literals with equal values of compatible types. An `xsd:integer` and an
/// `xsd:decimal` can be the same value, but neither is the same value as an
/// `xsd:double`, and an `xsd:float` is not the same value as an `xsd:double`.
pub fn same_value(a: &Term, b: &Term) -> EvalResult<bool> {
    if a == b {
        return Ok(true);
    }
    let (Term::Literal(_), Term::Literal(_)) = (a, b) else {
        return Ok(false);
    };
    let (x, y) = (val(a), val(b));
    if let (Ok(p), Ok(q)) = (Num::of(&x), Num::of(&y)) {
        let family = |n: &Num| match n {
            Num::Integer(_) | Num::Decimal(_) => 0,
            Num::Float(_) => 1,
            Num::Double(_) => 2,
        };
        if family(&p) != family(&q) {
            return Ok(false);
        }
        return Ok(equals(&x, &y).unwrap_or(false));
    }
    match (&x, &y) {
        (Value::Other { dt: d1, .. }, Value::Other { dt: d2, .. }) if is_cdt(d1) && is_cdt(d2) => {
            equals(&x, &y)
        }
        (Value::Other { .. }, _) | (_, Value::Other { .. }) => Ok(false),
        _ if std::mem::discriminant(&x) == std::mem::discriminant(&y) => {
            Ok(equals(&x, &y).unwrap_or(false))
        }
        _ => Ok(false),
    }
}

/// Equality of two elements: nulls equal only nulls, and a blank node can only be
/// compared with itself.
fn elem_equals(a: &Elem, b: &Elem) -> EvalResult<bool> {
    match (a, b) {
        (Elem::Null, Elem::Null) => Ok(true),
        (Elem::Null, _) | (_, Elem::Null) => Ok(false),
        (Elem::Term(x), Elem::Term(y)) => {
            if (x.is_blank_node() || y.is_blank_node()) && x != y {
                return Err(TypeError);
            }
            same_value(x, y)
        }
    }
}

/// `=` on two composite literals (`CompositeDatatypeList.isEqual` and its map form).
/// An ill-formed literal is only equal to the same literal.
pub fn equals_cdt(lex1: &str, dt1: &str, lex2: &str, dt2: &str) -> EvalResult<bool> {
    if dt1 != dt2 {
        return Ok(false);
    }
    if dt1 == LIST {
        let (Some(a), Some(b)) = (parse_list(lex1), parse_list(lex2)) else {
            return if lex1 == lex2 {
                Ok(true)
            } else {
                Err(TypeError)
            };
        };
        if a.len() != b.len() {
            return Ok(false);
        }
        for (x, y) in a.iter().zip(&b) {
            if !elem_equals(x, y)? {
                return Ok(false);
            }
        }
        Ok(true)
    } else {
        let (Some(a), Some(b)) = (parse_map(lex1), parse_map(lex2)) else {
            return if lex1 == lex2 {
                Ok(true)
            } else {
                Err(TypeError)
            };
        };
        if a.len() != b.len() {
            return Ok(false);
        }
        for (k, v) in &a {
            let Some((_, w)) = b.iter().find(|(k2, _)| k2 == k) else {
                return Ok(false);
            };
            if !elem_equals(v, w)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Compare two elements. In `sort` mode this is the ORDER BY order, with nulls first;
/// otherwise it is `<` (`None`: equal), which fails for values that are neither ordered
/// nor equal.
fn elem_cmp(a: &Elem, b: &Elem, sort: bool) -> EvalResult<Ordering> {
    match (a, b) {
        (Elem::Term(x), Elem::Term(y)) => {
            let (vx, vy) = (val(x), val(y));
            if sort {
                return Ok(order_cmp(Some(&vx), Some(&vy)));
            }
            if x.is_blank_node() && y.is_blank_node() {
                return Err(TypeError);
            }
            if let Ok(Some(o)) = compare(&vx, &vy)
                && o != Ordering::Equal
            {
                return Ok(o);
            }
            match equals(&vx, &vy) {
                Ok(true) => Ok(Ordering::Equal),
                _ => Err(TypeError),
            }
        }
        (Elem::Null, Elem::Null) => Ok(Ordering::Equal),
        _ if !sort => Err(TypeError),
        (Elem::Null, _) => Ok(Ordering::Less),
        (_, Elem::Null) => Ok(Ordering::Greater),
    }
}

/// Compare two composite literals of the same datatype: `<` (list-less-than and
/// map-less-than) unless `sort`, the ORDER BY order when `sort`. In the ORDER BY order,
/// literals that are otherwise equal are ordered by lexical form.
pub fn compare_cdt(lex1: &str, lex2: &str, dt: &str, sort: bool) -> EvalResult<Ordering> {
    let tie = || {
        if sort {
            lex1.cmp(lex2)
        } else {
            Ordering::Equal
        }
    };
    if dt == LIST {
        let (Some(a), Some(b)) = (parse_list(lex1), parse_list(lex2)) else {
            return Err(TypeError);
        };
        if a.is_empty() || b.is_empty() {
            return Ok(a.len().cmp(&b.len()).then_with(tie));
        }
        for (x, y) in a.iter().zip(&b) {
            let o = elem_cmp(x, y, sort)?;
            if o != Ordering::Equal {
                return Ok(o);
            }
        }
        Ok(a.len().cmp(&b.len()).then_with(tie))
    } else {
        let (Some(a), Some(b)) = (parse_map(lex1), parse_map(lex2)) else {
            return Err(TypeError);
        };
        if a.is_empty() || b.is_empty() {
            return Ok(a.len().cmp(&b.len()).then_with(tie));
        }
        for ((k1, v1), (k2, v2)) in a.iter().zip(&b) {
            let o = key_cmp(k1, k2);
            if o != Ordering::Equal {
                return Ok(o);
            }
            let o = elem_cmp(v1, v2, sort)?;
            if o != Ordering::Equal {
                return Ok(o);
            }
        }
        Ok(a.len().cmp(&b.len()).then_with(tie))
    }
}

// --------------------------------------------------------------------- functions ----

/// The term an argument evaluates to, with its lexical form.
fn term_of(v: Val, ctx: &Ctx) -> EvalResult<Term> {
    match v {
        Val::Id(id) | Val::Dec(id, _) => ctx.term(id).ok_or(TypeError),
        Val::V(v) => Ok(v.to_term()),
    }
}

/// Checked variant of [`term_of`] that keeps owned term credit live on `credit`.
fn term_of_into(v: Val, ctx: &Ctx, credit: &super::ctx::Charge<'_>) -> EvalResult<Term> {
    match v {
        Val::Id(id) | Val::Dec(id, _) => ctx
            .term_checked_into(id, credit)
            .map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?
            .ok_or(TypeError),
        Val::V(v) => {
            credit
                .add(super::ctx::ValueCache::entry_bytes(&v))
                .map_err(|e| {
                    ctx.fail_memory(&e);
                    TypeError
                })?;
            Ok(v.to_term())
        }
    }
}

/// A map key: an IRI or a literal.
fn key_of(t: Term) -> Option<Term> {
    matches!(t, Term::NamedNode(_) | Term::Literal(_)).then_some(t)
}

fn typed<'t>(t: &'t Term, dt: &str) -> EvalResult<&'t str> {
    match t {
        Term::Literal(l) if l.datatype().as_str() == dt => Ok(l.value()),
        _ => Err(TypeError),
    }
}

fn list_arg(t: &Term) -> EvalResult<Vec<Elem>> {
    parse_list(typed(t, LIST)?).ok_or(TypeError)
}

fn map_arg(t: &Term) -> EvalResult<Map> {
    parse_map(typed(t, MAP)?).ok_or(TypeError)
}

/// An element as a value. A literal whose lexical form its value does not keep, such as
/// `01` or `1.50`, stays that term, so `sameTerm` and `STR` see the form the list holds.
fn elem_val(e: &Elem, ctx: &Ctx) -> EvalResult<Val> {
    match e {
        Elem::Term(t) => {
            let v = Value::from_term(t);
            if matches!(t, Term::Literal(_)) && v.to_term() != *t {
                let id = ctx.intern_term(t);
                if ctx.has_memory_failure() {
                    return Err(TypeError);
                }
                Ok(Val::Dec(id, v))
            } else {
                Ok(Val::V(v))
            }
        }
        Elem::Null => Err(TypeError),
    }
}

fn integer(v: &Value) -> EvalResult<i64> {
    match v {
        Value::Integer(i) => Ok(i64::from(*i)),
        _ => Err(TypeError),
    }
}

/// Insert or replace a map entry, keeping key order.
pub fn map_put(m: &mut Map, key: Term, value: Elem) {
    match m.binary_search_by(|(k, _)| key_cmp(k, &key)) {
        Ok(at) => m[at].1 = value,
        Err(at) => m.insert(at, (key, value)),
    }
}

fn map_get<'m>(m: &'m Map, key: &Term) -> Option<&'m Elem> {
    m.binary_search_by(|(k, _)| key_cmp(k, key))
        .ok()
        .map(|at| &m[at].1)
}

/// A `cdt:` function call (`local` is the local name).
pub fn call(local: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    let ev = |i: usize| eval(args.get(i).ok_or(TypeError)?, row, ctx);
    let term = |i: usize| -> EvalResult<Term> { term_of(ev(i)?, ctx) };
    let n = args.len();
    let admit_value = |v: Value| -> EvalResult<Val> {
        if let Value::Other { ref lex, .. } = v
            && !ctx.reserve_decode(lex.len())
        {
            drop(v);
            return Err(TypeError);
        }
        Ok(Val::V(v))
    };
    let list = |l: &[Elem]| admit_value(list_value(l));
    let map = |m: &[(Term, Elem)]| admit_value(map_value(m));
    let arity = |lo: usize, hi: usize| {
        if (lo..=hi).contains(&n) {
            Ok(())
        } else {
            Err(TypeError)
        }
    };
    match local {
        // the constructors read expression errors as nulls, but never swallow fatal refusals
        "List" => {
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let mut l = Vec::with_capacity(n);
            for i in 0..n {
                match ev(i).and_then(|v| term_of_into(v, ctx, &decode)) {
                    Ok(t) => l.push(Elem::Term(t)),
                    Err(_) if ctx.has_memory_failure() => return Err(TypeError),
                    Err(_) => l.push(Elem::Null),
                }
            }
            list(&l)
        }
        "Map" => {
            if n % 2 == 1 {
                return Err(TypeError);
            }
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let mut m = Map::new();
            for i in (0..n).step_by(2) {
                let k = match ev(i).and_then(|v| term_of_into(v, ctx, &decode)) {
                    Ok(t) => match key_of(t) {
                        Some(k) => k,
                        None => continue,
                    },
                    Err(_) if ctx.has_memory_failure() => return Err(TypeError),
                    Err(_) => continue,
                };
                let v = match ev(i + 1).and_then(|v| term_of_into(v, ctx, &decode)) {
                    Ok(t) => Elem::Term(t),
                    Err(_) if ctx.has_memory_failure() => return Err(TypeError),
                    Err(_) => Elem::Null,
                };
                map_put(&mut m, k, v);
            }
            map(&m)
        }
        "concat" => match n {
            0 => list(&[]),
            1 => {
                let v = ev(0)?;
                list_arg(&term_of(v.clone(), ctx)?)?;
                Ok(v)
            }
            _ => {
                let decode = ctx.charge(0).map_err(|e| {
                    ctx.fail_memory(&e);
                    TypeError
                })?;
                let mut out = Vec::new();
                for i in 0..n {
                    let t = term_of_into(ev(i)?, ctx, &decode)?;
                    out.extend(list_arg(&t)?);
                }
                list(&out)
            }
        },
        "contains" => {
            arity(2, 2)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let l = list_arg(&term_of_into(ev(0)?, ctx, &decode)?)?;
            let x = ev(1)?;
            let id = x.clone().into_id(ctx);
            if ctx.has_memory_failure() {
                return Err(TypeError);
            }
            let x = x.value(ctx)?;
            // a blank node of a literal is the one its label names in this query
            let mut found = false;
            for e in &l {
                match e {
                    Elem::Term(t @ Term::BlankNode(_)) => {
                        let tid = ctx.intern_term(t);
                        if ctx.has_memory_failure() {
                            return Err(TypeError);
                        }
                        if tid == id {
                            found = true;
                            break;
                        }
                    }
                    Elem::Term(t) => {
                        if equals(&val(t), &x).unwrap_or(false) {
                            found = true;
                            break;
                        }
                    }
                    Elem::Null => {}
                }
            }
            drop((l, x));
            drop(decode);
            Ok(Val::Id(crate::id::Id::from_bool(found)))
        }
        "containsTerm" => {
            arity(2, 2)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let l = list_arg(&term_of_into(ev(0)?, ctx, &decode)?)?;
            let id = ev(1)?.into_id(ctx);
            if ctx.has_memory_failure() {
                return Err(TypeError);
            }
            let mut found = false;
            for e in &l {
                if let Elem::Term(t) = e {
                    let tid = ctx.intern_term(t);
                    if ctx.has_memory_failure() {
                        return Err(TypeError);
                    }
                    if tid == id {
                        found = true;
                        break;
                    }
                }
            }
            drop(l);
            drop(decode);
            Ok(Val::Id(crate::id::Id::from_bool(found)))
        }
        "containsKey" => {
            arity(2, 2)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let m = map_arg(&term_of_into(ev(0)?, ctx, &decode)?)?;
            let key = match ev(1).and_then(|v| term_of_into(v, ctx, &decode)) {
                Ok(t) => key_of(t),
                Err(_) if ctx.has_memory_failure() => return Err(TypeError),
                Err(_) => None,
            };
            let found = key.is_some_and(|k| map_get(&m, &k).is_some());
            drop(m);
            drop(decode);
            Ok(Val::Id(crate::id::Id::from_bool(found)))
        }
        "get" => {
            arity(2, 2)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let t = term_of_into(ev(0)?, ctx, &decode)?;
            match &t {
                Term::Literal(l) if l.datatype().as_str() == LIST => {
                    let i = integer(&ev(1)?.value(ctx)?)?;
                    let l = parse_list(l.value()).ok_or(TypeError)?;
                    if i < 1 || i as usize > l.len() {
                        return Err(TypeError);
                    }
                    elem_val(&l[i as usize - 1], ctx)
                }
                Term::Literal(l) if l.datatype().as_str() == MAP => {
                    let k = key_of(term_of_into(ev(1)?, ctx, &decode)?).ok_or(TypeError)?;
                    let m = parse_map(l.value()).ok_or(TypeError)?;
                    elem_val(map_get(&m, &k).ok_or(TypeError)?, ctx)
                }
                _ => Err(TypeError),
            }
        }
        "head" => {
            arity(1, 1)?;
            elem_val(list_arg(&term(0)?)?.first().ok_or(TypeError)?, ctx)
        }
        "tail" => {
            arity(1, 1)?;
            let l = list_arg(&term(0)?)?;
            if l.is_empty() {
                return Err(TypeError);
            }
            list(&l[1..])
        }
        "keys" => {
            arity(1, 1)?;
            let m = map_arg(&term(0)?)?;
            let l: Vec<Elem> = m.into_iter().map(|(k, _)| Elem::Term(k)).collect();
            list(&l)
        }
        "merge" => {
            arity(2, 2)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let (v1, v2) = (ev(0)?, ev(1)?);
            let a = map_arg(&term_of_into(v1.clone(), ctx, &decode)?)?;
            let b = map_arg(&term_of_into(v2.clone(), ctx, &decode)?)?;
            if a.is_empty() {
                return Ok(v2);
            }
            if b.is_empty() {
                return Ok(v1);
            }
            // the first map's entries win
            let mut m = b;
            for (k, v) in a {
                map_put(&mut m, k, v);
            }
            map(&m)
        }
        "put" => {
            arity(2, 3)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let k = key_of(term_of_into(ev(1)?, ctx, &decode)?).ok_or(TypeError)?;
            let v1 = ev(0)?;
            let mut m = map_arg(&term_of_into(v1.clone(), ctx, &decode)?)?;
            let v = if n == 3 {
                match ev(2).and_then(|v| term_of_into(v, ctx, &decode)) {
                    Ok(t) => Elem::Term(t),
                    Err(_) if ctx.has_memory_failure() => return Err(TypeError),
                    Err(_) => Elem::Null,
                }
            } else {
                Elem::Null
            };
            if map_get(&m, &k) == Some(&v) {
                return Ok(v1);
            }
            map_put(&mut m, k, v);
            map(&m)
        }
        "remove" => {
            arity(2, 2)?;
            let decode = ctx.charge(0).map_err(|e| {
                ctx.fail_memory(&e);
                TypeError
            })?;
            let v1 = ev(0)?;
            let mut m = map_arg(&term_of_into(v1.clone(), ctx, &decode)?)?;
            let k = match ev(1).and_then(|v| term_of_into(v, ctx, &decode)) {
                Ok(t) => match key_of(t) {
                    Some(k) => k,
                    None => return Ok(v1),
                },
                Err(_) if ctx.has_memory_failure() => return Err(TypeError),
                Err(_) => return Ok(v1),
            };
            match m.binary_search_by(|(x, _)| key_cmp(x, &k)) {
                Ok(at) => {
                    m.remove(at);
                    map(&m)
                }
                Err(_) => Ok(v1),
            }
        }
        "reverse" => {
            arity(1, 1)?;
            let v = ev(0)?;
            let mut l = list_arg(&term_of(v.clone(), ctx)?)?;
            if l.len() < 2 {
                return Ok(v);
            }
            l.reverse();
            list(&l)
        }
        "size" => {
            arity(1, 1)?;
            let t = term(0)?;
            let size = match &t {
                Term::Literal(l) if l.datatype().as_str() == LIST => {
                    parse_list(l.value()).ok_or(TypeError)?.len()
                }
                Term::Literal(l) if l.datatype().as_str() == MAP => {
                    parse_map(l.value()).ok_or(TypeError)?.len()
                }
                _ => return Err(TypeError),
            };
            Ok(Val::V(Value::Integer((size as i64).into())))
        }
        "subseq" => {
            arity(2, 3)?;
            let v1 = ev(0)?;
            let t = term_of(v1.clone(), ctx)?;
            typed(&t, LIST)?;
            let index = integer(&ev(1)?.value(ctx)?)?;
            if index < 1 {
                return Err(TypeError);
            }
            let length = if n == 3 {
                let len = integer(&ev(2)?.value(ctx)?)?;
                if len < 0 {
                    return Err(TypeError);
                }
                Some(len)
            } else {
                None
            };
            let l = list_arg(&t)?;
            let size = l.len() as i64;
            let length = length.unwrap_or(size - index + 1);
            if index > size + 1 || index + length > size + 1 {
                return Err(TypeError);
            }
            if index == size + 1 {
                if length != 0 {
                    return Err(TypeError);
                }
                if l.is_empty() {
                    return Ok(v1);
                }
                return list(&[]);
            }
            let start = (index - 1) as usize;
            list(&l[start..start + length as usize])
        }
        _ => Err(TypeError),
    }
}

/// The elements of a `cdt:List` literal or the entries of a `cdt:Map` literal, for
/// UNFOLD: `(first, second)` per solution, `None` for an unbound variable. A list gives
/// each element (unbound for null) and its position from 1; a map gives each key and its
/// value (unbound for null). Any other value gives nothing to unfold.
pub fn unfold(t: &Term) -> Option<Vec<(Option<Term>, Option<Term>)>> {
    let Term::Literal(l) = t else {
        return None;
    };
    match l.datatype().as_str() {
        LIST => {
            let l = parse_list(l.value())?;
            Some(
                l.into_iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let pos = Term::Literal(Literal::new_typed_literal(
                            (i + 1).to_string(),
                            xsd::INTEGER,
                        ));
                        let e = match e {
                            Elem::Term(t) => Some(t),
                            Elem::Null => None,
                        };
                        (e, Some(pos))
                    })
                    .collect(),
            )
        }
        MAP => {
            let m = parse_map(l.value())?;
            Some(
                m.into_iter()
                    .map(|(k, v)| {
                        let v = match v {
                            Elem::Term(t) => Some(t),
                            Elem::Null => None,
                        };
                        (Some(k), v)
                    })
                    .collect(),
            )
        }
        _ => None,
    }
}

/// The value FOLD gives for no solutions: an empty list or map.
pub fn empty(map: bool) -> Value {
    if map { map_value(&[]) } else { list_value(&[]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_and_key_decode_charges_overlap() {
        use crate::store::{Store, StoreOptions};

        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        let key = "k".repeat(4096);
        let map = Term::Literal(Literal::new_typed_literal(
            format!(r#"{{"{key}" : 1}}"#),
            NamedNode::new_unchecked(MAP),
        ));
        let key = Term::Literal(Literal::new_simple_literal(key));
        let ids = [ctx.intern_term(&map), ctx.intern_term(&key)];
        let args = ids.map(Expr::Const);
        let table = super::super::table::Table::new(Vec::new());
        let row = Row {
            table: &table,
            i: 0,
            map: &[],
            dec: None,
            exists: None,
        };

        assert!(
            matches!(call("containsKey", &args, &row, &ctx), Ok(Val::Id(id)) if id == crate::id::Id::from_bool(true))
        );
        ctx.mem_limit = ctx.live_bytes() + 48 * 1024;
        for (id, expected) in ids.into_iter().zip([map, key]) {
            let credit = ctx.charge(0).unwrap();
            assert_eq!(ctx.term_checked_into(id, &credit).unwrap(), Some(expected));
        }
        assert!(call("containsKey", &args, &row, &ctx).is_err());
        assert!(
            matches!(ctx.check(), Err(crate::Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory)
        );
    }

    #[test]
    fn list_constructor_refuses_serialized_result_after_input_decode() {
        use crate::store::{Store, StoreOptions};

        let store = Store::in_memory(StoreOptions::default());
        let term = Term::Literal(Literal::new_simple_literal("\"".repeat(4096)));
        let table = super::super::table::Table::new(Vec::new());
        let row = Row {
            table: &table,
            i: 0,
            map: &[],
            dec: None,
            exists: None,
        };
        let control = Ctx::new(store.snapshot());
        let id = control.intern_term(&term);
        let Ok(Val::V(Value::Other { lex, .. })) = call("List", &[Expr::Const(id)], &row, &control)
        else {
            panic!("control constructor must serialize its argument");
        };
        let output_bytes = 8 * lex.len() as u64 + 1024;
        let mut target = Ctx::new(store.snapshot());
        let id = target.intern_term(&term);
        let baseline = target.live_bytes();
        target.mem_limit = baseline + 80 * 1024;
        let input_bytes = {
            let credit = target.charge(0).unwrap();
            assert_eq!(target.term_checked_into(id, &credit).unwrap(), Some(term));
            credit.bytes()
        };
        assert!(output_bytes < target.mem_limit - baseline);
        assert!(call("List", &[Expr::Const(id)], &row, &target).is_err());
        assert!(matches!(
            target.check(),
            Err(crate::Error::BudgetExceeded(b))
                if b.kind == crate::BudgetKind::Memory && b.requested == baseline + input_bytes + output_bytes
        ));
    }

    fn lst(s: &str) -> Vec<Elem> {
        parse_list(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn parses_and_writes_lists() {
        let l = lst(
            r#"[1, "a"@en, <http://x>, _:b1, null, [2, 3], {"k" : 1.5}, true, 2.0e0, "x"^^<http://dt>]"#,
        );
        assert_eq!(l.len(), 10);
        assert_eq!(
            list_lexical(&l),
            r#"[1, "a"@en, <http://x>, _:b1, null, [2, 3], {"k" : 1.5}, true, 2.0e0, "x"^^<http://dt>]"#
        );
        assert_eq!(list_lexical(&lst("[]")), "[]");
        assert_eq!(list_lexical(&lst(" [ 'a' , '''b''' ] ")), r#"["a", "b"]"#);
        assert_eq!(list_lexical(&lst(r#"["a\"b\n"]"#)), r#"["a\"b\n"]"#);
        // Turtle has no decimal `1.`, so it is written in full
        assert_eq!(
            list_lexical(&lst("[1., .5, -2, +3, 1e3]")),
            r#"["1."^^<http://www.w3.org/2001/XMLSchema#decimal>, .5, -2, +3, 1e3]"#
        );
        for bad in [
            "[",
            "[1,]",
            "[1 2]",
            "[nul]",
            "1",
            "[1] x",
            "[\"a\" ^^ <x>]",
        ] {
            assert!(parse_list(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn parses_maps_in_key_order() {
        let m = parse_map(r#"{"b" : 1, <http://x> : null, 1 : "one"}"#).unwrap();
        assert_eq!(
            map_lexical(&m),
            r#"{<http://x> : null, 1 : "one", "b" : 1}"#
        );
        assert!(parse_map(r#"{"a" : 1, "a" : 2}"#).is_none());
        assert!(parse_map("{_:b : 1}").is_none());
        assert!(parse_map("{null : 1}").is_none());
    }

    #[test]
    fn relabels_blank_nodes_in_nested_literals() {
        let relabel = |lex: &str, dt: &str| {
            let l = Literal::new_typed_literal(lex, NamedNode::new_unchecked(dt));
            relabel_literal(&l, &mut |b| format!("x{b}")).map(|l| l.value().to_string())
        };
        assert_eq!(
            relabel(
                &format!("[_:a, [_:b], '[_:a]'^^<{LIST}>, \"_:c\", {{'k': _:c}}]"),
                LIST
            )
            .as_deref(),
            Some(r#"[_:xa, [_:xb], [_:xa], "_:c", {"k" : _:xc}]"#)
        );
        assert_eq!(
            relabel(&format!("{{'k': \"{{'j': _:a}}\"^^<{MAP}>}}"), MAP).as_deref(),
            Some(r#"{"k" : {"j" : _:xa}}"#)
        );
        // no blank node, an ill-formed literal and another datatype stay as they are
        assert_eq!(relabel("[\"_:a\"]", LIST), None);
        assert_eq!(relabel("[_:a", LIST), None);
        assert_eq!(relabel("[_:a]", "http://example/List"), None);
    }

    #[test]
    fn scans_bnode_ids_iteratively_across_strings_iris_and_quoted_cdts() {
        let scan_dt = |lex: &str, dt: &str| -> Vec<u64> {
            let mut ids = Vec::new();
            scan_bnode_ids(
                lex,
                dt,
                || Ok(()),
                |_| true,
                |id| {
                    ids.push(id.payload());
                    Ok(())
                },
            )
            .unwrap();
            ids
        };
        let scan = |lex: &str| scan_dt(lex, LIST);

        // Direct blank nodes, skipping strings, IRIs, query-local labels, non-stored labels,
        // and dotted labels inside a valid list.
        assert_eq!(
            scan(
                r#"[_:b0, "_:b1", '_:b2', """_:b3""", '''_:b4''', <urn:_:b5>, _:q6, _:foo, _:b7.1, _:ba]"#
            ),
            vec![0, 0xa]
        );

        // Unescaped, escaped, unicode-escaped (including leading '+' accepted by
        // u32::from_str_radix in Parser::string), map-key, and doubly-nested quoted CDT literals.
        let nested = format!(
            concat!(
                "[",
                "'[_:b1]'^^<{L}>, ",
                "'''{{\"k\": _:b2}}'''^^<{M}>, ",
                "\"[\\\"_:b99\\\", _:b3]\"^^<{L}>, ",
                "\"[\\u005F:b4]\"^^<{L}>, ",
                "{{\"[_:b5]\"^^<{L}> : 1}}, ",
                "\"[\\\"[_:b6]\\\"^^<{L}>]\"^^<{L}>, ",
                "\"[_:\\u+0627]\"^^<{L}>, ",
                "\"[_:\\U+00000628]\"^^<{L}>, ",
                "\"[_:b77]\"^^<http://www.w3.org/2001/XMLSchema#string>",
                "]"
            ),
            L = LIST,
            M = MAP,
        );
        assert_eq!(scan(&nested), vec![1, 2, 3, 4, 5, 6, 7, 8]);

        // Ill-typed CDT literals must visit zero blank nodes (matching parse_list/parse_map).
        for (lex, dt) in [
            ("[_:b7ffffffffff,]", LIST),
            ("[_:b0, foo_:b8]", LIST),
            ("[_:b9.]", LIST),
            ("[_:b0", LIST),
            ("[_:b0] x", LIST),
            (r#"["a"@123, _:b0]"#, LIST),
            (r#"{"k": _:b0}"#, LIST),
            ("[_:b0]", MAP),
            ("{_:b0 : 1}", MAP),
            (r#"{"a": _:b0, "a": 1}"#, MAP),
            (r#"{"a": _:b0, "\u0061": 1}"#, MAP),
            (
                r#"{1: _:b0, "1"^^<http://www.w3.org/2001/XMLSchema#integer>: 2}"#,
                MAP,
            ),
            (r#"{"a"@EN: _:b0, "a"@en: 2}"#, MAP),
            (
                r#"{TRUE: _:b0, "true"^^<http://www.w3.org/2001/XMLSchema#boolean>: 2}"#,
                MAP,
            ),
            (&format!("[\"[_:b0]\"^^<{LIST}>,]"), LIST),
        ] {
            assert_eq!(scan_dt(lex, dt), Vec::<u64>::new(), "ill-typed: {lex}");
        }

        // Valid outer list containing a real blank node and an ill-typed quoted nested CDT
        // visits only the valid blank node.
        let mixed = format!("[_:b0, \"[_:b7ffffffffff,]\"^^<{LIST}>, _:b2]");
        assert_eq!(scan(&mixed), vec![0, 2]);

        // Large map exceeding INLINE_MAP_KEY_POOL (512) checks duplicates accurately.
        let mut wide_ok = String::from("{");
        for i in 0..550 {
            if i > 0 {
                wide_ok.push_str(", ");
            }
            wide_ok.push_str(&format!("\"k{i}\": {i}"));
        }
        wide_ok.push_str(", \"last\": _:b3}");
        assert_eq!(scan_dt(&wide_ok, MAP), vec![3]);
        let mut wide_dup = wide_ok[..wide_ok.len() - 1].to_string();
        wide_dup.push_str(", \"k0\": 99}");
        assert_eq!(scan_dt(&wide_dup, MAP), Vec::<u64>::new());

        // Deeply nested bare lists and maps (including >256 deep multi-entry map) do not
        // overflow a 2 MiB stack.
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let depth = 25_000;
                let mut deep = String::with_capacity(depth * 2 + 5);
                for _ in 0..depth {
                    deep.push('[');
                }
                deep.push_str("_:b2a");
                for _ in 0..depth {
                    deep.push(']');
                }
                let mut ids = Vec::new();
                scan_bnode_ids(
                    &deep,
                    LIST,
                    || Ok(()),
                    |_| true,
                    |id| {
                        ids.push(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(ids, vec![0x2a]);

                // 300-deep map with a duplicate key at the innermost level (> INLINE_MAP_FRAMES).
                let mut deep_dup_map = String::new();
                for _ in 0..300 {
                    deep_dup_map.push_str("{\"k\":");
                }
                deep_dup_map.push_str("{\"a\": _:b2a, \"a\": 1}");
                for _ in 0..300 {
                    deep_dup_map.push('}');
                }
                let mut dup_ids = Vec::new();
                scan_bnode_ids(
                    &deep_dup_map,
                    MAP,
                    || Ok(()),
                    |_| true,
                    |id| {
                        dup_ids.push(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert!(dup_ids.is_empty());
            })
            .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn streaming_langtag_validation_matches_oxilangtag_at_arbitrary_lengths_with_zero_heap() {
        let long_ext = format!("en-a{}", "-zz".repeat(90));
        let long_var = format!("en{}", "-1901".repeat(60));
        let long_prefix_priv = format!("en-u{}-x-priv1-priv2", "-aa".repeat(90));
        let long_ext_bad_subtag = format!("en-a{}-123456789", "-zz".repeat(90));
        let long_ext_empty_priv = format!("en-a{}-x", "-zz".repeat(90));
        let long_priv = format!("x{}", "-abcd1234".repeat(40));

        let mut tags: Vec<String> = GRANDFATHERED_LANGTAGS
            .iter()
            .flat_map(|g| {
                [
                    (*g).to_string(),
                    g.to_ascii_uppercase(),
                    g.to_ascii_lowercase(),
                ]
            })
            .collect();
        tags.extend(
            [
                "en",
                "EN-us",
                "zh-cmn-Hans-CN",
                "en-aaa-bbb-ccc",
                "en-aaa-bbb-ccc-ddd",
                "abcd-aaa",
                "abcd-Latn-419-1901-biske",
                "x-a",
                "X-12345678-z",
                "x",
                "en-x",
                "en-x-a",
                "en-a-bb-c-dd",
                "en-a-b",
                "en-a-x-foo",
                "en-1-2",
                "en-1-ab",
                "en-1-ab-2",
                "en-123456789",
                "a-bc",
                "i-klingon-extra",
                "cel-gaulish-x",
                "en-gb-oed-1901",
            ]
            .into_iter()
            .map(str::to_string),
        );
        tags.extend([
            long_ext.clone(),
            long_var.clone(),
            long_prefix_priv.clone(),
            long_ext_bad_subtag,
            long_ext_empty_priv,
            long_priv,
        ]);

        for tag in &tags {
            let lex = format!("[\"v\"@{tag}, _:b0]");
            let parser_ok = parse_list(&lex).is_some();
            let mut visited = Vec::new();
            scan_bnode_ids(
                &lex,
                LIST,
                || Ok(()),
                |_| true,
                |id| {
                    visited.push(id.payload());
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(
                !visited.is_empty(),
                parser_ok,
                "scanner/parser mismatch on language tag {tag:?}"
            );
            if parser_ok {
                assert_eq!(visited, vec![0]);
            }
        }

        // Verify zero heap allocation while validating short, grandfathered, and >256-byte tags.
        let zero_heap_lex = format!(
            "[\"a\"@en, \"b\"@en-GB-oed, \"c\"@{long_ext}, \"d\"@{long_var}, \"e\"@{long_prefix_priv}, _:b5]"
        );
        let mut seen = None;
        let largest = crate::test_alloc::record_largest(|| {
            scan_bnode_ids(
                &zero_heap_lex,
                LIST,
                || Ok(()),
                |_| true,
                |id| {
                    seen = Some(id.payload());
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(seen, Some(5));
        assert_eq!(
            largest, 0,
            "language-tagged CDT validation must perform zero heap allocations"
        );
    }

    #[test]
    fn bare_nesting_past_131072_and_window_boundaries_scans_with_zero_heap() {
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                // Within the inline stack windows (<= 65,536 bare containers and <= 256 maps),
                // validation performs zero heap allocations even when allow_mem permits heap.
                let inline_list = format!("{}_:b0{}", "[".repeat(65_536), "]".repeat(65_536));
                let mut seen = None;
                let inline_largest = crate::test_alloc::record_largest(|| {
                    scan_bnode_ids(
                        &inline_list,
                        LIST,
                        || Ok(()),
                        |_| true,
                        |id| {
                            seen = Some(id.payload());
                            Ok(())
                        },
                    )
                    .unwrap();
                });
                assert_eq!(seen, Some(0));
                assert_eq!(
                    inline_largest, 0,
                    "65_536-deep bare list validation must perform zero heap allocations"
                );

                // Past 131,072 bare nesting: validates cleanly with allow_mem = true (spilling
                // preflighted overflow_bits for O(1) push/pop) AND with allow_mem = false in
                // zero heap via the bounded sliding-window fallback.
                let depth = 131_073;
                let valid = format!("{}_:b0{}", "[".repeat(depth), "]".repeat(depth));
                let mut seen_spill = None;
                scan_bnode_ids(
                    &valid,
                    LIST,
                    || Ok(()),
                    |_| true,
                    |id| {
                        seen_spill = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(seen_spill, Some(0));

                let mut seen_tight = None;
                let tight_largest = crate::test_alloc::record_largest(|| {
                    scan_bnode_ids(
                        &valid,
                        LIST,
                        || Ok(()),
                        |_| false,
                        |id| {
                            seen_tight = Some(id.payload());
                            Ok(())
                        },
                    )
                    .unwrap();
                });
                assert_eq!(seen_tight, Some(0));
                assert_eq!(
                    tight_largest, 0,
                    "131_073-deep monotonic bare list must validate with zero heap when heap is refused"
                );

                // Trailing comma at depth 131,073 must skip cleanly as ill-typed rather than error.
                let ill_typed = format!("{}_:b0,{}", "[".repeat(depth), "]".repeat(depth));
                let mut ill_seen = false;
                scan_bnode_ids(
                    &ill_typed,
                    LIST,
                    || Ok(()),
                    |_| true,
                    |_| {
                        ill_seen = true;
                        Ok(())
                    },
                )
                .unwrap();
                assert!(!ill_seen);

                // Mixed list/map nesting crossing both CONTAINER_WINDOW_BITS (65,536) and
                // INLINE_MAP_FRAMES (256) validates cleanly both with heap spilling and with
                // zero-heap sliding-window fallback.
                let outer_lists = 66_000;
                let inner_maps = 300;
                let mut mixed = String::new();
                for _ in 0..outer_lists {
                    mixed.push('[');
                }
                for _ in 0..inner_maps {
                    mixed.push_str("{\"k\":");
                }
                mixed.push_str("_:b9");
                for _ in 0..inner_maps {
                    mixed.push('}');
                }
                for _ in 0..outer_lists {
                    mixed.push(']');
                }
                let mut mixed_seen = None;
                scan_bnode_ids(
                    &mixed,
                    LIST,
                    || Ok(()),
                    |_| true,
                    |id| {
                        mixed_seen = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(mixed_seen, Some(9));

                let mut mixed_tight_seen = None;
                let mixed_largest = crate::test_alloc::record_largest(|| {
                    scan_bnode_ids(
                        &mixed,
                        LIST,
                        || Ok(()),
                        |_| false,
                        |id| {
                            mixed_tight_seen = Some(id.payload());
                            Ok(())
                        },
                    )
                    .unwrap();
                });
                assert_eq!(mixed_tight_seen, Some(9));
                assert_eq!(mixed_largest, 0);
            })
            .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn deep_and_oscillating_maps_and_lists_validate_linearly_without_false_storage_full() {
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                // 1. 1,025 copies of a 257-deep 2-key map chain in one cdt:List must succeed
                //    linearly with allow_mem = true (never hitting a false StorageFull).
                let chain = format!("{}_:b0{}", "{\"a\":".repeat(257), ",\"b\":0}".repeat(257));
                let list_lex = format!("[{}]", vec![chain; 1_025].join(","));
                let mut list_ticks = 0usize;
                let mut list_visits = 0usize;
                scan_bnode_ids(
                    &list_lex,
                    LIST,
                    || {
                        list_ticks += 1;
                        Ok(())
                    },
                    |_| true,
                    |id| {
                        assert_eq!(id.payload(), 0);
                        list_visits += 1;
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(list_visits, 1_025);
                assert!(
                    list_ticks * 1024 <= list_lex.len() * 16,
                    "1,025x257-deep list took superlinear work: {} steps for {} bytes",
                    list_ticks * 1024,
                    list_lex.len()
                );

                // 2. Oscillating 128<->257 deep map (726 oscillating entries): valid must visit
                //    b0 and ill-typed trailing-comma variant must skip cleanly without StorageFull.
                let inner_129 = format!("{}0{}", "{0:".repeat(129), "}".repeat(129));
                let mut osc_valid = format!("{}{{", "{0:".repeat(127));
                for j in 0..726 {
                    use std::fmt::Write;
                    if j > 0 {
                        osc_valid.push(',');
                    }
                    let _ = write!(&mut osc_valid, "{j}:{inner_129}");
                }
                let mut osc_ill = osc_valid.clone();
                osc_valid.push_str(",999:_:b0");
                osc_valid.push_str(&"}".repeat(128));
                osc_ill.push_str(",999:_:b0,");
                osc_ill.push_str(&"}".repeat(128));

                let mut osc_ticks = 0usize;
                let mut osc_seen = None;
                scan_bnode_ids(
                    &osc_valid,
                    MAP,
                    || {
                        osc_ticks += 1;
                        Ok(())
                    },
                    |_| true,
                    |id| {
                        osc_seen = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(osc_seen, Some(0));
                assert!(
                    osc_ticks * 1024 <= osc_valid.len() * 16,
                    "oscillating 128<->257 map took superlinear work: {} steps for {} bytes",
                    osc_ticks * 1024,
                    osc_valid.len()
                );

                let mut osc_ill_seen = false;
                scan_bnode_ids(
                    &osc_ill,
                    MAP,
                    || Ok(()),
                    |_| true,
                    |_| {
                        osc_ill_seen = true;
                        Ok(())
                    },
                )
                .unwrap();
                assert!(!osc_ill_seen);

                // 3. 32,768-deep chain of 2-key maps (every ancestor gets a later key on unwind):
                //    linear pass under allow_mem = true; linearly bounded StorageFull under tight cap.
                let deep2 = format!(
                    "{}_:b0{}",
                    "{\"a\":".repeat(32_768),
                    ",\"b\":0}".repeat(32_768)
                );
                let mut deep2_ticks = 0usize;
                let mut deep2_seen = None;
                scan_bnode_ids(
                    &deep2,
                    MAP,
                    || {
                        deep2_ticks += 1;
                        Ok(())
                    },
                    |_| true,
                    |id| {
                        deep2_seen = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(deep2_seen, Some(0));
                assert!(
                    deep2_ticks * 1024 <= deep2.len() * 16,
                    "32,768-deep 2-key map took superlinear work: {} steps for {} bytes",
                    deep2_ticks * 1024,
                    deep2.len()
                );

                // Duplicate at the outermost ancestor of the 32,768-deep chain must be caught.
                let deep2_dup = format!(
                    "{}_:b0{},\"a\":0}}",
                    "{\"a\":".repeat(32_768),
                    ",\"b\":0}".repeat(32_767)
                );
                let mut deep2_dup_seen = false;
                scan_bnode_ids(
                    &deep2_dup,
                    MAP,
                    || Ok(()),
                    |_| true,
                    |_| {
                        deep2_dup_seen = true;
                        Ok(())
                    },
                )
                .unwrap();
                assert!(!deep2_dup_seen);

                let mut deep2_tight_ticks = 0usize;
                let deep2_tight = scan_bnode_ids(
                    &deep2,
                    MAP,
                    || {
                        deep2_tight_ticks += 1;
                        Ok(())
                    },
                    |_| false,
                    |_| Ok(()),
                );
                assert!(matches!(
                    deep2_tight,
                    Err(crate::error::Error::StorageFull(_))
                ));
                assert!(
                    deep2_tight_ticks * 1024 <= deep2.len() * 16 + 300_000,
                    "tight-cap 32,768-deep map exceeded linear work bound: {} steps for {} bytes",
                    deep2_tight_ticks * 1024,
                    deep2.len()
                );

                // 4. 2,000-deep chain of 3-key maps (fills key_pool and spills overflow frames +
                //    proportional 4-slot tables) must also stay strictly linear.
                let deep3 = format!(
                    "{}_:b3{}",
                    "{\"a\":1,\"b\":".repeat(2_000),
                    ",\"c\":2}".repeat(2_000)
                );
                let mut deep3_ticks = 0usize;
                let mut deep3_seen = None;
                scan_bnode_ids(
                    &deep3,
                    MAP,
                    || {
                        deep3_ticks += 1;
                        Ok(())
                    },
                    |_| true,
                    |id| {
                        deep3_seen = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(deep3_seen, Some(3));
                assert!(
                    deep3_ticks * 1024 <= deep3.len() * 16,
                    "2,000-deep 3-key map took superlinear work: {} steps for {} bytes",
                    deep3_ticks * 1024,
                    deep3.len()
                );

                // 5. ~1 MiB WideTable-outer oscillating 128<->257 deep map (513 scalar keys +
                //    2,000 oscillating 129-deep map entries) must run in a single linear pass.
                let mut wide_osc = format!("{}{{", "{0:".repeat(127));
                for j in 0..513 {
                    use std::fmt::Write;
                    if j > 0 {
                        wide_osc.push(',');
                    }
                    let _ = write!(&mut wide_osc, "{j}:0");
                }
                let wide_head = wide_osc.clone();
                for j in 513..2_513 {
                    use std::fmt::Write;
                    let _ = write!(&mut wide_osc, ",{j}:{inner_129}");
                }
                wide_osc.push_str(",9999:_:b5");
                wide_osc.push_str(&"}".repeat(128));

                let mut wide_osc_ticks = 0usize;
                let mut wide_osc_seen = None;
                scan_bnode_ids(
                    &wide_osc,
                    MAP,
                    || {
                        wide_osc_ticks += 1;
                        Ok(())
                    },
                    |_| true,
                    |id| {
                        wide_osc_seen = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(wide_osc_seen, Some(5));
                assert!(
                    wide_osc_ticks * 1024 <= wide_osc.len() * 16,
                    "WideTable oscillating deep map took superlinear work: {} steps for {} bytes",
                    wide_osc_ticks * 1024,
                    wide_osc.len()
                );

                // Admit the 8 KiB outer table and its directory. The added
                // 2 KiB frame page crosses the cap and forces a window refill.
                let cap = 9_500;
                let mut max_admitted = 0;
                let mut wide_osc_tight_ticks = 0usize;
                let wide_osc_tight = scan_bnode_ids(
                    &wide_osc,
                    MAP,
                    || {
                        wide_osc_tight_ticks += 1;
                        Ok(())
                    },
                    |bytes| {
                        if bytes <= cap {
                            max_admitted = max_admitted.max(bytes);
                        }
                        bytes <= cap
                    },
                    |_| Ok(()),
                );
                assert!(max_admitted >= 8_192);
                assert!(matches!(
                    wide_osc_tight,
                    Err(crate::error::Error::StorageFull(_))
                ));
                assert!(
                    wide_osc_tight_ticks * 1024 <= wide_osc.len() * 16 + 300_000,
                    "tight-cap WideTable oscillating map exceeded linear work bound: {} steps",
                    wide_osc_tight_ticks * 1024
                );

                for (key, expected) in [(9999, Some(5)), (512, None)] {
                    let short =
                        format!("{wide_head},513:{inner_129},{key}:_:b5{}", "}".repeat(128));
                    let mut seen = None;
                    let mut refused = false;
                    scan_bnode_ids(
                        &short,
                        MAP,
                        || Ok(()),
                        |bytes| {
                            refused |= bytes > cap;
                            bytes <= cap
                        },
                        |id| {
                            seen = Some(id.payload());
                            Ok(())
                        },
                    )
                    .unwrap();
                    assert!(refused);
                    assert_eq!(seen, expected);
                }

                // 6. Bare lists oscillating 32,768<->65,537 must stay linear under allow_mem = true
                //    and refuse linearly under allow_mem = false (no quadratic refill_bits).
                let spike = format!("{}0{}", "[".repeat(32_769), "]".repeat(32_769));
                let mut list_osc = "[".repeat(32_768);
                for i in 0..24 {
                    if i > 0 {
                        list_osc.push(',');
                    }
                    list_osc.push_str(&spike);
                }
                list_osc.push_str(",_:b6");
                list_osc.push_str(&"]".repeat(32_768));

                let mut list_osc_ticks = 0usize;
                let mut list_osc_seen = None;
                scan_bnode_ids(
                    &list_osc,
                    LIST,
                    || {
                        list_osc_ticks += 1;
                        Ok(())
                    },
                    |_| true,
                    |id| {
                        list_osc_seen = Some(id.payload());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(list_osc_seen, Some(6));
                assert!(
                    list_osc_ticks * 1024 <= list_osc.len() * 16,
                    "oscillating deep bare list took superlinear work: {} steps for {} bytes",
                    list_osc_ticks * 1024,
                    list_osc.len()
                );

                let mut list_osc_tight_ticks = 0usize;
                let list_osc_tight = scan_bnode_ids(
                    &list_osc,
                    LIST,
                    || {
                        list_osc_tight_ticks += 1;
                        Ok(())
                    },
                    |_| false,
                    |_| Ok(()),
                );
                assert!(matches!(
                    list_osc_tight,
                    Err(crate::error::Error::StorageFull(_))
                ));
                assert!(
                    list_osc_tight_ticks * 1024 <= list_osc.len() * 16 + 300_000,
                    "tight-cap oscillating deep list exceeded linear work bound: {} steps",
                    list_osc_tight_ticks * 1024
                );
            })
            .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn closed_bit_spill_leaves_room_for_later_map_spill() {
        let list = format!(
            "{}0{}",
            "[".repeat(CONTAINER_WINDOW_BITS),
            "]".repeat(CONTAINER_WINDOW_BITS)
        );
        let inner = format!("{}0{}", "{0:".repeat(129), "}".repeat(129));
        let entries: Vec<_> = (0..32).map(|key| format!("{key}:{inner}")).collect();
        let map = format!(
            "{}{{{},999:_:b0{}",
            "{0:".repeat(127),
            entries.join(","),
            "}".repeat(128)
        );
        let lexical = format!("[{list},{map}]");

        // Either first page fits alone. Retaining the list page's charge would
        // force the map into repeated root refills and exhaust the fallback budget.
        let mut saw_id = false;
        scan_bnode_ids(
            &lexical,
            LIST,
            || Ok(()),
            |bytes| bytes <= 2_200,
            |id| {
                assert_eq!(id.payload(), 0);
                saw_id = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(saw_id);
    }

    #[test]
    fn refused_map_spill_polls_during_frame_walk() {
        let mut stack = ContainerStack::new();
        let mut stream = CdtStream::new("");
        let open_maps = INLINE_MAP_FRAMES + 7 * OVERFLOW_MAP_PAGE_SMALL;
        for _ in 0..open_maps {
            assert!(
                stack
                    .push_map(0, &mut stream, &mut || Ok(()), &mut |_| true)
                    .unwrap()
            );
            stack.update_top_map_frame(|frame| {
                frame.mode = MapKeyMode::Single { start: 0, tag: 0 };
            });
        }
        assert_eq!(stream.steps, 0);
        let cap = stack.active_heap_bytes;

        let mut refused = false;
        let result = stack.push_map(
            0,
            &mut stream,
            &mut || Err(crate::error::Error::Cancelled),
            &mut |bytes| {
                assert!(bytes > cap);
                refused = true;
                false
            },
        );
        assert!(refused);
        assert!(matches!(result, Err(crate::error::Error::Cancelled)));
        assert_eq!(stream.fallback_steps, 1023);
        assert!(!stream.in_fallback);
    }

    #[test]
    fn map_refill_charges_only_live_table_directory_entries_and_boundary() {
        let mut stack = ContainerStack::new();
        let new_base = 2;
        stack.map_frame_base = new_base + INLINE_MAP_FRAMES_HALF;
        stack.map_depth = stack.map_frame_base;

        // Many closed siblings at a depth that will also contain a live table.
        let prefix = format!(
            "{}[{},{}",
            "{0:".repeat(new_base),
            "{},".repeat(1_000).trim_end_matches(','),
            "{0:".repeat(INLINE_MAP_FRAMES_HALF),
        );
        for depth in [1, new_base, new_base + 1, stack.map_depth] {
            let table =
                WideKeySet::new_preflighted(4, false, &mut stack.active_heap_bytes, &mut |_| true)
                    .unwrap();
            stack.overflow_tables.push((depth, table));
        }

        let mut stream = CdtStream::new(&prefix);
        stream.pos = prefix.len();
        stack
            .refill_map_frames(&mut stream, 0, prefix.len(), &mut || Ok(()))
            .unwrap();

        assert_eq!(stack.map_frame_base, new_base);
        assert_eq!(stream.fallback_steps, prefix.len() as u64 + 3);
        assert!(matches!(stack.map_frames[0].mode, MapKeyMode::WideTable));
        assert!(matches!(
            stack.map_frames[1].mode,
            MapKeyMode::BatchedFallbackEmpty
        ));
        assert!(matches!(stack.top_map_frame().mode, MapKeyMode::WideTable));
        assert_eq!(stream.pos, prefix.len());
    }

    #[test]
    fn wide_key_set_polls_cancellation_during_rehash_and_insertion() {
        // Direct rehash cancellation test: growing a 4,096-slot table polls `check`
        // inside the rehash loop and leaves `active_heap` intact on cancellation.
        let mut stream = CdtStream::new("");
        let mut active = 0u64;
        let mut table =
            WideKeySet::new_preflighted(4_096, false, &mut active, &mut |_| true).unwrap();
        let before_bytes = active;
        for i in 0..3_072usize {
            let tag = mix_tag(i as u64);
            table
                .insert_unique(&mut stream, i as u64, tag, i, &mut || Ok(()))
                .unwrap();
        }
        let mut rehash_checks = 0usize;
        let err = table.grow_preflighted(
            &mut stream,
            &mut active,
            &mut || {
                rehash_checks += 1;
                Err(crate::error::Error::Cancelled)
            },
            &mut |_| true,
        );
        assert!(matches!(err, Err(crate::error::Error::Cancelled)));
        assert_eq!(rehash_checks, 1);
        assert_eq!(
            active, before_bytes,
            "cancelled rehash must not mutate active_heap"
        );
        assert_eq!(table.capacity(), 4_096);

        // Fewer than 1,024 old slots cannot poll on their own. Rehashing
        // the colliding keys must poll inside the linear probe loop.
        for large in [false, true] {
            let mut stream = CdtStream::new("");
            let mut active = 0;
            let mut table =
                WideKeySet::new_preflighted(512, large, &mut active, &mut |_| true).unwrap();
            let hash = 42;
            for pos in 0..64 {
                table
                    .insert_unique(&mut stream, hash, mix_tag(hash), pos, &mut || Ok(()))
                    .unwrap();
            }
            stream.steps = 0;
            let before_bytes = active;
            let result = table.grow_preflighted(
                &mut stream,
                &mut active,
                &mut || Err(crate::error::Error::Cancelled),
                &mut |_| true,
            );
            assert!(matches!(result, Err(crate::error::Error::Cancelled)));
            assert_eq!(table.capacity(), 512);
            assert_eq!(table.len, 64);
            assert_eq!(active, before_bytes);
        }

        // Direct insertion probe-chain cancellation test: colliding probes poll `check`.
        let mut probe_stream = CdtStream::new("");
        let mut probe_active = 0u64;
        let mut probe_table =
            WideKeySet::new_preflighted(4_096, false, &mut probe_active, &mut |_| true).unwrap();
        for i in 0..1_500usize {
            probe_table
                .insert_unique(&mut probe_stream, 0, 42, i, &mut || Ok(()))
                .unwrap();
        }
        let mut insert_checks = 0usize;
        let probe_err = probe_table.insert_unique(&mut probe_stream, 0, 42, 1_500, &mut || {
            insert_checks += 1;
            Err(crate::error::Error::Cancelled)
        });
        assert!(matches!(probe_err, Err(crate::error::Error::Cancelled)));
        assert_eq!(insert_checks, 1);

        // End the input at an invalid value immediately after the key that
        // triggers growth. Parsing this suffix cannot account for two polls
        // after preflight, even if the first falls just after rehash.
        let mut wide = String::from("{");
        for i in 0..3_073 {
            use std::fmt::Write;
            if i > 0 {
                wide.push(',');
            }
            let _ = write!(&mut wide, "\"k{i:05}\":");
            wide.push(if i == 3_072 { ']' } else { '0' });
        }
        wide.push('}');
        let growth_peak = WideKeySet::bytes_for(4_096, false)
            + WideKeySet::bytes_for(8_192, false)
            + 4 * std::mem::size_of::<(usize, WideKeySet)>() as u64;
        let growing = std::cell::Cell::new(false);
        let mut checks_after_preflight = 0;
        let res = scan_bnode_ids(
            &wide,
            MAP,
            || {
                if growing.get() {
                    checks_after_preflight += 1;
                    if checks_after_preflight == 2 {
                        return Err(crate::error::Error::Cancelled);
                    }
                }
                Ok(())
            },
            |bytes| {
                if bytes == growth_peak {
                    assert!(!growing.replace(true));
                }
                true
            },
            |_| Ok(()),
        );
        assert!(growing.get());
        assert!(matches!(res, Err(crate::error::Error::Cancelled)));
        assert_eq!(checks_after_preflight, 2);
    }

    #[test]
    fn narrow_maps_under_full_ancestor_pool_avoid_wide_table_allocation() {
        // Fill all 512 slots of the shared inline key pool in the outer map.
        let mut prefix = String::from("{");
        for i in 0..511 {
            use std::fmt::Write;
            if i > 0 {
                prefix.push(',');
            }
            let _ = write!(&mut prefix, "\"o{i}\":{i}");
        }
        prefix.push_str(",\"o511\":");

        // 1. A single-key inner map at the 512th outer entry must allocate ZERO heap.
        let one_key = format!("{prefix}{{\"inner\":_:b1}}}}");
        let mut seen_one = None;
        let one_largest = crate::test_alloc::record_largest(|| {
            scan_bnode_ids(
                &one_key,
                MAP,
                || Ok(()),
                |_| true,
                |id| {
                    seen_one = Some(id.payload());
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(seen_one, Some(1));
        assert_eq!(
            one_largest, 0,
            "one-key map under a full 512-key ancestor pool must allocate zero heap"
        );

        // 2. A 200-deep chain of one-key maps at that position must also allocate ZERO heap.
        let chain_one = format!(
            "{prefix}{}_:b2{}}}",
            "{\"inner\":".repeat(200),
            "}".repeat(200)
        );
        let mut seen_chain = None;
        let chain_largest = crate::test_alloc::record_largest(|| {
            scan_bnode_ids(
                &chain_one,
                MAP,
                || Ok(()),
                |_| true,
                |id| {
                    seen_chain = Some(id.payload());
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(seen_chain, Some(2));
        assert_eq!(
            chain_largest, 0,
            "chain of one-key maps under a full 512-key ancestor pool must allocate zero heap"
        );

        // 3. A two-key inner map under a full ancestor pool spills proportionally
        //    (4-slot table = 32 B + 4-entry directory), never an 8 KiB 1,024-slot table.
        let two_key = format!("{prefix}{{\"x\":1,\"y\":_:b3}}}}");
        let mut seen_two = None;
        let two_largest = crate::test_alloc::record_largest(|| {
            scan_bnode_ids(
                &two_key,
                MAP,
                || Ok(()),
                |_| true,
                |id| {
                    seen_two = Some(id.payload());
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(seen_two, Some(3));
        assert!(
            two_largest <= 512,
            "two-key map spill must be proportional to its key count, got {two_largest} bytes"
        );

        for key in ["x", "y"] {
            let duplicate = format!("{prefix}{{\"x\":1,\"y\":2,\"{key}\":_:b3}}}}");
            let largest = crate::test_alloc::record_largest(|| {
                scan_bnode_ids(
                    &duplicate,
                    MAP,
                    || Ok(()),
                    |_| true,
                    |_| panic!("a duplicate key must invalidate the map"),
                )
                .unwrap();
            });
            assert!((1..=512).contains(&largest));
        }
    }

    #[test]
    fn wide_and_deep_maps_validate_linearly_with_cancellation_and_memory_bounds() {
        // 50,000-key valid map and duplicate map.
        let mut wide = String::with_capacity(50_000 * 14);
        wide.push('{');
        for i in 0..50_000 {
            if i > 0 {
                wide.push(',');
            }
            use std::fmt::Write;
            let _ = write!(&mut wide, "\"k{i:05}\":{i}");
        }
        wide.push_str(",\"last\":_:b0}");
        let mut ids = Vec::new();
        scan_bnode_ids(
            &wide,
            MAP,
            || Ok(()),
            |_| true,
            |id| {
                ids.push(id.payload());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(ids, vec![0]);

        let mut wide_dup = wide[..wide.len() - 1].to_string();
        wide_dup.push_str(",\"k00000\":1}");
        let mut dup_ids = Vec::new();
        scan_bnode_ids(
            &wide_dup,
            MAP,
            || Ok(()),
            |_| true,
            |id| {
                dup_ids.push(id.payload());
                Ok(())
            },
        )
        .unwrap();
        assert!(dup_ids.is_empty());

        // Cancellation inside a wide map stops promptly.
        let mut ticks = 0usize;
        let cancelled = scan_bnode_ids(
            &wide,
            MAP,
            || {
                ticks += 1;
                if ticks >= 2 {
                    Err(crate::error::Error::Cancelled)
                } else {
                    Ok(())
                }
            },
            |_| true,
            |_| Ok(()),
        );
        assert!(matches!(cancelled, Err(crate::error::Error::Cancelled)));

        // When allow_mem refuses all heap allocations:
        // - a 600-key map validates in zero heap via bounded batched fallback,
        // - a 50,000-key map refuses promptly with StorageFull instead of going quadratic.
        let mut mid_map = String::from("{");
        for i in 0..600 {
            if i > 0 {
                mid_map.push(',');
            }
            use std::fmt::Write;
            let _ = write!(&mut mid_map, "\"m{i}\":{i}");
        }
        mid_map.push_str(",\"last\":_:b4}");
        let mut mid_seen = None;
        let mid_largest = crate::test_alloc::record_largest(|| {
            scan_bnode_ids(
                &mid_map,
                MAP,
                || Ok(()),
                |_| false,
                |id| {
                    mid_seen = Some(id.payload());
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(mid_seen, Some(4));
        assert_eq!(mid_largest, 0);

        let mut mid_dup = mid_map[..mid_map.len() - 1].to_string();
        mid_dup.push_str(",\"m0\":99}");
        let mut mid_dup_seen = false;
        scan_bnode_ids(
            &mid_dup,
            MAP,
            || Ok(()),
            |_| false,
            |_| {
                mid_dup_seen = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(!mid_dup_seen);

        let refused_wide = scan_bnode_ids(&wide, MAP, || Ok(()), |_| false, |_| Ok(()));
        assert!(matches!(
            refused_wide,
            Err(crate::error::Error::StorageFull(_))
        ));

        // 300-deep nested map where the outermost map receives 600 additional keys
        // after popping back from >INLINE_MAP_FRAMES nesting (exercises shift + refill
        // + try_promote_batched_to_wide).
        let mut deep_then_wide = String::from("{\"k0\":");
        for _ in 0..300 {
            deep_then_wide.push_str("{\"in\":");
        }
        deep_then_wide.push('0');
        for _ in 0..300 {
            deep_then_wide.push('}');
        }
        for i in 1..600 {
            use std::fmt::Write;
            let _ = write!(&mut deep_then_wide, ",\"k{i}\":{i}");
        }
        deep_then_wide.push_str(",\"last\":_:b7}");
        let mut dtw_ids = Vec::new();
        scan_bnode_ids(
            &deep_then_wide,
            MAP,
            || Ok(()),
            |_| true,
            |id| {
                dtw_ids.push(id.payload());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(dtw_ids, vec![7]);

        let mut dtw_dup = deep_then_wide[..deep_then_wide.len() - 1].to_string();
        dtw_dup.push_str(",\"k0\":1}");
        let mut dtw_dup_ids = Vec::new();
        scan_bnode_ids(
            &dtw_dup,
            MAP,
            || Ok(()),
            |_| true,
            |id| {
                dtw_dup_ids.push(id.payload());
                Ok(())
            },
        )
        .unwrap();
        assert!(dtw_dup_ids.is_empty());
    }

    #[test]
    fn equality_and_order() {
        let eq = |a: &str, b: &str| equals_cdt(a, LIST, b, LIST);
        assert_eq!(eq("[1]", "[1.0]"), Ok(true));
        assert_eq!(eq("[1]", "[1e0]"), Ok(false));
        assert_eq!(eq("[null]", "[null]"), Ok(true));
        assert_eq!(eq("[1, [2]]", "[1,[2]]"), Ok(true));
        assert_eq!(eq("[_:a]", "[_:b]"), Err(TypeError));
        let lt = |a: &str, b: &str| compare_cdt(a, b, LIST, false);
        assert_eq!(lt("[1]", "[2]"), Ok(Ordering::Less));
        assert_eq!(lt("[1, null]", "[2]"), Ok(Ordering::Less));
        assert_eq!(lt("[]", "[1]"), Ok(Ordering::Less));
        assert_eq!(lt("[1]", "[1, 2]"), Ok(Ordering::Less));
        assert_eq!(lt("[null]", "[1]"), Err(TypeError));
        assert_eq!(lt("[<http://a>]", "[<http://b>]"), Err(TypeError));
        let sort = |a: &str, b: &str| compare_cdt(a, b, LIST, true);
        assert_eq!(sort("[null]", "[1]"), Ok(Ordering::Less));
        assert_eq!(sort("[<http://a>]", "[<http://b>]"), Ok(Ordering::Less));
    }
}
