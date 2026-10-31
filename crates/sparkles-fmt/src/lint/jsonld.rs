//! Linting JSON-LD (spec X04): the term rules over value objects and over the strings of
//! terms with a datatype coercion, the rules over IRIs in `@id`, `@type` and `@context`,
//! and two rules of its own. `unused-context-term` finds a term of a `@context` that no
//! key, value or other definition of the document uses. `unmapped-key` finds a key that
//! no context in effect maps, which JSON-LD drops without a word.
//!
//! The reference parser is the strict JSON parser, so a document that is not JSON is a
//! `syntax` finding. The rules read a small model of the document built from the
//! lossless tree (strings with their spans, objects with their members) and follow the
//! contexts that apply to each object, including the contexts of terms and types that are
//! scoped. What they cannot know they leave alone. A context given as a URL may define
//! any term, so `unmapped-key` is silent beside one and so is a document that has no
//! context. `@vocab` maps every key. The values of a term with a map container, of a
//! term typed `@json` and of a value object are data, not properties.

use super::{Cst, Edit, Fix, LintError, Out, terms};
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::NodeId;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

pub(super) fn run(text: &str, out: &mut Out<'_>) -> Result<(), LintError> {
    let reference = crate::check::json::json_reference(text);
    out.deadline()?;
    if let Err(e) = reference {
        out.syntax(&e);
        return Ok(());
    }
    let tree = match crate::jsonld::parse::parse(text, crate::jsonld::lex::lex(text)) {
        Ok(t) => t,
        Err(e) => {
            out.syntax(&e);
            return Ok(());
        }
    };
    let c = Cst::new(&tree);
    let root = build(&c, c.tree.root());
    Walk { out }.value(&root, &Ctx::default());
    out.deadline()?;
    context_rules(&root, out);
    Ok(())
}

// ----------------------------------------------------------------------- model ------

/// A JSON value with the spans of the parts the rules report.
enum Val {
    Obj(Obj),
    Arr(Vec<Val>),
    Str(Str),
    Null,
    Other,
}

/// A string, its decoded value, and the span of its token with the quotes.
struct Str {
    value: String,
    start: usize,
    end: usize,
}

struct Obj {
    members: Vec<Member>,
    /// the end of `{` and the start of `}`
    open_end: usize,
    close_start: usize,
}

struct Member {
    key: Str,
    value: Val,
    /// from the key to the end of the value
    start: usize,
    end: usize,
    /// the `,` after the member
    comma: Option<(usize, usize)>,
}

impl Obj {
    fn get(&self, key: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.key.value == key)
    }

    /// The edit that removes member `i`, with the comma and the white space that go with it.
    fn removal(&self, i: usize) -> Edit {
        let m = &self.members[i];
        let (start, end) = if self.members.len() == 1 {
            (self.open_end, self.close_start)
        } else if m.comma.is_some() {
            (m.start, self.members[i + 1].start)
        } else {
            (self.members[i - 1].comma.map_or(m.start, |c| c.0), m.end)
        };
        Edit {
            start,
            end,
            insert: String::new(),
        }
    }
}

fn build(c: &Cst<'_, '_>, n: NodeId) -> Val {
    match c.kind(n) {
        NodeKind::JsonDocument => c
            .tree
            .child_nodes(n)
            .next()
            .map_or(Val::Other, |v| build(c, v)),
        NodeKind::JsonObject => {
            let toks = c.own_tokens(n);
            let open_end = toks.first().map_or(0, |&t| c.span(t).1);
            let close_start = toks.last().map_or(0, |&t| c.span(t).0);
            let members = c
                .tree
                .child_nodes(n)
                .filter(|&m| c.kind(m) == NodeKind::JsonMember)
                .filter_map(|m| member(c, m))
                .collect();
            Val::Obj(Obj {
                members,
                open_end,
                close_start,
            })
        }
        NodeKind::JsonArray => Val::Arr(c.tree.child_nodes(n).map(|v| build(c, v)).collect()),
        NodeKind::JsonScalar => match c.own_tokens(n).first() {
            Some(&t) if c.token_kind(t) == TokenKind::String2 => {
                let (start, end) = c.span(t);
                Val::Str(Str {
                    value: json_string(c.text(t)),
                    start,
                    end,
                })
            }
            Some(&t) if c.text(t) == "null" => Val::Null,
            _ => Val::Other,
        },
        _ => Val::Other,
    }
}

fn member(c: &Cst<'_, '_>, m: NodeId) -> Option<Member> {
    let toks = c.own_tokens(m);
    let key = *toks.first()?;
    let value_node = c.tree.child_nodes(m).next()?;
    let (start, end) = c.span(key);
    let comma = toks
        .last()
        .filter(|&&t| c.token_kind(t) == TokenKind::Comma)
        .map(|&t| c.span(t));
    Some(Member {
        key: Str {
            value: json_string(c.text(key)),
            start,
            end,
        },
        value: build(c, value_node),
        start,
        end: c.node_span(value_node).1,
        comma,
    })
}

/// The value of a JSON string token: its quotes dropped and its escapes decoded,
/// surrogate pairs included.
pub(super) fn json_string(token: &str) -> String {
    let inner = token
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(token);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex = |it: &mut std::str::Chars<'_>| -> Option<u32> {
                    let h: String = it.by_ref().take(4).collect();
                    u32::from_str_radix(&h, 16).ok()
                };
                let first = hex(&mut chars).unwrap_or(0xfffd);
                let code = if (0xd800..0xdc00).contains(&first) {
                    let mut ahead = chars.clone();
                    match (ahead.next(), ahead.next(), hex(&mut ahead)) {
                        (Some('\\'), Some('u'), Some(low)) if (0xdc00..0xe000).contains(&low) => {
                            chars = ahead;
                            0x10000 + ((first - 0xd800) << 10) + (low - 0xdc00)
                        }
                        _ => 0xfffd,
                    }
                } else {
                    first
                };
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

// -------------------------------------------------------------------- contexts ------

/// What the contexts in effect say about names.
#[derive(Clone, Default)]
struct Ctx<'a> {
    terms: HashMap<String, Term<'a>>,
    /// a `@vocab` maps every key
    vocab: bool,
    /// a context given by URL (or `@import`) may define anything
    remote: bool,
    /// some context has been applied
    seen: bool,
}

#[derive(Clone, Default)]
struct Term<'a> {
    /// the IRI the term stands for, when it can be told
    iri: Option<String>,
    /// the datatype of its values, or `@id`, `@vocab`, `@json` or `@none`
    coerce: Option<String>,
    /// its container is a language, index, id or type map
    map: bool,
    /// its values are JSON literals
    json: bool,
    /// the context of its definition
    scoped: Option<&'a Val>,
}

/// Apply a `@context` value to `ctx`.
fn apply<'a>(ctx: &mut Ctx<'a>, v: &'a Val) {
    match v {
        Val::Str(_) => {
            ctx.remote = true;
            ctx.seen = true;
        }
        Val::Null => {
            *ctx = Ctx {
                seen: true,
                ..Ctx::default()
            }
        }
        Val::Arr(items) => items.iter().for_each(|i| apply(ctx, i)),
        Val::Obj(o) => {
            ctx.seen = true;
            let raw: HashMap<&str, &Val> = o
                .members
                .iter()
                .filter(|m| !m.key.value.starts_with('@'))
                .map(|m| (m.key.value.as_str(), &m.value))
                .collect();
            for m in &o.members {
                match m.key.value.as_str() {
                    "@vocab" => ctx.vocab = matches!(m.value, Val::Str(_)),
                    "@import" => ctx.remote = true,
                    _ => {}
                }
            }
            for m in o.members.iter().filter(|m| !m.key.value.starts_with('@')) {
                let t = define(&m.key.value, &m.value, &raw, ctx);
                ctx.terms.insert(m.key.value.clone(), t);
            }
        }
        Val::Other => {}
    }
}

fn define<'a>(key: &str, def: &'a Val, raw: &HashMap<&str, &Val>, ctx: &Ctx<'_>) -> Term<'a> {
    match def {
        Val::Str(s) => Term {
            iri: expand(&s.value, raw, ctx, 0),
            ..Term::default()
        },
        Val::Obj(o) => {
            let string = |k: &str| match o.get(k) {
                Some(Member {
                    value: Val::Str(s), ..
                }) => Some(s.value.as_str()),
                _ => None,
            };
            let iri = match (o.get("@id"), string("@id")) {
                (_, Some(id)) => expand(id, raw, ctx, 0),
                (Some(_), None) => None,
                (None, None) if key.contains(':') => expand(key, raw, ctx, 0),
                (None, None) => None,
            };
            let coerce = string("@type").and_then(|t| {
                if t.starts_with('@') {
                    Some(t.to_string())
                } else {
                    expand(t, raw, ctx, 0)
                }
            });
            let containers: Vec<&str> = match o.get("@container").map(|m| &m.value) {
                Some(Val::Str(s)) => vec![s.value.as_str()],
                Some(Val::Arr(a)) => a
                    .iter()
                    .filter_map(|v| match v {
                        Val::Str(s) => Some(s.value.as_str()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Term {
                iri,
                map: containers
                    .iter()
                    .any(|c| matches!(*c, "@language" | "@index" | "@id" | "@type")),
                json: coerce.as_deref() == Some("@json"),
                coerce,
                scoped: o.get("@context").map(|m| &m.value),
            }
        }
        _ => Term::default(),
    }
}

/// The IRI a name stands for: an absolute IRI as it is, a compact IRI through its
/// prefix, a term through its definition. A name relative to `@vocab` is unknown.
fn expand(s: &str, raw: &HashMap<&str, &Val>, ctx: &Ctx<'_>, depth: usize) -> Option<String> {
    if depth > 8 || s.starts_with('@') {
        return None;
    }
    if let Some((prefix, rest)) = s.split_once(':') {
        if rest.starts_with("//") {
            return Some(s.to_string());
        }
        if prefix == "_" {
            return None;
        }
        return Some(match term_iri(prefix, raw, ctx, depth) {
            Some(ns) => format!("{ns}{rest}"),
            None => s.to_string(),
        });
    }
    term_iri(s, raw, ctx, depth)
}

fn term_iri(name: &str, raw: &HashMap<&str, &Val>, ctx: &Ctx<'_>, depth: usize) -> Option<String> {
    match raw.get(name) {
        Some(Val::Str(s)) => expand(&s.value, raw, ctx, depth + 1),
        Some(Val::Obj(o)) => match o.get("@id") {
            Some(Member {
                value: Val::Str(s), ..
            }) => expand(&s.value, raw, ctx, depth + 1),
            _ => None,
        },
        Some(_) => None,
        None => ctx.terms.get(name).and_then(|t| t.iri.clone()),
    }
}

impl Ctx<'_> {
    fn expand(&self, name: &str) -> Option<String> {
        expand(name, &HashMap::new(), self, 0)
    }
}

// ------------------------------------------------------------------------ walk ------

struct Walk<'o, 'c> {
    out: &'o mut Out<'c>,
}

impl<'a> Walk<'_, '_> {
    fn value(&mut self, v: &'a Val, ctx: &Ctx<'a>) {
        match v {
            Val::Obj(o) => self.object(o, ctx),
            Val::Arr(items) => items.iter().for_each(|i| self.value(i, ctx)),
            _ => {}
        }
    }

    fn object(&mut self, o: &'a Obj, ctx: &Ctx<'a>) {
        let mut here = Cow::Borrowed(ctx);
        if let Some(m) = o.get("@context") {
            let mut c = ctx.clone();
            apply(&mut c, &m.value);
            here = Cow::Owned(c);
        }
        if o.get("@value").is_some() {
            self.value_object(o, &here);
            return;
        }
        // the contexts of the object's types apply to its keys, and not to what is below
        let mut keys = Cow::Borrowed(&*here);
        for ty in o
            .get("@type")
            .map(|m| strings(&m.value))
            .unwrap_or_default()
        {
            if let Some(scoped) = here.terms.get(ty).and_then(|t| t.scoped) {
                let mut c = keys.into_owned();
                apply(&mut c, scoped);
                keys = Cow::Owned(c);
            }
        }
        for m in &o.members {
            let k = m.key.value.as_str();
            if k == "@context" {
                continue;
            }
            if k.starts_with('@') {
                self.keyword(k, &m.value, &here);
                continue;
            }
            let term = keys.terms.get(k);
            if term.is_none() && !k.contains(':') && !keys.vocab && !keys.remote && keys.seen {
                self.out.report(
                    "unmapped-key",
                    m.key.start,
                    m.key.end,
                    format!(
                        "no context maps the key \"{k}\", so JSON-LD drops it and its value; \
                         define it in the @context or write it as an IRI"
                    ),
                );
            }
            self.property(term, &m.value, &here);
        }
    }

    fn keyword(&mut self, k: &str, v: &'a Val, ctx: &Ctx<'a>) {
        match k {
            "@id" | "@type" => {
                for s in strings_of(v) {
                    self.iri(s, ctx);
                }
            }
            "@graph" | "@included" | "@reverse" | "@nest" | "@list" | "@set" => self.value(v, ctx),
            _ => {}
        }
    }

    /// The value of a property, under the term that names it (if it is one).
    fn property(&mut self, term: Option<&Term<'a>>, v: &'a Val, ctx: &Ctx<'a>) {
        let Some(t) = term else {
            return self.value(v, ctx);
        };
        if t.json {
            return;
        }
        let mut child = Cow::Borrowed(ctx);
        if let Some(scoped) = t.scoped {
            let mut c = ctx.clone();
            apply(&mut c, scoped);
            child = Cow::Owned(c);
        }
        if t.map {
            // the keys of a map are data; its values are nodes or strings
            if let Val::Obj(map) = v {
                map.members
                    .iter()
                    .for_each(|m| self.value(&m.value, &child));
            }
            return;
        }
        if let Some(local) = t.coerce.as_deref().and_then(|d| d.strip_prefix(terms::XSD))
            && local != "string"
            && terms::XSD_TYPES.contains(&local)
        {
            for s in strings_of(v) {
                terms::lexical_check(local, &s.value, s.start, s.end, self.out);
            }
        }
        self.value(v, &child);
    }

    /// An IRI written as the value of `@id` or `@type`.
    fn iri(&mut self, s: &Str, ctx: &Ctx<'_>) {
        let resolved = ctx.expand(&s.value);
        terms::iri_checks(
            resolved.as_deref().unwrap_or(&s.value),
            s.start,
            s.end,
            self.out,
        );
    }

    /// `{ "@value": …, "@type": …, "@language": … }`
    fn value_object(&mut self, o: &Obj, ctx: &Ctx<'_>) {
        if let Some(Member {
            value: Val::Str(l), ..
        }) = o.get("@language")
        {
            terms::language_tag_at(&l.value, l.start, l.end, &|t| format!("\"{t}\""), self.out);
        }
        let Some(ty) = o.members.iter().position(|m| m.key.value == "@type") else {
            return;
        };
        let Val::Str(t) = &o.members[ty].value else {
            return;
        };
        if t.value.starts_with('@') {
            return;
        }
        let Some(iri) = ctx.expand(&t.value) else {
            return;
        };
        self.iri(t, ctx);
        let Some(Member {
            value: Val::Str(lexical),
            ..
        }) = o.get("@value")
        else {
            return;
        };
        let m = &o.members[ty];
        terms::datatype_site(
            &terms::Site {
                lexical: &lexical.value,
                lexical_span: (lexical.start, lexical.end),
                datatype_text: &t.value,
                datatype_span: (t.start, t.end),
                redundant_span: (m.start, m.end),
                drop: o.removal(ty),
                drop_title: format!("Remove \"@type\": \"{}\"", t.value),
            },
            &iri,
            self.out,
        );
    }
}

/// The strings of a string or an array of strings.
fn strings_of(v: &Val) -> Vec<&Str> {
    match v {
        Val::Str(s) => vec![s],
        Val::Arr(items) => items
            .iter()
            .filter_map(|i| match i {
                Val::Str(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn strings(v: &Val) -> Vec<&str> {
    strings_of(v)
        .into_iter()
        .map(|s| s.value.as_str())
        .collect()
}

// ------------------------------------------------------------ the context rules ------

/// `unused-context-term`, and the IRIs the contexts define.
fn context_rules(root: &Val, out: &mut Out<'_>) {
    let mut scan = Scan::default();
    scan.value(root);
    for (o, i) in &scan.defs {
        let m = &o.members[*i];
        if scan.used.contains(&m.key.value) {
            continue;
        }
        let fix = (!scan.remote).then(|| Fix {
            title: format!("Remove the unused term {}", m.key.value),
            edits: vec![o.removal(*i)],
        });
        out.report_fix(
            "unused-context-term",
            m.start,
            m.end,
            format!(
                "the term \"{}\" is defined here but nothing in the document uses it",
                m.key.value
            ),
            fix,
        );
    }
    for s in &scan.iris {
        terms::iri_checks(&s.value, s.start, s.end, out);
    }
}

#[derive(Default)]
struct Scan<'a> {
    /// the term definitions: the object of the context and the index of the member
    defs: Vec<(&'a Obj, usize)>,
    /// every string of the document except the keys that define terms, and the prefix of
    /// each compact IRI
    used: HashSet<String>,
    /// a context is given by URL
    remote: bool,
    /// the IRIs the contexts define
    iris: Vec<&'a Str>,
}

impl<'a> Scan<'a> {
    fn value(&mut self, v: &'a Val) {
        match v {
            Val::Str(s) => self.used(&s.value),
            Val::Arr(items) => items.iter().for_each(|i| self.value(i)),
            Val::Obj(o) => self.object(o, false),
            _ => {}
        }
    }

    fn used(&mut self, s: &str) {
        if let Some((prefix, _)) = s.split_once(':') {
            self.used.insert(prefix.to_string());
        }
        self.used.insert(s.to_string());
    }

    fn object(&mut self, o: &'a Obj, is_context: bool) {
        for (i, m) in o.members.iter().enumerate() {
            let k = m.key.value.as_str();
            let definition = is_context && !k.starts_with('@');
            if definition {
                self.defs.push((o, i));
                self.definition(&m.value);
            } else {
                self.used(k);
                if is_context {
                    match k {
                        "@import" => self.remote = true,
                        "@vocab" | "@base" => {
                            if let Val::Str(s) = &m.value {
                                self.iris.push(s);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if k == "@context" {
                self.context(&m.value);
            } else {
                self.value(&m.value);
            }
        }
    }

    /// The IRIs a term definition holds.
    fn definition(&mut self, def: &'a Val) {
        match def {
            Val::Str(s) => self.iris.push(s),
            Val::Obj(o) => {
                if let Some(Member {
                    value: Val::Str(s), ..
                }) = o.get("@id")
                {
                    self.iris.push(s);
                }
            }
            _ => {}
        }
    }

    fn context(&mut self, v: &'a Val) {
        match v {
            Val::Obj(o) => self.object(o, true),
            Val::Arr(items) => items.iter().for_each(|i| self.context(i)),
            Val::Str(_) => self.remote = true,
            _ => {}
        }
    }
}
