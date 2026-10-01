//! SHACL validation shared by the `/{ds}/shacl` endpoint and `sparkles shacl`: the
//! validator's options for a data graph, report formats.

use crate::validation_common::GraphParam;
use anyhow::Result;
use oxrdfio::{RdfFormat, RdfSerializer};
use serde_json::Value as J;
use sparkles::store::Snapshot;
use sparkles_shacl::{ValidateOptions, ValidationReport};

/// Validation options for a data graph of `snap` (see
/// [`crate::validation_common::inputs`]).
pub fn validate_options(
    snap: &Snapshot,
    graph: &GraphParam,
    inferred: Option<&str>,
    use_inferred: bool,
) -> Result<ValidateOptions> {
    let i = crate::validation_common::inputs(snap, graph, inferred, use_inferred)?;
    Ok(ValidateOptions {
        data_graph: i.data_graph,
        extra_graphs: i.extra_graphs,
        exclude_graphs: i.exclude_graphs,
        ..Default::default()
    })
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
