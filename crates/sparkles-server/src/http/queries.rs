//! Stored, parameterized queries (C16): `/$/queries/{ds}[/{name}[/versions]]` manages
//! them and `/{ds}/queries/{name}` runs one. A run's parameter values are checked
//! against their types and bound to the query's variables as initial bindings, then the
//! query runs exactly as one sent to `/{ds}/sparql` with the caller's budgets and view.

use super::{
    AdminBody, ApiError, ApiResult, Params, QueryBody, St, blocking, content_type, dataset, err,
};
use crate::auth::Principal;
use crate::state::Dataset;
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value as J, json};
use sparkles::stored::{Change, Definition, RESERVED, Stored};
use std::collections::BTreeMap;

/// The version of the stored query a run used.
pub const SPARKLES_QUERY_VERSION: &str = "sparkles-query-version";

fn not_found(ds: &str, name: &str) -> ApiError {
    err(
        StatusCode::NOT_FOUND,
        format!("no stored query '{name}' in /{ds}"),
    )
}

fn etag(v: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"v{v}\"")).unwrap_or(HeaderValue::from_static("\"v0\""))
}

/// A stored version as JSON: the definition's fields, its name, kind and version.
pub(crate) fn stored_json(ds: &Dataset, name: &str, s: &Stored) -> J {
    let mut j = serde_json::to_value(&s.definition).unwrap_or_default();
    if let Some(o) = j.as_object_mut() {
        o.insert("name".into(), name.into());
        o.insert("dataset".into(), ds.name.clone().into());
        if let Ok(kind) = s.definition.check() {
            o.insert("kind".into(), json!(kind));
        }
        o.insert("mcp".into(), s.definition.mcp.into());
        o.insert(
            "version".into(),
            serde_json::to_value(&s.version).unwrap_or_default(),
        );
    }
    j
}

/// `GET /$/queries/{ds}`
pub(super) async fn list(st: St, Path(ds_name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let queries: Vec<J> = ds
        .queries
        .list()
        .into_iter()
        .map(|(name, s)| {
            let mut j = stored_json(&ds, &name, &s);
            if let Some(o) = j.as_object_mut() {
                // listings leave the text out
                o.remove("query");
                o.remove("dataset");
            }
            j
        })
        .collect();
    let mut out = json!({ "dataset": ds.name, "queries": queries });
    if let Some(e) = ds.queries.broken() {
        out["warning"] = e.into();
    }
    Ok(Json(out))
}

fn version_param(params: &Params) -> ApiResult<Option<u64>> {
    params
        .get("version")
        .map(|v| {
            v.trim_start_matches('v')
                .parse::<u64>()
                .map_err(|_| err(StatusCode::BAD_REQUEST, "version must be a version number"))
        })
        .transpose()
}

/// `GET /$/queries/{ds}/{name}`
pub(super) async fn get_query(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let version = version_param(&Params::from_query(&uri))?;
    let s = ds
        .queries
        .get(&name, version)
        .ok_or_else(|| not_found(&ds.name, &name))?;
    let mut r = Json(stored_json(&ds, &name, &s)).into_response();
    r.headers_mut()
        .insert(header::ETAG, etag(s.version.version));
    Ok(r)
}

/// `GET /$/queries/{ds}/{name}/versions`
pub(super) async fn versions(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let v = ds
        .queries
        .versions(&name)
        .ok_or_else(|| not_found(&ds.name, &name))?;
    Ok(Json(
        json!({ "dataset": ds.name, "name": name, "versions": v }),
    ))
}

/// The version an `If-Match` or `If-None-Match: *` header requires (`Some(0)`: none).
fn precondition(headers: &HeaderMap, exists: bool) -> ApiResult<Option<u64>> {
    let text = |h| headers.get(h).and_then(|v: &HeaderValue| v.to_str().ok());
    if let Some(m) = text(header::IF_NONE_MATCH) {
        return if m.trim() == "*" {
            Ok(Some(0))
        } else {
            Err(err(
                StatusCode::BAD_REQUEST,
                "If-None-Match takes * only here",
            ))
        };
    }
    let Some(m) = text(header::IF_MATCH) else {
        return Ok(None);
    };
    let m = m.trim();
    if m == "*" {
        return if exists {
            Ok(None)
        } else {
            Err(err(
                StatusCode::PRECONDITION_FAILED,
                "the stored query does not exist",
            ))
        };
    }
    m.trim_start_matches("W/")
        .trim_matches('"')
        .strip_prefix('v')
        .and_then(|v| v.parse::<u64>().ok())
        .map(Some)
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "If-Match must be a version tag such as \"v3\"",
            )
        })
}

/// `PUT /$/queries/{ds}/{name}`: the JSON definition, with an optional `message`.
pub(super) async fn put_query(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let mut j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    let Some(o) = j.as_object_mut() else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the body must be a JSON object",
        ));
    };
    let mut message = match o.remove("message") {
        None | Some(J::Null) => None,
        Some(J::String(m)) => Some(m),
        Some(_) => return Err(err(StatusCode::BAD_REQUEST, "message must be a string")),
    };
    // the fields of a GET answer, so that one can be sent back
    for k in ["name", "dataset", "kind", "version"] {
        o.remove(k);
    }
    let def: Definition = serde_json::from_value(j)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid definition: {e}")))?;
    if let Some(m) =
        super::commit_message_header(&headers).map_err(|e| err(StatusCode::BAD_REQUEST, e))?
        && message.is_none()
    {
        message = Some(m.to_string());
    }
    if let Some(m) = &message
        && (m.len() > 1024 || m.chars().any(char::is_control))
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "message must be at most 1024 bytes without control characters",
        ));
    }
    let if_version = precondition(&headers, ds.queries.get(&name, None).is_some())?;
    let author = (!p.name.is_empty()).then(|| p.name.to_string());
    blocking(move || {
        let change = Change {
            author,
            message: message
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
            dataset_commit: Some(ds.store.snapshot().commit),
            if_version,
        };
        let saved = ds.queries.put(&name, def, change)?;
        let status = if saved.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        let mut body = stored_json(&ds, &name, &saved.stored);
        body["changed"] = saved.changed.into();
        let mut r = (status, Json(body)).into_response();
        r.headers_mut()
            .insert(header::ETAG, etag(saved.stored.version.version));
        Ok(r)
    })
    .await
}

/// `DELETE /$/queries/{ds}/{name}`
pub(super) async fn delete_query(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let exists = ds.queries.get(&name, None).is_some();
    let if_version = precondition(&headers, exists)?.filter(|v| *v > 0);
    blocking(move || {
        if ds.queries.delete(&name, if_version)? {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(not_found(&ds.name, &name))
        }
    })
    .await
}

/// The parameter values of a run: the query string, a form body or a JSON object body,
/// without the request parameters of `/{ds}/sparql`. `$name` is the same as `name`.
fn values(params: &Params, json_body: Option<J>) -> ApiResult<BTreeMap<String, J>> {
    let mut out = BTreeMap::new();
    let mut add = |k: &str, v: J| -> ApiResult<()> {
        let k = k.strip_prefix('$').unwrap_or(k);
        if out.insert(k.to_string(), v).is_some() {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("parameter '{k}' is given more than once"),
            ));
        }
        Ok(())
    };
    for (k, v) in &params.0 {
        if !RESERVED.contains(&k.as_str()) {
            add(k, J::String(v.clone()))?;
        }
    }
    if let Some(body) = json_body {
        let J::Object(o) = body else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "a JSON body must be an object of parameter values",
            ));
        };
        for (k, v) in o {
            if !RESERVED.contains(&k.as_str()) {
                add(&k, v)?;
            }
        }
    }
    Ok(out)
}

/// `GET` or `POST /{ds}/queries/{name}`: run a stored query.
pub(super) async fn run(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    QueryBody(body): QueryBody,
) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let mut params = Params::from_query(&uri);
    let mut json_body = None;
    if method == Method::POST && !body.is_empty() {
        match content_type(&headers).as_str() {
            "application/x-www-form-urlencoded" => params.extend_form(&body),
            "application/json" => {
                json_body = Some(serde_json::from_slice(&body).map_err(|e| {
                    err(StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}"))
                })?);
            }
            ct => {
                return Err(err(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    format!(
                        "send parameters as application/x-www-form-urlencoded or application/json, not {ct}"
                    ),
                ));
            }
        }
    }
    let version = version_param(&params)?;
    let stored = ds
        .queries
        .get(&name, version)
        .ok_or_else(|| not_found(&ds.name, &name))?;
    let given = values(&params, json_body)?;
    let mut prefixes = sparkles::io::standard_prefixes();
    prefixes.extend(ds.store.prefixes());
    let bindings = stored.definition.bind(&given, &prefixes)?;
    // the definition's result format, unless the request names one
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*")
        .trim();
    if let Some(f) = &stored.definition.results
        && !params.has("format")
        && (accept.is_empty() || accept == "*/*")
    {
        params.0.push(("format".into(), f.clone()));
    }
    let v = stored.version.version;
    crate::obs::log_query_text(&stored.definition.query);
    let r: Response = super::run_query(
        st.0.clone(),
        ds,
        p,
        uri,
        headers,
        params,
        stored.definition.query,
        bindings,
    )
    .await?;
    let mut r = r;
    if let Ok(h) = HeaderValue::from_str(&v.to_string()) {
        r.headers_mut().insert(SPARKLES_QUERY_VERSION, h);
    }
    Ok(r)
}

#[cfg(test)]
mod tests;
