//! The HTTP routes of repositories and per-dataset backups (policies: `policies.rs`).
//!
//! Authorization (the route table in `auth/routes.rs`): `/$/repositories*` needs
//! `server-admin`, except `GET /$/repositories`, open to every caller and filtered here
//! (full entries for `server-admin`, `{name, type, readonly, reachable}` for callers
//! with `admin` on some dataset, `[]` otherwise). `/$/backups/{ds}…` needs `read` (GET)
//! or `admin` on `{ds}`; the handlers also check that the backup belongs to `{ds}`
//! ([`Lineage`]), answering `404` otherwise, and a restore needs `admin` on its target
//! too. Errors are `{error, code}`
//! (`sparkles_backup::BackupError::body`, status `http_status`); tasks answer `202`
//! with `Location` and the task.
//!
//! `--read-only` servers: backups can be created, verified and deleted, repositories
//! tested, collected and their locks broken (on writable repositories); restores and
//! changes to the registry answer `403 server-read-only`.
//!
//! Repositories registered here are held to what the backup config file allows
//! (`BackupState::check_api`: named credential sources only, `fs` roots) and connect
//! only where the server's outbound policy allows. Tasks started here are admitted
//! first (`BackupState::admit`: `503 too-many-tasks` beyond the queue).

use super::registry::{RepoEntry, no_such_repository};
use super::{BackupState, ClaimSpec, ops};
use crate::auth::{Level, Principal, ServerPerm};
use crate::state::{AppState, DbType, Task};
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::de::DeserializeOwned;
use serde_json::{Value as J, json};
use sparkles_backup::{
    BackupError, BackupPage, Code, ConfigSource, DatasetBackups, GcRequest, Identity, ListFilter,
    LockList, Manifest, RepoConfig, RepoType, Repository, RepositoryBrief, RepositoryEntry,
    RepositoryList, RestoreRequest, TestReport, TestStep, TestStepKind, VerifyLevel, VerifyRequest,
    layout,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

type St = State<Arc<AppState>>;

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
        .merge(
            Router::new()
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
                .route_layer(axum::middleware::from_fn(paths_for_caller)),
        )
        .merge(super::policies::routes())
}

/// The per-dataset routes answer callers without `server-admin` (dataset admins and
/// readers) with the absolute paths in error messages cut to their last component:
/// where a repository lives is the server admin's business.
async fn paths_for_caller(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let admin = req
        .extensions()
        .get::<Principal>()
        .is_none_or(|p| p.has(ServerPerm::ServerAdmin));
    let mut resp = next.run(req).await;
    if admin {
        return resp;
    }
    match resp.extensions_mut().remove::<crate::http::ErrorJson>() {
        Some(crate::http::ErrorJson(mut body)) => {
            crate::http::redact_json_paths(&mut body);
            let mut r = (resp.status(), Json(body.clone())).into_response();
            r.extensions_mut().insert(crate::http::ErrorJson(body));
            r
        }
        None => resp,
    }
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

/// A failed request: its error response.
pub struct Fail(Box<Response>);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        *self.0
    }
}

impl From<BackupError> for Fail {
    fn from(e: BackupError) -> Fail {
        Fail(Box::new(error_response(&e)))
    }
}

type Res = Result<Response, Fail>;

/// An error with a code outside [`Code`] (`no-such-dataset`, `no-such-lock`).
fn code_error(status: StatusCode, code: &str, msg: String) -> Fail {
    let body = json!({ "error": msg, "code": code });
    let mut r = (status, Json(body.clone())).into_response();
    r.extensions_mut().insert(crate::http::ErrorJson(body));
    Fail(Box::new(r))
}

fn no_such_dataset(name: &str) -> Fail {
    code_error(
        StatusCode::NOT_FOUND,
        "no-such-dataset",
        format!("no such dataset: /{name}"),
    )
}

fn backups(st: &AppState) -> Result<Arc<BackupState>, Fail> {
    Ok(ops::backup_state(st)?)
}

/// `403 server-read-only` on a `--read-only` server.
fn writable_server(st: &AppState) -> Result<(), Fail> {
    if st.read_only {
        return Err(BackupError::new(Code::ServerReadOnly, "server is read-only").into());
    }
    Ok(())
}

/// A JSON body of the shape `T` (an empty one is `{}`): `400 invalid-request` if it is
/// not JSON, or not of that shape. Every route of repositories, backups and policies
/// reads its body so.
pub fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, BackupError> {
    parse_as(body, Code::InvalidRequest)
}

/// [`parse`] for a repository configuration: a body that is not JSON is
/// `400 invalid-request`, one that is not a configuration (an unknown type, a field of
/// the wrong type) `400 invalid-config`.
fn parse_config<T: DeserializeOwned>(body: &[u8]) -> Result<T, BackupError> {
    parse_as(body, Code::InvalidConfig)
}

/// Parse a JSON body; `shape` is the code of a body that is JSON but not a `T`.
fn parse_as<T: DeserializeOwned>(body: &[u8], shape: Code) -> Result<T, BackupError> {
    let bytes: &[u8] = if body.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        body
    };
    serde_json::from_slice(bytes).map_err(|e| {
        let code = if e.is_data() {
            shape
        } else {
            Code::InvalidRequest
        };
        BackupError::new(code, format!("invalid request body: {e}"))
    })
}

fn query(uri: &Uri) -> BTreeMap<String, String> {
    form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

/// `409 repository-read-only` for a write to a read-only repository.
fn writable_repo(cfg: &RepoConfig) -> Result<(), Fail> {
    Ok(ops::writable(cfg)?)
}

/// The task `id` once it has started or queued (see `Slots::acquire`).
async fn started_task(
    st: &AppState,
    task: Task,
    started: tokio::sync::oneshot::Receiver<()>,
) -> Task {
    let _ = tokio::time::timeout(Duration::from_secs(10), started).await;
    st.tasks
        .lock()
        .iter()
        .find(|t| t.id == task.id)
        .cloned()
        .unwrap_or(task)
}

fn accepted(task: Task, location: Option<String>) -> Response {
    let mut r = (StatusCode::ACCEPTED, Json(task)).into_response();
    if let Some(l) = location
        && let Ok(v) = header::HeaderValue::from_str(&l)
    {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// The wire form of a serde enum value (`"exists"`, `"ok"`).
fn wire<T: serde::Serialize>(v: T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The connection test report of a location that could not be opened.
fn failed_test(e: &BackupError) -> TestReport {
    let steps = [
        TestStepKind::Create,
        TestStepKind::CreateAgain,
        TestStepKind::Read,
        TestStepKind::List,
        TestStepKind::Delete,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, step)| TestStep {
        step,
        ok: false,
        millis: 0,
        error: Some(if i == 0 {
            e.message().to_string()
        } else {
            "skipped".into()
        }),
    })
    .collect();
    TestReport {
        ok: false,
        conditional_writes: false,
        steps,
    }
}

/// Record a connection test in a repository's status.
fn record_test(e: &mut RepoEntry, t: &TestReport) {
    if t.ok {
        e.mark_reachable(Some(t.conditional_writes));
    } else {
        let err = t
            .steps
            .iter()
            .find_map(|s| s.error.clone().filter(|_| !s.ok))
            .unwrap_or_else(|| "the connection test failed".into());
        e.mark_unreachable(&err);
    }
}

// ----------------------------------------------------------- repositories ------

/// `GET /$/repositories` → `RepositoryList`
async fn list_repositories(State(st): St, Extension(p): Extension<Principal>) -> Res {
    let b = backups(&st)?;
    let names: Vec<String> = b.registry.repos.read().keys().cloned().collect();
    let repositories = if p.has(ServerPerm::ServerAdmin) {
        names
            .iter()
            .filter_map(|n| b.registry.view(n).ok())
            .map(|r| RepositoryEntry::Full(Box::new(r)))
            .collect()
    } else if st.datasets.read().keys().any(|d| p.can(d, Level::Admin)) {
        b.registry
            .repos
            .read()
            .values()
            .map(|e| {
                RepositoryEntry::Brief(RepositoryBrief {
                    name: e.config.name.clone(),
                    kind: e.config.kind,
                    readonly: e.config.readonly,
                    reachable: e.status.reachable,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(RepositoryList { repositories }).into_response())
}

/// `POST /$/repositories[?verify=false]` (body `RepoConfig`) → `201` + `Location` +
/// `Repository` with `test`
async fn add_repository(
    State(st): St,
    Extension(p): Extension<Principal>,
    uri: Uri,
    body: Bytes,
) -> Res {
    let b = backups(&st)?;
    writable_server(&st)?;
    let cfg: RepoConfig = parse_config(&body)?;
    let name = cfg.name.clone();
    if !layout::valid_repo_name(&name) {
        return Err(BackupError::new(
            Code::InvalidName,
            format!(
                "invalid repository name \u{201c}{name}\u{201d}: use a-z, 0-9, '_' and '-' (at most 64)"
            ),
        )
        .into());
    }
    if cfg.kind == RepoType::Memory {
        return Err(BackupError::new(
            Code::InvalidConfig,
            "memory repositories are for tests only",
        )
        .into());
    }
    let exists = |b: &BackupState| -> Result<(), Fail> {
        let repos = b.registry.repos.read();
        if repos.contains_key(&name) {
            return Err(BackupError::new(
                Code::RepositoryExists,
                format!("a repository named \u{201c}{name}\u{201d} exists"),
            )
            .into());
        }
        if b.registry.policies.read().contains_key(&name) {
            return Err(BackupError::new(
                Code::RepositoryExists,
                format!("a policy is named \u{201c}{name}\u{201d}"),
            )
            .into());
        }
        if let Some(other) = repos.values().find(|e| e.config.same_location(&cfg)) {
            return Err(BackupError::new(
                Code::RepositoryExists,
                format!(
                    "this location is already registered as \u{201c}{}\u{201d}",
                    other.config.name
                ),
            )
            .into());
        }
        Ok(())
    };
    exists(&b)?;
    cfg.validate(&b.forbid)?;
    let open_cfg = b.prepare(&cfg, ConfigSource::Api)?;
    let verify = query(&uri).get("verify").is_none_or(|v| v != "false");
    let env = b.open_env(&cfg, ConfigSource::Api, !cfg.readonly);
    let (repo, open_err) = match Repository::open(&open_cfg, &env).await {
        Ok(r) => (Some(Arc::new(r)), None),
        // an unreachable location is registered anyway, and shown unreachable
        Err(e) if e.code() == Code::RepositoryUnavailable => (None, Some(e)),
        Err(e) => return Err(e.into()),
    };
    if let Some(r) = &repo
        && let Some(other) = b
            .registry
            .repos
            .read()
            .values()
            .find(|e| e.id == Some(r.id()))
    {
        return Err(BackupError::new(
            Code::RepositoryExists,
            format!(
                "repository {} is already registered as \u{201c}{}\u{201d}",
                r.id(),
                other.config.name
            ),
        )
        .into());
    }
    let test = match (&repo, &open_err, verify) {
        (_, _, false) => None,
        (Some(r), _, true) => Some(r.test().await?),
        (None, Some(e), true) => Some(failed_test(e)),
        (None, None, true) => None,
    };
    let mut e = RepoEntry::new(cfg.clone(), ConfigSource::Api);
    match (&repo, &test) {
        (Some(r), t) => {
            e.id = Some(r.id());
            e.opened = Some(r.clone());
            match t {
                Some(t) => record_test(&mut e, t),
                None => e.mark_reachable(None),
            }
        }
        (None, _) => e.mark_unreachable(open_err.as_ref().map_or("unreachable", |e| e.message())),
    }
    {
        let mut repos = b.registry.repos.write();
        if repos.contains_key(&name) {
            return Err(BackupError::new(
                Code::RepositoryExists,
                format!("a repository named \u{201c}{name}\u{201d} exists"),
            )
            .into());
        }
        repos.insert(name.clone(), e);
    }
    if let Err(e) = b.registry.save_repositories(&b.dir) {
        b.registry.repos.write().remove(&name);
        return Err(BackupError::internal(format!("{e:#}")).into());
    }
    tracing::info!(
        target: "sparkles::audit",
        event = "repository_added",
        repository = name.as_str(),
        location = cfg.location().as_str(),
        principal = p.id().as_str()
    );
    b.refresh_later(&name);
    let mut view = b.registry.view(&name)?;
    view.test = test;
    let mut r = (StatusCode::CREATED, Json(view)).into_response();
    if let Ok(v) = header::HeaderValue::from_str(&format!("/$/repositories/{name}")) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    Ok(r)
}

/// `GET /$/repositories/{repo}` → `Repository` (its totals are refreshed in the
/// background)
async fn get_repository(State(st): St, Path(name): Path<String>) -> Res {
    let b = backups(&st)?;
    let view = b.registry.view(&name)?;
    b.refresh_later(&name);
    Ok(Json(view).into_response())
}

/// `PUT /$/repositories/{repo}` (body `RepoConfig`) → `Repository`;
/// `409 location-immutable`, `409 read-only-config`
async fn put_repository(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
    body: Bytes,
) -> Res {
    let b = backups(&st)?;
    writable_server(&st)?;
    let (old, source) = {
        let repos = b.registry.repos.read();
        let e = repos.get(&name).ok_or_else(|| no_such_repository(&name))?;
        (e.config.clone(), e.source)
    };
    if source == ConfigSource::Config {
        return Err(BackupError::new(
            Code::ReadOnlyConfig,
            format!("\u{201c}{name}\u{201d} comes from the config file; edit it there"),
        )
        .into());
    }
    let mut v: J = parse(&body)?;
    let Some(obj) = v.as_object_mut() else {
        return Err(BackupError::new(Code::InvalidConfig, "the body must be a JSON object").into());
    };
    match obj.get("name") {
        Some(n) if n.as_str() != Some(name.as_str()) => {
            return Err(BackupError::new(Code::InvalidName, "the name cannot change").into());
        }
        _ => {
            obj.insert("name".into(), name.clone().into());
        }
    }
    let cfg: RepoConfig = serde_json::from_value(v)
        .map_err(|e| BackupError::new(Code::InvalidConfig, format!("invalid request body: {e}")))?;
    if !old.same_location(&cfg) {
        return Err(BackupError::new(
            Code::LocationImmutable,
            "the location (type, path, bucket, prefix, endpoint) of a repository cannot change",
        )
        .into());
    }
    cfg.validate(&b.forbid)?;
    b.check_api(&cfg)?;
    b.registry
        .update(&name, |e| {
            e.config = cfg.clone();
            // reopened with the new settings (limits, credentials) at the next use
            e.opened = None;
            e.status.single_writer =
                !cfg.conditional_writes || e.status.conditional_writes == Some(false);
        })
        .ok_or_else(|| no_such_repository(&name))?;
    if let Err(e) = b.registry.save_repositories(&b.dir) {
        return Err(BackupError::internal(format!("{e:#}")).into());
    }
    tracing::info!(
        target: "sparkles::audit",
        event = "repository_changed",
        repository = name.as_str(),
        principal = p.id().as_str()
    );
    Ok(Json(b.registry.view(&name)?).into_response())
}

/// `DELETE /$/repositories/{repo}` → `204`; `409 repository-in-use`
async fn remove_repository(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
) -> Res {
    let b = backups(&st)?;
    writable_server(&st)?;
    let source = b
        .registry
        .repos
        .read()
        .get(&name)
        .map(|e| e.source)
        .ok_or_else(|| no_such_repository(&name))?;
    if source == ConfigSource::Config {
        return Err(BackupError::new(
            Code::ReadOnlyConfig,
            format!("\u{201c}{name}\u{201d} comes from the config file; remove it there"),
        )
        .into());
    }
    let users = b.registry.policy_users(&name);
    if !users.is_empty() {
        return Err(BackupError::new(
            Code::RepositoryInUse,
            format!(
                "policies {} back up into \u{201c}{name}\u{201d}",
                users.join(", ")
            ),
        )
        .with("policies", users)
        .into());
    }
    if let Some(t) = b.repo_task(&name) {
        return Err(BackupError::new(
            Code::RepositoryInUse,
            format!("task {t} uses \u{201c}{name}\u{201d}"),
        )
        .with("task", t)
        .into());
    }
    let Some(entry) = b.registry.repos.write().remove(&name) else {
        return Err(no_such_repository(&name).into());
    };
    if let Err(e) = b.registry.save_repositories(&b.dir) {
        b.registry.repos.write().insert(name, entry);
        return Err(BackupError::internal(format!("{e:#}")).into());
    }
    tracing::info!(
        target: "sparkles::audit",
        event = "repository_removed",
        repository = name.as_str(),
        principal = p.id().as_str()
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /$/repositories/{repo}/test` → `TestReport`
async fn test_repository(State(st): St, Path(name): Path<String>) -> Res {
    let b = backups(&st)?;
    b.registry.config(&name)?;
    let report = match b.open_repo(&name).await {
        Ok(r) => r.test().await?,
        Err(e) if e.code() == Code::NotImplemented => return Err(e.into()),
        Err(e) => failed_test(&e),
    };
    b.registry.update(&name, |e| record_test(e, &report));
    if report.ok {
        b.refresh_later(&name);
    }
    Ok(Json(report).into_response())
}

/// `POST /$/repositories/{repo}/verify` (body `VerifyRequest`, `exists` or `data`) →
/// `202` task `backup-verify` (server-scoped), `detail: VerifyReport`
async fn verify_repository(State(st): St, Path(name): Path<String>, body: Bytes) -> Res {
    let b = backups(&st)?;
    b.registry.config(&name)?;
    let req: VerifyRequest = parse(&body)?;
    if req.level == VerifyLevel::Restore {
        return Err(BackupError::new(
            Code::InvalidRequest,
            "a repository is verified at level exists or data",
        )
        .into());
    }
    b.open_repo(&name).await?;
    let admission = b.admit()?;
    let id = st.next_task_id();
    let claim = b.claim(
        &id,
        ClaimSpec {
            repo: Some(name.clone()),
            ..Default::default()
        },
    )?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let st2 = st.clone();
    let repo = name.clone();
    let task = st.start_task_opts(id, "backup-verify", "", Some(&name), true, move |h| {
        let _held = (claim, admission);
        let r = ops::verify(&st2, &repo, Vec::new(), req.level, h, Some(tx))
            .map_err(ops::task_error)?;
        h.set_detail(serde_json::to_value(&r)?);
        Ok(format!(
            "{}: {} ({})",
            wire(r.status),
            super::cli::plural(r.backups.len() as u64, "backup", "backups"),
            wire(req.level)
        ))
    });
    Ok(accepted(started_task(&st, task, rx).await, None))
}

/// `GET /$/repositories/{repo}/backups[?dataset=&datasetId=&policy=&limit=&before=]` →
/// `BackupPage`
async fn repository_backups(State(st): St, Path(name): Path<String>, uri: Uri) -> Res {
    let b = backups(&st)?;
    b.registry.config(&name)?;
    let q = query(&uri);
    let bad = |m: String| Fail::from(BackupError::new(Code::InvalidRequest, m));
    let dataset_id = match q.get("datasetId") {
        Some(s) => {
            Some(Uuid::parse_str(s).map_err(|_| bad(format!("datasetId {s:?} is not a UUID")))?)
        }
        None => None,
    };
    let limit = match q.get("limit") {
        Some(s) => s
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| bad(format!("limit {s:?} is not a positive number")))?
            .min(1000),
        None => 100,
    };
    let repo = b.open_repo(&name).await?;
    let f = ListFilter {
        dataset: q.get("dataset").cloned(),
        dataset_id,
        policy: q.get("policy").cloned(),
        limit: Some(limit + 1),
        before: q.get("before").cloned(),
    };
    let mut backups = repo.list(&f).await?;
    let next = if backups.len() > limit {
        backups.truncate(limit);
        backups.last().map(|s| s.completed.clone())
    } else {
        None
    };
    b.verified.annotate(Some(repo.id()), &mut backups);
    Ok(Json(BackupPage { backups, next }).into_response())
}

/// `POST /$/repositories/{repo}/gc` (body `GcRequest`) → `202` task `backup-gc`
/// (server-scoped), `detail: GcReport`
async fn gc_repository(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
    body: Bytes,
) -> Res {
    let b = backups(&st)?;
    writable_repo(&b.registry.config(&name)?)?;
    let req: GcRequest = parse(&body)?;
    let hours = req.grace_hours.unwrap_or(24.0);
    if !(hours.is_finite() && hours >= 0.0) {
        return Err(BackupError::new(Code::InvalidRequest, "graceHours must be \u{2265} 0").into());
    }
    b.open_repo(&name).await?;
    let grace = Duration::from_secs_f64(hours * 3600.0);
    let admission = b.admit()?;
    let (task, rx) = ops::start_gc(&st, &name, req.dry_run, grace, p.id(), Some(admission))?;
    Ok(accepted(started_task(&st, task, rx).await, None))
}

/// `GET /$/repositories/{repo}/locks` → `LockList`
async fn list_locks(State(st): St, Path(name): Path<String>) -> Res {
    let b = backups(&st)?;
    b.registry.config(&name)?;
    let repo = b.open_repo(&name).await?;
    Ok(Json(LockList {
        locks: repo.locks().await?,
    })
    .into_response())
}

/// `DELETE /$/repositories/{repo}/locks/{id}` → `204` (audited)
async fn break_lock(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path((name, id)): Path<(String, String)>,
) -> Res {
    let b = backups(&st)?;
    writable_repo(&b.registry.config(&name)?)?;
    let repo = b.open_repo(&name).await?;
    if !repo.break_lock(&id).await? {
        return Err(code_error(
            StatusCode::NOT_FOUND,
            "no-such-lock",
            format!("no lock {id} in {name}"),
        ));
    }
    tracing::info!(
        target: "sparkles::audit",
        event = "lock_broken",
        repository = name.as_str(),
        lock = id.as_str(),
        principal = p.id().as_str()
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------- backups ------

/// Which backups belong to `/$/backups/{ds}` for a caller. By dataset id, not by name
/// alone: a dataset that reuses the name of a deleted one must not reach the backups
/// of the old one (another tenant's data, perhaps).
/// * the backups of the live dataset `ds` (its id);
/// * those of the dataset it replaced by an in-place restore (its `forkedFrom` id,
///   taken under the same name);
/// * for `server-admin`, every backup taken of a dataset named `ds`; when no dataset
///   `ds` is served (disaster recovery), that is all there is, so only `server-admin`
///   sees them.
struct Lineage {
    live: Option<Uuid>,
    replaced: Option<Uuid>,
    by_name: bool,
}

impl Lineage {
    fn of(st: &AppState, ds: &str, p: &Principal) -> Lineage {
        let d = st.get(ds);
        Lineage {
            live: d.as_ref().map(|d| d.store.dataset_id()),
            replaced: d.and_then(|d| d.store.forked_from()).map(|f| f.id),
            by_name: p.has(ServerPerm::ServerAdmin),
        }
    }

    /// Whether a backup of the dataset `name` with id `id` belongs to `ds`.
    fn shows(&self, ds: &str, name: &str, id: Uuid) -> bool {
        Some(id) == self.live || (name == ds && (self.by_name || Some(id) == self.replaced))
    }

    /// `sameLineage`: taken of the live dataset or of the one it replaced.
    fn same(&self, id: Uuid) -> bool {
        Some(id) == self.live || Some(id) == self.replaced
    }
}

/// Backup `name` of repository `repo`, if it belongs to `ds`; `404 no-such-backup`
/// otherwise (hiding the backups of other datasets).
async fn find_backup(
    st: &AppState,
    b: &BackupState,
    p: &Principal,
    ds: &str,
    repo: &str,
    name: &str,
) -> Result<(Arc<Repository>, Manifest), Fail> {
    b.registry.config(repo)?;
    let r = b.open_repo(repo).await?;
    let hidden = || {
        Fail::from(BackupError::new(
            Code::NoSuchBackup,
            format!("no backup \u{201c}{name}\u{201d} of {ds} in {repo}"),
        ))
    };
    if !layout::valid_backup_name(name) {
        return Err(hidden());
    }
    let m = match r.manifest(name).await {
        Ok(m) => m,
        Err(e) if e.code() == Code::NoSuchBackup => return Err(hidden()),
        Err(e) => return Err(e.into()),
    };
    if !Lineage::of(st, ds, p).shows(ds, &m.dataset.name, m.dataset.id) {
        return Err(hidden());
    }
    Ok((r, m))
}

/// `GET /$/backups/{ds}[?repository=]` → `DatasetBackups`, newest first, with
/// `sameLineage`
async fn dataset_backups(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(ds): Path<String>,
    uri: Uri,
) -> Res {
    let b = backups(&st)?;
    let lineage = Lineage::of(&st, &ds, &p);
    let names: Vec<String> = match query(&uri).get("repository") {
        Some(r) => {
            b.registry.config(r)?;
            vec![r.clone()]
        }
        None => b
            .registry
            .repos
            .read()
            .iter()
            .filter(|(_, e)| !e.recently_unreachable())
            .map(|(n, _)| n.clone())
            .collect(),
    };
    let mut out = Vec::new();
    for name in names {
        let listed = match b.open_repo(&name).await {
            Ok(r) => r.list(&ListFilter::default()).await.map(|l| (r, l)),
            Err(e) => Err(e),
        };
        let (repo, list) = match listed {
            Ok(x) => x,
            Err(e) => {
                tracing::debug!(target: "sparkles::backup", "listing {name} for /{ds}: {e}");
                continue;
            }
        };
        let mut list: Vec<_> = list
            .into_iter()
            .filter(|s| lineage.shows(&ds, &s.dataset.name, s.dataset.id))
            .map(|mut s| {
                s.same_lineage = Some(lineage.same(s.dataset.id));
                s
            })
            .collect();
        b.verified.annotate(Some(repo.id()), &mut list);
        out.extend(list);
    }
    out.sort_by(|a, b| b.completed.cmp(&a.completed));
    Ok(Json(DatasetBackups {
        dataset: ds,
        dataset_id: lineage.live,
        backups: out,
    })
    .into_response())
}

/// `POST /$/backups/{ds}` (body `CreateBackupRequest`) → `202` task `backup-create` +
/// `Location: /$/backups/{ds}/{repo}/{name}`, `detail: BackupSummary`
async fn create_backup(State(st): St, Path(ds_name): Path<String>, body: Bytes) -> Res {
    let b = backups(&st)?;
    let Some(ds) = st.get(&ds_name) else {
        return Err(no_such_dataset(&ds_name));
    };
    let v: J = parse(&body)?;
    let text = |k: &str| v[k].as_str().map(str::to_string).filter(|s| !s.is_empty());
    let repo = text("repository").unwrap_or_default();
    writable_repo(&b.registry.config(&repo)?)?;
    let name =
        text("name").unwrap_or_else(|| layout::default_backup_name(&ds_name, chrono::Utc::now()));
    if !layout::valid_backup_name(&name) {
        return Err(BackupError::new(
            Code::InvalidName,
            format!("invalid backup name \u{201c}{name}\u{201d}"),
        )
        .into());
    }
    if let Some(t) = b.create_running(&ds_name, &repo) {
        return Err(BackupError::new(
            Code::BackupInProgress,
            format!("a backup of /{ds_name} into {repo} is running (task {t})"),
        )
        .with("task", t)
        .into());
    }
    let r = b.open_repo(&repo).await?;
    // `fs` repositories keep the disk reserve (each blob is checked again as it is
    // written)
    r.check_space(0, st.limits.min_free_disk_bytes, false)?;
    // an early check: the conditional create of the manifest decides for good
    if r.manifest(&name).await.is_ok() {
        return Err(BackupError::new(
            Code::BackupExists,
            format!("backup \u{201c}{name}\u{201d} exists in {repo}"),
        )
        .into());
    }
    let admission = b.admit()?;
    let id = st.next_task_id();
    let claim = b.claim(
        &id,
        ClaimSpec {
            create: Some((ds_name.clone(), repo.clone())),
            repo: Some(repo.clone()),
            ..Default::default()
        },
    )?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let st2 = st.clone();
    let args = ops::CreateArgs {
        name: name.clone(),
        note: text("note"),
        policy: None,
    };
    let repo2 = repo.clone();
    let task = st.start_task_opts(id, "backup-create", &ds_name, Some(&name), true, move |h| {
        let _held = (claim, admission);
        let s = ops::create(&st2, &ds, &repo2, args, h, Some(tx)).map_err(ops::task_error)?;
        h.set_detail(serde_json::to_value(&s)?);
        Ok(format!(
            "backed up /{} at commit {} into {repo2}/{}",
            ds.name, s.commit.seq, s.name
        ))
    });
    let location = format!("/$/backups/{ds_name}/{repo}/{name}");
    Ok(accepted(started_task(&st, task, rx).await, Some(location)))
}

/// `GET /$/backups/{ds}/{repo}/{backup}` → `Backup`
async fn get_backup(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path((ds, repo, name)): Path<(String, String, String)>,
) -> Res {
    let b = backups(&st)?;
    let (r, m) = find_backup(&st, &b, &p, &ds, &repo, &name).await?;
    let mut view = m.view(&repo);
    view.summary.verified = b.verified.get(r.id(), &name);
    Ok(Json(view).into_response())
}

/// `DELETE /$/backups/{ds}/{repo}/{backup}` → `204`; `409 backup-busy`
async fn delete_backup(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path((ds, repo, name)): Path<(String, String, String)>,
) -> Res {
    let b = backups(&st)?;
    writable_repo(&b.registry.config(&repo)?)?;
    find_backup(&st, &b, &p, &ds, &repo, &name).await?;
    ops::delete(&st, &b, &repo, &name, &p.id()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /$/backups/{ds}/{repo}/{backup}/restore` (body `RestoreRequest`) → `202` task
/// `backup-restore` + `Location: /$/datasets/{target}`
async fn restore_backup(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path((ds, repo, name)): Path<(String, String, String)>,
    body: Bytes,
) -> Res {
    let b = backups(&st)?;
    writable_server(&st)?;
    let req: RestoreRequest = parse(&body)?;
    let (_, m) = find_backup(&st, &b, &p, &ds, &repo, &name).await?;
    let target = req.target.clone().unwrap_or_else(|| ds.clone());
    if !crate::state::valid_name(&target) {
        return Err(BackupError::new(
            Code::InvalidName,
            format!("invalid dataset name \u{201c}{target}\u{201d}"),
        )
        .into());
    }
    // a restore may only create or replace a dataset its caller could then manage
    if p.level(&target) != Some(Level::Admin) {
        return Err(Fail(Box::new(crate::auth::forbidden(
            &p,
            &format!("no admin access to the target name /{target}"),
        ))));
    }
    let source = m.dataset.id;
    let existing = st.get(&target);
    if req.replace {
        let Some(t) = &existing else {
            return Err(no_such_dataset(&target));
        };
        if t.kind != DbType::Persistent || t.ephemeral {
            return Err(BackupError::new(
                Code::NotManaged,
                format!(
                    "/{target} is not a persistent dataset of this server's data directory and cannot be replaced"
                ),
            )
            .into());
        }
        let busy = b
            .target_busy(&target)
            .or_else(|| b.dataset_task(&target))
            .or_else(|| st.restoring.lock().get(&target).cloned());
        if let Some(task) = busy {
            return Err(BackupError::new(
                Code::DatasetBusy,
                format!("task {task} works on /{target}"),
            )
            .with("task", task)
            .into());
        }
    } else if existing.is_some() {
        return Err(BackupError::new(
            Code::DatasetExists,
            format!("dataset /{target} already exists"),
        )
        .into());
    }
    if req.identity == Identity::Keep {
        let in_use = st
            .datasets
            .read()
            .values()
            .any(|d| !(req.replace && d.name == target) && d.store.dataset_id() == source);
        if in_use {
            return Err(BackupError::new(
                Code::DuplicateDatasetId,
                format!("another dataset has the id {source}"),
            )
            .into());
        }
        if let Some(t) = existing.as_ref().filter(|_| req.replace)
            && t.store.dataset_id() == source
        {
            let head = t.store.head_commit().seq;
            if head > m.commit.seq {
                return Err(BackupError::new(
                    Code::DuplicateDatasetId,
                    format!(
                        "keeping the id would issue commits {}\u{2013}{head} again; use identity new",
                        m.commit.seq + 1
                    ),
                )
                .into());
            }
        }
    }
    drop(existing);
    // checked again by the restore itself, once it holds the repository lock
    sparkles_backup::restore::check_free_space(
        &st.data_dir,
        sparkles_backup::manifest::logical_size(&m),
        st.limits.min_free_disk_bytes.unwrap_or(0),
        &format!("restoring {name}"),
    )?;
    let admission = b.admit()?;
    let id = st.next_task_id();
    let claim = b.claim(
        &id,
        ClaimSpec {
            backup: Some((repo.clone(), name.clone())),
            repo: Some(repo.clone()),
            target: Some(target.clone()),
            ..Default::default()
        },
    )?;
    let reservation = if req.replace {
        None
    } else {
        Some(
            st.reserve(&target, &id)
                .map_err(|m| Fail::from(BackupError::new(Code::DatasetExists, m)))?,
        )
    };
    tracing::info!(
        target: "sparkles::audit",
        event = "restore_started",
        repository = repo.as_str(),
        backup = name.as_str(),
        target = target.as_str(),
        replace = req.replace,
        principal = p.id().as_str()
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    let st2 = st.clone();
    let replace = req.replace;
    let args = ops::RestoreArgs {
        repo,
        backup: name.clone(),
        source_id: source,
        target: target.clone(),
        req,
        reservation,
        task: id.clone(),
        principal: p.id(),
    };
    let t2 = target.clone();
    let task = st.start_task_opts(id, "backup-restore", &ds, Some(&target), true, move |h| {
        let _held = (claim, admission);
        let d = ops::restore(&st2, args, h, Some(tx)).map_err(ops::task_error)?;
        h.set_detail(d);
        Ok(format!(
            "restored {name} into /{t2}{}",
            if replace { " (replaced)" } else { "" }
        ))
    });
    Ok(accepted(
        started_task(&st, task, rx).await,
        Some(format!("/$/datasets/{target}")),
    ))
}

/// `POST /$/backups/{ds}/{repo}/{backup}/verify` (body `VerifyRequest`) → `202` task
/// `backup-verify`, `detail: VerifyReport`
async fn verify_backup(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path((ds, repo, name)): Path<(String, String, String)>,
    body: Bytes,
) -> Res {
    let b = backups(&st)?;
    let req: VerifyRequest = parse(&body)?;
    find_backup(&st, &b, &p, &ds, &repo, &name).await?;
    let admission = b.admit()?;
    let id = st.next_task_id();
    let claim = b.claim(
        &id,
        ClaimSpec {
            backup: Some((repo.clone(), name.clone())),
            repo: Some(repo.clone()),
            ..Default::default()
        },
    )?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let st2 = st.clone();
    let n2 = name.clone();
    let task = st.start_task_opts(id, "backup-verify", &ds, Some(&name), true, move |h| {
        let _held = (claim, admission);
        let r = ops::verify(&st2, &repo, vec![n2.clone()], req.level, h, Some(tx))
            .map_err(ops::task_error)?;
        h.set_detail(serde_json::to_value(&r)?);
        Ok(format!("{}: {n2} ({})", wire(r.status), wire(req.level)))
    });
    Ok(accepted(started_task(&st, task, rx).await, None))
}
