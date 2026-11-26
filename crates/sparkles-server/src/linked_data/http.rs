//! HTTP routes for the linked-data front end (spec C32, phase 1).
//!
//! Routes:
//! * `GET /$/ld` — IRI lookup and content negotiation (`?iri=...`). Slash IRIs answer
//!   `303 See Other` to `/$/ld/doc` with `Vary: Accept`; hash IRIs answer `200 OK` directly.
//! * `GET /$/ld/doc` — Direct representation endpoint (`?iri=...&format=...`), always
//!   answering `200 OK` with `Vary: Accept`, `Link` alternates and bounded HTML or RDF.
//! * `GET /$/ld/ns/{prefix}` and `GET /$/ld/ns/{prefix}/{*local}` — Path-based resolution
//!   under a hosted namespace prefix, applying the same 303/200 and ACL rules as `/$/ld`.
//! * `GET /$/resources/{id}/recipes` (wired in `resources::http`) — Server-admin export
//!   of PURL / w3id Apache `.htaccess` redirection recipes.

use super::{
    Format, LookupOutcome, LookupTarget, MemoryCharge, html, negotiate_format, recipes,
    resolve_and_describe, serialize_rdf, valid_local_part, valid_lookup_iri,
};
use crate::auth::Principal;
use crate::http::{ApiError, ApiResult, Params, blocking, err_code};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use sparkles::history::Resolved;
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/ld", get(resolve_query))
        .route("/$/ld/doc", get(document))
        .route("/$/ld/ns/{prefix}", get(resolve_ns_root))
        .route("/$/ld/ns/{prefix}/", get(resolve_ns_root))
        .route("/$/ld/ns/{prefix}/{*local}", get(resolve_ns_local))
}

const DESCRIBE_PARAMS: [&str; 5] = [
    "describe",
    "describe-labels",
    "describe-reifiers",
    "describe-max-triples",
    "describe-max-depth",
];

fn describe_extra(params: &Params) -> Vec<(String, String)> {
    params
        .0
        .iter()
        .filter(|(k, _)| DESCRIBE_PARAMS.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// Combine all `Accept` header fields per RFC 9110 §5.3.
fn combined_accept(headers: &HeaderMap) -> Option<String> {
    let vals: Vec<&str> = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    (!vals.is_empty()).then(|| vals.join(", "))
}

fn pick_format(params: &Params, headers: &HeaderMap) -> Result<Format, Box<Response>> {
    if let Some(f) = params.get("format") {
        return Format::from_param(f).ok_or_else(|| {
            Box::new(
                err_code(
                    StatusCode::BAD_REQUEST,
                    "unknown-format",
                    format!(
                        "unknown format '{f}': expected html, turtle, jsonld, rdfxml or ntriples"
                    ),
                )
                .into_response(),
            )
        });
    }
    let accept = combined_accept(headers);
    negotiate_format(accept.as_deref()).ok_or_else(|| {
        let mut resp = err_code(
            StatusCode::NOT_ACCEPTABLE,
            "not-acceptable",
            "no supported representation matches Accept (supported: text/html, text/turtle, application/ld+json, application/rdf+xml, application/n-triples)",
        )
        .into_response();
        resp.headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Accept"));
        Box::new(resp)
    })
}

fn parse_at(params: &Params) -> ApiResult<Option<sparkles::history::At>> {
    match params.get("at") {
        None => Ok(None),
        Some(raw) => raw.parse::<sparkles::history::At>().map(Some).map_err(|e| {
            err_code(
                StatusCode::BAD_REQUEST,
                "invalid-at",
                format!("invalid at selector {raw:?}: {e}"),
            )
        }),
    }
}

fn link_header(
    iri: &str,
    version: Option<&str>,
    at: Option<&str>,
    extra: &[(String, String)],
) -> String {
    Format::ALL
        .iter()
        .map(|fmt| {
            let u = html::doc_url(iri, *fmt, version, at, extra);
            format!("<{u}>; rel=\"alternate\"; type=\"{}\"", fmt.essence())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Live original URI for a historical linked-data response, stripping both `at` and
/// `version`. Returns `None` when the request IRI itself selected a catalog version
/// (exact or slash version prefix) or when stripping `at`/`version` would leave the URI
/// unchanged.
fn live_original_uri(uri: &Uri, iri_pinned_version: bool) -> Option<String> {
    if iri_pinned_version {
        return None;
    }
    let query_raw = uri.query().unwrap_or("");
    let filtered: Vec<&str> = query_raw
        .split('&')
        .filter(|kv| {
            !kv.is_empty() && !matches!(kv.split('=').next(), Some("at") | Some("version"))
        })
        .collect();
    let original = if filtered.is_empty() {
        uri.path().to_string()
    } else {
        format!("{}?{}", uri.path(), filtered.join("&"))
    };
    let current = match uri.query() {
        Some(q) if !q.is_empty() => format!("{}?{q}", uri.path()),
        _ => uri.path().to_string(),
    };
    (original != current).then_some(original)
}

/// Response buffer owner attached to [`Bytes::from_owner`] so the rendered allocation's
/// [`MemoryCharge`] (and [`PoolGrant`]) remains live until every `Bytes` frame or slice
/// referencing the buffer is dropped, even after the HTTP body stream ends.
struct ChargedBytes {
    // Declared before `_charge` so the heap allocation frees before releasing the charge.
    buf: Vec<u8>,
    _charge: MemoryCharge,
}

impl AsRef<[u8]> for ChargedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.buf
    }
}

enum RenderedBody {
    Rdf(Vec<u8>),
    Html(String),
}

struct RenderedDocument {
    iri: String,
    body: RenderedBody,
    truncated: bool,
    resolved_history: Option<Resolved>,
    iri_pinned_version: bool,
    memory_charge: MemoryCharge,
}

enum RenderedOutcome {
    Redirect { iri: String },
    Document(RenderedDocument),
}

fn build_document_response(
    doc: RenderedDocument,
    format: Format,
    uri: &Uri,
    version_param: Option<&str>,
    at_param: Option<&str>,
    extra: &[(String, String)],
) -> Response {
    let (is_html, buf) = match doc.body {
        RenderedBody::Rdf(bytes) => (false, bytes),
        RenderedBody::Html(html_body) => (true, html_body.into_bytes()),
    };
    let bytes = Bytes::from_owner(ChargedBytes {
        buf,
        _charge: doc.memory_charge,
    });
    let body = Body::from(bytes);
    let mut resp = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, format.media_type())],
        body,
    )
        .into_response();
    if is_html {
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(html::HTML_CSP),
        );
        h.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
    }

    let h = resp.headers_mut();
    h.insert(header::VARY, HeaderValue::from_static("Accept"));
    let mut links = link_header(&doc.iri, version_param, at_param, extra);
    if doc.truncated {
        h.insert(
            crate::http::describe::SPARKLES_DESCRIBE_TRUNCATED,
            HeaderValue::from_static("true"),
        );
    }

    if let Some(resolved) = &doc.resolved_history {
        if let Ok(v) = HeaderValue::from_str(&resolved.at.to_string()) {
            h.insert("sparkles-at", v);
        }
        h.insert("sparkles-head", resolved.head.into());
        if resolved.historical {
            let t = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(resolved.commit.timestamp_ms.max(0) as u64);
            if let Ok(v) = HeaderValue::from_str(&httpdate::fmt_http_date(t)) {
                h.insert("memento-datetime", v);
            }
            if let Some(orig) = live_original_uri(uri, doc.iri_pinned_version) {
                links.push_str(&format!(", <{orig}>; rel=\"original\""));
            }
        }
    }

    if let Ok(v) = HeaderValue::from_str(&links) {
        h.insert(header::LINK, v);
    }

    resp
}

async fn handle_lookup(
    st: Arc<AppState>,
    principal: Principal,
    uri: Uri,
    headers: HeaderMap,
    target: LookupTarget,
    redirect_slash: bool,
) -> ApiResult<Response> {
    // Validate every target-independent request parameter BEFORE reading the catalog so
    // private prefixes and absent prefixes answer identically.
    let params = Params::from_query(&uri);
    let format = match pick_format(&params, &headers) {
        Ok(f) => f,
        Err(resp) => return Ok(*resp),
    };
    let at = parse_at(&params)?;
    crate::http::describe::validate_params(&params)?;
    match &target {
        LookupTarget::Iri(iri) if !valid_lookup_iri(iri) => {
            return Err(err_code(
                StatusCode::BAD_REQUEST,
                "invalid-iri",
                "iri must be a valid absolute IRI",
            ));
        }
        LookupTarget::Namespace {
            local: Some(loc), ..
        } if !loc.is_empty() && !valid_local_part(loc) => {
            return Err(err_code(
                StatusCode::BAD_REQUEST,
                "invalid-iri",
                "iri must be a valid absolute IRI",
            ));
        }
        _ => {}
    }

    let version_param = params.get("version").map(str::to_string);
    let at_param = params.get("at").map(str::to_string);
    let extra = describe_extra(&params);
    let max_result_bytes = st.limits.max_result_bytes;

    // Capture the admitted query memory pool grant on the async request task before
    // crossing into `spawn_blocking`, where Tokio task-locals are absent.
    let grant = crate::server_memory::current_grant();

    let (st_c, p_c, ver_c, at_str_c, extra_c, params_c) = (
        st.clone(),
        principal,
        version_param.clone(),
        at_param.clone(),
        extra.clone(),
        params.clone(),
    );
    let rendered = blocking(move || {
        let catalog = st_c.resources.read().map_err(|e| {
            tracing::warn!("linked-data catalog read failed: {e:?}");
            err_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalog-unreadable",
                "resource catalog is temporarily unavailable",
            )
        })?;
        match resolve_and_describe(
            &st_c,
            &p_c,
            &catalog,
            target,
            ver_c.as_deref(),
            at,
            &params_c,
            grant,
            redirect_slash,
        )? {
            LookupOutcome::Redirect { iri } => Ok(RenderedOutcome::Redirect { iri }),
            LookupOutcome::Document(mut desc) => {
                // Serialize RDF or render HTML on the blocking thread while `desc`
                // charges the output buffer against the shared request memory budget and
                // pool grant.
                let body = match format.rdf_format() {
                    Some(rdf_fmt) => {
                        RenderedBody::Rdf(serialize_rdf(&mut desc, rdf_fmt, max_result_bytes)?)
                    }
                    None => RenderedBody::Html(
                        html::render(
                            &mut desc,
                            ver_c.as_deref(),
                            at_str_c.as_deref(),
                            &extra_c,
                            max_result_bytes,
                        )
                        .map_err(ApiError::from)?,
                    ),
                };
                // Drop the input triples/QueryResult (including any retained historical
                // snapshot) and release their charges, keeping the rendered body buffer
                // capacity charged inside `memory_charge` until the response `Bytes` drop.
                let super::Description {
                    iri,
                    triples,
                    truncated,
                    resolved_history,
                    iri_pinned_version,
                    mut memory_charge,
                    retained_triples_bytes,
                    retained_snapshot_bytes,
                    ..
                } = *desc;
                drop(triples);
                memory_charge.clear_external();
                memory_charge
                    .release(retained_triples_bytes.saturating_add(retained_snapshot_bytes));
                Ok(RenderedOutcome::Document(RenderedDocument {
                    iri,
                    body,
                    truncated,
                    resolved_history,
                    iri_pinned_version,
                    memory_charge,
                }))
            }
        }
    })
    .await?;

    match rendered {
        RenderedOutcome::Redirect { iri } => {
            let location = html::doc_url(
                &iri,
                format,
                version_param.as_deref(),
                at_param.as_deref(),
                &extra,
            );
            let mut resp = StatusCode::SEE_OTHER.into_response();
            let h = resp.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&location) {
                h.insert(header::LOCATION, v);
            }
            h.insert(header::VARY, HeaderValue::from_static("Accept"));
            Ok(resp)
        }
        RenderedOutcome::Document(doc) => Ok(build_document_response(
            doc,
            format,
            &uri,
            version_param.as_deref(),
            at_param.as_deref(),
            &extra,
        )),
    }
}

/// `GET /$/ld?iri=...`: content-negotiated IRI lookup with 303 for slash IRIs.
async fn resolve_query(
    State(st): St,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let params = Params::from_query(&uri);
    let Some(iri) = params.get("iri").map(str::to_string) else {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "missing-iri",
            "query parameter 'iri' is required",
        ));
    };
    handle_lookup(st, p, uri, headers, LookupTarget::Iri(iri), true).await
}

/// `GET /$/ld/doc?iri=...`: direct representation endpoint (200 OK, never 303).
async fn document(
    State(st): St,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let params = Params::from_query(&uri);
    let Some(iri) = params.get("iri").map(str::to_string) else {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "missing-iri",
            "query parameter 'iri' is required",
        ));
    };
    handle_lookup(st, p, uri, headers, LookupTarget::Iri(iri), false).await
}

/// `GET /$/ld/ns/{prefix}`: resolve the namespace root for hosted `prefix`.
async fn resolve_ns_root(
    State(st): St,
    Path(prefix): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    handle_lookup(
        st,
        p,
        uri,
        headers,
        LookupTarget::Namespace {
            prefix,
            local: None,
        },
        true,
    )
    .await
}

/// `GET /$/ld/ns/{prefix}/{*local}`: resolve `<namespace_iri><local>` for hosted `prefix`.
async fn resolve_ns_local(
    State(st): St,
    Path((prefix, local)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    handle_lookup(
        st,
        p,
        uri,
        headers,
        LookupTarget::Namespace {
            prefix,
            local: Some(local),
        },
        true,
    )
    .await
}

fn default_base_url(st: &AppState, headers: &HeaderMap) -> String {
    if let Some(public) = crate::auth::public_url(st) {
        return public;
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|h| !h.is_empty() && !h.chars().any(|c| c.is_control() || c.is_whitespace()))
        .unwrap_or("localhost:3030");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| matches!(*s, "http" | "https"))
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

/// `GET /$/resources/{id}/recipes`: export PURL or w3id redirection rules for a hosted
/// vocabulary resource (server `admin`).
pub async fn recipes(
    State(st): St,
    Path(id): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let params = Params::from_query(&uri);
    let kind = match params.get("kind") {
        None => recipes::RecipeKind::W3id,
        Some(k) => recipes::RecipeKind::parse(k).ok_or_else(|| {
            err_code(
                StatusCode::BAD_REQUEST,
                "bad-recipe-kind",
                format!("unknown recipe kind '{k}': expected w3id, htaccess or purl"),
            )
        })?,
    };
    let wants_json = match params.get("format") {
        Some("json") => true,
        Some("text") | Some("plain") => false,
        Some(other) => {
            return Err(err_code(
                StatusCode::BAD_REQUEST,
                "unknown-format",
                format!("unknown format '{other}': expected text or json"),
            ));
        }
        None => {
            let accept = combined_accept(&headers);
            let accept_str = accept.as_deref().unwrap_or("text/plain");
            matches!(
                crate::http::negotiate(accept_str, &["text/plain", "application/json"]),
                Some(1)
            )
        }
    };
    let base = params
        .get("base")
        .map(str::to_string)
        .unwrap_or_else(|| default_base_url(&st, &headers));
    let base = recipes::validate_base_url(&base)
        .map_err(|m| err_code(StatusCode::BAD_REQUEST, "bad-base-url", m))?;
    let mount = match params.get("mount") {
        Some(m) => Some(
            recipes::validate_mount(m)
                .map_err(|e| err_code(StatusCode::BAD_REQUEST, "bad-mount", e))?,
        ),
        None => None,
    };

    blocking(move || {
        let c = st.resources.read().map_err(ApiError::from)?;
        let r = crate::resources::find(&c, &id).ok_or_else(|| crate::resources::no_such(&id))?;
        if r.input.kind != crate::resources::Kind::Vocabulary {
            return Err(err_code(
                StatusCode::CONFLICT,
                "not-a-vocabulary",
                format!(
                    "{id} is a {}; only a vocabulary has hosted namespaces for recipes",
                    r.input.kind.as_str()
                ),
            ));
        }
        let export = recipes::generate(r, kind, &base, mount.as_deref())
            .map_err(|m| err_code(StatusCode::BAD_REQUEST, "bad-recipe", m))?;
        let mut resp = if wants_json {
            Json(export).into_response()
        } else {
            (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                export.content,
            )
                .into_response()
        };
        resp.headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Accept"));
        Ok(resp)
    })
    .await
}
