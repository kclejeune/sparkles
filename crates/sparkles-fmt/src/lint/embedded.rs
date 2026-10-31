//! The SPARQL inside SHACL: the plain string objects of `sh:select`, `sh:ask`,
//! `sh:construct` and `sh:update` in Turtle, TriG and JSON-LD documents are linted as
//! SPARQL (spec X04, G03). SHACLC has no syntax for these properties, and the line formats
//! are not read for them.
//!
//! A query sees the prefixes that `sh:prefixes` and `sh:declare` give it, including those
//! of the `owl:imports` closure of the nodes `sh:prefixes` names, and the prefixes of the
//! document itself. They are written in front of the query as `PREFIX` lines, and
//! everything found there is dropped again. Each literal is matched to the quad it makes,
//! so the prefixes and parameters are those of the node that holds it, in its graph, even
//! when another node holds the same text. A document prefix that a query writes counts as
//! used, so `unused-prefix` does not remove it. SHACL binds `$this`, `$shapesGraph`,
//! `$currentShape`, `$value` and `$PATH` before it runs a query, and a constraint
//! component's validators also see its parameters by their local names, so the variable
//! rules treat those as bound.
//!
//! A finding is mapped from the query text back into the literal through the table that
//! [`Decoded`] keeps for each decoded byte: where its source character starts and ends,
//! so an escape such as `\n` or `é` is covered as a whole. A fix is offered only for
//! a long string (`"""` or `'''`) without escapes, where the query text is the source text
//! and an edit lands exactly. A fix is also kept only if the edited query means what the
//! query meant and the string still ends where it did.

use super::{Cst, Edit, Fix, LintError, Out};
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::Element;
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term};
use std::collections::HashMap;

const SH: &str = "http://www.w3.org/ns/shacl#";

/// The properties whose plain string values are queries.
pub(super) const QUERY_PREDICATES: [&str; 4] = ["select", "ask", "construct", "update"];

/// The IRI of `sh:{local}`.
fn sh(local: &str) -> String {
    format!("{SH}{local}")
}

/// Whether `iri` is one of the query properties.
pub(super) fn is_query_predicate(iri: &str) -> bool {
    iri.strip_prefix(SH)
        .is_some_and(|l| QUERY_PREDICATES.contains(&l))
}

/// A string literal that holds a query, in the document.
pub(super) struct Site {
    /// the property's IRI
    pub predicate: String,
    pub text: Decoded,
    /// the string is `"""` or `'''`
    pub long: bool,
    /// the quote character of a long string
    pub quote: char,
}

/// The value of a string literal and, for each byte of it, where its source is.
pub(super) struct Decoded {
    pub value: String,
    /// for each byte of `value` and one past the end: where the source character that
    /// holds it starts in the document (past the end: where the content ends)
    starts: Vec<usize>,
    /// where that character ends (before the first byte: where the content starts)
    ends: Vec<usize>,
    /// the source holds no escape, so the value is the source text
    pub plain: bool,
}

struct Builder {
    value: String,
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl Builder {
    fn new(content_start: usize) -> Self {
        Builder {
            value: String::new(),
            starts: Vec::new(),
            ends: vec![content_start],
        }
    }

    /// `ch` decoded from the `len` source bytes at `at`.
    fn push(&mut self, ch: char, at: usize, len: usize) {
        self.value.push(ch);
        for _ in 0..ch.len_utf8() {
            self.starts.push(at);
            self.ends.push(at + len);
        }
    }

    fn finish(mut self, content_end: usize, plain: bool) -> Decoded {
        self.starts.push(content_end);
        Decoded {
            value: self.value,
            starts: self.starts,
            ends: self.ends,
            plain,
        }
    }
}

/// A Turtle string token at `at` in the document, decoded; and whether it is a long string.
pub(super) fn decode_turtle(token: &str, at: usize) -> (Decoded, bool, char) {
    let quote = token.chars().next().unwrap_or('"');
    let long = token.len() >= 6 && (token.starts_with("\"\"\"") || token.starts_with("'''"));
    let q = if long { 3 } else { 1 };
    let inner = &token[q..token.len() - q];
    let mut b = Builder::new(at + q);
    let mut i = 0;
    while i < inner.len() {
        let ch = inner[i..].chars().next().expect("a character");
        if ch != '\\' {
            b.push(ch, at + q + i, ch.len_utf8());
            i += ch.len_utf8();
            continue;
        }
        let rest = &inner[i + 1..];
        let (decoded, len) = match rest.chars().next() {
            Some('t') => ('\t', 2),
            Some('b') => ('\u{8}', 2),
            Some('n') => ('\n', 2),
            Some('r') => ('\r', 2),
            Some('f') => ('\u{c}', 2),
            Some(c @ ('"' | '\'' | '\\')) => (c, 2),
            Some(k @ ('u' | 'U')) => {
                let n = if k == 'u' { 4 } else { 8 };
                match rest
                    .get(1..1 + n)
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .and_then(char::from_u32)
                {
                    Some(c) => (c, 2 + n),
                    None => ('\\', 1),
                }
            }
            _ => ('\\', 1),
        };
        b.push(decoded, at + q + i, len);
        i += len;
    }
    let plain = !inner.contains('\\');
    (b.finish(at + q + inner.len(), plain), long, quote)
}

/// A JSON string token at `at` in the document, decoded.
pub(super) fn decode_json(token: &str, at: usize) -> Decoded {
    let inner = token
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(token);
    let mut b = Builder::new(at + 1);
    let hex4 = |s: &str| s.get(..4).and_then(|h| u32::from_str_radix(h, 16).ok());
    let mut i = 0;
    while i < inner.len() {
        let ch = inner[i..].chars().next().expect("a character");
        if ch != '\\' {
            b.push(ch, at + 1 + i, ch.len_utf8());
            i += ch.len_utf8();
            continue;
        }
        let rest = &inner[i + 1..];
        let (decoded, len) = match rest.chars().next() {
            Some('b') => ('\u{8}', 2),
            Some('f') => ('\u{c}', 2),
            Some('n') => ('\n', 2),
            Some('r') => ('\r', 2),
            Some('t') => ('\t', 2),
            Some(c @ ('"' | '\\' | '/')) => (c, 2),
            Some('u') => match rest.get(1..).and_then(hex4) {
                Some(hi @ 0xd800..=0xdbff) => {
                    let low = rest
                        .get(5..)
                        .and_then(|t| t.strip_prefix("\\u"))
                        .and_then(hex4)
                        .filter(|l| (0xdc00..0xe000).contains(l));
                    match low {
                        Some(lo) => (
                            char::from_u32(0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00))
                                .unwrap_or('\u{fffd}'),
                            12,
                        ),
                        None => ('\u{fffd}', 6),
                    }
                }
                Some(c) => (char::from_u32(c).unwrap_or('\u{fffd}'), 6),
                None => ('\\', 1),
            },
            _ => ('\\', 1),
        };
        b.push(decoded, at + 1 + i, len);
        i += len;
    }
    let plain = !inner.contains('\\');
    b.finish(at + 1 + inner.len(), plain)
}

// ---------------------------------------------------------------------- Turtle ------

/// The query literals of a Turtle or TriG document, and the prefixes it declares.
pub(super) fn turtle_sites(c: &Cst<'_, '_>) -> (Vec<Site>, Vec<(String, String)>) {
    let mut sites = Vec::new();
    let mut prefixes = Vec::new();
    for n in c.nodes() {
        match c.kind(n) {
            NodeKind::PrefixDecl => {
                let toks = c.own_tokens(n);
                if let (Some(&l), Some(&i)) = (
                    toks.iter()
                        .find(|&&t| c.token_kind(t) == TokenKind::PnameNs),
                    toks.iter().find(|&&t| c.token_kind(t) == TokenKind::IriRef),
                ) {
                    let label = c.text(l).trim_end_matches(':').to_string();
                    let iri = c.text(i);
                    prefixes.push((label, super::unescape_iri(&iri[1..iri.len() - 1])));
                }
            }
            NodeKind::PropertyListEntry => {
                let Some(&verb) = c.own_tokens(n).first() else {
                    continue;
                };
                let Some(predicate) = c.iri(verb).filter(|i| is_query_predicate(i)) else {
                    continue;
                };
                for obj in c
                    .tree
                    .child_nodes(n)
                    .filter(|&o| c.kind(o) == NodeKind::Object)
                {
                    let Some(string) = plain_string(c, obj) else {
                        continue;
                    };
                    let (s, _) = c.span(string);
                    let (text, long, quote) = decode_turtle(c.text(string), s);
                    sites.push(Site {
                        predicate: predicate.clone(),
                        text,
                        long,
                        quote,
                    });
                }
            }
            _ => {}
        }
    }
    (sites, prefixes)
}

/// The prefix labels the queries of `sites` write, so that the document's declarations
/// of them count as used. A label that a query declares itself counts too, which only
/// keeps a declaration that could have gone.
pub(super) fn labels(sites: &[Site]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for site in sites {
        let q = site.text.value.as_str();
        for t in crate::lex::lex(q, crate::lex::LexMode::Sparql) {
            if matches!(t.kind, TokenKind::PnameNs | TokenKind::PnameLn) {
                let s = t.text(q);
                out.insert(s[..s.find(':').unwrap_or(s.len())].to_string());
            }
        }
    }
    out
}

/// Lint the queries of a Turtle or TriG document.
pub(super) fn turtle(
    sites: Vec<Site>,
    prefixes: &[(String, String)],
    text: &str,
    trig: bool,
    undeclared: &[String],
    out: &mut Out<'_>,
) -> Result<(), LintError> {
    run(
        sites,
        prefixes,
        || super::turtle_quads(text, trig, undeclared).ok(),
        out,
    )
}

/// The string token of an object that is a plain string: one without a language tag, or
/// with the datatype `xsd:string`.
fn plain_string(c: &Cst<'_, '_>, obj: crate::tree::NodeId) -> Option<crate::tree::TokenId> {
    match *c.tree.children(obj).first()? {
        Element::Token(t) if c.token_kind(t).is_string() => Some(t),
        Element::Node(lit) if c.kind(lit) == NodeKind::Literal => {
            let toks = c.own_tokens(lit);
            match toks.as_slice() {
                [string, hat, dt]
                    if c.token_kind(*hat) == TokenKind::HatHat
                        && c.iri(*dt).as_deref()
                            == Some("http://www.w3.org/2001/XMLSchema#string") =>
                {
                    Some(*string)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

// ----------------------------------------------------------------------- facts ------

/// A node in its graph.
type Node = (GraphName, NamedOrBlankNode);

/// What the document says about the nodes that hold queries.
pub(super) struct Facts {
    by_subject: HashMap<Node, Vec<(String, Term)>>,
    parents: HashMap<Node, Vec<NamedOrBlankNode>>,
    /// the holders of each query property and query text, in the order of the quads,
    /// which is the order of the literals in the document
    holders: HashMap<(String, String), Vec<Node>>,
}

/// The prefixes and parameters a query sees.
#[derive(Default)]
struct Info {
    prefixes: Vec<(String, String)>,
    params: Vec<String>,
}

fn node(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Some(NamedOrBlankNode::BlankNode(b.clone())),
        _ => None,
    }
}

/// The value of a literal without a language tag.
fn plain(t: &Term) -> Option<&str> {
    match t {
        Term::Literal(l) if l.language().is_none() => Some(l.value()),
        _ => None,
    }
}

impl Facts {
    pub fn new(quads: &[Quad]) -> Facts {
        let mut f = Facts {
            by_subject: HashMap::new(),
            parents: HashMap::new(),
            holders: HashMap::new(),
        };
        for q in quads {
            let p = q.predicate.as_str();
            let g = q.graph_name.clone();
            f.by_subject
                .entry((g.clone(), q.subject.clone()))
                .or_default()
                .push((p.to_string(), q.object.clone()));
            if let Some(child) = node(&q.object) {
                f.parents
                    .entry((g.clone(), child))
                    .or_default()
                    .push(q.subject.clone());
            }
            if is_query_predicate(p)
                && let Some(v) = plain(&q.object)
            {
                f.holders
                    .entry((p.to_string(), v.to_string()))
                    .or_default()
                    .push((g, q.subject.clone()));
            }
        }
        f
    }

    /// The holders of the `nth` site with this predicate and query text, of `count`
    /// such sites. The literals and the quads come in the same order, so the `nth` quad
    /// is the site's. When the counts differ, which a construct the sites do not see could
    /// cause, every holder of the text is taken.
    fn holders(&self, predicate: &str, query: &str, nth: usize, count: usize) -> &[Node] {
        let all = self
            .holders
            .get(&(predicate.to_string(), query.to_string()))
            .map_or(&[][..], Vec::as_slice);
        if all.len() == count {
            &all[nth..=nth]
        } else {
            all
        }
    }

    /// The objects of `subject`'s `predicate` in `graph`.
    fn objects(
        &self,
        graph: &GraphName,
        subject: &NamedOrBlankNode,
        predicate: &str,
    ) -> Vec<&Term> {
        self.by_subject
            .get(&(graph.clone(), subject.clone()))
            .map(|v| {
                v.iter()
                    .filter(|(p, _)| p == predicate)
                    .map(|(_, o)| o)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The prefixes and parameters the queries of `holders` see.
    fn info(&self, holders: &[Node]) -> Info {
        let mut info = Info::default();
        for (g, s) in holders {
            // the node itself and the nodes that point at it (the constraint of a shape,
            // the component of a validator), in its graph
            let mut holders = vec![s.clone()];
            holders.extend(
                self.parents
                    .get(&(g.clone(), s.clone()))
                    .cloned()
                    .unwrap_or_default(),
            );
            let mut declarers: Vec<NamedOrBlankNode> = Vec::new();
            for h in &holders {
                for o in self.objects(g, h, &sh("prefixes")) {
                    declarers.extend(node(o));
                }
                for p in self.objects(g, h, &sh("parameter")) {
                    let Some(p) = node(p) else { continue };
                    for path in self.objects(g, &p, &sh("path")) {
                        if let Term::NamedNode(n) = path {
                            let name = n.as_str().rsplit(['#', '/', ':']).next().unwrap_or("");
                            if !name.is_empty() && !info.params.iter().any(|x| x == name) {
                                info.params.push(name.to_string());
                            }
                        }
                    }
                }
            }
            // the owl:imports closure
            let imports = "http://www.w3.org/2002/07/owl#imports";
            let mut at = 0;
            while at < declarers.len() {
                let d = declarers[at].clone();
                for o in self.objects(g, &d, imports) {
                    if let Some(n) = node(o)
                        && !declarers.contains(&n)
                    {
                        declarers.push(n);
                    }
                }
                at += 1;
            }
            for d in &declarers {
                for decl in self.objects(g, d, &sh("declare")) {
                    let Some(decl) = node(decl) else { continue };
                    let value = |p: &str| {
                        self.objects(g, &decl, &sh(p))
                            .into_iter()
                            .find_map(|t| match t {
                                Term::Literal(l) => Some(l.value().to_string()),
                                _ => None,
                            })
                    };
                    if let (Some(prefix), Some(ns)) = (value("prefix"), value("namespace"))
                        && !info.prefixes.contains(&(prefix.clone(), ns.clone()))
                    {
                        info.prefixes.push((prefix, ns));
                    }
                }
            }
        }
        info
    }
}

// ------------------------------------------------------------------------- run ------

/// A prefix label and namespace that can be written in a `PREFIX` line.
fn writable(label: &str, ns: &str) -> bool {
    let mut chars = label.chars();
    let label_ok = match chars.next() {
        None => true,
        Some(c) => {
            c.is_ascii_alphabetic()
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }
    };
    label_ok
        && !ns
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "<>\"{}|^`\\".contains(c))
}

/// Lint the query of each site as SPARQL and report the findings in the document.
/// `document_prefixes` are the prefixes the document declares itself, and `quads` reads
/// the document's triples for the `sh:prefixes` and `sh:declare` of the queries.
pub(super) fn run(
    sites: Vec<Site>,
    document_prefixes: &[(String, String)],
    quads: impl FnOnce() -> Option<Vec<Quad>>,
    out: &mut Out<'_>,
) -> Result<(), LintError> {
    if sites.is_empty() {
        return Ok(());
    }
    let facts = quads().map(|q| Facts::new(&q));
    // each site's place among the sites with its property and text
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    let nth: Vec<usize> = sites
        .iter()
        .map(|s| {
            let n = counts
                .entry((s.predicate.as_str(), s.text.value.as_str()))
                .or_default();
            *n += 1;
            *n - 1
        })
        .collect();
    for (site, &nth) in sites.iter().zip(&nth) {
        out.deadline()?;
        let query = site.text.value.as_str();
        let count = counts[&(site.predicate.as_str(), query)];
        let info = facts
            .as_ref()
            .map(|f| f.info(f.holders(&site.predicate, query, nth, count)))
            .unwrap_or_default();
        let mut prelude = String::new();
        for (label, ns) in document_prefixes.iter().chain(&info.prefixes) {
            if writable(label, ns) {
                prelude.push_str(&format!("PREFIX {label}: <{ns}>\n"));
            }
        }
        let mut prebound: Vec<String> = ["this", "shapesGraph", "currentShape", "value", "PATH"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        prebound.extend(info.params);
        let combined = format!("{prelude}{query}");
        let found = super::lint_embedded(&combined, out.opts, prebound)?;
        for d in found {
            // the prefixes written in front are not the document's to report
            if d.start < prelude.len() {
                continue;
            }
            let (vs, ve) = (d.start - prelude.len(), d.end.max(d.start) - prelude.len());
            let start = site.text.starts[vs.min(site.text.value.len())];
            let end = if ve > vs {
                site.text.ends[ve.min(site.text.value.len())]
            } else {
                start
            };
            let fix = d
                .fix
                .as_ref()
                .and_then(|f| map_fix(f, site, &prelude, &combined));
            out.report_fix(d.rule, start, end, d.message, fix);
        }
    }
    Ok(())
}

/// A fix in the query as a fix in the document, for a long string without escapes, if the
/// edited query still means the same and the string still ends where it did.
fn map_fix(fix: &Fix, site: &Site, prelude: &str, combined: &str) -> Option<Fix> {
    if !(site.long && site.text.plain) {
        return None;
    }
    let t = &site.text;
    let mut edits: Vec<Edit> = Vec::new();
    let mut value_edits: Vec<Edit> = Vec::new();
    for e in &fix.edits {
        if e.start < prelude.len() || e.insert.contains(['\\', '"', '\'']) {
            return None;
        }
        let (vs, ve) = (e.start - prelude.len(), e.end - prelude.len());
        if ve > t.value.len() {
            return None;
        }
        edits.push(Edit {
            start: t.starts[vs],
            end: if ve > vs { t.ends[ve] } else { t.starts[ve] },
            insert: e.insert.clone(),
        });
        value_edits.push(Edit {
            start: vs,
            end: ve,
            insert: e.insert.clone(),
        });
    }
    value_edits.sort_by_key(|e| e.start);
    let edited = super::apply(&t.value, &value_edits);
    let delimiter: String = std::iter::repeat_n(site.quote, 3).collect();
    if edited.contains(&delimiter) || edited.ends_with(site.quote) {
        return None;
    }
    let after = format!("{prelude}{edited}");
    if !super::same_meaning(combined, &after, crate::Language::Sparql) {
        return None;
    }
    Some(Fix {
        title: fix.title.clone(),
        edits,
    })
}
