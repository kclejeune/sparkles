//! The routes of spec C19 §6.
//!
//! - `GET /$/settings` reports the settings file, its reads and the declared names that
//!   match no dataset (server `admin`).
//! - `GET /$/settings/{ds}` answers every kind of a dataset, and
//!   `GET /$/settings/{ds}/{kind}` one kind with its layers, sources and locks and an
//!   `ETag` of its runtime layer (`read`).
//! - `PATCH`, `PUT` and `DELETE /$/settings/{ds}/{kind}` change the runtime layer
//!   (`admin`). They take `If-Match`, and writes to one kind of one dataset are
//!   serialized.
//!
//! The older routes `PUT /$/assistant/{ds}`, `PUT /$/memory/{ds}` and
//! `PUT /$/ingest/{ds}/settings` go through [`write`] too.

use super::merge::{
    at, diff, forbidden_member, merged, parse_path, path_string, prune, remove_at, set_at,
};
use super::{KINDS, Kind, Providers, Resolved, kind, resolve, runtime, store_runtime};
use crate::http::{AdminBody, ApiResult, blocking, dataset, err, err_body, err_code};
use crate::state::{AppState, Dataset};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/settings", get(status))
        .route("/$/settings/{ds}", get(get_all))
        .route(
            "/$/settings/{ds}/{kind}",
            get(get_kind)
                .put(put_kind)
                .patch(patch_kind)
                .delete(delete_kind),
        )
}

/// A change to a kind's runtime layer.
pub enum Write {
    /// make the effective object equal to the body
    Put(Bytes),
    /// merge the body into the runtime layer
    Patch(Bytes),
    /// clear the runtime layer, or one field of it
    Delete(Option<String>),
}

/// A change to a runtime layer, its body read.
pub(crate) enum Op {
    Put(Value),
    Patch(Value),
    Delete(Option<Vec<String>>),
}

fn bad(msg: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-settings", msg)
}

fn body_object(body: &[u8]) -> ApiResult<Value> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| bad(format!("the body is not JSON: {e}")))?;
    if !v.is_object() {
        return Err(bad("the body must be a JSON object"));
    }
    if let Some(f) = forbidden_member(&v) {
        return Err(bad(format!(
            "{f} is not allowed: providers are defined in the server's model configuration only"
        )));
    }
    Ok(v)
}

/// Whether an `If-Match` value names `etag`.
fn etag_matches(if_match: &str, etag: &str) -> bool {
    if_match
        .split(',')
        .map(str::trim)
        .any(|t| t == "*" || t.trim_start_matches("W/") == etag)
}

/// The runtime layer that `op` makes of `cur`'s (§6): `If-Match` checked, a `PUT`
/// turned into the fields where the body differs from the declared and default layers,
/// and the locks of §4.1 applied. A write that leaves a locked field alone or restates
/// its value stores nothing for it, and any other is refused with `locked-by-config`.
/// `owner` names the dataset, or the server, in the messages. The caller checks the
/// effective object after the change and stores the layer.
pub(crate) fn plan(
    cur: &Resolved,
    op: Op,
    if_match: Option<&str>,
    owner: &str,
) -> ApiResult<Value> {
    let kind = cur.kind;
    if let Some(m) = if_match
        && !etag_matches(m, &cur.etag)
    {
        return Err(err_code(
            StatusCode::PRECONDITION_FAILED,
            "precondition-failed",
            format!(
                "the {} settings of {owner} changed since {m}; read them again",
                kind.name
            ),
        ));
    }
    let mut new = match op {
        Op::Patch(p) => merged(&cur.runtime, &p),
        Op::Put(mut body) => {
            if kind.name == "assistant"
                && let Some(m) = body.as_object_mut()
            {
                // `GET /$/assistant/{ds}` adds it
                m.remove("status");
            }
            // a locked field left out keeps its value
            for l in &cur.locked {
                if at(&body, l).is_none()
                    && let Some(v) = at(&cur.effective, l)
                {
                    set_at(&mut body, l, v.clone());
                }
            }
            let target = (kind.normalize)(&body).map_err(bad)?;
            let base = (kind.normalize)(&cur.base).unwrap_or_else(|_| cur.base.clone());
            diff(&base, &target).unwrap_or_else(|| Value::Object(Map::new()))
        }
        Op::Delete(None) => Value::Object(Map::new()),
        Op::Delete(Some(f)) => {
            let mut v = cur.runtime.clone();
            remove_at(&mut v, &f);
            v
        }
    };
    prune(&mut new);
    // a locked field: a write that leaves its value alone or restates it stores
    // nothing for it, and any other is refused (§4.1)
    let before = merged(&cur.base, &cur.runtime);
    let after = merged(&cur.base, &new);
    let mut refused = Vec::new();
    for l in &cur.locked {
        if at(&after, l) == at(&before, l) {
            continue;
        }
        if at(&after, l) == at(&cur.base, l) {
            remove_at(&mut new, l);
            continue;
        }
        refused.push(path_string(l));
    }
    if !refused.is_empty() {
        return Err(err_body(
            StatusCode::CONFLICT,
            json!({
                "error": format!(
                    "the settings file locks {} of {} for {owner}",
                    refused.join(", "),
                    kind.name
                ),
                "code": "locked-by-config",
                "fields": refused,
            }),
        ));
    }
    Ok(new)
}

fn kind_of(name: &str) -> ApiResult<&'static Kind> {
    kind(name).ok_or_else(|| {
        err_code(
            StatusCode::NOT_FOUND,
            "unknown-kind",
            format!(
                "no settings kind {name:?}; the kinds are {}",
                KINDS.map(|k| k.name).join(", ")
            ),
        )
    })
}

/// Apply `w` to the runtime layer of `kind` for `ds` (§6): check `If-Match`, the locks
/// and the effective object after the change, then store the layer. The answer is the
/// kind after the change.
pub async fn write(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    kind: &'static Kind,
    w: Write,
    headers: &HeaderMap,
) -> ApiResult<Resolved> {
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let op = match w {
        Write::Put(b) => Op::Put(body_object(&b)?),
        Write::Patch(b) => Op::Patch(body_object(&b)?),
        Write::Delete(None) => Op::Delete(None),
        Write::Delete(Some(f)) => Op::Delete(Some(parse_path(&f).ok_or_else(|| {
            bad(format!(
                "{f:?} is not a field such as send or budget.perRequest"
            ))
        })?)),
    };
    blocking(move || {
        let internal = |e: anyhow::Error| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"));
        let lock = st.settings.write_lock(&ds.name, kind);
        let _g = lock.lock();
        let models = st.models();
        let providers = Providers::Checked(models.as_deref());
        let declared = st.settings.declared();
        let rt = runtime(&st, &ds, kind).map_err(internal)?;
        let cur = resolve(kind, &declared, &ds.name, rt, providers);
        let owner = format!("/{}", ds.name);
        let new = plan(&cur, op, if_match.as_deref(), &owner)?;
        let r = resolve(kind, &declared, &ds.name, new, providers);
        if let Err(e) = &r.status {
            return Err(bad(format!("{}: {e}", kind.name)));
        }
        if r.runtime != cur.runtime {
            store_runtime(&st, &ds, kind, &r.runtime).map_err(internal)?;
        }
        Ok(r)
    })
    .await
}

fn with_etag(r: &Resolved, dataset: &str) -> Response {
    let mut resp = Json(r.json(dataset)).into_response();
    if let Ok(v) = HeaderValue::from_str(&r.etag) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp
}

async fn status(State(st): St) -> Json<Value> {
    Json(st.settings.status_json(&st))
}

async fn get_all(State(st): St, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let kinds: Map<String, Value> = KINDS
            .iter()
            .map(|k| {
                (
                    k.name.to_string(),
                    super::resolved(&st, &ds, k).json(&ds.name),
                )
            })
            .collect();
        Ok(Json(json!({ "dataset": ds.name, "kinds": kinds })))
    })
    .await
}

async fn get_kind(
    State(st): St,
    Path((name, kind)): Path<(String, String)>,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let kind = kind_of(&kind)?;
    blocking(move || {
        let rt = runtime(&st, &ds, kind)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        let models = st.models();
        let r = resolve(
            kind,
            &st.settings.declared(),
            &ds.name,
            rt,
            Providers::Checked(models.as_deref()),
        );
        Ok(with_etag(&r, &ds.name))
    })
    .await
}

async fn put_kind(
    State(st): St,
    Path((name, kind)): Path<(String, String)>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let kind = kind_of(&kind)?;
    let r = write(st, ds.clone(), kind, Write::Put(body), &headers).await?;
    Ok(with_etag(&r, &ds.name))
}

async fn patch_kind(
    State(st): St,
    Path((name, kind)): Path<(String, String)>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let kind = kind_of(&kind)?;
    let r = write(st, ds.clone(), kind, Write::Patch(body), &headers).await?;
    Ok(with_etag(&r, &ds.name))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldParam {
    field: Option<String>,
    /// `?branch=` is read by the branch layer
    #[allow(dead_code)]
    branch: Option<String>,
}

async fn delete_kind(
    State(st): St,
    Path((name, kind)): Path<(String, String)>,
    headers: HeaderMap,
    Query(q): Query<FieldParam>,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let kind = kind_of(&kind)?;
    let r = write(st, ds.clone(), kind, Write::Delete(q.field), &headers).await?;
    Ok(with_etag(&r, &ds.name))
}
