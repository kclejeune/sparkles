//! The read tools of agent memory (spec C17 Phase 1a): `check_query`,
//! `similar_queries`, `link_entities` and `recall`.
//!
//! Every tool reads as the caller. Its queries go through [`Tools::query_options`],
//! which applies the caller's graph view and protections exactly as the SPARQL endpoint
//! does, and schema reports come from [`Tools::schema_report`], which gives a caller
//! limited to some graphs a report of its own. Nothing here reads the store around the
//! view, so a hidden entity, triple or graph can never be a candidate, a fact or a
//! citation, and a term that exists only in hidden data is reported as unknown, exactly
//! as an absent one.
//!
//! Every internal query is bounded by a `LIMIT` or by the caller's `VALUES`, so no tool
//! reads more than its caps (C17 §7) allow, and every query runs under the call's
//! deadline, memory budget and row limit.

mod assert;
mod brief;
mod check;
mod diagnose;
pub(crate) mod inbox;
pub(crate) mod ingest;
mod link;
pub(crate) mod policy;
mod recall;
pub(crate) mod review;
mod similar;
pub(crate) mod text;

use super::errors::{ErrorContext, ToolError};
use super::tools::{Tools, remaining};
use crate::state::Dataset;
use oxrdf::{NamedNode, Term};
use sparkles::error::Error;
use sparkles::sparql::{self, QueryOptions};
use sparkles::store::Snapshot;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

pub(crate) const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub(crate) const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
pub(crate) const RDFS_SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
pub(crate) const SKOS_ALT: &str = "http://www.w3.org/2004/02/skos/core#altLabel";
pub(crate) const SKOS_EXACT: &str = "http://www.w3.org/2004/02/skos/core#exactMatch";
pub(crate) const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
pub(crate) const PROV: &str = "http://www.w3.org/ns/prov#";
pub(crate) const SPK: &str = "urn:x-sparkles:";
/// The name grants and Jena give the default graph.
pub(crate) const DEFAULT_GRAPH: &str = "urn:x-arq:DefaultGraph";

/// The reciprocal rank fusion constant (Cormack et al.), as `spk:hybridSearch` uses.
pub(crate) const RRF_K: f64 = 60.0;

/// One snapshot read as the caller: the call's query options (the caller's view, its
/// budgets and deadline) and whether the materialized inferences take part.
pub(crate) struct Reader {
    pub snap: Arc<Snapshot>,
    pub opts: QueryOptions,
    pub deadline: Instant,
    pub reasoning: bool,
}

impl Reader {
    /// The rows of a SELECT query with `bindings` substituted, under the remaining time.
    pub fn rows(
        &self,
        q: &str,
        bindings: Vec<(String, Term)>,
    ) -> Result<Vec<Vec<Option<Term>>>, Error> {
        let mut opts = self.opts.clone();
        opts.timeout = Some(remaining(self.deadline)?);
        opts.initial_bindings = bindings;
        Ok(sparql::query(self.snap.clone(), q, &opts)?.rows())
    }

    /// An ASK query with `bindings` substituted.
    pub fn ask(&self, q: &str, bindings: Vec<(String, Term)>) -> Result<bool, Error> {
        let mut opts = self.opts.clone();
        opts.timeout = Some(remaining(self.deadline)?);
        opts.initial_bindings = bindings;
        Ok(sparql::query(self.snap.clone(), q, &opts)?.boolean)
    }

    /// `pattern` in every graph of the view, with `?g` bound to the graph: the default
    /// graph as [`DEFAULT_GRAPH`], and the named graphs by their names. The
    /// materialized inferences count only when the call reads them. `graphs` limits
    /// the graphs (IRIs, or [`DEFAULT_GRAPH`] for the default graph).
    pub fn quads(&self, pattern: &str, graphs: &[NamedNode]) -> String {
        self.quads_in(pattern, graphs, "g")
    }

    /// [`quads`](Self::quads) with the graph in `?<var>`.
    pub fn quads_in(&self, pattern: &str, graphs: &[NamedNode], var: &str) -> String {
        let mut out = String::new();
        if !graphs.is_empty() {
            out.push_str(&format!("VALUES ?{var} {{ "));
            for g in graphs {
                out.push_str(&g.to_string());
                out.push(' ');
            }
            out.push_str("} ");
        }
        out.push_str(&format!(
            "{{ {{ {pattern} }} BIND(<{DEFAULT_GRAPH}> AS ?{var}) }} UNION {{ GRAPH ?{var} {{ {pattern} }}"
        ));
        if !self.reasoning {
            out.push_str(&format!(
                " FILTER(?{var} != <{}>)",
                crate::http::INFERRED_GRAPH
            ));
        }
        out.push_str(" }");
        out
    }
}

/// `ASK` whether `?t` occurs in a triple of the reader's view, as subject, predicate or
/// object.
pub(crate) fn exists_term(r: &Reader) -> String {
    format!(
        "ASK {{ {{ {} }} UNION {{ {} }} UNION {{ {} }} }}",
        r.quads("?t ?p ?o", &[]),
        r.quads("?s ?p ?t", &[]),
        r.quads("?s ?t ?o", &[]),
    )
}

/// A term of a `VALUES` block, serialized by oxrdf (validated and escaped), never
/// copied from input text. Blank nodes cannot appear in `VALUES`.
pub(crate) fn values_term(t: &Term) -> Option<String> {
    match t {
        Term::NamedNode(n) => Some(n.to_string()),
        Term::Literal(l) => Some(l.to_string()),
        _ => None,
    }
}

/// `VALUES ?v { … }` of IRIs.
pub(crate) fn values_iris<'a>(var: &str, iris: impl IntoIterator<Item = &'a NamedNode>) -> String {
    let mut s = format!("VALUES ?{var} {{ ");
    for i in iris {
        s.push_str(&i.to_string());
        s.push(' ');
    }
    s.push('}');
    s
}

/// The named node of an IRI string (from the store or a validated argument).
pub(crate) fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

/// An IRI argument that must be a named node.
pub(crate) fn iri_arg(
    s: &str,
    prefixes: &BTreeMap<String, String>,
    what: &str,
) -> Result<NamedNode, ToolError> {
    match super::tools::parse_iri(s, prefixes, false)? {
        Term::NamedNode(n) => Ok(n),
        _ => Err(ToolError::bad_argument(format!("{what} must be IRIs"))),
    }
}

/// The local name of an IRI: what follows its last `#`, `/` or `:`.
pub(crate) fn local_name(iri: &str) -> &str {
    let i = iri.rfind(['#', '/', ':']).map_or(0, |i| i + 1);
    &iri[i..]
}

/// The graphs argument of `link_entities` and `recall`: at most 20 IRIs, or `default`.
pub(crate) fn graphs_arg(
    graphs: Option<&[String]>,
    prefixes: &BTreeMap<String, String>,
) -> Result<Vec<NamedNode>, ToolError> {
    let Some(graphs) = graphs else {
        return Ok(Vec::new());
    };
    if graphs.len() > 20 {
        return Err(ToolError::bad_argument("at most 20 graphs"));
    }
    if graphs.is_empty() {
        return Err(ToolError::bad_argument(
            "graphs must name at least one graph; leave it out to read every graph",
        ));
    }
    graphs
        .iter()
        .map(|g| match g.trim() {
            "default" => Ok(iri(DEFAULT_GRAPH)),
            g => iri_arg(g, prefixes, "graphs"),
        })
        .collect()
}

/// Whether a term is a vector literal, which no tool shows.
pub(crate) fn is_vector(t: &Term) -> bool {
    matches!(t, Term::Literal(l) if sparkles::vector::is_datatype(l.datatype().as_str()))
}

/// A JSON number rounded to 6 decimals (never NaN or infinite).
pub(crate) fn score_json(x: f64) -> serde_json::Value {
    if !x.is_finite() {
        return serde_json::Value::Null;
    }
    let r = (x * 1e6).round() / 1e6;
    serde_json::Number::from_f64(r).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

impl Tools<'_> {
    /// The reader of a call: the snapshot it names, the caller's query options for the
    /// `query` endpoint, and the deadline. Internal queries set the union default graph
    /// off, so that [`Reader::quads`] reads each quad once, and leave out the inferred
    /// default graph, which [`Reader::quads`] reads by its name.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reader(
        &self,
        ds: &Arc<Dataset>,
        at_commit: Option<u64>,
        at: Option<&serde_json::Value>,
        reasoning: Option<bool>,
        deadline: Instant,
        ctx: &ErrorContext,
    ) -> Result<Reader, ToolError> {
        let snap = self.snapshot(ds, at_commit, at, deadline)?;
        let reasoning = Self::reasoning(ds, reasoning);
        let mut opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &BTreeMap::new(),
            )
            .map_err(|e| ctx.engine(e))?;
        opts.union_default_graph = Some(false);
        opts.default_graph_extra.clear();
        Ok(Reader {
            snap,
            opts,
            deadline,
            reasoning,
        })
    }

    /// The schema report of the caller's view over every graph, or `None` when the
    /// caller's grants do not reach the `info` endpoint. A view restricted by graph
    /// grants or protections gets a report of its own.
    pub(crate) fn view_report(
        &self,
        ds: &Dataset,
        r: &Reader,
        ctx: &ErrorContext,
    ) -> Result<Option<Arc<sparkles::schema::SchemaReport>>, ToolError> {
        if self.info_endpoint(&ds.name).is_err() {
            return Ok(None);
        }
        let timeout = r.deadline.saturating_duration_since(self.call.arrived);
        self.schema_report(
            ds,
            &r.snap,
            sparkles::schema::GraphSelection::Union,
            r.reasoning,
            false,
            timeout,
            ctx,
        )
        .map(Some)
    }
}

/// The definition of `assert_facts`.
pub(crate) fn assert_tool(cfg: &super::McpConfig) -> [super::schemas::ToolDef; 1] {
    assert::tool_def(cfg)
}

/// The definitions of the ingestion tools.
pub(crate) fn ingest_tools(cfg: &super::McpConfig) -> [super::schemas::ToolDef; 4] {
    ingest::tool_defs(cfg)
}
