//! Evidence-profile settings and source holds in the server (spec C25 §3 and §7):
//! `GET` and `PUT /$/evidence/{ds}/settings` read and record a dataset's evidence
//! settings, `POST /$/evidence/{ds}/detach` stops protecting one graph of their roles,
//! and `GET` and `POST /$/evidence/{ds}/holds` and `DELETE
//! /$/evidence/{ds}/holds/{id}` list, acquire and release durable source holds.
//!
//! Authorization (the route table in `auth/routes.rs`): every route needs `admin` on
//! `{ds}`. The settings name the dataset's protected graphs and policies, and holds
//! name commits of any branch and decide what history is retained.

use crate::http::{AdminBody, ApiResult, blocking, dataset, err};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value as J, json};
use sparkles::evidence::SettingsStatus;
use sparkles::task::Control;
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/$/evidence/{ds}/settings",
            get(get_settings).put(put_settings),
        )
        .route("/$/evidence/{ds}/detach", post(detach))
        .route("/$/evidence/{ds}/holds", get(list_holds).post(acquire_hold))
        .route("/$/evidence/{ds}/holds/{id}", delete(release_hold))
}

/// The response body: the canonical settings, their digest, the registry revision and
/// the role graphs that are protected or detached.
fn body(status: SettingsStatus) -> ApiResult<Json<J>> {
    let settings = status.settings.canonical(&Control::none())?;
    Ok(Json(json!({
        "settings": settings.value(),
        "digest": status.digest,
        "revision": status.revision,
        "managed": status.managed,
        "detached": status.detached,
    })))
}

/// `GET /$/evidence/{ds}/settings`: the recorded settings, or the disabled defaults.
async fn get_settings(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    blocking(move || body(sparkles::evidence::settings(&ds.dataset)?)).await
}

/// `PUT /$/evidence/{ds}/settings`: record complete settings. A configuration that
/// would stop protecting a graph, or that races with another registry change, is a
/// `409` that writes nothing.
async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let control = Control::none();
        let settings = sparkles::evidence::parse_settings(&bytes, &control)?;
        body(sparkles::evidence::configure(
            &ds.dataset,
            &settings,
            &control,
        )?)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detach {
    graph: String,
}

/// `POST /$/evidence/{ds}/detach` with `{"graph": IRI}`: ordinary writes can change the
/// graph until the settings are recorded again.
async fn detach(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let request: Detach = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid detach request: {e}"),
        )
    })?;
    let ds = dataset(&st, &name)?;
    blocking(move || body(sparkles::evidence::detach(&ds.dataset, &request.graph)?)).await
}

// ------------------------------------------------------------------ holds ------

/// The most holds one page lists.
const MAX_PAGE: usize = 1000;

/// A branch's name by its id, `None` for a deleted one.
fn branch_names(
    store: &sparkles::store::Store,
) -> ApiResult<std::collections::HashMap<uuid::Uuid, String>> {
    let mut names = std::collections::HashMap::new();
    names.insert(store.dataset_id(), sparkles::branch::MAIN.to_string());
    if store.branch_set().is_some() {
        for b in store.branches()? {
            names.insert(b.id, b.name);
        }
    }
    Ok(names)
}

fn hold_json(
    h: &sparkles::evidence::SourceHold,
    names: &std::collections::HashMap<uuid::Uuid, String>,
) -> J {
    json!({
        "id": h.id,
        "owner": h.owner.as_str(),
        "created": sparkles::commit::rfc3339_ms(h.created_ms),
        "commits": h.commits.iter().map(|c| json!({
            "branch": names.get(&c.branch_id),
            "branchId": c.branch_id,
            "commit": c.seq,
        })).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListHolds {
    /// only the holds that name this branch
    branch: Option<String>,
    /// the id after which the page starts
    after: Option<uuid::Uuid>,
    limit: Option<usize>,
}

/// `GET /$/evidence/{ds}/holds`: the evidence holds by id, a page at a time.
async fn list_holds(
    State(st): St,
    Path(name): Path<String>,
    Query(q): Query<ListHolds>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let limit = q.limit.unwrap_or(MAX_PAGE);
    if limit == 0 || limit > MAX_PAGE {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("limit must be between 1 and {MAX_PAGE}"),
        ));
    }
    blocking(move || {
        let store = ds.dataset.store();
        let names = branch_names(store)?;
        let branch = match &q.branch {
            None => None,
            Some(b) => Some(branch_id(store, b)?),
        };
        let all = sparkles::evidence::holds(&ds.dataset)?;
        let held: usize = all.iter().map(|h| h.commits.len()).sum();
        let mut page: Vec<J> = Vec::new();
        let mut next = None;
        for h in all.iter().filter(|h| {
            q.after.is_none_or(|a| h.id > a)
                && branch.is_none_or(|b| h.commits.iter().any(|c| c.branch_id == b))
        }) {
            if page.len() == limit {
                next = page.last().map(|p: &J| p["id"].clone());
                break;
            }
            page.push(hold_json(h, &names));
        }
        Ok(Json(json!({
            "holds": page,
            "next": next,
            "heldCommits": held,
            "maxCommits": sparkles::evidence::MAX_COMMITS,
        })))
    })
    .await
}

/// The id of branch `name` of `store`'s dataset.
fn branch_id(store: &sparkles::store::Store, name: &str) -> ApiResult<uuid::Uuid> {
    if name == sparkles::branch::MAIN {
        return Ok(store.dataset_id());
    }
    Ok(store.branch_info(name)?.id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HeldCommit {
    branch: Option<String>,
    branch_id: Option<uuid::Uuid>,
    commit: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Acquire {
    id: Option<uuid::Uuid>,
    commits: Vec<HeldCommit>,
}

/// `POST /$/evidence/{ds}/holds` with `{"id"?, "commits": [{"branch" | "branchId",
/// "commit"}]}`: keep the commits readable. `201` with a new hold, `200` with the
/// existing one of the same id and commits.
async fn acquire_hold(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let request: Acquire = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid hold request: {e}"),
        )
    })?;
    if request.commits.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "a hold names at least one commit",
        ));
    }
    if request.commits.len() > sparkles::evidence::MAX_COMMITS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!(
                "a hold names at most {} commits",
                sparkles::evidence::MAX_COMMITS
            ),
        ));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let store = ds.dataset.store();
        let mut commits = Vec::with_capacity(request.commits.len());
        for c in &request.commits {
            let branch_id = match (&c.branch, c.branch_id) {
                (Some(b), None) => branch_id(store, b)?,
                (None, Some(id)) => id,
                (None, None) => branch_id(store, sparkles::branch::MAIN)?,
                (Some(_), Some(_)) => {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "a held commit names its branch by branch or branchId, not both",
                    ));
                }
            };
            commits.push(sparkles::evidence::CommitRef {
                branch_id,
                seq: c.commit,
            });
        }
        let id = request.id.unwrap_or_else(uuid::Uuid::new_v4);
        let acquired = sparkles::evidence::acquire_hold(&ds.dataset, id, &commits)?;
        let names = branch_names(store)?;
        let status = if acquired.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((status, Json(hold_json(&acquired.hold, &names))).into_response())
    })
    .await
}

/// `DELETE /$/evidence/{ds}/holds/{id}`: release the hold; `204`, or `404` when the
/// dataset has no evidence hold of that id.
async fn release_hold(State(st): St, Path((name, id)): Path<(String, String)>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let id: uuid::Uuid = id
        .parse()
        .map_err(|_| err(StatusCode::BAD_REQUEST, format!("not a hold id: {id}")))?;
    let ds = dataset(&st, &name)?;
    blocking(move || {
        if sparkles::evidence::release_hold(&ds.dataset, id)? {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(err(StatusCode::NOT_FOUND, format!("no evidence hold {id}")))
        }
    })
    .await
}
