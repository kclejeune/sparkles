//! Schema discovery API (extension): `GET /$/schema/{ds}` (summary with the first page
//! of classes and predicates), `GET /$/schema/{ds}/classes` and
//! `GET /$/schema/{ds}/predicates` (cursor-paginated lists).
//!
//! Every page of a listing comes from one report computed at one snapshot. The cursor
//! carries that snapshot's identity, a hash of the selection parameters and the last IRI
//! served; the dataset keeps its last report so a listing can be finished after a write
//! (see [`sparkles::handles::Schema`], which computes, keeps and pages the reports).
//!
//! The summary is also a VoID description in RDF (`Accept: text/turtle` and the other RDF
//! syntaxes, or `format=`), followed by the declarations unless `declarations=false`.

use super::{ApiError, ApiResult, INFERRED_GRAPH, Params, St, blocking, dataset, err};
use crate::auth::Principal;
use crate::state::{AppState, Dataset};
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use oxrdfio::RdfFormat;
use sparkles::handles::{ReportRequest, schema_error_of};
use sparkles::schema::{self, GraphSelection, SchemaError, SchemaOptions, VoidOptions};
use sparkles::sparql::results;
use std::time::Duration;

const MAX_LIMIT: usize = 10_000;

/// Parsed request parameters: the report request, and its timeout for the messages.
struct Request {
    report: ReportRequest,
    timeout: Duration,
}

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

fn parse(st: &AppState, ds: &Dataset, uri: &Uri, p: &Principal) -> ApiResult<Request> {
    let params = Params::from_query(uri);
    let graph_param = |k: &str| -> ApiResult<Option<GraphSelection>> {
        params
            .get(k)
            .map(|v| GraphSelection::parse(v).map_err(|e| bad(format!("{k}: {e}"))))
            .transpose()
    };
    let graph = graph_param("graph")?.unwrap_or(GraphSelection::Default);
    let declared_graph = graph_param("declaredGraph")?;
    let reasoning = match params.get("reasoning") {
        None => ds.reasoning.read().is_some(),
        Some("true") => true,
        Some("false") => false,
        Some(v) => return Err(bad(format!("reasoning must be true or false, not '{v}'"))),
    };
    let declared_all = match params.get("declared") {
        None | Some("asserted") => false,
        Some("all") => true,
        Some(v) => return Err(bad(format!("declared must be asserted or all, not '{v}'"))),
    };
    let limit = match params.get("limit") {
        None => sparkles::handles::schema::DEFAULT_PAGE,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| bad(format!("limit must be an integer from 1 to {MAX_LIMIT}")))?,
    };
    let timeout = match params.get("timeout") {
        None => st.default_timeout,
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|t| t.is_finite() && *t > 0.0)
            .and_then(|t| Duration::try_from_secs_f64(t).ok())
            .map(|t| st.limits.cap_timeout(t, Some(st.default_timeout)))
            .ok_or_else(|| bad("timeout must be a positive number of seconds"))?,
    };
    let cursor = params
        .get("cursor")
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    let at = super::history::at_param(&params)?;
    let mut subject_classes = false;
    for d in params.all("detail") {
        for d in d.split(',').map(str::trim).filter(|d| !d.is_empty()) {
            match d {
                "subjectClasses" => subject_classes = true,
                d => return Err(bad(format!("unknown detail '{d}': subjectClasses"))),
            }
        }
    }
    Ok(Request {
        report: ReportRequest {
            options: SchemaOptions {
                graph,
                declared_graph,
                inferred_graph: Some(INFERRED_GRAPH.to_string()),
                include_inferred: reasoning,
                declared_from_inferred: declared_all,
                deadline: None,
                cancel: None,
                max_entries: st.schema_max_entries,
                term_totals: false,
                // a caller limited to some graphs gets a report of those only
                graphs: p.view(&ds.name, crate::auth::Endpoint::Info),
                subject_classes,
            },
            at,
            timeout: Some(timeout),
            limit,
            cursor,
        },
        timeout,
    })
}

/// The response to an error of a schema discovery call: a missing graph is `404`, a
/// deadline that passed `408`, cancellation `503` and too many entries `413`.
fn schema_error(e: sparkles::Error, timeout: Duration) -> ApiError {
    let Some(se) = schema_error_of(&e) else {
        return e.into();
    };
    match se {
        SchemaError::NoSuchGraph(_) => err(StatusCode::NOT_FOUND, se.to_string()),
        SchemaError::Timeout { phase } => err(
            StatusCode::REQUEST_TIMEOUT,
            format!(
                "schema discovery exceeded {}s while {phase}; narrow graph= or raise timeout=",
                timeout.as_secs_f64()
            ),
        ),
        SchemaError::Cancelled => err(StatusCode::SERVICE_UNAVAILABLE, se.to_string()),
        SchemaError::TooManyEntries { .. } => err(StatusCode::PAYLOAD_TOO_LARGE, se.to_string()),
        #[allow(unreachable_patterns)]
        _ => e.into(),
    }
}

/// The header that says how a report came about.
pub(crate) const COMPUTED_HEADER: &str = "sparkles-schema-report";

#[derive(Clone, Copy)]
enum What {
    Summary,
    /// the summary as a VoID description, with the declarations or without
    Void(RdfFormat, bool),
    Classes,
    Predicates,
}

/// The RDF syntax a summary request asks for with `format=` or `Accept`; `None` is the
/// JSON document.
fn rdf_format(params: &Params, headers: &HeaderMap) -> ApiResult<Option<RdfFormat>> {
    if let Some(f) = params.get("format") {
        if f == "json" {
            return Ok(None);
        }
        return results::rdf_format_from_name(f).map(Some).ok_or_else(|| {
            bad(format!(
                "unknown format '{f}': json, turtle, ntriples, nquads, trig, rdfxml or jsonld"
            ))
        });
    }
    const OFFERS: [&str; 7] = [
        "application/json",
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
    Ok(match super::negotiate(accept, &OFFERS) {
        None | Some(0) => None,
        Some(i) => results::rdf_format_from_name(OFFERS[i]),
    })
}

async fn serve(st: St, name: String, uri: Uri, p: Principal, what: What) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let mut req = parse(&st, &ds, &uri, &p)?;
    // the constraints layer goes with the JSON summary only
    let shapes = match what {
        What::Summary => Some(constraints::shapes_param(&Params::from_query(&uri))?),
        _ => None,
    };
    // VoID is always complete (no cursor), and reports the distinct subjects and objects
    // of the selection as well
    if matches!(what, What::Void(..)) {
        req.report.options.term_totals = true;
        req.report.cursor = None;
    }
    blocking(move || {
        let outcome = ds
            .dataset
            .schema()
            .report(&req.report)
            .map_err(|e| schema_error(e, req.timeout))?;
        let r = &*outcome.report;
        let mut resp = match what {
            What::Void(format, declarations) => {
                let mut prefixes = sparkles::io::standard_prefixes();
                prefixes.extend(ds.store.prefixes());
                let opts = VoidOptions {
                    dataset: &ds.name,
                    declarations,
                    prefixes: prefixes.into_iter().collect(),
                };
                (
                    [(header::CONTENT_TYPE, results::rdf_media_type(format))],
                    schema::void_text(r, &opts, format),
                )
                    .into_response()
            }
            // the summary always starts both lists at the top
            What::Summary => {
                let layer = match &shapes {
                    Some(s) => ds.dataset.schema().constraints_at(
                        &outcome.snapshot,
                        s,
                        req.report.options.graphs.as_deref(),
                    )?,
                    None => None,
                };
                Json(outcome.summary(&ds.name).with_constraints(layer.as_ref())).into_response()
            }
            What::Classes => Json(outcome.classes()).into_response(),
            What::Predicates => Json(outcome.predicates()).into_response(),
        };
        if let Ok(v) = header::HeaderValue::from_str(&outcome.computed.to_string()) {
            resp.headers_mut().insert(COMPUTED_HEADER, v);
        }
        Ok(resp)
    })
    .await
}

pub(super) async fn summary(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let params = Params::from_query(&uri);
    let what = match rdf_format(&params, &headers)? {
        None => What::Summary,
        Some(f) => {
            let declarations = match params.get("declarations") {
                None | Some("true") => true,
                Some("false") => false,
                Some(v) => {
                    return Err(bad(format!(
                        "declarations must be true or false, not '{v}'"
                    )));
                }
            };
            What::Void(f, declarations)
        }
    };
    serve(st, name, uri, p, what).await
}

pub(super) async fn classes(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult<Response> {
    serve(st, name, uri, p, What::Classes).await
}

pub(super) async fn predicates(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult<Response> {
    serve(st, name, uri, p, What::Predicates).await
}

mod shapes;
pub(super) use shapes::shapes;

mod analysis;
pub(super) use analysis::{diff, profiles};

pub(crate) mod constraints;
pub(super) use constraints::constraints;

#[cfg(test)]
mod tests;
