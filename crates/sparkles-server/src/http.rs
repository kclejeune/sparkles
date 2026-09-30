//! HTTP layer: SPARQL 1.1 Protocol, Graph Store Protocol, Fuseki `/$/` admin API.

use crate::auth::{Level, Principal};
use crate::obs::{CancelOnDrop, Op, Outcome, RequestReport};
use crate::state::{AppState, Dataset, DbType, now, uptime_secs};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Multipart, Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use oxrdfio::{RdfFormat, RdfSerializer};
use serde_json::{Value as J, json};
use sparkles::BudgetKind;
use sparkles::index::Perm;
use sparkles::io::Source;
use sparkles::sparql::results::{self, LimitedWriter, SolutionsFormat};
use sparkles::sparql::{QueryKind, QueryOptions};
use sparkles::store::{Chunk, ReplaceTarget};
use sparkles::{Error, id::Id};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

mod schema;

pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

type St = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    let cors = crate::auth::cors_layer(
        &state,
        vec![
            header::HeaderName::from_static(SPARKLES_COMMIT),
            header::HeaderName::from_static(SPARKLES_DATASET_ID),
            crate::obs::X_REQUEST_ID.clone(),
            header::HeaderName::from_static(crate::reasoning::SPARKLES_INFERENCES),
        ],
    );
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/ui/") }))
        .route("/ui", get(|| async { Redirect::temporary("/ui/") }))
        .route("/ui/", get(crate::ui::serve_index))
        .route("/ui/{*path}", get(crate::ui::serve))
        .route("/$/ping", get(ping).post(ping))
        .merge(crate::auth::routes())
        .route("/$/server", get(server_info))
        .route("/$/metrics", get(crate::obs::metrics_endpoint))
        .route("/$/ready", get(crate::obs::ready_endpoint))
        .route("/$/ready/{ds}", get(crate::obs::ready_dataset))
        .route("/$/datasets", get(list_datasets).post(create_dataset))
        .route("/$/datasets/{ds}", get(get_dataset).delete(delete_dataset))
        .route("/$/datasets/{ds}/clone", post(clone_dataset))
        .route("/$/stats/{ds}", get(stats))
        .route("/$/schema/{ds}", get(schema::summary))
        .route("/$/schema/{ds}/classes", get(schema::classes))
        .route("/$/schema/{ds}/predicates", get(schema::predicates))
        .route("/$/compact/{ds}", post(compact))
        .route("/$/backup/{ds}", post(backup))
        .route(
            "/$/reason/{ds}",
            get(reason_status).post(reason).delete(unreason),
        )
        .route("/$/reason/{ds}/diagnostics", get(reason_diagnostics))
        .route("/$/tasks", get(list_tasks))
        .route("/$/tasks/{id}", get(get_task))
        .route("/$/prefixes/{ds}", get(prefixes))
        .route("/$/cache/clear/{ds}", post(clear_cache))
        .route(
            "/$/text/{ds}",
            get(text_status).put(text_enable).delete(text_disable),
        )
        .route("/$/text/{ds}/rebuild", post(text_rebuild))
        .route("/$/commits/{ds}", get(list_commits))
        .route("/$/commits/{ds}/{reference}", get(get_commit))
        .route("/{ds}", any(dataset_root))
        .route("/{ds}/sparql", any(query_endpoint))
        .route("/{ds}/query", any(query_endpoint))
        .route("/{ds}/update", post(update_endpoint))
        .route("/{ds}/data", any(gsp))
        .route("/{ds}/get", get(gsp).head(gsp))
        .route("/{ds}/upload", post(upload))
        .route("/{ds}/explain", get(explain).post(explain))
        .route("/{ds}/shacl", post(shacl))
        .layer(DefaultBodyLimit::max(8 << 30))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware,
        ))
        .layer(cors)
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(crate::obs::MakeSpan)
                .on_request(())
                // the access log is written by `obs::observe`
                .on_response(()),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::obs::observe,
        ))
        .layer(axum::middleware::from_fn(crate::alloc::track))
        .with_state(state)
}

// ------------------------------------------------------------------ errors ------

pub struct ApiError(StatusCode, J);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // engine errors with an outcome of their own have their own status codes (see
        // `From<Error>`): 408 timeout, 503 cancelled, 507 with a `budget` field
        let budget = self
            .1
            .get("budget")
            .and_then(J::as_str)
            .and_then(|b| BudgetKind::ALL.into_iter().find(|k| k.as_str() == b));
        let outcome = match self.0 {
            StatusCode::REQUEST_TIMEOUT => Some(Outcome::Timeout),
            StatusCode::SERVICE_UNAVAILABLE => Some(Outcome::Cancelled),
            StatusCode::INSUFFICIENT_STORAGE if budget.is_some() => Some(Outcome::Budget),
            _ => None,
        };
        RequestReport {
            outcome,
            budget,
            ..Default::default()
        }
        .attach((self.0, Json(self.1)).into_response())
    }
}

fn err(status: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(status, json!({ "error": msg.into() }))
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let msg = e.to_string();
        let status = match &e {
            Error::SparqlSyntax(_) | Error::Invalid(_) | Error::RdfParse(_) => {
                StatusCode::BAD_REQUEST
            }
            Error::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            Error::Timeout => StatusCode::REQUEST_TIMEOUT,
            Error::Cancelled => StatusCode::SERVICE_UNAVAILABLE,
            Error::BudgetExceeded(_) => StatusCode::INSUFFICIENT_STORAGE,
            Error::Poisoned | Error::TextUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Error::Service(_) => StatusCode::BAD_GATEWAY,
            Error::NotPermitted(_) => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = match e {
            Error::SparqlSyntax(_) => syntax_error_body(&msg),
            Error::BudgetExceeded(b) => json!({
                "error": msg,
                "budget": b.kind,
                "limit": b.limit,
                "requested": b.requested,
            }),
            _ => json!({ "error": msg }),
        };
        ApiError(status, body)
    }
}

/// `{error, detail?, line?, column?}` for a SPARQL parse error. Parser messages look like
/// "SPARQL syntax error: error at 3:14: expected one of …" and the list of expected
/// tokens can run to several hundred characters, so `error` gets a short summary and
/// the full text goes to `detail`.
fn syntax_error_body(msg: &str) -> J {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)error at (\d+):(\d+):\s*(.*)").unwrap()
    });
    const MAX: usize = 160;
    let Some(c) = RE.captures(msg) else {
        return json!({ "error": msg });
    };
    let (line, column) = (
        c[1].parse::<u64>().unwrap_or(0),
        c[2].parse::<u64>().unwrap_or(0),
    );
    let rest = c[3].trim();
    let mut summary = format!("SPARQL syntax error at line {line}, column {column}: {rest}");
    let mut body = json!({ "line": line, "column": column });
    if summary.chars().count() > MAX {
        summary = summary
            .chars()
            .take(MAX)
            .collect::<String>()
            .trim_end()
            .to_string()
            + "…";
        body["detail"] = msg.into();
    }
    body["error"] = summary.into();
    body
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast::<Error>() {
            Ok(e) => e.into(),
            Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        }
    }
}

type ApiResult<T = Response> = Result<T, ApiError>;

fn dataset(st: &AppState, name: &str) -> ApiResult<Arc<Dataset>> {
    st.get(name)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no such dataset: /{name}")))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

// ------------------------------------------------------------------ params ------

#[derive(Default, Debug)]
struct Params(Vec<(String, String)>);

impl Params {
    fn from_query(uri: &Uri) -> Params {
        Params(
            uri.query()
                .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
                .unwrap_or_default(),
        )
    }
    fn extend_form(&mut self, body: &[u8]) {
        self.0.extend(form_urlencoded::parse(body).into_owned());
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|(a, _)| a == k)
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(a, _)| a == k)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

fn content_type(h: &HeaderMap) -> String {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Pick the best offer for an Accept header (q-values, wildcards).
fn negotiate(accept: &str, offers: &[&str]) -> Option<usize> {
    let mut best: Option<(f32, usize, usize)> = None; // (q, specificity, offer)
    for part in accept.split(',') {
        let mut it = part.split(';');
        let mt = it.next().unwrap_or("").trim().to_ascii_lowercase();
        let mut q = 1.0f32;
        for p in it {
            if let Some(v) = p.trim().strip_prefix("q=") {
                q = v.parse().unwrap_or(1.0);
            }
        }
        if q <= 0.0 {
            continue;
        }
        for (i, o) in offers.iter().enumerate() {
            let spec = if mt == *o {
                2
            } else if mt == "*/*" || (mt.ends_with("/*") && o.starts_with(&mt[..mt.len() - 1])) {
                0
            } else {
                continue;
            };
            let cand = (q, spec, i);
            if best.is_none_or(|b| {
                (cand.0, cand.1) > (b.0, b.1) || ((cand.0, cand.1) == (b.0, b.1) && cand.2 < b.2)
            }) {
                best = Some(cand);
            }
        }
    }
    best.map(|b| b.2)
}

fn solutions_format(params: &Params, headers: &HeaderMap) -> SolutionsFormat {
    if let Some(f) = params.get("format").and_then(SolutionsFormat::from_name) {
        return f;
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    const OFFERS: [&str; 7] = [
        "application/sparql-results+json",
        "application/x-sparkles+json",
        "application/sparql-results+xml",
        "text/csv",
        "text/tab-separated-values",
        "application/json",
        "application/xml",
    ];
    match negotiate(accept, &OFFERS) {
        Some(1) => SolutionsFormat::Sparkles,
        Some(2) | Some(6) => SolutionsFormat::Xml,
        Some(3) => SolutionsFormat::Csv,
        Some(4) => SolutionsFormat::Tsv,
        _ => SolutionsFormat::Json,
    }
}

fn rdf_format(params: &Params, headers: &HeaderMap, quads: bool) -> RdfFormat {
    if let Some(f) = params.get("format").and_then(results::rdf_format_from_name) {
        return f;
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    let offers: &[&str] = if quads {
        &[
            "application/trig",
            "application/n-quads",
            "application/ld+json",
            "text/plain",
        ]
    } else {
        &[
            "text/turtle",
            "application/n-triples",
            "application/ld+json",
            "application/rdf+xml",
            "application/trig",
            "application/n-quads",
            "text/plain",
        ]
    };
    negotiate(accept, offers)
        .and_then(|i| results::rdf_format_from_name(offers[i]))
        .unwrap_or(if quads {
            RdfFormat::TriG
        } else {
            RdfFormat::Turtle
        })
}

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "true" | "1" | "yes" | "on"
    )
}

fn timeout_param(st: &AppState, params: &Params) -> Duration {
    params
        .get("timeout")
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|t| t.is_finite() && *t > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(st.default_timeout)
}

/// The `timeout` parameter of an update, else the server's update timeout (none by
/// default).
fn update_timeout(st: &AppState, params: &Params) -> Option<Duration> {
    params
        .get("timeout")
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|t| t.is_finite() && *t > 0.0)
        .map(Duration::from_secs_f64)
        .or(st.limits.update_timeout)
}

fn query_options(st: &AppState, ds: &Dataset, params: &Params) -> QueryOptions {
    let timeout = timeout_param(st, params);
    let reasoning =
        params.get("reasoning").is_none_or(|v| v != "false") && ds.reasoning.read().is_some();
    QueryOptions {
        timeout: Some(timeout),
        max_rows: Some(st.limits.max_rows),
        max_memory_bytes: st.limits.query_memory_bytes,
        no_cache: params.get("nocache").is_some_and(truthy),
        default_graph_uris: params.all("default-graph-uri"),
        named_graph_uris: params.all("named-graph-uri"),
        allow_service: st.allow_service,
        default_graph_extra: if reasoning {
            vec![INFERRED_GRAPH.to_string()]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

// ------------------------------------------------------------------ SPARQL ------

async fn dataset_root(
    st: St,
    Path(name): Path<String>,
    p: Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    if method == Method::POST && ct == "application/x-www-form-urlencoded" {
        params.extend_form(&body);
    }
    if params.has("query") || ct == "application/sparql-query" {
        return query_endpoint(st, Path(name), p, method, uri, headers, body).await;
    }
    if params.has("update") || ct == "application/sparql-update" {
        // SPARQL 1.1 Protocol: updates only by POST
        if method != Method::POST {
            return Err(err(
                StatusCode::METHOD_NOT_ALLOWED,
                "use POST for SPARQL Update",
            ));
        }
        // a form body is only seen here: the auth layer checked read
        if !p.can(&name, Level::Write) {
            dataset(&st, &name)?;
            let msg = format!("write access to /{name} required");
            return Ok(crate::auth::forbidden(&p, &msg));
        }
        return update_endpoint(st, Path(name), p, uri, headers, body).await;
    }
    gsp(st, Path(name), method, uri, headers, body).await
}

async fn query_endpoint(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    let query = match (method.clone(), ct.as_str()) {
        (Method::POST, "application/sparql-query") => String::from_utf8_lossy(&body).into_owned(),
        (Method::POST, "application/x-www-form-urlencoded") => {
            params.extend_form(&body);
            params.get("query").unwrap_or_default().to_string()
        }
        (Method::GET | Method::HEAD | Method::POST, _) => {
            params.get("query").unwrap_or_default().to_string()
        }
        _ => return Err(err(StatusCode::METHOD_NOT_ALLOWED, "use GET or POST")),
    };
    if query.trim().is_empty() {
        if params.has("update") {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "SPARQL Update must be sent to the update endpoint",
            ));
        }
        return Err(err(StatusCode::BAD_REQUEST, "missing 'query' parameter"));
    }
    crate::obs::log_query_text(&query);
    let mut opts = query_options(&st, &ds, &params);
    crate::auth::restrict(&mut opts, &p);
    // a client that disconnects drops this future: the flag stops the query at its
    // next check
    let cancel = Arc::new(AtomicBool::new(false));
    opts.cancel = Some(cancel.clone());
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    let limit = st.limits.max_result_bytes;
    let sfmt = solutions_format(&params, &headers);
    let rfmt = rdf_format(&params, &headers, false);
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    let prefixes = ds.store.prefixes();
    blocking(move || {
        let t = std::time::Instant::now();
        let snap = ds.store.snapshot();
        let seq = snap.commit;
        let r = sparkles::sparql::query(snap, &query, &opts)?;
        let is_graph = !matches!(r.kind, QueryKind::Select | QueryKind::Ask);
        let sparkles_doc =
            sfmt == SolutionsFormat::Sparkles && (!is_graph || params_wants_sparkles(&headers));
        if let Some(l) = limit
            && r.kind == QueryKind::Select
        {
            // refuse before serializing when even the smallest encoding is too large
            let rows = send.map_or(r.len(), |s| s.min(r.len()));
            let min = sfmt.min_bytes(rows, r.vars.len());
            if min > l {
                return Err(Error::BudgetExceeded(sparkles::Budget {
                    kind: BudgetKind::ResultBytes,
                    limit: l,
                    requested: min,
                })
                .into());
            }
        }
        let ts = std::time::Instant::now();
        let mut w = LimitedWriter::new(Vec::new(), limit, Some(cancel));
        let ct: String = match r.kind {
            _ if sparkles_doc => {
                // Build the document once, then patch the serialization time into it.
                let mut doc = results::sparkles_json(&r, send);
                let ser_ms = ts.elapsed().as_secs_f64() * 1000.0;
                if let Some(timing) = doc.pointer_mut("/meta/timing").and_then(J::as_object_mut) {
                    let total = timing.get("totalMs").and_then(J::as_f64).unwrap_or(0.0);
                    timing.insert("serializeMs".into(), ser_ms.into());
                    timing.insert("totalMs".into(), (total + ser_ms).into());
                }
                if let Some(meta) = doc.pointer_mut("/meta").and_then(J::as_object_mut) {
                    meta.insert("commit".into(), seq.into());
                    meta.insert("datasetId".into(), ds.store.dataset_id().to_string().into());
                }
                serde_json::to_writer(&mut w, &doc).map_err(|e| w.classify(Error::Io(e.into())))?;
                SolutionsFormat::Sparkles.media_type().into()
            }
            QueryKind::Select | QueryKind::Ask => {
                results::write_solutions(&r, sfmt, &mut w, send).map_err(|e| w.classify(e))?;
                sfmt.media_type().into()
            }
            _ => {
                results::write_graph(&r, rfmt, &prefixes, &mut w).map_err(|e| w.classify(e))?;
                results::rdf_media_type(rfmt).into()
            }
        };
        let serialize_ms = ts.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!("query executed in {:?} ({} results)", t.elapsed(), r.len());
        let buf = w.into_inner();
        let report = RequestReport {
            operation: Some(Op::Query),
            rows: Some(r.len() as u64),
            response_bytes: Some(buf.len() as u64),
            serialize_ms: Some(serialize_ms),
            mem_peak_bytes: Some(r.mem_peak_bytes),
            timing: Some(r.timing),
            ..Default::default()
        };
        let resp = with_commit(
            ([(header::CONTENT_TYPE, ct)], buf).into_response(),
            &ds,
            seq,
        );
        Ok(report.attach(with_inferences(
            resp,
            &ds,
            !opts.default_graph_extra.is_empty(),
            seq,
        )))
    })
    .await
}

fn params_wants_sparkles(h: &HeaderMap) -> bool {
    h.get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/x-sparkles+json"))
}

// ---------------------------------------------------------------- full-text ------

#[cfg(feature = "text")]
async fn text_status(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(match ds.store.text_status() {
        Some(s) => serde_json::to_value(s).unwrap(),
        None => json!({ "enabled": false }),
    }))
}

/// `PUT /$/text/{ds}`: enable (or reconfigure) full-text search; the body is the
/// configuration (empty: defaults). The index is built in a background task.
#[cfg(feature = "text")]
async fn text_enable(State(st): St, Path(name): Path<String>, body: Bytes) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let cfg: sparkles::text::TextConfig = if body.iter().all(u8::is_ascii_whitespace) {
        Default::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!("invalid text configuration: {e}"),
            )
        })?
    };
    let task = st.start_task("text-rebuild", &name, move |h| {
        h.progress(0.1, "building the full-text index");
        let s = ds.store.enable_text(cfg)?;
        Ok(format!("full-text index: {} documents", s.docs))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(feature = "text")]
async fn text_disable(State(st): St, Path(name): Path<String>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        ds.store.disable_text()?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

#[cfg(feature = "text")]
async fn text_rebuild(State(st): St, Path(name): Path<String>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    if !ds.store.text_enabled() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "full-text search is not enabled",
        ));
    }
    let running = st
        .tasks
        .lock()
        .iter()
        .any(|t| t.kind == "text-rebuild" && t.dataset == name && t.state == "running");
    if running {
        return Err(err(
            StatusCode::CONFLICT,
            "text index rebuild already running",
        ));
    }
    let task = st.start_task("text-rebuild", &name, move |h| {
        h.progress(0.1, "rebuilding the full-text index");
        let s = ds.store.rebuild_text()?;
        Ok(format!("full-text index: {} documents", s.docs))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(not(feature = "text"))]
async fn text_status() -> ApiResult<Json<J>> {
    Err(sparkles::text::not_built().into())
}
#[cfg(not(feature = "text"))]
async fn text_enable() -> ApiResult {
    Err(sparkles::text::not_built().into())
}
#[cfg(not(feature = "text"))]
async fn text_disable() -> ApiResult {
    Err(sparkles::text::not_built().into())
}
#[cfg(not(feature = "text"))]
async fn text_rebuild() -> ApiResult {
    Err(sparkles::text::not_built().into())
}

// ------------------------------------------------------------------ commits ------

/// Response header: the commit (`seq`) a response reflects.
const SPARKLES_COMMIT: &str = "sparkles-commit";
/// Response header: the dataset id that commit belongs to.
const SPARKLES_DATASET_ID: &str = "sparkles-dataset-id";

/// Add the commit headers to a response.
fn with_commit(mut r: Response, ds: &Dataset, seq: u64) -> Response {
    let h = r.headers_mut();
    h.insert(SPARKLES_COMMIT, seq.into());
    if let Ok(v) = header::HeaderValue::from_str(&ds.store.dataset_id().to_string()) {
        h.insert(SPARKLES_DATASET_ID, v);
    }
    r
}

/// The client asked for a commit receipt: `receipt=true`, or an `Accept` that names the
/// Sparkles media type (`*/*` does not count).
fn receipt_wanted(params: &Params, headers: &HeaderMap) -> bool {
    params.get("receipt").is_some_and(truthy) || params_wants_sparkles(headers)
}

/// A write response: `body` as JSON, plus the receipt members when asked for (as the
/// Sparkles media type), plus the commit headers.
fn write_response(
    ds: &Dataset,
    status: StatusCode,
    body: Option<J>,
    receipt: &sparkles::commit::Receipt,
    wanted: bool,
) -> Response {
    let r = if wanted {
        let mut doc = body.unwrap_or_else(|| json!({}));
        if let (Some(m), Ok(J::Object(rm))) = (doc.as_object_mut(), serde_json::to_value(receipt)) {
            m.insert("dataset".into(), ds.name.clone().into());
            m.extend(rm);
        }
        let status = if status == StatusCode::NO_CONTENT {
            StatusCode::OK
        } else {
            status
        };
        (
            status,
            [(header::CONTENT_TYPE, SolutionsFormat::Sparkles.media_type())],
            doc.to_string(),
        )
            .into_response()
    } else {
        match body {
            Some(b) => (status, Json(b)).into_response(),
            None => status.into_response(),
        }
    };
    with_commit(r, ds, receipt.commit.seq)
}

/// Parse `N`, `commit:N` or `head` (resolved by the caller).
fn parse_commit_ref(s: &str) -> Option<Option<u64>> {
    if s == "head" {
        return Some(None);
    }
    s.strip_prefix("commit:")
        .unwrap_or(s)
        .parse()
        .ok()
        .map(Some)
}

async fn list_commits(State(st): St, Path(name): Path<String>, uri: Uri) -> ApiResult<Json<J>> {
    use sparkles::commit::CommitRange;
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let num = |k: &str| -> ApiResult<Option<u64>> {
        params
            .get(k)
            .map(|v| {
                v.parse::<u64>().map_err(|_| {
                    err(
                        StatusCode::BAD_REQUEST,
                        format!("invalid commit range: {k}={v}"),
                    )
                })
            })
            .transpose()
    };
    let limit = num("limit")?.unwrap_or(50);
    if limit == 0 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "invalid commit range: limit=0",
        ));
    }
    let limit = limit.min(1000) as usize;
    let range = match (num("before")?, num("after")?) {
        (Some(_), Some(_)) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "invalid commit range: use either before or after",
            ));
        }
        (Some(b), None) => CommitRange::Before(b),
        (None, Some(a)) => CommitRange::After(a),
        (None, None) => CommitRange::Latest,
    };
    let head = ds.store.head_commit();
    let page = ds.store.commits(range, limit);
    let next = match (range, page.commits.last()) {
        (CommitRange::After(_), Some(last)) if last.seq < head.seq => Some(format!(
            "/$/commits/{name}?after={}&limit={limit}",
            last.seq
        )),
        (CommitRange::Latest | CommitRange::Before(_), Some(last))
            if last.seq > page.first_retained =>
        {
            Some(format!(
                "/$/commits/{name}?before={}&limit={limit}",
                last.seq
            ))
        }
        _ => None,
    };
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "head": head.seq,
        "firstRetained": page.first_retained,
        "complete": page.complete,
        "commits": page.commits,
        "next": next,
    })))
}

async fn get_commit(
    State(st): St,
    Path((name, reference)): Path<(String, String)>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let head = ds.store.head_commit();
    let seq = parse_commit_ref(&reference)
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("invalid commit reference '{reference}'"),
            )
        })?
        .unwrap_or(head.seq);
    if seq > head.seq {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!("no commit {seq} in dataset {name} (head is {})", head.seq),
        ));
    }
    let c = ds.store.commit(seq).ok_or_else(|| {
        err(
            StatusCode::GONE,
            format!("commit metadata before {} is no longer retained", seq + 1),
        )
    })?;
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "commit": c,
    })))
}

async fn update_endpoint(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    let update = match ct.as_str() {
        "application/sparql-update" => String::from_utf8_lossy(&body).into_owned(),
        "application/x-www-form-urlencoded" => {
            params.extend_form(&body);
            params.get("update").unwrap_or_default().to_string()
        }
        _ => params.get("update").unwrap_or_default().to_string(),
    };
    if update.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "missing 'update' parameter"));
    }
    crate::obs::log_query_text(&update);
    let mut opts = QueryOptions {
        allow_service: st.allow_service,
        timeout: update_timeout(&st, &params),
        max_rows: Some(st.limits.max_rows),
        max_memory_bytes: st.limits.query_memory_bytes,
        ..Default::default()
    };
    crate::auth::restrict(&mut opts, &p);
    let wanted = receipt_wanted(&params, &headers);
    blocking(move || {
        let stats = sparkles::sparql::update::update_as(
            &ds.store,
            &update,
            &opts,
            sparkles::commit::CommitKind::Update,
        )?;
        let receipt = stats.commit.expect("update receipts");
        let report = RequestReport {
            operation: Some(Op::Update),
            rows: Some(stats.inserted + stats.deleted),
            mem_peak_bytes: Some(stats.mem_peak_bytes),
            ..Default::default()
        };
        let body = serde_json::to_value(&stats).unwrap();
        Ok(report.attach(write_response(
            &ds,
            StatusCode::OK,
            Some(body),
            &receipt,
            wanted,
        )))
    })
    .await
}

async fn explain(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    let query = if method == Method::POST && ct == "application/sparql-query" {
        String::from_utf8_lossy(&body).into_owned()
    } else {
        if method == Method::POST {
            params.extend_form(&body);
        }
        params.get("query").unwrap_or_default().to_string()
    };
    let mut opts = query_options(&st, &ds, &params);
    crate::auth::restrict(&mut opts, &p);
    blocking(move || {
        let snap = ds.store.snapshot();
        let seq = snap.commit;
        let (algebra, plan) = sparkles::sparql::explain(snap, &query, &opts)?;
        Ok(with_commit(
            Json(json!({ "algebra": algebra, "plan": plan })).into_response(),
            &ds,
            seq,
        ))
    })
    .await
}

// ------------------------------------------------------ Graph Store Protocol ------

/// Report of a write: its operation and the quads it changed.
fn write_report(op: Op, quads: u64) -> RequestReport {
    RequestReport {
        operation: Some(op),
        rows: Some(quads),
        ..Default::default()
    }
}

enum Target {
    Default,
    Named(String),
    Dataset,
}

fn gsp_target(params: &Params) -> Target {
    if params.has("default") {
        Target::Default
    } else if let Some(g) = params.get("graph") {
        if g == "default" || g == sparkles::sparql::ctx::DEFAULT_GRAPH_IRI {
            Target::Default
        } else {
            Target::Named(g.to_string())
        }
    } else {
        Target::Dataset
    }
}

/// Size of the chunks of a streamed response body.
const STREAM_CHUNK: usize = 64 << 10;

/// A `Write` that hands [`STREAM_CHUNK`]-sized chunks to a streamed response body.
/// Writes fail once the client has gone away.
struct ChunkWriter {
    tx: tokio::sync::mpsc::Sender<std::io::Result<Bytes>>,
    buf: Vec<u8>,
    sent: u64,
}

impl ChunkWriter {
    fn send(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(STREAM_CHUNK));
        self.sent += chunk.len() as u64;
        self.tx
            .blocking_send(Ok(Bytes::from(chunk)))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client disconnected"))
    }
}

impl std::io::Write for ChunkWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= STREAM_CHUNK {
            self.send()?;
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Graph Store GET body: the quads of graph `g` (every graph when `None`) of one
/// snapshot, serialized on a blocking thread and streamed, so memory stays flat and no
/// result-size budget applies. An error after the first byte aborts the response (the
/// client sees a truncated transfer); a client disconnect stops the serialization.
fn stream_graph(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    snap: Arc<sparkles::store::Snapshot>,
    g: Option<Id>,
    fmt: RdfFormat,
) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        let _span = span.enter();
        let mut w = ChunkWriter {
            tx: tx.clone(),
            buf: Vec::with_capacity(STREAM_CHUNK),
            sent: 0,
        };
        let mut ser = RdfSerializer::from_format(fmt);
        if matches!(fmt, RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml) {
            for (p, ns) in ds.store.prefixes() {
                if let Ok(s) = ser.clone().with_prefix(p, ns) {
                    ser = s;
                }
            }
        }
        let mut quads = 0u64;
        let written = (|| -> sparkles::Result<()> {
            let mut s = ser.for_writer(&mut w);
            let prefix: Vec<u64> = g.map(|g| vec![g.0]).unwrap_or_default();
            let mut write = |k: &[u64; 4]| -> sparkles::Result<()> {
                if let Some(q) = snap.quad_to_terms(&Perm::Gspo.to_quad(k)) {
                    if g.is_some() {
                        s.serialize_triple(oxrdf::TripleRef::new(
                            &q.subject,
                            &q.predicate,
                            &q.object,
                        ))?;
                    } else {
                        s.serialize_quad(&q)?;
                    }
                    quads += 1;
                }
                Ok(())
            };
            // serialize while scanning: nothing but the current block is held
            snap.scan(Perm::Gspo, &prefix, |c| {
                match c {
                    Chunk::Block(b, start, end) => {
                        for i in start..end {
                            write(&b.key(i))?;
                        }
                    }
                    Chunk::Row(k) => write(&k)?,
                }
                Ok(true)
            })?;
            s.finish()?;
            Ok(())
        })()
        .and_then(|()| Ok(w.send()?));
        st.metrics
            .add_response_bytes(Some(&ds.name), Op::Gsp, w.sent);
        match written {
            Ok(()) => tracing::debug!(quads, bytes = w.sent, "graph store stream finished"),
            Err(_) if tx.is_closed() => {
                tracing::debug!(
                    quads,
                    bytes = w.sent,
                    "graph store stream: client disconnected"
                )
            }
            Err(e) => {
                tracing::warn!(quads, bytes = w.sent, "graph store stream aborted: {e}");
                let _ = tx.blocking_send(Err(std::io::Error::other(e.to_string())));
            }
        }
    });
    axum::body::Body::from_stream(ChunkStream(rx))
}

/// The receiving end of a [`ChunkWriter`] as a body stream. It keeps answering `None`
/// after the end, since the compression layer polls once more.
struct ChunkStream(tokio::sync::mpsc::Receiver<std::io::Result<Bytes>>);

impl futures_util::Stream for ChunkStream {
    type Item = std::io::Result<Bytes>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

async fn gsp(
    State(st): St,
    Path(name): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let target = gsp_target(&params);
    match method {
        Method::GET | Method::HEAD => {
            let quads = matches!(target, Target::Dataset);
            let fmt = rdf_format(&params, &headers, quads);
            let head = method == Method::HEAD;
            // resolve the graph (or 404) before the response starts
            let (snap, g) = blocking({
                let ds = ds.clone();
                move || {
                    let snap = ds.store.snapshot();
                    let g = match &target {
                        Target::Default => Some(Id::DEFAULT_GRAPH),
                        Target::Named(iri) => Some(
                            snap.lookup_iri(iri)
                                .filter(|g| snap.count(Perm::Gspo, &[g.0]).unwrap_or(0) > 0)
                                .ok_or_else(|| {
                                    err(StatusCode::NOT_FOUND, format!("no such graph: <{iri}>"))
                                })?,
                        ),
                        Target::Dataset => None,
                    };
                    Ok((snap, g))
                }
            })
            .await?;
            let seq = snap.commit;
            let ct = [(header::CONTENT_TYPE, results::rdf_media_type(fmt))];
            let resp = if head {
                ct.into_response()
            } else {
                (ct, stream_graph(st.clone(), ds.clone(), snap, g, fmt)).into_response()
            };
            let report = RequestReport {
                operation: Some(Op::Gsp),
                ..Default::default()
            };
            Ok(report.attach(with_commit(resp, &ds, seq)))
        }
        Method::PUT | Method::POST => {
            if st.read_only {
                return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
            }
            let ct = content_type(&headers);
            let format = sparkles::io::format_for_media_type(&ct)
                .or_else(|| params.get("format").and_then(results::rdf_format_from_name))
                .ok_or_else(|| {
                    err(
                        StatusCode::UNSUPPORTED_MEDIA_TYPE,
                        format!("unsupported content type '{ct}'"),
                    )
                })?;
            let replace = method == Method::PUT;
            let wanted = receipt_wanted(&params, &headers);
            blocking(move || {
                let graph = match &target {
                    Target::Named(iri) => Some(
                        oxrdf::NamedNode::new(iri.clone())
                            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?,
                    ),
                    _ => None,
                };
                let src = Source::from_bytes(body.to_vec(), format, graph.clone());
                use sparkles::commit::CommitKind;
                let (count, receipt) = if replace {
                    // parse first, then clear and insert atomically
                    let t = match graph {
                        Some(g) => ReplaceTarget::Named(g),
                        None if matches!(target, Target::Dataset) => ReplaceTarget::All,
                        None => ReplaceTarget::Default,
                    };
                    ds.store.replace_as(t, &[src], CommitKind::GspPut)?
                } else {
                    let r = ds.store.load_as(&[src], CommitKind::GspPost)?;
                    (if r.committed { r.commit.inserted } else { 0 }, r)
                };
                let status = if replace {
                    StatusCode::OK
                } else {
                    StatusCode::CREATED
                };
                let body = json!({ "count": count, "tripleCount": count, "quadCount": count });
                Ok(write_report(Op::Gsp, count).attach(write_response(
                    &ds,
                    status,
                    Some(body),
                    &receipt,
                    wanted,
                )))
            })
            .await
        }
        Method::DELETE => {
            if st.read_only {
                return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
            }
            let wanted = receipt_wanted(&params, &headers);
            blocking(move || {
                let snap = ds.store.snapshot();
                let clear = match &target {
                    Target::Default => "CLEAR DEFAULT".to_string(),
                    Target::Named(iri) => {
                        let exists = snap
                            .lookup_iri(iri)
                            .is_some_and(|g| snap.count(Perm::Gspo, &[g.0]).unwrap_or(0) > 0);
                        if !exists {
                            return Err(err(
                                StatusCode::NOT_FOUND,
                                format!("no such graph: <{iri}>"),
                            ));
                        }
                        format!("CLEAR GRAPH <{iri}>")
                    }
                    Target::Dataset => "CLEAR ALL".to_string(),
                };
                let stats = sparkles::sparql::update::update_as(
                    &ds.store,
                    &clear,
                    &QueryOptions::default(),
                    sparkles::commit::CommitKind::GspDelete,
                )?;
                let receipt = stats.commit.expect("update receipts");
                Ok(write_report(Op::Gsp, stats.deleted).attach(write_response(
                    &ds,
                    StatusCode::NO_CONTENT,
                    None,
                    &receipt,
                    wanted,
                )))
            })
            .await
        }
        _ => Err(err(StatusCode::METHOD_NOT_ALLOWED, "unsupported method")),
    }
}

async fn upload(
    State(st): St,
    Path(name): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    request: axum::extract::Request,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let wanted = receipt_wanted(&params, &headers);
    let ct = content_type(&headers);
    let tmp = tempfile::Builder::new()
        .prefix("sparkles-upload-")
        .tempdir()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut graph: Option<String> = params.get("graph").map(str::to_string);
    if ct == "multipart/form-data" {
        use axum::extract::FromRequest;
        let mut mp = Multipart::from_request(request, &())
            .await
            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
        while let Some(field) = mp
            .next_field()
            .await
            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?
        {
            match field.name() {
                Some("graph") => {
                    let g = field
                        .text()
                        .await
                        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
                    if !g.trim().is_empty() {
                        graph = Some(g.trim().to_string());
                    }
                }
                _ => {
                    let fname = field.file_name().unwrap_or("upload.ttl").to_string();
                    let fname = std::path::Path::new(&fname)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "upload.ttl".into());
                    let data = field
                        .bytes()
                        .await
                        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
                    let path = tmp.path().join(format!("{}-{fname}", files.len()));
                    std::fs::write(&path, &data)
                        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                    files.push(path);
                }
            }
        }
    } else {
        // plain body: format from content type
        let format = sparkles::io::format_for_media_type(&ct).ok_or_else(|| {
            err(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("unsupported content type '{ct}'"),
            )
        })?;
        let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
        let ext = match format {
            RdfFormat::NTriples => "nt",
            RdfFormat::NQuads => "nq",
            RdfFormat::TriG => "trig",
            RdfFormat::RdfXml => "rdf",
            RdfFormat::JsonLd { .. } => "jsonld",
            _ => "ttl",
        };
        let path = tmp.path().join(format!("body.{ext}"));
        std::fs::write(&path, &bytes)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        files.push(path);
    }
    if files.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "no files in upload"));
    }
    blocking(move || {
        let g = match graph {
            Some(g) => Some(
                oxrdf::NamedNode::new(g)
                    .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?,
            ),
            None => None,
        };
        let sources = files
            .iter()
            .map(|p| Source::from_path(p, g.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let receipt = ds
            .store
            .load_as(&sources, sparkles::commit::CommitKind::Upload)?;
        let count = if receipt.committed {
            receipt.commit.inserted
        } else {
            0
        };
        drop(tmp);
        let body = json!({ "count": count, "tripleCount": count, "quadCount": count });
        Ok(write_report(Op::Upload, count).attach(write_response(
            &ds,
            StatusCode::OK,
            Some(body),
            &receipt,
            wanted,
        )))
    })
    .await
}

// ------------------------------------------------------------------- admin ------

async fn ping() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain")], now())
}

/// `{state, docs}` of the dataset's full-text index, or null.
fn text_summary(ds: &Dataset) -> J {
    #[cfg(feature = "text")]
    if let Some(s) = ds.store.text_status() {
        return json!({ "state": s.state, "docs": s.docs });
    }
    let _ = ds;
    J::Null
}

fn dataset_info(ds: &Dataset) -> J {
    let n = &ds.name;
    #[allow(unused_mut)]
    let mut endpoints = json!({
        "query": format!("/{n}/sparql"),
        "update": format!("/{n}/update"),
        "gsp": format!("/{n}/data"),
        "upload": format!("/{n}/upload"),
    });
    #[cfg(feature = "shacl")]
    {
        endpoints["shacl"] = format!("/{n}/shacl").into();
    }
    let head = ds.store.head_commit();
    let mut info = json!({
        "name": n,
        "type": ds.kind,
        "endpoints": endpoints,
        "quads": ds.store.snapshot().len(),
        "reasoning": crate::reasoning::info_json(ds),
        "id": ds.store.dataset_id(),
        "head": head.seq,
        "modified": head.timestamp(),
        "text": text_summary(ds),
    });
    // a clone: where it was forked from
    if let Some(f) = ds.store.forked_from() {
        info["forkedFrom"] = json!(f);
    }
    if let Some(o) = ds.store.root().and_then(crate::clone::read_origin) {
        info["origin"] = o;
    }
    info
}

/// The `DatasetInfo` of every dataset the caller may read, with its `access` level
/// when auth is enabled.
fn visible_datasets(st: &AppState, p: &Principal) -> Vec<J> {
    let datasets: Vec<Arc<Dataset>> = st.datasets.read().values().cloned().collect();
    datasets
        .iter()
        .filter(|d| p.can(&d.name, Level::Read))
        .map(|d| dataset_info_for(d, p))
        .collect()
}

fn dataset_info_for(d: &Dataset, p: &Principal) -> J {
    let mut info = dataset_info(d);
    if let Some(a) = p.access(&d.name) {
        info["access"] = a.as_str().into();
    }
    info
}

async fn server_info(State(st): St, Extension(p): Extension<Principal>) -> Json<J> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "startedAt": st.started_at,
        "uptimeSeconds": uptime_secs(&st),
        "readOnly": st.read_only,
        "datasets": visible_datasets(&st, &p),
        "limits": st.limits.json(st.default_timeout),
        "auth": crate::auth::server_json(&st),
    }))
}

async fn list_datasets(State(st): St, Extension(p): Extension<Principal>) -> Json<J> {
    Json(json!({ "datasets": visible_datasets(&st, &p) }))
}

async fn get_dataset(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(dataset_info_for(&ds, &p)))
}

async fn create_dataset(State(st): St, uri: Uri, headers: HeaderMap, body: Bytes) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let mut params = Params::from_query(&uri);
    let (name, kind) = if content_type(&headers) == "application/json" {
        let v: J = serde_json::from_slice(&body)
            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
        (
            v["dbName"].as_str().unwrap_or_default().to_string(),
            v["dbType"].as_str().unwrap_or("persistent").to_string(),
        )
    } else {
        params.extend_form(&body);
        (
            params.get("dbName").unwrap_or_default().to_string(),
            params.get("dbType").unwrap_or("persistent").to_string(),
        )
    };
    let name = name.trim_start_matches('/').to_string();
    let kind = match kind.as_str() {
        "mem" => DbType::Mem,
        "persistent" | "tdb" | "tdb2" => DbType::Persistent,
        other => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("unknown dbType '{other}'"),
            ));
        }
    };
    if !crate::state::valid_name(&name) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!(
                "invalid dataset name '{name}': use letters, digits, '_', '-' or '.' (max 64 characters)"
            ),
        ));
    }
    if st.get(&name).is_some() {
        return Err(err(
            StatusCode::CONFLICT,
            format!("dataset /{name} already exists"),
        ));
    }
    if let Some(t) = st.reserved_by(&name) {
        return Err(err(
            StatusCode::CONFLICT,
            format!("dataset /{name} is being created by task {t}"),
        ));
    }
    let st2 = st.clone();
    let ds = blocking(move || Ok(st2.create(&name, kind)?)).await?;
    Ok((StatusCode::CREATED, Json(dataset_info(&ds))).into_response())
}

/// `POST /$/datasets/{ds}/clone?name=NEW[&inferences=copy|drop]` (query, form or JSON
/// body): copy one snapshot of the dataset into a new persistent dataset, as a task.
async fn clone_dataset(
    State(st): St,
    Path(source): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let src = dataset(&st, &source)?;
    let mut params = Params::from_query(&uri);
    let (name, inferences, kind) =
        if content_type(&headers) == "application/json" && !body.is_empty() {
            let v: J = serde_json::from_slice(&body)
                .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
            let get = |k: &str| v[k].as_str().or_else(|| params.get(k)).map(str::to_string);
            (get("name"), get("inferences"), get("type"))
        } else {
            params.extend_form(&body);
            let get = |k: &str| params.get(k).map(str::to_string);
            (get("name"), get("inferences"), get("type"))
        };
    let name = name.unwrap_or_default().trim_start_matches('/').to_string();
    if !crate::state::valid_name(&name) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid dataset name '{name}'"),
        ));
    }
    // a clone may only create a dataset its caller could then manage
    if p.level(&name) != Some(Level::Admin) {
        let msg = format!("no admin access to the target name /{name}");
        return Ok(crate::auth::forbidden(&p, &msg));
    }
    if kind.as_deref().is_some_and(|k| k == "mem") {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "clone into an in-memory dataset is not supported yet",
        ));
    }
    let inferences = match inferences.as_deref() {
        None => crate::clone::Inferences::Copy,
        Some(i) => crate::clone::Inferences::parse(i).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("inferences must be copy or drop, not '{i}'"),
            )
        })?,
    };
    let id = st.next_task_id();
    let reservation = st
        .reserve(&name, &id)
        .map_err(|m| err(StatusCode::CONFLICT, m))?;
    let databases = st.data_dir.join("databases");
    let tmp = databases.join(format!(".clone-{name}-{id}"));
    let dst = databases.join(&name);
    let st2 = st.clone();
    let target = name.clone();
    let task = st.start_task_as(id, "clone", &source, Some(&name), move |h| {
        let h2 = h.clone();
        let progress: sparkles::store::ProgressFn =
            Arc::new(move |p, msg: &str| h2.progress(p * 0.95, msg));
        let reasoning = src.reasoning.read().clone();
        let rep = crate::clone::clone_into(
            &src.store,
            &src.name,
            reasoning,
            &tmp,
            &dst,
            inferences,
            Some(progress),
        )?;
        h.progress(0.97, "registering");
        st2.adopt(reservation)?;
        Ok(format!(
            "cloned /{} at commit {} ({} quads) into /{target}",
            src.name, rep.forked_from.seq, rep.quads
        ))
    });
    let location = format!("/$/datasets/{name}");
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(task),
    )
        .into_response())
}

async fn delete_dataset(State(st): St, Path(name): Path<String>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let st2 = st.clone();
    let n = name.clone();
    let deleted = blocking(move || Ok(st2.delete(&n)?)).await?;
    if deleted {
        Ok(StatusCode::OK.into_response())
    } else {
        Err(err(
            StatusCode::NOT_FOUND,
            format!("no such dataset: /{name}"),
        ))
    }
}

async fn stats(State(st): St, Path(name): Path<String>) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let reasoning = crate::reasoning::status_json(&st, &ds);
    blocking(move || {
        let snap = ds.store.snapshot();
        let gen_ = &snap.generation;
        let term = |id: u64| {
            snap.term(Id(id)).map(|t| match t {
                oxrdf::Term::NamedNode(n) => n.into_string(),
                t => t.to_string(),
            })
        };
        // graphs
        let mut graphs = Vec::new();
        for g in snap.distinct_first(Perm::Gspo)? {
            let n = snap.count(Perm::Gspo, &[g])?;
            let name = if g == Id::DEFAULT_GRAPH.0 {
                J::Null
            } else {
                term(g).map_or(J::Null, J::String)
            };
            graphs.push(json!({ "name": name, "quads": n }));
            if graphs.len() >= 1000 {
                break;
            }
        }
        // predicates: exact counts; distinct S/O from build statistics
        let mut preds: Vec<(u64, u64)> = Vec::new();
        for p in snap.distinct_first(Perm::Pso)? {
            preds.push((p, snap.count(Perm::Pso, &[p])?));
            if preds.len() >= 10_000 {
                break;
            }
        }
        preds.sort_by_key(|p| std::cmp::Reverse(p.1));
        let predicates: Vec<J> = preds
            .iter()
            .take(100)
            .map(|&(p, count)| {
                let ps = gen_.stats.predicate(p);
                json!({
                    "iri": term(p).unwrap_or_default(),
                    "count": count,
                    "distinctSubjects": ps.map_or(0, |s| s.distinct_subjects),
                    "distinctObjects": ps.map_or(0, |s| s.distinct_objects),
                })
            })
            .collect();
        // classes
        let mut classes: Vec<(u64, u64)> = if snap.delta.is_empty() {
            gen_.stats.classes.clone()
        } else {
            let mut m: std::collections::HashMap<u64, u64> = Default::default();
            if let Some(t) = snap.lookup_iri(oxrdf::vocab::rdf::TYPE.as_str()) {
                for k in snap.scan_keys(Perm::Pos, &[t.0])? {
                    *m.entry(k[1]).or_default() += 1;
                }
            }
            m.into_iter().collect()
        };
        classes.sort_by_key(|c| std::cmp::Reverse(c.1));
        let classes: Vec<J> = classes
            .iter()
            .take(100)
            .map(|&(c, n)| json!({ "iri": term(c).unwrap_or_default(), "instances": n }))
            .collect();
        let cache = ds.store.cache();
        let rcache = ds.store.result_cache();
        Ok(Json(json!({
            "name": ds.name,
            "quads": snap.len(),
            "baseQuads": gen_.meta.quads,
            "deltaInserts": snap.delta.inserts(),
            "deltaDeletes": snap.delta.deletes(),
            "terms": gen_.vocab.len() + gen_.dvocab.len(),
            "generation": gen_.name,
            "graphs": graphs,
            "predicates": predicates,
            "classes": classes,
            "diskBytes": ds.store.disk_bytes(),
            "reasoning": reasoning,
            "cache": {
                "entries": cache.entries(),
                "bytes": cache.bytes(),
                "hits": cache.hits(),
                "misses": cache.misses(),
            },
            "resultCache": {
                "enabled": rcache.enabled(),
                "entries": rcache.entries(),
                "bytes": rcache.bytes(),
                "hits": rcache.hits(),
                "misses": rcache.misses(),
            },
        }))
        .into_response())
    })
    .await
}

/// `POST /$/cache/clear/{ds}` (extension): drop the dataset's cached query results.
async fn clear_cache(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let c = ds.store.result_cache();
    let (entries, bytes) = (c.entries(), c.bytes());
    c.clear();
    Ok(Json(json!({ "cleared": entries, "bytes": bytes })))
}

async fn prefixes(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let mut p = sparkles::io::standard_prefixes();
    p.extend(ds.store.prefixes());
    Ok(Json(json!({ "prefixes": p })))
}

async fn compact(State(st): St, Path(name): Path<String>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let task = st.start_task("compact", &name, move |h| {
        h.progress(0.1, "rebuilding index");
        ds.store.compact()?;
        Ok(format!(
            "compacted to {}",
            ds.store.snapshot().generation.name
        ))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

async fn backup(State(st): St, Path(name): Path<String>) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let dir = st.data_dir.join("backups");
    let task = st.start_task("backup", &name, move |h| {
        h.progress(0.1, "writing N-Quads");
        let p = ds.store.backup(&dir, &ds.name)?;
        Ok(format!("backup written to {}", p.display()))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(feature = "reasoning")]
async fn reason(
    State(st): St,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let query = Params::from_query(&uri);
    let (profile_name, rules, rerun) =
        if content_type(&headers) == "application/json" && !body.is_empty() {
            let v: J = serde_json::from_slice(&body)
                .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
            (
                v["profile"].as_str().unwrap_or("rdfs").to_string(),
                v["rules"].as_str().map(str::to_string),
                v["rerun"].as_bool().unwrap_or(false),
            )
        } else {
            let mut p = Params::default();
            p.extend_form(&body);
            (
                p.get("profile").unwrap_or("rdfs").to_string(),
                p.get("rules").map(str::to_string),
                p.get("rerun").is_some_and(truthy),
            )
        };
    let profile: sparkles_reasoner::Profile = if rerun || query.get("rerun").is_some_and(truthy) {
        // the recorded profile, including its custom rules
        let info = ds
            .reasoning
            .read()
            .clone()
            .ok_or_else(|| err(StatusCode::CONFLICT, "no recorded reasoning to re-run"))?;
        crate::reasoning::recorded_profile(&info)
            .map_err(|e| err(StatusCode::CONFLICT, format!("{e:#}")))?
    } else if profile_name == "rules" {
        sparkles_reasoner::Profile::Rules(rules.unwrap_or_default())
    } else {
        profile_name.parse().map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unknown profile '{profile_name}'"),
            )
        })?
    };
    let task = crate::reasoning::start_reason(&st, ds, profile, false);
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(feature = "reasoning")]
async fn unreason(State(st): St, Path(name): Path<String>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let st2 = st.clone();
    blocking(move || {
        let n = sparkles_reasoner::clear(&ds.store)?;
        ds.set_reasoning(None)?;
        st2.save_registry()?;
        Ok(Json(json!({ "removed": n })).into_response())
    })
    .await
}

#[cfg(not(feature = "reasoning"))]
async fn reason() -> ApiResult {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "built without the `reasoning` feature",
    ))
}

#[cfg(not(feature = "reasoning"))]
async fn unreason() -> ApiResult {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "built without the `reasoning` feature",
    ))
}

/// `GET /$/reason/{ds}`: the reasoning status, with the freshness of the inferences.
async fn reason_status(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(match crate::reasoning::status_json(&st, &ds) {
        J::Null => json!({ "reasoning": null, "head": ds.store.head_commit().seq }),
        s => s,
    }))
}

/// `GET /$/reason/{ds}/diagnostics`: inconsistency checks over data (and inferences).
#[cfg(feature = "reasoning")]
async fn reason_diagnostics(State(st): St, Path(name): Path<String>, uri: Uri) -> ApiResult {
    use sparkles_reasoner::diagnostics::{self, Closure, DiagnoseOptions};
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let checks: Vec<String> = params
        .get("checks")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect();
    if let Err(bad) = diagnostics::select_checks(&checks) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("unknown diagnostics check '{bad}'"),
        ));
    }
    let limit = match params.get("limit") {
        None => 100,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|l| (1..=diagnostics::MAX_LIMIT).contains(l))
            .ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("limit must be between 1 and {}", diagnostics::MAX_LIMIT),
                )
            })?,
    };
    let closure = match params.get("closure") {
        None => Closure::Subclass,
        Some(c) => Closure::parse(c).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unknown closure '{c}' (expected subclass or none)"),
            )
        })?,
    };
    let info = ds.reasoning.read().clone();
    let inferences = info.is_some() && params.get("reasoning").is_none_or(|v| v != "false");
    let mut prefixes: Vec<(String, String)> = ds.store.prefixes().into_iter().collect();
    prefixes.retain(|(_, ns)| !ns.is_empty());
    let opts = DiagnoseOptions {
        checks,
        limit,
        inferences,
        closure,
        timeout: Some(timeout_param(&st, &params)),
        prefixes,
    };
    blocking(move || {
        let (_, j) = crate::reasoning::diagnostics_json(&ds.name, &ds.store, info.as_ref(), &opts)?;
        Ok(with_commit(
            Json(j.clone()).into_response(),
            &ds,
            j["commit"].as_u64().unwrap_or(0),
        ))
    })
    .await
}

#[cfg(not(feature = "reasoning"))]
async fn reason_diagnostics() -> ApiResult {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "built without the `reasoning` feature",
    ))
}

/// Add `Sparkles-Inferences` to a read of commit `seq` that `included` the inferred
/// graph, when those inferences are not known to be fresh.
fn with_inferences(mut r: Response, ds: &Dataset, included: bool, seq: u64) -> Response {
    if included && let Some(v) = crate::reasoning::inferences_header(ds, seq) {
        r.headers_mut()
            .insert(crate::reasoning::SPARKLES_INFERENCES, v);
    }
    r
}

async fn list_tasks(State(st): St, Extension(p): Extension<Principal>) -> Json<J> {
    let tasks: Vec<_> = st
        .tasks
        .lock()
        .iter()
        .filter(|t| {
            p.can(&t.dataset, Level::Read)
                || t.target.as_deref().is_some_and(|x| p.can(x, Level::Read))
        })
        .cloned()
        .collect();
    Json(serde_json::to_value(tasks).unwrap())
}

async fn get_task(
    State(st): St,
    Path(id): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    st.tasks
        .lock()
        .iter()
        .find(|t| t.id == id && p.can(&t.dataset, Level::Read))
        .map(|t| Json(serde_json::to_value(t).unwrap()))
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such task"))
}

// ------------------------------------------------------------------- SHACL ------

/// Fuseki's SHACL service: `POST /{ds}/shacl?graph=default|union|<iri>` with the
/// shapes graph as the body; answers with the validation report.
#[cfg(feature = "shacl")]
async fn shacl(
    State(st): St,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    use crate::shacl::{DataGraph, ReportFormat};
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let ct = content_type(&headers);
    // Turtle unless the content type names another RDF syntax (curl's default
    // `application/x-www-form-urlencoded` included; `text/plain` too, as Turtle is a
    // superset of N-Triples)
    let format = match ct.as_str() {
        "text/plain" => RdfFormat::Turtle,
        ct => sparkles::io::format_for_media_type(ct).unwrap_or(RdfFormat::Turtle),
    };
    let graph = DataGraph::parse(params.get("graph").unwrap_or("default"))
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let rfmt = match params.get("format") {
        Some(f) => ReportFormat::from_name(f).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unknown report format '{f}'"),
            )
        })?,
        None => {
            let accept = headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("*/*");
            negotiate(accept, &ReportFormat::OFFERS)
                .and_then(|i| ReportFormat::from_name(ReportFormat::OFFERS[i]))
                .unwrap_or(ReportFormat::Rdf(RdfFormat::Turtle))
        }
    };
    let use_inferred = params.get("reasoning").is_none_or(|v| v != "false");
    let has_inferred = ds.reasoning.read().is_some();
    let timeout = timeout_param(&st, &params);
    blocking(move || {
        let text = std::str::from_utf8(&body)
            .map_err(|_| err(StatusCode::BAD_REQUEST, "shapes graph is not UTF-8"))?;
        let shapes = sparkles_shacl::Shapes::parse(text, format, None)
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
        let snap = ds.store.snapshot();
        if let DataGraph::Named(iri) = &graph
            && !crate::shacl::graph_exists(&snap, iri)
        {
            return Err(err(
                StatusCode::NOT_FOUND,
                format!("no such graph: <{iri}>"),
            ));
        }
        let inferred = has_inferred.then_some(INFERRED_GRAPH);
        let mut opts = crate::shacl::validate_options(&snap, &graph, inferred, use_inferred)?;
        opts.timeout = Some(timeout);
        let t = std::time::Instant::now();
        let report = sparkles_shacl::validate(&snap, &shapes, &opts).map_err(|e| {
            let msg = format!("{e:#}");
            match e.downcast::<Error>() {
                Ok(e) => ApiError::from(e),
                Err(_) if msg.contains("timed out") => err(StatusCode::REQUEST_TIMEOUT, msg),
                Err(_) => err(StatusCode::BAD_REQUEST, msg),
            }
        })?;
        tracing::debug!(
            "SHACL validation of /{} in {:?}: {} results",
            ds.name,
            t.elapsed(),
            report.results.len()
        );
        let buf = crate::shacl::write_report(&report, rfmt)?;
        let resp = ([(header::CONTENT_TYPE, rfmt.media_type())], buf).into_response();
        Ok(with_inferences(
            resp,
            &ds,
            has_inferred && use_inferred,
            snap.commit,
        ))
    })
    .await
}

#[cfg(not(feature = "shacl"))]
async fn shacl() -> ApiResult {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "built without the `shacl` feature",
    ))
}

#[cfg(test)]
mod obs_tests;
#[cfg(test)]
mod router_tests;

#[cfg(test)]
mod tests {
    use super::{negotiate, syntax_error_body};

    #[test]
    fn syntax_errors() {
        let b = syntax_error_body("SPARQL syntax error: error at 3:2: expected OPTIONAL");
        assert_eq!(b["line"], 3);
        assert_eq!(b["column"], 2);
        assert_eq!(
            b["error"],
            "SPARQL syntax error at line 3, column 2: expected OPTIONAL"
        );
        assert!(b.get("detail").is_none());

        let long = format!(
            "SPARQL syntax error: error at 1:41: expected one of {}",
            "\"x\", ".repeat(100)
        );
        let b = syntax_error_body(&long);
        assert_eq!(b["line"], 1);
        assert!(b["error"].as_str().unwrap().ends_with('…'));
        assert!(b["error"].as_str().unwrap().chars().count() <= 161);
        assert_eq!(b["detail"], long.as_str());

        assert_eq!(
            syntax_error_body("something else")["error"],
            "something else"
        );
    }

    #[test]
    fn content_negotiation() {
        let offers = [
            "application/sparql-results+json",
            "text/csv",
            "application/sparql-results+xml",
        ];
        assert_eq!(negotiate("text/csv", &offers), Some(1));
        assert_eq!(negotiate("*/*", &offers), Some(0));
        assert_eq!(
            negotiate("text/csv;q=0.5, application/sparql-results+xml", &offers),
            Some(2)
        );
        assert_eq!(negotiate("text/*", &offers), Some(1));
        assert_eq!(negotiate("image/png", &offers), None);
    }
}
