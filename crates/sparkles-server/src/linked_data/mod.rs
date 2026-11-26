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
use sparkles::sparql::results::LimitedWriter;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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

    /// Parse a `format=` query parameter or media type name.
    pub fn from_param(s: &str) -> Option<Format> {
        let base = s.split(';').next()?.trim().to_ascii_lowercase();
        match base.as_str() {
            "html" | "text/html" | "xhtml" | "application/xhtml+xml" => Some(Format::Html),
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

/// Negotiate an output [`Format`] from an HTTP `Accept` header following RFC 9110:
/// exact media type > `type/*` > `*/*`, honoring explicit `q=0` exclusions.
/// An absent or empty `Accept` header defaults to [`Format::Turtle`].
pub fn negotiate_format(accept: Option<&str>) -> Option<Format> {
    let Some(raw) = accept.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(Format::Turtle);
    };

    const OFFERS: [(&str, Format); 6] = [
        ("text/turtle", Format::Turtle),
        ("application/ld+json", Format::JsonLd),
        ("application/rdf+xml", Format::RdfXml),
        ("application/n-triples", Format::NTriples),
        ("text/html", Format::Html),
        ("application/xhtml+xml", Format::Html),
    ];

    let mut ranges: Vec<(String, f32)> = Vec::new();
    for part in raw.split(',') {
        let mut it = part.split(';');
        let mt = it.next().unwrap_or("").trim().to_ascii_lowercase();
        if mt.is_empty() || !mt.contains('/') {
            continue;
        }
        let mut q = 1.0f32;
        let mut bad_q = false;
        for param in it {
            let param = param.trim();
            if let Some(v) = param
                .strip_prefix("q=")
                .or_else(|| param.strip_prefix("Q="))
            {
                match v.trim().parse::<f32>() {
                    Ok(n) if n.is_finite() && (0.0..=1.0).contains(&n) => q = n,
                    _ => bad_q = true,
                }
            }
        }
        if !bad_q {
            ranges.push((mt, q));
        }
    }
    if ranges.is_empty() {
        return None;
    }

    let mut best: Option<(f32, u8, usize, Format)> = None;
    for (idx, (offer_mt, fmt)) in OFFERS.iter().enumerate() {
        let offer_type = offer_mt.split('/').next().unwrap_or("");
        // RFC 9110 §12.5.1: the most specific matching media range determines q for this offer.
        let mut matched: Option<(u8, f32)> = None;
        for (range_mt, q) in &ranges {
            let spec = if range_mt == offer_mt {
                3u8
            } else if let Some(prefix) = range_mt.strip_suffix("/*") {
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
            match matched {
                None => matched = Some((spec, *q)),
                Some((cur_spec, cur_q)) if spec > cur_spec || (spec == cur_spec && *q > cur_q) => {
                    matched = Some((spec, *q));
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

/// Validate an IRI before embedding it in a SPARQL query or resolving it.
pub fn valid_lookup_iri(iri: &str) -> bool {
    !iri.is_empty()
        && iri.len() <= 4096
        && !iri
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '{' | '}' | '"' | '`' | '\\' | ' '))
        && NamedNode::new(iri).is_ok()
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
    let (resource, mut version, namespace, is_hash_document, is_exact_version_iri) =
        match (best_ver, best_ns) {
            (Some((true, _, hdoc, r, v)), _) => {
                let ns = r
                    .input
                    .namespaces
                    .iter()
                    .find(|n| prefix_matches(&n.iri, iri))
                    .or_else(|| r.input.namespaces.first());
                (r, Some(v), ns, hdoc, true)
            }
            (Some((false, vlen, hdoc, r, v)), Some((nlen, _, _, _))) if vlen >= nlen => {
                let ns = r
                    .input
                    .namespaces
                    .iter()
                    .find(|n| prefix_matches(&n.iri, iri))
                    .or_else(|| r.input.namespaces.first());
                (r, Some(v), ns, hdoc, false)
            }
            (Some((false, _, hdoc, r, v)), None) => {
                let ns = r
                    .input
                    .namespaces
                    .iter()
                    .find(|n| prefix_matches(&n.iri, iri))
                    .or_else(|| r.input.namespaces.first());
                (r, Some(v), ns, hdoc, false)
            }
            (_, Some((_, hdoc, r, ns))) => (r, None, Some(ns), hdoc, false),
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
            // Dataset resource without per-version graph list: only permitted if the
            // caller's view is not completely empty (QueryOptions enforces graph & triple ACLs).
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

fn describe_sparql(iri: &str, target: &ResolvedTarget<'_>) -> String {
    if target.is_exact_version_iri {
        format!(
            "DESCRIBE <{iri}> ?ont WHERE {{ OPTIONAL {{ ?ont <http://www.w3.org/2002/07/owl#versionIRI> <{iri}> }} }}"
        )
    } else if target.is_hash_document {
        let with_hash = if iri.ends_with('#') {
            iri.to_string()
        } else {
            format!("{iri}#")
        };
        let without_hash = iri.trim_end_matches('#');
        let prefix = escape_sparql_string(&with_hash);
        format!(
            "DESCRIBE <{with_hash}> <{without_hash}> ?term WHERE {{ \
             OPTIONAL {{ \
               {{ SELECT DISTINCT ?term WHERE {{ \
                 {{ ?term a <http://www.w3.org/2002/07/owl#Ontology> }} \
                 UNION {{ ?term a <http://www.w3.org/2004/02/skos/core#ConceptScheme> }} \
                 UNION {{ ?term ?p ?o . FILTER(ISIRI(?term) && STRSTARTS(STR(?term), \"{prefix}\")) }} \
               }} ORDER BY ?term LIMIT {HASH_NS_TERM_LIMIT} }} \
             }} \
           }}"
        )
    } else {
        format!("DESCRIBE <{iri}>")
    }
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

/// Authorized, bounded description of an IRI.
pub struct Description {
    pub iri: String,
    pub triples: Vec<Triple>,
    pub truncated: bool,
    pub prefixes: BTreeMap<String, String>,
    pub resolved_history: Option<Resolved>,
    pub resource_title: String,
    pub version_name: Option<String>,
    pub namespace_prefix: Option<String>,
    pub license: Option<String>,
    pub is_hash: bool,
    /// Hosted namespace IRIs that the caller is permitted to read (for safe internal links).
    pub readable_namespaces: Vec<String>,
}

/// Generic 404 used whenever a resource, namespace, or term is absent or not readable.
pub fn not_found() -> ApiError {
    err_code(
        StatusCode::NOT_FOUND,
        "linked-data-not-found",
        "resource not found",
    )
}

/// Namespaces of vocabularies in `catalog` that `principal` may read at least one graph of.
fn readable_namespaces(st: &AppState, principal: &Principal, catalog: &CatalogFile) -> Vec<String> {
    let mut out = Vec::new();
    for r in &catalog.resources {
        if r.input.kind != Kind::Vocabulary {
            continue;
        }
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
        if readable {
            for ns in &r.input.namespaces {
                out.push(ns.iri.clone());
            }
        }
    }
    out
}

/// Resolve and run a bounded, authorized `DESCRIBE` for `iri`.
pub fn describe_authorized(
    st: &AppState,
    principal: &Principal,
    catalog: &CatalogFile,
    iri: &str,
    requested_version: Option<&str>,
    at_override: Option<At>,
    describe_params: &Params,
) -> ApiResult<Description> {
    if !valid_lookup_iri(iri) {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-iri",
            "iri must be a valid absolute IRI",
        ));
    }
    let Some(target) = resolve_iri(catalog, iri, requested_version) else {
        return Err(not_found());
    };
    let permitted = permitted_locations(st, principal, &target);
    if permitted.is_empty() {
        return Err(not_found());
    }
    if target.version_conflict {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "version-conflict",
            "version parameter conflicts with the requested version IRI",
        ));
    }
    if target.version_missing {
        return Err(not_found());
    }

    let effective_at = match (target.pinned_snapshot(), at_override) {
        (Some(pin_str), Some(req_at)) => {
            let pin_at: At = pin_str.parse().map_err(|e: sparkles::Error| {
                err_code(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "bad-snapshot",
                    e.to_string(),
                )
            })?;
            if pin_at != req_at {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "snapshot-conflict",
                    "at parameter conflicts with the version's pinned snapshot",
                ));
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

    let sparql = describe_sparql(iri, &target);
    let multi = permitted.len() > 1;
    let mut merged: Vec<Triple> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut truncated = false;
    let mut prefixes: BTreeMap<String, String> = BTreeMap::new();
    let mut resolved_history: Option<Resolved> = None;

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
    for ns in &target.resource.input.namespaces {
        prefixes.insert(ns.prefix.clone(), ns.iri.clone());
    }

    let mut global_cap = DEFAULT_MAX_TRIPLES as usize;
    for (idx, (ds, allowed_graphs)) in permitted.into_iter().enumerate() {
        let mut opts = CallerContext::for_caller(st, &ds, principal).read_options();
        let desc = crate::http::describe::request_options(&ds, describe_params)?
            .lowered(Some(DEFAULT_MAX_TRIPLES), Some(DEFAULT_MAX_DEPTH));
        if let Some(limit) = desc.max_triples {
            global_cap = global_cap.min(limit as usize);
        }
        opts.describe = desc;
        if !allowed_graphs.is_empty() {
            opts.default_graph_uris = allowed_graphs;
            // Prevent materialized inferences from unrelated graphs leaking into this
            // vocabulary's linked-data view.
            opts.default_graph_extra.clear();
        }
        let (snap, resolved) =
            crate::http::history::snapshot_for(&ds, effective_at.as_ref(), &opts)
                .map_err(ApiError::from)?;
        if resolved_history.is_none() {
            resolved_history = resolved;
        }
        let res = sparkles::sparql::query(snap, &sparql, &opts).map_err(ApiError::from)?;
        if res.describe_truncated {
            truncated = true;
        }
        for (p, ns) in ds.dataset.prefixes() {
            prefixes.entry(p).or_insert(ns);
        }
        let bnode_prefix = format!("d{idx}");
        for t in res.triples() {
            let triple = if multi {
                scope_bnode(t.clone(), &bnode_prefix)
            } else {
                t.clone()
            };
            let key = triple.to_string();
            if seen.insert(key) {
                if merged.len() >= global_cap {
                    truncated = true;
                    break;
                }
                merged.push(triple);
            }
        }
        if merged.len() >= global_cap && truncated {
            break;
        }
    }

    if merged.is_empty() {
        return Err(not_found());
    }

    let licence = target
        .resource
        .licence_of(target.version)
        .expression
        .map(str::to_string);

    Ok(Description {
        iri: iri.to_string(),
        triples: merged,
        truncated,
        prefixes,
        resolved_history,
        resource_title: target.resource.input.title.clone(),
        version_name: target.version.map(|v| v.input.version.clone()),
        namespace_prefix: target.namespace.map(|n| n.prefix.clone()),
        license: licence,
        is_hash: target.is_hash,
        readable_namespaces: readable_namespaces(st, principal, catalog),
    })
}

/// Serialize `desc` into the requested RDF syntax, respecting `max_bytes`.
pub fn serialize_rdf(
    desc: &Description,
    format: RdfFormat,
    max_bytes: Option<u64>,
) -> ApiResult<Vec<u8>> {
    let prefix_iter = desc.prefixes.iter().map(|(p, ns)| (p.clone(), ns.clone()));
    let ser = sparkles::io::with_prefixes(RdfSerializer::from_format(format), prefix_iter);
    let mut writer = LimitedWriter::new(Vec::new(), max_bytes, None);
    let mut w = ser.for_writer(&mut writer);
    for t in &desc.triples {
        if let Err(e) = w.serialize_triple(t) {
            drop(w);
            return Err(ApiError::from(writer.classify(sparkles::Error::Io(e))));
        }
    }
    if let Err(e) = w.finish() {
        return Err(ApiError::from(writer.classify(sparkles::Error::Io(e))));
    }
    Ok(writer.into_inner())
}
