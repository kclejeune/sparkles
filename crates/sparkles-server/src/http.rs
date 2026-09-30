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

mod history;
mod schema;
mod stream;
mod validation;

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
            header::HeaderName::from_static(history::SPARKLES_AT),
            header::HeaderName::from_static(validation::SPARKLES_VALIDATION),
            header::HeaderName::from_static(history::SPARKLES_HEAD),
            header::HeaderName::from_static("memento-datetime"),
            header::LINK,
            header::RETRY_AFTER,
            header::HeaderName::from_static("ratelimit"),
            header::HeaderName::from_static("ratelimit-policy"),
            header::HeaderName::from_static("traceresponse"),
        ],
    );
    let app = Router::new()
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
        .route(
            "/{ds}/upload",
            post(upload).layer(DefaultBodyLimit::max(
                state
                    .limits
                    .max_upload_bytes
                    .map_or(usize::MAX, |b| b as usize),
            )),
        )
        .route("/{ds}/explain", get(explain).post(explain))
        .route("/{ds}/shacl", post(shacl))
        .route("/$/vector/{ds}", get(vector_status))
        .route("/{ds}/prefixes", any(dataset_prefixes))
        .route(
            "/$/snapshots/{ds}",
            get(history::list_snapshots).post(history::create_snapshot),
        )
        .route(
            "/$/snapshots/{ds}/{name}",
            get(history::get_snapshot).delete(history::delete_snapshot),
        )
        .route(
            "/$/history/{ds}",
            get(history::get_history).put(history::put_history),
        )
        .route(
            "/$/validation/{ds}",
            get(validation::get_validation)
                .put(validation::put_validation)
                .delete(validation::delete_validation),
        )
        .layer(axum::middleware::from_fn(error_request_id))
        // for extractors without a ceiling of their own (see `limited_body!`)
        .layer(DefaultBodyLimit::max(
            state
                .limits
                .max_admin_body_bytes
                .map_or(usize::MAX, |b| b as usize),
        ))
        .layer(state.http_compression.layer())
        // compressed request bodies: marked (outermost), decompressed, then capped
        .layer(axum::middleware::from_fn_with_state(
            state.limits.max_decompressed_bytes,
            crate::compress::limit_decompressed,
        ))
        .layer(tower_http::decompression::RequestDecompressionLayer::new())
        .layer(axum::middleware::from_fn(crate::compress::mark_encoded));
    // inside `observe` (limited requests are logged and counted) and CORS (browsers
    // can read the 429); inside the auth layer, which puts the principal in the request
    let app = match &state.rate_limit {
        Some(rl) => app.layer(axum::middleware::from_fn_with_state(
            rl.clone(),
            crate::ratelimit::limit,
        )),
        None => app,
    };
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::auth::middleware,
    ))
    // outside the auth layer: an address that failed to authenticate too often is refused
    // before any credential is checked, and every failure is charged to it
    .layer(axum::middleware::from_fn_with_state(
        state.rate_limit.clone(),
        crate::ratelimit::admit,
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
        let mut body = self.1;
        // a rejection's header and Turtle report travel beside the JSON body
        let (vheader, turtle) = match body.as_object_mut() {
            Some(o) => (o.remove("_validationHeader"), o.remove("_turtle")),
            None => (None, None),
        };
        let mut resp = RequestReport {
            outcome,
            budget,
            ..Default::default()
        }
        .attach((self.0, Json(body.clone())).into_response());
        if let Some(h) = vheader.as_ref().and_then(J::as_str)
            && let Ok(v) = header::HeaderValue::from_str(h)
        {
            resp.headers_mut()
                .insert(validation::SPARKLES_VALIDATION, v);
        }
        if let Some(t) = turtle.as_ref().and_then(J::as_str) {
            resp.extensions_mut()
                .insert(validation::RejectionTurtle(t.to_string()));
        }
        resp.extensions_mut().insert(ErrorJson(body));
        resp
    }
}

/// The JSON body of an [`ApiError`] response, for [`error_request_id`].
#[derive(Clone)]
pub(crate) struct ErrorJson(pub(crate) J);

/// Add the request's id to JSON error bodies (`requestId`), so an error shown by a
/// client can be found in the logs. Runs inside the layer that assigns the id.
async fn error_request_id(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let id = req
        .headers()
        .get(&crate::obs::X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // `Accept: text/turtle` (named explicitly) turns a rejection into a Turtle report
    let turtle = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/turtle"));
    let mut resp = next.run(req).await;
    if let Some(validation::RejectionTurtle(t)) = resp.extensions_mut().remove()
        && turtle
    {
        resp.extensions_mut().remove::<ErrorJson>();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/turtle"),
        );
        resp.headers_mut()
            .insert(header::CONTENT_LENGTH, t.len().into());
        *resp.body_mut() = axum::body::Body::from(t);
        return resp;
    }
    add_request_id(&mut resp, id);
    resp
}

/// Put `requestId` into a response's [`ErrorJson`] body (also used by the auth layer,
/// which answers outside [`error_request_id`]).
pub(crate) fn add_request_id(resp: &mut Response, id: Option<String>) {
    if let (Some(ErrorJson(mut body)), Some(id)) = (resp.extensions_mut().remove(), id)
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("requestId".into(), id.into());
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        resp.headers_mut()
            .insert(header::CONTENT_LENGTH, bytes.len().into());
        *resp.body_mut() = axum::body::Body::from(bytes);
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
            Error::BudgetExceeded(b) if b.kind == BudgetKind::DecompressedBytes => {
                StatusCode::PAYLOAD_TOO_LARGE
            }
            Error::BudgetExceeded(_) => StatusCode::INSUFFICIENT_STORAGE,
            Error::Poisoned | Error::TextUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Error::Service(_) => StatusCode::BAD_GATEWAY,
            Error::NotPermitted(_) => StatusCode::FORBIDDEN,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::HistoryGone(_) => StatusCode::GONE,
            Error::HistoryUnsupported(_) => StatusCode::NOT_IMPLEMENTED,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Rejected(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::GuardMissing(_) => StatusCode::NOT_IMPLEMENTED,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = match e {
            Error::SparqlSyntax(_) => syntax_error_body(&msg),
            Error::HistoryGone(g) => history::gone_body(&g),
            Error::Rejected(r) => return validation::rejection(&r),
            Error::HistoryUnsupported(_) => json!({ "error": msg, "code": "history-unsupported" }),
            Error::Conflict(_) if msg.starts_with("history-limit") => {
                json!({ "error": msg, "code": "history-limit" })
            }
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

/// Run `f` on a blocking thread, inside the request's span (so engine events carry the
/// request id and engine spans are its children).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
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

/// A positive `timeout` parameter in seconds (others are ignored).
fn timeout_secs(params: &Params) -> Option<Duration> {
    params
        .get("timeout")
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|t| t.is_finite() && *t > 0.0)
        .and_then(|t| Duration::try_from_secs_f64(t).ok())
}

/// The `timeout` parameter of a query, capped at `--max-timeout`, else the server's
/// default.
fn timeout_param(st: &AppState, params: &Params) -> Duration {
    timeout_secs(params)
        .map(|t| st.limits.cap_timeout(t, Some(st.default_timeout)))
        .unwrap_or(st.default_timeout)
}

/// The `timeout` parameter of an update, capped at `--max-timeout`, else the server's
/// update timeout (none by default).
fn update_timeout(st: &AppState, params: &Params) -> Option<Duration> {
    timeout_secs(params)
        .map(|t| st.limits.cap_timeout(t, st.limits.update_timeout))
        .or(st.limits.update_timeout)
}

/// A `408` names the timeout that applied (`timeoutSeconds`).
fn with_timeout(mut e: ApiError, timeout: Option<Duration>) -> ApiError {
    if e.0 == StatusCode::REQUEST_TIMEOUT
        && let (Some(t), Some(o)) = (timeout, e.1.as_object_mut())
    {
        o.insert("timeoutSeconds".into(), t.as_secs_f64().into());
    }
    e
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
        outbound: st.outbound.clone(),
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
    body: axum::body::Body,
) -> ApiResult {
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    // the auth layer granted a form POST read access, since only its body tells a query
    // from an update: every other operation is re-checked here
    let form = method == Method::POST && ct == "application/x-www-form-urlencoded";
    let query = params.has("query") || ct == "application/sparql-query";
    let update = params.has("update") || ct == "application/sparql-update";
    if !(form || query || update) {
        // a Graph Store request: its body streams
        return gsp(st, Path(name), p, method, uri, headers, body).await;
    }
    let l = &st.limits;
    let (limit, flag) = if query {
        (l.max_query_body_bytes, "--max-query-body-mb")
    } else if update {
        (l.max_update_body_bytes, "--max-update-body-mb")
    } else {
        // a query or an update, as the body will tell
        match (l.max_query_body_bytes, l.max_update_body_bytes) {
            (Some(q), Some(u)) if q > u => (Some(q), "--max-query-body-mb"),
            (Some(_), Some(u)) => (Some(u), "--max-update-body-mb"),
            _ => (None, ""),
        }
    };
    let body = read_body(body, limit, flag).await?;
    if form {
        params.extend_form(&body);
    }
    if params.has("query") || ct == "application/sparql-query" {
        if form && (body.len() as u64) > l.max_query_body_bytes.unwrap_or(u64::MAX) {
            return Err(err(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds --max-query-body-mb",
            ));
        }
        return query_endpoint(st, Path(name), p, method, uri, headers, QueryBody(body)).await;
    }
    if params.has("update") || ct == "application/sparql-update" {
        // SPARQL 1.1 Protocol: updates only by POST
        if method != Method::POST {
            return Err(err(
                StatusCode::METHOD_NOT_ALLOWED,
                "use POST for SPARQL Update",
            ));
        }
        if let Some(denied) = crate::auth::dataset_denial(&st, &p, &headers, &name, Level::Write) {
            return Ok(denied);
        }
        return update_endpoint(st, Path(name), p, uri, headers, UpdateBody(body)).await;
    }
    // a form with neither a query nor an update: refused as the write any other POST body
    // would be, and never taken for an RDF payload
    if let Some(denied) = crate::auth::dataset_denial(&st, &p, &headers, &name, Level::Write) {
        return Ok(denied);
    }
    dataset(&st, &name)?;
    Err(err(
        StatusCode::BAD_REQUEST,
        "missing 'query' or 'update' parameter",
    ))
}

async fn query_endpoint(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    QueryBody(body): QueryBody,
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
    let with_extra = !opts.default_graph_extra.is_empty();
    let at = history::at_param(&params)?;
    let timeout = opts.timeout;
    let (r, seq, resolved, t0) = blocking({
        let ds = ds.clone();
        move || {
            let t = std::time::Instant::now();
            let t0 = crate::otel::start();
            let (snap, resolved) = history::snapshot_for(&ds, at.as_ref(), &opts)?;
            let seq = snap.commit;
            let r = sparkles::sparql::query(snap, &query, &opts)?;
            tracing::debug!("query executed in {:?} ({} results)", t.elapsed(), r.len());
            Ok((r, seq, resolved, t0))
        }
    })
    .await
    .map_err(|e| with_timeout(e, timeout))?;
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
    let ct: String = if sparkles_doc {
        SolutionsFormat::Sparkles.media_type().into()
    } else if is_graph {
        results::rdf_media_type(rfmt).into()
    } else {
        sfmt.media_type().into()
    };
    let report = RequestReport {
        operation: Some(Op::Query),
        rows: Some(r.len() as u64),
        mem_peak_bytes: Some(r.mem_peak_bytes),
        timing: Some(r.timing.clone()),
        ..Default::default()
    };
    let dataset_id = ds.store.dataset_id().to_string();
    let write = move |w: &mut LimitedWriter<stream::SwitchWriter>| -> sparkles::Result<()> {
        let ts = std::time::Instant::now();
        let written = serialize_result(
            &r,
            w,
            sparkles_doc,
            is_graph,
            sfmt,
            rfmt,
            send,
            &prefixes,
            seq,
            dataset_id,
        );
        // phase spans once serialization has ended (the request span is current here)
        crate::otel::query_done(t0, &r, ts.elapsed().as_secs_f64() * 1000.0);
        written
    };
    // weak: the serializer thread must not keep the server state (and its stores' locks)
    // alive after the response
    let (metrics_st, name) = (Arc::downgrade(&st), ds.name.clone());
    let body = stream::serialize(limit, write, move |end| {
        if let Some(st) = metrics_st.upgrade() {
            st.metrics
                .add_response_bytes(Some(&name), Op::Query, end.bytes);
        }
        stream_end_log("query", &end);
    })
    .await?;
    let (body, report) = match body {
        stream::Serialized::Whole { body, serialize_ms } => {
            let report = RequestReport {
                response_bytes: Some(body.len() as u64),
                serialize_ms: Some(serialize_ms),
                ..report
            };
            (axum::body::Body::from(body), report)
        }
        stream::Serialized::Streamed(body) => (body, report),
    };
    let resp = with_commit(
        ([(header::CONTENT_TYPE, ct)], body).into_response(),
        &ds,
        seq,
    );
    let resp = history::history_headers(resp, resolved.as_ref(), &uri);
    // freshness is reported for the live state only
    let resp = match &resolved {
        Some(r) if r.historical => resp,
        _ => with_inferences(resp, &ds, with_extra, seq),
    };
    Ok(report.attach(resp))
}

/// Serialize a query result: the Sparkles JSON document, an RDF graph or solutions.
#[allow(clippy::too_many_arguments)]
fn serialize_result(
    r: &sparkles::sparql::QueryResult,
    w: &mut LimitedWriter<stream::SwitchWriter>,
    sparkles_doc: bool,
    is_graph: bool,
    sfmt: SolutionsFormat,
    rfmt: RdfFormat,
    send: Option<usize>,
    prefixes: &std::collections::BTreeMap<String, String>,
    seq: u64,
    dataset_id: String,
) -> sparkles::Result<()> {
    if sparkles_doc {
        // Build the document once, then patch the serialization time into it.
        let ts = std::time::Instant::now();
        let mut doc = results::sparkles_json(r, send);
        let ser_ms = ts.elapsed().as_secs_f64() * 1000.0;
        if let Some(timing) = doc.pointer_mut("/meta/timing").and_then(J::as_object_mut) {
            let total = timing.get("totalMs").and_then(J::as_f64).unwrap_or(0.0);
            timing.insert("serializeMs".into(), ser_ms.into());
            timing.insert("totalMs".into(), (total + ser_ms).into());
        }
        if let Some(meta) = doc.pointer_mut("/meta").and_then(J::as_object_mut) {
            meta.insert("commit".into(), seq.into());
            meta.insert("datasetId".into(), dataset_id.into());
        }
        serde_json::to_writer(w, &doc).map_err(|e| Error::Io(e.into()))
    } else if is_graph {
        results::write_graph(r, rfmt, prefixes, w)
    } else {
        results::write_solutions(r, sfmt, w, send)
    }
}

/// Log how a streamed response ended (its bytes are counted in the metrics).
fn stream_end_log(what: &str, end: &stream::StreamEnd) {
    let (bytes, ms) = (end.bytes, end.serialize_ms);
    match &end.error {
        None => tracing::debug!(bytes, serialize_ms = ms, "{what} stream finished"),
        Some(_) if end.disconnected => {
            tracing::debug!(bytes, "{what} stream: client disconnected")
        }
        Some(e) => tracing::warn!(bytes, "{what} stream aborted: {e}"),
    }
}

fn params_wants_sparkles(h: &HeaderMap) -> bool {
    h.get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/x-sparkles+json"))
}

// ------------------------------------------------------------------- vectors ------

/// `GET /$/vector/{ds}`: the vector memory budget and the predicates whose vectors are
/// packed in the current generation (packing happens on a predicate's first search).
async fn vector_status(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let snap = ds.store.snapshot();
    let vectors = &snap.generation.vectors;
    let predicates: Vec<J> = vectors
        .status()
        .into_iter()
        .map(|p| {
            let iri = match snap.term(Id(p.predicate)) {
                Some(oxrdf::Term::NamedNode(n)) => n.into_string(),
                other => other.map(|t| t.to_string()).unwrap_or_default(),
            };
            json!({
                "predicate": iri,
                "bytes": p.bytes,
                "malformed": p.malformed,
                "dimensions": p.dims.iter().map(|(dim, rows)| json!({ "dimension": dim, "vectors": rows })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({
        "budgetBytes": sparkles::vector::budget(),
        "usedBytes": vectors.used_bytes(),
        "generation": snap.generation.name,
        "predicates": predicates,
    })))
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
async fn text_enable(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult {
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
    crate::otel::commit(receipt);
    let validation = receipt.validation.clone();
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
    validation::with_validation(
        with_commit(r, ds, receipt.commit.seq),
        validation.as_deref(),
    )
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
    let (oldest, reconstructable, commits) = history::commit_list_extras(&ds, &page.commits);
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "head": head.seq,
        "firstRetained": page.first_retained,
        "complete": page.complete,
        "oldestReconstructable": oldest,
        "reconstructable": reconstructable,
        "commits": commits,
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
    UpdateBody(body): UpdateBody,
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
    history::reject_at(&params)?;
    if update.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "missing 'update' parameter"));
    }
    crate::obs::log_query_text(&update);
    let mut opts = QueryOptions {
        allow_service: st.allow_service,
        outbound: st.outbound.clone(),
        timeout: update_timeout(&st, &params),
        max_rows: Some(st.limits.max_rows),
        max_memory_bytes: st.limits.query_memory_bytes,
        ..Default::default()
    };
    crate::auth::restrict(&mut opts, &p);
    opts.write = validation::write_options(&st, &params, &headers, opts.timeout)?;
    let wanted = receipt_wanted(&params, &headers);
    let timeout = opts.timeout;
    blocking(move || {
        let t0 = crate::otel::start();
        let stats = sparkles::sparql::update::update_as(
            &ds.store,
            &update,
            &opts,
            sparkles::commit::CommitKind::Update,
        )?;
        crate::otel::update_done(t0, &stats);
        let receipt = stats.commit.clone().expect("update receipts");
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
    .map_err(|e| with_timeout(e, timeout))
}

async fn explain(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    QueryBody(body): QueryBody,
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
    let at = history::at_param(&params)?;
    blocking(move || {
        let (snap, resolved) = history::snapshot_for(&ds, at.as_ref(), &opts)?;
        let seq = snap.commit;
        let (algebra, plan) = sparkles::sparql::explain(snap, &query, &opts)?;
        let resp = with_commit(
            Json(json!({ "algebra": algebra, "plan": plan })).into_response(),
            &ds,
            seq,
        );
        Ok(history::history_headers(resp, resolved.as_ref(), &uri))
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

/// Request bodies up to this size are kept in memory; larger ones go to a temp file.
const SPOOL_AFTER: usize = 16 << 20;

/// A request body: in memory, or spooled to a temporary file that lives as long as this.
enum Spooled {
    Memory(Vec<u8>),
    File(tempfile::NamedTempFile),
}

impl Spooled {
    /// The body as an RDF source, and the temporary file to keep until it is read.
    fn into_source(
        self,
        format: RdfFormat,
        graph: Option<oxrdf::NamedNode>,
        max_decompressed: Option<u64>,
    ) -> (Source, Option<tempfile::NamedTempFile>) {
        match self {
            Spooled::Memory(b) => {
                let mut s = Source::from_bytes(b, format, graph);
                s.max_decompressed = max_decompressed;
                (s, None)
            }
            Spooled::File(f) => (
                Source {
                    data: sparkles::io::SourceData::File(f.path().to_path_buf()),
                    format,
                    compression: None,
                    max_decompressed,
                    graph,
                    base: None,
                    name: "<request body>".into(),
                },
                Some(f),
            ),
        }
    }
}

/// A request body that could not be read: 413 past the decompressed-size cap, else 400.
fn body_error(e: axum::Error) -> ApiError {
    if crate::compress::is_length_limit(&e) {
        err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "decompressed request body exceeds --max-decompressed-mb",
        )
    } else {
        err(StatusCode::BAD_REQUEST, e.to_string())
    }
}

/// Read a whole request body of at most `limit` bytes (`flag` names the setting): `413`
/// before more than that is held. This runs inside the decompression layer, so the
/// limit counts decompressed bytes.
async fn read_body(body: axum::body::Body, limit: Option<u64>, flag: &str) -> ApiResult<Bytes> {
    use futures_util::StreamExt;
    use http_body::Body as _;
    let max = limit.unwrap_or(u64::MAX);
    let too_large = || {
        let size = sparkles::error::human_bytes(max);
        err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("request body exceeds {size} ({flag})"),
        )
    };
    // a declared length is refused before anything is read
    if body.size_hint().lower() > max {
        return Err(too_large());
    }
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(body_error)?;
        if (buf.len() + chunk.len()) as u64 > max {
            return Err(too_large());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.into())
}

/// Body extractors with the ceiling of their request class. Only the Graph Store and
/// upload endpoints stream bodies of any size.
macro_rules! limited_body {
    ($(#[$doc:meta])* $name:ident, $field:ident, $flag:literal) => {
        $(#[$doc])*
        pub(crate) struct $name(pub Bytes);

        impl axum::extract::FromRequest<Arc<AppState>> for $name {
            type Rejection = ApiError;

            async fn from_request(
                req: axum::extract::Request,
                st: &Arc<AppState>,
            ) -> Result<Self, ApiError> {
                read_body(req.into_body(), st.limits.$field, $flag)
                    .await
                    .map($name)
            }
        }
    };
}

limited_body!(
    /// A SPARQL query body (also `/{ds}/explain` and a `/{ds}/shacl` shapes graph).
    QueryBody,
    max_query_body_bytes,
    "--max-query-body-mb"
);
limited_body!(
    /// A SPARQL update body.
    UpdateBody,
    max_update_body_bytes,
    "--max-update-body-mb"
);
limited_body!(
    /// The body of an admin request (`/$/…`) or a prefix change.
    AdminBody,
    max_admin_body_bytes,
    "--max-admin-body-mb"
);

/// How often, in bytes written, a spooled body re-checks the free disk space.
const DISK_CHECK_EVERY: u64 = 64 << 20;

/// The ceilings of a streamed request body (Graph Store writes, uploads): its size
/// after decoding (`--max-upload-mb`), and the free space its temporary files leave on
/// disk (`--min-free-disk-mb`).
struct BodyBudget {
    max: Option<u64>,
    reserve: Option<u64>,
    read: u64,
    /// bytes written since the last free-space check (`None`: not checked yet)
    unchecked: Option<u64>,
}

impl BodyBudget {
    fn new(limits: &crate::state::Limits) -> BodyBudget {
        BodyBudget {
            max: limits.max_upload_bytes,
            reserve: limits.min_free_disk_bytes,
            read: 0,
            unchecked: None,
        }
    }

    /// Count `n` more bytes of the body: `413` past `--max-upload-mb`.
    fn read(&mut self, n: usize) -> ApiResult<()> {
        self.read += n as u64;
        match self.max {
            Some(max) if self.read > max => Err(err(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "request body exceeds {} (--max-upload-mb)",
                    sparkles::error::human_bytes(max)
                ),
            )),
            _ => Ok(()),
        }
    }

    /// Before writing `n` bytes to a file in `dir`: `507` when the free space there
    /// would come within [`DISK_CHECK_EVERY`] of the reserve. Checked on the first write
    /// and then every [`DISK_CHECK_EVERY`] bytes.
    fn disk(&mut self, dir: &std::path::Path, n: usize) -> ApiResult<()> {
        let Some(reserve) = self.reserve else {
            return Ok(());
        };
        let n = n as u64;
        if let Some(u) = self.unchecked
            && u + n < DISK_CHECK_EVERY
        {
            self.unchecked = Some(u + n);
            return Ok(());
        }
        let free = free_disk_bytes(dir)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if free < reserve.saturating_add(n).saturating_add(DISK_CHECK_EVERY) {
            let h = sparkles::error::human_bytes;
            return Err(err(
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "not enough free disk space to receive the request body ({} free, {} kept free by --min-free-disk-mb)",
                    h(free),
                    h(reserve)
                ),
            ));
        }
        self.unchecked = Some(n);
        Ok(())
    }
}

/// Free space for an unprivileged user on the file system of `dir`.
#[cfg(unix)]
fn free_disk_bytes(dir: &std::path::Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `s` is valid for writes; statvfs initializes
    // it when it returns 0
    if unsafe { libc::statvfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized by the successful call above
    let s = unsafe { s.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Ok((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

#[cfg(not(unix))]
fn free_disk_bytes(_: &std::path::Path) -> std::io::Result<u64> {
    Ok(u64::MAX)
}

/// Read a request body, spooling it to a temporary file once it passes
/// [`SPOOL_AFTER`] bytes, so a large upload is never held in memory whole. Large
/// sources then take the bulk path, which parses them as a stream.
async fn spool(body: axum::body::Body, budget: &mut BodyBudget) -> ApiResult<Spooled> {
    spool_after(body, SPOOL_AFTER, budget).await
}

async fn spool_after(
    body: axum::body::Body,
    limit: usize,
    budget: &mut BodyBudget,
) -> ApiResult<Spooled> {
    use futures_util::StreamExt;
    use std::io::Write;
    let io = |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    let tmp = std::env::temp_dir();
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    let mut file: Option<tempfile::NamedTempFile> = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(body_error)?;
        budget.read(chunk.len())?;
        match &mut file {
            Some(f) => {
                budget.disk(&tmp, chunk.len())?;
                f.write_all(&chunk).map_err(io)?
            }
            None => {
                buf.extend_from_slice(&chunk);
                if buf.len() > limit {
                    budget.disk(&tmp, buf.len())?;
                    let mut f = tempfile::Builder::new()
                        .prefix("sparkles-body-")
                        .tempfile_in(&tmp)
                        .map_err(io)?;
                    f.write_all(&buf).map_err(io)?;
                    buf = Vec::new();
                    file = Some(f);
                }
            }
        }
    }
    Ok(match file {
        Some(mut f) => {
            f.flush().map_err(io)?;
            Spooled::File(f)
        }
        None => Spooled::Memory(buf),
    })
}

/// Graph Store GET body: the quads of graph `g` (every graph when `None`) of one
/// snapshot, serialized while scanning (see [`stream`]) under the result-size budget.
/// Returns the body and, for a body returned whole, its bytes and quads.
async fn graph_body(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    snap: Arc<sparkles::store::Snapshot>,
    g: Option<Id>,
    fmt: RdfFormat,
) -> ApiResult<(axum::body::Body, Option<(u64, u64)>)> {
    let quads = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = quads.clone();
    // an export reads every block once: keep the blocks queries use cached
    let snap = snap.without_cache_fill();
    let prefixes = ds.store.prefixes();
    let write = move |w: &mut LimitedWriter<stream::SwitchWriter>| -> sparkles::Result<()> {
        let mut ser = RdfSerializer::from_format(fmt);
        if matches!(fmt, RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml) {
            for (p, ns) in prefixes {
                if let Ok(s) = ser.clone().with_prefix(p, ns) {
                    ser = s;
                }
            }
        }
        let mut s = ser.for_writer(w);
        let prefix: Vec<u64> = g.map(|g| vec![g.0]).unwrap_or_default();
        let mut n = 0u64;
        let mut write = |k: &[u64; 4]| -> sparkles::Result<()> {
            if let Some(q) = snap.quad_to_terms(&Perm::Gspo.to_quad(k)) {
                if g.is_some() {
                    s.serialize_triple(oxrdf::TripleRef::new(&q.subject, &q.predicate, &q.object))?;
                } else {
                    s.serialize_quad(&q)?;
                }
                n += 1;
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
        counted.store(n, std::sync::atomic::Ordering::Relaxed);
        s.finish()?;
        Ok(())
    };
    let limit = st.limits.max_result_bytes;
    let (name, st) = (ds.name.clone(), Arc::downgrade(&st));
    drop(ds);
    let body = stream::serialize(limit, write, move |end| {
        if let Some(st) = st.upgrade() {
            st.metrics
                .add_response_bytes(Some(&name), Op::Gsp, end.bytes);
        }
        stream_end_log("graph store", &end);
    })
    .await?;
    Ok(match body {
        stream::Serialized::Whole { body, .. } => {
            let whole = (
                body.len() as u64,
                quads.load(std::sync::atomic::Ordering::Relaxed),
            );
            (axum::body::Body::from(body), Some(whole))
        }
        stream::Serialized::Streamed(body) => (body, None),
    })
}

async fn gsp(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult {
    // every write needs write access, whichever route (or fallthrough) led here
    if !matches!(method, Method::GET | Method::HEAD)
        && let Some(denied) = crate::auth::dataset_denial(&st, &p, &headers, &name, Level::Write)
    {
        return Ok(denied);
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let target = gsp_target(&params);
    match method {
        Method::GET | Method::HEAD => {
            let quads = matches!(target, Target::Dataset);
            let fmt = rdf_format(&params, &headers, quads);
            let head = method == Method::HEAD;
            // resolve the graph (or 404) before the response starts
            let at = history::at_param(&params)?;
            let opts = query_options(&st, &ds, &params);
            let (snap, g, resolved) = blocking({
                let ds = ds.clone();
                move || {
                    let (snap, resolved) = history::snapshot_for(&ds, at.as_ref(), &opts)?;
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
                    Ok((snap, g, resolved))
                }
            })
            .await?;
            let seq = snap.commit;
            let ct = [(header::CONTENT_TYPE, results::rdf_media_type(fmt))];
            let mut report = RequestReport {
                operation: Some(Op::Gsp),
                ..Default::default()
            };
            let resp = if head {
                ct.into_response()
            } else {
                let (body, whole) = graph_body(st.clone(), ds.clone(), snap, g, fmt).await?;
                if let Some((bytes, quads)) = whole {
                    report.response_bytes = Some(bytes);
                    report.rows = Some(quads);
                }
                (ct, body).into_response()
            };
            let resp = with_commit(resp, &ds, seq);
            Ok(report.attach(history::history_headers(resp, resolved.as_ref(), &uri)))
        }
        Method::PUT | Method::POST => {
            if st.read_only {
                return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
            }
            history::reject_at(&params)?;
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
            let wopts =
                validation::write_options(&st, &params, &headers, st.limits.update_timeout)?;
            let body = spool(body, &mut BodyBudget::new(&st.limits)).await?;
            blocking(move || {
                let graph = match &target {
                    Target::Named(iri) => Some(
                        oxrdf::NamedNode::new(iri.clone())
                            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?,
                    ),
                    _ => None,
                };
                let (src, _spooled) =
                    body.into_source(format, graph.clone(), st.limits.max_decompressed_bytes);
                use sparkles::commit::CommitKind;
                let (count, receipt) = if replace {
                    // parse first, then clear and insert atomically
                    let t = match graph {
                        Some(g) => ReplaceTarget::Named(g),
                        None if matches!(target, Target::Dataset) => ReplaceTarget::All,
                        None => ReplaceTarget::Default,
                    };
                    ds.store
                        .replace_with(t, &[src], CommitKind::GspPut, &wopts)?
                } else {
                    let r = ds.store.load_with(&[src], CommitKind::GspPost, &wopts)?;
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
            history::reject_at(&params)?;
            let wanted = receipt_wanted(&params, &headers);
            let wopts =
                validation::write_options(&st, &params, &headers, st.limits.update_timeout)?;
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
                    &QueryOptions {
                        write: wopts,
                        ..Default::default()
                    },
                    sparkles::commit::CommitKind::GspDelete,
                )?;
                let receipt = stats.commit.clone().expect("update receipts");
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
    history::reject_at(&params)?;
    let wanted = receipt_wanted(&params, &headers);
    let wopts = validation::write_options(&st, &params, &headers, st.limits.update_timeout)?;
    let ct = content_type(&headers);
    let tmp = tempfile::Builder::new()
        .prefix("sparkles-upload-")
        .tempdir()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut graph: Option<String> = params.get("graph").map(str::to_string);
    let mut budget = BodyBudget::new(&st.limits);
    if ct == "multipart/form-data" {
        use axum::extract::FromRequest;
        // the route's body limit (`--max-upload-mb`) applies to the whole stream too
        let mp_err = |e: axum::extract::multipart::MultipartError| err(e.status(), e.body_text());
        let mut mp = Multipart::from_request(request, &())
            .await
            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
        while let Some(mut field) = mp.next_field().await.map_err(mp_err)? {
            match field.name() {
                Some("graph") => {
                    let mut g = Vec::new();
                    while let Some(chunk) = field.chunk().await.map_err(mp_err)? {
                        budget.read(chunk.len())?;
                        g.extend_from_slice(&chunk);
                        if g.len() > 64 << 10 {
                            return Err(err(StatusCode::BAD_REQUEST, "graph field too long"));
                        }
                    }
                    let g = String::from_utf8(g)
                        .map_err(|_| err(StatusCode::BAD_REQUEST, "graph field is not UTF-8"))?;
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
                    // copied chunk by chunk: an upload is never held in memory whole
                    let path = tmp.path().join(format!("{}-{fname}", files.len()));
                    let io =
                        |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
                    let mut out =
                        std::io::BufWriter::new(std::fs::File::create(&path).map_err(io)?);
                    while let Some(chunk) = field.chunk().await.map_err(mp_err)? {
                        budget.read(chunk.len())?;
                        budget.disk(tmp.path(), chunk.len())?;
                        std::io::Write::write_all(&mut out, &chunk).map_err(io)?;
                    }
                    std::io::Write::flush(&mut out).map_err(io)?;
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
        let body = spool_after(request.into_body(), 0, &mut budget).await?;
        let ext = match format {
            RdfFormat::NTriples => "nt",
            RdfFormat::NQuads => "nq",
            RdfFormat::TriG => "trig",
            RdfFormat::RdfXml => "rdf",
            RdfFormat::JsonLd { .. } => "jsonld",
            _ => "ttl",
        };
        let path = tmp.path().join(format!("body.{ext}"));
        let io = |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        match body {
            Spooled::File(f) => {
                f.persist(&path).map_err(|e| io(e.error))?;
            }
            Spooled::Memory(b) => std::fs::write(&path, b).map_err(io)?,
        }
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
            .map(|p| {
                Source::from_path(p, g.clone()).map(|mut s| {
                    s.max_decompressed = st.limits.max_decompressed_bytes;
                    s
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let receipt = ds
            .store
            .load_with(&sources, sparkles::commit::CommitKind::Upload, &wopts)?;
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

async fn create_dataset(
    State(st): St,
    uri: Uri,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult {
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
    AdminBody(body): AdminBody,
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

/// `/{ds}/prefixes`, after Fuseki's prefixes service. GET: `?prefix=` → its IRI,
/// `?uri=` → the prefixes bound to it, neither → all stored prefixes. POST / PUT with
/// `prefix` and `uri` (query, form or JSON body) sets one; DELETE `?prefix=` removes one.
async fn dataset_prefixes(
    State(st): St,
    Path(name): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    match content_type(&headers).as_str() {
        "application/x-www-form-urlencoded" => params.extend_form(&body),
        "application/json" => {
            let j: J = serde_json::from_slice(&body)
                .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
            for k in ["prefix", "uri"] {
                if let Some(v) = j.get(k).and_then(J::as_str) {
                    params.0.push((k.to_string(), v.to_string()));
                }
            }
        }
        _ => {}
    }
    let prefixes = ds.store.prefixes();
    let prefix = params.get("prefix").map(str::to_string);
    let iri = params.get("uri").map(str::to_string);
    if method != Method::GET && method != Method::HEAD && st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let missing = |what: &str| {
        err(
            StatusCode::BAD_REQUEST,
            format!("missing '{what}' parameter"),
        )
    };
    match method {
        Method::GET | Method::HEAD => Ok(match (prefix, iri) {
            (Some(p), _) => match prefixes.get(&p) {
                Some(u) => Json(json!({ "prefix": p, "uri": u })).into_response(),
                None => return Err(err(StatusCode::NOT_FOUND, format!("no prefix '{p}'"))),
            },
            (None, Some(u)) => {
                let bound: Vec<&String> = prefixes
                    .iter()
                    .filter(|(_, v)| **v == u)
                    .map(|(k, _)| k)
                    .collect();
                Json(json!({ "uri": u, "prefixes": bound })).into_response()
            }
            (None, None) => Json(json!({ "prefixes": prefixes })).into_response(),
        }),
        Method::POST | Method::PUT => {
            let p = prefix.ok_or_else(|| missing("prefix"))?;
            let u = iri.ok_or_else(|| missing("uri"))?;
            ds.store.set_prefix(&p, &u)?;
            Ok(Json(json!({ "prefix": p, "uri": u })).into_response())
        }
        Method::DELETE => {
            let p = prefix.ok_or_else(|| missing("prefix"))?;
            if ds.store.remove_prefix(&p)? {
                Ok(StatusCode::NO_CONTENT.into_response())
            } else {
                Err(err(StatusCode::NOT_FOUND, format!("no prefix '{p}'")))
            }
        }
        _ => Err(err(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")),
    }
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

/// `POST /$/backup/{ds}[?compression=gzip|zstd|brotli|lz4|none&level=N]`: an N-Quads
/// backup in `backups/`, gzip by default (as Fuseki).
async fn backup(State(st): St, Path(name): Path<String>, uri: Uri) -> ApiResult {
    use sparkles::codec::{Codec, Level};
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let codec = match params.get("compression") {
        Some(c) => Codec::parse(c)?,
        None => Codec::Gzip,
    };
    let level = match params.get("level") {
        Some(l) => Some(Level(l.parse().map_err(|_| {
            err(StatusCode::BAD_REQUEST, "level must be an integer")
        })?)),
        None => None,
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let dir = st.data_dir.join("backups");
    let task = st.start_task("backup", &name, move |h| {
        h.progress(0.1, "writing N-Quads");
        let t = std::time::Instant::now();
        let p = ds
            .store
            .backup_with(&dir, &ds.name, codec, level, threads)?;
        let size = std::fs::metadata(&p).map_or(0, |m| m.len());
        Ok(format!(
            "backup written to {} ({}, {codec}, {:.1} s)",
            p.display(),
            sparkles::error::human_bytes(size),
            t.elapsed().as_secs_f64()
        ))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(feature = "reasoning")]
async fn reason(
    State(st): St,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
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
    QueryBody(body): QueryBody,
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
        let _validate = tracing::info_span!("shacl.validate").entered();
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
        let resp = with_commit(resp, &ds, snap.commit);
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
mod compress_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod obs_tests;
#[cfg(test)]
mod router_tests;
#[cfg(all(test, feature = "shacl"))]
mod validation_tests;

#[cfg(test)]
mod spool_tests {
    use super::*;

    #[tokio::test]
    async fn large_bodies_are_spooled_to_a_file() {
        let data: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect();
        let limits = crate::state::Limits::default();
        let mut budget = BodyBudget::new(&limits);
        let Ok(small) =
            spool_after(axum::body::Body::from(data.clone()), 1 << 20, &mut budget).await
        else {
            panic!("spooling failed")
        };
        assert!(matches!(&small, Spooled::Memory(b) if *b == data));
        let mut budget = BodyBudget::new(&limits);
        let Ok(Spooled::File(f)) =
            spool_after(axum::body::Body::from(data.clone()), 100, &mut budget).await
        else {
            panic!("expected a file")
        };
        assert_eq!(std::fs::read(f.path()).unwrap(), data);
        let path = f.path().to_path_buf();
        let (src, guard) = Spooled::File(f).into_source(RdfFormat::NTriples, None, None);
        assert!(matches!(src.data, sparkles::io::SourceData::File(ref p) if *p == path));
        drop(guard);
        assert!(!path.exists(), "the temporary file goes with its guard");
    }
}

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
