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
//! Authorization: read-only runs need `read` through the query endpoint; write-back
//! runs need `write` through the update endpoint. Body-aware admission precedes the
//! restore guard and uses the corresponding Query or Update HTTP limits. Reading the
//! projections needs `read` and changing them `admin`.

use super::{AdminBody, ApiResult, Params, St, blocking, dataset, err};
use crate::auth::{Endpoint, Level, Principal};
use axum::Extension;
use axum::Json;
use axum::extract::{FromRequestParts, MatchedPath, Request};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value as J, json};
use sparkles::algo::catalog::{ProjectionDef, Stored};
use sparkles::sparql::algorun::AlgoRequest;
use std::sync::Arc;

#[cfg(test)]
pub(super) mod probe;
pub(super) mod view;

/// The kind of the task of a run that writes its results.
pub const TASK: &str = "algo-run";

/// Private admission result. The original request parts carry this to the handler;
/// the JSON body is parsed once, inside both request decompression limits.
#[derive(Clone)]
pub(super) struct PreparedAlgoRun {
    dataset: String,
    request: Arc<AlgoRequest>,
    /// the view-only members `nodes`, `keep`, `async` and `top` (spec U10 §3.2)
    view: Option<view::ViewOpts>,
    #[cfg(test)]
    probe: probe::Probe,
}

/// Body-aware authorization and class admission must wrap the restoring guard:
/// otherwise an endpoint-denied caller could learn its task ID before authorization.
pub(super) async fn admit_run(State(st): St, req: Request, next: Next) -> Response {
    if req.method() != axum::http::Method::POST
        || req
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str)
            != Some("/$/algo/{ds}/run")
    {
        return next.run(req).await;
    }
    let (mut parts, body) = req.into_parts();
    let Path(name) = match Path::<String>::from_request_parts(&mut parts, &st).await {
        Ok(path) => path,
        Err(error) => return error.into_response(),
    };
    let Some(p) = parts.extensions.get::<Principal>() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing algorithm caller",
        )
        .into_response();
    };
    // Authorize before the body is read: a caller that can neither read the dataset
    // through the query endpoint nor write it through the update endpoint can run
    // nothing here, and does not get the body buffered and parsed.
    #[cfg(test)]
    probe::event(&parts.extensions, "pre-grant");
    if let Some(denied) =
        crate::auth::dataset_denial(&st, p, &parts.headers, &name, Level::Read, Endpoint::Query)
        && crate::auth::dataset_denial(
            &st,
            p,
            &parts.headers,
            &name,
            Level::Write,
            Endpoint::Update,
        )
        .is_some()
    {
        return denied;
    }
    let body =
        match super::read_body(body, st.limits.max_admin_body_bytes, "--max-admin-body-mb").await {
            Ok(body) => body,
            Err(error) => return error.into_response(),
        };
    #[cfg(test)]
    probe::event(&parts.extensions, "parse");
    let parsed = serde_json::from_slice::<J>(&body)
        .map_err(|e| bad(format!("invalid JSON: {e}")))
        .and_then(|mut j| {
            let view = view::ViewOpts::split(&mut j)?;
            let request = AlgoRequest::from_json(j)?;
            if view.is_some() && request.write.is_some() {
                return Err(bad(
                    "nodes, keep, async and top apply only to a run without a write part",
                ));
            }
            Ok((request, view))
        });
    let (request, view) = match parsed {
        Ok((request, view)) => (Arc::new(request), view),
        Err(error) => return error.into_response(),
    };
    let (level, endpoint, class) = if request.write.is_some() {
        (
            Level::Write,
            Endpoint::Update,
            crate::ratelimit::Class::Update,
        )
    } else {
        (Level::Read, Endpoint::Query, crate::ratelimit::Class::Query)
    };
    #[cfg(test)]
    probe::event(&parts.extensions, "grant");
    if let Some(denied) =
        crate::auth::dataset_denial(&st, p, &parts.headers, &name, level, endpoint)
    {
        return denied;
    }
    // a run that reads may read a branch; one that writes keeps to main, where its run
    // record and its named projections live
    if request.write.is_some()
        && super::branches::current().is_some_and(|b| b != sparkles::branch::MAIN)
    {
        return bad("a run that writes its results runs on main; leave out the branch")
            .into_response();
    }
    if request.write.is_some() && st.read_only {
        return err(StatusCode::FORBIDDEN, "server is read-only").into_response();
    }
    let id = parts
        .headers
        .get(&crate::obs::X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let turtle = parts
        .headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/turtle"));
    let prepared = PreparedAlgoRun {
        dataset: name.clone(),
        request,
        view,
        #[cfg(test)]
        probe: parts
            .extensions
            .get::<probe::Probe>()
            .cloned()
            .unwrap_or_default(),
    };
    parts.extensions.insert(prepared);
    let req = Request::from_parts(parts, axum::body::Body::empty());
    // A read run is a query and waits for its initial grant of the memory pool as one
    // does. A write-back run takes its grant when its task starts (see `run`).
    let query = matches!(class, crate::ratelimit::Class::Query);
    let state = st.clone();
    let admitted = |req: Request| async move {
        if query {
            crate::server_memory::granted(&state, req, next).await
        } else {
            next.run(req).await
        }
    };
    match &st.rate_limit {
        Some(rl) => {
            crate::ratelimit::limit_as(rl, class, Some(name), req, |req| async move {
                let mut resp = admitted(req).await;
                // Outer JSON/Turtle formatting must not replace PermitBody after
                // admission, including when only the raw Body remains with a client.
                super::finalize_error_response(&mut resp, id, turtle);
                resp
            })
            .await
        }
        None => admitted(req).await,
    }
}

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
    prepared: Option<Extension<PreparedAlgoRun>>,
) -> ApiResult {
    let Some(Extension(prepared)) = prepared.filter(|p| p.0.dataset == name) else {
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing or mismatched algorithm admission",
        ));
    };
    let req = prepared.request;
    #[cfg(test)]
    prepared.probe.events.lock().push("lookup");
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    if let Some(v) = prepared.view {
        return view::run(State(st), ds, p, params, req, v).await;
    }
    if req.write.is_none() {
        let summary = params
            .get("summary")
            .is_some_and(|v| matches!(v, "" | "true" | "1"));
        #[cfg(test)]
        prepared.probe.events.lock().push("projection");
        let query = ds.dataset.algorithms().query_text(&req, summary)?;
        let run = super::run_query(st, ds, p, uri, headers, params, query, Vec::new());
        #[cfg(test)]
        return probe::CURRENT.scope(prepared.probe, run).await;
        #[cfg(not(test))]
        return run.await;
    }
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    super::task_start_check(&st, Some(TASK), &ds.name)?;
    let mut opts = super::query_options(&st, &ds, &params);
    // A write-back run is an update, so it has the update deadline: none unless the
    // request or `--update-timeout` gives one, and never above `--max-timeout`. The
    // server's query timeout does not apply.
    opts.timeout = super::update_timeout(&st, &params);
    crate::auth::restrict(&mut opts, &p, &ds.name, Endpoint::Update);
    // The run takes its initial grant of the memory pool when its task starts, waiting
    // in arrival order up to `--query-admission-wait` as a request does, so that a run
    // queued behind others holds no memory while it waits.
    opts.grant = None;
    let admission = st.catalog.pool().map(|pool| {
        (
            pool.clone(),
            crate::server_memory::initial_grant(&st, pool),
            st.admission.wait,
        )
    });
    let author = super::author(&p);
    #[cfg(test)]
    prepared.probe.events.lock().push("task-id");
    let id = st.next_task_id();
    let key = ds.name.clone();
    let d = ds.clone();
    let task = st.start_task_opts(id, TASK, &key, None, true, move |h| {
        let mut opts = opts;
        if let Some((pool, want, wait)) = admission {
            let control = h.control();
            let grant = pool.admit_blocking(want, wait, || control.cancel.is_cancelled())?;
            opts.grant = Some(Arc::new(grant));
        }
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

/// `GET /$/algo/{ds}/projections`. The projection cache is shared by every dataset,
/// so only a server administrator sees its size.
pub(super) async fn list(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let projections: Vec<J> = ds
        .dataset
        .algorithms()
        .projections()
        .list()
        .iter()
        .map(|(n, s)| stored_json(n, s))
        .collect();
    let mut out = json!({
        "dataset": ds.name,
        "projections": projections,
    });
    if p.has(crate::auth::ServerPerm::ServerAdmin) {
        let (bytes, entries) = sparkles::algo::cache::stats();
        out["cache"] = json!({ "bytes": bytes, "entries": entries });
    }
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
