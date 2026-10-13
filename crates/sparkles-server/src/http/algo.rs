//! Graph algorithms (F14): `POST /$/algo/{ds}/run` runs an algorithm on a projection of
//! the dataset, and `/$/algo/{ds}/projections[/{name}]` manages the named projections
//! that `algo:projection "name"` refers to.
//!
//! A run without a `write` part is the query its request stands for, answered like any
//! query (the `Accept` header and the request parameters of `/{ds}/sparql` apply, and
//! `summary=true` answers only with the statistics of the run). A run with a `write`
//! part computes at the head commit and writes its results in batched commits as a
//! cancellable task (`202`, then `GET /$/tasks/{id}` and `DELETE /$/tasks/{id}`), with
//! the run record in the output graph.
//!
//! Authorization: a run needs `read` on `{ds}` and, with a `write` part, `write` through
//! the update endpoint. Reading the projections needs `read` and changing them `admin`.

use super::{AdminBody, ApiResult, Params, St, blocking, dataset, err};
use crate::auth::{Endpoint, Level, Principal};
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value as J, json};
use sparkles::algo::catalog::{ProjectionDef, Stored};
use sparkles::sparql::algorun::AlgoRequest;

/// The kind of the task of a run that writes its results.
pub const TASK: &str = "algo-run";

fn bad(m: impl Into<String>) -> super::ApiError {
    err(StatusCode::BAD_REQUEST, m)
}

fn etag(revision: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"r{revision}\"")).unwrap_or(HeaderValue::from_static("\"r0\""))
}

fn stored_json(name: &str, s: &Stored) -> J {
    let mut j = serde_json::to_value(s).unwrap_or_default();
    if let Some(o) = j.as_object_mut() {
        o.insert("name".into(), name.into());
    }
    j
}

/// `POST /$/algo/{ds}/run`
pub(super) async fn run(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let j: J = serde_json::from_slice(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
    let req = AlgoRequest::from_json(j)?;
    let params = Params::from_query(&uri);
    if req.write.is_none() {
        let summary = params
            .get("summary")
            .is_some_and(|v| matches!(v, "" | "true" | "1"));
        let query = ds.dataset.algorithms().query_text(&req, summary)?;
        return super::run_query(st, ds, p, uri, headers, params, query, Vec::new()).await;
    }
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if let Some(denied) =
        crate::auth::dataset_denial(&st, &p, &headers, &name, Level::Write, Endpoint::Update)
    {
        return Ok(denied);
    }
    super::task_start_check(&st, Some(TASK), &ds.name)?;
    let mut opts = super::query_options(&st, &ds, &params);
    crate::auth::restrict(&mut opts, &p, &ds.name, Endpoint::Update);
    let author = super::author(&p);
    let id = st.next_task_id();
    let key = ds.name.clone();
    let d = ds.clone();
    let task = st.start_task_opts(id, TASK, &key, None, true, move |h| {
        let rep = d
            .dataset
            .algorithms()
            .run_write(&req, &opts, author, &h.control())?;
        h.set_detail(serde_json::to_value(&rep)?);
        Ok(format!(
            "{}: {} triples for {} rows in {} commits at commit {}",
            rep.algorithm, rep.triples, rep.rows, rep.commits, rep.source_commit
        ))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// `GET /$/algo/{ds}/projections`
pub(super) async fn list(st: St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let projections: Vec<J> = ds
        .dataset
        .algorithms()
        .projections()
        .list()
        .iter()
        .map(|(n, s)| stored_json(n, s))
        .collect();
    let (bytes, entries) = sparkles::algo::cache::stats();
    let mut out = json!({
        "dataset": ds.name,
        "projections": projections,
        "cache": { "bytes": bytes, "entries": entries },
    });
    if let Some(e) = ds.dataset.algorithms().projections().error() {
        out["warning"] = e.into();
    }
    Ok(Json(out))
}

/// `GET /$/algo/{ds}/projections/{name}`
pub(super) async fn get_one(st: St, Path((ds_name, name)): Path<(String, String)>) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let s = ds
        .dataset
        .algorithms()
        .projections()
        .get(&name)
        .ok_or_else(|| not_found(&ds.name, &name))?;
    let mut r = Json(stored_json(&name, &s)).into_response();
    r.headers_mut().insert(header::ETAG, etag(s.revision));
    Ok(r)
}

fn not_found(ds: &str, name: &str) -> super::ApiError {
    err(
        StatusCode::NOT_FOUND,
        format!("no projection '{name}' in /{ds}"),
    )
}

/// The revision an `If-Match` or `If-None-Match: *` header requires (`Some(0)`: none).
fn precondition(headers: &HeaderMap, exists: bool) -> ApiResult<Option<u64>> {
    let text = |h| headers.get(h).and_then(|v: &HeaderValue| v.to_str().ok());
    if let Some(m) = text(header::IF_NONE_MATCH) {
        return if m.trim() == "*" {
            Ok(Some(0))
        } else {
            Err(bad("If-None-Match takes * only here"))
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
                "the projection does not exist",
            ))
        };
    }
    m.trim_start_matches("W/")
        .trim_matches('"')
        .strip_prefix('r')
        .and_then(|v| v.parse::<u64>().ok())
        .map(Some)
        .ok_or_else(|| bad("If-Match must be a revision tag such as \"r3\""))
}

/// `PUT /$/algo/{ds}/projections/{name}`: a definition, or the answer of a `GET` with
/// its `definition` member.
pub(super) async fn put_one(
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
    let mut j: J = serde_json::from_slice(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
    if !j.is_object() {
        return Err(bad("the body must be a JSON object"));
    }
    if let Some(d) = j.get_mut("definition").map(J::take) {
        j = d;
    }
    let def = ProjectionDef::from_json(j)?;
    let exists = ds.dataset.algorithms().projections().get(&name).is_some();
    let if_revision = precondition(&headers, exists)?;
    let author = (!p.name.is_empty()).then(|| p.name.to_string());
    blocking(move || {
        let (stored, created) =
            ds.dataset
                .algorithms()
                .projections()
                .put(&name, def, author, if_revision)?;
        let status = if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        let mut r = (status, Json(stored_json(&name, &stored))).into_response();
        r.headers_mut().insert(header::ETAG, etag(stored.revision));
        Ok::<Response, _>(r)
    })
    .await
}

/// `DELETE /$/algo/{ds}/projections/{name}`
pub(super) async fn delete_one(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let exists = ds.dataset.algorithms().projections().get(&name).is_some();
    let if_revision = precondition(&headers, exists)?.filter(|v| *v > 0);
    blocking(move || {
        if ds
            .dataset
            .algorithms()
            .projections()
            .delete(&name, if_revision)?
        {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(not_found(&ds.name, &name))
        }
    })
    .await
}
