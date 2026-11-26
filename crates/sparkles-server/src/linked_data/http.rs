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
    Description, Format, describe_authorized, html, negotiate_format, not_found, recipes,
    resolve_prefix, serialize_rdf,
};
use crate::auth::Principal;
use crate::http::{ApiError, ApiResult, Params, blocking, err_code};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/ld", get(resolve_query))
        .route("/$/ld/doc", get(document))
        .route("/$/ld/ns/{prefix}", get(resolve_ns_root))
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
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());
    negotiate_format(accept).ok_or_else(|| {
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

fn render_response(
    desc: &Description,
    format: Format,
    uri: &Uri,
    version_param: Option<&str>,
    at_param: Option<&str>,
    extra: &[(String, String)],
    max_result_bytes: Option<u64>,
) -> ApiResult<Response> {
    let mut resp = match format.rdf_format() {
        Some(rdf_fmt) => {
            let bytes = serialize_rdf(desc, rdf_fmt, max_result_bytes)?;
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, format.media_type())],
                bytes,
            )
                .into_response()
        }
        None => {
            let body = html::render(desc, version_param, at_param, extra);
            if let Some(limit) = max_result_bytes
                && (body.len() as u64) > limit
            {
                return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::ResultBytes,
                    limit,
                    requested: body.len() as u64,
                })
                .into());
            }
            let mut r = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, format.media_type())],
                body,
            )
                .into_response();
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(html::HTML_CSP),
            );
            h.insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            r
        }
    };

    {
        let h = resp.headers_mut();
        h.insert(header::VARY, HeaderValue::from_static("Accept"));
        let links = link_header(&desc.iri, version_param, at_param, extra);
        if let Ok(v) = HeaderValue::from_str(&links) {
            h.insert(header::LINK, v);
        }
        if desc.truncated {
            h.insert(
                crate::http::describe::SPARKLES_DESCRIBE_TRUNCATED,
                HeaderValue::from_static("true"),
            );
        }
    }

    // Apply history/Memento headers when resolved to a pinned snapshot or commit.
    // Note: `history_headers` sets `Link: <original>; rel="original"`; preserve our
    // alternate links by appending when both exist.
    if let Some(resolved) = &desc.resolved_history {
        let existing_link = resp
            .headers()
            .get(header::LINK)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        resp = crate::http::history::history_headers(resp, Some(resolved), uri);
        if let Some(alt) = existing_link {
            let combined = match resp
                .headers()
                .get(header::LINK)
                .and_then(|v| v.to_str().ok())
            {
                Some(orig) if orig != alt => format!("{alt}, {orig}"),
                _ => alt,
            };
            if let Ok(v) = HeaderValue::from_str(&combined) {
                resp.headers_mut().insert(header::LINK, v);
            }
        }
    }

    Ok(resp)
}

async fn handle_lookup(
    st: Arc<AppState>,
    principal: Principal,
    uri: Uri,
    headers: HeaderMap,
    iri: String,
    redirect_slash: bool,
) -> ApiResult<Response> {
    let params = Params::from_query(&uri);
    let format = match pick_format(&params, &headers) {
        Ok(f) => f,
        Err(resp) => return Ok(*resp),
    };
    let at = parse_at(&params)?;
    let version_param = params.get("version").map(str::to_string);
    let at_param = params.get("at").map(str::to_string);
    let extra = describe_extra(&params);
    let max_result_bytes = st.limits.max_result_bytes;

    let (st_c, p_c, iri_c, ver_c, params_c) = (
        st.clone(),
        principal,
        iri.clone(),
        version_param.clone(),
        params.clone(),
    );
    let desc = blocking(move || {
        let catalog = st_c.resources.read().map_err(ApiError::from)?;
        describe_authorized(
            &st_c,
            &p_c,
            &catalog,
            &iri_c,
            ver_c.as_deref(),
            at,
            &params_c,
        )
    })
    .await?;

    if redirect_slash && !desc.is_hash {
        let location = html::doc_url(
            &desc.iri,
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
        return Ok(resp);
    }

    render_response(
        &desc,
        format,
        &uri,
        version_param.as_deref(),
        at_param.as_deref(),
        &extra,
        max_result_bytes,
    )
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
    handle_lookup(st, p, uri, headers, iri, true).await
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
    handle_lookup(st, p, uri, headers, iri, false).await
}

/// `GET /$/ld/ns/{prefix}`: resolve the namespace root for hosted `prefix`.
async fn resolve_ns_root(
    State(st): St,
    Path(prefix): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let st_c = st.clone();
    let iri = blocking(move || {
        let catalog = st_c.resources.read().map_err(ApiError::from)?;
        resolve_prefix(&catalog, &prefix, None).ok_or_else(not_found)
    })
    .await?;
    handle_lookup(st, p, uri, headers, iri, true).await
}

/// `GET /$/ld/ns/{prefix}/{*local}`: resolve `<namespace_iri><local>` for hosted `prefix`.
async fn resolve_ns_local(
    State(st): St,
    Path((prefix, local)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let st_c = st.clone();
    let iri = blocking(move || {
        let catalog = st_c.resources.read().map_err(ApiError::from)?;
        resolve_prefix(&catalog, &prefix, Some(&local)).ok_or_else(not_found)
    })
    .await?;
    handle_lookup(st, p, uri, headers, iri, true).await
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
            let accept = headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain");
            matches!(
                crate::http::negotiate(accept, &["text/plain", "application/json"]),
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
        let export = recipes::generate(r, kind, &base)
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
