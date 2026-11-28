//! Safe, escaped HTML5 renderer for linked-data resource descriptions.
//!
//! All dynamic text, IRIs, language tags, datatypes and attribute values are strictly
//! HTML-escaped. Only `http://`, `https://` and relative `/$/ld...` URLs are rendered
//! as clickable `href` links; any other IRI scheme (such as `javascript:` or `data:`)
//! is rendered as plain escaped text. Output is bounded against `max_result_bytes` as
//! chunks are written so oversized pages abort without allocating past the limit.

use super::publication::{Field, field};
use super::{Description, Format, HostedTarget, MemoryCharge};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const SKOS_PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
const DCT_TITLE: &str = "http://purl.org/dc/terms/title";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Strict CSP for linked-data HTML pages: no scripts, no external subresources, no framing.
pub const HTML_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Whether `iri` may be placed in an HTML `href` attribute directly.
/// Only absolute `http://` and `https://` URLs without whitespace or control chars pass.
pub fn safe_external_href(iri: &str) -> bool {
    if iri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let lower = iri.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

pub(super) fn percent_encode_param(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Build a `/$/ld/doc` link for `iri` and `format`, preserving optional `version` and `at`.
pub fn doc_url(
    iri: &str,
    format: Format,
    version: Option<&str>,
    at: Option<&str>,
    extra_describe: &[(String, String)],
) -> String {
    let mut url = format!(
        "/$/ld/doc?iri={}&format={}",
        percent_encode_param(iri),
        format.param_name()
    );
    if let Some(v) = version {
        let _ = write!(url, "&version={}", percent_encode_param(v));
    }
    if let Some(a) = at {
        let _ = write!(url, "&at={}", percent_encode_param(a));
    }
    for (k, v) in extra_describe {
        let _ = write!(
            url,
            "&{}={}",
            percent_encode_param(k),
            percent_encode_param(v)
        );
    }
    url
}

fn compact_iri<'a>(
    iri: &'a str,
    prefixes: &'a BTreeMap<String, String>,
) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(&'a str, &'a str)> = None;
    for (prefix, ns) in prefixes {
        if let Some(local) = iri.strip_prefix(ns.as_str())
            && !local.is_empty()
            && !local.contains('/')
            && best.is_none_or(|(_, b_ns)| ns.len() > b_ns.len())
        {
            best = Some((prefix.as_str(), ns.as_str()));
        }
    }
    best.map(|(p, ns)| (p, &iri[ns.len()..]))
}

/// Bounded HTML writer enforcing both `max_result_bytes` (on logical payload length) and
/// the request's shared [`MemoryCharge`] (on backing buffer capacity) before every growth.
pub(crate) struct HtmlWriter<'a> {
    buf: String,
    limit: Option<u64>,
    charge: &'a mut MemoryCharge,
    charged: u64,
}

impl<'a> HtmlWriter<'a> {
    pub(crate) fn new(limit: Option<u64>, charge: &'a mut MemoryCharge) -> Self {
        Self {
            buf: String::new(),
            limit,
            charge,
            charged: 0,
        }
    }

    #[inline]
    fn reserve(&mut self, add: usize) -> sparkles::Result<()> {
        let Some(want) = self.buf.len().checked_add(add) else {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Memory,
                limit: self.limit.unwrap_or(u64::MAX),
                requested: u64::MAX,
            }));
        };
        if let Some(limit) = self.limit
            && want as u64 > limit
        {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::ResultBytes,
                limit,
                requested: want as u64,
            }));
        }
        if want > self.buf.capacity() {
            let target = want.max(self.buf.capacity().saturating_mul(2)).max(256);
            let delta = (target as u64).saturating_sub(self.charged);
            self.charge.charge(delta)?;
            self.buf.reserve_exact(target - self.buf.len());
            let actual = self.buf.capacity() as u64;
            let charged_now = self.charged.saturating_add(delta);
            self.charged = charged_now;
            if actual > charged_now {
                self.charge.charge(actual - charged_now)?;
                self.charged = actual;
            }
        }
        Ok(())
    }

    pub(crate) fn push_str(&mut self, s: &str) -> sparkles::Result<()> {
        self.reserve(s.len())?;
        self.buf.push_str(s);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    fn push_u64(&mut self, n: u64) -> sparkles::Result<()> {
        let s = n.to_string();
        self.push_str(&s)
    }

    /// Write `s` with HTML escaping directly into `self.buf`, checking `limit` and
    /// `charge` as slices are emitted so oversized literals abort immediately.
    pub(super) fn push_escaped(&mut self, s: &str) -> sparkles::Result<()> {
        let mut last = 0usize;
        for (i, c) in s.char_indices() {
            let repl = match c {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#39;",
                _ => continue,
            };
            if i > last {
                self.push_str(&s[last..i])?;
            }
            self.push_str(repl)?;
            last = i + c.len_utf8();
        }
        if last < s.len() {
            self.push_str(&s[last..])?;
        }
        Ok(())
    }

    pub(super) fn into_string(self) -> String {
        self.buf
    }
}

fn write_compact_or_iri(
    w: &mut HtmlWriter<'_>,
    iri: &str,
    prefixes: &BTreeMap<String, String>,
) -> sparkles::Result<()> {
    match compact_iri(iri, prefixes) {
        Some((p, local)) => {
            w.push_escaped(p)?;
            w.push_str(":")?;
            w.push_escaped(local)
        }
        None => w.push_escaped(iri),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_iri(
    w: &mut HtmlWriter<'_>,
    iri: &str,
    prefixes: &BTreeMap<String, String>,
    readable_targets: &[HostedTarget],
    version: Option<&str>,
    at: Option<&str>,
) -> sparkles::Result<()> {
    if let Some(hit) = super::classify_hosted_link(iri, readable_targets) {
        let mut href = format!("/$/ld?iri={}", percent_encode_param(iri));
        if hit.same_resource && !hit.is_version_iri {
            if let Some(v) = version {
                let _ = write!(href, "&version={}", percent_encode_param(v));
            }
            if let Some(a) = at {
                let _ = write!(href, "&at={}", percent_encode_param(a));
            }
        }
        w.push_str("<a href=\"")?;
        w.push_escaped(&href)?;
        w.push_str("\" title=\"")?;
        w.push_escaped(iri)?;
        w.push_str("\">")?;
        write_compact_or_iri(w, iri, prefixes)?;
        w.push_str("</a>")
    } else if safe_external_href(iri) {
        w.push_str("<a href=\"")?;
        w.push_escaped(iri)?;
        w.push_str("\" rel=\"noopener noreferrer\" title=\"")?;
        w.push_escaped(iri)?;
        w.push_str("\">")?;
        write_compact_or_iri(w, iri, prefixes)?;
        w.push_str("</a>")
    } else {
        w.push_str("<code title=\"")?;
        w.push_escaped(iri)?;
        w.push_str("\">")?;
        write_compact_or_iri(w, iri, prefixes)?;
        w.push_str("</code>")
    }
}

#[allow(clippy::too_many_arguments)]
fn write_subject(
    w: &mut HtmlWriter<'_>,
    subj: &NamedOrBlankNode,
    prefixes: &BTreeMap<String, String>,
    readable_targets: &[HostedTarget],
    version: Option<&str>,
    at: Option<&str>,
) -> sparkles::Result<()> {
    match subj {
        NamedOrBlankNode::NamedNode(n) => {
            write_iri(w, n.as_str(), prefixes, readable_targets, version, at)
        }
        NamedOrBlankNode::BlankNode(b) => {
            w.push_str("<code>_:")?;
            w.push_escaped(b.as_str())?;
            w.push_str("</code>")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_term(
    w: &mut HtmlWriter<'_>,
    term: &Term,
    prefixes: &BTreeMap<String, String>,
    readable_targets: &[HostedTarget],
    version: Option<&str>,
    at: Option<&str>,
) -> sparkles::Result<()> {
    match term {
        Term::NamedNode(n) => write_iri(w, n.as_str(), prefixes, readable_targets, version, at),
        Term::BlankNode(b) => {
            w.push_str("<code>_:")?;
            w.push_escaped(b.as_str())?;
            w.push_str("</code>")
        }
        Term::Literal(l) => {
            w.push_str("<span class=\"lit\">")?;
            w.push_escaped(l.value())?;
            w.push_str("</span>")?;
            if let Some(lang) = l.language() {
                w.push_str(" <span class=\"tag\">@")?;
                w.push_escaped(lang)?;
                w.push_str("</span>")?;
            } else {
                let dt = l.datatype().as_str();
                if dt != "http://www.w3.org/2001/XMLSchema#string" {
                    w.push_str(" <span class=\"tag\">^^")?;
                    write_compact_or_iri(w, dt, prefixes)?;
                    w.push_str("</span>")?;
                }
            }
            Ok(())
        }
        #[allow(unreachable_patterns)]
        other => {
            w.push_str("<code>")?;
            w.push_escaped(&other.to_string())?;
            w.push_str("</code>")
        }
    }
}

fn cmp_named_or_blank(a: &NamedOrBlankNode, b: &NamedOrBlankNode) -> Ordering {
    match (a, b) {
        (NamedOrBlankNode::NamedNode(x), NamedOrBlankNode::NamedNode(y)) => {
            x.as_str().cmp(y.as_str())
        }
        (NamedOrBlankNode::BlankNode(x), NamedOrBlankNode::BlankNode(y)) => {
            x.as_str().cmp(y.as_str())
        }
        (NamedOrBlankNode::NamedNode(_), NamedOrBlankNode::BlankNode(_)) => Ordering::Less,
        (NamedOrBlankNode::BlankNode(_), NamedOrBlankNode::NamedNode(_)) => Ordering::Greater,
    }
}

fn cmp_term(a: &Term, b: &Term) -> Ordering {
    match (a, b) {
        (Term::NamedNode(x), Term::NamedNode(y)) => x.as_str().cmp(y.as_str()),
        (Term::BlankNode(x), Term::BlankNode(y)) => x.as_str().cmp(y.as_str()),
        (Term::Literal(x), Term::Literal(y)) => (
            x.value(),
            x.language().unwrap_or(""),
            x.datatype().as_str(),
        )
            .cmp(&(y.value(), y.language().unwrap_or(""), y.datatype().as_str())),
        _ => {
            let tag = |t: &Term| match t {
                Term::NamedNode(_) => 0u8,
                Term::BlankNode(_) => 1u8,
                Term::Literal(_) => 2u8,
                #[allow(unreachable_patterns)]
                _ => 3u8,
            };
            tag(a).cmp(&tag(b))
        }
    }
}

fn preferred_label<'a>(iri: &'a str, triples: &'a [Triple]) -> &'a str {
    let mut best: Option<(u8, &'a str)> = None;
    for t in triples {
        let NamedOrBlankNode::NamedNode(s) = &t.subject else {
            continue;
        };
        if s.as_str() != iri {
            continue;
        }
        let pred = t.predicate.as_str();
        let pred_rank = match pred {
            SKOS_PREF_LABEL => 0u8,
            RDFS_LABEL => 1u8,
            DCT_TITLE => 2u8,
            _ => continue,
        };
        let Term::Literal(lit) = &t.object else {
            continue;
        };
        let lang_rank = match lit.language() {
            Some(l) if l.eq_ignore_ascii_case("en") => 0u8,
            None => 1u8,
            Some(_) => 2u8,
        };
        let rank = pred_rank * 4 + lang_rank;
        if best.is_none_or(|(b, _)| rank < b) {
            best = Some((rank, lit.value()));
        }
    }
    if let Some((_, label)) = best {
        return label;
    }
    // Fall back to local name if present, else the full IRI.
    iri.rsplit_once('#')
        .or_else(|| iri.trim_end_matches('/').rsplit_once('/'))
        .map(|(_, local)| local)
        .filter(|s| !s.is_empty())
        .unwrap_or(iri)
}

/// Render a self-contained, escaped HTML5 page for `desc`, enforcing both
/// `max_result_bytes` and `desc.memory_charge` during construction.
pub fn render(
    desc: &mut Description,
    version_param: Option<&str>,
    at_param: Option<&str>,
    extra_describe: &[(String, String)],
    max_result_bytes: Option<u64>,
) -> sparkles::Result<String> {
    let triples = desc.triples.as_slice();
    let title = preferred_label(&desc.iri, triples);
    let mut outgoing: Vec<&Triple> = Vec::new();
    let mut incoming: Vec<&Triple> = Vec::new();
    let mut related: Vec<&Triple> = Vec::new();

    for t in triples {
        let is_subj =
            matches!(&t.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == desc.iri);
        let is_obj = matches!(&t.object, Term::NamedNode(n) if n.as_str() == desc.iri);
        if is_subj {
            outgoing.push(t);
        } else if is_obj {
            incoming.push(t);
        } else {
            related.push(t);
        }
    }
    // Sort deterministically without allocating temporary strings: rdf:type first, then
    // predicate, then object.
    outgoing.sort_by(|a, b| {
        let ra = a.predicate.as_str() != RDF_TYPE;
        let rb = b.predicate.as_str() != RDF_TYPE;
        ra.cmp(&rb)
            .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
            .then_with(|| cmp_term(&a.object, &b.object))
    });
    incoming.sort_by(|a, b| {
        a.predicate
            .as_str()
            .cmp(b.predicate.as_str())
            .then_with(|| cmp_named_or_blank(&a.subject, &b.subject))
    });
    related.sort_by(|a, b| {
        cmp_named_or_blank(&a.subject, &b.subject)
            .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
            .then_with(|| cmp_term(&a.object, &b.object))
    });

    let mut w = HtmlWriter::new(max_result_bytes, &mut desc.memory_charge);
    w.push_str(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>",
    )?;
    w.push_escaped(title)?;
    w.push_str("</title>\n")?;

    for fmt in [
        Format::Turtle,
        Format::JsonLd,
        Format::RdfXml,
        Format::NTriples,
    ] {
        let href = doc_url(&desc.iri, fmt, version_param, at_param, extra_describe);
        w.push_str("<link rel=\"alternate\" type=\"")?;
        w.push_str(fmt.essence())?;
        w.push_str("\" href=\"")?;
        w.push_escaped(&href)?;
        w.push_str("\" title=\"")?;
        w.push_str(fmt.label())?;
        w.push_str("\">\n")?;
    }
    w.push_str(
        "<style>\n\
         body{font-family:system-ui,-apple-system,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;line-height:1.5;color:#1f2937;background:#fff}\n\
         h1{margin:0 0 .25rem;font-size:1.6rem}\n\
         .iri{font-family:ui-monospace,SFMono-Regular,monospace;font-size:.9rem;color:#4b5563;word-break:break-all;margin-bottom:1rem}\n\
         .meta{display:flex;flex-wrap:wrap;gap:.5rem 1.25rem;font-size:.875rem;color:#374151;background:#f9fafb;border:1px solid #e5e7eb;border-radius:6px;padding:.75rem 1rem;margin-bottom:1.25rem}\n\
         .formats{display:flex;flex-wrap:wrap;gap:.5rem;margin-bottom:1.5rem;font-size:.875rem}\n\
         .formats a{padding:.25rem .6rem;border:1px solid #d1d5db;border-radius:4px;text-decoration:none;color:#1d4ed8;background:#f8fafc}\n\
         .notice{background:#fef3c7;border:1px solid #f59e0b;border-radius:6px;padding:.6rem .9rem;margin-bottom:1.25rem;font-size:.875rem}\n\
         h2{font-size:1.1rem;margin:1.5rem 0 .5rem;border-bottom:1px solid #e5e7eb;padding-bottom:.25rem}\n\
         table{width:100%;border-collapse:collapse;font-size:.92rem}\n\
         th,td{text-align:left;vertical-align:top;padding:.45rem .6rem;border-bottom:1px solid #f3f4f6}\n\
         th{width:28%;font-weight:600;color:#374151}\n\
         code{font-family:ui-monospace,SFMono-Regular,monospace;font-size:.88em;word-break:break-all}\n\
         .tag{font-family:ui-monospace,SFMono-Regular,monospace;font-size:.78em;color:#6b7280}\n\
         a{color:#1d4ed8}\n\
         </style>\n\
         </head>\n\
         <body>\n<h1>",
    )?;
    w.push_escaped(title)?;
    w.push_str("</h1>\n<div class=\"iri\"><code>")?;
    w.push_escaped(&desc.iri)?;
    w.push_str("</code></div>\n")?;

    // Metadata bar
    w.push_str("<div class=\"meta\"><span><strong>Resource:</strong> ")?;
    w.push_escaped(&desc.resource_title)?;
    w.push_str("</span>")?;
    if let Some(p) = &desc.namespace_prefix {
        w.push_str("<span><strong>Prefix:</strong> <code>")?;
        w.push_escaped(p)?;
        w.push_str("</code></span>")?;
    }
    if let Some(v) = &desc.version_name {
        w.push_str("<span><strong>Version:</strong> <code>")?;
        w.push_escaped(v)?;
        w.push_str("</code></span>")?;
    }
    if let Some(h) = &desc.resolved_history {
        w.push_str("<span><strong>Snapshot:</strong> <code>")?;
        w.push_escaped(&h.at.to_string())?;
        w.push_str("</code> (commit ")?;
        w.push_u64(h.commit.seq)?;
        w.push_str(")</span>")?;
    }
    if let Some(lic) = &desc.license {
        w.push_str("<span><strong>Licence:</strong> <code>")?;
        w.push_escaped(lic)?;
        w.push_str("</code></span>")?;
    }
    w.push_str("</div>\n")?;

    // Alternate RDF format links
    w.push_str("<nav class=\"formats\" aria-label=\"Alternate formats\">")?;
    for fmt in [
        Format::Turtle,
        Format::JsonLd,
        Format::RdfXml,
        Format::NTriples,
    ] {
        let href = doc_url(&desc.iri, fmt, version_param, at_param, extra_describe);
        w.push_str("<a href=\"")?;
        w.push_escaped(&href)?;
        w.push_str("\">")?;
        w.push_str(fmt.label())?;
        w.push_str("</a>")?;
    }
    w.push_str("</nav>\n")?;

    if desc.truncated {
        w.push_str(
            "<div class=\"notice\">This description was truncated at the server's DESCRIBE triple limit.</div>\n",
        )?;
    }

    let section = |triple: &&Triple| match field(triple.predicate.as_str()) {
        Some(Field::Definition) => "Definitions",
        Some(Field::Synonym) => "Synonyms",
        Some(Field::Parent) => "Hierarchy",
        Some(Field::Equivalent) => "Equivalent terms",
        Some(Field::Usage) => "Usage",
        Some(Field::Example) => "Examples",
        _ => "Properties",
    };
    for heading in [
        "Definitions",
        "Synonyms",
        "Hierarchy",
        "Equivalent terms",
        "Usage",
        "Examples",
        "Properties",
    ] {
        let mut rows = outgoing
            .iter()
            .filter(|triple| section(triple) == heading)
            .peekable();
        if rows.peek().is_none() {
            continue;
        }
        w.push_str("<h2>")?;
        w.push_str(heading)?;
        w.push_str("</h2>\n<table><tbody>\n")?;
        for t in rows {
            w.push_str("<tr><th>")?;
            write_iri(
                &mut w,
                t.predicate.as_str(),
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</th><td>")?;
            write_term(
                &mut w,
                &t.object,
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</td></tr>\n")?;
        }
        w.push_str("</tbody></table>\n")?;
    }

    if !incoming.is_empty() {
        w.push_str("<h2>Referenced by</h2>\n<table><tbody>\n")?;
        for t in incoming {
            w.push_str("<tr><th>")?;
            write_iri(
                &mut w,
                t.predicate.as_str(),
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</th><td>")?;
            write_subject(
                &mut w,
                &t.subject,
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</td></tr>\n")?;
        }
        w.push_str("</tbody></table>\n")?;
    }

    if !related.is_empty() {
        w.push_str("<h2>Related statements</h2>\n<table><thead><tr><th>Subject</th><th>Predicate</th><th>Object</th></tr></thead><tbody>\n")?;
        for t in related {
            w.push_str("<tr><td>")?;
            write_subject(
                &mut w,
                &t.subject,
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</td><td>")?;
            write_iri(
                &mut w,
                t.predicate.as_str(),
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</td><td>")?;
            write_term(
                &mut w,
                &t.object,
                &desc.prefixes,
                &desc.readable_targets,
                version_param,
                at_param,
            )?;
            w.push_str("</td></tr>\n")?;
        }
        w.push_str("</tbody></table>\n")?;
    }

    w.push_str("</body>\n</html>\n")?;
    Ok(w.into_string())
}
