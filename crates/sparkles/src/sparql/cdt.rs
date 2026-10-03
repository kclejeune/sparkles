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
                Ok(Val::Dec(ctx.intern_term(t), v))
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
    let list = |l: &[Elem]| Ok(Val::V(list_value(l)));
    let map = |m: &[(Term, Elem)]| Ok(Val::V(map_value(m)));
    let arity = |lo: usize, hi: usize| {
        if (lo..=hi).contains(&n) {
            Ok(())
        } else {
            Err(TypeError)
        }
    };
    match local {
        // the constructors read errors as nulls
        "List" => {
            let l: Vec<Elem> = (0..n)
                .map(|i| term(i).map_or(Elem::Null, Elem::Term))
                .collect();
            list(&l)
        }
        "Map" => {
            if n % 2 == 1 {
                return Err(TypeError);
            }
            let mut m = Map::new();
            for i in (0..n).step_by(2) {
                let Some(k) = term(i).ok().and_then(key_of) else {
                    continue;
                };
                let v = term(i + 1).map_or(Elem::Null, Elem::Term);
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
                let mut out = Vec::new();
                for i in 0..n {
                    out.extend(list_arg(&term(i)?)?);
                }
                list(&out)
            }
        },
        "contains" => {
            arity(2, 2)?;
            let l = list_arg(&term(0)?)?;
            let x = ev(1)?;
            let id = x.clone().into_id(ctx);
            let x = x.value(ctx)?;
            // a blank node of a literal is the one its label names in this query
            Ok(Val::Id(crate::id::Id::from_bool(l.iter().any(
                |e| match e {
                    Elem::Term(t @ Term::BlankNode(_)) => ctx.intern_term(t) == id,
                    Elem::Term(t) => equals(&val(t), &x).unwrap_or(false),
                    Elem::Null => false,
                },
            ))))
        }
        "containsTerm" => {
            arity(2, 2)?;
            let l = list_arg(&term(0)?)?;
            let id = ev(1)?.into_id(ctx);
            Ok(Val::Id(crate::id::Id::from_bool(l.iter().any(
                |e| matches!(e, Elem::Term(t) if ctx.intern_term(t) == id),
            ))))
        }
        "containsKey" => {
            arity(2, 2)?;
            let m = map_arg(&term(0)?)?;
            let found = term(1)
                .ok()
                .and_then(key_of)
                .is_some_and(|k| map_get(&m, &k).is_some());
            Ok(Val::Id(crate::id::Id::from_bool(found)))
        }
        "get" => {
            arity(2, 2)?;
            let t = term(0)?;
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
                    let k = key_of(term(1)?).ok_or(TypeError)?;
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
            let (v1, v2) = (ev(0)?, ev(1)?);
            let a = map_arg(&term_of(v1.clone(), ctx)?)?;
            let b = map_arg(&term_of(v2.clone(), ctx)?)?;
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
            let k = key_of(term(1)?).ok_or(TypeError)?;
            let v1 = ev(0)?;
            let mut m = map_arg(&term_of(v1.clone(), ctx)?)?;
            let v = if n == 3 {
                term(2).map_or(Elem::Null, Elem::Term)
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
            let v1 = ev(0)?;
            let mut m = map_arg(&term_of(v1.clone(), ctx)?)?;
            let Some(k) = ev(1)
                .ok()
                .and_then(|v| term_of(v, ctx).ok())
                .and_then(key_of)
            else {
                return Ok(v1);
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
