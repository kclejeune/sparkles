//! SPARQL 1.1 Service Description (<https://www.w3.org/TR/sparql11-service-description/>):
//! `GET /{ds}/sparql` or `/{ds}/query` without a query, asking for RDF, describes the
//! dataset's query service and its update service.
//!
//! The description names the endpoints, the languages, the result and input formats,
//! the features, every extension function, aggregate and property function of this build
//! ([`sparkles::sparql::catalog`]), the default entailment regime and the default
//! dataset, with a link to the dataset's VoID description (`/$/schema/{ds}`). Only callers
//! that may query the dataset get here. Named graphs a caller may not read are left out,
//! and triple counts are given only to callers that see every graph.

use super::{ApiResult, INFERRED_GRAPH, Params, blocking, negotiate};
use crate::auth::{Endpoint, Principal, ServerPerm};
use crate::state::{AppState, Dataset};
use axum::http::{HeaderMap, Method, header};
use axum::response::{IntoResponse, Response};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfSerializer};
use sparkles::id::Id;
use sparkles::index::Perm;
use sparkles::sparql::{catalog, results};
use std::sync::Arc;

const SD: &str = "http://www.w3.org/ns/sparql-service-description#";
const FORMATS: &str = "http://www.w3.org/ns/formats/";
const ENT: &str = "http://www.w3.org/ns/entailment/";
const OWL_PROFILE_RL: &str = "http://www.w3.org/ns/owl-profile/RL";
const VOID: &str = "http://rdfs.org/ns/void#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// The named graphs described at most.
const MAX_GRAPHS: usize = 1000;

/// The RDF syntax of a request for the service description: a `GET` or `HEAD` without
/// `query` or `update` that asks for RDF with `format=` or `Accept` (no `Accept`, or
/// `*/*`, is Turtle). `None` for every other request.
pub(super) fn requested(
    method: &Method,
    params: &Params,
    headers: &HeaderMap,
) -> Option<RdfFormat> {
    if !matches!(*method, Method::GET | Method::HEAD) || params.has("query") || params.has("update")
    {
        return None;
    }
    if let Some(f) = params.get("format") {
        return results::rdf_format_from_name(f);
    }
    const OFFERS: [&str; 6] = [
        "text/turtle",
        "application/n-triples",
        "application/ld+json",
        "application/rdf+xml",
        "application/trig",
        "application/n-quads",
    ];
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    negotiate(accept, &OFFERS).and_then(|i| results::rdf_format_from_name(OFFERS[i]))
}

/// The external base URL: `server.public_url`, or the request's scheme and `Host`.
fn base_url(st: &AppState, h: &HeaderMap) -> String {
    if let Some(u) = crate::auth::public_url(st) {
        return u.trim_end_matches('/').to_string();
    }
    let scheme = h
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|p| p.split(',').next().unwrap_or("http").trim().to_string())
        .filter(|p| p == "https" || p == "http")
        .unwrap_or_else(|| "http".into());
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn sd(local: &str) -> NamedNode {
    iri(&format!("{SD}{local}"))
}

fn format_iri(local: &str) -> NamedNode {
    iri(&format!("{FORMATS}{local}"))
}

struct Out(Vec<Triple>);

impl Out {
    fn add(&mut self, s: &NamedOrBlankNode, p: NamedNode, o: impl Into<Term>) {
        self.0.push(Triple::new(s.clone(), p, o));
    }
}

/// What the description says about the dataset, read on a blocking thread.
struct DatasetFacts {
    union_default_graph: bool,
    /// triples in the default graph, for a caller that sees every graph (and a default
    /// graph that is one stored graph)
    default_triples: Option<u64>,
    /// named graphs the caller may read, with their triple counts for a caller that sees
    /// every graph
    named: Vec<(NamedNode, Option<u64>)>,
    /// more named graphs than [`MAX_GRAPHS`]
    truncated: bool,
}

fn facts(ds: &Dataset, p: &Principal, reasoning: bool) -> ApiResult<DatasetFacts> {
    let snap = ds.store.snapshot();
    let view = p.view(&ds.name, Endpoint::Query);
    let full = view.is_none();
    let union = snap.union_default_graph;
    let default_triples = if full && !union && !reasoning {
        Some(snap.count(Perm::Gspo, &[Id::DEFAULT_GRAPH.0])?)
    } else {
        None
    };
    let mut named = Vec::new();
    let mut truncated = false;
    for g in snap.distinct_first(Perm::Gspo)? {
        if g == Id::DEFAULT_GRAPH.0 {
            continue;
        }
        if view.as_ref().is_some_and(|v| !v.readable_id(&snap, Id(g))) {
            continue;
        }
        let Some(oxrdf::Term::NamedNode(n)) = snap.term(Id(g)) else {
            continue;
        };
        if named.len() == MAX_GRAPHS {
            truncated = true;
            break;
        }
        let count = if full {
            Some(snap.count(Perm::Gspo, &[g])?)
        } else {
            None
        };
        named.push((n, count));
    }
    Ok(DatasetFacts {
        union_default_graph: union,
        default_triples,
        named,
        truncated,
    })
}

/// The service description of dataset `ds`, as the caller `p` may see it, in `format`.
pub(super) async fn describe(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    p: Principal,
    headers: HeaderMap,
    path: String,
    format: RdfFormat,
) -> ApiResult<Response> {
    let base = base_url(&st, &headers);
    let reasoning = ds.reasoning.read().clone();
    let federate = st.allow_service && p.has(ServerPerm::Federate);
    let read_only = st.read_only;
    blocking(move || {
        let f = facts(&ds, &p, reasoning.is_some())?;
        let enc = |s: &str| utf8_percent(s);
        let name = enc(&ds.name);
        let mut out = Out(Vec::new());
        let dataset: NamedOrBlankNode = BlankNode::default().into();
        // the query service, and the update service unless the server is read-only
        let query: NamedOrBlankNode = iri(&format!("{base}{path}")).into();
        let mut services = vec![(query, false)];
        if !read_only {
            services.push((iri(&format!("{base}/{name}/update")).into(), true));
        }
        let functions = catalog::extension_functions();
        let aggregates = catalog::extension_aggregates();
        let properties = catalog::property_functions();
        for (s, update) in &services {
            let NamedOrBlankNode::NamedNode(endpoint) = s else {
                continue;
            };
            out.add(s, rdf::TYPE.into_owned(), sd("Service"));
            out.add(s, sd("endpoint"), endpoint.clone());
            let languages: &[&str] = if *update {
                &["SPARQL11Update", "SPARQLUpdate"]
            } else {
                &["SPARQL10Query", "SPARQL11Query", "SPARQLQuery"]
            };
            for l in languages {
                out.add(s, sd("supportedLanguage"), sd(l));
            }
            if *update {
                for f in [
                    "Turtle",
                    "N-Triples",
                    "N-Quads",
                    "TriG",
                    "RDF_XML",
                    "JSON-LD",
                ] {
                    out.add(s, sd("inputFormat"), format_iri(f));
                }
            } else {
                for f in [
                    "SPARQL_Results_JSON",
                    "SPARQL_Results_XML",
                    "SPARQL_Results_CSV",
                    "SPARQL_Results_TSV",
                    "Turtle",
                    "N-Triples",
                    "N-Quads",
                    "TriG",
                    "RDF_XML",
                    "JSON-LD",
                ] {
                    out.add(s, sd("resultFormat"), format_iri(f));
                }
            }
            if f.union_default_graph {
                out.add(s, sd("feature"), sd("UnionDefaultGraph"));
            }
            if federate {
                out.add(s, sd("feature"), sd("BasicFederatedQuery"));
            }
            for f in &functions {
                out.add(s, sd("extensionFunction"), iri(f));
            }
            for a in &aggregates {
                out.add(s, sd("extensionAggregate"), iri(a));
            }
            for pf in &properties {
                out.add(s, sd("propertyFeature"), iri(pf));
            }
            // inferences are materialized, so queries match them as stored triples
            let (regime, profile) = match reasoning.as_ref().map(|r| r.profile.as_str()) {
                Some("rdfs") => ("RDFS", None),
                Some("owl-rl") => ("OWL-RDF-Based", Some(OWL_PROFILE_RL)),
                _ => ("Simple", None),
            };
            out.add(
                s,
                sd("defaultEntailmentRegime"),
                iri(&format!("{ENT}{regime}")),
            );
            if let Some(pr) = profile {
                out.add(s, sd("defaultSupportedEntailmentProfile"), iri(pr));
            }
            if let Some(r) = &reasoning {
                out.add(
                    s,
                    iri(&format!("{RDFS}comment")),
                    Literal::new_simple_literal(format!(
                        "The default graph includes the inferences of the '{}' profile, \
                         materialized into <{INFERRED_GRAPH}>. They are not recomputed \
                         during query evaluation.",
                        r.profile
                    )),
                );
            }
            out.add(s, sd("defaultDataset"), dataset.clone());
        }
        // the default dataset
        out.add(&dataset, rdf::TYPE.into_owned(), sd("Dataset"));
        out.add(
            &dataset,
            iri(&format!("{RDFS}seeAlso")),
            iri(&format!("{base}/$/schema/{name}?format=turtle")),
        );
        let default: NamedOrBlankNode = BlankNode::default().into();
        out.add(&dataset, sd("defaultGraph"), default.clone());
        out.add(&default, rdf::TYPE.into_owned(), sd("Graph"));
        if let Some(n) = f.default_triples {
            out.add(&default, iri(&format!("{VOID}triples")), count(n));
        }
        for (g, n) in &f.named {
            let ng: NamedOrBlankNode = BlankNode::default().into();
            out.add(&dataset, sd("namedGraph"), ng.clone());
            out.add(&ng, rdf::TYPE.into_owned(), sd("NamedGraph"));
            out.add(&ng, sd("name"), g.clone());
            if let Some(n) = n {
                let graph: NamedOrBlankNode = BlankNode::default().into();
                out.add(&ng, sd("graph"), graph.clone());
                out.add(&graph, rdf::TYPE.into_owned(), sd("Graph"));
                out.add(&graph, iri(&format!("{VOID}triples")), count(*n));
            }
        }
        if f.truncated {
            out.add(
                &dataset,
                iri(&format!("{RDFS}comment")),
                Literal::new_simple_literal(format!(
                    "Only the first {MAX_GRAPHS} named graphs are listed."
                )),
            );
        }
        Ok((
            [(header::CONTENT_TYPE, results::rdf_media_type(format))],
            write(&out.0, format),
        )
            .into_response())
    })
    .await
}

fn count(n: u64) -> Literal {
    Literal::new_typed_literal(n.to_string(), xsd::INTEGER)
}

/// A dataset name as one path segment.
fn utf8_percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn write(triples: &[Triple], format: RdfFormat) -> String {
    let prefixes = [
        ("sd", SD),
        ("formats", FORMATS),
        ("ent", ENT),
        ("void", VOID),
        ("rdfs", RDFS),
        ("fn", catalog::FN),
        ("math", catalog::MATH),
        ("afn", sparkles::sparql::aggext::AFN),
        ("agg", spargebra::algebra::ARQ_AGGREGATE_NAMESPACE),
        ("text", "http://jena.apache.org/text#"),
        ("spk", "urn:x-sparkles:"),
        ("geof", "http://www.opengis.net/def/function/geosparql/"),
        ("spatial", "http://jena.apache.org/spatial#"),
        ("spatialF", "http://jena.apache.org/function/spatial#"),
    ]
    .into_iter()
    .map(|(p, ns)| (p.to_string(), ns.to_string()));
    let ser = sparkles::io::with_prefixes(RdfSerializer::from_format(format), prefixes);
    let mut w = ser.for_writer(Vec::new());
    for t in triples {
        w.serialize_triple(t)
            .expect("writing to memory does not fail");
    }
    String::from_utf8(w.finish().expect("writing to memory does not fail"))
        .expect("RDF syntaxes are UTF-8")
}
