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
//! context. `@vocab` maps every key, and its IRI expands the names relative to it. A
//! term that aliases a keyword stands for the keyword. The values of a term with a map
//! container and of a value object are data, not properties. A JSON literal, the value of
//! a term typed `@json` or of a value object whose type is `@json`, is not read at all, so
//! nothing in it counts as a context, a term or a use of one.
//!
//! The model is only a guide. Which values are SHACL queries, and which node holds each, is
//! read from oxjsonld's expansion of the document, and every fix is kept only when the
//! document with that one edit expands to the same dataset. A document that cannot be
//! expanded here, because it names a context by URL, gets neither queries nor fixes.

use super::{Cst, Edit, Fix, LintError, Out, embedded, terms};
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
    let before = out.found.len();
    let mut walk = Walk {
        out,
        json: HashSet::new(),
    };
    walk.value(&root, &Ctx::default());
    // the values of JSON literals are data: no term is defined or used in them
    let mut scan = Scan {
        json: std::mem::take(&mut walk.json),
        ..Scan::default()
    };
    scan.value(&root);
    out.deadline()?;
    // the real expansion decides which values are queries and who holds them
    // the clock is read only with a deadline: the browser's build has none, and no clock
    let started = out.opts.deadline.map(|_| std::time::Instant::now());
    // a document over the limit is not expanded at all: nothing here would use it
    let quads = match text.len() <= PROBE_LIMIT {
        true => super::jsonld_quads_within(text, out.opts.deadline)?,
        false => None,
    };
    let mut work = Work::new(text.len());
    if let (Some(started), Some(deadline)) = (started, out.opts.deadline) {
        work.expansion_takes(started.elapsed(), deadline);
    }
    if let Some(quads) = &quads {
        let mut strings: HashMap<&str, Vec<&Str>> = HashMap::new();
        value_strings(&root, &mut strings);
        let facts = embedded::Facts::new(quads, out.opts.deadline)?;
        let sites = query_sites(quads, &facts, &strings, text, out.opts.deadline, &mut work)?;
        // every site has its holder, so no count of source texts is needed
        embedded::run(sites, &scan.prefixes(), &facts, &HashMap::new(), out)?;
    }
    context_rules(&scan, out);
    gate_fixes(
        text,
        quads.as_deref(),
        &mut out.found[before..],
        out.opts.deadline,
        &mut work,
    )
}

/// The largest JSON-LD document whose queries are probed and whose fixes are checked.
/// A larger one gets no query findings and no fixes.
pub(super) const PROBE_LIMIT: usize = 8 << 20;

/// The work the probes and fix checks of one document may do together. Each expands the
/// whole document again. Together they expand at most four times the document, or 1 MiB
/// for a small one, and never more than 64 MiB, and the budget holds with no deadline too,
/// as in the browser. With a deadline, a probe or check is also not started when one
/// expansion of the document, as long as the document's own took, would end past the
/// deadline, because an expansion that must see the whole document before its first quad
/// cannot stop early. One that does not fit is skipped, so its query gets no findings or
/// its finding no fix.
pub(super) struct Work {
    left: usize,
    /// how long one expansion of the document takes, and the deadline
    expansion: Option<(std::time::Duration, std::time::Instant)>,
}

impl Work {
    pub(super) fn new(len: usize) -> Work {
        Work {
            left: len.saturating_mul(4).clamp(1 << 20, 64 << 20),
            expansion: None,
        }
    }

    /// One expansion of the document takes `took`, and the work ends at `deadline`.
    pub(super) fn expansion_takes(
        &mut self,
        took: std::time::Duration,
        deadline: std::time::Instant,
    ) {
        self.expansion = Some((took, deadline));
    }

    /// Take `bytes` of the budget, or `false` when they do not fit or there is no time
    /// for one more expansion.
    fn spend(&mut self, bytes: usize) -> bool {
        if bytes > self.left {
            return false;
        }
        if let Some((took, deadline)) = self.expansion
            && std::time::Instant::now() + took > deadline
        {
            return false;
        }
        self.left -= bytes;
        #[cfg(test)]
        PROBED.with(|p| p.set(p.get() + bytes));
        true
    }
}

#[cfg(test)]
thread_local! {
    /// The bytes [`Work`] has let this thread expand, for the tests.
    pub(super) static PROBED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The most fixes [`gate_fixes`] checks one by one. The fixes of a document with more
/// findings are dropped, and the findings stay.
const GATED_FIXES: usize = 256;

/// Keep a fix of a JSON-LD finding only when the document with that one edit applied
/// expands to a dataset isomorphic to the document's own, within the deadline. The rules
/// read JSON-LD with a model of its contexts, and this check is what makes their edits
/// safe however the model is wrong: an edit inside a JSON literal, through a context the
/// model missed, changes the dataset and is dropped. A document that cannot be expanded
/// here gets no fixes.
fn gate_fixes(
    text: &str,
    quads: Option<&[oxrdf::Quad]>,
    found: &mut [super::Diagnostic],
    deadline: Option<std::time::Instant>,
    work: &mut Work,
) -> Result<(), LintError> {
    let fixable = found.iter().filter(|d| d.fix.is_some()).count();
    for d in found.iter_mut().filter(|d| d.fix.is_some()) {
        let keep = match quads {
            Some(before) if fixable <= GATED_FIXES => {
                let mut edits = d.fix.as_ref().expect("a fix").edits.clone();
                edits.sort_by_key(|e| e.start);
                let edited = super::apply(text, &edits);
                if !work.spend(edited.len()) {
                    d.fix = None;
                    continue;
                }
                match super::jsonld_quads_within(&edited, deadline)? {
                    Some(after) => {
                        match crate::check::graph::isomorphic_bounded(before, &after, deadline) {
                            Ok(same) => same == Some(true),
                            Err(_) => return Err(LintError::Timeout),
                        }
                    }
                    None => false,
                }
            }
            _ => false,
        };
        if !keep {
            d.fix = None;
        }
    }
    Ok(())
}

/// Every string of the document that is a value, not a key, by its value.
fn value_strings<'v>(v: &'v Val, out: &mut HashMap<&'v str, Vec<&'v Str>>) {
    match v {
        Val::Str(s) => out.entry(s.value.as_str()).or_default().push(s),
        Val::Arr(items) => items.iter().for_each(|i| value_strings(i, out)),
        Val::Obj(o) => o.members.iter().for_each(|m| value_strings(&m.value, out)),
        Val::Null | Val::Other => {}
    }
}

/// The most source strings [`query_sites`] probes.
const PROBES: usize = 256;

/// The SHACL queries of the expanded document that map back to the source without doubt:
/// an `xsd:string` object of a query property whose text is written by exactly one value
/// string of the document and held by exactly one node, and that string is the one that
/// makes the quad. That is proved by a probe: the document with the string replaced by a
/// fresh text expands to the same dataset with only that quad's literal replaced. A query
/// made from anything else, such as the key of an `@index` map, fails the probe. Each query
/// that passes becomes a site with its holder, and any other query is skipped.
fn query_sites(
    quads: &[oxrdf::Quad],
    facts: &embedded::Facts,
    strings: &HashMap<&str, Vec<&Str>>,
    text: &str,
    deadline: Option<std::time::Instant>,
    work: &mut Work,
) -> Result<Vec<embedded::Site>, LintError> {
    // how many quads have each query property and text, and how many there are in all
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for q in quads {
        if let Some(v) = embedded::query_text(&q.object)
            .filter(|_| embedded::is_query_predicate(q.predicate.as_str()))
        {
            *counts.entry((q.predicate.as_str(), v)).or_default() += 1;
        }
    }
    let mut seen = HashSet::new();
    let mut sites = Vec::new();
    let mut probes = 0;
    for q in quads {
        let p = q.predicate.as_str();
        let Some(v) = embedded::is_query_predicate(p)
            .then(|| embedded::query_text(&q.object))
            .flatten()
        else {
            continue;
        };
        if !seen.insert((p, v)) {
            continue;
        }
        let (Some(holder), Some([s])) =
            (facts.only_holder(p, v), strings.get(v).map(Vec::as_slice))
        else {
            continue;
        };
        if probes == PROBES {
            break;
        }
        probes += 1;
        if !probe(
            text,
            s,
            p,
            probes,
            (counts[&(p, v)], quads.len()),
            deadline,
            work,
        )? {
            continue;
        }
        sites.push(embedded::Site {
            predicate: p.to_string(),
            text: embedded::decode_json(&text[s.start..s.end], s.start),
            long: false,
            quote: '"',
            holder: Some(holder.clone()),
        });
    }
    sites.sort_by_key(|s| s.text.start());
    Ok(sites)
}

/// Whether the value string `s` is what makes the quad of `predicate` with its text. The
/// document with `s` replaced by a fresh text is expanded, streaming under the deadline
/// and without keeping its quads, and it must have as many quads as before (`counts.1`),
/// one fewer with `predicate` and the old text than the `counts.0` there were, and exactly
/// one with the fresh text, under `predicate`. Only one node holds the old text, so that
/// quad is the holder's. A probe that does not fit the `work` budget is not made, and the
/// answer is `false`.
pub(super) fn probe(
    text: &str,
    s: &Str,
    predicate: &str,
    n: usize,
    counts: (usize, usize),
    deadline: Option<std::time::Instant>,
    work: &mut Work,
) -> Result<bool, LintError> {
    let fresh = format!("urn:x-sparkles-lint-probe:{n}");
    if text.contains(&fresh) {
        return Ok(false);
    }
    let probed = format!("{}\"{fresh}\"{}", &text[..s.start], &text[s.end..]);
    if !work.spend(probed.len()) {
        return Ok(false);
    }
    let (mut total, mut old, mut new, mut elsewhere) = (0usize, 0usize, 0usize, false);
    let expanded = super::jsonld_stream(&probed, deadline, |q| {
        total += 1;
        let Some(v) = embedded::query_text(&q.object) else {
            return;
        };
        if v == fresh {
            if q.predicate.as_str() == predicate {
                new += 1;
            } else {
                elsewhere = true;
            }
        } else if v == s.value && q.predicate.as_str() == predicate {
            old += 1;
        }
    })?;
    Ok(expanded && !elsewhere && new == 1 && old + 1 == counts.0 && total == counts.1)
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
pub(super) struct Str {
    pub(super) value: String,
    pub(super) start: usize,
    pub(super) end: usize,
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
    /// the IRI of the `@vocab`, when it can be told
    vocab_iri: Option<String>,
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
    /// the keyword the term is an alias of
    keyword: Option<String>,
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
                match (m.key.value.as_str(), &m.value) {
                    ("@vocab", Val::Str(v)) => {
                        // an absolute or compact IRI; one relative to the base is unknown
                        let iri = if v.value.contains(':') {
                            expand(&v.value, &raw, ctx, 0, true)
                        } else {
                            None
                        };
                        ctx.vocab = true;
                        ctx.vocab_iri = iri.filter(|i| i.contains(':'));
                    }
                    ("@vocab", _) => {
                        ctx.vocab = false;
                        ctx.vocab_iri = None;
                    }
                    ("@import", _) => ctx.remote = true,
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
        Val::Str(s) if KEYWORDS.contains(&s.value.as_str()) => Term {
            keyword: Some(s.value.clone()),
            ..Term::default()
        },
        Val::Str(s) => Term {
            iri: expand(&s.value, raw, ctx, 0, true),
            ..Term::default()
        },
        Val::Obj(o) => {
            let string = |k: &str| match o.get(k) {
                Some(Member {
                    value: Val::Str(s), ..
                }) => Some(s.value.as_str()),
                _ => None,
            };
            if let Some(k) = string("@id").filter(|k| KEYWORDS.contains(k)) {
                // an alias of `@nest` may scope a context over what it nests
                return Term {
                    keyword: Some(k.to_string()),
                    scoped: o.get("@context").map(|m| &m.value),
                    ..Term::default()
                };
            }
            let iri = match (o.get("@id"), string("@id")) {
                (_, Some(id)) => expand(id, raw, ctx, 0, true),
                (Some(_), None) => None,
                (None, None) => implicit_iri(key, raw, ctx),
            };
            let coerce = string("@type").and_then(|t| {
                if t.starts_with('@') {
                    Some(t.to_string())
                } else {
                    expand(t, raw, ctx, 0, true)
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
                keyword: None,
                coerce,
                scoped: o.get("@context").map(|m| &m.value),
            }
        }
        _ => Term::default(),
    }
}

/// The IRI of a term defined without `@id`: a compact IRI or absolute IRI as written, or
/// the key relative to the vocabulary. An earlier definition of the same term does not
/// count, because this definition replaces it.
fn implicit_iri(key: &str, raw: &HashMap<&str, &Val>, ctx: &Ctx<'_>) -> Option<String> {
    if key.contains(':') {
        return expand(key, raw, ctx, 0, false);
    }
    ctx.vocab_iri.as_ref().map(|v| format!("{v}{key}"))
}

/// The keywords of JSON-LD 1.1 that a term can be an alias of.
const KEYWORDS: &[&str] = &[
    "@base",
    "@container",
    "@direction",
    "@graph",
    "@id",
    "@import",
    "@included",
    "@index",
    "@json",
    "@language",
    "@list",
    "@nest",
    "@none",
    "@prefix",
    "@propagate",
    "@protected",
    "@reverse",
    "@set",
    "@type",
    "@value",
    "@version",
    "@vocab",
];

/// The IRI a name stands for: an absolute IRI as it is, a compact IRI through its
/// prefix, a term through its definition. With `vocab`, as for keys, types and the IRIs
/// of term definitions, any other name is relative to the `@vocab` IRI. Without it, as
/// for `@id`, such a name is relative to the base and unknown here.
fn expand(
    s: &str,
    raw: &HashMap<&str, &Val>,
    ctx: &Ctx<'_>,
    depth: usize,
    vocab: bool,
) -> Option<String> {
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
    if vocab && (raw.contains_key(s) || ctx.terms.contains_key(s)) {
        return term_iri(s, raw, ctx, depth);
    }
    match (vocab, &ctx.vocab_iri) {
        (true, Some(v)) => Some(format!("{v}{s}")),
        _ => None,
    }
}

fn term_iri(name: &str, raw: &HashMap<&str, &Val>, ctx: &Ctx<'_>, depth: usize) -> Option<String> {
    match raw.get(name) {
        Some(Val::Str(s)) => expand(&s.value, raw, ctx, depth + 1, true),
        Some(Val::Obj(o)) => match o.get("@id") {
            Some(Member {
                value: Val::Str(s), ..
            }) => expand(&s.value, raw, ctx, depth + 1, true),
            Some(_) => None,
            None => expand(name, &HashMap::new(), ctx, depth + 1, true)
                .filter(|_| name.contains(':') || ctx.vocab_iri.is_some()),
        },
        Some(_) => None,
        None => ctx.terms.get(name).and_then(|t| t.iri.clone()),
    }
}

impl Ctx<'_> {
    /// A name relative to the vocabulary: a key, a type or a datatype.
    fn expand_vocab(&self, name: &str) -> Option<String> {
        expand(name, &HashMap::new(), self, 0, true)
    }

    /// A name relative to the document: the value of `@id`.
    fn expand_id(&self, name: &str) -> Option<String> {
        expand(name, &HashMap::new(), self, 0, false)
    }

    /// The keyword `key` is, written as it is or through an alias.
    fn keyword<'k>(&'k self, key: &'k str) -> Option<&'k str> {
        if key.starts_with('@') {
            return Some(key);
        }
        self.terms.get(key).and_then(|t| t.keyword.as_deref())
    }

    /// The member of `o` for keyword `kw`, written as it is or through an alias.
    fn find<'o>(&self, o: &'o Obj, kw: &str) -> Option<&'o Member> {
        o.members
            .iter()
            .find(|m| self.keyword(&m.key.value) == Some(kw))
    }
}

// ------------------------------------------------------------------------ walk ------

struct Walk<'o, 'c> {
    out: &'o mut Out<'c>,
    /// the spans of the members whose values are JSON literals
    json: HashSet<(usize, usize)>,
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
        if here.find(o, "@value").is_some() {
            self.value_object(o, &here);
            return;
        }
        // the contexts of the object's types apply to its keys, and not to what is below
        let mut keys = Cow::Borrowed(&*here);
        for ty in here
            .find(o, "@type")
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
            // `@type` is read in the object's own context, the other keys in the one its
            // types scope
            let kw = here
                .keyword(k)
                .filter(|kw| *kw == "@type")
                .or_else(|| keys.keyword(k));
            if let Some(kw) = kw {
                // an alias's scoped context applies to what it nests
                match keys.terms.get(k).and_then(|t| t.scoped) {
                    Some(scoped) => {
                        let mut c = (*here).clone();
                        apply(&mut c, scoped);
                        self.keyword(kw, &m.value, &c);
                    }
                    None => self.keyword(kw, &m.value, &here),
                }
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
            if term.is_some_and(|t| t.json) {
                self.json.insert((m.start, m.end));
                continue;
            }
            self.property(term, &m.value, &here);
        }
    }

    fn keyword(&mut self, k: &str, v: &'a Val, ctx: &Ctx<'a>) {
        match k {
            "@id" => {
                for s in strings_of(v) {
                    self.iri(s, ctx.expand_id(&s.value));
                }
            }
            "@type" => {
                for s in strings_of(v) {
                    self.iri(s, ctx.expand_vocab(&s.value));
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

    /// An IRI written as the value of `@id` or `@type`, and what it expands to.
    fn iri(&mut self, s: &Str, resolved: Option<String>) {
        terms::iri_checks(
            resolved.as_deref().unwrap_or(&s.value),
            s.start,
            s.end,
            self.out,
        );
    }

    /// `{ "@value": …, "@type": …, "@language": … }`, the keywords written as they are or
    /// through aliases
    fn value_object(&mut self, o: &Obj, ctx: &Ctx<'_>) {
        if let Some(Member {
            value: Val::Str(l), ..
        }) = ctx.find(o, "@language")
        {
            terms::language_tag_at(&l.value, l.start, l.end, &|t| format!("\"{t}\""), self.out);
        }
        let Some(ty) = o
            .members
            .iter()
            .position(|m| ctx.keyword(&m.key.value) == Some("@type"))
        else {
            return;
        };
        let Val::Str(t) = &o.members[ty].value else {
            return;
        };
        if ctx.keyword(&t.value) == Some("@json") {
            // a JSON literal: its value is data
            if let Some(v) = ctx.find(o, "@value") {
                self.json.insert((v.start, v.end));
            }
            return;
        }
        if t.value.starts_with('@') {
            return;
        }
        let Some(iri) = ctx.expand_vocab(&t.value) else {
            return;
        };
        self.iri(t, Some(iri.clone()));
        let Some(Member {
            value: Val::Str(lexical),
            ..
        }) = ctx.find(o, "@value")
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
                drop_title: format!("Remove \"{}\": \"{}\"", m.key.value, t.value),
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
fn context_rules(scan: &Scan<'_>, out: &mut Out<'_>) {
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
    /// the spans of the members whose values are JSON literals, which are not read
    json: HashSet<(usize, usize)>,
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
        // a string with spaces is text, such as a SHACL query, whose `label:name` words
        // use prefixes
        if s.contains(char::is_whitespace) {
            for (i, _) in s.match_indices(':') {
                let label: String = s[..i]
                    .chars()
                    .rev()
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                self.used.insert(label);
            }
        }
    }

    /// The prefixes the contexts declare: a term for an absolute IRI that ends in `/`, `#`
    /// or `:`, the first time a label is defined.
    fn prefixes(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for (o, i) in &self.defs {
            let m = &o.members[*i];
            let iri = match &m.value {
                Val::Str(s) => Some(s.value.as_str()),
                Val::Obj(d) => match d.get("@id") {
                    Some(Member {
                        value: Val::Str(s), ..
                    }) => Some(s.value.as_str()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(iri) = iri
                && iri.contains(':')
                && iri.ends_with(['/', '#', ':'])
                && !out.iter().any(|(l, _)| *l == m.key.value)
            {
                out.push((m.key.value.clone(), iri.to_string()));
            }
        }
        out
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
            if self.json.contains(&(m.start, m.end)) {
                continue;
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
