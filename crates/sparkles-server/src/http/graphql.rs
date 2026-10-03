//! The GraphQL endpoint (C03): `GET` and `POST /{ds}/graphql` run a document against the
//! installed schema, `/{ds}/graphql/schema` returns the API schema, and
//! `/$/graphql/{ds}[/versions|/draft]` manages the configuration in `graphql.json`.
//! Every fetch group runs with the query options `/{ds}/sparql` would build for the
//! caller, so graph views, protections of triples, budgets and the result cache apply.

use super::{
    AdminBody, ApiError, ApiResult, Params, QueryBody, St, blocking, budgets, cancel_on_drop,
    content_type, dataset, err, history, query_options, with_commit,
};
use crate::auth::{Endpoint, Level, Principal};
use crate::graphql::Backing;
use crate::obs::{GraphqlReport, Op, Outcome, RequestReport};
use crate::state::{AppState, Dataset};
use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use parking_lot::Mutex;
use serde_json::{Map, Value as J, json};
use sparkles::history::{At, Resolved};
use sparkles_graphql::{Change, Code, Compiled, Config, GqlError, Request};
use std::sync::Arc;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/{ds}/graphql", get(run).post(run))
        .route("/{ds}/graphql/schema", get(api_schema))
        .route(
            "/$/graphql/{ds}",
            get(get_config).put(put_config).delete(delete_config),
        )
        .route("/$/graphql/{ds}/versions", get(versions))
        .route("/$/graphql/{ds}/draft", get(draft_schema))
}

pub const GRAPHQL_RESPONSE: &str = "application/graphql-response+json";

/// A response whose body is a GraphQL response with one error.
fn gql_error(status: StatusCode, ct: &'static str, e: GqlError) -> Response {
    let mut r = (
        status,
        [(header::CONTENT_TYPE, ct)],
        Json(json!({ "errors": [e.to_json()] })),
    )
        .into_response();
    let report = RequestReport {
        operation: Some(Op::Graphql),
        ..Default::default()
    };
    r = report.attach(r);
    r
}

fn not_installed(ds: &str) -> GqlError {
    GqlError::new(
        Code::BadRequest,
        format!(
            "no GraphQL schema is installed for /{ds}; draft one with GET /$/graphql/{ds}/draft and install it with PUT /$/graphql/{ds}"
        ),
    )
}

/// The installed schema, compiled.
fn compiled(ds: &Dataset) -> Result<Option<Arc<Compiled>>, ApiError> {
    ds.graphql
        .compiled()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// The response media type: `application/graphql-response+json` when the client
/// accepts it, else `application/json`.
fn media_type(headers: &HeaderMap) -> &'static str {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let wants = accept.split(',').any(|part| {
        let mut it = part.split(';');
        let mt = it.next().unwrap_or("").trim();
        let q0 = it.any(|p| {
            p.trim()
                .strip_prefix("q=")
                .is_some_and(|q| q.trim().parse::<f32>().is_ok_and(|q| q == 0.0))
        });
        mt.eq_ignore_ascii_case(GRAPHQL_RESPONSE) && !q0
    });
    if wants {
        GRAPHQL_RESPONSE
    } else {
        "application/json"
    }
}

fn bad_request(msg: impl Into<String>) -> GqlError {
    GqlError::new(Code::BadRequest, msg)
}

/// The GraphQL request of an HTTP request (GraphQL over HTTP): the query string of a
/// `GET`, or a JSON body (`application/json`) or a document body (`application/graphql`)
/// of a `POST`.
fn request(
    method: &Method,
    params: &Params,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Request, (StatusCode, GqlError)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, bad_request(m));
    let json_param = |k: &str| -> Result<Option<J>, (StatusCode, GqlError)> {
        match params.get(k) {
            None | Some("") => Ok(None),
            Some(s) => serde_json::from_str(s)
                .map(Some)
                .map_err(|e| bad(format!("{k} is not JSON: {e}"))),
        }
    };
    let variables = |v: Option<J>| -> Result<Map<String, J>, (StatusCode, GqlError)> {
        match v {
            None | Some(J::Null) => Ok(Map::new()),
            Some(J::Object(o)) => Ok(o),
            Some(_) => Err(bad("variables must be a JSON object".into())),
        }
    };
    if *method == Method::GET || *method == Method::HEAD || body.is_empty() && params.has("query") {
        let query = params
            .get("query")
            .ok_or_else(|| bad("missing 'query' parameter".into()))?
            .to_string();
        return Ok(Request {
            query,
            operation_name: params.get("operationName").map(str::to_string),
            variables: variables(json_param("variables")?)?,
        });
    }
    match content_type(headers).as_str() {
        "application/json" => {
            let v: J = serde_json::from_slice(body)
                .map_err(|e| bad(format!("the body is not JSON: {e}")))?;
            let J::Object(mut o) = v else {
                return Err(bad("the body must be a JSON object".into()));
            };
            let query = match o.remove("query") {
                Some(J::String(q)) => q,
                _ => return Err(bad("the body has no 'query' string".into())),
            };
            let operation_name = match o.remove("operationName") {
                None | Some(J::Null) => None,
                Some(J::String(s)) => Some(s),
                Some(_) => return Err(bad("operationName must be a string".into())),
            };
            Ok(Request {
                query,
                operation_name,
                variables: variables(o.remove("variables"))?,
            })
        }
        "application/graphql" => Ok(Request {
            query: String::from_utf8_lossy(body).into_owned(),
            operation_name: params.get("operationName").map(str::to_string),
            variables: variables(json_param("variables")?)?,
        }),
        ct => Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            bad_request(format!(
                "send GraphQL requests as application/json or application/graphql, not {ct}"
            )),
        )),
    }
}

/// `GET` or `POST /{ds}/graphql`.
pub(super) async fn run(
    st: St,
    Path(ds_name): Path<String>,
    Extension(p): Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    QueryBody(body): QueryBody,
) -> ApiResult {
    let ct = media_type(&headers);
    let Ok(ds) = dataset(&st, &ds_name) else {
        return Ok(gql_error(
            StatusCode::NOT_FOUND,
            ct,
            GqlError::new(Code::BadRequest, format!("no such dataset: /{ds_name}")),
        ));
    };
    let Some(c) = compiled(&ds)? else {
        return Ok(gql_error(
            StatusCode::NOT_FOUND,
            ct,
            not_installed(&ds.name),
        ));
    };
    let params = Params::from_query(&uri);
    let req = match request(&method, &params, &headers, &body) {
        Ok(r) => r,
        Err((status, e)) => return Ok(gql_error(status, ct, e)),
    };
    crate::obs::log_query_text(&req.query);
    let mut qopts = query_options(&st, &ds, &params);
    let asked = match budgets::Overrides::parse(&params) {
        Ok(a) => a,
        Err(e) => {
            return Ok(gql_error(
                StatusCode::BAD_REQUEST,
                ct,
                bad_request(api_message(&e)),
            ));
        }
    };
    asked.apply(&mut qopts);
    crate::auth::restrict(&mut qopts, &p, &ds.name, Endpoint::Graphql);
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    qopts.cancel = Some(cancel);
    let at = match history::at_param(&params) {
        Ok(a) => a,
        Err(e) => {
            return Ok(gql_error(
                StatusCode::BAD_REQUEST,
                ct,
                bad_request(api_message(&e)),
            ));
        }
    };
    let opts = sparkles_graphql::Options {
        limits: st.graphql_limits,
        admin: p.can(&ds.name, Level::Admin),
        explain: params.get("explain").is_some_and(super::truthy),
        get: method == Method::GET || method == Method::HEAD,
        at,
        max_result_bytes: asked.result_limit(st.limits.max_result_bytes),
        query: qopts,
    };
    let resolved: Arc<Mutex<Option<Resolved>>> = Arc::new(Mutex::new(None));
    let r = {
        let (ds, resolved) = (ds.clone(), resolved.clone());
        blocking(move || {
            let hist_opts = opts.query.clone();
            let resolve = |at: Option<&At>| {
                let (snap, r) = history::snapshot_for(&ds, at, &hist_opts)?;
                if r.is_some() {
                    *resolved.lock() = r;
                }
                Ok(snap)
            };
            Ok(sparkles_graphql::execute(&c, &req, &opts, &resolve))
        })
        .await?
    };
    let status = StatusCode::from_u16(r.outcome.status(ct == GRAPHQL_RESPONSE))
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = serde_json::to_vec(&r.body).unwrap_or_default();
    let n = bytes.len() as u64;
    let mut resp = (status, [(header::CONTENT_TYPE, ct)], bytes).into_response();
    if let Some(commit) = r.commit {
        resp = with_commit(resp, &ds, commit);
    }
    let resolved = resolved.lock().take();
    resp = history::history_headers(resp, resolved.as_ref(), &uri);
    let outcome = match r.outcome {
        sparkles_graphql::Outcome::Budget => Some(Outcome::Budget),
        sparkles_graphql::Outcome::Timeout => Some(Outcome::Timeout),
        sparkles_graphql::Outcome::Cancelled => Some(Outcome::Cancelled),
        sparkles_graphql::Outcome::Executed => None,
        _ => Some(Outcome::ClientError),
    };
    let budget = r.body["errors"][0]["extensions"]["budget"]
        .as_str()
        .and_then(|b| {
            sparkles::BudgetKind::ALL
                .into_iter()
                .find(|k| k.as_str() == b)
        });
    let report = RequestReport {
        operation: Some(Op::Graphql),
        outcome,
        budget,
        response_bytes: Some(n),
        graphql: Some(GraphqlReport {
            operation: r.operation_name.clone(),
            document: r.doc_hash.clone(),
            groups: r.groups as u64,
        }),
        ..Default::default()
    };
    Ok(report.attach(resp))
}

fn api_message(e: &ApiError) -> String {
    e.1.get("error")
        .and_then(J::as_str)
        .unwrap_or("bad request")
        .to_string()
}

/// `GET /{ds}/graphql/schema`: the API schema as SDL.
pub(super) async fn api_schema(st: St, Path(ds_name): Path<String>) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let c = compiled(&ds)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, not_installed(&ds.name).message))?;
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        c.api_sdl.clone(),
    )
        .into_response())
}

fn etag(v: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"v{v}\"")).unwrap_or(HeaderValue::from_static("\"v0\""))
}

fn stored_json(ds: &Dataset, s: &sparkles_graphql::Stored) -> J {
    let mut j = serde_json::to_value(s).unwrap_or_default();
    if let Some(o) = j.as_object_mut() {
        o.insert("dataset".into(), ds.name.clone().into());
    }
    j
}

/// `GET /$/graphql/{ds}`: the configuration with its version (`?version=N` for a kept
/// one).
pub(super) async fn get_config(st: St, Path(ds_name): Path<String>, uri: Uri) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let params = Params::from_query(&uri);
    let version = params
        .get("version")
        .map(|v| {
            v.trim_start_matches('v')
                .parse::<u64>()
                .map_err(|_| err(StatusCode::BAD_REQUEST, "version must be a version number"))
        })
        .transpose()?;
    let s = ds.graphql.get(version).ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            format!("no GraphQL configuration for /{}", ds.name),
        )
    })?;
    let mut r = Json(stored_json(&ds, &s)).into_response();
    r.headers_mut()
        .insert(header::ETAG, etag(s.version.version));
    Ok(r)
}

/// `GET /$/graphql/{ds}/versions`
pub(super) async fn versions(st: St, Path(ds_name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    Ok(Json(
        json!({ "dataset": ds.name, "versions": ds.graphql.versions() }),
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
                "no GraphQL configuration is installed",
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

/// `PUT /$/graphql/{ds}`: the configuration as JSON (with an optional `message`), or the
/// SDL alone as `application/graphql`, which keeps the other fields.
pub(super) async fn put_config(
    st: St,
    Path(ds_name): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let mut message = None;
    let config: Config = match content_type(&headers).as_str() {
        "application/graphql" | "text/plain" => {
            let sdl = String::from_utf8(body.to_vec())
                .map_err(|_| err(StatusCode::BAD_REQUEST, "the SDL is not UTF-8"))?;
            match ds.graphql.get(None) {
                Some(cur) => Config { sdl, ..cur.config },
                None => Config::new(sdl),
            }
        }
        _ => {
            let mut j: J = serde_json::from_slice(&body)
                .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
            let Some(o) = j.as_object_mut() else {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "the body must be a JSON object",
                ));
            };
            message = match o.remove("message") {
                None | Some(J::Null) => None,
                Some(J::String(m)) => Some(m),
                Some(_) => return Err(err(StatusCode::BAD_REQUEST, "message must be a string")),
            };
            // the fields of a GET answer, so that one can be sent back
            for k in [
                "dataset",
                "version",
                "parent",
                "created",
                "author",
                "datasetCommit",
                "digest",
                "format",
            ] {
                o.remove(k);
            }
            let j = match o.remove("config") {
                Some(c) => c,
                None => j,
            };
            serde_json::from_value(j).map_err(|e| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("invalid configuration: {e}"),
                )
            })?
        }
    };
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
    let if_version = precondition(&headers, ds.graphql.get(None).is_some())?;
    let author = (!p.name.is_empty()).then(|| p.name.to_string());
    blocking(move || {
        let backing = Backing::of(ds.validation.read().clone(), &ds.store, &config.data_graph);
        let change = Change {
            author,
            message: message.map(|m| m.trim().to_string()).filter(|m| !m.is_empty()),
            dataset_commit: Some(ds.store.snapshot().commit),
            if_version,
        };
        let backs = |c: &str, p: &str, i: bool| backing.backs(c, p, i);
        match ds.graphql.put(config, change, &backs) {
            Ok((saved, warnings)) => {
                let status = if saved.created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                };
                let mut body = stored_json(&ds, &saved.stored);
                body["changed"] = saved.changed.into();
                body["warnings"] = warnings.into();
                let mut r = (status, Json(body)).into_response();
                r.headers_mut()
                    .insert(header::ETAG, etag(saved.stored.version.version));
                Ok(r)
            }
            Err(sparkles_graphql::PutError::Engine(e)) => Err(e.into()),
            Err(sparkles_graphql::PutError::Sdl(errs)) => Err(ApiError(
                StatusCode::BAD_REQUEST,
                json!({
                    "error": format!("the mapping schema is invalid: {}", errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")),
                    "errors": errs.iter().map(|e| json!({ "message": e.message, "line": e.line })).collect::<Vec<_>>(),
                }),
            )),
        }
    })
    .await
}

/// `DELETE /$/graphql/{ds}`
pub(super) async fn delete_config(
    st: St,
    Path(ds_name): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let exists = ds.graphql.get(None).is_some();
    let if_version = precondition(&headers, exists)?.filter(|v| *v > 0);
    blocking(move || {
        if ds.graphql.delete(if_version)? {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(err(
                StatusCode::NOT_FOUND,
                format!("no GraphQL configuration for /{}", ds.name),
            ))
        }
    })
    .await
}

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// `GET /$/graphql/{ds}/draft`: a mapping schema drafted from SHACL shapes
/// (`source=shapes`: the guard's, or `shapesGraph`) or from the data
/// (`source=observed`), as SDL, or as JSON with its decisions (`format=json`).
pub(super) async fn draft_schema(
    st: St,
    Path(ds_name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let params = Params::from_query(&uri);
    let json_out = params.get("format") == Some("json")
        || headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("application/json"));
    let graph = params
        .get("graph")
        .map(|v| sparkles::schema::GraphSelection::parse(v).map_err(|e| bad(format!("graph: {e}"))))
        .transpose()?
        .unwrap_or(sparkles::schema::GraphSelection::Default);
    let support = match params.get("support") {
        None => 1.0,
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|s| *s > 0.0 && *s <= 1.0)
            .ok_or_else(|| bad(format!("support must be a number in (0, 1], not '{v}'")))?,
    };
    let mut classes = Vec::new();
    for c in params.all("class") {
        let c = c.trim();
        let iri = c
            .strip_prefix('<')
            .and_then(|i| i.strip_suffix('>'))
            .unwrap_or(c);
        oxrdf::NamedNode::new(iri).map_err(|e| bad(format!("class: invalid IRI '{iri}': {e}")))?;
        classes.push(iri.to_string());
    }
    let min_instances = match params.get("minInstances") {
        None => 1,
        Some(v) => v.parse::<u64>().map_err(|_| {
            bad(format!(
                "minInstances must be a non-negative integer, not '{v}'"
            ))
        })?,
    };
    let req = crate::graphql::DraftRequest {
        source: params.get("source").map(str::to_string),
        shapes_graph: params.get("shapesGraph").map(str::to_string),
        graph,
        support,
        classes,
        min_instances,
        reasoning: params.get("reasoning") == Some("true"),
        timeout: super::timeout_param(&st, &params),
        view: p.view(&ds.name, Endpoint::Info),
        max_entries: st.schema_max_entries,
        inferred_graph: super::INFERRED_GRAPH.to_string(),
    };
    blocking(move || {
        let data_graph = ds
            .graphql
            .get(None)
            .map(|s| s.config.data_graph)
            .unwrap_or_default();
        let v = ds.validation.read().clone();
        let (d, commit) = crate::graphql::draft(&ds.name, &ds.store, v, &data_graph, req)?;
        Ok(if json_out {
            let mut j = serde_json::to_value(&d).unwrap_or_default();
            j["dataset"] = ds.name.clone().into();
            j["commit"] = commit.into();
            Json(j).into_response()
        } else {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], d.sdl).into_response()
        })
    })
    .await
}

#[cfg(test)]
mod tests;
