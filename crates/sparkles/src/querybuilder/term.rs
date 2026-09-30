//! RDF terms, variables and property paths as used by the query builders.
//!
//! A [`Node`] is anything that can stand in a triple-pattern position (or as an
//! expression operand): a variable, an IRI, a prefixed name, a blank node, a
//! literal, a number, a boolean, an RDF 1.2 triple term or (in predicate position
//! only) a property path. Nodes are created from Rust values through [`IntoNode`]:
//!
//! * `&str` / `String` are parsed as SPARQL term syntax: `?x` / `$x`, `<iri>`,
//!   `prefix:local`, `a`, `_:label`, `[]`, quoted literals with optional `@lang` or
//!   `^^datatype`, numbers, `true` / `false`, `UNDEF`, or a property path such as
//!   `foaf:knows+` or `^ex:p/ex:q`.
//! * The helpers [`var`], [`iri`], [`lit`], [`lit_lang`], [`lit_typed`] build nodes from
//!   *data* rather than syntax: their input is always escaped, so untrusted strings can
//!   never break out of the term.
//! * `oxrdf` terms, `i32`/`i64`/…, `f32`/`f64` and `bool` convert directly.
//!
//! Invalid input does not panic: the node remembers the problem and
//! [`build()`](super::SelectBuilder::build) reports it.

use std::fmt;
use std::sync::LazyLock;

use oxrdf::{
    BlankNode, BlankNodeRef, GraphName, Literal, LiteralRef, NamedNode, NamedNodeRef,
    NamedOrBlankNode, Term, Triple, Variable, VariableRef,
};

pub(crate) const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_DOUBLE: &str = "http://www.w3.org/2001/XMLSchema#double";
const XSD_FLOAT: &str = "http://www.w3.org/2001/XMLSchema#float";

/// A term, variable or property path in a query being built.
///
/// Build nodes with [`var`], [`iri`], [`lit`], [`lit_lang`], [`lit_typed`], [`undef`],
/// [`node`] or any [`IntoNode`] conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct Node(pub(crate) N);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum N {
    Var(String),
    /// IRI value (unescaped).
    Iri(String),
    Prefixed(String, String),
    Blank(String),
    Anon,
    /// The `a` keyword (rdf:type).
    A,
    Literal {
        value: String,
        lang: Option<String>,
        datatype: Option<Box<Node>>,
    },
    /// A numeric literal token in SPARQL syntax (validated).
    Number(String),
    Bool(bool),
    TripleTerm(Box<[Node; 3]>),
    /// Property path syntax (validated token by token).
    Path(String),
    Undef,
    Invalid(String),
}

impl Node {
    pub(crate) fn invalid(msg: impl Into<String>) -> Node {
        Node(N::Invalid(msg.into()))
    }

    /// Is this a variable?
    pub fn is_var(&self) -> bool {
        matches!(self.0, N::Var(_))
    }

    /// The variable name (without `?`), if this is a variable.
    pub fn var_name(&self) -> Option<&str> {
        match &self.0 {
            N::Var(v) => Some(v),
            _ => None,
        }
    }

    /// The problem with this node, if it could not be parsed / converted.
    pub fn error(&self) -> Option<&str> {
        match &self.0 {
            N::Invalid(e) => Some(e),
            _ => None,
        }
    }
}

/// Renders the node in SPARQL syntax (prefixed names are not expanded).
impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            N::Var(v) => write!(f, "?{v}"),
            N::Iri(i) => write_iri(f, i),
            N::Prefixed(p, l) => write!(f, "{p}:{l}"),
            N::Blank(b) => write!(f, "_:{b}"),
            N::Anon => f.write_str("[]"),
            N::A => f.write_str("a"),
            N::Literal {
                value,
                lang,
                datatype,
            } => {
                write_string_literal(f, value)?;
                if let Some(l) = lang {
                    write!(f, "@{l}")?;
                } else if let Some(dt) = datatype {
                    write!(f, "^^{dt}")?;
                }
                Ok(())
            }
            N::Number(n) => f.write_str(n),
            N::Bool(b) => write!(f, "{b}"),
            N::TripleTerm(t) => write!(f, "<<( {} {} {} )>>", t[0], t[1], t[2]),
            N::Path(p) => f.write_str(p),
            N::Undef => f.write_str("UNDEF"),
            N::Invalid(e) => write!(f, "<<invalid: {e}>>"),
        }
    }
}

/// Writes `<iri>`, escaping every character that is not allowed raw in an `IRIREF`
/// as `\uXXXX` (the SPARQL parser then validates the unescaped IRI).
pub(crate) fn write_iri(f: &mut impl fmt::Write, iri: &str) -> fmt::Result {
    f.write_char('<')?;
    for c in iri.chars() {
        if matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\') || c <= ' ' {
            write!(f, "\\u{:04X}", c as u32)?;
        } else {
            f.write_char(c)?;
        }
    }
    f.write_char('>')
}

/// Writes a double-quoted string literal with every special character escaped, so the
/// value can never terminate the literal early.
pub(crate) fn write_string_literal(f: &mut impl fmt::Write, s: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            '\u{8}' => f.write_str("\\b")?,
            '\u{c}' => f.write_str("\\f")?,
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

// ------------------------------------------------------------------ constructors ----

/// A variable. The leading `?` / `$` is optional: `var("x")` and `var("?x")` are the same.
pub fn var(name: impl AsRef<str>) -> Node {
    let name = name.as_ref();
    let bare = name
        .strip_prefix('?')
        .or_else(|| name.strip_prefix('$'))
        .unwrap_or(name);
    if is_varname(bare) {
        Node(N::Var(bare.to_string()))
    } else {
        Node::invalid(format!("invalid variable name {name:?}"))
    }
}

/// An IRI from its value (without angle brackets; `<...>` is accepted and stripped).
///
/// Characters that may not appear raw in SPARQL IRIs are `\u`-escaped, so the value can
/// not break out of the IRI; an IRI that is invalid after unescaping is reported by
/// `build()`.
pub fn iri(iri: impl AsRef<str>) -> Node {
    let s = iri.as_ref();
    let s = s
        .strip_prefix('<')
        .and_then(|x| x.strip_suffix('>'))
        .unwrap_or(s);
    Node(N::Iri(s.to_string()))
}

/// A simple (`xsd:string`) literal. The value is escaped; any string is safe.
pub fn lit(value: impl AsRef<str>) -> Node {
    Node(N::Literal {
        value: value.as_ref().to_string(),
        lang: None,
        datatype: None,
    })
}

/// A language-tagged literal (`"chat"@fr`); `lang` may carry a base direction (`ar--rtl`).
pub fn lit_lang(value: impl AsRef<str>, lang: impl AsRef<str>) -> Node {
    let lang = lang.as_ref();
    if !LANGTAG.is_match(lang) {
        return Node::invalid(format!("invalid language tag {lang:?}"));
    }
    Node(N::Literal {
        value: value.as_ref().to_string(),
        lang: Some(lang.to_string()),
        datatype: None,
    })
}

/// A typed literal; the datatype is any IRI node (`iri(..)`, `"xsd:date"`, `NamedNode`).
pub fn lit_typed(value: impl AsRef<str>, datatype: impl IntoNode) -> Node {
    let dt = datatype.into_node();
    match &dt.0 {
        N::Iri(_) | N::Prefixed(..) => Node(N::Literal {
            value: value.as_ref().to_string(),
            lang: None,
            datatype: Some(Box::new(dt)),
        }),
        N::Invalid(_) => dt,
        _ => Node::invalid(format!("literal datatype must be an IRI, got {dt}")),
    }
}

/// `UNDEF`, for [`VALUES`](super::WhereBuilder::values_rows) rows.
pub fn undef() -> Node {
    Node(N::Undef)
}

/// Any [`IntoNode`] value as a [`Node`] (handy for heterogeneous `VALUES` rows).
pub fn node(n: impl IntoNode) -> Node {
    n.into_node()
}

/// An RDF 1.2 triple term `<<( s p o )>>`.
pub fn triple_term(s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Node {
    Node(N::TripleTerm(Box::new([
        s.into_node(),
        p.into_node(),
        o.into_node(),
    ])))
}

// ------------------------------------------------------------------- IntoNode ----

/// Conversion into a [`Node`]. Strings are parsed as SPARQL term (or path) syntax;
/// `oxrdf` terms and Rust scalars convert directly.
pub trait IntoNode {
    fn into_node(self) -> Node;
}

impl IntoNode for Node {
    fn into_node(self) -> Node {
        self
    }
}
impl IntoNode for &Node {
    fn into_node(self) -> Node {
        self.clone()
    }
}
impl IntoNode for &str {
    fn into_node(self) -> Node {
        parse_term(self)
    }
}
impl IntoNode for String {
    fn into_node(self) -> Node {
        parse_term(&self)
    }
}
impl IntoNode for &String {
    fn into_node(self) -> Node {
        parse_term(self)
    }
}
impl<T: IntoNode> IntoNode for Option<T> {
    /// `None` is `UNDEF`.
    fn into_node(self) -> Node {
        match self {
            Some(t) => t.into_node(),
            None => undef(),
        }
    }
}
impl IntoNode for NamedNodeRef<'_> {
    fn into_node(self) -> Node {
        Node(N::Iri(self.as_str().to_string()))
    }
}
impl IntoNode for &NamedNode {
    fn into_node(self) -> Node {
        self.as_ref().into_node()
    }
}
impl IntoNode for NamedNode {
    fn into_node(self) -> Node {
        Node(N::Iri(self.into_string()))
    }
}
impl IntoNode for BlankNodeRef<'_> {
    fn into_node(self) -> Node {
        Node(N::Blank(self.as_str().to_string()))
    }
}
impl IntoNode for &BlankNode {
    fn into_node(self) -> Node {
        self.as_ref().into_node()
    }
}
impl IntoNode for BlankNode {
    fn into_node(self) -> Node {
        self.as_ref().into_node()
    }
}
impl IntoNode for LiteralRef<'_> {
    fn into_node(self) -> Node {
        let lang = self.language().map(|l| match self.direction() {
            Some(d) => format!("{l}--{d}"),
            None => l.to_string(),
        });
        let datatype = if lang.is_some() || self.datatype() == oxrdf::vocab::xsd::STRING {
            None
        } else {
            Some(Box::new(self.datatype().into_node()))
        };
        Node(N::Literal {
            value: self.value().to_string(),
            lang,
            datatype,
        })
    }
}
impl IntoNode for &Literal {
    fn into_node(self) -> Node {
        self.as_ref().into_node()
    }
}
impl IntoNode for Literal {
    fn into_node(self) -> Node {
        self.as_ref().into_node()
    }
}
impl IntoNode for &NamedOrBlankNode {
    fn into_node(self) -> Node {
        match self {
            NamedOrBlankNode::NamedNode(n) => n.into_node(),
            NamedOrBlankNode::BlankNode(b) => b.into_node(),
        }
    }
}
impl IntoNode for NamedOrBlankNode {
    fn into_node(self) -> Node {
        (&self).into_node()
    }
}
impl IntoNode for &Triple {
    fn into_node(self) -> Node {
        triple_term(&self.subject, &self.predicate, &self.object)
    }
}
impl IntoNode for Triple {
    fn into_node(self) -> Node {
        (&self).into_node()
    }
}
impl IntoNode for &Term {
    fn into_node(self) -> Node {
        match self {
            Term::NamedNode(n) => n.into_node(),
            Term::BlankNode(b) => b.into_node(),
            Term::Literal(l) => l.into_node(),
            Term::Triple(t) => t.as_ref().into_node(),
        }
    }
}
impl IntoNode for Term {
    fn into_node(self) -> Node {
        (&self).into_node()
    }
}
impl IntoNode for &GraphName {
    /// The default graph cannot be named; it converts to an invalid node.
    fn into_node(self) -> Node {
        match self {
            GraphName::NamedNode(n) => n.into_node(),
            GraphName::BlankNode(b) => b.into_node(),
            GraphName::DefaultGraph => Node::invalid("the default graph has no name"),
        }
    }
}
impl IntoNode for VariableRef<'_> {
    fn into_node(self) -> Node {
        var(self.as_str())
    }
}
impl IntoNode for &Variable {
    fn into_node(self) -> Node {
        var(self.as_str())
    }
}
impl IntoNode for Variable {
    fn into_node(self) -> Node {
        var(self.as_str())
    }
}
impl IntoNode for bool {
    fn into_node(self) -> Node {
        Node(N::Bool(self))
    }
}

macro_rules! int_into_node {
    ($($t:ty),*) => {$(
        impl IntoNode for $t {
            fn into_node(self) -> Node {
                Node(N::Number(self.to_string()))
            }
        }
    )*};
}
int_into_node!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

fn float_node(v: f64, dt: &str, display: String) -> Node {
    if v.is_nan() {
        lit_typed("NaN", iri(dt))
    } else if v.is_infinite() {
        lit_typed(if v > 0.0 { "INF" } else { "-INF" }, iri(dt))
    } else if dt == XSD_DOUBLE {
        // `1.5e0` is a SPARQL DOUBLE token.
        Node(N::Number(display))
    } else {
        lit_typed(display, iri(dt))
    }
}
impl IntoNode for f64 {
    /// An `xsd:double` (`1.5e0`; `NaN` / `INF` as typed literals).
    fn into_node(self) -> Node {
        float_node(self, XSD_DOUBLE, format!("{self:e}"))
    }
}
impl IntoNode for f32 {
    /// An `xsd:float` typed literal.
    fn into_node(self) -> Node {
        float_node(f64::from(self), XSD_FLOAT, format!("{self:e}"))
    }
}

// ------------------------------------------------------------------ char classes ----

pub(crate) fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}
fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}
pub(crate) fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || matches!(c, '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}
fn is_varname_char(c: char) -> bool {
    is_pn_chars_u(c)
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || matches!(c, '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}
pub(crate) fn is_varname(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if is_pn_chars_u(c) || c.is_ascii_digit() => cs.all(is_varname_char),
        _ => false,
    }
}
/// `PN_PREFIX?` (the empty prefix is allowed).
fn is_pn_prefix(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let mut cs = s.chars();
    is_pn_chars_base(cs.next().unwrap())
        && !s.ends_with('.')
        && cs.all(|c| is_pn_chars(c) || c == '.')
}
const PLX_ESCAPABLE: &str = "_~.-!$&'()*+,;=/?#@%";
/// Length in bytes of the `PN_LOCAL` starting at `s` (0 when there is none).
fn pn_local_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    let mut last_ok = 0;
    let mut first = true;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        let step;
        let ok_end;
        if c == '%' {
            if b.len() >= i + 3 && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit()
            {
                step = 3;
                ok_end = true;
            } else {
                break;
            }
        } else if c == '\\' {
            match s[i + 1..].chars().next() {
                Some(e) if PLX_ESCAPABLE.contains(e) => {
                    step = 1 + e.len_utf8();
                    ok_end = true;
                }
                _ => break,
            }
        } else if (first && (is_pn_chars_u(c) || c == ':' || c.is_ascii_digit()))
            || (!first && (is_pn_chars(c) || c == ':'))
        {
            step = c.len_utf8();
            ok_end = true;
        } else if !first && c == '.' {
            step = 1;
            ok_end = false;
        } else {
            break;
        }
        first = false;
        i += step;
        if ok_end {
            last_ok = i;
        }
    }
    last_ok
}

static LANGTAG: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z]+(-[a-zA-Z0-9]+)*(--[a-zA-Z]+)?$").expect("valid regex")
});
static NUMBER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[+-]?([0-9]+|[0-9]*\.[0-9]+|([0-9]+\.[0-9]*|\.[0-9]+|[0-9]+)[eE][+-]?[0-9]+)$")
        .expect("valid regex")
});

// ---------------------------------------------------------------------- lexer ----

/// Token kinds produced by [`tokenize`]; enough structure to find variables and prefixed
/// names in raw SPARQL fragments (expressions, paths) and to validate property paths.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Var(String),
    PName(String),
    Iri,
    Str,
    Blank,
    Number,
    Ident(String),
    Punct(char),
    Ws,
    Comment,
}

/// Splits a SPARQL fragment into tokens (`(kind, start, end)` byte spans). Returns an
/// error for unterminated strings / IRIs.
pub(crate) fn tokenize(src: &str) -> Result<Vec<(Tok, usize, usize)>, String> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < src.len() {
        let c = src[i..].chars().next().unwrap();
        let start = i;
        let tok = if c.is_whitespace() {
            while i < src.len() && src[i..].chars().next().unwrap().is_whitespace() {
                i += src[i..].chars().next().unwrap().len_utf8();
            }
            Tok::Ws
        } else if c == '#' {
            while i < src.len() && b[i] != b'\n' {
                i += 1;
            }
            Tok::Comment
        } else if (c == '?' || c == '$')
            && src[i + 1..]
                .chars()
                .next()
                .is_some_and(|n| is_pn_chars_u(n) || n.is_ascii_digit())
        {
            i += 1;
            let s = i;
            while i < src.len() && is_varname_char(src[i..].chars().next().unwrap()) {
                i += src[i..].chars().next().unwrap().len_utf8();
            }
            Tok::Var(src[s..i].to_string())
        } else if c == '"' || c == '\'' {
            i = string_end(src, i).ok_or_else(|| format!("unterminated string in {src:?}"))?;
            if src[i..].starts_with('@') {
                i += 1;
                while i < src.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
                    i += 1;
                }
            }
            Tok::Str
        } else if c == '<' && iriref_end(src, i).is_some() {
            i = iriref_end(src, i).unwrap();
            Tok::Iri
        } else if c == '_' && src[i + 1..].starts_with(':') {
            i += 2;
            while i < src.len() {
                let c = src[i..].chars().next().unwrap();
                if is_pn_chars(c) || c == '.' {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            while src[..i].ends_with('.') {
                i -= 1;
            }
            Tok::Blank
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            while i < src.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            while src[..i].ends_with('.') && i > start + 1 {
                i -= 1;
            }
            if i < src.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < src.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < src.len() && b[j].is_ascii_digit() {
                    while j < src.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            Tok::Number
        } else if is_pn_chars_base(c) || c == ':' {
            // Identifier, keyword or prefixed name.
            while i < src.len() {
                let c = src[i..].chars().next().unwrap();
                if is_pn_chars(c) || c == '.' {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            while src[..i].ends_with('.') && i > start {
                i -= 1;
            }
            if src[i..].starts_with(':') {
                let prefix = src[start..i].to_string();
                i += 1;
                i += pn_local_len(&src[i..]);
                Tok::PName(prefix)
            } else {
                Tok::Ident(src[start..i].to_string())
            }
        } else {
            i += c.len_utf8();
            Tok::Punct(c)
        };
        out.push((tok, start, i));
    }
    Ok(out)
}

/// End (exclusive) of the string literal starting at `i`.
fn string_end(src: &str, i: usize) -> Option<usize> {
    let q = &src[i..i + 1];
    let long = src[i..].starts_with(&q.repeat(3));
    let mut j = if long { i + 3 } else { i + 1 };
    let b = src.as_bytes();
    while j < src.len() {
        if b[j] == b'\\' {
            j += 1 + src[j + 1..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if long {
            if src[j..].starts_with(&q.repeat(3)) {
                // A long string may end with up to two extra quotes: `"""a"""""`.
                let mut k = j + 3;
                while src[k..].starts_with(q) && k < j + 5 {
                    k += 1;
                }
                return Some(k);
            }
        } else {
            if src[j..].starts_with(q) {
                return Some(j + 1);
            }
            if b[j] == b'\n' || b[j] == b'\r' {
                return None;
            }
        }
        j += src[j..].chars().next().unwrap().len_utf8();
    }
    None
}

/// End (exclusive) of the `IRIREF` starting at `i`, if the text there is one.
fn iriref_end(src: &str, i: usize) -> Option<usize> {
    for (k, c) in src[i + 1..].char_indices() {
        match c {
            '>' => return Some(i + 1 + k + 1),
            '<' | '"' | '{' | '}' | '|' | '^' | '`' => return None,
            c if c <= ' ' => return None,
            _ => {}
        }
    }
    None
}

fn unescape_string(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut cs = s.chars();
    while let Some(c) = cs.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match cs.next() {
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{8}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{c}'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some(u @ ('u' | 'U')) => {
                let n = if u == 'u' { 4 } else { 8 };
                let hex: String = cs.by_ref().take(n).collect();
                let ch = (hex.len() == n)
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("invalid \\{u} escape"))?;
                out.push(ch);
            }
            other => return Err(format!("invalid escape sequence \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- term parser ----

/// Parses SPARQL term syntax (or a property path) into a [`Node`].
pub(crate) fn parse_term(src: &str) -> Node {
    let s = src.trim();
    let Some(c) = s.chars().next() else {
        return Node::invalid("empty term");
    };
    match c {
        '?' | '$' => {
            if is_varname(&s[1..]) {
                Node(N::Var(s[1..].to_string()))
            } else {
                Node::invalid(format!("invalid variable {s:?}"))
            }
        }
        '<' if !s.starts_with("<<") => match iriref_end(s, 0) {
            Some(end) if end == s.len() => match unescape_string(&s[1..s.len() - 1]) {
                Ok(v) => Node(N::Iri(v)),
                Err(_) => Node::invalid(format!("invalid IRI {s:?}")),
            },
            Some(_) => parse_path(s),
            None => Node::invalid(format!("invalid IRI {s:?}")),
        },
        '"' | '\'' => parse_literal(s),
        '_' if s.starts_with("_:") => {
            let label = &s[2..];
            let mut cs = label.chars();
            let ok = cs
                .next()
                .is_some_and(|c| is_pn_chars_u(c) || c.is_ascii_digit())
                && !label.ends_with('.')
                && cs.all(|c| is_pn_chars(c) || c == '.');
            if ok {
                Node(N::Blank(label.to_string()))
            } else {
                Node::invalid(format!("invalid blank node {s:?}"))
            }
        }
        '[' => {
            if s[1..].trim() == "]" {
                Node(N::Anon)
            } else {
                Node::invalid(format!("invalid term {s:?}"))
            }
        }
        '0'..='9' | '+' | '-' | '.' => {
            if NUMBER.is_match(s) {
                Node(N::Number(s.to_string()))
            } else {
                Node::invalid(format!("invalid number {s:?}"))
            }
        }
        _ => {
            match s {
                "a" => return Node(N::A),
                "true" => return Node(N::Bool(true)),
                "false" => return Node(N::Bool(false)),
                _ if s.eq_ignore_ascii_case("UNDEF") => return Node(N::Undef),
                _ => {}
            }
            if let Some(colon) = s.find(':') {
                let (prefix, local) = (&s[..colon], &s[colon + 1..]);
                if is_pn_prefix(prefix) && pn_local_len(local) == local.len() {
                    return Node(N::Prefixed(prefix.to_string(), local.to_string()));
                }
            }
            parse_path(s)
        }
    }
}

fn parse_literal(s: &str) -> Node {
    let Some(end) = string_end(s, 0) else {
        return Node::invalid(format!("unterminated string literal {s:?}"));
    };
    let q = if s.starts_with(&s[..1].repeat(3)) {
        3
    } else {
        1
    };
    let body = &s[q..end - q];
    let value = match unescape_string(body) {
        Ok(v) => v,
        Err(e) => return Node::invalid(format!("{e} in {s:?}")),
    };
    let rest = &s[end..];
    if rest.is_empty() {
        lit(value)
    } else if let Some(lang) = rest.strip_prefix('@') {
        lit_lang(value, lang)
    } else if let Some(dt) = rest.strip_prefix("^^") {
        lit_typed(value, parse_term(dt))
    } else {
        Node::invalid(format!("invalid literal {s:?}"))
    }
}

/// Accepts property path syntax: IRIs, prefixed names, `a`, `^ / | ( ) * + ? !`.
fn parse_path(s: &str) -> Node {
    let bad = || Node::invalid(format!("invalid term or property path {s:?}"));
    let Ok(toks) = tokenize(s) else {
        return bad();
    };
    let mut depth = 0i32;
    let mut has_op = false;
    for (t, _, _) in &toks {
        match t {
            Tok::Iri | Tok::PName(_) | Tok::Ws => {}
            Tok::Ident(a) if a == "a" => {}
            Tok::Punct('(') => {
                depth += 1;
                has_op = true;
            }
            Tok::Punct(')') => {
                depth -= 1;
                if depth < 0 {
                    return bad();
                }
            }
            Tok::Punct('^' | '/' | '|' | '*' | '+' | '?' | '!') => has_op = true,
            _ => return bad(),
        }
    }
    if depth != 0 || !has_op {
        return bad();
    }
    Node(N::Path(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terms() {
        assert_eq!(parse_term("?x"), var("x"));
        assert_eq!(parse_term("$x"), var("x"));
        assert_eq!(parse_term("<http://e/x>"), iri("http://e/x"));
        assert_eq!(parse_term("a").0, N::A);
        assert_eq!(parse_term("foaf:name").0, N::Prefixed("foaf".into(), "name".into()));
        assert_eq!(parse_term(":x").0, N::Prefixed("".into(), "x".into()));
        assert_eq!(parse_term("\"a\\\"b\""), lit("a\"b"));
        assert_eq!(parse_term("'''x\ny'''"), lit("x\ny"));
        assert_eq!(parse_term("\"chat\"@fr"), lit_lang("chat", "fr"));
        assert_eq!(
            parse_term("\"1\"^^xsd:int"),
            lit_typed("1", Node(N::Prefixed("xsd".into(), "int".into())))
        );
        assert_eq!(parse_term("-1.5e3").0, N::Number("-1.5e3".into()));
        assert_eq!(parse_term("true").0, N::Bool(true));
        assert_eq!(parse_term("_:b1").0, N::Blank("b1".into()));
        assert_eq!(parse_term("[ ]").0, N::Anon);
        assert!(matches!(parse_term("foaf:knows+").0, N::Path(_)));
        assert!(matches!(parse_term("^ex:p/(ex:q|a)*").0, N::Path(_)));
        assert!(matches!(parse_term("not a term").0, N::Invalid(_)));
        assert!(matches!(parse_term("\"x\" } ").0, N::Invalid(_)));
        assert!(matches!(parse_term("?").0, N::Invalid(_)));
        assert!(matches!(parse_term("ex:p ; ?x").0, N::Invalid(_)));
    }

    #[test]
    fn tokenizes() {
        let t: Vec<Tok> = tokenize("?a < 3 && regex(str(?b), \"x:y\") || <http://e/f>(?c) = ex:d.")
            .unwrap()
            .into_iter()
            .map(|t| t.0)
            .filter(|t| *t != Tok::Ws)
            .collect();
        assert!(t.contains(&Tok::Var("a".into())));
        assert!(t.contains(&Tok::PName("ex".into())));
        assert!(t.contains(&Tok::Iri));
        assert!(!t.contains(&Tok::PName("x".into())));
        assert_eq!(t.last(), Some(&Tok::Punct('.')));
    }
}
