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
    #[cfg(feature = "auth")]
    auth: bool,
    read_only: bool,
    /// `repositories.json` entries
    repos: Vec<J>,
    /// a backup config file (loaded without the checks of `BackupState::new`)
    config: Option<&'static str>,
    max_tasks: usize,
    /// `--min-free-disk-mb` (default: the server's default)
    min_free_disk: Option<u64>,
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

#[cfg(feature = "auth")]
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
    if let Some(m) = o.min_free_disk {
        st.limits.min_free_disk_bytes = Some(m);
    }
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
    // JSON, but not a configuration
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

/// Unsupported encryption metadata must not be projected into a plaintext config.
#[tokio::test]
async fn encryption_requests_never_register_or_replace_plaintext_repositories() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    let destination = tempfile::tempdir().unwrap();
    let untouched = destination.path().join("must-not-be-created");
    let registry_file = s
        .dir
        .path()
        .join("backup")
        .join(registry::REPOSITORIES_FILE);
    let original = std::fs::read(&registry_file).unwrap();
    for encryption in [
        json!({"keys": [{"source": "env", "name": "PRIVATE_PROVIDER_REFERENCE"}]}),
        J::Bool(false),
        J::Null,
    ] {
        let mut body = fs_repo("secure", untouched.to_str().unwrap());
        body["encryption"] = encryption.clone();
        let response = post(&s.app, "/$/repositories", body).await;
        expect(&response, StatusCode::BAD_REQUEST, "invalid-config");
        assert!(
            !response
                .body
                .to_string()
                .contains("PRIVATE_PROVIDER_REFERENCE")
        );
        assert!(!untouched.exists());
        assert_eq!(s.b().registry.repos.read().len(), 1);

        let mut body = fs_repo("local", "/srv/r");
        body["readonly"] = J::Bool(true);
        body["encryption"] = encryption;
        let response = call(&s.app, "PUT", "/$/repositories/local", None, body).await;
        expect(&response, StatusCode::BAD_REQUEST, "invalid-config");
        assert!(
            !response
                .body
                .to_string()
                .contains("PRIVATE_PROVIDER_REFERENCE")
        );
        assert!(!s.b().registry.repos.read()["local"].config.readonly);
        assert_eq!(std::fs::read(&registry_file).unwrap(), original);
    }
}

/// A body that is not JSON answers `400 invalid-request` on every route that reads one.
#[tokio::test]
async fn malformed_bodies_are_invalid_requests() {
    let s = server(Opts {
        repos: vec![fs_repo("local", "/srv/r")],
        ..Default::default()
    });
    for (method, uri) in [
        ("POST", "/$/repositories"),
        ("PUT", "/$/repositories/local"),
        ("POST", "/$/repositories/local/verify"),
        ("POST", "/$/repositories/local/gc"),
        ("POST", "/$/backups/ds"),
        ("POST", "/$/backups/ds/local/b1/restore"),
        ("POST", "/$/backups/ds/local/b1/verify"),
        ("POST", "/$/backup-policies"),
        ("POST", "/$/backup-policies/preview"),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())))
            .body(Body::from("{\"name\": "))
            .unwrap();
        let res = s.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: J = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {body}");
        assert_eq!(body["code"], "invalid-request", "{method} {uri}: {body}");
    }
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
    // the others pass the server check (in-memory datasets too)
    let r = post(
        &s.app,
        "/$/backups/mem",
        json!({"repository": "local", "name": "no spaces"}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-name");
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
    expect(&r, StatusCode::BAD_REQUEST, "invalid-request");
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

#[test]
fn a_request_past_the_guard_does_not_repopulate_drained_restore_routing() {
    let s = server(Opts::default());
    let held = s.st.catalog.get_for_request("ds").unwrap().unwrap();
    let reservation =
        s.st.catalog
            .reserve("ds", sparkles::catalog::ReservationKind::Restore, "78")
            .unwrap();
    let tmp = s.dir.path().join("databases/.restore-ds-78");
    let id = restored_copy(&tmp, 3);
    s.st.forget_routing("ds");
    // The accepted request only now asks the server for its routing object.
    drop(s.st.get("ds").unwrap());
    drop(held);
    let restored =
        s.st.catalog
            .replace_restored_reserved(reservation, &tmp, "78", false)
            .unwrap();
    assert_eq!(restored.dataset_id(), id);
    assert_eq!(s.st.get("ds").unwrap().store.dataset_id(), id);
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
                // In-process router calls can finish without yielding, unlike HTTP, so
                // a poller that never yields would keep its worker from other tasks.
                tokio::task::yield_now().await;
            }
            seen
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    // a long request holds the dataset: the swap drains it, and requests that arrive
    // meanwhile are answered 503
    let held = s.st.get("ds").unwrap();
    let st = s.st.clone();
    let t = tmp.clone();
    let swap =
        tokio::task::spawn_blocking(move || swap::replace_in_place(&st, "ds", &t, "77", false));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while s.st.catalog.restoring_by("ds").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the swap never started"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let r = get(&s.app, "/$/datasets/ds").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.body);
    assert_eq!(r.body["code"], "dataset-restoring");
    drop(held);
    let ds = swap.await.unwrap().unwrap();
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
    ] {
        assert!(text.contains(f), "{f} missing");
    }
}

#[test]
fn one_server_per_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let e = sparkles::Catalog::open(dir.path(), Default::default())
        .err()
        .unwrap();
    assert!(
        matches!(e, sparkles::Error::Locked { pid: Some(pid), .. } if pid == std::process::id()),
        "{e:#}"
    );
    drop(catalog);
    let _again = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
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
    // a fresh `ds` (the fixture's has a commit already) with three commits, holding
    // `<urn:b> <urn:p> 2` at head 3
    assert!(s.st.delete("ds").unwrap());
    let ds = s.st.create("ds", DbType::Persistent).unwrap();
    for u in [
        "INSERT DATA { <urn:a> <urn:p> 1 }",
        "INSERT DATA { <urn:b> <urn:p> 2 }",
        "DELETE DATA { <urn:a> <urn:p> 1 }",
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
    // a fresh repository: about everything is uploaded
    let (added, logical) = (
        d["addedBytes"].as_u64().unwrap(),
        d["logicalBytes"].as_u64().unwrap(),
    );
    assert!(added > logical / 2 && added <= logical + 4096, "{d}");
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
    // the seven index permutations (and the vocabulary's `vocab.dat`)
    let index = |ext: &str| {
        files
            .iter()
            .filter(|f| f.ends_with(ext) && !f.ends_with("/vocab.dat"))
            .count()
    };
    assert_eq!(index(".dat"), 7, "{files:?}");
    assert_eq!(index(".meta"), 7, "{files:?}");
    assert!(files.contains(&"gen-0001/vocab.dat"), "{files:?}");
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("text/") || *f == "sparkles.lock")
    );
    // a second backup, and a restore to a new name
    update(&s, "ds", "INSERT DATA { <urn:c> <urn:p> 3 }");
    let t = run(
        &s,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b2"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    // incremental: only the appended bytes and changed meta files
    let d = t.detail.unwrap();
    assert_eq!(d["commit"]["seq"], 4);
    assert!(d["addedBytes"].as_u64().unwrap() < 10_000, "{d}");
    let b2 = get(&s.app, "/$/backups/ds/local/b2").await.body;
    assert!(
        b2["stats"]["newBlobs"].as_u64().unwrap() <= 5,
        "{}",
        b2["stats"]
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
    let r = get(
        &s.app,
        "/ds-r/sparql?query=SELECT%20%3Fs%20%7B%3Fs%20%3Fp%20%3Fo%7D",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.headers["sparkles-commit"], "3");
    assert_ne!(r.headers["sparkles-dataset-id"], ds_id.to_string().as_str());
    let rows = r.body["results"]["bindings"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{}", r.body);
    assert_eq!(rows[0]["s"]["value"], "urn:b");
    let r = get(&s.app, "/$/commits/ds-r").await;
    let seqs: Vec<u64> = r.body["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [3, 2, 1, 0], "{}", r.body);
    update(&s, "ds-r", "INSERT DATA { <urn:x> <urn:p> 9 }");
    assert_eq!(s.st.get("ds-r").unwrap().store.head_commit().seq, 4);
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
    let new_id = r.body["id"].as_str().unwrap().to_string();
    assert_ne!(new_id, ds_id.to_string());
    update(&s, "ds", "INSERT DATA { <urn:f> <urn:p> 6 }");
    let d = s.st.get("ds").unwrap();
    assert_eq!(d.store.head_commit().seq, 4);
    assert_eq!(d.store.dataset_id().to_string(), new_id);
    drop(d);
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
    // the name can be used again
    let t = run(
        &s,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b1"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = get(&s.app, "/$/backups/ds/local/b1").await;
    assert_eq!(r.body["dataset"]["id"], new_id.as_str());
    // metrics
    let r = get(&s.app, "/$/metrics").await;
    let text = r.body.as_str().unwrap().to_string();
    assert!(text.contains("sparkles_backup_operations_total{repository=\"local\",operation=\"create\",result=\"ok\"} 3"), "{text}");
    assert!(text.contains(
        "sparkles_backup_last_success_timestamp_seconds{dataset=\"ds\",repository=\"local\"}"
    ));
    // the object requests of every operation: the uploads of the backups, the
    // downloads of the restores, the listings
    let requests = |op: &str| -> u64 {
        let prefix = format!(
            "sparkles_backup_object_requests_total{{repository=\"local\",op=\"{op}\",result=\"ok\"}} "
        );
        text.lines()
            .find_map(|l| l.strip_prefix(prefix.as_str()))
            .map_or(0, |n| n.parse().unwrap())
    };
    for op in ["put", "get", "head", "list", "delete"] {
        assert!(requests(op) > 0, "no {op} requests counted in\n{text}");
    }
}

/// An in-memory dataset is backed up through a temporary copy, verified, and
/// restored as a new persistent dataset with its validation and prefixes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_in_memory_datasets_back_up_and_restore() {
    let (s, _repo) = with_local(false).await;
    let mem = s.st.get("mem").unwrap();
    for i in 0..20 {
        update(
            &s,
            "mem",
            &format!("INSERT DATA {{ <urn:m{i}> a <urn:T> ; <urn:p> {i} }}"),
        );
    }
    mem.store.set_prefix("ex", "http://example.org/").unwrap();
    #[cfg(feature = "shacl")]
    {
        let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
            <urn:S> a sh:NodeShape ; sh:targetClass <urn:T> ;\n\
            sh:property [ sh:path <urn:p> ; sh:minCount 1 ] .";
        let r = call(
            &s.app,
            "PUT",
            "/$/validation/mem",
            None,
            json!({"mode": "reject", "shapes": {"inline": shapes}}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }
    let head = mem.store.head_commit().seq;
    let t = run(
        &s,
        "/$/backups/mem",
        json!({"repository": "local", "name": "m1"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let d = t.detail.unwrap();
    assert_eq!(d["commit"]["seq"], head);
    assert_eq!(d["commit"]["quads"], 40);
    assert_eq!(d["dataset"]["id"], mem.store.dataset_id().to_string());
    let m = get(&s.app, "/$/backups/mem/local/m1").await.body;
    assert_eq!(m["dataset"]["type"], "mem");
    let files: Vec<&str> = m["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for f in [
        "CURRENT",
        "commits.bin",
        "prefixes.json",
        "gen-0001/spo.dat",
    ] {
        assert!(files.contains(&f), "{f} in {files:?}");
    }
    #[cfg(feature = "shacl")]
    assert!(files.contains(&"validation.json"), "{files:?}");
    // the temporary copy is gone
    let tmp = s.dir.path().join("tmp");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    // listed under the dataset
    let r = get(&s.app, "/$/backups/mem").await;
    assert_eq!(r.body["backups"][0]["name"], "m1", "{}", r.body);
    let t = run(
        &s,
        "/$/backups/mem/local/m1/verify",
        json!({"level": "restore"}),
    )
    .await;
    assert_eq!(t.detail.unwrap()["status"], "ok");
    // the in-memory dataset cannot be replaced
    let r = post(
        &s.app,
        "/$/backups/mem/local/m1/restore",
        json!({"target": "mem", "replace": true}),
    )
    .await;
    expect(&r, StatusCode::CONFLICT, "not-managed");
    // a new persistent dataset; the live dataset has the id, so the copy gets a new one
    let t = run(
        &s,
        "/$/backups/mem/local/m1/restore",
        json!({"target": "mem-r"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = get(&s.app, "/$/datasets/mem-r").await;
    assert_eq!(r.body["type"], "persistent", "{}", r.body);
    assert_eq!(r.body["head"], head);
    assert_eq!(
        r.body["forkedFrom"],
        json!({"id": mem.store.dataset_id(), "seq": head})
    );
    let restored = s.st.get("mem-r").unwrap();
    assert_eq!(restored.store.snapshot().len(), 40);
    assert_eq!(
        restored.store.prefixes().get("ex").map(String::as_str),
        Some("http://example.org/")
    );
    #[cfg(feature = "shacl")]
    {
        let r = get(&s.app, "/$/validation/mem-r").await;
        assert_eq!(r.body["config"]["mode"], "reject", "{}", r.body);
        let u = "INSERT DATA { <urn:bad> a <urn:T> }";
        let r = sparkles::sparql::update::update(&restored.store, u, &QueryOptions::default());
        assert!(r.is_err(), "the restored validation rejects a violation");
    }
    // a second, unchanged backup reuses the pieces of the first
    let t = run(
        &s,
        "/$/backups/mem",
        json!({"repository": "local", "name": "m2"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let d = t.detail.unwrap();
    assert!(
        d["addedBytes"].as_u64().unwrap() < d["logicalBytes"].as_u64().unwrap() / 2,
        "{d}"
    );
    // the Fuseki-style N-Quads dump works on in-memory datasets too
    let r = post(&s.app, "/$/backup/mem", J::Null).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    let t = wait_task(&s.st, r.body["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done", "{:?}", t.message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    // cancel once some blobs are uploaded
    let t0 = Instant::now();
    while !s.st.tasks.lock().iter().any(|t| {
        t.id == id
            && t.message
                .as_deref()
                .is_some_and(|m| m.contains(" new blob") && !m.contains(" 0 new blobs"))
    }) {
        assert!(t0.elapsed() < Duration::from_secs(30), "no upload progress");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let r = call(&s.app, "DELETE", &format!("/$/tasks/{id}"), None, J::Null).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(wait_task(&s.st, &id).await.state, "cancelled");
    let r = get(&s.app, "/$/backups/ds/local/b3").await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    let r = get(&s.app, "/$/backups/ds").await;
    assert_eq!(r.body["backups"], json!([]), "{}", r.body);
    // the second attempt reuses what the first uploaded
    let t = run(
        &s,
        "/$/backups/ds",
        json!({"repository": "local", "name": "b3"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = get(&s.app, "/$/backups/ds/local/b3").await;
    assert!(
        r.body["stats"]["reusedBlobs"].as_u64().unwrap() > 0,
        "{}",
        r.body["stats"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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

/// The blob file of the first piece of `path` in backup `name` of `ds`.
async fn blob_file(s: &Srv, repo: &std::path::Path, name: &str, path: &str) -> (String, PathBuf) {
    let m = get(&s.app, &format!("/$/backups/ds/local/{name}"))
        .await
        .body;
    let id = m["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap()["blobs"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let file = repo.join(format!("blobs/{}/{id}", &id[..2]));
    (id, file)
}

/// Back up `ds` into `local` as `name`; the task must succeed.
async fn backup_ds(s: &Srv, name: &str) {
    let t = run(
        s,
        "/$/backups/ds",
        json!({"repository": "local", "name": name}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
}

async fn delete_ds_backup(s: &Srv, name: &str) {
    let r = call(
        &s.app,
        "DELETE",
        &format!("/$/backups/ds/local/{name}"),
        None,
        J::Null,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_damaged_blobs_fail_verifications_and_restores() {
    let (s, repo) = with_local(false).await;
    backup_ds(&s, "b1").await;
    update(&s, "ds", "INSERT DATA { <urn:c> <urn:p> 3 }");
    backup_ds(&s, "b2").await;
    let (id, file) = blob_file(&s, repo.path(), "b1", "gen-0001/spo.dat").await;
    let saved = std::fs::read(&file).unwrap();
    // a missing blob: both backups share it
    std::fs::remove_file(&file).unwrap();
    let t = run(
        &s,
        "/$/backups/ds/local/b1/verify",
        json!({"level": "exists"}),
    )
    .await;
    let d = t.detail.unwrap();
    assert_eq!(d["status"], "error", "{d}");
    assert_eq!(d["backups"][0]["missing"], json!([id]));
    let t = run(
        &s,
        "/$/repositories/local/verify",
        json!({"level": "exists"}),
    )
    .await;
    let d = t.detail.unwrap();
    assert_eq!(d["status"], "error", "{d}");
    assert!(
        d["backups"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["missing"] == json!([id])),
        "{d}"
    );
    // a flipped byte: present, but not intact
    let mut bad = saved.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    std::fs::write(&file, &bad).unwrap();
    let t = run(
        &s,
        "/$/backups/ds/local/b1/verify",
        json!({"level": "exists"}),
    )
    .await;
    assert_eq!(t.detail.unwrap()["status"], "ok");
    let t = run(
        &s,
        "/$/backups/ds/local/b1/verify",
        json!({"level": "data"}),
    )
    .await;
    let d = t.detail.unwrap();
    assert_eq!(d["backups"][0]["corrupt"], json!([id]), "{d}");
    // a restore of it fails and leaves nothing behind
    let t = run(&s, "/$/backups/ds/local/b1/restore", json!({"target": "x"})).await;
    assert_eq!(t.state, "failed");
    let msg = t.message.unwrap_or_default();
    assert!(msg.contains(&id), "{msg}");
    assert!(s.st.get("x").is_none());
    let r = get(&s.app, "/$/datasets/x").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let left: Vec<String> = std::fs::read_dir(s.dir.path().join("databases"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n == "x" || n.starts_with(".restore-x-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // repaired, a verification by restoring passes with a check report
    std::fs::write(&file, &saved).unwrap();
    let t = run(
        &s,
        "/$/backups/ds/local/b1/verify",
        json!({"level": "restore"}),
    )
    .await;
    let d = t.detail.unwrap();
    assert_eq!(d["status"], "ok", "{d}");
    assert_eq!(d["backups"][0]["check"]["status"], "ok", "{d}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_gc_collects_the_blobs_of_deleted_backups() {
    let (s, _repo) = with_local(false).await;
    backup_ds(&s, "b1").await;
    update(&s, "ds", "INSERT DATA { <urn:c> <urn:p> 3 }");
    backup_ds(&s, "b2").await;
    delete_ds_backup(&s, "b1").await;
    // after a compaction, a backup of the new generation; then nothing needs gen-0001
    s.st.get("ds").unwrap().store.compact().unwrap();
    backup_ds(&s, "b4").await;
    let b4 = get(&s.app, "/$/backups/ds/local/b4").await.body;
    assert_eq!(b4["generation"], "gen-0002");
    delete_ds_backup(&s, "b2").await;
    let t = run(
        &s,
        "/$/repositories/local/gc",
        json!({"dryRun": true, "graceHours": 0}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let dry = t.detail.unwrap();
    assert!(dry["candidates"].as_u64().unwrap() > 0, "{dry}");
    let r = get(&s.app, "/$/repositories/local").await;
    assert!(r.body["lastGc"].is_null(), "{}", r.body);
    let t = run(&s, "/$/repositories/local/gc", json!({"graceHours": 0})).await;
    let real = t.detail.unwrap();
    assert_eq!(real["deleted"], dry["candidates"], "{real}");
    assert_eq!(real["deletedBytes"], dry["deletedBytes"], "{real}");
    assert_eq!(real["storedBytesAfter"], dry["storedBytesAfter"], "{real}");
    let r = get(&s.app, "/$/repositories/local").await;
    assert_eq!(r.body["lastGc"]["deleted"], real["deleted"], "{}", r.body);
    let t = run(
        &s,
        "/$/backups/ds/local/b4/verify",
        json!({"level": "data"}),
    )
    .await;
    assert_eq!(t.detail.unwrap()["status"], "ok");
    // (a backup taken right after a compaction restores too)
    let t = run(
        &s,
        "/$/backups/ds/local/b4/verify",
        json!({"level": "restore"}),
    )
    .await;
    let d = t.detail.unwrap();
    assert_eq!(d["status"], "ok", "{d}");
    // with the default grace period, a fresh orphan stays
    update(&s, "ds", "INSERT DATA { <urn:d> <urn:p> 4 }");
    backup_ds(&s, "b5").await;
    delete_ds_backup(&s, "b5").await;
    let t = run(&s, "/$/repositories/local/gc", json!({})).await;
    let d = t.detail.unwrap();
    assert!(d["keptYoung"].as_u64().unwrap() >= 1, "{d}");
    assert_eq!(d["deleted"], 0, "{d}");
}

// ------------------------------------------------ API registrations (security) ------

/// Load backup config `text` into the server's registry (as a SIGHUP reload would).
fn load_config(s: &Srv, text: &str) {
    s.b()
        .registry
        .replace_config(&config::ConfigFile::parse(text).unwrap())
        .unwrap();
}

/// An HTTP server on 127.0.0.1 answering every request with `403` and an S3 error
/// body holding a secret; its port.
async fn forbidding_endpoint() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut c, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf).await;
                let body = "<Error><Code>AccessDenied</Code><Message>internal-secret-123</Message></Error>";
                let res = format!(
                    "HTTP/1.1 403 Forbidden\r\ncontent-type: application/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = c.write_all(res.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_registrations_name_credential_sources_and_honour_the_outbound_policy() {
    let s = server(Opts {
        // registered before this server's rules: ambient credentials
        repos: vec![json!({"name": "old", "type": "s3", "bucket": "b",
            "credentials": {"source": "env", "accessKeyIdVar": "HOME", "secretAccessKeyVar": "PATH"}})],
        ..Default::default()
    });
    let keys = s.dir.path().join("keys.json");
    std::fs::write(&keys, r#"{"accessKeyId": "AK", "secretAccessKey": "SK"}"#).unwrap();
    load_config(
        &s,
        &format!(
            "version = 1\n[credentials.lab]\nsource = \"file\"\npath = {:?}\n",
            keys.display().to_string()
        ),
    );
    let port = forbidding_endpoint().await;
    let s3 = |endpoint: &str, credentials: J| {
        json!({"name": "lab", "type": "s3", "bucket": "b", "endpoint": endpoint,
            "allowHttp": true, "credentials": credentials})
    };
    let local = format!("http://127.0.0.1:{port}");
    let named = json!({"source": "named", "name": "lab"});
    // credentials the caller would pick: environment variables, files, the default chain
    for c in [
        json!({"source": "env", "accessKeyIdVar": "HOME", "secretAccessKeyVar": "PATH"}),
        json!({"source": "file", "path": "/etc/shadow"}),
        json!({"source": "default"}),
    ] {
        let r = post(&s.app, "/$/repositories", s3(&local, c)).await;
        expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
        assert_eq!(r.body["field"], "credentials", "{}", r.body);
    }
    let r = post(
        &s.app,
        "/$/repositories",
        s3(&local, json!({"source": "named", "name": "nope"})),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    assert_eq!(r.body["field"], "credentials.name");
    let r = post(
        &s.app,
        "/$/repositories",
        json!({"name": "g", "type": "gcs", "bucket": "b"}),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    // loopback, the metadata service and names resolving to them are refused by the
    // server's default outbound policy
    for e in [
        local.clone(),
        format!("http://localhost:{port}"),
        "http://169.254.169.254".to_string(),
        "http://[::ffff:127.0.0.1]:9000".to_string(),
    ] {
        let r = post(&s.app, "/$/repositories", s3(&e, named.clone())).await;
        expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
        assert!(
            r.body["error"]
                .as_str()
                .unwrap()
                .contains("outbound policy"),
            "{e}: {}",
            r.body
        );
    }
    assert_eq!(
        get(&s.app, "/$/repositories").await.body["repositories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // allowed by the policy: the endpoint is contacted, and its answer is not echoed
    *s.b().outbound.write() = sparkles::outbound::OutboundPolicy {
        allow_private: true,
        ..Default::default()
    };
    let r = post(&s.app, "/$/repositories", s3(&local, named.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["credentials"], named);
    assert_eq!(r.body["test"]["ok"], false, "{}", r.body);
    let text = r.body.to_string();
    assert!(text.contains("AccessDenied"), "{text}");
    assert!(!text.contains("internal-secret-123"), "{text}");
    let r = post(&s.app, "/$/repositories/lab/test", J::Null).await;
    assert!(
        !r.body.to_string().contains("internal-secret-123"),
        "{}",
        r.body
    );
    // a changed location is refused, other changes are checked like a registration
    let mut changed = s3(
        &local,
        json!({"source": "env", "accessKeyIdVar": "A", "secretAccessKeyVar": "B"}),
    );
    let r = call(&s.app, "PUT", "/$/repositories/lab", None, changed.clone()).await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    changed["credentials"] = named.clone();
    changed["maxConcurrency"] = json!(2);
    let r = call(&s.app, "PUT", "/$/repositories/lab", None, changed).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    // an entry with ambient credentials from before is not opened
    let r = post(&s.app, "/$/repositories/old/test", J::Null).await;
    assert_eq!(r.body["ok"], false);
    assert!(
        r.body.to_string().contains("credential source"),
        "{}",
        r.body
    );
}

#[tokio::test]
async fn fs_repositories_stay_out_of_the_servers_directories_and_under_the_api_roots() {
    let s = server(Opts::default());
    let outside = tempfile::tempdir().unwrap();
    // a symbolic link into the data directory
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(s.dir.path(), outside.path().join("link")).unwrap();
        let r = post(
            &s.app,
            "/$/repositories",
            fs_repo("sneaky", &format!("{}/link/repo", outside.path().display())),
        )
        .await;
        expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    }
    let r = post(
        &s.app,
        "/$/repositories",
        fs_repo("dots", &format!("{}/x/../../y", outside.path().display())),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    // with [api] fs_roots, only below them
    let root = outside.path().join("roots");
    std::fs::create_dir_all(&root).unwrap();
    load_config(
        &s,
        &format!(
            "version = 1\n[api]\nfs_roots = [{:?}]\n",
            root.display().to_string()
        ),
    );
    let r = post(
        &s.app,
        "/$/repositories",
        fs_repo("elsewhere", &format!("{}/else", outside.path().display())),
    )
    .await;
    expect(&r, StatusCode::BAD_REQUEST, "invalid-config");
    assert_eq!(r.body["field"], "path");
    let r = post(
        &s.app,
        "/$/repositories",
        fs_repo("inside", &format!("{}/r1", root.display())),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    // the directories of the server's config files
    let conf = tempfile::tempdir().unwrap();
    let file = conf.path().join("backup.toml");
    std::fs::write(&file, "version = 1\n").unwrap();
    let mut b = BackupState::new(s.dir.path(), Some(file.clone()), 1).unwrap();
    b.forbid_config_dir(&conf.path().join("auth.toml"));
    let cfg = RepoConfig {
        name: "c".into(),
        kind: RepoType::Fs,
        path: Some(conf.path().join("repo").display().to_string()),
        ..Default::default()
    };
    assert_eq!(
        cfg.validate(&b.forbid).unwrap_err().code(),
        Code::InvalidConfig
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backups_and_restores_keep_the_disk_reserve() {
    let (s, repo) = with_local(false).await;
    backup_ds(&s, "b1").await;
    // a second server on the same repository whose disk reserve cannot be met
    let full = server(Opts {
        repos: vec![fs_repo("local", repo.path().to_str().unwrap())],
        min_free_disk: Some(u64::MAX / 2),
        ..Default::default()
    });
    start(&full.st, &Handle::current());
    let r = post(&full.app, "/$/backups/ds", json!({"repository": "local"})).await;
    expect(&r, StatusCode::INSUFFICIENT_STORAGE, "insufficient-storage");
    assert!(
        r.body["error"]
            .as_str()
            .unwrap()
            .contains("--min-free-disk-mb"),
        "{}",
        r.body
    );
    let r = post(
        &full.app,
        "/$/backups/ds/local/b1/restore",
        json!({"target": "copy"}),
    )
    .await;
    expect(&r, StatusCode::INSUFFICIENT_STORAGE, "insufficient-storage");
    assert!(full.st.get("copy").is_none());
    assert!(full.st.tasks.lock().is_empty());
    // the task checks again (the free space may shrink while it waits)
    let st = full.st.clone();
    let o = sparkles_backup::CreateOptions {
        name: "b2".into(),
        ..Default::default()
    };
    let e = tokio::task::spawn_blocking(move || ops::create_for_policy(&st, "ds", "local", o))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(e.code(), Code::InsufficientStorage);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_tasks_beyond_the_queue_are_refused() {
    let (s, _repo) = with_local(false).await;
    let b = s.b();
    let held: Vec<Admission> = (0..b.max_tasks * (1 + QUEUE_PER_SLOT))
        .map(|_| b.admit().unwrap())
        .collect();
    assert_eq!(b.admit().err().unwrap().code(), Code::TooManyTasks);
    for (uri, body) in [
        ("/$/backups/ds", json!({"repository": "local"})),
        ("/$/repositories/local/gc", json!({})),
        ("/$/repositories/local/verify", json!({})),
    ] {
        let r = post(&s.app, uri, body).await;
        expect(&r, StatusCode::SERVICE_UNAVAILABLE, "too-many-tasks");
    }
    drop(held);
    backup_ds(&s, "b1").await;
}

// ---------------------------------------------- permissions and lineage (auth) ------

/// Run a request as `user` and wait for its task; the task.
#[cfg(feature = "auth")]
async fn run_as(s: &Srv, user: &str, uri: &str, body: J) -> Task {
    let r = call(&s.app, "POST", uri, Some(user), body).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{uri}: {}", r.body);
    wait_task(&s.st, r.body["id"].as_str().unwrap()).await
}

#[cfg(feature = "auth")]
fn names(r: &R) -> Vec<String> {
    r.body["backups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect()
}

#[cfg(feature = "auth")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_permissions_and_lineage() {
    let s = server(Opts {
        auth: true,
        ..Default::default()
    });
    start(&s.st, &Handle::current());
    let app = &s.app;
    let repo = tempfile::tempdir().unwrap();
    let r = call(
        app,
        "POST",
        "/$/repositories",
        Some("alice"),
        json!({"name": "local", "type": "fs", "path": repo.path()}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    // anonymous callers and callers without admin anywhere see no repositories
    let r = call(app, "GET", "/$/repositories", None, J::Null).await;
    assert!(
        r.status == StatusCode::UNAUTHORIZED || r.body["repositories"] == json!([]),
        "{} {}",
        r.status,
        r.body
    );
    let r = call(app, "GET", "/$/repositories", Some("dave"), J::Null).await;
    assert_eq!(r.body["repositories"], json!([]));
    let r = call(app, "GET", "/$/repositories", Some("carol"), J::Null).await;
    assert_eq!(
        r.body["repositories"],
        json!([{"name": "local", "type": "fs", "readonly": false, "reachable": true}])
    );
    // alice backs up secret; carol (admin on wiki*) backs up wiki
    let t = run_as(
        &s,
        "alice",
        "/$/backups/secret",
        json!({"repository": "local", "name": "s1"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let t = run_as(
        &s,
        "carol",
        "/$/backups/wiki",
        json!({"repository": "local", "name": "w1"}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    // the backup of secret is not reachable through wiki
    for m in ["GET", "DELETE"] {
        let r = call(app, m, "/$/backups/wiki/local/s1", Some("carol"), J::Null).await;
        expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    }
    let r = call(
        app,
        "POST",
        "/$/backups/wiki/local/s1/restore",
        Some("carol"),
        json!({"target": "wiki-s"}),
    )
    .await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    // a restore may only create a dataset its caller administers
    let r = call(
        app,
        "POST",
        "/$/backups/wiki/local/w1/restore",
        Some("carol"),
        json!({"target": "secret2"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert!(r.body.to_string().contains("/secret2"), "{}", r.body);
    // dave reads wiki: its backups, but no changes, and not carol's tasks
    let r = call(app, "GET", "/$/backups/wiki", Some("dave"), J::Null).await;
    assert_eq!(names(&r), ["w1"]);
    let r = call(
        app,
        "POST",
        "/$/backups/wiki",
        Some("dave"),
        json!({"repository": "local"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let slow = call(
        app,
        "POST",
        "/$/backups/wiki/local/w1/verify",
        Some("carol"),
        json!({"level": "restore"}),
    )
    .await;
    assert_eq!(slow.status, StatusCode::ACCEPTED, "{}", slow.body);
    let id = slow.body["id"].as_str().unwrap().to_string();
    let r = call(
        app,
        "DELETE",
        &format!("/$/tasks/{id}"),
        Some("dave"),
        J::Null,
    )
    .await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.status == StatusCode::NOT_FOUND,
        "{} {}",
        r.status,
        r.body
    );
    wait_task(&s.st, &id).await;
    // carol restores wiki in place: a new id, and its earlier backups still belong to it
    let t = run_as(
        &s,
        "carol",
        "/$/backups/wiki/local/w1/restore",
        json!({"replace": true}),
    )
    .await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let r = call(app, "GET", "/$/backups/wiki", Some("carol"), J::Null).await;
    assert_eq!(names(&r), ["w1"]);
    assert_eq!(r.body["backups"][0]["sameLineage"], true);
    // a new dataset under an old name does not inherit the old one's backups
    assert!(s.st.delete("wiki").unwrap());
    s.st.create("wiki", DbType::Persistent).unwrap();
    let r = call(app, "GET", "/$/backups/wiki", Some("carol"), J::Null).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(names(&r).is_empty(), "{}", r.body);
    let r = call(
        app,
        "GET",
        "/$/backups/wiki/local/w1",
        Some("carol"),
        J::Null,
    )
    .await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    let r = call(
        app,
        "POST",
        "/$/backups/wiki/local/w1/restore",
        Some("carol"),
        json!({"target": "wiki-old"}),
    )
    .await;
    expect(&r, StatusCode::NOT_FOUND, "no-such-backup");
    // a server admin still finds them by name, marked as another lineage
    let r = call(app, "GET", "/$/backups/wiki", Some("alice"), J::Null).await;
    assert_eq!(names(&r), ["w1"]);
    assert_eq!(r.body["backups"][0]["sameLineage"], false);
    // backups of a dataset that is gone: server admins only
    assert!(s.st.delete("wiki").unwrap());
    let r = call(app, "GET", "/$/backups/wiki", Some("carol"), J::Null).await;
    assert!(names(&r).is_empty(), "{}", r.body);
    let r = call(app, "GET", "/$/backups/wiki", Some("alice"), J::Null).await;
    assert_eq!(names(&r), ["w1"]);
    let t = run_as(&s, "alice", "/$/backups/wiki/local/w1/restore", json!({})).await;
    assert_eq!(t.state, "done", "{:?}", t.message);
}

#[cfg(feature = "auth")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dataset_admins_see_no_absolute_paths() {
    let s = server(Opts {
        auth: true,
        ..Default::default()
    });
    let app = &s.app;
    // a repository whose directory cannot be created: its errors name the path
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("file"), b"x").unwrap();
    let path = tmp.path().join("file").join("repo");
    let r = call(
        app,
        "POST",
        "/$/repositories?verify=false",
        Some("alice"),
        json!({"name": "gone", "type": "fs", "path": path}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let secret = tmp.path().display().to_string();
    let r = call(
        app,
        "GET",
        "/$/backups/wiki/gone/b1",
        Some("alice"),
        J::Null,
    )
    .await;
    expect(&r, StatusCode::BAD_GATEWAY, "repository-unavailable");
    assert!(
        r.body["error"].as_str().unwrap().contains(&secret),
        "{}",
        r.body
    );
    let r = call(
        app,
        "GET",
        "/$/backups/wiki/gone/b1",
        Some("carol"),
        J::Null,
    )
    .await;
    expect(&r, StatusCode::BAD_GATEWAY, "repository-unavailable");
    let msg = r.body["error"].as_str().unwrap();
    assert!(!msg.contains(&secret) && msg.contains("…/repo"), "{msg}");
    // and in task details
    let id = s.st.next_task_id();
    let root = format!("{secret}/file/repo");
    let t = s.st.start_task_opts(
        id.clone(),
        "backup-verify",
        "wiki",
        Some("b1"),
        false,
        move |h| {
            h.set_detail(json!({"check": {"root": root}}));
            Ok("done".into())
        },
    );
    wait_task(&s.st, &t.id).await;
    let r = call(
        app,
        "GET",
        &format!("/$/tasks/{id}"),
        Some("carol"),
        J::Null,
    )
    .await;
    assert_eq!(r.body["detail"]["check"]["root"], "…/repo", "{}", r.body);
    let r = call(
        app,
        "GET",
        &format!("/$/tasks/{id}"),
        Some("alice"),
        J::Null,
    )
    .await;
    assert!(
        r.body["detail"]["check"]["root"]
            .as_str()
            .unwrap()
            .contains(&secret)
    );
}

#[cfg(feature = "auth")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_branch_backups_follow_identity_and_authorization_after_rename() {
    let s = server(Opts {
        auth: true,
        ..Default::default()
    });
    start(&s.st, &Handle::current());
    let repo = tempfile::tempdir().unwrap();
    let r = call(
        &s.app,
        "POST",
        "/$/repositories",
        Some("alice"),
        json!({"name":"local", "type":"fs", "path":repo.path()}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let ds = s.st.get("wiki").unwrap();
    ds.dataset
        .create_branch("work", &Default::default())
        .unwrap();
    ds.dataset
        .branch("work")
        .unwrap()
        .update("INSERT DATA { <urn:branch> <urn:p> 2 }")
        .unwrap();
    let created = call(
        &s.app,
        "POST",
        "/$/backups/wiki?branch=work",
        Some("carol"),
        json!({"repository":"local", "name":"branch"}),
    )
    .await;
    assert_eq!(created.status, StatusCode::ACCEPTED);
    let location = created.headers.get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/$/backups/wiki/local/branch?branch=work");
    let t = wait_task(&s.st, created.body["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let located = call(&s.app, "GET", location, Some("carol"), J::Null).await;
    assert_eq!(located.status, StatusCode::OK);
    let main = call(&s.app, "GET", "/$/backups/wiki", Some("alice"), J::Null).await;
    assert!(names(&main).is_empty());
    ds.dataset.rename_branch("work", "renamed").unwrap();
    let listing = call(
        &s.app,
        "GET",
        "/$/backups/wiki?branch=renamed",
        Some("carol"),
        J::Null,
    )
    .await;
    assert_eq!(names(&listing), ["branch"]);
    let scoped = call(
        &s.app,
        "GET",
        "/$/backups/wiki/local/branch?branch=renamed",
        Some("carol"),
        J::Null,
    )
    .await;
    assert_eq!(scoped.status, StatusCode::OK);
    assert_eq!(scoped.body["dataset"]["branch"]["name"], "work");
    let hidden = call(
        &s.app,
        "GET",
        "/$/backups/wiki/local/branch",
        Some("carol"),
        J::Null,
    )
    .await;
    expect(&hidden, StatusCode::NOT_FOUND, "no-such-backup");
    let admin = call(
        &s.app,
        "GET",
        "/$/backups/wiki/local/branch",
        Some("alice"),
        J::Null,
    )
    .await;
    assert_eq!(admin.status, StatusCode::OK);
    let unrelated = call(
        &s.app,
        "GET",
        "/$/backups/unrelated/local/branch",
        Some("alice"),
        J::Null,
    )
    .await;
    expect(&unrelated, StatusCode::NOT_FOUND, "no-such-backup");
    let denied = call(
        &s.app,
        "POST",
        "/$/backups/wiki?branch=renamed",
        Some("dave"),
        json!({"repository":"local","name":"denied"}),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let restored = run_as(
        &s,
        "alice",
        "/$/backups/wiki/local/branch/restore",
        json!({"target":"copy"}),
    )
    .await;
    assert_eq!(restored.state, "done", "{:?}", restored.message);
    let copy = s.st.get("copy").unwrap();
    assert_eq!(copy.store.snapshot().len(), 2);
    assert_eq!(copy.store.branches().unwrap().len(), 1);
}
