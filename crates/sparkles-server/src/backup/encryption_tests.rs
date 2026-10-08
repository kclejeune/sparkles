//! Trusted operator configuration: real encrypted server flows and reload boundaries.
use super::*;
use crate::{
    auth::Peer,
    http::router,
    state::{DbType, Task},
};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sparkles::backup::config::{ConfigFile, KeyInput, KeySource, RepoToml, RepositoryEncryption};
use sparkles::store::StoreOptions;
use std::os::unix::fs::PermissionsExt;
use tower::ServiceExt;

struct Fixture {
    root: tempfile::TempDir,
    config: PathBuf,
    key: PathBuf,
    repo: PathBuf,
    st: Arc<AppState>,
    app: Router,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config/backup.toml");
        let key = root.path().join("secret-key-should-not-appear");
        let repo = root.path().join("repo");
        private_key(&key, 41);
        write_config(&config, &repo, Some(file_settings(&key)));
        let mut state = AppState::new(
            &root.path().join("data"),
            StoreOptions::default(),
            Duration::from_secs(30),
        )
        .unwrap();
        state.backup = Some(Arc::new(
            BackupState::new(&state.data_dir, Some(config.clone()), 2).unwrap(),
        ));
        let ds = state.create("ds", DbType::Persistent).unwrap();
        ds.dataset
            .update("INSERT DATA { <urn:s> <urn:p> 1 }")
            .unwrap();
        state.set_phase(crate::obs::Phase::Ready);
        let st = Arc::new(state);
        let app = router(st.clone());
        Self {
            root,
            config,
            key,
            repo,
            st,
            app,
        }
    }
    fn b(&self) -> Arc<BackupState> {
        self.st.backup.clone().unwrap()
    }
    fn settings(&self) -> RepositoryEncryption {
        file_settings(&self.key)
    }
    fn reload(&self, settings: Option<RepositoryEncryption>) {
        write_config(&self.config, &self.repo, settings);
        self.b().reload().unwrap();
    }
    async fn run(&self, uri: &str, body: Value) -> Task {
        let (status, value) = call(&self.app, "POST", uri, body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
        self.wait(value["id"].as_str().unwrap()).await
    }
    async fn wait(&self, id: &str) -> Task {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let task = self
                    .st
                    .tasks
                    .lock()
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                    .unwrap();
                if !task.active() {
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("backup task timed out")
    }
}
fn private_key(path: &Path, byte: u8) {
    // Printable raw keys are refused as likely passwords, so write hex.
    std::fs::write(path, format!("{byte:02x}").repeat(32)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
fn file_settings(path: &Path) -> RepositoryEncryption {
    RepositoryEncryption {
        keys: vec![KeyInput {
            label: "private-provider-label".into(),
            key: KeySource::File {
                path: path.to_str().unwrap().into(),
            },
        }],
        single_key_ok: true,
    }
}
fn write_config(path: &Path, repo: &Path, encryption: Option<RepositoryEncryption>) {
    let mut file = ConfigFile {
        version: 1,
        ..Default::default()
    };
    file.repositories.insert(
        "enc".into(),
        RepoToml {
            kind: RepoType::Fs,
            path: Some(repo.to_str().unwrap().into()),
            encryption,
            conditional_writes: true,
            ..Default::default()
        },
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{}\n[policies.nightly]\nrepository=\"enc\"\ndatasets=[\"ds\"]\nschedule=\"0 0 1 1 *\"\nname_template=\"scheduled\"\n", toml::to_string(&file).unwrap())).unwrap();
}
async fn call(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())))
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_encrypted_server_create_policy_branch_restore_verify_gc_and_restart() {
    let s = Fixture::new();
    start(&s.st, &Handle::current());
    let created = s
        .run("/$/backups/ds", json!({"repository":"enc", "name":"main"}))
        .await;
    assert_eq!(created.state, "done", "{:?}", created.message);
    let ds = s.st.get("ds").unwrap();
    ds.dataset
        .create_branch("work", &Default::default())
        .unwrap();
    ds.dataset
        .branch("work")
        .unwrap()
        .update("INSERT DATA { <urn:branch> <urn:p> 2 }")
        .unwrap();
    let branch = s
        .run(
            "/$/backups/ds?branch=work",
            json!({"repository":"enc", "name":"branch"}),
        )
        .await;
    assert_eq!(branch.state, "done", "{:?}", branch.message);
    let scheduled = policies::start_run(
        &s.st,
        "nightly".into(),
        sparkles_backup::RunTrigger::Schedule,
        Some(chrono::Utc::now()),
    )
    .unwrap_or_else(|_| panic!("scheduled policy was not admitted"));
    let policy = s.wait(&scheduled.id).await;
    assert_eq!(policy.state, "done", "{:?}", policy.message);
    let (status, listing) = call(&s.app, "GET", "/$/repositories/enc/backups", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(listing["backups"].as_array().unwrap().len(), 3);
    let verify = s
        .run("/$/backups/ds/enc/main/verify", json!({"level":"data"}))
        .await;
    assert_eq!(verify.state, "done", "{:?}", verify.message);
    assert_eq!(verify.detail.unwrap()["status"], "ok");
    let restore = s
        .run("/$/backups/ds/enc/branch/restore", json!({"target":"copy"}))
        .await;
    assert_eq!(restore.state, "done", "{:?}", restore.message);
    assert_eq!(s.st.get("copy").unwrap().store.snapshot().len(), 2);
    let gc = s
        .run("/$/repositories/enc/gc", json!({"dryRun":true}))
        .await;
    assert_eq!(gc.state, "done", "{:?}", gc.message);
    let marker: Value =
        serde_json::from_slice(&std::fs::read(s.repo.join("sparkles-repo.json")).unwrap()).unwrap();
    assert_eq!(marker["format"], 1);
    assert_eq!(marker["hash"], "hmac-sha256");
    assert!(marker["encryption"].is_object());
    let repo_id = s.b().open_repo("enc").await.unwrap().id();
    // A fresh server registry/runtime opens the same persisted encrypted repository.
    let mut restarted = AppState::new(
        &s.root.path().join("restart-data"),
        StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    restarted.backup = Some(Arc::new(
        BackupState::new(&restarted.data_dir, Some(s.config.clone()), 2).unwrap(),
    ));
    restarted.set_phase(crate::obs::Phase::Ready);
    let restarted = Arc::new(restarted);
    let app = router(restarted.clone());
    let (status, listing) = call(&app, "GET", "/$/repositories/enc/backups", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(listing["backups"].as_array().unwrap().len(), 3);
    assert_eq!(
        restarted
            .backup
            .as_ref()
            .unwrap()
            .open_repo("enc")
            .await
            .unwrap()
            .id(),
        repo_id
    );
}

#[tokio::test]
async fn unchanged_reload_resolves_changed_keys_preserves_uuid_and_redacts_diagnostics() {
    let s = Fixture::new();
    let old = s.b().open_repo("enc").await.unwrap();
    let id = old.id();
    let generation = s.b().registry.repos.read()["enc"].generation();
    private_key(&s.key, 42);
    s.reload(Some(s.settings()));
    assert_ne!(s.b().registry.repos.read()["enc"].generation(), generation);
    let err = s.b().open_repo("enc").await.unwrap_err();
    assert_eq!(err.code(), Code::WrongRepositoryKey);
    assert_eq!(s.b().registry.repos.read()["enc"].id, Some(id));
    let (status, view) = call(&s.app, "GET", "/$/repositories/enc", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let text = view.to_string();
    assert!(
        !text.contains("private-provider-label")
            && !text.contains("secret-key-should-not-appear")
            && !text.contains("encryption"),
        "{text}"
    );
    // Retained admitted handles remain usable; new calls require current inputs.
    assert!(old.stats().await.is_ok());
    std::fs::remove_file(&s.key).unwrap();
    s.reload(Some(s.settings()));
    assert_eq!(
        s.b().open_repo("enc").await.unwrap_err().code(),
        Code::RepositoryKeyRequired
    );
    private_key(&s.key, 41);
    s.reload(Some(s.settings()));
    assert_eq!(s.b().open_repo("enc").await.unwrap().id(), id);
    s.reload(None);
    assert_eq!(
        s.b().open_repo("enc").await.unwrap_err().code(),
        Code::RepositoryKeyRequired
    );
    assert_eq!(s.b().registry.repos.read()["enc"].id, Some(id));
    s.reload(Some(s.settings()));
    assert_eq!(s.b().open_repo("enc").await.unwrap().id(), id);
}

#[tokio::test]
async fn invalid_reload_keeps_previous_opened_registry() {
    let s = Fixture::new();
    let old = s.b().open_repo("enc").await.unwrap();
    let token = s.b().registry.repos.read()["enc"].generation();
    std::fs::write(
        &s.config,
        "version=1\n[repositories.enc.encryption]\nkeys=[]\n",
    )
    .unwrap();
    assert!(s.b().reload().is_err());
    assert_eq!(s.b().registry.repos.read()["enc"].generation(), token);
    assert!(Arc::ptr_eq(&s.b().open_repo("enc").await.unwrap(), &old));
}

async fn wait_invocation(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(path).is_ok_and(|text| !text.is_empty()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn late_provider_open_cannot_initialize_cache_or_return_old_generation() {
    let s = Fixture::new();
    let ready = s.root.path().join("ready");
    let release = s.root.path().join("release");
    let script = s.root.path().join("provider.sh");
    std::fs::write(
        &script,
        "touch \"$1\"\nwhile [ ! -f \"$2\" ]; do sleep 0.01; done\ncat \"$3\"\n",
    )
    .unwrap();
    let settings = RepositoryEncryption {
        keys: vec![KeyInput {
            label: "old-provider".into(),
            key: KeySource::Command {
                argv: vec![
                    "/bin/sh".into(),
                    script.display().to_string(),
                    ready.display().to_string(),
                    release.display().to_string(),
                    s.key.display().to_string(),
                ],
                timeout_secs: 5,
            },
        }],
        single_key_ok: true,
    };
    s.reload(Some(settings));
    let b = s.b();
    let pending = tokio::spawn(async move { b.open_repo("enc").await });
    wait_file(&ready).await;
    assert!(!s.repo.exists());
    let b = s.b();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let old_waiter = tokio::spawn(async move {
        let _ = started.send(());
        b.open_repo("enc").await
    });
    waiting.await.unwrap();
    s.reload(Some(s.settings()));
    std::fs::write(&release, b"go").unwrap();
    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(err.code(), Code::RepositoryUnavailable);
    assert!(err.message().contains("configuration changed"));
    assert_eq!(
        old_waiter.await.unwrap().unwrap_err().code(),
        Code::RepositoryUnavailable
    );
    assert!(
        !s.repo.exists(),
        "old generation initialized before checking reload"
    );
    assert!(s.b().registry.repos.read()["enc"].opened.is_none());
    assert!(s.b().open_repo("enc").await.is_ok());
}

#[tokio::test]
async fn provider_cancellation_and_forbidden_server_key_locations_do_not_open_repository() {
    let s = Fixture::new();
    let key = s.st.data_dir.join("private-key");
    private_key(&key, 41);
    s.reload(Some(file_settings(&key)));
    assert_eq!(
        s.b().open_repo("enc").await.unwrap_err().code(),
        Code::RepositoryKeyRequired
    );
    assert!(!s.repo.exists());
    let ready = s.root.path().join("cancel-ready");
    let script = s.root.path().join("cancel.sh");
    std::fs::write(&script, "echo $$ > \"$1\"\nsleep 30\n").unwrap();
    let settings = RepositoryEncryption {
        keys: vec![KeyInput {
            label: "cancel-provider".into(),
            key: KeySource::Command {
                argv: vec![
                    "/bin/sh".into(),
                    script.display().to_string(),
                    ready.display().to_string(),
                ],
                timeout_secs: 30,
            },
        }],
        single_key_ok: true,
    };
    s.reload(Some(settings));
    let b = s.b();
    let pending = tokio::spawn(async move { b.open_repo("enc").await });
    wait_file(&ready).await;
    let pid: libc::pid_t = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&ready)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        // Inspect only the child PID this fixture owns; cancellation must reap it.
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancelled provider was not reaped");
    assert!(!s.repo.exists());
    assert!(s.b().registry.repos.read()["enc"].opened.is_none());
    s.reload(Some(s.settings()));
    assert!(s.b().open_repo("enc").await.is_ok());
}

/// A native repository's actual store, with one marker read paused after provider resolution.
#[derive(Debug)]
struct PausedStore {
    inner: Arc<dyn ObjectStore>,
    arrived: tokio::sync::Notify,
    release: tokio::sync::Notify,
    once: AtomicBool,
    fail_list: AtomicBool,
    fail_probe: AtomicBool,
    detail: String,
}
impl std::fmt::Display for PausedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("paused repository store")
    }
}
// Explicit boxed futures implement the object-safe backend trait without another test dependency.
impl ObjectStore for PausedStore {
    fn put_opts<'s, 'k, 'f>(
        &'s self,
        key: &'k sparkles_backup::object_store::path::Path,
        payload: sparkles_backup::object_store::PutPayload,
        options: sparkles_backup::object_store::PutOptions,
    ) -> futures_util::future::BoxFuture<
        'f,
        sparkles_backup::object_store::Result<sparkles_backup::object_store::PutResult>,
    >
    where
        's: 'f,
        'k: 'f,
        Self: 'f,
    {
        if key.as_ref().starts_with("probe/") && self.fail_probe.load(Ordering::Relaxed) {
            return Box::pin(async {
                Err(sparkles_backup::object_store::Error::Generic {
                    store: "diagnostic fixture",
                    source: self.detail.clone().into(),
                })
            });
        }
        self.inner.put_opts(key, payload, options)
    }
    fn put_multipart_opts<'s, 'k, 'f>(
        &'s self,
        key: &'k sparkles_backup::object_store::path::Path,
        options: sparkles_backup::object_store::PutMultipartOptions,
    ) -> futures_util::future::BoxFuture<
        'f,
        sparkles_backup::object_store::Result<
            Box<dyn sparkles_backup::object_store::MultipartUpload>,
        >,
    >
    where
        's: 'f,
        'k: 'f,
        Self: 'f,
    {
        self.inner.put_multipart_opts(key, options)
    }
    fn get_opts<'s, 'k, 'f>(
        &'s self,
        key: &'k sparkles_backup::object_store::path::Path,
        options: sparkles_backup::object_store::GetOptions,
    ) -> futures_util::future::BoxFuture<
        'f,
        sparkles_backup::object_store::Result<sparkles_backup::object_store::GetResult>,
    >
    where
        's: 'f,
        'k: 'f,
        Self: 'f,
    {
        Box::pin(async move {
            if key.as_ref() == "sparkles-repo.json" && self.once.swap(false, Ordering::SeqCst) {
                self.arrived.notify_one();
                self.release.notified().await;
            }
            self.inner.get_opts(key, options).await
        })
    }
    fn delete_stream(
        &self,
        keys: futures_util::stream::BoxStream<
            'static,
            sparkles_backup::object_store::Result<sparkles_backup::object_store::path::Path>,
        >,
    ) -> futures_util::stream::BoxStream<
        'static,
        sparkles_backup::object_store::Result<sparkles_backup::object_store::path::Path>,
    > {
        self.inner.delete_stream(keys)
    }
    fn list(
        &self,
        prefix: Option<&sparkles_backup::object_store::path::Path>,
    ) -> futures_util::stream::BoxStream<
        'static,
        sparkles_backup::object_store::Result<sparkles_backup::object_store::ObjectMeta>,
    > {
        if self.fail_list.load(Ordering::Relaxed) {
            let detail = self.detail.clone();
            return Box::pin(futures_util::stream::once(async move {
                Err(sparkles_backup::object_store::Error::Generic {
                    store: "diagnostic fixture",
                    source: detail.into(),
                })
            }));
        }
        self.inner.list(prefix)
    }
    fn list_with_delimiter<'s, 'k, 'f>(
        &'s self,
        prefix: Option<&'k sparkles_backup::object_store::path::Path>,
    ) -> futures_util::future::BoxFuture<
        'f,
        sparkles_backup::object_store::Result<sparkles_backup::object_store::ListResult>,
    >
    where
        's: 'f,
        'k: 'f,
        Self: 'f,
    {
        self.inner.list_with_delimiter(prefix)
    }
    fn copy_opts<'s, 'k, 't, 'f>(
        &'s self,
        from: &'k sparkles_backup::object_store::path::Path,
        to: &'t sparkles_backup::object_store::path::Path,
        options: sparkles_backup::object_store::CopyOptions,
    ) -> futures_util::future::BoxFuture<'f, sparkles_backup::object_store::Result<()>>
    where
        's: 'f,
        'k: 'f,
        't: 'f,
        Self: 'f,
    {
        self.inner.copy_opts(from, to, options)
    }
}

#[tokio::test]
async fn late_engine_open_cannot_publish_or_return_reloaded_generation() {
    let s = Fixture::new();
    let original = s.b().open_repo("enc").await.unwrap();
    let id = original.id();
    let gate = Arc::new(PausedStore {
        inner: original.store().clone(),
        arrived: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        once: AtomicBool::new(true),
        fail_list: AtomicBool::new(false),
        fail_probe: AtomicBool::new(false),
        detail: String::new(),
    });
    s.b().stores.lock().insert("enc".into(), gate.clone());
    s.reload(Some(s.settings()));
    let b = s.b();
    let pending = tokio::spawn(async move { b.open_repo("enc").await });
    tokio::time::timeout(Duration::from_secs(5), gate.arrived.notified())
        .await
        .unwrap();
    s.reload(Some(s.settings()));
    gate.release.notify_one();
    let err = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), Code::RepositoryUnavailable);
    assert!(err.message().contains("configuration changed"));
    let b = s.b();
    {
        let entries = b.registry.repos.read();
        assert!(entries["enc"].opened.is_none());
        assert_eq!(entries["enc"].id, Some(id));
        assert!(entries["enc"].status.checked.is_empty());
    }
    b.stores.lock().remove("enc");
    assert_eq!(b.open_repo("enc").await.unwrap().id(), id);
}

#[tokio::test]
async fn replaced_encrypted_repository_uuid_is_refused_without_mutation_or_reference_disclosure() {
    let s = Fixture::new();
    let old = s.b().open_repo("enc").await.unwrap();
    let original_id = old.id();
    let other_path = s.root.path().join("other-repository");
    let mut config = s.b().registry.config("enc").unwrap();
    config.path = Some(other_path.display().to_string());
    let inputs = sparkles::backup::keys::resolve(
        &s.settings(),
        &sparkles::backup::keys::KeyContext::default(),
        &sparkles_backup::Ctl::default(),
    )
    .await
    .unwrap();
    let other = Repository::open_encrypted(&config, &OpenEnv::default(), &inputs)
        .await
        .unwrap();
    assert_ne!(other.id(), original_id);
    std::fs::rename(&s.repo, s.root.path().join("original-repository")).unwrap();
    std::fs::rename(&other_path, &s.repo).unwrap();
    let marker_before = std::fs::read(s.repo.join("sparkles-repo.json")).unwrap();
    s.reload(Some(s.settings()));
    let err = s.b().open_repo("enc").await.unwrap_err();
    assert_eq!(err.code(), Code::RepositoryUnavailable);
    assert_eq!(err.message(), "encrypted repository could not be opened");
    assert_eq!(s.b().registry.repos.read()["enc"].id, Some(original_id));
    assert!(s.b().registry.repos.read()["enc"].opened.is_none());
    assert_eq!(
        std::fs::read(s.repo.join("sparkles-repo.json")).unwrap(),
        marker_before
    );
}

#[tokio::test]
async fn provider_timeout_is_bounded_and_redacted_without_repository_initialization() {
    let s = Fixture::new();
    let settings = RepositoryEncryption {
        keys: vec![KeyInput {
            label: "private-command-label".into(),
            key: KeySource::Command {
                argv: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
                timeout_secs: 1,
            },
        }],
        single_key_ok: true,
    };
    s.reload(Some(settings));
    let err = tokio::time::timeout(Duration::from_secs(5), s.b().open_repo("enc"))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), Code::RepositoryKeyRequired);
    assert!(!err.message().contains("private-command-label") && !err.message().contains("sleep"));
    assert!(!s.repo.exists());
    assert!(s.b().registry.repos.read()["enc"].opened.is_none());
}

fn counted_provider(s: &Fixture, count: &Path, release: &Path) -> RepositoryEncryption {
    let script = s.root.path().join("counted-provider.sh");
    std::fs::write(
        &script,
        "echo run >> \"$1\"\nwhile [ ! -f \"$2\" ]; do sleep 0.01; done\ncat \"$3\"\n",
    )
    .unwrap();
    RepositoryEncryption {
        keys: vec![KeyInput {
            label: "counted-provider".into(),
            key: KeySource::Command {
                argv: vec![
                    "/bin/sh".into(),
                    script.display().to_string(),
                    count.display().to_string(),
                    release.display().to_string(),
                    s.key.display().to_string(),
                ],
                timeout_secs: 5,
            },
        }],
        single_key_ok: true,
    }
}
#[tokio::test]
async fn concurrent_cache_misses_resolve_provider_once_and_share_protected_handle() {
    let s = Fixture::new();
    let count = s.root.path().join("invocations");
    let release = s.root.path().join("provider-release");
    s.reload(Some(counted_provider(&s, &count, &release)));
    let mut opens = Vec::new();
    for _ in 0..12 {
        let b = s.b();
        opens.push(tokio::spawn(async move { b.open_repo("enc").await }));
    }
    wait_invocation(&count).await;
    std::fs::write(&release, b"go").unwrap();
    let mut handles = Vec::new();
    for pending in opens {
        handles.push(
            tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
    }
    assert!(handles.iter().all(|h| Arc::ptr_eq(h, &handles[0])));
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
}
#[tokio::test]
async fn cancelled_first_opener_releases_same_generation_gate_for_waiter() {
    let s = Fixture::new();
    let count = s.root.path().join("cancelled-invocations");
    let release = s.root.path().join("cancelled-provider-release");
    s.reload(Some(counted_provider(&s, &count, &release)));
    let token = s.b().registry.repos.read()["enc"].generation();
    let b = s.b();
    let first = tokio::spawn(async move { b.open_repo("enc").await });
    wait_invocation(&count).await;
    let b = s.b();
    let waiter = tokio::spawn(async move { b.open_repo("enc").await });
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    std::fs::write(&release, b"go").unwrap();
    let opened = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(s.b().registry.repos.read()["enc"].generation(), token);
    assert!(Arc::ptr_eq(&opened, &s.b().open_repo("enc").await.unwrap()));
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
}

#[tokio::test]
async fn supplied_ctl_cancels_provider_and_same_generation_recovers() {
    let s = Fixture::new();
    let count = s.root.path().join("ctl-invocations");
    let release = s.root.path().join("ctl-provider-release");
    s.reload(Some(counted_provider(&s, &count, &release)));
    let cancel = Arc::new(AtomicBool::new(false));
    let ctl = sparkles_backup::Ctl::with_cancel(cancel.clone());
    let b = s.b();
    let first = tokio::spawn(async move { b.open_repo_with_ctl("enc", &ctl).await });
    wait_invocation(&count).await;
    cancel.store(true, Ordering::Relaxed);
    let err = tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), Code::Cancelled);
    assert!(!s.repo.exists());
    assert!(s.b().registry.repos.read()["enc"].status.checked.is_empty());
    std::fs::write(&release, b"go").unwrap();
    assert!(s.b().open_repo("enc").await.is_ok());
}

#[tokio::test]
async fn supplied_ctl_cancels_gate_waiter_without_cancelling_active_provider() {
    let s = Fixture::new();
    let count = s.root.path().join("waiter-invocations");
    let release = s.root.path().join("waiter-provider-release");
    s.reload(Some(counted_provider(&s, &count, &release)));
    let b = s.b();
    let first = tokio::spawn(async move { b.open_repo("enc").await });
    wait_invocation(&count).await;
    let cancel = Arc::new(AtomicBool::new(false));
    let ctl = sparkles_backup::Ctl::with_cancel(cancel.clone());
    let b = s.b();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        let _ = started.send(());
        b.open_repo_with_ctl("enc", &ctl).await
    });
    waiting.await.unwrap();
    cancel.store(true, Ordering::Relaxed);
    let err = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), Code::Cancelled);
    assert!(!first.is_finished());
    std::fs::write(&release, b"go").unwrap();
    assert!(first.await.unwrap().is_ok());
    assert!(s.b().open_repo("enc").await.is_ok());
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_scheduled_policy_cancellation_interrupts_provider_before_engine_open() {
    let s = Fixture::new();
    let count = s.root.path().join("policy-cancel-invocations");
    let release = s.root.path().join("policy-cancel-release");
    s.reload(Some(counted_provider(&s, &count, &release)));
    // No background refresh steals the provider: this task's policy-list stage opens it.
    let task = policies::start_run(
        &s.st,
        "nightly".into(),
        sparkles_backup::RunTrigger::Schedule,
        Some(chrono::Utc::now()),
    )
    .unwrap_or_else(|_| panic!("scheduled policy was not admitted"));
    wait_invocation(&count).await;
    assert!(s.st.cancel_task(&task.id).unwrap().is_ok());
    let ended = tokio::time::timeout(Duration::from_secs(2), s.wait(&task.id))
        .await
        .unwrap();
    assert_eq!(ended.state, "cancelled", "{:?}", ended.message);
    assert!(!s.repo.exists());
    assert!(s.b().registry.repos.read()["enc"].opened.is_none());
    std::fs::write(&release, b"go").unwrap();
    assert!(s.b().open_repo("enc").await.is_ok());
}

#[tokio::test]
async fn externally_attached_dataset_key_is_refused_by_opened_fd_ancestor_validation() {
    let s = Fixture::new();
    let external = s.root.path().join("external-dataset");
    s.st.attach("external", DbType::Persistent, Some(&external))
        .unwrap();
    let key = external.join("private-key");
    private_key(&key, 41);
    s.reload(Some(file_settings(&key)));
    assert_eq!(
        s.b().open_repo("enc").await.unwrap_err().code(),
        Code::RepositoryKeyRequired
    );
    assert!(!s.repo.exists());
}

#[test]
fn encrypted_memory_configuration_is_refused_before_provider_or_repository_mutation() {
    let s = Fixture::new();
    let mut config = config::load(&s.config).unwrap();
    let repo = config.repositories.get_mut("enc").unwrap();
    repo.kind = RepoType::Memory;
    repo.path = None;
    assert!(
        validate_file(&config, &s.st.data_dir)
            .unwrap_err()
            .to_string()
            .contains("persistent backend")
    );
    assert!(!s.repo.exists());
    assert!(s.b().registry.repos.read()["enc"].opened.is_none());
    std::fs::write(&s.config, toml::to_string(&config).unwrap()).unwrap();
    let token = s.b().registry.repos.read()["enc"].generation();
    assert!(s.b().reload().is_err());
    assert_eq!(s.b().registry.repos.read()["enc"].generation(), token);
}

async fn diagnostic_store(s: &Fixture) -> Arc<PausedStore> {
    let opened = s.b().open_repo("enc").await.unwrap();
    let store = Arc::new(PausedStore {
        inner: opened.store().clone(),
        arrived: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        once: AtomicBool::new(false),
        fail_list: AtomicBool::new(false),
        fail_probe: AtomicBool::new(false),
        detail: format!(
            "PRIVATE_BACKEND_DETAIL: private-provider-label {}",
            s.key.display()
        ),
    });
    s.b().stores.lock().insert("enc".into(), store.clone());
    s.b().registry.update("enc", |e| e.opened = None);
    s.b().open_repo("enc").await.unwrap();
    store
}
#[tokio::test]
async fn encrypted_post_open_backend_errors_are_static_in_admin_status_and_test_reports() {
    let s = Fixture::new();
    let fault = diagnostic_store(&s).await;
    fault.fail_list.store(true, Ordering::Relaxed);
    s.b().refresh("enc").await;
    let (status, view) = call(&s.app, "GET", "/$/repositories/enc", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        view["status"]["error"],
        "encrypted repository statistics are unavailable"
    );
    assert!(
        !view.to_string().contains("PRIVATE_BACKEND_DETAIL")
            && !view.to_string().contains("private-provider-label")
            && !view.to_string().contains("secret-key-should-not-appear")
    );
    fault.fail_list.store(false, Ordering::Relaxed);
    fault.fail_probe.store(true, Ordering::Relaxed);
    let (status, report) = call(&s.app, "POST", "/$/repositories/enc/test", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["ok"], false);
    assert_eq!(
        report["steps"][0]["error"],
        "encrypted repository connection check failed"
    );
    assert!(
        !report.to_string().contains("PRIVATE_BACKEND_DETAIL")
            && !report.to_string().contains("private-provider-label")
            && !report.to_string().contains("secret-key-should-not-appear")
    );
    assert_eq!(
        s.b().registry.repos.read()["enc"].status.error.as_deref(),
        Some("encrypted repository connection check failed")
    );
}
#[tokio::test]
async fn plaintext_post_open_backend_errors_keep_existing_operator_diagnostics() {
    let s = Fixture::new();
    s.reload(None);
    let fault = diagnostic_store(&s).await;
    fault.fail_list.store(true, Ordering::Relaxed);
    s.b().refresh("enc").await;
    assert!(
        s.b().registry.repos.read()["enc"]
            .status
            .error
            .as_deref()
            .unwrap()
            .contains("PRIVATE_BACKEND_DETAIL")
    );
    fault.fail_list.store(false, Ordering::Relaxed);
    fault.fail_probe.store(true, Ordering::Relaxed);
    let (status, report) = call(&s.app, "POST", "/$/repositories/enc/test", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["ok"], false);
    assert!(
        report["steps"][0]["error"]
            .as_str()
            .unwrap()
            .contains("PRIVATE_BACKEND_DETAIL")
    );
}
