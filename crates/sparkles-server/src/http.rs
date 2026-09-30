//! HTTP layer: SPARQL 1.1 Protocol, Graph Store Protocol, Fuseki `/$/` admin API.

#[cfg(feature = "reasoning")]
use crate::state::ReasoningInfo;
use crate::state::{AppState, Dataset, DbType, now, uptime_secs};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use oxrdfio::{RdfFormat, RdfSerializer};
use serde_json::{Value as J, json};
use sparkles::index::Perm;
use sparkles::io::Source;
use sparkles::sparql::results::{self, SolutionsFormat};
use sparkles::sparql::{QueryKind, QueryOptions};
use sparkles::{Error, id::Id};
use std::sync::Arc;
use std::time::Duration;

pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

type St = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    let cors = tower_http::cors::CorsLayer::very_permissive();
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/ui/") }))
        .route("/ui", get(|| async { Redirect::temporary("/ui/") }))
        .route("/ui/", get(crate::ui::serve_index))
        .route("/ui/{*path}", get(crate::ui::serve))
        .route("/$/ping", get(ping).post(ping))
        .route("/$/server", get(server_info))
        .route("/$/datasets", get(list_datasets).post(create_dataset))
        .route("/$/datasets/{ds}", get(get_dataset).delete(delete_dataset))
        .route("/$/stats/{ds}", get(stats))
        .route("/$/compact/{ds}", post(compact))
        .route("/$/backup/{ds}", post(backup))
        .route("/$/reason/{ds}", post(reason).delete(unreason))
        .route("/$/tasks", get(list_tasks))
        .route("/$/tasks/{id}", get(get_task))
        .route("/$/prefixes/{ds}", get(prefixes))
        .route("/{ds}", any(dataset_root))
        .route("/{ds}/sparql", any(query_endpoint))
        .route("/{ds}/query", any(query_endpoint))
        .route("/{ds}/update", post(update_endpoint))
        .route("/{ds}/data", any(gsp))
        .route("/{ds}/get", get(gsp).head(gsp))
        .route("/{ds}/upload", post(upload))
        .route("/{ds}/explain", get(explain).post(explain))
        .layer(DefaultBodyLimit::max(8 << 30))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(cors)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

// ------------------------------------------------------------------ errors ------

pub struct ApiError(StatusCode, J);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
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
            Error::MemoryLimit(_) => StatusCode::INSUFFICIENT_STORAGE,
            Error::Service(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = if let Error::SparqlSyntax(_) = e {
            syntax_error_body(&msg)
        } else {
            json!({ "error": msg })
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

fn query_options(st: &AppState, ds: &Dataset, params: &Params) -> QueryOptions {
    let timeout = params
        .get("timeout")
        .and_then(|t| t.parse::<f64>().ok())
        .map(Duration::from_secs_f64)
        .unwrap_or(st.default_timeout);
    let reasoning =
        params.get("reasoning").is_none_or(|v| v != "false") && ds.reasoning.read().is_some();
    QueryOptions {
        timeout: Some(timeout),
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
        return query_endpoint(st, Path(name), method, uri, headers, body).await;
    }
    if params.has("update") || ct == "application/sparql-update" {
        return update_endpoint(st, Path(name), uri, headers, body).await;
    }
    gsp(st, Path(name), method, uri, headers, body).await
}

async fn query_endpoint(
    State(st): St,
    Path(name): Path<String>,
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
    let opts = query_options(&st, &ds, &params);
    let sfmt = solutions_format(&params, &headers);
    let rfmt = rdf_format(&params, &headers, false);
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    let prefixes = ds.store.prefixes();
    blocking(move || {
        let t = std::time::Instant::now();
        let r = sparkles::sparql::query(ds.store.snapshot(), &query, &opts)?;
        let mut buf = Vec::new();
        let is_graph = !matches!(r.kind, QueryKind::Select | QueryKind::Ask);
        let ct: String = match r.kind {
            _ if sfmt == SolutionsFormat::Sparkles
                && (!is_graph || params_wants_sparkles(&headers)) =>
            {
                // Build the document once, then patch the serialization time into it.
                let ts = std::time::Instant::now();
                let mut doc = results::sparkles_json(&r, send);
                let ser_ms = ts.elapsed().as_secs_f64() * 1000.0;
                if let Some(timing) = doc.pointer_mut("/meta/timing").and_then(J::as_object_mut) {
                    let total = timing.get("totalMs").and_then(J::as_f64).unwrap_or(0.0);
                    timing.insert("serializeMs".into(), ser_ms.into());
                    timing.insert("totalMs".into(), (total + ser_ms).into());
                }
                serde_json::to_writer(&mut buf, &doc)
                    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                SolutionsFormat::Sparkles.media_type().into()
            }
            QueryKind::Select | QueryKind::Ask => {
                results::write_solutions(&r, sfmt, &mut buf, send)?;
                sfmt.media_type().into()
            }
            _ => {
                results::write_graph(&r, rfmt, &prefixes, &mut buf)?;
                results::rdf_media_type(rfmt).into()
            }
        };
        tracing::debug!("query executed in {:?} ({} results)", t.elapsed(), r.len());
        Ok(([(header::CONTENT_TYPE, ct)], buf).into_response())
    })
    .await
}

fn params_wants_sparkles(h: &HeaderMap) -> bool {
    h.get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/x-sparkles+json"))
}

async fn update_endpoint(
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
    let opts = QueryOptions {
        allow_service: st.allow_service,
        ..Default::default()
    };
    blocking(move || {
        let stats = sparkles::sparql::update::update(&ds.store, &update, &opts)?;
        Ok(Json(serde_json::to_value(stats).unwrap()).into_response())
    })
    .await
}

async fn explain(
    State(st): St,
    Path(name): Path<String>,
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
    let opts = query_options(&st, &ds, &params);
    blocking(move || {
        let (algebra, plan) = sparkles::sparql::explain(ds.store.snapshot(), &query, &opts)?;
        Ok(Json(json!({ "algebra": algebra, "plan": plan })).into_response())
    })
    .await
}

// ------------------------------------------------------ Graph Store Protocol ------

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
            blocking(move || {
                let snap = ds.store.snapshot();
                let prefixes = ds.store.prefixes();
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
                let mut buf = Vec::new();
                if !head {
                    let mut ser = RdfSerializer::from_format(fmt);
                    if matches!(fmt, RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml) {
                        for (p, ns) in &prefixes {
                            if let Ok(s) = ser.clone().with_prefix(p.clone(), ns.clone()) {
                                ser = s;
                            }
                        }
                    }
                    let mut w = ser.for_writer(&mut buf);
                    let prefix: Vec<u64> = g.map(|g| vec![g.0]).unwrap_or_default();
                    for k in snap.scan_keys(Perm::Gspo, &prefix)? {
                        if let Some(q) = snap.quad_to_terms(&Perm::Gspo.to_quad(&k)) {
                            if g.is_some() {
                                w.serialize_triple(oxrdf::TripleRef::new(
                                    &q.subject,
                                    &q.predicate,
                                    &q.object,
                                ))
                                .map_err(Error::Io)?;
                            } else {
                                w.serialize_quad(&q).map_err(Error::Io)?;
                            }
                        }
                    }
                    w.finish().map_err(Error::Io)?;
                }
                Ok(([(header::CONTENT_TYPE, results::rdf_media_type(fmt))], buf).into_response())
            })
            .await
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
            blocking(move || {
                let graph = match &target {
                    Target::Named(iri) => Some(
                        oxrdf::NamedNode::new(iri.clone())
                            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?,
                    ),
                    _ => None,
                };
                if replace {
                    let clear = match &target {
                        Target::Default => "CLEAR SILENT DEFAULT".to_string(),
                        Target::Named(iri) => format!("CLEAR SILENT GRAPH <{iri}>"),
                        Target::Dataset => "CLEAR SILENT ALL".to_string(),
                    };
                    sparkles::sparql::update::update(&ds.store, &clear, &QueryOptions::default())?;
                }
                let before = ds.store.snapshot().len();
                let src = Source::from_bytes(body.to_vec(), format, graph);
                ds.store.load(&[src])?;
                let count = ds.store.snapshot().len().saturating_sub(before);
                let status = if replace {
                    StatusCode::OK
                } else {
                    StatusCode::CREATED
                };
                Ok((
                    status,
                    Json(json!({ "count": count, "tripleCount": count, "quadCount": count })),
                )
                    .into_response())
            })
            .await
        }
        Method::DELETE => {
            if st.read_only {
                return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
            }
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
                sparkles::sparql::update::update(&ds.store, &clear, &QueryOptions::default())?;
                Ok(StatusCode::NO_CONTENT.into_response())
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
        let before = ds.store.snapshot().len();
        ds.store.load(&sources)?;
        let count = ds.store.snapshot().len().saturating_sub(before);
        drop(tmp);
        Ok(
            Json(json!({ "count": count, "tripleCount": count, "quadCount": count }))
                .into_response(),
        )
    })
    .await
}

// ------------------------------------------------------------------- admin ------

async fn ping() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain")], now())
}

fn dataset_info(ds: &Dataset) -> J {
    let n = &ds.name;
    json!({
        "name": n,
        "type": ds.kind,
        "endpoints": {
            "query": format!("/{n}/sparql"),
            "update": format!("/{n}/update"),
            "gsp": format!("/{n}/data"),
            "upload": format!("/{n}/upload"),
        },
        "quads": ds.store.snapshot().len(),
        "reasoning": *ds.reasoning.read(),
    })
}

async fn server_info(State(st): St) -> Json<J> {
    let datasets: Vec<J> = st
        .datasets
        .read()
        .values()
        .map(|d| dataset_info(d))
        .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "startedAt": st.started_at,
        "uptimeSeconds": uptime_secs(&st),
        "datasets": datasets,
    }))
}

async fn list_datasets(State(st): St) -> Json<J> {
    let datasets: Vec<J> = st
        .datasets
        .read()
        .values()
        .map(|d| dataset_info(d))
        .collect();
    Json(json!({ "datasets": datasets }))
}

async fn get_dataset(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(dataset_info(&ds)))
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
    let st2 = st.clone();
    let ds = blocking(move || Ok(st2.create(&name, kind)?)).await?;
    Ok((StatusCode::CREATED, Json(dataset_info(&ds))).into_response())
}

async fn delete_dataset(State(st): St, Path(name): Path<String>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let st2 = st.clone();
    let deleted = blocking(move || Ok(st2.delete(&name)?)).await?;
    if deleted {
        Ok(StatusCode::OK.into_response())
    } else {
        Err(err(StatusCode::NOT_FOUND, "no such dataset"))
    }
}

async fn stats(State(st): St, Path(name): Path<String>) -> ApiResult {
    let ds = dataset(&st, &name)?;
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
            "cache": {
                "entries": cache.entries(),
                "bytes": cache.bytes(),
                "hits": cache.hits(),
                "misses": cache.misses(),
            },
        }))
        .into_response())
    })
    .await
}

async fn prefixes(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let mut p = sparkles::io::standard_prefixes();
    p.extend(ds.store.prefixes());
    Ok(Json(json!({ "prefixes": p })))
}

async fn compact(State(st): St, Path(name): Path<String>) -> ApiResult {
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
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let (profile_name, rules) = if content_type(&headers) == "application/json" && !body.is_empty()
    {
        let v: J = serde_json::from_slice(&body)
            .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
        (
            v["profile"].as_str().unwrap_or("rdfs").to_string(),
            v["rules"].as_str().map(str::to_string),
        )
    } else {
        let mut p = Params::default();
        p.extend_form(&body);
        (
            p.get("profile").unwrap_or("rdfs").to_string(),
            p.get("rules").map(str::to_string),
        )
    };
    let profile: sparkles_reasoner::Profile = if profile_name == "rules" {
        sparkles_reasoner::Profile::Rules(rules.unwrap_or_default())
    } else {
        profile_name.parse().map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unknown profile '{profile_name}'"),
            )
        })?
    };
    let st2 = st.clone();
    let task = st.start_task("reason", &name, move |h| {
        let h2 = h.clone();
        let progress: sparkles_reasoner::ProgressFn =
            Arc::new(move |p, msg: &str| h2.progress(p, msg));
        h.progress(0.05, "loading triples");
        let opts = sparkles_reasoner::ReasonOptions {
            progress: Some(progress),
            ..Default::default()
        };
        let report = sparkles_reasoner::materialize(&ds.store, &profile, &opts)?;
        *ds.reasoning.write() = Some(ReasoningInfo {
            profile: profile_name.clone(),
            inferred: report.inferred,
            at: now(),
        });
        st2.save_registry()?;
        Ok(format!(
            "{} inferred triples in {} ms ({} iterations){}",
            report.inferred,
            report.millis,
            report.iterations,
            if report.warnings.is_empty() {
                String::new()
            } else {
                format!("; warnings: {}", report.warnings.join("; "))
            }
        ))
    });
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
        *ds.reasoning.write() = None;
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

async fn list_tasks(State(st): St) -> Json<J> {
    let tasks = st.tasks.lock().clone();
    Json(serde_json::to_value(tasks).unwrap())
}

async fn get_task(State(st): St, Path(id): Path<String>) -> ApiResult<Json<J>> {
    st.tasks
        .lock()
        .iter()
        .find(|t| t.id == id)
        .map(|t| Json(serde_json::to_value(t).unwrap()))
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such task"))
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
