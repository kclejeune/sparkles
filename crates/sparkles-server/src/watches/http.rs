//! The watch routes: `GET` and `POST /$/watches`, `DELETE /$/watches/{id}`,
//! `GET /$/watches/{id}/matches` and `POST /$/watches/{id}/seen`.
//!
//! Every route answers only the caller's own watches, and only those whose graph the
//! caller can read now on the watch's branch, with its token scopes and its
//! branch-qualified grants, so a token limited to one dataset sees nothing of another.

use super::{NewWatch, Owner, Refusal, Scope, Watch};
use crate::auth::{Endpoint, Principal};
use crate::http::{ApiError, ApiResult, blocking, err_code};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

type St = State<Arc<AppState>>;

/// The most matches one answer lists.
const MAX_LIMIT: usize = 200;
/// The deepest depth limit a subtree watch takes.
const MAX_DEPTH: u32 = 1_000;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/watches", get(list).post(create))
        .route("/$/watches/{id}", axum::routing::delete(remove))
        .route("/$/watches/{id}/matches", get(matches))
        .route("/$/watches/{id}/seen", post(seen))
}

/// Whether `p` may read `graph` (the default graph for `None`) of `dataset` on
/// `branch` through the history endpoint, without triple protections.
pub(crate) fn reads(p: &Principal, dataset: &str, branch: &str, graph: Option<&str>) -> bool {
    if p.is_local() {
        return true;
    }
    let on = (branch != sparkles::branch::MAIN).then_some(branch);
    let p = p.clone().on_branch(on);
    if p.level(dataset).is_none() {
        return false;
    }
    match p.view(dataset, Endpoint::Diff) {
        None => true,
        Some(v) => {
            let term = graph.map(|g| oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(g)));
            v.triples.is_none() && v.settled().readable(term.as_ref())
        }
    }
}

fn refusal(r: Refusal) -> ApiError {
    match r {
        Refusal::Unavailable(m) => err_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "watches-unavailable",
            format!("the watches file is refused: {m}"),
        ),
        Refusal::Limit(m) => err_code(StatusCode::CONFLICT, "watch-limit", m),
        Refusal::Missing => err_code(StatusCode::NOT_FOUND, "no-such-watch", "no such watch"),
        Refusal::Save(m) => err_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "watches-not-saved",
            format!("the watch could not be saved: {m}"),
        ),
    }
}

fn owner(p: &Principal) -> ApiResult<Owner> {
    Owner::of(p).ok_or_else(|| {
        err_code(
            StatusCode::FORBIDDEN,
            "watch-needs-owner",
            "watches belong to a user or an account; anonymous callers and static tokens have none",
        )
    })
}

fn bad(m: impl Into<String>) -> ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-request", m)
}

/// A watch as the routes show it.
pub(crate) fn watch_json(w: &Watch) -> Value {
    let mut j = json!({
        "id": w.id,
        "dataset": w.dataset,
        "branch": w.branch,
        "entity": w.entity,
        "scope": w.scope,
        "created": w.created,
        "since": w.since,
        "seen": w.seen,
        "unread": w.unread(),
        "overflow": w.overflow,
        "truncated": w.truncated,
    });
    if let Some(g) = &w.graph {
        j["graph"] = json!(g);
    }
    if let Some(d) = w.depth {
        j["depth"] = json!(d);
    }
    if let Some(m) = w.matches.last() {
        j["latest"] = json!(m.commit);
    }
    j
}

#[derive(Deserialize)]
struct ListQuery {
    dataset: Option<String>,
    entity: Option<String>,
    graph: Option<String>,
}

async fn list(
    State(st): St,
    Extension(p): Extension<Principal>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    blocking(move || {
        let o = owner(&p)?;
        let ws = st.watches.list(&st, &o).map_err(refusal)?;
        let graph = q.graph.as_deref().map(|g| (g != "default").then_some(g));
        let out: Vec<Value> = ws
            .iter()
            .filter(|w| q.dataset.as_deref().is_none_or(|d| d == w.dataset))
            .filter(|w| q.entity.as_deref().is_none_or(|e| e == w.entity))
            .filter(|w| graph.is_none_or(|g| g == w.graph.as_deref()))
            .filter(|w| reads_watch(&st, &p, w))
            .map(watch_json)
            .collect();
        let unread: usize = out.iter().filter_map(|w| w["unread"].as_u64()).sum::<u64>() as usize;
        Ok(Json(json!({ "watches": out, "unread": unread })))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    dataset: String,
    #[serde(default)]
    branch: Option<String>,
    /// the graph's IRI; absent or `default` for the default graph
    #[serde(default)]
    graph: Option<String>,
    entity: String,
    #[serde(default)]
    scope: Option<Scope>,
    #[serde(default)]
    depth: Option<u32>,
}

async fn create(
    State(st): St,
    Extension(p): Extension<Principal>,
    Json(b): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    blocking(move || {
        let o = owner(&p)?;
        oxrdf::NamedNode::new(&b.entity).map_err(|e| bad(format!("entity: {e}")))?;
        let graph = match b.graph.as_deref() {
            None | Some("default") => None,
            Some(g) => {
                oxrdf::NamedNode::new(g).map_err(|e| bad(format!("graph: {e}")))?;
                Some(g.to_string())
            }
        };
        let scope = b.scope.unwrap_or(Scope::Entity);
        if let Some(d) = b.depth
            && (scope != Scope::Subtree || !(1..=MAX_DEPTH).contains(&d))
        {
            return Err(bad(format!(
                "depth: from 1 to {MAX_DEPTH}, and only for the subtree scope"
            )));
        }
        let branch = b
            .branch
            .unwrap_or_else(|| sparkles::branch::MAIN.to_string());
        let missing = || {
            err_code(
                StatusCode::NOT_FOUND,
                "not-found",
                format!("no such dataset or branch: /{}@{branch}", b.dataset),
            )
        };
        let main = match st.lookup(&b.dataset) {
            crate::state::Lookup::Open(ds) => ds,
            crate::state::Lookup::Unavailable(u) => {
                return Err(crate::http::unavailable_error(&b.dataset, &u));
            }
            crate::state::Lookup::Missing => return Err(missing()),
        };
        let on = (branch != sparkles::branch::MAIN).then_some(branch.as_str());
        let pb = p.clone().on_branch(on);
        if !p.is_local() && pb.level(&b.dataset).is_none() {
            return Err(missing());
        }
        let branch_id = main.store.branch_id_of(&branch).map_err(|_| missing())?;
        let ds = st
            .branch_dataset(&main, &branch)
            .map_err(|_| missing())?;
        if !p.is_local()
            && let Some(v) = pb.view(&b.dataset, Endpoint::Diff)
        {
            let term = graph
                .as_deref()
                .map(|g| oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(g)));
            if !v.settled().readable(term.as_ref()) {
                return Err(err_code(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "you cannot read this graph",
                ));
            }
            if v.triples.is_some() {
                return Err(err_code(
                    StatusCode::CONFLICT,
                    "watch-unsupported-protections",
                    "watches are not available while triple protections limit what you read in this dataset",
                ));
            }
        }
        let n = NewWatch {
            dataset: b.dataset,
            branch,
            branch_id,
            graph,
            entity: b.entity,
            scope,
            depth: b.depth,
            head: ds.store.snapshot().commit,
        };
        let (w, made) = st
            .watches
            .create(&st, &o, n, chrono::Utc::now())
            .map_err(refusal)?;
        let status = if made {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((status, Json(watch_json(&w))))
    })
    .await
}

/// The current dataset and branch names of watch `w`'s branch, which a rename may have
/// changed since the follower last recorded them; `None` when the branch is gone.
fn current(st: &AppState, w: &Watch) -> Option<(String, String)> {
    let found = |d: &Arc<crate::state::Dataset>| {
        d.store
            .listed_branch_name(w.branch_id, &Default::default())
            .ok()
            .flatten()
            .map(|b| (d.name.clone(), b))
    };
    match st.lookup(&w.dataset) {
        crate::state::Lookup::Open(d) => found(&d),
        _ => st.datasets().values().find_map(found),
    }
}

/// Whether `p` may read watch `w`'s graph now, on its branch's current name.
fn reads_watch(st: &AppState, p: &Principal, w: &Watch) -> bool {
    current(st, w).is_some_and(|(ds, b)| reads(p, &ds, &b, w.graph.as_deref()))
}

/// The caller's watch `id`, when it may still read the watch's graph.
fn visible(st: &AppState, p: &Principal, o: &Owner, id: &str) -> ApiResult<Watch> {
    let w = st.watches.get(st, o, id).map_err(refusal)?;
    if !reads_watch(st, p, &w) {
        return Err(err_code(
            StatusCode::FORBIDDEN,
            "watch-access-lost",
            "you can no longer read the watched graph",
        ));
    }
    Ok(w)
}

async fn remove(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    blocking(move || {
        let o = owner(&p)?;
        visible(&st, &p, &o, &id)?;
        st.watches.delete(&st, &o, &id).map_err(refusal)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

#[derive(Deserialize)]
struct MatchQuery {
    /// only commits above this one
    after: Option<u64>,
    /// only commits below this one, for the next page
    before: Option<u64>,
    limit: Option<usize>,
}

async fn matches(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<MatchQuery>,
) -> ApiResult<Json<Value>> {
    blocking(move || {
        let o = owner(&p)?;
        let limit = q.limit.unwrap_or(50);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(bad(format!("limit: from 1 to {MAX_LIMIT}")));
        }
        let w = visible(&st, &p, &o, &id)?;
        let all: Vec<&super::Match> = w
            .matches
            .iter()
            .rev()
            .filter(|m| q.after.is_none_or(|a| m.commit > a))
            .filter(|m| q.before.is_none_or(|b| m.commit < b))
            .collect();
        let more = all.len() > limit;
        let page: Vec<Value> = all
            .into_iter()
            .take(limit)
            .map(|m| {
                let mut j = serde_json::to_value(m).unwrap_or_default();
                j["unread"] = json!(m.commit > w.seen);
                j
            })
            .collect();
        Ok(Json(
            json!({ "watch": watch_json(&w), "matches": page, "more": more }),
        ))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeenBody {
    commit: u64,
}

async fn seen(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(b): Json<SeenBody>,
) -> ApiResult<Json<Value>> {
    blocking(move || {
        let o = owner(&p)?;
        let w = visible(&st, &p, &o, &id)?;
        let head = current(&st, &w).and_then(|(ds, b)| match st.lookup(&ds) {
            crate::state::Lookup::Open(main) => st
                .branch_dataset(&main, &b)
                .map(|d| d.store.snapshot().commit)
                .ok(),
            _ => None,
        });
        if head.is_some_and(|h| b.commit > h) {
            return Err(bad("commit: above the branch's head"));
        }
        let w = st.watches.seen(&st, &o, &id, b.commit).map_err(refusal)?;
        Ok(Json(watch_json(&w)))
    })
    .await
}
