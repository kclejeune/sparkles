//! The HTTP routes of repositories and per-dataset backups (policies: `policies.rs`).
//!
//! Authorization (the route table in `auth/routes.rs`): `/$/repositories*` needs
//! `server-admin`, except `GET /$/repositories`, open to every caller and filtered here
//! (full entries for `server-admin`, `{name, type, readonly, reachable}` for callers
//! with `admin` on some dataset, `[]` otherwise). `/$/backups/{ds}…` needs `read` (GET)
//! or `admin` on `{ds}`; the handlers also check that the backup belongs to `{ds}` (by
//! `dataset.name`, or the live dataset's id), answering `404` otherwise, and a restore
//! needs `admin` on its target too. Errors are `{error, code}`
//! (`sparkles_backup::BackupError::body`, status `http_status`); tasks answer `202`
//! with `Location` and the task.

use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use sparkles_backup::BackupError;
use std::sync::Arc;

/// The routes, merged into `http::router` before its layers.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/$/repositories",
            get(list_repositories).post(add_repository),
        )
        .route(
            "/$/repositories/{repo}",
            get(get_repository)
                .put(put_repository)
                .delete(remove_repository),
        )
        .route("/$/repositories/{repo}/test", post(test_repository))
        .route("/$/repositories/{repo}/verify", post(verify_repository))
        .route("/$/repositories/{repo}/backups", get(repository_backups))
        .route("/$/repositories/{repo}/gc", post(gc_repository))
        .route("/$/repositories/{repo}/locks", get(list_locks))
        .route(
            "/$/repositories/{repo}/locks/{id}",
            axum::routing::delete(break_lock),
        )
        .route("/$/backups/{ds}", get(dataset_backups).post(create_backup))
        .route(
            "/$/backups/{ds}/{repo}/{backup}",
            get(get_backup).delete(delete_backup),
        )
        .route(
            "/$/backups/{ds}/{repo}/{backup}/restore",
            post(restore_backup),
        )
        .route(
            "/$/backups/{ds}/{repo}/{backup}/verify",
            post(verify_backup),
        )
        .merge(super::policies::routes())
}

/// A backup error as an HTTP response (`{error, code, …}` with its status).
pub fn error_response(e: &BackupError) -> Response {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = e.body();
    let mut r = (status, Json(body.clone())).into_response();
    // the body gets the request id like every other error body
    r.extensions_mut().insert(crate::http::ErrorJson(body));
    r
}

/// `501 {error, code: "not-implemented"}` for a route whose handler is not built yet.
pub fn not_implemented(what: &str) -> Response {
    error_response(&BackupError::unsupported(what))
}

/// `GET /$/repositories` → `RepositoryList`
async fn list_repositories() -> Response {
    not_implemented("listing repositories")
}

/// `POST /$/repositories[?verify=false]` (body `RepoConfig`) → `201` + `Location` +
/// `Repository` with `test`
async fn add_repository() -> Response {
    not_implemented("registering a repository")
}

/// `GET /$/repositories/{repo}` → `Repository`
async fn get_repository() -> Response {
    not_implemented("showing a repository")
}

/// `PUT /$/repositories/{repo}` (body `RepoConfig`) → `Repository`;
/// `409 location-immutable`, `409 read-only-config`
async fn put_repository() -> Response {
    not_implemented("changing a repository")
}

/// `DELETE /$/repositories/{repo}` → `204`; `409 repository-in-use`
async fn remove_repository() -> Response {
    not_implemented("unregistering a repository")
}

/// `POST /$/repositories/{repo}/test` → `TestReport`
async fn test_repository() -> Response {
    not_implemented("testing a repository")
}

/// `POST /$/repositories/{repo}/verify` (body `VerifyRequest`, `exists` or `data`) →
/// `202` task `backup-verify` (server-scoped), `detail: VerifyReport`
async fn verify_repository() -> Response {
    not_implemented("verifying a repository")
}

/// `GET /$/repositories/{repo}/backups[?dataset=&datasetId=&policy=&limit=&before=]` →
/// `BackupPage`
async fn repository_backups() -> Response {
    not_implemented("listing a repository's backups")
}

/// `POST /$/repositories/{repo}/gc` (body `GcRequest`) → `202` task `backup-gc`
/// (server-scoped), `detail: GcReport`
async fn gc_repository() -> Response {
    not_implemented("garbage collection")
}

/// `GET /$/repositories/{repo}/locks` → `LockList`
async fn list_locks() -> Response {
    not_implemented("listing locks")
}

/// `DELETE /$/repositories/{repo}/locks/{id}` → `204` (audited)
async fn break_lock() -> Response {
    not_implemented("breaking a lock")
}

/// `GET /$/backups/{ds}[?repository=]` → `DatasetBackups`, newest first, with
/// `sameLineage`
async fn dataset_backups() -> Response {
    not_implemented("listing a dataset's backups")
}

/// `POST /$/backups/{ds}` (body `CreateBackupRequest`) → `202` task `backup-create` +
/// `Location: /$/backups/{ds}/{repo}/{name}`, `detail: BackupSummary`
async fn create_backup() -> Response {
    not_implemented("creating a backup")
}

/// `GET /$/backups/{ds}/{repo}/{backup}` → `Backup`
async fn get_backup() -> Response {
    not_implemented("showing a backup")
}

/// `DELETE /$/backups/{ds}/{repo}/{backup}` → `204`; `409 backup-busy`
async fn delete_backup() -> Response {
    not_implemented("deleting a backup")
}

/// `POST /$/backups/{ds}/{repo}/{backup}/restore` (body `RestoreRequest`) → `202` task
/// `backup-restore` + `Location: /$/datasets/{target}`
async fn restore_backup() -> Response {
    not_implemented("restoring a backup")
}

/// `POST /$/backups/{ds}/{repo}/{backup}/verify` (body `VerifyRequest`) → `202` task
/// `backup-verify`, `detail: VerifyReport`
async fn verify_backup() -> Response {
    not_implemented("verifying a backup")
}
