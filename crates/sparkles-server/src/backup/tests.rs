//! Server-level tests of backup repositories: the routes (through the router), task
//! slots and claims, in-place swaps under load, and the data-directory lock.
//!
//! The end-to-end tests at the bottom drive the repository engine and are ignored
//! until it is complete.

use super::*;
use crate::auth::Peer;
use crate::http::router;
use crate::state::{DbType, Task};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{Value as J, json};
use sparkles::sparql::QueryOptions;
use sparkles::store::{Store, StoreOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tower::ServiceExt;

// ---------------------------------------------------------------- fixture ------

#[derive(Default)]
struct Opts {
    auth: bool,
    read_only: bool,
    /// `repositories.json` entries
    repos: Vec<J>,
    /// a backup config file (loaded without the checks of `BackupState::new`)
    config: Option<&'static str>,
    max_tasks: usize,
}

struct Srv {
    dir: tempfile::TempDir,
    st: Arc<AppState>,
    app: Router,
}

impl Srv {
    fn b(&self) -> Arc<BackupState> {
        self.st.backup.clone().unwrap()
    }
}

fn auth_config() -> String {
    let h = |pw: &str| crate::auth::hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
version = 1

[[users]]
name = "alice"
password = "{}"
server = ["server-admin"]

[[users]]
name = "carol"
password = "{}"
datasets = {{ "wiki*" = "admin" }}

[[users]]
name = "dave"
password = "{}"
datasets = {{ wiki = "read" }}
"#,
        h("alice-pw"),
        h("carol-pw"),
        h("dave-pw")
    )
}

/// A server with the persistent datasets `ds`, `wiki` and `secret`, the in-memory
/// dataset `mem`, and backups enabled.
fn server(o: Opts) -> Srv {
    let dir = tempfile::tempdir().unwrap();
    if !o.repos.is_empty() {
        let p = dir.path().join("backup");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join(registry::REPOSITORIES_FILE),
            serde_json::to_vec(&json!({"version": 1, "repositories": o.repos})).unwrap(),
        )
        .unwrap();
    }
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = o.read_only;
    #[cfg(feature = "auth")]
    if o.auth {
        let cfg = dir.path().join("auth.toml");
        std::fs::write(&cfg, auth_config()).unwrap();
        st.auth = Some(Arc::new(
            crate::auth::Auth::open(&cfg, dir.path()).unwrap().0,
        ));
    }
    let max_tasks = if o.max_tasks == 0 { 2 } else { o.max_tasks };
    let b = BackupState::new(dir.path(), None, max_tasks).unwrap();
    if let Some(c) = o.config {
        b.registry
            .replace_config(&config::ConfigFile::parse(c).unwrap())
            .unwrap();
    }
    st.backup = Some(Arc::new(b));
    let st = Arc::new(st);
    for name in ["ds", "wiki", "secret"] {
        let ds = st.create(name, DbType::Persistent).unwrap();
        sparkles::sparql::update::update(
            &ds.store,
            &format!("INSERT DATA {{ <urn:{name}> <urn:p> 1 }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    st.attach("mem", DbType::Mem, None).unwrap();
    st.set_phase(crate::obs::Phase::Ready);
    let app = router(st.clone());
    Srv { dir, st, app }
}

fn fs_repo(name: &str, path: &str) -> J {
    json!({"name": name, "type": "fs", "path": path})
}

struct R {
    status: StatusCode,
    headers: HeaderMap,
    body: J,
}

fn basic(user: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{user}-pw"))
    )
}

async fn call(app: &Router, method: &str, uri: &str, user: Option<&str>, body: J) -> R {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())));
    if let Some(u) = user {
        req = req.header("authorization", basic(u));
    }
    let body = if body.is_null() {
        Body::empty()
    } else {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        J::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| J::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    R {
        status,
        headers,
        body,
    }
}

async fn get(app: &Router, uri: &str) -> R {
    call(app, "GET", uri, None, J::Null).await
}

async fn post(app: &Router, uri: &str, body: J) -> R {
    call(app, "POST", uri, None, body).await
}

fn expect(r: &R, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["code"], code, "{}", r.body);
    assert!(r.body["error"].is_string(), "{}", r.body);
}

async fn wait_task(st: &AppState, id: &str) -> Task {
    let t0 = Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if !t.active() {
            return t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "task {id} did not end"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn task_state_of(st: &AppState, id: &str) -> String {
    st.tasks
        .lock()
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.state.clone())
        .unwrap()
}

// ----------------------------------------------------------- repositories ------

#[tokio::test]
async fn without_backups_the_routes_are_not_implemented() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let app = router(st);
    let r = get(&app, "/$/repositories").await;
    expect(&r, StatusCode::NOT_IMPLEMENTED, "not-implemented");
    let r = get(&app, "/$/backups/ds").await;
    expect(&r, StatusCode::NOT_IMPLEMENTED, "not-implemented");
}

#[tokio::test]
async fn repositories_are_listed_with_their_source_and_policies() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        config: Some(
            "version = 1\n[repositories.cfg]\ntype = \"s3\"\nbucket = \"b\"\nreadonly = true\n[policies.nightly]\nrepository = \"local\"\nschedule = \"30 2 * * *\"\n",
        ),
        ..Default::default()
    });
    let r = get(&s.app, "/$/repositories").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let list = r.body["repositories"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let cfg = &list[0];
    assert_eq!(cfg["name"], "cfg");
    assert_eq!(cfg["source"], "config");
    assert_eq!(cfg["readonly"], true);
    let local = &list[1];
    assert_eq!(local["name"], "local");
    assert_eq!(local["source"], "api");
    assert_eq!(local["type"], "fs");
    assert_eq!(local["policies"], json!(["nightly"]));
    assert!(local["id"].is_null() && local["stats"].is_null() && local["lastGc"].is_null());
    assert_eq!(local["status"]["reachable"], false);
    let r = get(&s.app, "/$/repositories/local").await;
    assert_eq!(r.body["path"], "/srv/r");
    let r = get(&s.app, "/$/repositories/none").await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-repository");
    for (m, uri) in [
        ("PUT", "/$/repositories/none"),
        ("DELETE", "/$/repositories/none"),
        ("POST", "/$/repositories/none/test"),
        ("POST", "/$/repositories/none/verify"),
        ("GET", "/$/repositories/none/backups"),
        ("POST", "/$/repositories/none/gc"),
        ("GET", "/$/repositories/none/locks"),
        ("DELETE", "/$/repositories/none/locks/x"),
    ] {
        let body = if m == "PUT" {
            fs_repo("none", "/x")
        } else {
            J::Null
        };
        let r = call(&s.app, m, uri, None, body).await;
        expect(&r, StatusCode::NOT_FOUND, "no-such-repository");
    }
}

#[tokio::test]
async fn registrations_are_checked_before_any_repository_is_touched() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    let r = post(&s.app, "/$/repositories", fs_repo("Bad Name", "/srv/x")).await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-name");
    let r = post(&s.app, "/$/repositories", fs_repo("local", "/srv/x")).await;
    expect(&r, StatusCode::CONFLICT, "repository-exists");
    let r = post(&s.app, "/$/repositories", fs_repo("again", "/srv/r")).await;
    expect(&r, StatusCode::CONFLICT, "repository-exists");
    assert!(
        r.body["error"].as_str().unwrap().contains("local"),
        "{}",
        r.body
    );
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "m", "type": "memory"}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "x", "type": "ftp"}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    let r = call(
        &s.app,
        "POST",
        "/$/repositories",
        None,
        J::String("{not json".into()),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
}

#[tokio::test]
async fn a_read_only_server_changes_no_registration_and_restores_nothing() {
    let s = server(Opts {
        read_only: true,
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    for (m, uri, body) in [
        ("POST", "/$/repositories", fs_repo("other", "/srv/o")),
        ("PUT", "/$/repositories/local", fs_repo("local", "/srv/r")),
        ("DELETE", "/$/repositories/local", J::Null),
        ("POST", "/$/backups/ds/local/b1/restore", json!({})),
    ] {
        let r = call(&s.app, m, uri, None, body).await;
        expect(&r, StatusCode::FORBIDDEN, "server-read-only");
    }
    // the others pass the server check (and reach the repository)
    let r = post(&s.app, "/$/backups/mem", json!({"repository": "local"})).await;
    expect(&r, StatusCode::NOT_IMPLEMENTED, "backup-unsupported");
}

#[tokio::test]
async fn config_repositories_are_read_only_through_the_api() {
    let s = server(Opts {
        config: Some("version = 1\n[repositories.cfg]\ntype = \"fs\"\npath = \"/srv/c\"\n"),
        ..Default::default()
    });
    let r = call(
        &s.app,
        "PUT",
        "/$/repositories/cfg",
        None,
        fs_repo("cfg", "/srv/c"),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "read-only-config");
    let r = call(&s.app, "DELETE", "/$/repositories/cfg", None, J::Null).await;
    expect(&r, StatusCode::CONFLICT, "read-only-config");
    let r = post(&s.app, "/$/repositories", fs_repo("cfg", "/srv/other")).await;
    expect(&r, StatusCode::CONFLICT, "repository-exists");
}

#[tokio::test]
async fn unregistering_waits_for_policies_and_tasks() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r"), fs_repo("used", "/srv/u")],
        config: Some(
            "version = 1\n[policies.nightly]\nrepository = \"used\"\nschedule = \"30 2 * * *\"\n",
        ),
        ..Default::default()
    });
    let r = call(&s.app, "DELETE", "/$/repositories/used", None, J::Null).await;
    expect(&r, StatusCode::CONFLICT, "repository-in-use");
    assert_eq!(r.body["policies"], json!(["nightly"]));
    let claim = s
        .b()
        .claim(
            "42",
            ClaimSpec {
                repo: Some("local".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let r = call(&s.app, "DELETE", "/$/repositories/local", None, J::Null).await;
    expect(&r, StatusCode::CONFLICT, "repository-in-use");
    assert_eq!(r.body["task"], "42");
    drop(claim);
    let r = call(&s.app, "DELETE", "/$/repositories/local", None, J::Null).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let saved = std::fs::read_to_string(
        s.dir
            .path()
            .join("backup")
            .join(registry::REPOSITORIES_FILE),
    )
    .unwrap();
    assert!(
        !saved.contains("/srv/r") && saved.contains("/srv/u"),
        "{saved}"
    );
    let r = get(&s.app, "/$/repositories/local").await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-repository");
}

#[tokio::test]
async fn a_changed_location_is_refused() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    let r = call(
        &s.app,
        "PUT",
        "/$/repositories/local",
        None,
        fs_repo("local", "/srv/elsewhere"),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "location-immutable");
    let r = call(
        &s.app,
        "PUT",
        "/$/repositories/local",
        None,
        fs_repo("renamed", "/srv/r"),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-name");
}

// ---------------------------------------------------------------- backups ------

#[tokio::test]
async fn backup_requests_are_checked_in_order() {
    let s = server(Opts {
        repos: vec![
            fs_repo("local", "/srv/r"),
            json!({"name": "ro", "type": "fs", "path": "/srv/ro", "readonly": true}),
        ],
        ..Default::default()
    });
    let r = post(&s.app, "/$/backups/none", json!({"repository": "local"})).await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-dataset");
    let r = post(&s.app, "/$/backups/ds", json!({"repository": "nope"})).await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-repository");
    let r = post(&s.app, "/$/backups/ds", json!({})).await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-repository");
    let r = post(&s.app, "/$/backups/ds", json!({"repository": "ro"})).await;
    expect(&r, StatusCode::CONFLICT, "repository-read-only");
    let r = post(&s.app, "/$/backups/mem", json!({"repository": "local"})).await;
    expect(&r, StatusCode::NOT_IMPLEMENTED, "backup-unsupported");
    let r = post(
        &s.app,
        "/$/backups/ds",
        json!({"repository": "local", "name": "no spaces"}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-name");
    let _claim = s
        .b()
        .claim(
            "17",
            ClaimSpec {
                create: Some(("ds".into(), "local".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let r = post(&s.app, "/$/backups/ds", json!({"repository": "local"})).await;
    expect(&r, StatusCode::CONFLICT, "backup-in-progress");
    assert_eq!(r.body["task"], "17");
    // GC and lock breaking write: not on read-only repositories
    let r = post(&s.app, "/$/repositories/ro/gc", json!({})).await;
    expect(&r, StatusCode::CONFLICT, "repository-read-only");
    let r = call(
        &s.app,
        "DELETE",
        "/$/repositories/ro/locks/x",
        None,
        J::Null,
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "repository-read-only");
    let r = call(&s.app, "DELETE", "/$/backups/ds/ro/b1", None, J::Null).await;
    expect(&r, StatusCode::CONFLICT, "repository-read-only");
    let r = post(
        &s.app,
        "/$/repositories/local/gc",
        json!({"graceHours": -1}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    let r = post(
        &s.app,
        "/$/repositories/local/verify",
        json!({"level": "restore"}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-request");
}

#[tokio::test]
async fn dataset_listings_skip_unreachable_repositories() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    let r = get(&s.app, "/$/backups/ds").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["dataset"], "ds");
    let id = s.st.get("ds").unwrap().store.dataset_id().to_string();
    assert_eq!(r.body["datasetId"], id.as_str());
    assert_eq!(r.body["backups"], json!([]));
    // a dataset that is not here (disaster recovery) has no live id
    let r = get(&s.app, "/$/backups/gone").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body["datasetId"].is_null());
    let r = get(&s.app, "/$/backups/ds?repository=nope").await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-repository");
}

#[tokio::test]
async fn busy_backups_are_not_deleted() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    let b = s.b();
    let claim = b
        .claim(
            "9",
            ClaimSpec {
                backup: Some(("local".into(), "b1".into())),
                repo: Some("local".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(b.backup_busy("local", "b1").as_deref(), Some("9"));
    assert_eq!(
        b.busy_backups("local").into_iter().collect::<Vec<_>>(),
        ["b1"]
    );
    let e = ops::delete(&s.st, &b, "local", "b1", "local")
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::BackupBusy);
    assert_eq!(e.body()["task"], "9");
    drop(claim);
    assert!(b.backup_busy("local", "b1").is_none() && b.busy_backups("local").is_empty());
}

#[test]
fn claims_conflict_and_release() {
    let dir = tempfile::tempdir().unwrap();
    let b = Arc::new(BackupState::new(dir.path(), None, 2).unwrap());
    let c1 = b
        .claim(
            "1",
            ClaimSpec {
                create: Some(("ds".into(), "r".into())),
                repo: Some("r".into()),
                ..Default::default()
            },
        )
        .unwrap();
    // another repository is fine; the same pair is not
    let c2 = b
        .claim(
            "2",
            ClaimSpec {
                create: Some(("ds".into(), "s".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let e = b
        .claim(
            "3",
            ClaimSpec {
                create: Some(("ds".into(), "r".into())),
                ..Default::default()
            },
        )
        .err()
        .unwrap();
    assert_eq!(e.code(), Code::BackupInProgress);
    assert_eq!(b.dataset_task("ds").as_deref(), Some("1"));
    let t = b
        .claim(
            "4",
            ClaimSpec {
                target: Some("x".into()),
                repo: Some("r".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let e = b
        .claim(
            "5",
            ClaimSpec {
                target: Some("x".into()),
                ..Default::default()
            },
        )
        .err()
        .unwrap();
    assert_eq!(e.code(), Code::DatasetBusy);
    assert_eq!(b.repo_task("r").as_deref(), Some("1"));
    drop(c1);
    assert_eq!(b.repo_task("r").as_deref(), Some("4"));
    assert!(b.create_running("ds", "r").is_none());
    drop((c2, t));
    assert!(b.repo_task("r").is_none() && b.target_busy("x").is_none());
    assert!(b.dataset_task("ds").is_none());
}

// ------------------------------------------------------------------ slots ------

/// A task that holds a backup slot until `release` is set.
fn slot_task(
    st: &Arc<AppState>,
    release: Arc<AtomicBool>,
) -> (String, tokio::sync::oneshot::Receiver<()>) {
    let id = st.next_task_id();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = st.backup.clone().unwrap();
    st.start_task_opts(id.clone(), "test", "ds", None, true, move |h| {
        let _slot = b.slots.acquire(h, Some(tx)).map_err(anyhow::Error::new)?;
        while !release.load(Ordering::Relaxed) {
            if h.is_cancelled() {
                return Err(anyhow::Error::new(BackupError::cancelled()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok("done".into())
    });
    (id, rx)
}

#[tokio::test]
async fn tasks_queue_for_slots_and_can_be_cancelled_while_queued() {
    let s = server(Opts {
        max_tasks: 1,
        ..Default::default()
    });
    let b = s.b();
    let release = Arc::new(AtomicBool::new(false));
    let (a, rx) = slot_task(&s.st, release.clone());
    rx.await.unwrap();
    assert_eq!(task_state_of(&s.st, &a), "running");
    assert_eq!(b.slots.in_use(), 1);
    let (q1, rx) = slot_task(&s.st, Arc::new(AtomicBool::new(true)));
    rx.await.unwrap();
    assert_eq!(task_state_of(&s.st, &q1), "queued");
    let (q2, rx) = slot_task(&s.st, Arc::new(AtomicBool::new(true)));
    rx.await.unwrap();
    assert_eq!(task_state_of(&s.st, &q2), "queued");
    // a queued task is cancelled without ever running
    let r = call(&s.app, "DELETE", &format!("/$/tasks/{q2}"), None, J::Null).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(wait_task(&s.st, &q2).await.state, "cancelled");
    // the running one ends: the queued one runs
    release.store(true, Ordering::Relaxed);
    assert_eq!(wait_task(&s.st, &a).await.state, "done");
    assert_eq!(wait_task(&s.st, &q1).await.state, "done");
    assert_eq!(b.slots.in_use(), 0);
}

// ------------------------------------------------------------ in-place swap ------

/// A closed database at `dir` with `n` quads `<urn:r{i}> <urn:p> i`.
fn restored_copy(dir: &std::path::Path, n: usize) -> uuid::Uuid {
    let s = Store::open(dir, StoreOptions::default()).unwrap();
    for i in 0..n {
        sparkles::sparql::update::update(
            &s,
            &format!("INSERT DATA {{ <urn:r{i}> <urn:p> {i} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    s.dataset_id()
}

/// A client polling a dataset during an in-place swap sees its old or new state, or a
/// retryable `503`, never a `404` or `500`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_polling_client_never_sees_the_dataset_missing_during_a_swap() {
    let s = server(Opts::default());
    let tmp = s.dir.path().join("databases").join(".restore-ds-77");
    let new_id = restored_copy(&tmp, 3);
    let stop = Arc::new(AtomicBool::new(false));
    let mut pollers = Vec::new();
    for uri in [
        "/ds/sparql?query=SELECT%20(COUNT(*)%20AS%20%3Fn)%20%7B%3Fs%20%3Fp%20%3Fo%7D",
        "/$/datasets/ds",
        "/$/backups/ds",
    ] {
        let app = s.app.clone();
        let stop = stop.clone();
        pollers.push(tokio::spawn(async move {
            let mut seen: BTreeMap<u16, usize> = BTreeMap::new();
            while !stop.load(Ordering::Relaxed) {
                let r = get(&app, uri).await;
                if r.status == StatusCode::SERVICE_UNAVAILABLE {
                    assert_eq!(r.headers.get("retry-after").unwrap(), "5");
                    assert_eq!(r.body["code"], "dataset-restoring", "{}", r.body);
                }
                *seen.entry(r.status.as_u16()).or_default() += 1;
            }
            seen
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    // a long request holds the dataset for a while: the swap drains it
    let held = s.st.get("ds").unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    let st = s.st.clone();
    let t = tmp.clone();
    let ds =
        tokio::task::spawn_blocking(move || swap::replace_in_place(&st, "ds", &t, "77", false))
            .await
            .unwrap()
            .unwrap();
    release.join().unwrap();
    assert_eq!(ds.store.dataset_id(), new_id);
    drop(ds);
    tokio::time::sleep(Duration::from_millis(50)).await;
    stop.store(true, Ordering::Relaxed);
    let mut total: BTreeMap<u16, usize> = BTreeMap::new();
    for p in pollers {
        for (k, v) in p.await.unwrap() {
            *total.entry(k).or_default() += v;
        }
    }
    assert!(
        total.keys().all(|s| *s == 200 || *s == 503),
        "statuses seen: {total:?}"
    );
    assert!(total.get(&503).is_some_and(|n| *n > 0), "{total:?}");
    let r = get(&s.app, "/$/datasets/ds").await;
    assert_eq!(r.body["id"], new_id.to_string().as_str());
    assert_eq!(r.body["quads"], 3);
    assert!(!tmp.exists());
}

#[tokio::test]
async fn restored_datasets_name_their_backup() {
    let s = server(Opts::default());
    let ds = s.st.get("wiki").unwrap();
    let root = ds.store.root().unwrap().to_path_buf();
    let src = uuid::Uuid::new_v4();
    let rec = sparkles_backup::RestoreRecord {
        restore_format: 1,
        repository: sparkles_backup::RestoreRepository {
            name: "local".into(),
            id: uuid::Uuid::new_v4(),
        },
        backup: "b1".into(),
        source: sparkles_backup::RestoreSource {
            dataset_id: src,
            seq: 3,
            name: "wiki".into(),
        },
        identity: "new".into(),
        time: sparkles_backup::now_rfc3339(),
    };
    std::fs::write(root.join("restore.json"), serde_json::to_vec(&rec).unwrap()).unwrap();
    let r = get(&s.app, "/$/datasets/wiki").await;
    assert_eq!(
        r.body["restoredFrom"],
        json!({"repository": "local", "backup": "b1", "datasetId": src, "seq": 3})
    );
    let r = get(&s.app, "/$/datasets/ds").await;
    assert!(r.body.get("restoredFrom").is_none(), "{}", r.body);
}

#[tokio::test]
async fn metrics_include_the_backup_families() {
    let s = server(Opts::default());
    s.b().metrics.capture_lock(Duration::from_micros(300));
    let r = get(&s.app, "/$/metrics").await;
    let text = r.body.as_str().unwrap_or_default().to_string();
    for f in [
        "# TYPE sparkles_backup_operations_total counter",
        "# TYPE sparkles_backup_last_success_timestamp_seconds gauge",
        "sparkles_backup_capture_lock_seconds_count 1",
        "# TYPE sparkles_backup_policy_runs_total counter",
    ] {
        assert!(text.contains(f), "{f} missing");
    }
}

#[test]
fn one_server_per_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!data_dir_in_use(dir.path()));
    let lock = lock_data_dir(dir.path()).unwrap();
    assert!(data_dir_in_use(dir.path()));
    let e = lock_data_dir(dir.path()).unwrap_err();
    assert!(
        format!("{e:#}").contains("in use by another sparkles server"),
        "{e:#}"
    );
    drop(lock);
    assert!(!data_dir_in_use(dir.path()));
    let _again = lock_data_dir(dir.path()).unwrap();
}

// ---------------------------------------------------------- permissions ------

#[cfg(feature = "auth")]
#[tokio::test]
async fn permissions_follow_the_route_table_and_the_handlers() {
    let s = server(Opts {
        auth: true,
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    let app = &s.app;
    // carol administers wiki*: names and types only
    let r = call(app, "GET", "/$/repositories", Some("carol"), J::Null).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body["repositories"],
        json!([{"name": "local", "type": "fs", "readonly": false, "reachable": false}])
    );
    let r = call(
        app,
        "POST",
        "/$/repositories",
        Some("carol"),
        fs_repo("x", "/srv/x"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // dave reads wiki: no repositories, but wiki's backups
    let r = call(app, "GET", "/$/repositories", Some("dave"), J::Null).await;
    assert_eq!(r.body["repositories"], json!([]));
    let r = call(app, "GET", "/$/backups/wiki", Some("dave"), J::Null).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = call(
        app,
        "POST",
        "/$/backups/wiki",
        Some("dave"),
        json!({"repository": "local"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(app, "GET", "/$/backups/secret", Some("dave"), J::Null).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // alice sees everything
    let r = call(app, "GET", "/$/repositories", Some("alice"), J::Null).await;
    assert_eq!(r.body["repositories"][0]["path"], "/srv/r");
    let r = call(
        app,
        "POST",
        "/$/repositories",
        Some("alice"),
        fs_repo("Bad", "/srv/x"),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-name");
}

// --------------------------------------------------- end to end (engine) ------

/// A server whose `ds` has commits 1–3 of the acceptance setup, and repository `local`
/// registered at a fresh directory.
async fn with_local(read_only_repo: bool) -> (Srv, tempfile::TempDir) {
    let s = server(Opts::default());
    let ds = s.st.get("ds").unwrap();
    // `ds` holds commit 1 already (INSERT <urn:ds>); rebuild the acceptance state
    for u in [
        "DELETE DATA { <urn:ds> <urn:p> 1 }",
        "INSERT DATA { <urn:a> <urn:p> 1 }",
        "INSERT DATA { <urn:b> <urn:p> 2 }",
    ] {
        sparkles::sparql::update::update(&ds.store, u, &QueryOptions::default()).unwrap();
    }
    start(&s.st, &Handle::current());
    let repo = tempfile::tempdir().unwrap();
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "local", "type": "fs", "path": repo.path(), "readonly": read_only_repo}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    (s, repo)
}

async fn run(s: &Srv, uri: &str, body: J) -> Task {
    let r = post(&s.app, uri, body).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{uri}: {}", r.body);
    wait_task(&s.st, r.body["id"].as_str().unwrap()).await
}

fn update(s: &Srv, ds: &str, u: &str) {
    let d = s.st.get(ds).unwrap();
    sparkles::sparql::update::update(&d.store, u, &QueryOptions::default()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs S2/S5"]
async fn e2e_register_and_test() {
    let (s, repo) = with_local(false).await;
    let r = get(&s.app, "/$/repositories/local").await;
    assert_eq!(r.body["status"]["reachable"], true, "{}", r.body);
    let marker: J =
        serde_json::from_slice(&std::fs::read(repo.path().join("sparkles-repo.json")).unwrap())
            .unwrap();
    assert_eq!(marker["format"], 1);
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "local2", "type": "fs", "path": repo.path()}),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "repository-exists");
    let junk = tempfile::tempdir().unwrap();
    std::fs::write(junk.path().join("x"), b"x").unwrap();
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "junk", "type": "fs", "path": junk.path()}),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "not-a-repository");
    let r = post(&s.app, "/$/repositories", fs_repo("rel", "relative/dir")).await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    let inside = s.dir.path().join("inside");
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "inside", "type": "fs", "path": inside}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    let r = post(&s.app, "/$/repositories/local/test", J::Null).await;
    assert_eq!(r.body["ok"], true);
    assert_eq!(r.body["conditionalWrites"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs S2/S5"]
async fn e2e_backup_restore_delete_and_metrics() {
    let (s, repo) = with_local(false).await;
    let ds_id = s.st.get("ds").unwrap().store.dataset_id();
    // first backup
    let r = post(
        &s.app,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b1"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(r.headers.get("location").unwrap(), "/$/backups/ds/local/b1");
    let t = wait_task(&s.st, r.body["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let d = t.detail.unwrap();
    assert_eq!(d["commit"]["seq"], 3);
    let r = get(&s.app, "/$/backups/ds/local/b1").await;
    assert_eq!(r.body["dataset"]["id"], ds_id.to_string().as_str());
    let files: Vec<&str> = r.body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for f in ["CURRENT", "commits.bin", "gen-0001/wal.log"] {
        assert!(files.contains(&f), "{f} in {files:?}");
    }
    assert_eq!(files.iter().filter(|f| f.ends_with(".dat")).count(), 7);
    assert_eq!(files.iter().filter(|f| f.ends_with(".meta")).count(), 7);
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("text/") || *f == "sparkles.lock")
    );
    // a second backup, and a restore to a new name
    update(&s, "ds", "INSERT DATA { <urn:c> <urn:p> 3 }");
    assert_eq!(
        run(
            &s,
            "/$/backups/ds",
            json!({"repository": "local", "name": "b2"})
        )
        .await
        .state,
        "done"
    );
    let t = run(
        &s,
        "/$/backups/ds/local/b1/restore",
        json!({"target": "ds-r"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = get(&s.app, "/$/datasets/ds-r").await;
    assert_eq!(r.body["restoredFrom"]["backup"], "b1");
    assert_eq!(r.body["forkedFrom"], json!({"id": ds_id, "seq": 3}));
    assert_ne!(r.body["id"], ds_id.to_string().as_str());
    assert_eq!(r.body["head"], 3);
    // conflicts
    let r = post(
        &s.app,
        "/$/backups/ds/local/b1/restore",
        json!({"target": "ds-r"}),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "dataset-exists");
    let r = post(&s.app, "/$/backups/wiki/local/b1", J::Null).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    let r = get(&s.app, "/$/backups/wiki/local/b1").await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    // in place: head 6 → 3, a new id
    for u in [
        "INSERT DATA { <urn:d> <urn:p> 4 }",
        "INSERT DATA { <urn:e> <urn:p> 5 }",
    ] {
        update(&s, "ds", u);
    }
    let r = post(
        &s.app,
        "/$/backups/ds/local/b1/restore",
        json!({"replace": true, "identity": "keep"}),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "duplicate-dataset-id");
    let t = run(
        &s,
        "/$/backups/ds/local/b1/restore",
        json!({"replace": true}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = get(&s.app, "/$/datasets/ds").await;
    assert_eq!(r.body["head"], 3);
    assert_eq!(r.body["forkedFrom"]["seq"], 3);
    assert!(
        std::fs::read_dir(s.dir.path().join("databases"))
            .unwrap()
            .all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".replaced-"))
    );
    // delete, verify, re-create
    let r = call(&s.app, "DELETE", "/$/backups/ds/local/b1", None, J::Null).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    assert!(!repo.path().join("backups").join("b1.json").exists());
    let t = run(
        &s,
        "/$/backups/ds/local/b2/verify",
        json!({"level": "data"}),
    )
    .await;
    assert_eq!(t.detail.unwrap()["status"], "ok");
    let r = get(&s.app, "/$/repositories/local/backups").await;
    assert_eq!(r.body["backups"][0]["verified"]["level"], "data");
    // metrics
    let r = get(&s.app, "/$/metrics").await;
    let text = r.body.as_str().unwrap().to_string();
    assert!(text.contains("sparkles_backup_operations_total{repository=\"local\",operation=\"create\",result=\"ok\"} 2"), "{text}");
    assert!(text.contains(
        "sparkles_backup_last_success_timestamp_seconds{dataset=\"ds\",repository=\"local\"}"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs S2/S5"]
async fn e2e_cancel_and_conflicts() {
    let (s, _repo) = with_local(false).await;
    let ds = s.st.get("ds").unwrap();
    let mut data = String::new();
    for i in 0..30_000 {
        data.push_str(&format!(
            "<urn:s{i}> <urn:p> \"{i} some padding text to make it larger\" .\n"
        ));
    }
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let mut cfg = get(&s.app, "/$/repositories/local").await.body;
    cfg["maxUploadBytesPerSec"] = json!(1_048_576);
    let r = call(&s.app, "PUT", "/$/repositories/local", None, cfg).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = post(
        &s.app,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b3"}),
    )
    .await;
    let id = r.body["id"].as_str().unwrap().to_string();
    let again = post(
        &s.app,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b4"}),
    )
    .await;
    expect(&again, StatusCode::CONFLICT, "backup-in-progress");
    assert_eq!(again.body["task"], id.as_str());
    let r = call(&s.app, "DELETE", &format!("/$/tasks/{id}"), None, J::Null).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(wait_task(&s.st, &id).await.state, "cancelled");
    let r = get(&s.app, "/$/backups/ds/local/b3").await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    let t = run(
        &s,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b3"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs S2/S5"]
async fn e2e_disaster_recovery_from_a_read_only_repository() {
    let (a, repo) = with_local(false).await;
    let id = a.st.get("ds").unwrap().store.dataset_id();
    assert_eq!(
        run(
            &a,
            "/$/backups/ds",
            json!({"repository": "local", "name": "b1"})
        )
        .await
        .state,
        "done"
    );
    update(&a, "ds", "INSERT DATA { <urn:c> <urn:p> 3 }");
    assert_eq!(
        run(
            &a,
            "/$/backups/ds",
            json!({"repository": "local", "name": "b2"})
        )
        .await
        .state,
        "done"
    );
    let locks_before = std::fs::read_dir(repo.path().join("locks")).map_or(0, |d| d.count());
    // server B: a fresh data directory, the repository read-only
    let b = server(Opts {
        repos: vec![json!({"name": "local", "type": "fs", "path": repo.path(), "readonly": true})],
        ..Default::default()
    });
    start(&b.st, &Handle::current());
    b.st.delete("ds").unwrap();
    let r = get(&b.app, "/$/repositories/local/backups?dataset=ds").await;
    let names: Vec<&str> = r.body["backups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["b2", "b1"]);
    let t = run(&b, "/$/backups/ds/local/b2/restore", json!({})).await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = get(&b.app, "/$/datasets/ds").await;
    assert_eq!(r.body["id"], id.to_string().as_str());
    assert_eq!(r.body["head"], 4);
    let r = post(&b.app, "/$/backups/ds", json!({"repository": "local"})).await;
    expect(&r, StatusCode::CONFLICT, "repository-read-only");
    let locks_after = std::fs::read_dir(repo.path().join("locks")).map_or(0, |d| d.count());
    assert_eq!(locks_before, locks_after);
}
