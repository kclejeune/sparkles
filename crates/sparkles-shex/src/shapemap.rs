//! Shape maps: the compact syntax (ShapeMap draft plus Jena's `BASE`/`PREFIX`
//! directives, commas, a trailing `.` and `a`), the JSON syntax, and the expansion of a
//! query map into a fixed map over the data graph.

use crate::ir::PairKind;
use crate::{
    Association, CompiledSchema, NodeSelector, ParseError, PrefixMap, SchemaError, ShapeLabel,
    ShapeMap,
};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use rustc_hash::FxHashSet;
use sparkles::id::Id;
use sparkles::store::parse_bnode_label;
use sparkles::validation::DataGraph;

/// Parse the compact syntax (see [`ShapeMap::parse`]).
pub fn parse(text: &str, prefixes: &PrefixMap, base: Option<&str>) -> Result<ShapeMap, ParseError> {
    let mut p = Parser::new(text, prefixes.clone(), base);
    p.skip_bom();
    p.directives()?;
    let mut map = Vec::new();
    loop {
        p.ws();
        if p.at_end() {
            break;
        }
        if !map.is_empty() && p.eat(',') {
            p.ws();
        }
        map.push(p.association()?);
        p.ws();
        // Jena: an optional `.` after each association
        p.eat('.');
    }
    if map.is_empty() {
        return Err(p.error("expected a shape association"));
    }
    Ok(ShapeMap(map))
}

/// Parse the JSON syntax (see [`ShapeMap::from_json`]).
pub fn from_json(json: &str) -> Result<ShapeMap, ParseError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| ParseError::new(e.to_string(), e.line(), e.column()))?;
    let at = |i: usize, msg: String| ParseError::new(format!("association {}: {msg}", i + 1), 1, 1);
    let serde_json::Value::Array(items) = v else {
        return Err(ParseError::new("a JSON shape map is an array", 1, 1));
    };
    let mut seen = FxHashSet::default();
    let mut map = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let field = |names: [&str; 2]| {
            names
                .iter()
                .find_map(|n| item.get(*n))
                .ok_or_else(|| at(i, format!("no \"{}\"", names[0])))
                .and_then(|v| {
                    v.as_str()
                        .ok_or_else(|| at(i, format!("\"{}\" is not a string", names[0])))
                })
        };
        let node = field(["node", "nodeSelector"])?;
        let shape = field(["shape", "shapeLabel"])?;
        let node = json_selector(node).map_err(|e| at(i, e.message))?;
        let shape = json_label(shape);
        if !seen.insert((node.clone(), shape.clone())) {
            return Err(at(i, "duplicate node and shape".to_string()));
        }
        map.push(Association { node, shape });
    }
    Ok(ShapeMap(map))
}

/// A node selector of a JSON map: compact term syntax, a `{…}` pattern, `SPARQL …`,
/// or a bare IRI (as in shexTest's map files).
fn json_selector(s: &str) -> Result<NodeSelector, ParseError> {
    let t = s.trim();
    let looks_compact = t.starts_with(['<', '"', '\'', '{', '_'])
        || t.starts_with(|c: char| c.is_ascii_digit() || c == '+' || c == '-')
        || t == "true"
        || t == "false"
        || t.get(..6).is_some_and(|k| k.eq_ignore_ascii_case("sparql"));
    if !looks_compact {
        return Ok(NodeSelector::Term(NamedNode::new_unchecked(t).into()));
    }
    let mut p = Parser::new(t, PrefixMap::new(), None);
    let n = p.selector()?;
    p.ws();
    if !p.at_end() {
        return Err(p.error("unexpected text after the node selector"));
    }
    Ok(n)
}

/// A shape label of a JSON map: `START`, `_:label`, or an IRI (with or without `<>`).
fn json_label(s: &str) -> ShapeLabel {
    let t = s.trim();
    if t.eq_ignore_ascii_case("start") || t.eq_ignore_ascii_case("@start") {
        return ShapeLabel::Start;
    }
    if let Some(b) = t.strip_prefix("_:") {
        return ShapeLabel::BNode(b.to_string());
    }
    let t = t
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .unwrap_or(t);
    ShapeLabel::Iri(t.to_string())
}

/// A compact shape-map parser over a text.
struct Parser<'a> {
    src: &'a str,
    pos: usize,
    prefixes: PrefixMap,
    base: Option<oxiri::Iri<String>>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, prefixes: PrefixMap, base: Option<&str>) -> Parser<'a> {
        Parser {
            src,
            pos: 0,
            prefixes,
            base: base.and_then(|b| oxiri::Iri::parse(b.to_string()).ok()),
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    /// An error at the current position (1-based line and column in characters).
    fn error(&self, msg: impl Into<String>) -> ParseError {
        self.error_at(self.pos, msg)
    }

    fn error_at(&self, pos: usize, msg: impl Into<String>) -> ParseError {
        let before = &self.src[..pos.min(self.src.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        let mut msg = msg.into();
        match self.src[pos.min(self.src.len())..].chars().next() {
            Some(c) => msg.push_str(&format!(", found '{c}'")),
            None => msg.push_str(", found the end of the text"),
        }
        ParseError::new(msg, line, column)
    }

    fn skip_bom(&mut self) {
        self.eat('\u{feff}');
    }

    /// Skip whitespace and comments (`# …`, `/* … */`).
    fn ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('#') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                Some('/') if self.peek2() == Some('*') => match self.rest()[2..].find("*/") {
                    Some(i) => self.pos += 2 + i + 2,
                    None => self.pos = self.src.len(),
                },
                _ => return,
            }
        }
    }

    /// A keyword (case-insensitive) followed by something that cannot continue a name.
    fn keyword(&mut self, kw: &str) -> bool {
        let r = self.rest();
        let ok = r.len() >= kw.len()
            && r.is_char_boundary(kw.len())
            && r[..kw.len()].eq_ignore_ascii_case(kw)
            && !r[kw.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':'));
        if ok {
            self.pos += kw.len();
        }
        ok
    }

    /// `BASE <iri>` and `PREFIX p: <iri>` (also `@base`/`@prefix` with a final `.`).
    fn directives(&mut self) -> Result<(), ParseError> {
        loop {
            self.ws();
            let turtle = self.peek() == Some('@')
                && (self.rest()[1..].to_ascii_lowercase().starts_with("base")
                    || self.rest()[1..].to_ascii_lowercase().starts_with("prefix"));
            if turtle {
                self.bump();
            }
            if self.keyword("base") {
                self.ws();
                let iri = self.iriref()?;
                self.base = Some(
                    oxiri::Iri::parse(iri.clone())
                        .map_err(|e| self.error(format!("invalid base IRI <{iri}>: {e}")))?,
                );
            } else if self.keyword("prefix") {
                self.ws();
                let start = self.pos;
                let name = self.pn_prefix();
                if !self.eat(':') {
                    return Err(self.error_at(start, "expected a prefix name ending in ':'"));
                }
                self.ws();
                let iri = self.iriref()?;
                self.prefixes.retain(|(p, _)| *p != name);
                self.prefixes.push((name, iri));
            } else {
                return Ok(());
            }
            if turtle {
                self.ws();
                if !self.eat('.') {
                    return Err(self.error("expected '.' after the directive"));
                }
            }
        }
    }

    fn association(&mut self) -> Result<Association, ParseError> {
        let node = self.selector()?;
        self.ws();
        let shape = self.shape_spec()?;
        Ok(Association { node, shape })
    }

    /// A node, a `{FOCUS p o}` / `{s p FOCUS}` pattern, or `SPARQL "…"`.
    fn selector(&mut self) -> Result<NodeSelector, ParseError> {
        self.ws();
        if self.eat('{') {
            return self.pattern();
        }
        if self.keyword("sparql") {
            self.ws();
            if !matches!(self.peek(), Some('"' | '\'')) {
                return Err(self.error("expected the query of a SPARQL selector as a string"));
            }
            return Ok(NodeSelector::Sparql(self.string()?));
        }
        Ok(NodeSelector::Term(self.object_term()?))
    }

    fn pattern(&mut self) -> Result<NodeSelector, ParseError> {
        self.ws();
        let sel = if self.keyword("focus") {
            self.ws();
            let predicate = self.predicate()?;
            self.ws();
            let object = if self.underscore() {
                None
            } else {
                Some(self.object_term()?)
            };
            NodeSelector::Focus {
                subject: None,
                predicate,
                object,
                focus_is_subject: true,
            }
        } else {
            let subject = if self.underscore() {
                None
            } else {
                let start = self.pos;
                match self.object_term()? {
                    t @ (Term::NamedNode(_) | Term::BlankNode(_)) => Some(t),
                    _ => return Err(self.error_at(start, "expected FOCUS, '_' or a subject")),
                }
            };
            self.ws();
            let predicate = self.predicate()?;
            self.ws();
            if !self.keyword("focus") {
                return Err(self.error("expected FOCUS"));
            }
            NodeSelector::Focus {
                subject,
                predicate,
                object: None,
                focus_is_subject: false,
            }
        };
        self.ws();
        if !self.eat('}') {
            return Err(self.error("expected '}'"));
        }
        Ok(sel)
    }

    /// `_` as a wildcard (not the start of a blank node).
    fn underscore(&mut self) -> bool {
        if self.peek() == Some('_') && self.peek2() != Some(':') {
            self.bump();
            true
        } else {
            false
        }
    }

    fn predicate(&mut self) -> Result<NamedNode, ParseError> {
        if self.keyword("a") {
            return Ok(rdf::TYPE.into_owned());
        }
        let iri = self.iri()?;
        Ok(NamedNode::new_unchecked(iri))
    }

    /// `@<iri>`, `@prefix:name`, `@START`, `@_:label`.
    fn shape_spec(&mut self) -> Result<ShapeLabel, ParseError> {
        if !self.eat('@') {
            return Err(self.error("expected '@' and a shape label"));
        }
        self.ws();
        if self.keyword("start") {
            return Ok(ShapeLabel::Start);
        }
        if self.rest().starts_with("_:") {
            return Ok(ShapeLabel::BNode(self.bnode_label()?));
        }
        Ok(ShapeLabel::Iri(self.iri()?))
    }

    fn object_term(&mut self) -> Result<Term, ParseError> {
        match self.peek() {
            Some('"' | '\'') => self.literal(),
            Some('_') if self.peek2() == Some(':') => {
                Ok(BlankNode::new_unchecked(self.bnode_label()?).into())
            }
            Some(c) if c.is_ascii_digit() || matches!(c, '+' | '-' | '.') => self.number(),
            _ => {
                if self.keyword("true") {
                    return Ok(Literal::from(true).into());
                }
                if self.keyword("false") {
                    return Ok(Literal::from(false).into());
                }
                Ok(NamedNode::new_unchecked(self.iri()?).into())
            }
        }
    }

    fn bnode_label(&mut self) -> Result<String, ParseError> {
        self.pos += 2;
        let start = self.pos;
        while let Some(c) = self.peek() {
            let ok = c.is_alphanumeric() || matches!(c, '_' | '-' | '.');
            if !ok {
                break;
            }
            self.bump();
        }
        while self.src[start..self.pos].ends_with('.') {
            self.pos -= 1;
        }
        if self.pos == start {
            return Err(self.error("expected a blank node label"));
        }
        Ok(self.src[start..self.pos].to_string())
    }

    /// An IRI: `<…>` or a prefixed name.
    fn iri(&mut self) -> Result<String, ParseError> {
        if self.peek() == Some('<') {
            return self.iriref();
        }
        let start = self.pos;
        let prefix = self.pn_prefix();
        if !self.eat(':') {
            return Err(self.error_at(start, "expected an IRI or a prefixed name"));
        }
        let local = self.pn_local()?;
        match self.prefixes.iter().rev().find(|(p, _)| *p == prefix) {
            Some((_, ns)) => Ok(format!("{ns}{local}")),
            None => Err(self.error_at(start, format!("undefined prefix '{prefix}:'"))),
        }
    }

    /// `<…>`, with `\u` escapes, resolved against the base.
    fn iriref(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        if !self.eat('<') {
            return Err(self.error("expected '<'"));
        }
        let mut s = String::new();
        loop {
            match self.bump() {
                Some('>') => break,
                Some('\\') => s.push(self.uchar()?),
                Some(c) if c <= ' ' || "<\"{}|^`".contains(c) => {
                    return Err(self.error_at(start, "invalid character in an IRI"));
                }
                Some(c) => s.push(c),
                None => return Err(self.error_at(start, "unterminated IRI")),
            }
        }
        match &self.base {
            Some(b) => b
                .resolve(&s)
                .map(|i| i.into_inner())
                .map_err(|e| self.error_at(start, format!("invalid IRI <{s}>: {e}"))),
            None => match oxiri::Iri::parse(s.clone()) {
                Ok(i) => Ok(i.into_inner()),
                Err(e) => Err(self.error_at(start, format!("invalid IRI <{s}>: {e}"))),
            },
        }
    }

    /// The rest of `\uXXXX` or `\UXXXXXXXX` (after the backslash).
    fn uchar(&mut self) -> Result<char, ParseError> {
        let n = match self.bump() {
            Some('u') => 4,
            Some('U') => 8,
            _ => return Err(self.error("expected \\u or \\U")),
        };
        let hex: String = self.rest().chars().take(n).collect();
        if hex.len() != n {
            return Err(self.error("invalid \\u escape"));
        }
        let c = u32::from_str_radix(&hex, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| self.error("invalid \\u escape"))?;
        self.pos += n;
        Ok(c)
    }

    fn pn_prefix(&mut self) -> String {
        let start = self.pos;
        if self.peek().is_some_and(|c| c.is_alphabetic()) {
            while self
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                self.bump();
            }
            while self.src[start..self.pos].ends_with('.') {
                self.pos -= 1;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// The local part of a prefixed name, with `\` escapes and `%HH` kept as written.
    fn pn_local(&mut self) -> Result<String, ParseError> {
        let mut s = String::new();
        loop {
            match self.peek() {
                Some(c) if c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.') => {
                    self.bump();
                    s.push(c);
                }
                Some('%') => {
                    self.bump();
                    s.push('%');
                    for _ in 0..2 {
                        match self.bump() {
                            Some(h) if h.is_ascii_hexdigit() => s.push(h),
                            _ => return Err(self.error("invalid %-escape in a prefixed name")),
                        }
                    }
                }
                Some('\\') => {
                    self.bump();
                    match self.bump() {
                        Some(c) if "_~.-!$&'()*+,;=/?#@%".contains(c) => s.push(c),
                        _ => return Err(self.error("invalid escape in a prefixed name")),
                    }
                }
                _ => break,
            }
        }
        // a final '.' ends the association, not the name
        while s.ends_with('.') {
            s.pop();
            self.pos -= 1;
        }
        Ok(s)
    }

    /// A string in any of the four quote forms, unescaped.
    fn string(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        let q = self.bump().unwrap_or('"');
        let long = self.rest().starts_with(&format!("{q}{q}"));
        if long {
            self.pos += 2;
        }
        let mut s = String::new();
        loop {
            if long {
                let three: String = [q; 3].iter().collect();
                if self.rest().starts_with(&three) && !self.rest()[3..].starts_with(q) {
                    self.pos += 3;
                    return Ok(s);
                }
            }
            match self.bump() {
                None => return Err(self.error_at(start, "unterminated string")),
                Some(c) if c == q && !long => return Ok(s),
                Some('\n' | '\r') if !long => {
                    return Err(self.error_at(start, "unterminated string"));
                }
                Some('\\') => match self.peek() {
                    Some('u' | 'U') => s.push(self.uchar()?),
                    Some(e) => {
                        self.bump();
                        s.push(match e {
                            't' => '\t',
                            'b' => '\u{8}',
                            'n' => '\n',
                            'r' => '\r',
                            'f' => '\u{c}',
                            '"' | '\'' | '\\' => e,
                            _ => return Err(self.error("invalid escape in a string")),
                        });
                    }
                    None => return Err(self.error_at(start, "unterminated string")),
                },
                Some(c) => s.push(c),
            }
        }
    }

    /// A string with an optional language tag or datatype. `"x"@en@ex:S` is a
    /// language-tagged literal; in `"x"@ex:S` and `"x"@START` the `@` starts the shape
    /// label.
    fn literal(&mut self) -> Result<Term, ParseError> {
        let value = self.string()?;
        if self.rest().starts_with("^^") {
            self.pos += 2;
            let dt = self.iri()?;
            return Ok(Literal::new_typed_literal(value, NamedNode::new_unchecked(dt)).into());
        }
        if self.peek() == Some('@') {
            let r = &self.rest()[1..];
            let len = r
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(r.len());
            let tag = &r[..len];
            let next = r[len..].chars().next();
            let after = r[len..].trim_start();
            // a prefixed name, or START not followed by the label
            let label = matches!(next, Some(':' | '_' | '.'))
                || (tag.eq_ignore_ascii_case("start") && !after.starts_with(['@', '}']));
            if !tag.is_empty() && tag.starts_with(|c: char| c.is_ascii_alphabetic()) && !label {
                let tag = tag.to_string();
                self.pos += 1 + len;
                return Literal::new_language_tagged_literal(value, &tag)
                    .map(Term::from)
                    .map_err(|e| self.error(format!("invalid language tag @{tag}: {e}")));
            }
        }
        Ok(Literal::new_simple_literal(value).into())
    }

    /// An integer, decimal or double.
    fn number(&mut self) -> Result<Term, ParseError> {
        let start = self.pos;
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        let digits = |p: &mut Parser<'_>| {
            let s = p.pos;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.bump();
            }
            p.pos - s
        };
        let int = digits(self);
        let mut frac = 0;
        if self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            frac = digits(self);
        }
        let mut exp = false;
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.pos;
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if digits(self) > 0 {
                exp = true;
            } else {
                self.pos = save;
            }
        }
        if int + frac == 0 {
            return Err(self.error_at(start, "expected a node"));
        }
        let lex = &self.src[start..self.pos];
        let dt = if exp {
            xsd::DOUBLE
        } else if frac > 0 {
            xsd::DECIMAL
        } else {
            xsd::INTEGER
        };
        Ok(Literal::new_typed_literal(lex, dt).into())
    }
}

/// An association of the fixed map.
#[derive(Clone, Debug, PartialEq)]
pub struct FixedEntry {
    pub node: Term,
    /// the node's store id (`None`: not in the store, so its neighbourhood is empty)
    pub id: Option<Id>,
    pub shape: ShapeLabel,
    pub kind: PairKind,
}

/// The pair kind of a shape label of a map.
pub fn label_kind(schema: &CompiledSchema, label: &ShapeLabel) -> Result<PairKind, SchemaError> {
    match label {
        ShapeLabel::Start => schema.ir().start.ok_or_else(|| {
            SchemaError::new("the shape map uses START, but the schema has no start shape")
        }),
        _ => {
            let l = label.as_shexj().unwrap_or_default();
            schema.label(&l).ok_or_else(|| {
                SchemaError::new(format!(
                    "the shape map uses {}, which the schema does not define",
                    match label {
                        ShapeLabel::BNode(b) => format!("_:{b}"),
                        _ => format!("<{l}>"),
                    }
                ))
            })
        }
    }
}

/// The fixed map of a query map: each selector expanded over the data graph, the
/// associations deduplicated per (node, label) in first-seen order; and the warnings
/// (blank-node labels that select nothing). A label the schema does not define, or
/// START without a start shape, is a [`crate::SchemaError`].
pub fn expand(
    map: &ShapeMap,
    data: &DataGraph,
    schema: &CompiledSchema,
) -> anyhow::Result<(Vec<FixedEntry>, Vec<String>)> {
    let snap = &data.snap;
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let mut seen: FxHashSet<(Term, ShapeLabel)> = FxHashSet::default();
    let mut push =
        |out: &mut Vec<FixedEntry>, node: Term, id: Option<Id>, a: &Association, kind: PairKind| {
            if seen.insert((node.clone(), a.shape.clone())) {
                out.push(FixedEntry {
                    node,
                    id,
                    shape: a.shape.clone(),
                    kind,
                });
            }
        };
    for a in &map.0 {
        let kind = label_kind(schema, &a.shape)?;
        match &a.node {
            NodeSelector::Term(Term::BlankNode(b)) => match parse_bnode_label(b.as_str()) {
                Some(id) => push(&mut out, Term::BlankNode(b.clone()), Some(id), a, kind),
                None => warnings.push(format!(
                    "the blank node _:{} selects nothing: only labels of stored blank nodes (_:b…) name nodes",
                    b.as_str()
                )),
            },
            NodeSelector::Term(t) => push(&mut out, t.clone(), snap.lookup_term(t), a, kind),
            NodeSelector::Focus {
                subject,
                predicate,
                object,
                focus_is_subject,
            } => {
                let Some(p) = snap.lookup_iri(predicate.as_str()) else {
                    continue;
                };
                let fixed = |t: &Option<Term>| -> Option<Option<Id>> {
                    match t {
                        None => Some(None),
                        Some(Term::BlankNode(b)) => parse_bnode_label(b.as_str()).map(Some),
                        Some(t) => snap.lookup_term(t).map(Some),
                    }
                };
                let ids = if *focus_is_subject {
                    match fixed(object) {
                        None => continue,
                        Some(Some(o)) => data.subjects(p, o)?,
                        Some(None) => data.subjects_of(p)?,
                    }
                } else {
                    match fixed(subject) {
                        None => continue,
                        Some(Some(s)) => data.objects(s, p)?,
                        Some(None) => data.objects_of(p)?,
                    }
                };
                for id in ids {
                    if let Some(t) = snap.term(id) {
                        push(&mut out, t, Some(id), a, kind);
                    }
                }
            }
            NodeSelector::Sparql(_) => {
                return Err(SchemaError::new("SPARQL node selectors are not supported yet").into());
            }
        }
    }
    Ok((out, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Ir, PairKindInfo, SeId};
    use sparkles::io::{RdfFormat, Source};
    use sparkles::store::{Store, StoreOptions};

    const EX: &str = "http://ex.org/";

    fn ex(s: &str) -> String {
        format!("{EX}{s}")
    }

    fn iri(s: &str) -> Term {
        NamedNode::new_unchecked(ex(s)).into()
    }

    fn schema_prefixes() -> PrefixMap {
        vec![
            ("ex".into(), EX.into()),
            ("xsd".into(), "http://www.w3.org/2001/XMLSchema#".into()),
        ]
    }

    fn assoc(node: NodeSelector, shape: ShapeLabel) -> Association {
        Association { node, shape }
    }

    fn person() -> ShapeLabel {
        ShapeLabel::Iri(ex("Person"))
    }

    fn term(t: impl Into<Term>) -> NodeSelector {
        NodeSelector::Term(t.into())
    }

    #[test]
    fn compact_syntax() {
        let text = r#"
            PREFIX ex: <http://ex.org/>
            # a comment
            {FOCUS a ex:Person}@ex:Person,
            ex:alice@ex:Person
            "42"^^xsd:integer@<http://ex.org/Answer>,
            _:b1f@START.
            "chat"@fr@ex:Word , "x"@ex:Word "y"@start {_ ex:knows FOCUS}@ex:Person
            {ex:bob ex:knows focus}@ex:Person {FOCUS ex:age _}@ex:Person
            <alice> @ <Person> 1.5@ex:N -2@ex:N 1e3@ex:N true@ex:B .
        "#;
        let m = parse(text, &schema_prefixes(), Some("http://ex.org/")).unwrap();
        let word = || ShapeLabel::Iri(ex("Word"));
        let n = || ShapeLabel::Iri(ex("N"));
        let knows = NamedNode::new_unchecked(ex("knows"));
        let typed = |l: &str, dt: oxrdf::NamedNodeRef<'_>| term(Literal::new_typed_literal(l, dt));
        let expected = vec![
            assoc(
                NodeSelector::Focus {
                    subject: None,
                    predicate: rdf::TYPE.into_owned(),
                    object: Some(iri("Person")),
                    focus_is_subject: true,
                },
                person(),
            ),
            assoc(term(iri("alice")), person()),
            assoc(typed("42", xsd::INTEGER), ShapeLabel::Iri(ex("Answer"))),
            assoc(term(BlankNode::new_unchecked("b1f")), ShapeLabel::Start),
            assoc(
                term(Literal::new_language_tagged_literal("chat", "fr").unwrap()),
                word(),
            ),
            assoc(term(Literal::new_simple_literal("x")), word()),
            assoc(term(Literal::new_simple_literal("y")), ShapeLabel::Start),
            assoc(
                NodeSelector::Focus {
                    subject: None,
                    predicate: knows.clone(),
                    object: None,
                    focus_is_subject: false,
                },
                person(),
            ),
            assoc(
                NodeSelector::Focus {
                    subject: Some(iri("bob")),
                    predicate: knows,
                    object: None,
                    focus_is_subject: false,
                },
                person(),
            ),
            assoc(
                NodeSelector::Focus {
                    subject: None,
                    predicate: NamedNode::new_unchecked(ex("age")),
                    object: None,
                    focus_is_subject: true,
                },
                person(),
            ),
            assoc(term(iri("alice")), person()),
            assoc(typed("1.5", xsd::DECIMAL), n()),
            assoc(typed("-2", xsd::INTEGER), n()),
            assoc(typed("1e3", xsd::DOUBLE), n()),
            assoc(term(Literal::from(true)), ShapeLabel::Iri(ex("B"))),
        ];
        assert_eq!(m.0.len(), expected.len(), "{:#?}", m.0);
        for (a, b) in m.0.iter().zip(&expected) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn the_benchmark_map() {
        let m = parse(include_str!("../examples/bench.smap"), &vec![], None).unwrap();
        assert_eq!(m.0.len(), 8);
        assert!(m.0.iter().all(|a| matches!(
            &a.shape,
            ShapeLabel::Iri(i) if i.starts_with("http://example.org/")
        )));
    }

    #[test]
    fn directives_and_prefixes() {
        // without directives the schema's prefixes apply; directives add to them
        let m = parse("ex:a@ex:S", &schema_prefixes(), None).unwrap();
        assert_eq!(m.0[0].node, term(iri("a")));
        let m = parse(
            "BASE <http://b.org/> PREFIX ex: <http://other.org/> ex:a@<S> \"1\"^^xsd:int@<S>",
            &schema_prefixes(),
            None,
        )
        .unwrap();
        assert_eq!(
            m.0[0],
            assoc(
                term(NamedNode::new_unchecked("http://other.org/a")),
                ShapeLabel::Iri("http://b.org/S".into())
            )
        );
        assert_eq!(m.0.len(), 2);
        let m = parse("@prefix p: <http://p.org/> . p:x@p:S", &vec![], None).unwrap();
        assert_eq!(m.0[0].shape, ShapeLabel::Iri("http://p.org/S".into()));
        let m = parse(
            "SPARQL '''SELECT ?focus { ?focus a ?c }'''@START",
            &vec![],
            None,
        )
        .unwrap();
        assert_eq!(
            m.0[0].node,
            NodeSelector::Sparql("SELECT ?focus { ?focus a ?c }".into())
        );
    }

    #[test]
    fn syntax_errors_have_positions() {
        let err = |t: &str| parse(t, &schema_prefixes(), None).unwrap_err();
        let e = err("ex:a@ex:S\n  nope:x@ex:S");
        assert_eq!((e.line, e.column), (2, 3), "{e}");
        assert!(e.message.contains("undefined prefix 'nope:'"), "{e}");
        let e = err("ex:a ex:S");
        assert_eq!((e.line, e.column), (1, 6), "{e}");
        assert!(e.message.contains("expected '@'"), "{e}");
        let e = err("{FOCUS a ex:S @ex:S");
        assert!(e.message.contains("'}'"), "{e}");
        assert!(err("").message.contains("expected a shape association"));
        assert!(err("{ex:a ex:p ex:b}@ex:S").message.contains("FOCUS"));
        assert!(err("<a>@ex:S").message.contains("invalid IRI"));
        assert!(err("\"abc@ex:S").message.contains("unterminated string"));
    }

    #[test]
    fn json_maps() {
        let m = from_json(
            r#"[{"node": "http://ex.org/a", "shape": "http://ex.org/S"},
                {"nodeSelector": "<http://ex.org/b>", "shapeLabel": "START"},
                {"node": "{FOCUS <http://ex.org/p> _}", "shape": "<http://ex.org/S>",
                 "status": "conformant"},
                {"node": "\"chat\"@fr", "shape": "_:s"},
                {"node": "\"x\"", "shape": "http://ex.org/S"}]"#,
        )
        .unwrap();
        let s = || ShapeLabel::Iri(ex("S"));
        assert_eq!(m.0[0], assoc(term(iri("a")), s()));
        assert_eq!(m.0[1], assoc(term(iri("b")), ShapeLabel::Start));
        assert!(matches!(
            m.0[2].node,
            NodeSelector::Focus {
                focus_is_subject: true,
                object: None,
                ..
            }
        ));
        assert_eq!(m.0[2].shape, s());
        assert_eq!(
            m.0[3],
            assoc(
                term(Literal::new_language_tagged_literal("chat", "fr").unwrap()),
                ShapeLabel::BNode("s".into())
            )
        );
        assert_eq!(m.0[4].node, term(Literal::new_simple_literal("x")));
        let dup = from_json(
            r#"[{"node": "http://ex.org/a", "shape": "http://ex.org/S"},
                {"node": "<http://ex.org/a>", "shape": "<http://ex.org/S>"}]"#,
        )
        .unwrap_err();
        assert!(dup.message.contains("association 2: duplicate"), "{dup}");
        assert!(from_json("{}").is_err());
        let e = from_json(r#"[{"node": "http://ex.org/a"}]"#).unwrap_err();
        assert!(e.message.contains("shape"), "{e}");
        let e = from_json("[\n{").unwrap_err();
        assert_eq!(e.line, 2);
    }

    /// A schema that declares ex:Person and ex:Org, with or without a start shape.
    fn schema(start: bool) -> CompiledSchema {
        let mut ir = Ir::default();
        for (i, l) in ["Person", "Org"].iter().enumerate() {
            ir.pairs.push(PairKindInfo {
                se: SeId(0),
                label: Some(crate::Label::Iri(ex(l))),
                stratum: 0,
            });
            ir.labels.insert(ex(l), PairKind(i as u32));
        }
        ir.start = start.then_some(PairKind(0));
        CompiledSchema::from_ir(ir, schema_prefixes(), None)
    }

    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        let trig = r#"
            @prefix ex: <http://ex.org/> .
            ex:alice a ex:Person ; ex:knows ex:bob .
            ex:bob a ex:Person ; ex:knows ex:alice, ex:carol .
            ex:acme a ex:Org .
            ex:g1 { ex:carol a ex:Person ; ex:knows ex:dave . }
        "#;
        s.load(&[Source::from_bytes(
            trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        s
    }

    fn names(entries: &[FixedEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match &e.node {
                Term::NamedNode(n) => n.as_str().trim_start_matches(EX).to_string(),
                t => t.to_string(),
            })
            .collect()
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn expansion() {
        let store = store();
        let snap = store.snapshot();
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let schema = schema(true);
        let map = parse(
            "{FOCUS a ex:Person}@ex:Person, ex:alice@ex:Person, ex:nobody@ex:Org,
             {ex:bob ex:knows FOCUS}@ex:Person, {_ ex:knows FOCUS}@START,
             {FOCUS ex:knows _}@ex:Person, {FOCUS ex:absent _}@ex:Person, _:x@ex:Org",
            &schema_prefixes(),
            None,
        )
        .unwrap();
        let (fixed, warnings) = expand(&map, &data, &schema).unwrap();
        let n = names(&fixed);
        // {FOCUS a ex:Person} in the default graph; ex:alice again is a duplicate
        assert_eq!(sorted(n[..2].to_vec()), ["alice", "bob"]);
        assert_eq!(n[2], "nobody");
        assert_eq!(fixed[2].shape, ShapeLabel::Iri(ex("Org")));
        // {ex:bob ex:knows FOCUS}: alice is there already, carol is new
        assert_eq!(n[3], "carol");
        // {_ ex:knows FOCUS}@START: alice, bob, carol (dave is in another graph);
        // {FOCUS ex:knows _}@ex:Person adds nothing new
        assert_eq!(sorted(n[4..].to_vec()), ["alice", "bob", "carol"]);
        assert!(fixed[4..].iter().all(|e| e.shape == ShapeLabel::Start));
        assert!(fixed[4..].iter().all(|e| e.kind == PairKind(0)));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("_:x"), "{warnings:?}");
        // the absent node has no id; the others do
        assert_eq!((fixed[2].id, fixed[2].kind), (None, PairKind(1)));
        assert!(
            fixed
                .iter()
                .filter(|e| e.node != iri("nobody"))
                .all(|e| e.id.is_some())
        );

        // over a named graph, and the union of all graphs
        let g1 = DataGraph::new(snap.clone(), Some(&ex("g1")), &[], &[]).unwrap();
        let map = parse("{FOCUS a ex:Person}@ex:Person", &schema_prefixes(), None).unwrap();
        let (fixed, _) = expand(&map, &g1, &schema).unwrap();
        assert_eq!(names(&fixed), ["carol"]);
        let all = DataGraph::new(snap.clone(), Some("urn:x-arq:UnionGraph"), &[], &[]).unwrap();
        let (fixed, _) = expand(&map, &all, &schema).unwrap();
        assert_eq!(sorted(names(&fixed)), ["alice", "bob", "carol"]);
        let map = parse("{_ ex:knows FOCUS}@ex:Person", &schema_prefixes(), None).unwrap();
        let (fixed, _) = expand(&map, &g1, &schema).unwrap();
        assert_eq!(names(&fixed), ["dave"]);
    }

    #[test]
    fn stored_blank_nodes_by_label() {
        let store = store();
        let mut txn = store.write();
        let b = txn.new_bnode();
        let p = txn.intern(&iri("knows")).unwrap();
        let alice = txn.intern(&iri("alice")).unwrap();
        txn.insert([b, p, alice, Id::DEFAULT_GRAPH]).unwrap();
        txn.commit().unwrap();
        let snap = store.snapshot();
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let label = sparkles::store::bnode_for(b);
        let text = format!("_:{}@ex:Person", label.as_str());
        let map = parse(&text, &schema_prefixes(), None).unwrap();
        let (fixed, warnings) = expand(&map, &data, &schema(false)).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(fixed[0].id, Some(b));
        // a focus pattern finds the blank node too
        let map = parse(
            "{FOCUS ex:knows ex:alice}@ex:Person",
            &schema_prefixes(),
            None,
        )
        .unwrap();
        let (fixed, _) = expand(&map, &data, &schema(false)).unwrap();
        assert!(fixed.iter().any(|e| e.id == Some(b)), "{fixed:?}");
    }

    #[test]
    fn labels_must_be_defined() {
        let store = store();
        let snap = store.snapshot();
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let err = |m: &str, start: bool| {
            let map = parse(m, &schema_prefixes(), None).unwrap();
            expand(&map, &data, &schema(start))
                .unwrap_err()
                .downcast::<SchemaError>()
                .unwrap()
                .message
        };
        assert!(err("ex:a@ex:Nope", true).contains("<http://ex.org/Nope>"));
        assert!(err("ex:a@START", false).contains("no start shape"));
        let sparql = err("SPARQL 'SELECT ?focus {}'@ex:Person", true);
        assert!(sparql.contains("not supported yet"), "{sparql}");
        // a label is checked even when its selector selects nothing
        assert!(err("{FOCUS ex:absent _}@ex:Nope", true).contains("Nope"));
    }
}
