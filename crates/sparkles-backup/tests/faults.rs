//! Backends that fail, race and misbehave: concurrent writers, cancellation, failing
//! and retried requests, conditional-write support, repository markers.

mod common;

use common::*;
use object_store::memory::InMemory;
use object_store::path::Path as Key;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use sparkles_backup::{
    Code, FileKind, ListFilter, OpenEnv, RepoConfig, Repository, TestStepKind, VerifyOptions,
    VerifyStatus,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn mem() -> Arc<dyn ObjectStore> {
    Arc::new(InMemory::new())
}

/// Two writers race for one name: exactly one wins, the loser's blobs are orphans.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_name_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    make_db(&a);
    make_db(&b);
    let inner = mem();
    let mut f = Faulty::new(inner.clone());
    // both manifests are sent only once both writers got that far
    f.manifest_barrier = Some(Arc::new(tokio::sync::Barrier::new(2)));
    let shared: Arc<dyn ObjectStore> = Arc::new(f);
    let r1 = open_on(shared.clone(), &memory_config("one")).await;
    let r2 = open_on(shared.clone(), &memory_config("two")).await;
    assert_eq!(r1.id(), r2.id());
    let (oa, ob) = (opts("n1", "a"), opts("n1", "b"));
    let (x, y) = tokio::join!(
        r1.create(closed_source(&a), &oa),
        r2.create(closed_source(&b), &ob),
    );
    let (winner, loser) = match (&x, &y) {
        (Ok(w), Err(e)) | (Err(e), Ok(w)) => (w.clone(), e.clone()),
        _ => panic!("exactly one must win: {x:?} {y:?}"),
    };
    assert_eq!(loser.code(), Code::BackupExists);
    let m = r1.manifest("n1").await.unwrap();
    assert_eq!(m.dataset.id, winner.dataset.id);
    let v = r1.verify(&[], &VerifyOptions::default()).await.unwrap();
    assert_eq!(v.backups.len(), 1);
    assert_eq!(v.backups[0].status, VerifyStatus::Ok);
    let orphans = v.orphans.unwrap();
    assert!(orphans.blobs > 0 && orphans.bytes > 0, "{orphans:?}");
    assert_eq!(v.status, VerifyStatus::Warning);
}

/// A cancelled backup leaves no manifest; the next attempt reuses what it uploaded.
#[tokio::test]
async fn cancel_then_reuse() {
    let inner = mem();
    let files = || {
        (0..8)
            .map(|i| {
                (
                    [
                        "gen-0001/spo.dat",
                        "gen-0001/sop.dat",
                        "gen-0001/pso.dat",
                        "gen-0001/pos.dat",
                        "gen-0001/osp.dat",
                        "gen-0001/ops.dat",
                        "gen-0001/gspo.dat",
                        "gen-0001/vocab.dat",
                    ][i],
                    FileKind::Immutable,
                    noise(64 << 10, i as u64),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut cfg = memory_config("slow");
    cfg.max_upload_bytes_per_sec = Some(128 << 10);
    let slow = open_on(inner.clone(), &cfg).await;
    let mut o = opts("b3", "ds");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    o.ctl.progress = Some(Arc::new(move |f, m: &str| {
        s2.lock().unwrap().push((f, m.to_string()))
    }));
    let cancel = o.ctl.cancel.clone();
    let src = synthetic(files());
    let id = src.dataset_id;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel.store(true, Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    let e = slow.create(src, &o).await.unwrap_err();
    assert!(e.is_cancelled(), "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(inner.head(&Key::from("backups/b3.json")).await.is_err());
    assert!(slow.list(&ListFilter::default()).await.unwrap().is_empty());
    let uploaded = count(inner.as_ref(), "blobs").await;
    assert!(
        (1..8).contains(&uploaded),
        "{uploaded} blobs before the cancel"
    );
    {
        let seen = seen.lock().unwrap();
        let (f, m) = seen.last().unwrap();
        assert!(*f > 0.05 && *f < 0.95, "{f}");
        assert!(
            // "1 new blob" or "N new blobs", depending on how far the upload got
            m.starts_with("uploading ")
                && (m.contains(" new blob · ") || m.contains(" new blobs · ")),
            "{m}"
        );
    }
    // again, unthrottled: the same dataset's files, the blobs of the first attempt reused
    let fast = open_on(inner.clone(), &memory_config("fast")).await;
    let mut again = synthetic(files());
    again.dataset_id = id;
    let s = fast.create(again, &opts("b3", "ds")).await.unwrap();
    let m = fast.manifest("b3").await.unwrap();
    assert_eq!(m.stats.reused_blobs, uploaded as u64);
    assert_eq!(m.stats.new_blobs, 8 - uploaded as u64);
    assert_eq!(s.name, "b3");
}

/// A backend that keeps failing blob uploads fails the backup without a manifest.
#[tokio::test]
async fn failing_uploads_fail_the_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let puts = Arc::new(AtomicUsize::new(0));
    let p2 = puts.clone();
    let mut f = Faulty::new(mem());
    f.hook = Some(Box::new(move |k: &Key, _: &object_store::PutOptions| {
        (k.as_ref().starts_with("blobs/") && p2.fetch_add(1, Ordering::SeqCst) >= 2)
            .then(|| generic("503 Service Unavailable"))
    }));
    let store: Arc<dyn ObjectStore> = Arc::new(f);
    let repo = open_on(store.clone(), &memory_config("bad")).await;
    let e = repo
        .create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::RepositoryUnavailable, "{e:?}");
    assert_eq!(e.http_status(), 502);
    assert!(e.message().contains("503"), "{}", e.message());
    assert!(store.head(&Key::from("backups/b1.json")).await.is_err());
    assert!(repo.list(&ListFilter::default()).await.unwrap().is_empty());
    let r = repo.requests();
    assert!(r.put.error >= 3, "{r:?}");
}

/// Transient failures are retried, and counted.
#[tokio::test]
async fn transient_failures_are_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let failures = Arc::new(AtomicUsize::new(0));
    let f2 = failures.clone();
    let mut f = Faulty::new(mem());
    f.hook = Some(Box::new(move |k: &Key, _: &object_store::PutOptions| {
        (k.as_ref().starts_with("blobs/") && f2.fetch_add(1, Ordering::SeqCst) < 2)
            .then(|| generic("503 Service Unavailable"))
    }));
    let repo = open_on(Arc::new(f), &memory_config("flaky")).await;
    let s = repo
        .create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    assert_eq!(s.commit.seq, 3);
    let r = repo.requests();
    assert_eq!(r.errors(), 2, "{r:?}");
    assert_eq!(r.put.error, 2);
    let v = repo
        .verify(
            &[],
            &VerifyOptions {
                level: sparkles_backup::VerifyLevel::Data,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
}

/// "Already exists" answered as a precondition failure (412) is success for blobs
/// and `backup-exists` for a manifest.
#[tokio::test]
async fn precondition_failures_mean_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let mut f = Faulty::new(mem());
    f.precondition = true;
    let repo = open_on(Arc::new(f), &memory_config("r2")).await;
    let first = repo
        .create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    // without a parent, every blob is sent again and found to exist
    assert!(repo.delete("b1").await.unwrap());
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    let m = repo.manifest("b1").await.unwrap();
    assert_eq!(m.stats.new_blobs, 0, "{:?}", m.stats);
    assert_eq!(m.stats.added_bytes, 0);
    assert_eq!(m.stats.logical_bytes, first.logical_bytes);
    assert_eq!(repo.requests().errors(), 0);
    assert_eq!(
        repo.create(closed_source(&db), &opts("b1", "ds"))
            .await
            .unwrap_err()
            .code(),
        Code::BackupExists
    );
}

/// Conditional-write detection by the connection test.
#[tokio::test]
async fn connection_test_detects_conditional_writes() {
    // memory: supported
    let (repo, inner) = memory_repo().await;
    assert_eq!(repo.conditional_writes(), None);
    let t = repo.test().await.unwrap();
    assert!(t.ok && t.conditional_writes, "{t:?}");
    let steps: Vec<TestStepKind> = t.steps.iter().map(|s| s.step).collect();
    assert_eq!(
        steps,
        [
            TestStepKind::Create,
            TestStepKind::CreateAgain,
            TestStepKind::Read,
            TestStepKind::List,
            TestStepKind::Delete
        ]
    );
    assert_eq!(repo.conditional_writes(), Some(true));
    assert!(!repo.single_writer());
    assert_eq!(count(inner.as_ref(), "probe").await, 0);
    let json = serde_json::to_value(&t).unwrap();
    assert_eq!(json["steps"][1]["step"], "create-again");
    assert_eq!(json["conditionalWrites"], true);

    // a service that ignores If-None-Match: detected, single writer
    let mut f = Faulty::new(mem());
    f.ignore_create = true;
    let repo = open_on(Arc::new(f), &memory_config("legacy")).await;
    let t = repo.test().await.unwrap();
    assert!(!t.conditional_writes && !t.ok, "{t:?}");
    assert!(!t.steps[1].ok);
    assert!(
        t.steps[1]
            .error
            .as_deref()
            .unwrap()
            .contains("If-None-Match")
    );
    assert!(
        t.steps
            .iter()
            .filter(|s| s.step != TestStepKind::CreateAgain)
            .all(|s| s.ok)
    );
    assert!(repo.single_writer());
    // backups still work there, with HEAD before each write
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    let r = repo.requests();
    assert!(r.head.ok + 3 >= r.put.ok, "{r:?}");
    assert_eq!(
        repo.create(closed_source(&db), &opts("b1", "ds"))
            .await
            .unwrap_err()
            .code(),
        Code::BackupExists
    );

    // conditional puts refused as not implemented (S3 with them disabled): fallback
    let mut f = Faulty::new(mem());
    f.no_create = true;
    let store: Arc<dyn ObjectStore> = Arc::new(f);
    let repo = open_on(store.clone(), &memory_config("nocond")).await;
    assert!(!repo.single_writer());
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    assert!(repo.single_writer(), "learned from the refused create");
    let t = open_on(store, &memory_config("nocond"))
        .await
        .test()
        .await
        .unwrap();
    assert!(t.ok && !t.conditional_writes, "{t:?}");

    // configured off: single writer, and the test does not ask
    let mut cfg = memory_config("off");
    cfg.conditional_writes = false;
    let repo = open_on(mem(), &cfg).await;
    assert!(repo.single_writer());
    let t = repo.test().await.unwrap();
    assert!(t.ok && !t.conditional_writes, "{t:?}");
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
}

/// A failed first step skips the others.
#[tokio::test]
async fn connection_test_reports_failures() {
    let mut f = Faulty::new(mem());
    f.hook = Some(Box::new(|k: &Key, _: &object_store::PutOptions| {
        k.as_ref()
            .starts_with("probe/")
            .then(|| generic("AccessDenied"))
    }));
    let repo = open_on(Arc::new(f), &memory_config("denied")).await;
    let t = repo.test().await.unwrap();
    assert!(!t.ok && !t.conditional_writes);
    assert!(
        t.steps[0]
            .error
            .as_deref()
            .unwrap()
            .contains("AccessDenied")
    );
    assert_eq!(t.steps.len(), 5);
    assert!(
        t.steps[1..]
            .iter()
            .all(|s| !s.ok && s.error.as_deref() == Some("skipped"))
    );
}

/// Markers: attach, initialize, refuse.
#[tokio::test]
async fn repository_markers() {
    let cfg = memory_config("m");
    let open = |store: Arc<dyn ObjectStore>, cfg: RepoConfig, init: bool| async move {
        Repository::open(
            &cfg,
            &OpenEnv {
                store: Some(store),
                init,
                ..Default::default()
            },
        )
        .await
    };
    // empty: initialized once, then attached
    let s = mem();
    let r = open(s.clone(), cfg.clone(), true).await.unwrap();
    let marker: serde_json::Value = serde_json::from_slice(
        &s.get(&Key::from("sparkles-repo.json"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["format"], 1);
    assert_eq!(marker["kind"], "sparkles-backup-repository");
    assert_eq!(marker["pieceBytes"], 32 << 20);
    assert!(marker["encryption"].is_null());
    assert_eq!(r.marker().piece_bytes, 32 << 20);
    assert_eq!(
        open(s.clone(), cfg.clone(), false).await.unwrap().id(),
        r.id()
    );
    // two initializers at once agree
    let s = mem();
    let (x, y) = tokio::join!(
        open(s.clone(), cfg.clone(), true),
        open(s.clone(), cfg.clone(), true)
    );
    assert_eq!(x.unwrap().id(), y.unwrap().id());
    // empty, but attach only or read-only
    let e = open(mem(), cfg.clone(), false).await.unwrap_err();
    assert_eq!(e.code(), Code::NotARepository);
    let mut ro = cfg.clone();
    ro.readonly = true;
    let s = mem();
    assert_eq!(
        open(s.clone(), ro, true).await.unwrap_err().code(),
        Code::NotARepository
    );
    assert_eq!(count(s.as_ref(), "").await, 0);
    // other objects, no marker
    let s = mem();
    s.put(&Key::from("photos/cat.jpg"), PutPayload::from_static(b"x"))
        .await
        .unwrap();
    let e = open(s.clone(), cfg.clone(), true).await.unwrap_err();
    assert_eq!(e.code(), Code::NotARepository);
    assert_eq!(e.http_status(), 409);
    assert_eq!(count(s.as_ref(), "").await, 1);
    // markers this build cannot use
    for (field, value) in [
        ("format", serde_json::json!(2)),
        ("encryption", serde_json::json!({"scheme": "age"})),
        ("kind", serde_json::json!("something-else")),
    ] {
        let mut m = marker.clone();
        m[field] = value;
        let s = mem();
        s.put(
            &Key::from("sparkles-repo.json"),
            PutPayload::from(serde_json::to_vec(&m).unwrap()),
        )
        .await
        .unwrap();
        let e = open(s, cfg.clone(), true).await.unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleRepository, "{field}");
        assert_eq!(e.http_status(), 422);
    }
    let s = mem();
    s.put(
        &Key::from("sparkles-repo.json"),
        PutPayload::from_static(b"{"),
    )
    .await
    .unwrap();
    assert_eq!(
        open(s, cfg.clone(), true).await.unwrap_err().code(),
        Code::IncompatibleRepository
    );
    // unknown fields are fine
    let mut m = marker.clone();
    m["futureField"] = serde_json::json!(true);
    let s = mem();
    s.put(
        &Key::from("sparkles-repo.json"),
        PutPayload::from(serde_json::to_vec(&m).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(open(s, cfg, true).await.unwrap().id().to_string(), m["id"]);
}

/// Large pieces: a file over 32 MiB is split, and a second dataset with the same file
/// finds its pieces with HEAD instead of sending them.
#[tokio::test]
async fn large_pieces_are_checked_before_sending() {
    let big = noise((32 << 20) + 1000, 7);
    let (repo, _) = memory_repo().await;
    repo.create(
        synthetic(vec![("gen-0001/spo.dat", FileKind::Immutable, big.clone())]),
        &opts("x1", "x"),
    )
    .await
    .unwrap();
    let m = repo.manifest("x1").await.unwrap();
    let sizes: Vec<u64> = m.files[0].blobs.iter().map(|b| b.size).collect();
    assert_eq!(sizes, [32 << 20, 1000]);
    assert_eq!(m.stats.new_blobs, 2);
    // incompressible: stored raw
    assert_eq!(m.stats.added_bytes, big.len() as u64 + 32);
    let before = repo.requests();
    repo.create(
        synthetic(vec![("gen-0001/spo.dat", FileKind::Immutable, big)]),
        &opts("y1", "y"),
    )
    .await
    .unwrap();
    let after = repo.requests();
    let m = repo.manifest("y1").await.unwrap();
    assert_eq!(m.stats.new_blobs, 0);
    assert_eq!(m.stats.reused_blobs, 2);
    assert_eq!(m.parent, None);
    // one HEAD for the big piece (plus the early name check); the small one is a create
    assert_eq!(after.head.ok - before.head.ok, 2);
}
