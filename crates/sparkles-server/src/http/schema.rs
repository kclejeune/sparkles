//! Schema discovery API (extension): `GET /$/schema/{ds}` (summary with the first page
//! of classes and predicates), `GET /$/schema/{ds}/classes` and
//! `GET /$/schema/{ds}/predicates` (cursor-paginated lists).
//!
//! Every page of a listing comes from one report computed at one snapshot. The cursor
//! carries that snapshot's identity, a hash of the selection parameters and the last IRI
//! served; the dataset keeps its last report so a listing can be finished after a write.
//!
//! The summary is also a VoID description in RDF (`Accept: text/turtle` and the other RDF
//! syntaxes, or `format=`), followed by the declarations unless `declarations=false`.

use super::{ApiError, ApiResult, INFERRED_GRAPH, Params, St, blocking, dataset, err};
use crate::auth::Principal;
use crate::state::{AppState, Dataset, SchemaCacheEntry};
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oxrdfio::RdfFormat;
use serde::{Deserialize, Serialize};
use sparkles::schema::{
    self, GraphSelection, HasIri, Page, SchemaError, SchemaOptions, SchemaReport, VoidOptions,
};
use sparkles::sparql::results;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;

/// Continuation token: base64url JSON.
#[derive(Serialize, Deserialize)]
struct Cursor {
    /// snapshot identity of the report
    v: u64,
    /// selection hash
    h: u64,
    /// last IRI of the previous page
    a: String,
}

impl Cursor {
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(s: &str) -> Option<Cursor> {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(s.trim()).ok()?).ok()
    }
}

/// Parsed request parameters.
struct Request {
    opts: SchemaOptions,
    /// hash of the parameters that select the report (not `limit` / `cursor`)
    selection: u64,
    limit: usize,
    cursor: Option<Cursor>,
    timeout: Duration,
    /// a past state (`at`)
    at: Option<sparkles::history::At>,
}

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// FNV-1a: stable across restarts and builds.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
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
        None => DEFAULT_LIMIT,
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
        .map(|c| Cursor::decode(c).ok_or_else(|| bad("malformed cursor")))
        .transpose()?;
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
    let declared_name = declared_graph.as_ref().unwrap_or(&graph).name().to_string();
    // a caller limited to some graphs gets a report of those only
    let graphs = p.view(&ds.name, crate::auth::Endpoint::Info);
    let view = graphs.as_ref().map_or(String::new(), |g| g.read_key());
    let mut selection = format!(
        "{}\n{declared_name}\n{reasoning}\n{declared_all}\n{view}",
        graph.name()
    );
    if subject_classes {
        selection.push_str("\nsubjectClasses");
    }
    let selection = fnv(&selection);
    Ok(Request {
        opts: SchemaOptions {
            graph,
            declared_graph,
            inferred_graph: Some(INFERRED_GRAPH.to_string()),
            include_inferred: reasoning,
            declared_from_inferred: declared_all,
            deadline: None,
            cancel: None,
            max_entries: st.schema_max_entries,
            term_totals: false,
            graphs,
            subject_classes,
        },
        selection,
        limit,
        cursor,
        timeout,
        at,
    })
}

fn schema_error(e: SchemaError, timeout: Duration) -> ApiError {
    match e {
        SchemaError::NoSuchGraph(_) => err(StatusCode::NOT_FOUND, e.to_string()),
        SchemaError::Timeout { phase } => err(
            StatusCode::REQUEST_TIMEOUT,
            format!(
                "schema discovery exceeded {}s while {phase}; narrow graph= or raise timeout=",
                timeout.as_secs_f64()
            ),
        ),
        SchemaError::Cancelled => err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        SchemaError::TooManyEntries { .. } => err(StatusCode::PAYLOAD_TOO_LARGE, e.to_string()),
        SchemaError::Store(e) => e.into(),
    }
}

/// How the report a request was answered from came about (the `Sparkles-Schema-Report`
/// header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Computed {
    /// the dataset's cached report
    Cached,
    /// computed from the indexes
    Full,
    /// the cached report of an earlier commit, brought up to date from this many changes
    Updated(usize),
}

impl Computed {
    pub(crate) fn header(self) -> String {
        match self {
            Computed::Cached => "cached".into(),
            Computed::Full => "full".into(),
            Computed::Updated(n) => format!("updated; changes={n}"),
        }
    }
}

/// The header that says how a report came about.
pub(crate) const COMPUTED_HEADER: &str = "sparkles-schema-report";

/// The dataset's cached report brought up to date at `snap`, when the cache holds a
/// report of the same selection at an earlier commit, the changes since then are in the
/// write-ahead logs and number at most [`schema::max_changes`], and the request is one
/// a report can be maintained for. `None` otherwise: the caller computes a new report.
pub(crate) fn maintained(
    ds: &Dataset,
    snap: &Arc<sparkles::store::Snapshot>,
    selection: u64,
    opts: &SchemaOptions,
) -> Result<Option<(SchemaReport, usize)>, SchemaError> {
    let (old, mark) = match ds.schema_cache.lock().as_ref() {
        Some(e) if e.selection == selection => (e.report.clone(), e.mark.clone()),
        _ => return Ok(None),
    };
    if old.maintenance.is_none() || old.snapshot.commit > snap.commit {
        return Ok(None);
    }
    let o = sparkles::store::DiffOptions {
        max_quads: schema::max_changes(&old),
        deadline: opts.deadline,
        cancel: opts.cancel.clone(),
        ..Default::default()
    };
    // a persistent dataset reads the changes from its write-ahead logs; an in-memory one
    // compares the deltas of two states of one generation
    let diff = match &mark {
        Some(m) => ds.store.diff_since(m, snap, &o).ok().flatten(),
        None => {
            let at = sparkles::history::At::Commit;
            ds.store
                .diff(&at(old.snapshot.commit), &at(snap.commit), &o)
                .ok()
                // a state comparison reads the whole store: a new report costs as much
                .filter(|d| d.method != sparkles::store::DiffMethod::Compare)
        }
    };
    // history that is gone, or more changes than an update is worth, is a new report
    let Some(diff) = diff else {
        return Ok(None);
    };
    Ok(schema::update(&old, snap, &diff, opts)?.map(|r| (r, diff.len())))
}

/// The report the request is answered from: the cached one when it matches the cursor
/// or the current snapshot, the cached one brought up to date when it can be, otherwise
/// a fresh one (which replaces the cache).
fn report(
    ds: &Dataset,
    req: &mut Request,
) -> ApiResult<(Arc<SchemaReport>, Arc<sparkles::store::Snapshot>, Computed)> {
    let (snap, current) = match &req.at {
        None => {
            let snap = ds.store.snapshot();
            let id = schema::snapshot_identity(&snap);
            (snap, id)
        }
        Some(at) => {
            let o = sparkles::history::HistoryOptions {
                cancel: None,
                deadline: Some(Instant::now() + req.timeout),
            };
            let (snap, r) = ds.store.snapshot_at(at, &o)?;
            // a past state never changes: its identity is its commit (live versions are
            // small counters, so the high bit keeps the two apart)
            let id = if r.historical {
                (1 << 63) | r.commit.seq
            } else {
                schema::snapshot_identity(&snap)
            };
            (snap, id)
        }
    };
    let wanted = match &req.cursor {
        Some(c) if c.h != req.selection => {
            return Err(bad(
                "the cursor belongs to different graph/declaredGraph/reasoning/declared/detail parameters",
            ));
        }
        Some(c) => c.v,
        None => current,
    };
    if let Some(e) = ds.schema_cache.lock().as_ref()
        && e.identity == wanted
        && e.selection == req.selection
        && (e.report.term_totals.is_some() || !req.opts.term_totals)
    {
        return Ok((e.report.clone(), snap, Computed::Cached));
    }
    if wanted != current {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "snapshot changed since the first page (version {wanted} → {current}); restart from the first page"
            ),
        ));
    }
    req.opts.deadline = Some(Instant::now() + req.timeout);
    let started = Instant::now();
    let updated = if req.at.is_none() && req.opts.graphs.is_none() {
        maintained(ds, &snap, req.selection, &req.opts).map_err(|e| schema_error(e, req.timeout))?
    } else {
        None
    };
    let (report, how) = match updated {
        Some((r, n)) => (Arc::new(r), Computed::Updated(n)),
        None => (
            Arc::new(schema::discover(&snap, &req.opts).map_err(|e| schema_error(e, req.timeout))?),
            Computed::Full,
        ),
    };
    tracing::debug!(
        "schema of /{} at version {current} ({}) in {:?}: {} classes, {} predicates",
        ds.name,
        how.header(),
        started.elapsed(),
        report.classes.len(),
        report.predicates.len()
    );
    // the cache keeps the report everyone with access to every graph shares
    if req.opts.graphs.is_none() {
        *ds.schema_cache.lock() = Some(SchemaCacheEntry {
            identity: current,
            selection: req.selection,
            report: report.clone(),
            mark: (!ds.store.is_persistent()).then(|| snap.mark()),
        });
    }
    Ok((report, snap, how))
}

/// One page of `items` after the request's cursor.
fn page<'a, T: HasIri>(
    report: &SchemaReport,
    items: &'a [T],
    req: &Request,
    after_cursor: bool,
) -> Page<'a, T> {
    let after = req
        .cursor
        .as_ref()
        .filter(|_| after_cursor)
        .map(|c| c.a.as_str());
    let (items_page, more) = schema::page_after(items, after, req.limit);
    Page {
        items: items_page,
        total: items.len(),
        next: more.then(|| {
            Cursor {
                v: report.snapshot.version,
                h: req.selection,
                a: items_page.last().map_or_else(
                    || after.unwrap_or_default().to_string(),
                    |x| x.iri().to_string(),
                ),
            }
            .encode()
        }),
    }
}

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
        What::Summary => Some(constraints::ShapesRequest::parse(&Params::from_query(
            &uri,
        ))?),
        _ => None,
    };
    // VoID is always complete (no cursor), and reports the distinct subjects and objects
    // of the selection as well
    if matches!(what, What::Void(..)) {
        req.opts.term_totals = true;
        req.cursor = None;
    }
    blocking(move || {
        let (report, snap, how) = report(&ds, &mut req)?;
        let r = &*report;
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
                    Some(s) => constraints::layer(&ds, &snap, s, req.opts.graphs.as_deref())?,
                    None => None,
                };
                Json(
                    r.summary(
                        &ds.name,
                        page(r, &r.classes, &req, false),
                        page(r, &r.predicates, &req, false),
                    )
                    .with_constraints(layer.as_ref()),
                )
                .into_response()
            }
            What::Classes => Json(page(r, &r.classes, &req, true)).into_response(),
            What::Predicates => Json(page(r, &r.predicates, &req, true)).into_response(),
        };
        if let Ok(v) = header::HeaderValue::from_str(&how.header()) {
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

pub(crate) mod constraints;
pub(super) use constraints::constraints;

#[cfg(test)]
mod tests;
