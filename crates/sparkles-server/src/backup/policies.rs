//! Lifecycle policies on the server: `policies.json` (API policies), `policy-state.json`
//! (`lastScheduledFor`, `lastSuccess`, `consecutiveFailures`), `runs.json` (a ring of
//! the last 1000 `PolicyRun`s), all written with `state::write_file_atomic` under
//! `<data>/backup/`; the `backup-policy` task (back up the selected datasets one after
//! another, apply retention, optionally start GC); and the `/$/backup-policies` routes
//! (all `server-admin`). A policy may not be named `preview` (that path is the preview
//! route).

use super::http::not_implemented;
use crate::state::AppState;
use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use std::sync::Arc;

/// The routes, merged by `backup::http::routes`.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/backup-policies", get(list_policies).post(create_policy))
        .route("/$/backup-policies/preview", post(preview))
        .route(
            "/$/backup-policies/{policy}",
            get(get_policy).put(put_policy).delete(remove_policy),
        )
        .route("/$/backup-policies/{policy}/run", post(run_policy))
        .route("/$/backup-policies/{policy}/retention", post(retention))
        .route("/$/backup-policies/{policy}/runs", get(list_runs))
}

/// `GET /$/backup-policies` → `PolicyList`
async fn list_policies() -> Response {
    not_implemented("listing policies")
}

/// `POST /$/backup-policies` (body `PolicyConfig`) → `201` + `Policy`
async fn create_policy() -> Response {
    not_implemented("creating a policy")
}

/// `POST /$/backup-policies/preview` (body `PreviewRequest`) → `PreviewResponse`;
/// `400 invalid-schedule`
async fn preview() -> Response {
    not_implemented("schedule previews")
}

/// `GET /$/backup-policies/{policy}` → `Policy`
async fn get_policy() -> Response {
    not_implemented("showing a policy")
}

/// `PUT /$/backup-policies/{policy}` (body `PolicyConfig`) → `Policy`;
/// `409 read-only-config` for a config-file policy
async fn put_policy() -> Response {
    not_implemented("changing a policy")
}

/// `DELETE /$/backup-policies/{policy}` → `204`
async fn remove_policy() -> Response {
    not_implemented("removing a policy")
}

/// `POST /$/backup-policies/{policy}/run` → `202` task `backup-policy` (server-scoped),
/// `detail: PolicyRun`; the schedule does not move
async fn run_policy() -> Response {
    not_implemented("running a policy")
}

/// `POST /$/backup-policies/{policy}/retention[?dryRun=true]` → `RetentionResponse`
async fn retention() -> Response {
    not_implemented("policy retention")
}

/// `GET /$/backup-policies/{policy}/runs[?limit=]` → `PolicyRunList`, newest first
async fn list_runs() -> Response {
    not_implemented("policy run history")
}
