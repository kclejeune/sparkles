use super::*;
use crate::fixture::{self, Flaky};
use crate::layout;
use crate::{BlobRef, CheckLevel, VerifyLevel};
use object_store::{ObjectStoreExt, PutPayload};
use sparkles_core::store::StoreOptions;
use std::cell::Cell;
use std::sync::atomic::Ordering::SeqCst;

thread_local! {
    /// `swap_dir`'s second rename fails on this thread
    pub(super) static FAIL_SECOND_RENAME: Cell<bool> = const { Cell::new(false) };
}

/// A repository holding backup `b1` of the acceptance database (head 3, one quad).
async fn setup() -> (tempfile::TempDir, Repository, Manifest) {
    let dir = tempfile::tempdir().unwrap();
    fixture::make_db(&dir.path().join("src"));
    let repo = fixture::memory_repo().await;
    let m = fixture::put_backup(&repo, &dir.path().join("src"), "b1").await;
    (dir, repo, m)
}

/// A repository holding backup `b1` of the larger database (head 5, 3002 quads,
/// `gen-0002` with multi-piece index files).
async fn setup_big() -> (tempfile::TempDir, Repository, Manifest) {
    let dir = tempfile::tempdir().unwrap();
    fixture::make_big_db(&dir.path().join("src"));
    let repo = fixture::memory_repo().await;
    let m = fixture::put_backup(&repo, &dir.path().join("src"), "b1").await;
    (dir, repo, m)
}

fn nquads(root: &Path) -> String {
    let s = Store::open(root, StoreOptions::default()).unwrap();
    let mut out = Vec::new();
    s.dump_nquads(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// The entries of a directory, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

/// The id of the last blob of the file `path` of `m`.
fn blob_of(m: &Manifest, path: &str) -> String {
    m.files
        .iter()
        .find(|f| f.path == path)
        .unwrap()
        .blobs
        .last()
        .unwrap()
        .id
        .clone()
}

#[tokio::test]
async fn a_restore_reproduces_the_backed_up_database() {
    let (dir, repo, m) = setup_big().await;
    assert_eq!((m.commit.seq, m.commit.quads), (5, 3002));
    // the vocabulary spans several pieces
    let vocab = m
        .files
        .iter()
        .find(|f| f.path == "gen-0002/vocab.dat")
        .unwrap();
    assert!(vocab.blobs.len() > 1, "{vocab:?}");
    let tmp = dir.path().join("restored");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let o = RestoreOptions {
        ctl: Ctl {
            progress: Some(Arc::new(move |f, msg: &str| {
                s2.lock().unwrap().push((f, msg.to_string()))
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    let r = repo.restore("b1", &tmp, &o).await.unwrap();
    assert_eq!(r.identity, "kept");
    assert_eq!(r.dataset_id, m.dataset.id);
    assert_eq!(r.forked_from, None);
    assert_eq!(r.backup.name, "b1");
    assert_eq!(r.backup.commit.seq, 5);
    assert_eq!(r.check.as_ref().unwrap()["mode"], "quick");
    assert_ne!(r.check.as_ref().unwrap()["status"], "error");
    {
        let progress = seen.lock().unwrap();
        assert!(progress.iter().any(|(_, m)| m.starts_with("downloading")));
        assert_eq!(progress.last().unwrap().0, 1.0);
        assert!(progress.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    // byte-identical files, and the recorded provenance
    let src = dir.path().join("src");
    for p in [
        "commits.bin",
        "CURRENT",
        "gen-0002/wal.log",
        "gen-0002/spo.dat",
    ] {
        assert_eq!(
            std::fs::read(tmp.join(p)).unwrap(),
            std::fs::read(src.join(p)).unwrap(),
            "{p}"
        );
    }
    let rec: RestoreRecord =
        serde_json::from_slice(&std::fs::read(tmp.join("restore.json")).unwrap()).unwrap();
    assert_eq!(rec.backup, "b1");
    assert_eq!(rec.repository.id, repo.id());
    assert_eq!(rec.repository.name, "local");
    assert_eq!(
        (
            rec.source.dataset_id,
            rec.source.seq,
            rec.source.name.as_str()
        ),
        (m.dataset.id, 5, "ds")
    );
    assert_eq!(rec.identity, "kept");

    // the restored database opens at the backed-up commit
    let s = Store::open(&tmp, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 5);
    assert_eq!(s.dataset_id(), m.dataset.id);
    drop(s);
    let dump = nquads(&tmp);
    assert_eq!(dump, nquads(&src));
    assert!(dump.contains("<urn:c>") && !dump.contains("<urn:a>"));

    // the shared lock is released
    assert!(repo.locks().await.unwrap().is_empty());
}

#[tokio::test]
async fn check_levels() {
    let (dir, repo, _) = setup().await;
    for (level, mode) in [(CheckLevel::Full, Some("full")), (CheckLevel::None, None)] {
        let tmp = dir.path().join(format!("r-{mode:?}"));
        let o = RestoreOptions {
            check: level,
            ..Default::default()
        };
        let r = repo.restore("b1", &tmp, &o).await.unwrap();
        assert_eq!(r.check.as_ref().map(|c| c["mode"].as_str().unwrap()), mode);
    }
}

#[tokio::test]
async fn unsupported_dataset_metadata_is_refused_even_without_integrity_checks() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src");
    fixture::make_db(&source);
    let repo = fixture::memory_repo().await;
    let original = std::fs::read(source.join("dataset.json")).unwrap();
    for (field, value) in [("format", 2), ("minimumReader", 3)] {
        let mut capture = crate::Source::from_closed_dir(&source).unwrap();
        let mut metadata: serde_json::Value = serde_json::from_slice(&original).unwrap();
        metadata[field] = value.into();
        let bytes = serde_json::to_vec(&metadata).unwrap();
        let file = capture
            .files
            .iter_mut()
            .find(|f| f.path == "dataset.json")
            .unwrap();
        file.len = bytes.len() as u64;
        file.src = sparkles_core::store::FileSource::Bytes(bytes.into());
        repo.create(
            capture,
            &crate::CreateOptions {
                name: field.into(),
                dataset_name: "ds".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        for identity in [Identity::New, Identity::Keep] {
            let destination = dir.path().join(format!("{field}-{identity:?}"));
            let error = repo
                .restore(
                    field,
                    &destination,
                    &RestoreOptions {
                        check: CheckLevel::None,
                        identity,
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err();
            assert!(error.message().contains("this build supports"), "{error}");
            assert!(!destination.exists());
            assert_eq!(
                std::fs::read(source.join("dataset.json")).unwrap(),
                original
            );
        }
    }
    assert!(repo.locks().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_corrupt_blob_fails_the_restore_and_leaves_nothing() {
    let (dir, repo, m) = setup_big().await;
    let id = blob_of(&m, "gen-0002/spo.dat");
    fixture::tamper_blob(&repo, &id, |b| {
        let last = b.len() - 1;
        b[last] ^= 1;
    })
    .await;
    let parent = dir.path().join("databases");
    std::fs::create_dir(&parent).unwrap();
    let tmp = parent.join(".restore-x-1");
    let e = repo
        .restore("b1", &tmp, &RestoreOptions::default())
        .await
        .unwrap_err();
    assert!(
        e.message().contains(&format!(
            "checksum mismatch in blob {id} (gen-0002/spo.dat)"
        )),
        "{e}"
    );
    assert_eq!(e.body()["blob"], id.as_str());
    assert!(entries(&parent).is_empty());
    assert!(repo.locks().await.unwrap().is_empty());

    // verification at the restore level reports it
    let v = repo
        .verify_restore(
            "b1",
            &VerifyOptions {
                tmp_dir: Some(parent.clone()),
                level: VerifyLevel::Restore,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(v.status, VerifyStatus::Error);
    assert_eq!(v.corrupt, std::slice::from_ref(&id));
    assert!(entries(&parent).is_empty());

    // a missing blob
    repo.store.delete(&layout::blob_key(&id)).await.unwrap();
    let e = repo
        .restore("b1", &tmp, &RestoreOptions::default())
        .await
        .unwrap_err();
    assert!(e.message().contains(&format!("missing blob {id}")), "{e}");
    let v = repo
        .verify_restore(
            "b1",
            &VerifyOptions {
                tmp_dir: Some(parent.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((v.missing, v.corrupt), (vec![id], vec![]));
    assert!(entries(&parent).is_empty());
}

#[tokio::test]
async fn a_blob_that_fails_its_hash_is_fetched_again_twice() {
    let (dir, repo, m) = setup_big().await;
    let flaky = Arc::new(Flaky::new(repo.store.clone()));
    let id = blob_of(&m, "gen-0002/spo.dat");
    *flaky.target.lock().unwrap() = Some(layout::blob_key(&id));
    let repo = fixture::repo_with(flaky.clone(), |_| {}).await;
    let blobs: usize = m.files.iter().map(|f| f.blobs.len()).sum();

    // two bad reads: the third succeeds
    flaky.corrupt.store(2, SeqCst);
    flaky.gets.store(0, SeqCst);
    repo.restore("b1", &dir.path().join("a"), &RestoreOptions::default())
        .await
        .unwrap();
    // the manifest, every blob once, and two retries
    assert_eq!(flaky.gets.load(SeqCst), 1 + blobs + 2);

    // three bad reads fail
    flaky.corrupt.store(3, SeqCst);
    let e = repo
        .restore("b1", &dir.path().join("b"), &RestoreOptions::default())
        .await
        .unwrap_err();
    assert!(e.message().contains("checksum mismatch"), "{e}");
    assert!(!dir.path().join("b").exists());
}

#[tokio::test]
async fn hostile_manifests_are_refused_before_anything_is_written() {
    let (dir, repo, good) = setup().await;
    let parent = dir.path().join("out");
    std::fs::create_dir(&parent).unwrap();
    let tmp = parent.join("t");
    let attempt = |m: Manifest| {
        let (repo, tmp) = (&repo, tmp.clone());
        async move {
            let m = Manifest {
                name: "evil".into(),
                ..m
            };
            fixture::put_manifest(repo, &m).await;
            repo.restore("evil", &tmp, &RestoreOptions::default())
                .await
                .unwrap_err()
        }
    };
    let i = good
        .files
        .iter()
        .position(|f| f.path == "gen-0001/spo.dat")
        .unwrap();
    for (path, field) in [
        ("../x", format!("files[{i}].path")),
        ("gen-0001/../../x", format!("files[{i}].path")),
        ("/etc/passwd", format!("files[{i}].path")),
        ("CURRENT", format!("files[{}].path", i.max(1))),
    ] {
        let mut m = good.clone();
        m.files[i].path = path.into();
        let e = attempt(m).await;
        assert_eq!(e.code(), Code::InvalidBackup, "{path}: {e}");
        if path != "CURRENT" {
            assert_eq!(e.body()["field"], field.as_str(), "{path}");
        }
    }
    let mut m = good.clone();
    m.files[i].size += 1;
    assert_eq!(
        attempt(m).await.body()["field"],
        format!("files[{i}].size").as_str()
    );
    let mut m = good.clone();
    m.files[i].blobs[0].id = "ABC".into();
    assert_eq!(
        attempt(m).await.body()["field"],
        format!("files[{i}].blobs[0].id").as_str()
    );

    // CURRENT naming another generation: found after the download
    let mut m = good.clone();
    let e = crate::blob::encode(b"gen-0002", false);
    repo.store
        .put(&layout::blob_key(&e.id), PutPayload::from(e.bytes.clone()))
        .await
        .unwrap();
    let c = m.files.iter_mut().find(|f| f.path == "CURRENT").unwrap();
    c.size = 8;
    c.sha256 = e.id.clone();
    c.blobs = vec![BlobRef {
        id: e.id.clone(),
        size: 8,
    }];
    let e = attempt(m).await;
    assert_eq!(e.body()["field"], "CURRENT", "{e}");

    // a file whose blobs do not hash to its recorded sha256
    let mut m = good.clone();
    m.files[i].sha256 = "0".repeat(64);
    assert_eq!(
        attempt(m).await.body()["field"],
        format!("files[{i}].sha256").as_str()
    );

    // a manifest over 16 MiB is refused unread
    repo.store
        .put(
            &layout::manifest_key("big"),
            PutPayload::from(vec![b' '; 20 << 20]),
        )
        .await
        .unwrap();
    let e = repo
        .restore("big", &tmp, &RestoreOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        (e.code(), e.body()["field"].clone()),
        (Code::InvalidBackup, "manifest".into())
    );

    // nothing was created outside, and the temporary directory is gone
    assert!(entries(&parent).is_empty());
    assert!(!dir.path().join("x").exists());
}

#[tokio::test]
async fn a_newer_index_format_is_refused() {
    let (dir, repo, mut m) = setup().await;
    m.index_format = sparkles_core::builder::FORMAT_VERSION + 1;
    fixture::put_manifest(&repo, &m).await;
    let e = repo
        .restore("b1", &dir.path().join("t"), &RestoreOptions::default())
        .await
        .unwrap_err();
    assert_eq!((e.code(), e.http_status()), (Code::IncompatibleFormat, 422));
    assert!(!dir.path().join("t").exists());
}

#[tokio::test]
async fn unknown_backups_existing_targets_and_disk_space() {
    let (dir, repo, m) = setup().await;
    let e = repo
        .restore("nope", &dir.path().join("t"), &RestoreOptions::default())
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::NoSuchBackup);
    assert!(!dir.path().join("t").exists());

    // an existing directory is refused and left alone
    let existing = dir.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("keep"), b"x").unwrap();
    let e = repo
        .restore("b1", &existing, &RestoreOptions::default())
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidRequest);
    assert_eq!(entries(&existing), ["keep"]);

    // a backup larger than the free disk space
    let mut big = fixture::repo_with(repo.store.clone(), |_| {}).await;
    big.marker.piece_bytes = u64::MAX;
    let mut huge = m.clone();
    huge.name = "huge".into();
    huge.files[0].size = 1 << 60;
    huge.files[0].blobs = vec![BlobRef {
        id: "0".repeat(64),
        size: 1 << 60,
    }];
    fixture::put_manifest(&big, &huge).await;
    let e = big
        .restore("huge", &dir.path().join("t"), &RestoreOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        (e.code(), e.http_status()),
        (Code::InsufficientStorage, 507)
    );
    assert!(!dir.path().join("t").exists());

    // a backup that fits, but would leave less than the store's disk reserve free
    let o = RestoreOptions {
        store_opts: StoreOptions {
            min_free_disk_bytes: Some(u64::MAX / 2),
            ..Default::default()
        },
        ..Default::default()
    };
    let e = repo
        .restore("b1", &dir.path().join("t"), &o)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::InsufficientStorage);
    assert!(e.message().contains("--min-free-disk-mb"), "{e}");
    assert!(!dir.path().join("t").exists());
}

#[tokio::test]
async fn cancellation_removes_the_directory() {
    let (dir, repo, _) = setup().await;
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let o = RestoreOptions {
        ctl: Ctl {
            progress: Some(Arc::new(move |_, msg: &str| {
                if msg.starts_with("downloading") {
                    cancel.store(true, SeqCst);
                }
            })),
            ..ctl
        },
        ..Default::default()
    };
    let e = repo
        .restore("b1", &dir.path().join("t"), &o)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::Cancelled);
    assert!(!dir.path().join("t").exists());
    assert!(repo.locks().await.unwrap().is_empty());
}

fn opts(identity: Identity, in_use: bool, in_place_head: Option<u64>) -> RestoreOptions {
    RestoreOptions {
        identity,
        id_in_use: Arc::new(move |_| in_use),
        in_place_head,
        ..Default::default()
    }
}

#[tokio::test]
async fn the_identity_rule() {
    let (_dir, _repo, m) = setup().await;
    let rule = |i, used, head| identity_rule(&m, &opts(i, used, head));
    // auto keeps the id only when no dataset has it (in place: the replaced one counts)
    assert!(rule(Identity::Auto, false, None).unwrap());
    assert!(!rule(Identity::Auto, true, None).unwrap());
    assert!(!rule(Identity::Auto, true, Some(3)).unwrap());
    assert!(!rule(Identity::New, false, None).unwrap());
    // keep: refused when the id is used elsewhere, or in place over a newer head
    assert!(rule(Identity::Keep, false, None).unwrap());
    assert_eq!(
        rule(Identity::Keep, true, None).unwrap_err().code(),
        Code::DuplicateDatasetId
    );
    let e = rule(Identity::Keep, true, Some(6)).unwrap_err();
    assert_eq!((e.code(), e.http_status()), (Code::DuplicateDatasetId, 409));
    assert!(e.message().contains("commits 4 to 6"), "{e}");
    assert!(rule(Identity::Keep, true, Some(3)).unwrap());
}

#[tokio::test]
async fn keep_is_refused_before_downloading() {
    let (dir, repo, _) = setup().await;
    let e = repo
        .restore(
            "b1",
            &dir.path().join("t"),
            &opts(Identity::Keep, true, None),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::DuplicateDatasetId);
    assert!(!dir.path().join("t").exists());
}

#[tokio::test]
async fn a_new_identity_forks_the_lineage() {
    let (dir, repo, m) = setup().await;
    let tmp = dir.path().join("t");
    let r = repo
        .restore("b1", &tmp, &opts(Identity::Auto, true, None))
        .await
        .unwrap();
    assert_eq!(r.identity, "new");
    assert_ne!(r.dataset_id, m.dataset.id);
    assert_eq!(
        r.forked_from,
        Some(ForkedFrom {
            id: m.dataset.id,
            seq: 3
        })
    );
    let s = Store::open(&tmp, StoreOptions::default()).unwrap();
    assert_eq!(s.dataset_id(), r.dataset_id);
    assert_eq!(s.head_commit().seq, 3);
    fixture::upd(&s, "INSERT DATA { <urn:c> <urn:p> 3 }");
    assert_eq!(s.head_commit().seq, 4);
}

#[tokio::test]
async fn read_only_repositories_restore_without_locks() {
    let (dir, repo, _) = setup().await;
    let ro = fixture::repo_with(repo.store.clone(), |c| c.readonly = true).await;
    // a lock would be refused: a fresh exclusive lock is planted
    let plant = serde_json::json!({"kind": "exclusive", "operation": "gc", "created": "x",
        "holder": {"host": "h", "pid": 1, "server": "", "version": "0"}});
    repo.store
        .put(
            &layout::lock_key("x"),
            PutPayload::from(serde_json::to_vec(&plant).unwrap()),
        )
        .await
        .unwrap();
    ro.restore("b1", &dir.path().join("t"), &RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(repo.locks().await.unwrap().len(), 1);
}

#[tokio::test]
async fn verify_at_the_restore_level() {
    let (dir, repo, _) = setup().await;
    let o = VerifyOptions {
        level: VerifyLevel::Restore,
        tmp_dir: Some(dir.path().join("tmp")),
        ..Default::default()
    };
    let v = repo.verify_restore("b1", &o).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:#?}");
    let check = v.check.unwrap();
    assert_eq!(check["mode"], "full");
    assert_eq!(check["status"], "ok", "{check:#}");
    assert!(v.missing.is_empty() && v.corrupt.is_empty());
    assert!(entries(&dir.path().join("tmp")).is_empty());
    assert_eq!(
        repo.verify_restore("nope", &o).await.unwrap_err().code(),
        Code::NoSuchBackup
    );
}

#[tokio::test]
async fn verify_at_the_restore_level_through_verify() {
    let (dir, repo, _) = setup().await;
    let o = VerifyOptions {
        level: VerifyLevel::Restore,
        tmp_dir: Some(dir.path().join("tmp")),
        ..Default::default()
    };
    let v = repo.verify(&["b1".to_string()], &o).await.unwrap();
    assert_eq!(
        (v.level, v.status),
        (VerifyLevel::Restore, VerifyStatus::Ok)
    );
    assert_eq!(v.backups[0].check.as_ref().unwrap()["status"], "ok");
    assert!(entries(&dir.path().join("tmp")).is_empty());
}

#[tokio::test]
async fn verify_reports_a_backup_that_does_not_restore_to_its_commit() {
    let (dir, repo, mut m) = setup().await;
    m.commit.quads += 1;
    fixture::put_manifest(&repo, &m).await;
    let o = VerifyOptions {
        tmp_dir: Some(dir.path().join("tmp")),
        ..Default::default()
    };
    let v = repo.verify_restore("b1", &o).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Error);
    assert_eq!(v.check.unwrap()["code"], "restore-mismatch");
    assert!(entries(&dir.path().join("tmp")).is_empty());
}

// ------------------------------------------------------------------ swap_dir ------

fn db_with(dir: &Path, name: &str, marker: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::create_dir(&p).unwrap();
    std::fs::write(p.join("marker"), marker).unwrap();
    p
}

#[test]
fn swap_dir_replaces_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = db_with(dir.path(), "db", "old");
    let restored = db_with(dir.path(), "db.restore-1", "new");
    swap_dir(&target, &restored, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(target.join("marker")).unwrap(),
        "new"
    );
    assert_eq!(entries(dir.path()), ["db"]);

    // kept on request
    let restored = db_with(dir.path(), "db.restore-2", "newer");
    swap_dir(&target, &restored, true).unwrap();
    assert_eq!(
        entries(dir.path()),
        [
            "db".to_string(),
            format!("db.replaced-{}", std::process::id())
        ]
    );
    let kept = dir
        .path()
        .join(format!("db.replaced-{}", std::process::id()));
    assert_eq!(std::fs::read_to_string(kept.join("marker")).unwrap(), "new");
    // a leftover replaced directory is not overwritten
    let restored = db_with(dir.path(), "db.restore-3", "x");
    assert_eq!(
        swap_dir(&target, &restored, false).unwrap_err().code(),
        Code::DatasetExists
    );
    std::fs::remove_dir_all(&kept).unwrap();

    // a missing target is a plain rename
    let fresh = dir.path().join("fresh");
    swap_dir(&fresh, &restored, false).unwrap();
    assert_eq!(std::fs::read_to_string(fresh.join("marker")).unwrap(), "x");
}

#[test]
fn swap_dir_refuses_an_open_database() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("db");
    let s = Store::open(&target, StoreOptions::default()).unwrap();
    let restored = db_with(dir.path(), "db.restore-1", "new");
    let e = swap_dir(&target, &restored, false).unwrap_err();
    assert_eq!(e.code(), Code::DatasetBusy);
    assert!(e.message().contains("in use by another process"), "{e}");
    drop(s);
    assert!(target.join("CURRENT").exists() && restored.exists());
}

#[test]
fn swap_dir_rolls_back_when_the_second_rename_fails() {
    let dir = tempfile::tempdir().unwrap();
    let target = db_with(dir.path(), "db", "old");
    let restored = db_with(dir.path(), "db.restore-1", "new");
    FAIL_SECOND_RENAME.with(|f| f.set(true));
    let e = swap_dir(&target, &restored, false).unwrap_err();
    FAIL_SECOND_RENAME.with(|f| f.set(false));
    assert!(e.message().contains("failpoint"), "{e}");
    assert_eq!(
        std::fs::read_to_string(target.join("marker")).unwrap(),
        "old"
    );
    assert_eq!(entries(dir.path()), ["db", "db.restore-1"]);
    // and the lock was released
    drop(lock_database(&target).unwrap());
    swap_dir(&target, &restored, false).unwrap();
}

// ------------------------------------------------------------- full round trip ------

/// Backups of a live store, taken while another thread commits, restore to exactly
/// their captured commits.
#[tokio::test(flavor = "multi_thread")]
async fn backups_under_concurrent_writes_restore_to_their_commits() {
    use crate::{CreateOptions, Source};
    use sparkles_core::history::At;
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    let store = Arc::new(Store::open(&root, StoreOptions::default()).unwrap());
    let repo = fixture::memory_repo().await;
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (store, stop) = (store.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(SeqCst) {
                fixture::upd(&store, &format!("INSERT DATA {{ <urn:w{i}> <urn:p> {i} }}"));
                i += 1;
            }
        })
    };
    let mut holds = Vec::new();
    let mut names = Vec::new();
    for n in 1..=5 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let name = format!("c{n}");
        let cap = store.backup_capture(&name).unwrap();
        holds.push(cap.lock_hold);
        let o = CreateOptions {
            name: name.clone(),
            dataset_name: "ds".into(),
            ..Default::default()
        };
        repo.create(Source::from(cap), &o).await.unwrap();
        names.push(name);
    }
    stop.store(true, SeqCst);
    writer.join().unwrap();

    for name in names {
        let m = repo.manifest(&name).await.unwrap();
        let tmp = dir.path().join(&name);
        let o = RestoreOptions {
            check: CheckLevel::Full,
            ..Default::default()
        };
        let r = repo.restore(&name, &tmp, &o).await.unwrap();
        assert_eq!(r.check.unwrap()["status"], "ok");
        let restored = Store::open(&tmp, StoreOptions::default()).unwrap();
        assert_eq!(restored.head_commit().seq, m.commit.seq);
        assert_eq!(restored.snapshot().len(), m.commit.quads);
        let mut got = Vec::new();
        restored.dump_nquads(&mut got).unwrap();
        let mut want = Vec::new();
        store
            .dump_nquads_at(&At::Commit(m.commit.seq), &mut want)
            .unwrap();
        assert_eq!(got, want, "{name}");
    }
    holds.sort();
    // the p99 of five is the largest
    assert!(
        *holds.last().unwrap() < std::time::Duration::from_millis(5),
        "{holds:?}"
    );
}

#[tokio::test]
async fn branch_ordinal_reservation_reads_legacy_and_current_manifest_metadata() {
    for next_ordinal in [None, Some(7)] {
        let (dir, repo, mut manifest) = setup().await;
        manifest.dataset.branch = Some(sparkles_core::store::BackupBranch {
            dataset_id: Uuid::new_v4(),
            id: manifest.dataset.id,
            name: "work".into(),
            next_ordinal: 4,
        });
        manifest.dataset.next_ordinal = next_ordinal;
        fixture::put_manifest(&repo, &manifest).await;
        let target = dir.path().join("restore");
        repo.restore("b1", &target, &RestoreOptions::default())
            .await
            .unwrap();
        let store = Store::open(&target, StoreOptions::default()).unwrap();
        let branch = store.create_branch("new", &Default::default()).unwrap();
        assert_eq!(branch.ordinal as u32, next_ordinal.unwrap_or(4));
        assert_ne!(store.dataset_id(), manifest.dataset.id);
    }
}
