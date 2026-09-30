//! SHACL validation shared by the `/{ds}/shacl` endpoint and `sparkles shacl`:
//! data-graph selection (Fuseki's `graph=default|union|<iri>`), report formats.

use anyhow::{Context, Result};
use oxrdfio::{RdfFormat, RdfSerializer};
use serde_json::Value as J;
use sparkles::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use sparkles::store::Snapshot;
use sparkles_shacl::{ValidateOptions, ValidationReport};

/// The data graph selected by Fuseki's `graph` parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataGraph {
    /// the store's default graph (the union of all graphs with `--union-default-graph`)
    Default,
    /// the union of all graphs (`urn:x-arq:UnionGraph`)
    Union,
    Named(String),
}

impl DataGraph {
    /// `default`, `union`, the Jena special graph IRIs, or a graph IRI.
    pub fn parse(s: &str) -> Result<DataGraph> {
        Ok(match s.trim() {
            "" | "default" | DEFAULT_GRAPH_IRI => DataGraph::Default,
            "union" | UNION_GRAPH_IRI => DataGraph::Union,
            iri => {
                let iri = iri
                    .strip_prefix('<')
                    .and_then(|i| i.strip_suffix('>'))
                    .unwrap_or(iri);
                oxrdf::NamedNode::new(iri).with_context(|| format!("invalid graph IRI '{iri}'"))?;
                DataGraph::Named(iri.to_string())
            }
        })
    }
}

/// Validation options for a data graph of `snap`. `inferred` is the graph of
/// materialized inferences, if any: with `use_inferred` it is merged into the data
/// graph, otherwise it is kept out (even from the union of all graphs, which would
/// include it).
pub fn validate_options(
    snap: &Snapshot,
    graph: &DataGraph,
    inferred: Option<&str>,
    use_inferred: bool,
) -> Result<ValidateOptions> {
    let all_graphs = matches!(graph, DataGraph::Union)
        || (*graph == DataGraph::Default && snap.union_default_graph);
    let mut opts = ValidateOptions::default();
    match graph {
        DataGraph::Named(iri) => opts.data_graph = Some(iri.clone()),
        DataGraph::Union => opts.data_graph = Some(UNION_GRAPH_IRI.to_string()),
        DataGraph::Default => {}
    }
    let Some(inferred) = inferred else {
        return Ok(opts);
    };
    if use_inferred {
        opts.extra_graphs.push(inferred.to_string());
    } else if all_graphs && snap.lookup_iri(inferred).is_some() {
        // The union of all graphs includes the inferred graph; spell it out without it.
        opts.data_graph = Some(DEFAULT_GRAPH_IRI.to_string());
        for g in snap.graph_ids()? {
            if let Some(oxrdf::Term::NamedNode(n)) = snap.term(g)
                && n.as_str() != inferred
            {
                opts.extra_graphs.push(n.into_string());
            }
        }
    }
    Ok(opts)
}

/// The fewest bytes one result takes in any report format (the one-line text form).
pub const MIN_RESULT_BYTES: u64 = 48;

/// Estimated memory of one result while the report is built.
const RESULT_MEMORY_BYTES: u64 = 512;

/// The most results a `/{ds}/shacl` report may hold: what fits in the response budget
/// (`--max-result-mb`) at [`MIN_RESULT_BYTES`] each, and in the memory budget
/// (`--query-memory-mb`) at an estimated 512 bytes each.
pub fn max_results(limits: &crate::state::Limits) -> Option<usize> {
    let by_bytes = limits.max_result_bytes.map(|b| b / MIN_RESULT_BYTES);
    let by_memory = limits.query_memory_bytes.map(|b| b / RESULT_MEMORY_BYTES);
    let n = match (by_bytes, by_memory) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(usize::try_from(n).unwrap_or(usize::MAX).max(1))
}

/// The thread pool of `/{ds}/shacl` validations: half the cores, shared by all
/// requests, so validations never take every core from queries.
pub fn pool() -> Option<std::sync::Arc<rayon::ThreadPool>> {
    static POOL: std::sync::OnceLock<Option<std::sync::Arc<rayon::ThreadPool>>> =
        std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(2, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads((n / 2).max(1))
            .thread_name(|i| format!("shacl-{i}"))
            .build()
            .map(std::sync::Arc::new)
            .ok()
    })
    .clone()
}

/// Does the named graph exist (hold at least one quad)?
pub fn graph_exists(snap: &Snapshot, iri: &str) -> bool {
    snap.lookup_iri(iri)
        .is_some_and(|g| snap.count(sparkles::index::Perm::Gspo, &[g.0]).unwrap_or(0) > 0)
}

/// Output formats of a validation report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportFormat {
    Rdf(RdfFormat),
    /// compact JSON (`{conforms, results: [...]}`)
    Json,
    /// human-readable summary
    Text,
}

impl ReportFormat {
    /// From a short name (`ttl`, `nt`, `jsonld`, `rdfxml`, `json`, `text`) or media type.
    pub fn from_name(s: &str) -> Option<ReportFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "json" | "application/json" => Some(ReportFormat::Json),
            "text" | "txt" => Some(ReportFormat::Text),
            // N-Quads / TriG of a single default graph are the N-Triples / Turtle forms
            other => sparkles::sparql::results::rdf_format_from_name(other).map(|f| {
                ReportFormat::Rdf(match f {
                    RdfFormat::NQuads => RdfFormat::NTriples,
                    RdfFormat::TriG => RdfFormat::Turtle,
                    f => f,
                })
            }),
        }
    }

    /// Accept-header offers, in preference order (Turtle first: the Fuseki default).
    pub const OFFERS: [&'static str; 6] = [
        "text/turtle",
        "application/n-triples",
        "application/ld+json",
        "application/json",
        "application/rdf+xml",
        "text/plain",
    ];

    pub fn media_type(self) -> &'static str {
        match self {
            ReportFormat::Rdf(f) => sparkles::sparql::results::rdf_media_type(f),
            ReportFormat::Json => "application/json",
            ReportFormat::Text => "text/plain; charset=utf-8",
        }
    }
}

/// Serialize a report.
pub fn write_report(report: &ValidationReport, fmt: ReportFormat) -> Result<Vec<u8>> {
    Ok(match fmt {
        ReportFormat::Rdf(RdfFormat::Turtle) => report.to_turtle().into_bytes(),
        ReportFormat::Rdf(f) => {
            let mut ser = RdfSerializer::from_format(f);
            if f == RdfFormat::RdfXml {
                ser = ser.with_prefix("sh", sparkles_shacl::vocab::SH_NS)?;
            }
            let mut w = ser.for_writer(Vec::new());
            for t in report.to_rdf() {
                w.serialize_triple(&t)?;
            }
            w.finish()?
        }
        ReportFormat::Json => serde_json::to_vec(&report_json(report))?,
        ReportFormat::Text => report.to_string().into_bytes(),
    })
}

/// Compact JSON form of a report (see [`sparkles_shacl::report::to_json`]).
pub fn report_json(report: &ValidationReport) -> J {
    sparkles_shacl::report::to_json(report)
}
