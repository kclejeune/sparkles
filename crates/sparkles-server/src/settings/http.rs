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
//!
//! The server-wide kinds of §11.3 have the same four methods at
//! `/$/server/settings/{kind}` (server `admin`), through [`super::server::write`], and
//! the runtime secrets of §11.2 are at `/$/server/secrets` ([`super::secrets`]).

use super::merge::{
    at, diff, forbidden_member, merged, parse_path, path_string, prune, remove_at, set_at,
};
use super::{
    KINDS, Kind, Providers, Resolved, SERVER_KINDS, kind, resolve, runtime, server_kind,
    store_runtime,
};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, blocking, dataset, err, err_body, err_code};
use crate::state::{AppState, Dataset};
use axum::Extension;
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
        .route(
            "/$/server/settings/{kind}",
            get(get_server)
                .put(put_server)
                .patch(patch_server)
                .delete(delete_server),
        )
        .merge(super::secrets::routes())
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

pub(crate) fn bad(msg: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-settings", msg)
}

/// The body of a write as a JSON object. Only the model configuration (`models`) may
/// name endpoints and key references.
fn body_object(body: &[u8], kind: &Kind) -> ApiResult<Value> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| bad(format!("the body is not JSON: {e}")))?;
    if !v.is_object() {
        return Err(bad("the body must be a JSON object"));
    }
    if kind.scope == super::Scope::Dataset
        && let Some(f) = forbidden_member(&v)
    {
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
        Op::Patch(p) => {
            let mut new = merged(&cur.runtime, &p);
            // `null` for a member of a removable map that the declared layers define
            // removes it: the runtime layer keeps the `null`
            for m in kind.removable {
                let Some(Value::Object(members)) = p.get(*m) else {
                    continue;
                };
                for (name, v) in members {
                    let path = [m.to_string(), name.clone()];
                    if v.is_null() && at(&cur.base, &path).is_some() {
                        set_at(&mut new, &path, Value::Null);
                    }
                }
            }
            new
        }
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
    // compared with defaults filled in, so restating a value the settings file
    // implies (one it leaves out) is not a change
    let norm = |v: Value| (kind.normalize)(&v).unwrap_or(v);
    let base = norm(cur.base.clone());
    let before = norm(merged(&cur.base, &cur.runtime));
    let after = norm(merged(&cur.base, &new));
    let mut refused = Vec::new();
    for l in &cur.locked {
        if at(&after, l) == at(&base, l) {
            remove_at(&mut new, l);
            continue;
        }
        if at(&after, l) == at(&before, l) {
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

fn server_kind_of(name: &str) -> ApiResult<&'static Kind> {
    server_kind(name).ok_or_else(|| {
        err_code(
            StatusCode::NOT_FOUND,
            "unknown-kind",
            format!(
                "no server settings kind {name:?}; the kinds are {}",
                SERVER_KINDS.map(|k| k.name).join(", ")
            ),
        )
    })
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
    refuse_read_only(&st)?;
    let if_match = if_match(headers);
    let op = op_of(w, kind)?;
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

/// A settings write on a read-only server (`serve --read-only`) is refused as other
/// admin writes are.
pub(crate) fn refuse_read_only(st: &AppState) -> ApiResult<()> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    Ok(())
}

/// The `If-Match` value of a write.
pub(crate) fn if_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// A write with its body read.
pub(crate) fn op_of(w: Write, kind: &Kind) -> ApiResult<Op> {
    Ok(match w {
        Write::Put(b) => Op::Put(body_object(&b, kind)?),
        Write::Patch(b) => Op::Patch(body_object(&b, kind)?),
        Write::Delete(None) => Op::Delete(None),
        Write::Delete(Some(f)) => Op::Delete(Some(parse_path(&f).ok_or_else(|| {
            bad(format!(
                "{f:?} is not a field such as send or budget.perRequest"
            ))
        })?)),
    })
}

fn with_etag(r: &Resolved, dataset: &str) -> Response {
    tagged(Json(r.json(dataset)).into_response(), &r.etag)
}

fn tagged(mut resp: Response, etag: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(etag) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp
}

fn with_etag_server(r: &Resolved) -> Response {
    tagged(Json(r.json_server()).into_response(), &r.etag)
}

/// The principal's log name, for the audit records.
pub(crate) fn who(p: Option<Extension<Principal>>) -> String {
    p.map_or_else(|| "local".into(), |Extension(p)| p.id())
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

// ------------------------------------------------------ server-wide kinds (§11.3) ------

async fn get_server(State(st): St, Path(kind): Path<String>) -> ApiResult<Response> {
    let kind = server_kind_of(&kind)?;
    blocking(move || Ok(with_etag_server(&super::server::resolved_kind(&st, kind)))).await
}

async fn put_server(
    State(st): St,
    Path(kind): Path<String>,
    p: Option<Extension<Principal>>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    let kind = server_kind_of(&kind)?;
    let r = super::server::write(st, kind, Write::Put(body), &headers, who(p)).await?;
    Ok(with_etag_server(&r))
}

async fn patch_server(
    State(st): St,
    Path(kind): Path<String>,
    p: Option<Extension<Principal>>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    let kind = server_kind_of(&kind)?;
    let r = super::server::write(st, kind, Write::Patch(body), &headers, who(p)).await?;
    Ok(with_etag_server(&r))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerFieldParam {
    field: Option<String>,
}

async fn delete_server(
    State(st): St,
    Path(kind): Path<String>,
    p: Option<Extension<Principal>>,
    headers: HeaderMap,
    Query(q): Query<ServerFieldParam>,
) -> ApiResult<Response> {
    let kind = server_kind_of(&kind)?;
    let r = super::server::write(st, kind, Write::Delete(q.field), &headers, who(p)).await?;
    Ok(with_etag_server(&r))
}
