//! Linked-data publishing front end (spec C32, phase 1).
//!
//! Serves resolvable IRIs under hosted namespaces registered in the C30 resource catalog
//! (`resources.json`):
//!
//! * Content negotiation (`Vary: Accept`) between escaped HTML for browsers (`text/html`)
//!   and bounded `DESCRIBE` RDF (`text/turtle`, `application/ld+json`,
//!   `application/rdf+xml`, `application/n-triples`) for semantic-web agents.
//! * HTTP `303 See Other` redirects from slash IRIs to their document URL (`/$/ld/doc`),
//!   while hash IRIs and hash namespace documents answer `200 OK` directly.
//! * Version IRI resolution to pinned snapshots or commits configured on catalog
//!   versions (`versionIri` and `snapshot`).
//! * Strict authorization enforcement: every lookup, redirect target, HTML page and RDF
//!   description shows only data the caller is permitted to read under dataset, graph and
//!   triple ACLs and server query budgets.
//! * PURL and w3id recipe export for hosted vocabulary namespaces ([`recipes`]).

pub mod html;
pub mod http;
pub mod recipes;
#[cfg(test)]
mod tests;

use crate::auth::{Endpoint, Level, Principal};
use crate::http::caller::CallerContext;
use crate::http::{ApiError, ApiResult, Params, err_code};
use crate::resources::{CatalogFile, Kind, Namespace, Resource, Version};
use crate::state::{AppState, Dataset};
use axum::http::StatusCode;
use oxrdf::{BlankNode, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{JsonLdProfileSet, RdfFormat, RdfSerializer};
use sparkles::history::{At, Resolved};
use sparkles::sparql::QueryResult;
use sparkles::sparql::describe::DescribeOptions;
use sparkles::sparql::pool::PoolGrant;
use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default upper bound on triples returned by a linked-data `DESCRIBE`.
pub const DEFAULT_MAX_TRIPLES: u64 = 10_000;
/// Default upper bound on blank-node / reifier traversal depth.
pub const DEFAULT_MAX_DEPTH: u32 = 16;
/// Upper bound on distinct terms enumerated for a bare hash-namespace document.
const HASH_NS_TERM_LIMIT: usize = 200;

/// Supported output representations for linked-data responses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Html,
    Turtle,
    JsonLd,
    RdfXml,
    NTriples,
}

impl Format {
    pub const ALL: [Format; 5] = [
        Format::Html,
        Format::Turtle,
        Format::JsonLd,
        Format::RdfXml,
        Format::NTriples,
    ];

    /// Parse a `format=` query parameter or emitted media type name.
    pub fn from_param(s: &str) -> Option<Format> {
        let base = s.split(';').next()?.trim().to_ascii_lowercase();
        match base.as_str() {
            "html" | "text/html" => Some(Format::Html),
            "turtle" | "ttl" | "text/turtle" => Some(Format::Turtle),
            "jsonld" | "json-ld" | "application/ld+json" => Some(Format::JsonLd),
            "rdfxml" | "rdf" | "xml" | "application/rdf+xml" => Some(Format::RdfXml),
            "ntriples" | "nt" | "application/n-triples" => Some(Format::NTriples),
            _ => None,
        }
    }

    pub fn param_name(self) -> &'static str {
        match self {
            Format::Html => "html",
            Format::Turtle => "turtle",
            Format::JsonLd => "jsonld",
            Format::RdfXml => "rdfxml",
            Format::NTriples => "ntriples",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Html => "HTML",
            Format::Turtle => "Turtle",
            Format::JsonLd => "JSON-LD",
            Format::RdfXml => "RDF/XML",
            Format::NTriples => "N-Triples",
        }
    }

    /// `Content-Type` header value.
    pub fn media_type(self) -> &'static str {
        match self {
            Format::Html => "text/html; charset=utf-8",
            Format::Turtle => "text/turtle; charset=utf-8",
            Format::JsonLd => "application/ld+json",
            Format::RdfXml => "application/rdf+xml",
            Format::NTriples => "application/n-triples",
        }
    }

    /// Media type without parameters, for `Link` headers and `Accept` matching.
    pub fn essence(self) -> &'static str {
        match self {
            Format::Html => "text/html",
            Format::Turtle => "text/turtle",
            Format::JsonLd => "application/ld+json",
            Format::RdfXml => "application/rdf+xml",
            Format::NTriples => "application/n-triples",
        }
    }

    pub fn rdf_format(self) -> Option<RdfFormat> {
        match self {
            Format::Html => None,
            Format::Turtle => Some(RdfFormat::Turtle),
            Format::JsonLd => Some(RdfFormat::JsonLd {
                profile: JsonLdProfileSet::empty(),
            }),
            Format::RdfXml => Some(RdfFormat::RdfXml),
            Format::NTriples => Some(RdfFormat::NTriples),
        }
    }
}

/// Split `s` on `sep`, ignoring separators inside double-quoted strings.
fn split_outside_quotes(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_quotes = false;
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == sep {
            parts.push(&s[start..i]);
            start = i + sep.len_utf8();
        }
    }
    parts.push(&s[start..]);
    parts
}

fn unquote_param(raw: &str) -> Option<String> {
    let s = raw.trim();
    if let Some(inner) = s.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                out.push(chars.next()?);
            } else {
                out.push(c);
            }
        }
        Some(out)
    } else if !s.is_empty() {
        Some(s.to_string())
    } else {
        None
    }
}

/// Negotiate an output [`Format`] from a combined HTTP `Accept` header following
/// RFC 9110 §12.5.1: exact media type (with matching media parameters) > `type/*` > `*/*`,
/// honoring explicit `q=0` exclusions and advertising only the media types emitted.
/// An absent or empty `Accept` header defaults to [`Format::Turtle`].
pub fn negotiate_format(accept: Option<&str>) -> Option<Format> {
    let Some(raw) = accept.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(Format::Turtle);
    };

    type Offer = (
        &'static str,
        Format,
        &'static [(&'static str, &'static str)],
    );
    const OFFERS: [Offer; 5] = [
        ("text/turtle", Format::Turtle, &[("charset", "utf-8")]),
        ("application/ld+json", Format::JsonLd, &[]),
        ("application/rdf+xml", Format::RdfXml, &[]),
        ("application/n-triples", Format::NTriples, &[]),
        ("text/html", Format::Html, &[("charset", "utf-8")]),
    ];

    struct MediaRange {
        mt: String,
        params: Vec<(String, String)>,
        q: f32,
    }

    let mut ranges: Vec<MediaRange> = Vec::new();
    for part in split_outside_quotes(raw, ',') {
        let pieces = split_outside_quotes(part, ';');
        let mt = pieces
            .first()
            .copied()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let Some((r_type, r_sub)) = mt.split_once('/') else {
            continue;
        };
        if r_type.is_empty() || r_sub.is_empty() || r_sub.contains('/') {
            continue;
        }
        let mut params = Vec::new();
        let mut q = 1.0f32;
        let mut seen_q = false;
        let mut bad = false;
        for param in pieces.iter().skip(1) {
            let param = param.trim();
            if param.is_empty() {
                continue;
            }
            let Some((k, v)) = param.split_once('=') else {
                bad = true;
                break;
            };
            let key = k.trim().to_ascii_lowercase();
            if key == "q" {
                if seen_q {
                    bad = true;
                    break;
                }
                seen_q = true;
                match v.trim().parse::<f32>() {
                    Ok(n) if n.is_finite() && (0.0..=1.0).contains(&n) => q = n,
                    _ => {
                        bad = true;
                        break;
                    }
                }
            } else {
                // RFC 9110 §12.5.1: process weight regardless of position among parameters.
                let Some(val) = unquote_param(v) else {
                    bad = true;
                    break;
                };
                params.push((key, val));
            }
        }
        if !bad {
            ranges.push(MediaRange { mt, params, q });
        }
    }
    if ranges.is_empty() {
        return None;
    }

    let mut best: Option<(f32, (u8, usize), usize, Format)> = None;
    for (idx, (offer_mt, fmt, offer_params)) in OFFERS.iter().enumerate() {
        let offer_type = offer_mt.split('/').next().unwrap_or("");
        // RFC 9110 §12.5.1: the most specific matching media range determines q for this offer.
        let mut matched: Option<((u8, usize), f32)> = None;
        for range in &ranges {
            let type_spec = if range.mt == *offer_mt {
                3u8
            } else if let Some(prefix) = range.mt.strip_suffix("/*") {
                if prefix == "*" {
                    1u8
                } else if prefix == offer_type {
                    2u8
                } else {
                    continue;
                }
            } else {
                continue;
            };
            let params_match = range.params.iter().all(|(k, v)| {
                offer_params
                    .iter()
                    .any(|(ok, ov)| ok == k && ov.eq_ignore_ascii_case(v))
            });
            if !params_match {
                continue;
            }
            let spec = (type_spec, range.params.len());
            match matched {
                None => matched = Some((spec, range.q)),
                Some((cur_spec, cur_q))
                    if spec > cur_spec || (spec == cur_spec && range.q > cur_q) =>
                {
                    matched = Some((spec, range.q));
                }
                _ => {}
            }
        }
        let Some((spec, q)) = matched else {
            continue;
        };
        if q <= 0.0 {
            continue;
        }
        let take = match best {
            None => true,
            Some((bq, bspec, bidx, _)) => {
                q > bq || (q == bq && (spec > bspec || (spec == bspec && idx < bidx)))
            }
        };
        if take {
            best = Some((q, spec, idx, *fmt));
        }
    }
    best.map(|(_, _, _, fmt)| fmt)
}

/// Whether `s` contains only characters valid in an IRI / local path segment.
fn has_forbidden_iri_char(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | '{' | '}' | '"' | '`' | '\\' | ' '))
}

/// Validate a local path suffix on `/$/ld/ns/{prefix}/{*local}` before catalog lookup
/// so malformed paths return `400 invalid-iri` independent of prefix existence.
pub fn valid_local_part(local: &str) -> bool {
    !local.is_empty() && local.len() <= 4096 && !has_forbidden_iri_char(local)
}

/// Validate an IRI before embedding it in a SPARQL query or resolving it.
pub fn valid_lookup_iri(iri: &str) -> bool {
    !iri.is_empty()
        && iri.len() <= 4096
        && !has_forbidden_iri_char(iri)
        && NamedNode::new(iri).is_ok()
}

/// What a linked-data HTTP request resolves: either a direct IRI or a hosted prefix path.
#[derive(Clone, Debug)]
pub enum LookupTarget {
    Iri(String),
    Namespace {
        prefix: String,
        local: Option<String>,
    },
}

/// Result of matching a requested IRI against the resource catalog.
#[derive(Clone, Debug)]
pub struct ResolvedTarget<'a> {
    pub resource: &'a Resource,
    pub version: Option<&'a Version>,
    pub namespace: Option<&'a Namespace>,
    /// True for hash IRIs and bare hash-namespace documents (200 directly, no 303).
    pub is_hash: bool,
    /// True when the request targets a bare `#` namespace document (with or without `#`).
    pub is_hash_document: bool,
    /// True when the IRI matched a version's `versionIri` exactly.
    pub is_exact_version_iri: bool,
    /// True when the IRI itself selected a catalog version (exact match or slash version
    /// prefix), so stripping `at`/`version` query parameters would still resolve to a
    /// pinned version rather than a live resource.
    pub iri_pinned_version: bool,
    /// True when `?version=` was given and conflicts with the matched `versionIri`.
    pub version_conflict: bool,
    /// True when `?version=` was given and does not exist on the matched resource.
    pub version_missing: bool,
}

impl<'a> ResolvedTarget<'a> {
    pub fn pinned_snapshot(&self) -> Option<&'a str> {
        self.version.and_then(|v| v.input.snapshot.as_deref())
    }
}

/// Whether `iri` falls under prefix `base`: exact match, or prefix match when `base`
/// ends with a delimiter (`/`, `#`, `:`).
fn prefix_matches(base: &str, iri: &str) -> bool {
    if iri == base {
        return true;
    }
    (base.ends_with('/') || base.ends_with('#') || base.ends_with(':')) && iri.starts_with(base)
}

/// Match `iri` against hosted vocabulary namespaces and configured version IRIs.
pub fn resolve_iri<'a>(
    catalog: &'a CatalogFile,
    iri: &str,
    requested_version: Option<&str>,
) -> Option<ResolvedTarget<'a>> {
    // 1. Check version IRIs across all resources (exact match > longest prefix match).
    let mut best_ver: Option<(bool, usize, bool, &'a Resource, &'a Version)> = None;
    for r in &catalog.resources {
        for v in &r.versions {
            let Some(vi) = v.input.version_iri.as_deref() else {
                continue;
            };
            let (exact, is_hash_doc) = if iri == vi {
                (true, vi.ends_with('#'))
            } else if vi.ends_with('#') && iri == vi.trim_end_matches('#') {
                (true, true)
            } else if prefix_matches(vi, iri) {
                (false, false)
            } else {
                continue;
            };
            let len = vi.len();
            let better = match best_ver {
                None => true,
                Some((b_exact, b_len, _, _, _)) => {
                    (exact && !b_exact) || (exact == b_exact && len > b_len)
                }
            };
            if better {
                best_ver = Some((exact, len, is_hash_doc, r, v));
            }
        }
    }

    // 2. Check hosted namespaces of vocabulary resources (longest match wins).
    let mut best_ns: Option<(usize, bool, &'a Resource, &'a Namespace)> = None;
    for r in &catalog.resources {
        if r.input.kind != Kind::Vocabulary {
            continue;
        }
        for ns in &r.input.namespaces {
            let is_hash_doc = if ns.iri.ends_with('#') {
                iri == ns.iri || iri == ns.iri.trim_end_matches('#')
            } else {
                false
            };
            if !is_hash_doc && !prefix_matches(&ns.iri, iri) {
                continue;
            }
            let len = ns.iri.len();
            if best_ns.is_none_or(|(b_len, _, _, _)| len > b_len) {
                best_ns = Some((len, is_hash_doc, r, ns));
            }
        }
    }

    // Pick the more specific match: exact version IRI > longer prefix (version IRI breaks ties).
    let (
        resource,
        mut version,
        namespace,
        is_hash_document,
        is_exact_version_iri,
        iri_pinned_version,
    ) = match (best_ver, best_ns) {
        (Some((true, _, hdoc, r, v)), _) => {
            let ns = r
                .input
                .namespaces
                .iter()
                .find(|n| prefix_matches(&n.iri, iri))
                .or_else(|| r.input.namespaces.first());
            (r, Some(v), ns, hdoc, true, true)
        }
        (Some((false, vlen, hdoc, r, v)), Some((nlen, _, _, _))) if vlen >= nlen => {
            let ns = r
                .input
                .namespaces
                .iter()
                .find(|n| prefix_matches(&n.iri, iri))
                .or_else(|| r.input.namespaces.first());
            (r, Some(v), ns, hdoc, false, true)
        }
        (Some((false, _, hdoc, r, v)), None) => {
            let ns = r
                .input
                .namespaces
                .iter()
                .find(|n| prefix_matches(&n.iri, iri))
                .or_else(|| r.input.namespaces.first());
            (r, Some(v), ns, hdoc, false, true)
        }
        (_, Some((_, hdoc, r, ns))) => (r, None, Some(ns), hdoc, false, false),
        (None, None) => return None,
    };

    let mut version_conflict = false;
    let mut version_missing = false;
    if let Some(req_v) = requested_version {
        match version {
            Some(matched) if matched.input.version != req_v => {
                version_conflict = true;
            }
            Some(_) => {}
            None => match resource.version(req_v) {
                Some(v) => version = Some(v),
                None => version_missing = true,
            },
        }
    }

    let is_hash = is_hash_document
        || iri.contains('#')
        || namespace.is_some_and(|n| n.iri.ends_with('#') && iri == n.iri.trim_end_matches('#'));

    Some(ResolvedTarget {
        resource,
        version,
        namespace,
        is_hash,
        is_hash_document,
        is_exact_version_iri,
        iri_pinned_version,
        version_conflict,
        version_missing,
    })
}

/// Expand a hosted namespace `prefix` and optional `local` path into its full IRI.
pub fn resolve_prefix(catalog: &CatalogFile, prefix: &str, local: Option<&str>) -> Option<String> {
    let ns = catalog
        .resources
        .iter()
        .filter(|r| r.input.kind == Kind::Vocabulary)
        .flat_map(|r| r.input.namespaces.iter())
        .find(|n| n.prefix == prefix)?;
    match local.filter(|s| !s.is_empty()) {
        None => {
            if ns.iri.ends_with('#') {
                Some(ns.iri.trim_end_matches('#').to_string())
            } else {
                Some(ns.iri.clone())
            }
        }
        Some(loc) => Some(format!("{}{loc}", ns.iri)),
    }
}

/// Permitted `(dataset, allowed_graphs)` backing locations for `target` under `principal`.
fn permitted_locations(
    st: &AppState,
    principal: &Principal,
    target: &ResolvedTarget<'_>,
) -> Vec<(Arc<Dataset>, Vec<String>)> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    match target.resource.input.kind {
        Kind::Vocabulary => {
            for loc in &target.resource.input.graphs {
                grouped
                    .entry(loc.dataset.clone())
                    .or_default()
                    .push(loc.graph.clone());
            }
        }
        Kind::Dataset => {
            if let Some(ds_name) = &target.resource.input.dataset {
                let graphs = target
                    .version
                    .map(|v| {
                        v.input
                            .graphs
                            .iter()
                            .map(|g| g.graph.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                grouped.insert(ds_name.clone(), graphs);
            }
        }
        Kind::Source => return Vec::new(),
    }

    let mut out = Vec::new();
    for (ds_name, candidate_graphs) in grouped {
        if !principal.can_at(&ds_name, Endpoint::Query, Level::Read) {
            continue;
        }
        let Ok(ds) = crate::http::dataset(st, &ds_name) else {
            continue;
        };
        let view = principal.view(&ds.name, Endpoint::Query);
        if candidate_graphs.is_empty() {
            // Dataset resource without per-version graph list: QueryOptions enforces
            // graph and triple ACLs.
            out.push((ds, Vec::new()));
            continue;
        }
        let allowed: Vec<String> = candidate_graphs
            .into_iter()
            .filter(|g| view.as_ref().is_none_or(|v| v.read.allows_named_iri(g)))
            .collect();
        if !allowed.is_empty() {
            out.push((ds, allowed));
        }
    }
    out
}

/// Escape a string for a SPARQL double-quoted literal.
fn escape_sparql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn hash_doc_term_pattern(iri: &str, target: &ResolvedTarget<'_>) -> (String, String, String) {
    let base = if target.is_exact_version_iri {
        target
            .version
            .and_then(|v| v.input.version_iri.as_deref())
            .filter(|vi| valid_lookup_iri(vi))
            .unwrap_or(iri)
    } else {
        iri
    };
    let with_hash = if base.ends_with('#') {
        base.to_string()
    } else {
        format!("{base}#")
    };
    let without_hash = with_hash.trim_end_matches('#').to_string();
    let prefix = escape_sparql_string(&with_hash);
    let ns_clause = if target.is_exact_version_iri {
        target
            .namespace
            .filter(|n| n.iri.ends_with('#') && n.iri != with_hash)
            .map(|n| {
                let p = escape_sparql_string(&n.iri);
                // Bind fallback vocabulary-namespace term discovery to a readable
                // owl:versionIRI assertion so unrelated readable triples in the same
                // backing graph can never authorize a private hash version lookup.
                format!(
                    " UNION {{ \
                      {{ ?ver_ont <http://www.w3.org/2002/07/owl#versionIRI> <{with_hash}> }} \
                      UNION {{ ?ver_ont <http://www.w3.org/2002/07/owl#versionIRI> <{without_hash}> }} \
                      ?term ?p2 ?o2 . FILTER(ISIRI(?term) && STRSTARTS(STR(?term), \"{p}\")) \
                    }}"
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let pattern = format!(
        "{{ ?term ?p ?o . FILTER(ISIRI(?term) && STRSTARTS(STR(?term), \"{prefix}\")) }}{ns_clause}"
    );
    (with_hash, without_hash, pattern)
}

fn describe_sparql(iri: &str, target: &ResolvedTarget<'_>) -> String {
    if target.is_exact_version_iri && target.is_hash_document {
        let (with_hash, without_hash, pattern) = hash_doc_term_pattern(iri, target);
        format!(
            "DESCRIBE <{with_hash}> <{without_hash}> ?ont ?term WHERE {{ \
             OPTIONAL {{ \
               {{ ?ont <http://www.w3.org/2002/07/owl#versionIRI> <{with_hash}> }} \
               UNION {{ ?ont <http://www.w3.org/2002/07/owl#versionIRI> <{without_hash}> }} \
             }} \
             OPTIONAL {{ \
               {{ SELECT DISTINCT ?term WHERE {{ {pattern} }} ORDER BY ?term LIMIT {HASH_NS_TERM_LIMIT} }} \
             }} \
           }}"
        )
    } else if target.is_exact_version_iri {
        let configured_vi = target
            .version
            .and_then(|v| v.input.version_iri.as_deref())
            .filter(|vi| valid_lookup_iri(vi))
            .unwrap_or(iri);
        if configured_vi == iri {
            format!(
                "DESCRIBE <{iri}> ?ont WHERE {{ OPTIONAL {{ ?ont <http://www.w3.org/2002/07/owl#versionIRI> <{iri}> }} }}"
            )
        } else {
            format!(
                "DESCRIBE <{iri}> <{configured_vi}> ?ont WHERE {{ OPTIONAL {{ \
                 {{ ?ont <http://www.w3.org/2002/07/owl#versionIRI> <{configured_vi}> }} \
                 UNION {{ ?ont <http://www.w3.org/2002/07/owl#versionIRI> <{iri}> }} \
               }} }}"
            )
        }
    } else if target.is_hash_document {
        let (with_hash, without_hash, pattern) = hash_doc_term_pattern(iri, target);
        format!(
            "DESCRIBE <{with_hash}> <{without_hash}> ?term WHERE {{ \
             OPTIONAL {{ \
               {{ SELECT DISTINCT ?term WHERE {{ {pattern} }} ORDER BY ?term LIMIT {HASH_NS_TERM_LIMIT} }} \
             }} \
           }}"
        )
    } else {
        format!("DESCRIBE <{iri}>")
    }
}

fn hash_doc_overflow_sparql(iri: &str, target: &ResolvedTarget<'_>) -> String {
    let (_, _, pattern) = hash_doc_term_pattern(iri, target);
    format!(
        "SELECT DISTINCT ?term WHERE {{ {pattern} }} ORDER BY ?term OFFSET {HASH_NS_TERM_LIMIT} LIMIT 1"
    )
}

fn scope_bnode(t: Triple, prefix: &str) -> Triple {
    let map_nb = |nb: NamedOrBlankNode| match nb {
        NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(BlankNode::new_unchecked(
            format!("{prefix}_{}", b.as_str()),
        )),
        other => other,
    };
    let map_term = |term: Term| match term {
        Term::BlankNode(b) => {
            Term::BlankNode(BlankNode::new_unchecked(format!("{prefix}_{}", b.as_str())))
        }
        other => other,
    };
    Triple::new(map_nb(t.subject), t.predicate, map_term(t.object))
}

fn term_bytes(term: &Term) -> u64 {
    match term {
        Term::NamedNode(n) => 24 + n.as_str().len() as u64,
        Term::BlankNode(b) => 24 + b.as_str().len() as u64,
        Term::Literal(l) => {
            64 + l.value().len() as u64
                + l.language().map_or(0, |s| s.len() as u64)
                + l.datatype().as_str().len() as u64
        }
        #[allow(unreachable_patterns)]
        other => 64 + other.to_string().len() as u64,
    }
}

fn triple_bytes(t: &Triple) -> u64 {
    let s = match &t.subject {
        NamedOrBlankNode::NamedNode(n) => 24 + n.as_str().len() as u64,
        NamedOrBlankNode::BlankNode(b) => 24 + b.as_str().len() as u64,
    };
    let p = 24 + t.predicate.as_str().len() as u64;
    let o = term_bytes(&t.object);
    64 + s + p + o
}

/// Estimated byte size of `t` after scoping blank nodes with `prefix` (without allocating).
fn scoped_triple_bytes(t: &Triple, prefix: &str) -> u64 {
    let extra = (prefix.len() + 1) as u64;
    let mut bytes = triple_bytes(t);
    if matches!(&t.subject, NamedOrBlankNode::BlankNode(_)) {
        bytes = bytes.saturating_add(extra);
    }
    if matches!(&t.object, Term::BlankNode(_)) {
        bytes = bytes.saturating_add(extra);
    }
    bytes
}

/// Request-wide memory charge against both `--query-memory-mb` and the admitted
/// [`PoolGrant`], keeping live historical snapshots, retained triples/results, and
/// buffered response bodies accounted until the HTTP response body drops.
pub(crate) struct MemoryCharge {
    grant: Option<Arc<PoolGrant>>,
    limit: Option<u64>,
    /// Bytes directly charged on `grant` and against `limit` by this account.
    held: u64,
    /// Bytes retained by a live `QueryResult` (which already holds its own credit on
    /// `grant`, so counted only against `limit` here).
    external_held: u64,
}

impl MemoryCharge {
    pub(crate) fn new(grant: Option<Arc<PoolGrant>>, limit: Option<u64>) -> Self {
        Self {
            grant,
            limit,
            held: 0,
            external_held: 0,
        }
    }

    #[inline]
    fn total_held(&self) -> u64 {
        self.held.saturating_add(self.external_held)
    }

    pub(crate) fn charge(&mut self, n: u64) -> sparkles::Result<()> {
        let total = self.total_held().saturating_add(n);
        if let Some(limit) = self.limit
            && total > limit
        {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Memory,
                limit,
                requested: total,
            }));
        }
        if let Some(g) = &self.grant {
            g.charge(n)?;
        }
        self.held = self.held.saturating_add(n);
        Ok(())
    }

    pub(crate) fn release(&mut self, n: u64) {
        let n = n.min(self.held);
        if let Some(g) = &self.grant {
            g.release(n);
        }
        self.held -= n;
    }

    pub(crate) fn set_external(&mut self, bytes: u64) {
        self.external_held = bytes;
    }

    pub(crate) fn clear_external(&mut self) {
        self.external_held = 0;
    }

    /// Remaining per-query memory budget for the next engine invocation.
    pub(crate) fn engine_budget(&self) -> sparkles::Result<Option<u64>> {
        let Some(limit) = self.limit else {
            return Ok(None);
        };
        let used = self.total_held();
        if used >= limit {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Memory,
                limit,
                requested: used,
            }));
        }
        Ok(Some(limit - used))
    }

    /// Atomically reserve a pool-aware historical replay allowance up to `history_cap`,
    /// retrying once with the grant's already-covered room if another grant claims free
    /// pool granules first. Returns `Ok(None)` only when neither a per-query limit nor a
    /// query memory pool is configured.
    pub(crate) fn reserve_replay(&mut self, history_cap: u64) -> sparkles::Result<Option<u64>> {
        let local_rem = match self.engine_budget()? {
            Some(rem) => Some(rem),
            None if self.grant.is_some() => Some(u64::MAX),
            None => return Ok(None),
        };
        let local = local_rem.unwrap_or(u64::MAX).min(history_cap);
        let desired = self
            .grant
            .as_ref()
            .map_or(local, |g| local.min(g.growth_room()));
        match self.charge(desired) {
            Ok(()) => Ok(Some(desired)),
            Err(sparkles::Error::BudgetExceeded(_)) => {
                if let Some(g) = &self.grant {
                    let covered = local.min(g.granted().saturating_sub(g.used()));
                    self.charge(covered)?;
                    Ok(Some(covered))
                } else {
                    Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                        kind: sparkles::BudgetKind::Memory,
                        limit: self.limit.unwrap_or(0),
                        requested: self.total_held().saturating_add(desired),
                    }))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Settle a replay reservation down (or up) to the snapshot's actual retained charge
    /// while the snapshot remains live.
    pub(crate) fn settle_replay(
        &mut self,
        reserved: Option<u64>,
        retained: u64,
    ) -> sparkles::Result<u64> {
        let Some(res) = reserved else {
            return Ok(0);
        };
        if retained <= res {
            self.release(res - retained);
            Ok(retained)
        } else {
            self.charge(retained - res)?;
            Ok(retained)
        }
    }
}

impl Drop for MemoryCharge {
    fn drop(&mut self) {
        self.release(self.held);
    }
}

struct Deadline {
    at: Option<Instant>,
}

impl Deadline {
    fn new(timeout: Duration) -> Self {
        Self {
            at: (!timeout.is_zero()).then(|| Instant::now() + timeout),
        }
    }

    #[cfg(test)]
    fn expire_now(&mut self) {
        self.at = Some(Instant::now());
    }

    fn left(&self) -> sparkles::Result<Option<Duration>> {
        let Some(at) = self.at else {
            return Ok(None);
        };
        let now = Instant::now();
        if now >= at {
            return Err(sparkles::Error::Timeout);
        }
        Ok(Some(at.saturating_duration_since(now)))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(crate) struct TestHooks {
    /// Expire the request deadline immediately before querying dataset index `idx`.
    pub(crate) expire_deadline_before_dataset: Option<usize>,
    /// Charge any remaining query memory budget onto `snap_charge` right after
    /// `settle_replay` so the defensive `charge.engine_budget()` error arm runs.
    pub(crate) exhaust_budget_after_settle: bool,
}

/// Readable hosted namespace or version IRI entry used by the HTML renderer to apply the
/// same longest-match ownership precedence as [`resolve_iri`].
#[derive(Clone, Debug)]
pub struct HostedTarget {
    pub iri: String,
    pub same_resource: bool,
    pub is_version_iri: bool,
}

/// Classify `iri` against the caller's readable hosted targets using the same precedence
/// as [`resolve_iri`]: exact version IRI > longest prefix (version IRI breaks ties).
pub(crate) fn classify_hosted_link<'a>(
    iri: &str,
    targets: &'a [HostedTarget],
) -> Option<&'a HostedTarget> {
    let mut best_ver: Option<(bool, usize, &'a HostedTarget)> = None;
    let mut best_ns: Option<(usize, &'a HostedTarget)> = None;
    for t in targets {
        if t.is_version_iri {
            let exact =
                iri == t.iri || (t.iri.ends_with('#') && iri == t.iri.trim_end_matches('#'));
            if !exact && !prefix_matches(&t.iri, iri) {
                continue;
            }
            let len = t.iri.len();
            let better = match best_ver {
                None => true,
                Some((b_exact, b_len, _)) => {
                    (exact && !b_exact) || (exact == b_exact && len > b_len)
                }
            };
            if better {
                best_ver = Some((exact, len, t));
            }
        } else {
            let is_hash_doc =
                t.iri.ends_with('#') && (iri == t.iri || iri == t.iri.trim_end_matches('#'));
            if !is_hash_doc && !prefix_matches(&t.iri, iri) {
                continue;
            }
            let len = t.iri.len();
            if best_ns.is_none_or(|(b_len, _)| len > b_len) {
                best_ns = Some((len, t));
            }
        }
    }
    match (best_ver, best_ns) {
        (Some((true, _, t)), _) => Some(t),
        (Some((false, vlen, t)), Some((nlen, _))) if vlen >= nlen => Some(t),
        (Some((false, _, t)), None) => Some(t),
        (_, Some((_, t))) => Some(t),
        (None, None) => None,
    }
}

/// Owned triple storage for a [`Description`]: single-dataset lookups retain the engine's
/// charged [`QueryResult`] directly without cloning; multi-dataset merges hold a charged
/// deduplicated vector.
pub enum TriplesSource {
    Single(QueryResult),
    Merged(Vec<Triple>),
}

impl TriplesSource {
    pub fn as_slice(&self) -> &[Triple] {
        match self {
            TriplesSource::Single(res) => res.triples(),
            TriplesSource::Merged(triples) => triples.as_slice(),
        }
    }
}

/// Authorized, bounded description of an IRI.
pub struct Description {
    pub iri: String,
    pub triples: TriplesSource,
    pub truncated: bool,
    pub prefixes: BTreeMap<String, String>,
    pub resolved_history: Option<Resolved>,
    pub resource_title: String,
    pub version_name: Option<String>,
    pub namespace_prefix: Option<String>,
    pub license: Option<String>,
    /// True when the requested IRI itself pinned a catalog version (exact or prefix).
    pub iri_pinned_version: bool,
    /// Hosted namespaces and version IRIs readable by the caller (for safe internal links
    /// and ownership-accurate `version`/`at` propagation).
    pub readable_targets: Vec<HostedTarget>,
    pub(crate) memory_charge: MemoryCharge,
    /// Bytes inside `memory_charge.held` that belong to `TriplesSource::Merged` (0 for
    /// `Single`, whose `QueryResult` holds its own pool grant credit).
    pub(crate) retained_triples_bytes: u64,
    /// Bytes inside `memory_charge.held` that belong to the live historical `Snapshot`
    /// retained by `TriplesSource::Single(QueryResult)` until `triples` is dropped.
    pub(crate) retained_snapshot_bytes: u64,
}

/// Outcome of resolving and authorizing a linked-data lookup.
pub enum LookupOutcome {
    /// Slash IRI with at least one readable triple: answer `303 See Other`.
    Redirect { iri: String },
    /// Direct `200 OK` representation (hash IRI, hash namespace document, or `/$/ld/doc`).
    Document(Box<Description>),
}

/// Generic 404 used whenever a resource, namespace, or term is absent or not readable.
pub fn not_found() -> ApiError {
    err_code(
        StatusCode::NOT_FOUND,
        "linked-data-not-found",
        "resource not found",
    )
}

fn history_gone() -> ApiError {
    err_code(
        StatusCode::GONE,
        "history-gone",
        "requested historical state is no longer retained",
    )
}

/// Hosted namespaces and version IRIs of catalog resources that `principal` may read at
/// least one backing location of.
fn readable_targets(
    st: &AppState,
    principal: &Principal,
    catalog: &CatalogFile,
    current_resource_id: &str,
) -> Vec<HostedTarget> {
    let mut out = Vec::new();
    for r in &catalog.resources {
        let same_resource = r.id == current_resource_id;
        match r.input.kind {
            Kind::Vocabulary => {
                let readable = r.input.graphs.iter().any(|l| {
                    if !principal.can_at(&l.dataset, Endpoint::Query, Level::Read) {
                        return false;
                    }
                    let Ok(ds) = crate::http::dataset(st, &l.dataset) else {
                        return false;
                    };
                    principal
                        .view(&ds.name, Endpoint::Query)
                        .is_none_or(|v| v.read.allows_named_iri(&l.graph))
                });
                if !readable {
                    continue;
                }
                for ns in &r.input.namespaces {
                    out.push(HostedTarget {
                        iri: ns.iri.clone(),
                        same_resource,
                        is_version_iri: false,
                    });
                }
                for v in &r.versions {
                    if let Some(vi) = &v.input.version_iri {
                        out.push(HostedTarget {
                            iri: vi.clone(),
                            same_resource,
                            is_version_iri: true,
                        });
                    }
                }
            }
            Kind::Dataset => {
                let Some(ds_name) = r.input.dataset.as_deref() else {
                    continue;
                };
                if !principal.can_at(ds_name, Endpoint::Query, Level::Read) {
                    continue;
                }
                let Ok(ds) = crate::http::dataset(st, ds_name) else {
                    continue;
                };
                let view = principal.view(&ds.name, Endpoint::Query);
                for v in &r.versions {
                    let Some(vi) = &v.input.version_iri else {
                        continue;
                    };
                    let version_readable = v.input.graphs.is_empty()
                        || v.input.graphs.iter().any(|g| {
                            view.as_ref()
                                .is_none_or(|vw| vw.read.allows_named_iri(&g.graph))
                        });
                    if version_readable {
                        out.push(HostedTarget {
                            iri: vi.clone(),
                            same_resource,
                            is_version_iri: true,
                        });
                    }
                }
            }
            Kind::Source => {}
        }
    }
    out
}

struct DatasetPlan {
    ds: Arc<Dataset>,
    allowed_graphs: Vec<String>,
    describe: DescribeOptions,
}

enum ProbeResult {
    Readable,
    Empty,
    HistoryGone,
    ReplayFailed(sparkles::Error),
}

#[allow(clippy::too_many_arguments)]
fn probe_one_dataset(
    st: &AppState,
    principal: &Principal,
    plan: &DatasetPlan,
    sparql: &str,
    at: Option<&At>,
    grant: &Option<Arc<PoolGrant>>,
    charge: &mut MemoryCharge,
    deadline: &Deadline,
    #[cfg(test)] hooks: TestHooks,
) -> ApiResult<ProbeResult> {
    let mut opts = CallerContext::for_caller(st, &plan.ds, principal).read_options();
    opts.grant = grant.clone();
    opts.timeout = match deadline.left() {
        Ok(t) => t,
        Err(e) => {
            if at.is_some() && !matches!(e, sparkles::Error::Cancelled) {
                return Ok(ProbeResult::ReplayFailed(e));
            }
            return Err(ApiError::from(e));
        }
    };
    let mut desc = plan.describe.clone();
    desc.max_triples = Some(1);
    opts.describe = desc;
    if !plan.allowed_graphs.is_empty() {
        opts.default_graph_uris = plan.allowed_graphs.clone();
        opts.default_graph_extra.clear();
    }

    let (snap, snap_charge) = if at.is_some() {
        let reserved = match charge.reserve_replay(plan.ds.store.history_cache_capacity()) {
            Ok(r) => r,
            Err(sparkles::Error::Cancelled) => {
                return Err(ApiError::from(sparkles::Error::Cancelled));
            }
            Err(e) => return Ok(ProbeResult::ReplayFailed(e)),
        };
        let (snap, _, retained) =
            match crate::http::history::snapshot_for_limited(&plan.ds, at, &opts, reserved) {
                Ok(triple) => triple,
                Err(e) => {
                    if let Some(r) = reserved {
                        charge.release(r);
                    }
                    return match e {
                        sparkles::Error::NotFound(_) => Ok(ProbeResult::Empty),
                        sparkles::Error::HistoryGone(_) => Ok(ProbeResult::HistoryGone),
                        sparkles::Error::Cancelled => Err(ApiError::from(e)),
                        other => Ok(ProbeResult::ReplayFailed(other)),
                    };
                }
            };
        let held = match charge.settle_replay(reserved, retained) {
            Ok(h) => h,
            Err(e) => {
                drop(snap);
                if let Some(r) = reserved {
                    charge.release(r);
                }
                return Ok(ProbeResult::ReplayFailed(e));
            }
        };
        #[cfg(test)]
        let held = if hooks.exhaust_budget_after_settle {
            if let Ok(Some(rem)) = charge.engine_budget() {
                let _ = charge.charge(rem);
                held.saturating_add(rem)
            } else {
                held
            }
        } else {
            held
        };
        (snap, held)
    } else {
        (plan.ds.store.snapshot(), 0)
    };

    opts.max_memory_bytes = match charge.engine_budget() {
        Ok(b) => b,
        Err(e) => {
            drop(snap);
            charge.release(snap_charge);
            if at.is_some() && !matches!(e, sparkles::Error::Cancelled) {
                return Ok(ProbeResult::ReplayFailed(e));
            }
            return Err(ApiError::from(e));
        }
    };
    match sparkles::sparql::query(snap, sparql, &opts) {
        Ok(res) => {
            let empty = res.triples().is_empty();
            // Drop `QueryResult` (whose context retains `Arc<Snapshot>`) before releasing
            // the snapshot's replay charge.
            drop(res);
            charge.release(snap_charge);
            if empty {
                Ok(ProbeResult::Empty)
            } else {
                Ok(ProbeResult::Readable)
            }
        }
        Err(e) => {
            charge.release(snap_charge);
            if at.is_some() && !matches!(e, sparkles::Error::Cancelled) {
                Ok(ProbeResult::ReplayFailed(e))
            } else {
                Err(ApiError::from(e))
            }
        }
    }
}

/// Whether `principal` can read at least one triple for `sparql` on `plan`'s live
/// snapshot. Returns `false` if live readability cannot be established within the
/// remaining budget or deadline.
#[allow(clippy::too_many_arguments)]
fn live_is_readable(
    st: &AppState,
    principal: &Principal,
    plan: &DatasetPlan,
    sparql: &str,
    grant: &Option<Arc<PoolGrant>>,
    charge: &mut MemoryCharge,
    deadline: &Deadline,
) -> bool {
    matches!(
        probe_one_dataset(
            st,
            principal,
            plan,
            sparql,
            None,
            grant,
            charge,
            deadline,
            #[cfg(test)]
            TestHooks::default(),
        ),
        Ok(ProbeResult::Readable)
    )
}

#[allow(clippy::too_many_arguments)]
fn probe_any_readable(
    st: &AppState,
    principal: &Principal,
    plans: &[DatasetPlan],
    sparql: &str,
    at: Option<&At>,
    grant: &Option<Arc<PoolGrant>>,
    charge: &mut MemoryCharge,
    deadline: &Deadline,
    hide_replay_errors: bool,
    #[cfg(test)] hooks: TestHooks,
) -> ApiResult<ProbeResult> {
    let mut saw_gone = false;
    for plan in plans {
        match probe_one_dataset(
            st,
            principal,
            plan,
            sparql,
            at,
            grant,
            charge,
            deadline,
            #[cfg(test)]
            hooks,
        )? {
            ProbeResult::Readable => return Ok(ProbeResult::Readable),
            ProbeResult::Empty => {}
            ProbeResult::HistoryGone => {
                // Only report 410 if the caller can read at least one triple on the live
                // snapshot; otherwise hidden terms stay 404.
                if !hide_replay_errors
                    && live_is_readable(st, principal, plan, sparql, grant, charge, deadline)
                {
                    saw_gone = true;
                }
            }
            ProbeResult::ReplayFailed(e) => {
                // Never disclose a matched catalog entry or replay byte count before
                // establishing that the caller can read the target.
                if !hide_replay_errors
                    && live_is_readable(st, principal, plan, sparql, grant, charge, deadline)
                {
                    return Err(ApiError::from(e));
                }
            }
        }
    }
    if saw_gone {
        Ok(ProbeResult::HistoryGone)
    } else {
        Ok(ProbeResult::Empty)
    }
}

/// Resolve and run a bounded, authorized linked-data lookup for `target`.
///
/// When `redirect_slash` is true and the matched target is a slash IRI, executes a
/// 1-triple existence probe under the caller's ACLs and returns [`LookupOutcome::Redirect`]
/// instead of materializing a full description.
#[allow(clippy::too_many_arguments)]
pub fn resolve_and_describe(
    st: &AppState,
    principal: &Principal,
    catalog: &CatalogFile,
    target: LookupTarget,
    requested_version: Option<&str>,
    at_override: Option<At>,
    describe_params: &Params,
    grant: Option<Arc<PoolGrant>>,
    redirect_slash: bool,
) -> ApiResult<LookupOutcome> {
    resolve_and_describe_inner(
        st,
        principal,
        catalog,
        target,
        requested_version,
        at_override,
        describe_params,
        grant,
        redirect_slash,
        #[cfg(test)]
        TestHooks::default(),
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_and_describe_with_hooks(
    st: &AppState,
    principal: &Principal,
    catalog: &CatalogFile,
    target: LookupTarget,
    requested_version: Option<&str>,
    at_override: Option<At>,
    describe_params: &Params,
    grant: Option<Arc<PoolGrant>>,
    redirect_slash: bool,
    hooks: TestHooks,
) -> ApiResult<LookupOutcome> {
    resolve_and_describe_inner(
        st,
        principal,
        catalog,
        target,
        requested_version,
        at_override,
        describe_params,
        grant,
        redirect_slash,
        hooks,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_and_describe_inner(
    st: &AppState,
    principal: &Principal,
    catalog: &CatalogFile,
    target: LookupTarget,
    requested_version: Option<&str>,
    at_override: Option<At>,
    describe_params: &Params,
    grant: Option<Arc<PoolGrant>>,
    redirect_slash: bool,
    #[cfg(test)] hooks: TestHooks,
) -> ApiResult<LookupOutcome> {
    let iri = match target {
        LookupTarget::Iri(iri) => {
            if !valid_lookup_iri(&iri) {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "invalid-iri",
                    "iri must be a valid absolute IRI",
                ));
            }
            iri
        }
        LookupTarget::Namespace { prefix, local } => {
            if let Some(loc) = local.as_deref().filter(|s| !s.is_empty())
                && !valid_local_part(loc)
            {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "invalid-iri",
                    "iri must be a valid absolute IRI",
                ));
            }
            let expanded =
                resolve_prefix(catalog, &prefix, local.as_deref()).ok_or_else(not_found)?;
            if !valid_lookup_iri(&expanded) {
                return Err(not_found());
            }
            expanded
        }
    };

    let Some(resolved_target) = resolve_iri(catalog, &iri, requested_version) else {
        return Err(not_found());
    };
    let permitted = permitted_locations(st, principal, &resolved_target);
    if permitted.is_empty() {
        return Err(not_found());
    }
    if resolved_target.version_missing {
        return Err(not_found());
    }

    // Precompute every permitted dataset's DESCRIBE options and the minimum global cap
    // before running any query so a later dataset can never lower the cap retroactively.
    let mut plans = Vec::with_capacity(permitted.len());
    let mut global_cap = DEFAULT_MAX_TRIPLES as usize;
    for (ds, allowed_graphs) in permitted {
        let desc = crate::http::describe::request_options(&ds, describe_params)?
            .lowered(Some(DEFAULT_MAX_TRIPLES), Some(DEFAULT_MAX_DEPTH));
        if let Some(limit) = desc.max_triples {
            global_cap = global_cap.min(limit as usize);
        }
        plans.push(DatasetPlan {
            ds,
            allowed_graphs,
            describe: desc,
        });
    }

    #[allow(unused_mut)]
    let mut deadline = Deadline::new(st.default_timeout);
    let mut charge = MemoryCharge::new(grant.clone(), st.limits.query_memory_bytes);
    let sparql = describe_sparql(&iri, &resolved_target);

    if resolved_target.version_conflict {
        let pin_at = resolved_target
            .pinned_snapshot()
            .and_then(|s| s.parse::<At>().ok());
        match probe_any_readable(
            st,
            principal,
            &plans,
            &sparql,
            pin_at.as_ref(),
            &grant,
            &mut charge,
            &deadline,
            true,
            #[cfg(test)]
            hooks,
        )? {
            ProbeResult::Readable => {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "version-conflict",
                    "version parameter conflicts with the requested version IRI",
                ));
            }
            ProbeResult::Empty | ProbeResult::HistoryGone | ProbeResult::ReplayFailed(_) => {
                return Err(not_found());
            }
        }
    }

    let effective_at = match (resolved_target.pinned_snapshot(), at_override) {
        (Some(pin_str), Some(req_at)) => {
            let pin_at: At = pin_str.parse().map_err(|e: sparkles::Error| {
                err_code(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "bad-snapshot",
                    e.to_string(),
                )
            })?;
            if pin_at != req_at {
                match probe_any_readable(
                    st,
                    principal,
                    &plans,
                    &sparql,
                    Some(&pin_at),
                    &grant,
                    &mut charge,
                    &deadline,
                    true,
                    #[cfg(test)]
                    hooks,
                )? {
                    ProbeResult::Readable => {
                        return Err(err_code(
                            StatusCode::BAD_REQUEST,
                            "snapshot-conflict",
                            "at parameter conflicts with the version's pinned snapshot",
                        ));
                    }
                    ProbeResult::Empty
                    | ProbeResult::HistoryGone
                    | ProbeResult::ReplayFailed(_) => return Err(not_found()),
                }
            }
            Some(pin_at)
        }
        (Some(pin_str), None) => Some(pin_str.parse().map_err(|e: sparkles::Error| {
            err_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "bad-snapshot",
                e.to_string(),
            )
        })?),
        (None, req_at) => req_at,
    };

    // For slash IRIs on redirecting endpoints (/$/ld, /$/ld/ns/...), verify at least one
    // readable triple exists via a 1-triple probe rather than running a full DESCRIBE.
    if redirect_slash && !resolved_target.is_hash {
        return match probe_any_readable(
            st,
            principal,
            &plans,
            &sparql,
            effective_at.as_ref(),
            &grant,
            &mut charge,
            &deadline,
            false,
            #[cfg(test)]
            hooks,
        )? {
            ProbeResult::Readable => Ok(LookupOutcome::Redirect { iri }),
            ProbeResult::HistoryGone => Err(history_gone()),
            ProbeResult::Empty | ProbeResult::ReplayFailed(_) => Err(not_found()),
        };
    }

    let multi = plans.len() > 1;
    let mut single_result: Option<QueryResult> = None;
    let mut retained_snapshot_bytes: u64 = 0;
    let mut merged: Vec<Triple> = Vec::new();
    let mut merged_bytes: u64 = 0;
    let mut seen: Option<HashSet<Triple>> = multi.then(HashSet::new);
    let mut seen_bytes: u64 = 0;
    let mut truncated = false;
    let mut prefixes: BTreeMap<String, String> = BTreeMap::new();
    let mut contributing_datasets: usize = 0;
    let mut single_history: Option<Resolved> = None;
    let mut saw_history_gone = false;

    // Seed common semantic-web prefixes plus the matched resource's own namespaces.
    for (p, ns) in [
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("skos", "http://www.w3.org/2004/02/skos/core#"),
        ("dcterms", "http://purl.org/dc/terms/"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ] {
        prefixes.insert(p.to_string(), ns.to_string());
    }
    for ns in &resolved_target.resource.input.namespaces {
        prefixes.insert(ns.prefix.clone(), ns.iri.clone());
    }

    for (idx, plan) in plans.iter().enumerate() {
        #[cfg(test)]
        if hooks.expire_deadline_before_dataset == Some(idx) {
            deadline.expire_now();
        }
        let readability_established = contributing_datasets > 0;
        let mut opts = CallerContext::for_caller(st, &plan.ds, principal).read_options();
        opts.grant = grant.clone();
        opts.timeout = match deadline.left() {
            Ok(t) => t,
            Err(e) => {
                // Once an earlier dataset has contributed readable triples, propagate
                // timeout rather than returning an incomplete description as an unmarked
                // 200. Before any readable triple is established, keep the generic 404
                // because `live_is_readable` cannot run under the expired deadline.
                if effective_at.is_some()
                    && !matches!(e, sparkles::Error::Cancelled)
                    && !readability_established
                {
                    continue;
                }
                return Err(ApiError::from(e));
            }
        };
        let mut desc = plan.describe.clone();
        desc.max_triples = Some(global_cap as u64);
        opts.describe = desc;
        if !plan.allowed_graphs.is_empty() {
            opts.default_graph_uris = plan.allowed_graphs.clone();
            // Prevent materialized inferences from unrelated graphs leaking into this
            // vocabulary's linked-data view.
            opts.default_graph_extra.clear();
        }

        let (snap, resolved, snap_charge) = if effective_at.is_some() {
            let reserved = match charge.reserve_replay(plan.ds.store.history_cache_capacity()) {
                Ok(r) => r,
                Err(sparkles::Error::Cancelled) => {
                    return Err(ApiError::from(sparkles::Error::Cancelled));
                }
                Err(e) => {
                    if readability_established
                        || live_is_readable(
                            st,
                            principal,
                            plan,
                            &sparql,
                            &grant,
                            &mut charge,
                            &deadline,
                        )
                    {
                        return Err(ApiError::from(e));
                    }
                    continue;
                }
            };
            let (snap, resolved, retained) = match crate::http::history::snapshot_for_limited(
                &plan.ds,
                effective_at.as_ref(),
                &opts,
                reserved,
            ) {
                Ok(triple) => triple,
                Err(e) => {
                    if let Some(r) = reserved {
                        charge.release(r);
                    }
                    match e {
                        sparkles::Error::NotFound(_) => continue,
                        sparkles::Error::HistoryGone(_) => {
                            if live_is_readable(
                                st,
                                principal,
                                plan,
                                &sparql,
                                &grant,
                                &mut charge,
                                &deadline,
                            ) {
                                saw_history_gone = true;
                            }
                            continue;
                        }
                        sparkles::Error::Cancelled => return Err(ApiError::from(e)),
                        other => {
                            if readability_established
                                || live_is_readable(
                                    st,
                                    principal,
                                    plan,
                                    &sparql,
                                    &grant,
                                    &mut charge,
                                    &deadline,
                                )
                            {
                                return Err(ApiError::from(other));
                            }
                            continue;
                        }
                    }
                }
            };
            let held = match charge.settle_replay(reserved, retained) {
                Ok(h) => h,
                Err(e) => {
                    drop(snap);
                    if let Some(r) = reserved {
                        charge.release(r);
                    }
                    if readability_established
                        || live_is_readable(
                            st,
                            principal,
                            plan,
                            &sparql,
                            &grant,
                            &mut charge,
                            &deadline,
                        )
                    {
                        return Err(ApiError::from(e));
                    }
                    continue;
                }
            };
            #[cfg(test)]
            let held = if hooks.exhaust_budget_after_settle {
                if let Ok(Some(rem)) = charge.engine_budget() {
                    let _ = charge.charge(rem);
                    held.saturating_add(rem)
                } else {
                    held
                }
            } else {
                held
            };
            (snap, resolved, held)
        } else {
            (plan.ds.store.snapshot(), None, 0)
        };

        opts.max_memory_bytes = match charge.engine_budget() {
            Ok(b) => b,
            Err(e) => {
                drop(snap);
                charge.release(snap_charge);
                if effective_at.is_some()
                    && !matches!(e, sparkles::Error::Cancelled)
                    && !readability_established
                    && !live_is_readable(
                        st,
                        principal,
                        plan,
                        &sparql,
                        &grant,
                        &mut charge,
                        &deadline,
                    )
                {
                    continue;
                }
                return Err(ApiError::from(e));
            }
        };
        let res = match sparkles::sparql::query(snap.clone(), &sparql, &opts) {
            Ok(r) => r,
            Err(e) => {
                drop(snap);
                charge.release(snap_charge);
                if effective_at.is_some()
                    && !matches!(e, sparkles::Error::Cancelled)
                    && !readability_established
                    && !live_is_readable(
                        st,
                        principal,
                        plan,
                        &sparql,
                        &grant,
                        &mut charge,
                        &deadline,
                    )
                {
                    continue;
                }
                return Err(ApiError::from(e));
            }
        };
        if res.describe_truncated {
            truncated = true;
        }
        // Detect hash-document term-limit truncation when the query returned enough
        // triples that more than HASH_NS_TERM_LIMIT terms may exist.
        if resolved_target.is_hash_document
            && !truncated
            && res.triples().len() >= HASH_NS_TERM_LIMIT
        {
            let res_bytes: u64 = res.triples().iter().map(triple_bytes).sum();
            charge.set_external(res_bytes);
            let overflow_res = (|| -> ApiResult<bool> {
                let mut check_opts = opts.clone();
                check_opts.timeout = deadline.left().map_err(ApiError::from)?;
                check_opts.max_memory_bytes = charge.engine_budget().map_err(ApiError::from)?;
                let overflow_q = hash_doc_overflow_sparql(&iri, &resolved_target);
                let overflow = sparkles::sparql::query(snap.clone(), &overflow_q, &check_opts)
                    .map_err(ApiError::from)?;
                Ok(!overflow.is_empty())
            })();
            charge.clear_external();
            match overflow_res {
                Ok(true) => truncated = true,
                Ok(false) => {}
                Err(e) => {
                    drop(res);
                    drop(snap);
                    charge.release(snap_charge);
                    return Err(e);
                }
            }
        }
        // Drop our local `Arc<Snapshot>`; `res` still owns a clone via its query context,
        // so `snap_charge` stays live until `res` itself is dropped.
        drop(snap);

        if !multi {
            if !res.triples().is_empty() {
                let res_bytes: u64 = res.triples().iter().map(triple_bytes).sum();
                charge.set_external(res_bytes);
                retained_snapshot_bytes = snap_charge;
                single_history = resolved;
                for (p, ns) in plan.ds.dataset.prefixes() {
                    prefixes.entry(p).or_insert(ns);
                }
                single_result = Some(res);
            } else {
                drop(res);
                charge.release(snap_charge);
            }
            break;
        }

        // Multi-dataset merge: keep `res`'s live bytes and `snap_charge` accounted while
        // iterating, and charge every triple BEFORE cloning it.
        let res_bytes: u64 = res.triples().iter().map(triple_bytes).sum();
        charge.set_external(res_bytes);
        let bnode_prefix = format!("d{idx}");
        let mut contributed = false;
        let merge_res: ApiResult<()> = (|| {
            for t in res.triples() {
                let has_bnode = matches!(&t.subject, NamedOrBlankNode::BlankNode(_))
                    || matches!(&t.object, Term::BlankNode(_));
                let add_bytes = scoped_triple_bytes(t, &bnode_prefix);
                if !has_bnode {
                    if let Some(set) = &seen
                        && set.contains(t)
                    {
                        continue;
                    }
                    if merged.len() >= global_cap {
                        truncated = true;
                        break;
                    }
                    let total_add = if seen.is_some() {
                        add_bytes.saturating_mul(2)
                    } else {
                        add_bytes
                    };
                    charge.charge(total_add).map_err(ApiError::from)?;
                    let triple = t.clone();
                    if let Some(set) = &mut seen {
                        set.insert(triple.clone());
                        seen_bytes = seen_bytes.saturating_add(add_bytes);
                    }
                    merged.push(triple);
                    merged_bytes = merged_bytes.saturating_add(add_bytes);
                    contributed = true;
                } else {
                    charge.charge(add_bytes).map_err(ApiError::from)?;
                    let triple = scope_bnode(t.clone(), &bnode_prefix);
                    if let Some(set) = &seen
                        && set.contains(&triple)
                    {
                        drop(triple);
                        charge.release(add_bytes);
                        continue;
                    }
                    if merged.len() >= global_cap {
                        drop(triple);
                        charge.release(add_bytes);
                        truncated = true;
                        break;
                    }
                    if let Some(set) = &mut seen {
                        if let Err(e) = charge.charge(add_bytes) {
                            drop(triple);
                            charge.release(add_bytes);
                            return Err(ApiError::from(e));
                        }
                        set.insert(triple.clone());
                        seen_bytes = seen_bytes.saturating_add(add_bytes);
                    }
                    merged.push(triple);
                    merged_bytes = merged_bytes.saturating_add(add_bytes);
                    contributed = true;
                }
            }
            Ok(())
        })();
        drop(res);
        charge.clear_external();
        charge.release(snap_charge);
        merge_res?;

        if contributed {
            contributing_datasets += 1;
            if contributing_datasets == 1 {
                single_history = resolved;
            } else {
                single_history = None;
            }
            for (p, ns) in plan.ds.dataset.prefixes() {
                prefixes.entry(p).or_insert(ns);
            }
        }
        if merged.len() >= global_cap && truncated {
            break;
        }
    }

    drop(seen);
    charge.release(seen_bytes);

    let triples_source = if let Some(res) = single_result {
        TriplesSource::Single(res)
    } else if !merged.is_empty() {
        TriplesSource::Merged(merged)
    } else {
        if saw_history_gone {
            return Err(history_gone());
        }
        return Err(not_found());
    };

    let licence = resolved_target
        .resource
        .licence_of(resolved_target.version)
        .expression
        .map(str::to_string);

    Ok(LookupOutcome::Document(Box::new(Description {
        iri,
        triples: triples_source,
        truncated,
        prefixes,
        resolved_history: single_history,
        resource_title: resolved_target.resource.input.title.clone(),
        version_name: resolved_target.version.map(|v| v.input.version.clone()),
        namespace_prefix: resolved_target.namespace.map(|n| n.prefix.clone()),
        license: licence,
        iri_pinned_version: resolved_target.iri_pinned_version,
        readable_targets: readable_targets(st, principal, catalog, &resolved_target.resource.id),
        memory_charge: charge,
        retained_triples_bytes: merged_bytes,
        retained_snapshot_bytes,
    })))
}

pub(crate) struct ChargedLimitedWriter<'a> {
    buf: Vec<u8>,
    max_bytes: Option<u64>,
    charge: &'a mut MemoryCharge,
    charged: u64,
    budget_err: Option<sparkles::Error>,
}

impl<'a> ChargedLimitedWriter<'a> {
    pub(crate) fn new(max_bytes: Option<u64>, charge: &'a mut MemoryCharge) -> Self {
        Self {
            buf: Vec::new(),
            max_bytes,
            charge,
            charged: 0,
            budget_err: None,
        }
    }

    fn reserve_for(&mut self, add: usize) -> sparkles::Result<()> {
        let Some(want) = self.buf.len().checked_add(add) else {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Memory,
                limit: self.max_bytes.unwrap_or(u64::MAX),
                requested: u64::MAX,
            }));
        };
        if let Some(limit) = self.max_bytes
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

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    #[cfg(test)]
    pub(crate) fn take_budget_err(&mut self) -> Option<sparkles::Error> {
        self.budget_err.take()
    }
}

impl std::io::Write for ChargedLimitedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if let Err(e) = self.reserve_for(data.len()) {
            self.budget_err = Some(e);
            return Err(std::io::Error::other("output budget exceeded"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialize `desc` into the requested RDF syntax, enforcing both `max_bytes` and
/// `desc.memory_charge` (query memory ceiling and admitted pool grant) before growing the
/// output buffer.
pub fn serialize_rdf(
    desc: &mut Description,
    format: RdfFormat,
    max_bytes: Option<u64>,
) -> ApiResult<Vec<u8>> {
    let prefix_iter = desc.prefixes.iter().map(|(p, ns)| (p.clone(), ns.clone()));
    let ser = sparkles::io::with_prefixes(RdfSerializer::from_format(format), prefix_iter);
    let triples = desc.triples.as_slice();
    let mut writer = ChargedLimitedWriter::new(max_bytes, &mut desc.memory_charge);
    let mut w = ser.for_writer(&mut writer);
    for t in triples {
        if let Err(e) = w.serialize_triple(t) {
            drop(w);
            return Err(ApiError::from(
                writer.budget_err.unwrap_or(sparkles::Error::Io(e)),
            ));
        }
    }
    if let Err(e) = w.finish() {
        return Err(ApiError::from(
            writer.budget_err.unwrap_or(sparkles::Error::Io(e)),
        ));
    }
    let _ = writer.flush();
    Ok(writer.buf)
}
