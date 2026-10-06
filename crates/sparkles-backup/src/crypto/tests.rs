use super::*;
use crate::{
    Code, CreateOptions, Ctl, GcOptions, OpenEnv, RepoConfig, RepoType, Repository, RestoreOptions,
    Source, VerifyLevel, VerifyOptions, VerifyStatus, fixture, layout,
};
use futures::TryStreamExt;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload, memory::InMemory};
use sparkles_core::store::{Store, StoreOptions};
use std::{sync::Arc, time::Duration};

pub(super) fn options(byte: u8) -> EncryptionOptions {
    EncryptionOptions {
        keys: vec![LocalKey::new("primary", LocalKeySource::File, [byte; 32]).unwrap()],
        single_key_ok: true,
        ..Default::default()
    }
}
pub(super) async fn open(
    store: Arc<InMemory>,
    cache: Option<std::path::PathBuf>,
    options: &EncryptionOptions,
) -> Repository {
    Repository::open_encrypted(
        &RepoConfig {
            name: "encrypted".into(),
            kind: RepoType::Memory,
            conditional_writes: true,
            ..Default::default()
        },
        &OpenEnv {
            store: Some(store),
            cache_dir: cache,
            ..Default::default()
        },
        options,
    )
    .await
    .unwrap()
}
async fn backup(repo: &Repository, path: &std::path::Path, name: &str) {
    repo.create(
        Source::from_closed_dir(path).unwrap(),
        &CreateOptions {
            name: name.into(),
            dataset_name: "private-dataset-label".into(),
            note: Some("private-note-needle".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn identical_local_key_bytes_recover_every_epoch_across_provider_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    fixture::make_db(&db);
    let expected = Store::open(&db, StoreOptions::default())
        .unwrap()
        .snapshot()
        .len();
    let store = Arc::new(InMemory::new());
    let original = EncryptionOptions {
        keys: vec![LocalKey::new("primary", LocalKeySource::Env, [81; 32]).unwrap()],
        single_key_ok: true,
        ..Default::default()
    };
    let repo = open(store.clone(), None, &original).await;
    backup(&repo, &db, "historical").await;
    repo.key_rotate(&Ctl::default()).await.unwrap();
    backup(&repo, &db, "current").await;
    let ids = repo
        .key_list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();
    for (index, source) in [
        LocalKeySource::File,
        LocalKeySource::Credential,
        LocalKeySource::Command,
        LocalKeySource::Env,
    ]
    .into_iter()
    .enumerate()
    {
        let supplied = EncryptionOptions {
            keys: vec![LocalKey::new("primary", source, [81; 32]).unwrap()],
            single_key_ok: true,
            ..Default::default()
        };
        let recovered = open(store.clone(), None, &supplied).await;
        assert_eq!(
            recovered
                .security()
                .await
                .unwrap()
                .sealed
                .unwrap()
                .keys
                .len(),
            2
        );
        let completed = recovered
            .key_add(&supplied.keys[0], &Ctl::default())
            .await
            .unwrap();
        assert_eq!(completed.into_iter().map(|s| s.id).collect::<Vec<_>>(), ids);
        assert!(
            recovered
                .key_list()
                .await
                .unwrap()
                .iter()
                .all(|s| s.source == "env")
        );
        for name in ["historical", "current"] {
            let restored = tmp.path().join(format!("restore-{index}-{name}"));
            recovered
                .restore(name, &restored, &RestoreOptions::default())
                .await
                .unwrap();
            let restored = Store::open(&restored, StoreOptions::default()).unwrap();
            assert_eq!(restored.snapshot().len(), expected);
        }
        let wrong = EncryptionOptions {
            keys: vec![LocalKey::new("primary", source, [82; 32]).unwrap()],
            single_key_ok: true,
            ..Default::default()
        };
        assert!(
            Repository::open_encrypted(
                &RepoConfig {
                    name: "encrypted".into(),
                    kind: RepoType::Memory,
                    conditional_writes: true,
                    ..Default::default()
                },
                &OpenEnv {
                    store: Some(store.clone()),
                    ..Default::default()
                },
                &wrong
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn encrypted_repository_roundtrip_dedup_append_cache_verify_and_gc() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("db");
    let id = fixture::make_big_db(&path);
    let store = Arc::new(InMemory::new());
    let opts = options(13);
    let repo = open(store.clone(), Some(tmp.path().join("cache")), &opts).await;
    backup(&repo, &path, "first").await;
    backup(&repo, &path, "same").await;
    let first = repo.manifest("first").await.unwrap();
    let same = repo.manifest("same").await.unwrap();
    assert_eq!(first.files, same.files);
    assert_eq!(same.stats.new_blobs, 0);
    assert_eq!(first.encryption.as_ref().unwrap()["epoch"], 1);
    {
        let db = Store::open(&path, StoreOptions::default()).unwrap();
        fixture::upd(&db, "INSERT DATA { <urn:private-subject> <urn:p> 99 }");
    }
    backup(&repo, &path, "append").await;
    let append = repo.manifest("append").await.unwrap();
    let old = first
        .files
        .iter()
        .find(|f| f.path.ends_with("wal.log"))
        .unwrap();
    let new = append
        .files
        .iter()
        .find(|f| f.path.ends_with("wal.log"))
        .unwrap();
    assert!(new.blobs.starts_with(&old.blobs));
    assert!(new.blobs.len() > old.blobs.len());
    let restored = tmp.path().join("restore");
    repo.restore("append", &restored, &RestoreOptions::default())
        .await
        .unwrap();
    let db = Store::open(&restored, StoreOptions::default()).unwrap();
    assert_eq!(db.dataset_id(), id);
    assert_eq!(db.head_commit().seq, append.commit.seq);
    drop(db);
    for level in [VerifyLevel::Data, VerifyLevel::Restore] {
        let report = repo
            .verify(
                &["append".into()],
                &VerifyOptions {
                    level,
                    tmp_dir: Some(tmp.path().to_owned()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(report.status, VerifyStatus::Ok);
    }
    let mut listed = store.list(None);
    while let Some(meta) = listed.try_next().await.unwrap() {
        let bytes = store
            .get(&meta.location)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        for secret in [
            b"private-dataset-label".as_slice(),
            b"private-note-needle",
            b"urn:private-subject",
        ] {
            assert!(
                !bytes.windows(secret.len()).any(|b| b == secret),
                "{}",
                meta.location
            );
        }
    }
    let cached = std::fs::read(
        tmp.path()
            .join("cache")
            .join(repo.id().to_string())
            .join("append.json"),
    )
    .unwrap();
    assert!(!String::from_utf8_lossy(&cached).contains("private-note-needle"));
    let fresh = open(store.clone(), Some(tmp.path().join("cache")), &opts).await;
    assert_eq!(fresh.list(&Default::default()).await.unwrap().len(), 3);
    // A tampered disk cache is discarded and refetched/authenticated.
    std::fs::write(
        tmp.path()
            .join("cache")
            .join(repo.id().to_string())
            .join("append.json"),
        b"{\"manifest\":\"plaintext\"}",
    )
    .unwrap();
    let fresh = open(store.clone(), Some(tmp.path().join("cache")), &opts).await;
    assert_eq!(fresh.list(&Default::default()).await.unwrap().len(), 3);
    repo.delete("first").await.unwrap();
    repo.delete("same").await.unwrap();
    repo.delete("append").await.unwrap();
    let gc = repo
        .gc(&GcOptions {
            grace: Duration::ZERO,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(gc.deleted > 0);
}

#[tokio::test]
async fn missing_wrong_keys_and_authenticated_tampering_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("db");
    fixture::make_db(&path);
    let store = Arc::new(InMemory::new());
    let opts = options(14);
    let repo = open(store.clone(), None, &opts).await;
    backup(&repo, &path, "first").await;
    let cfg = repo.config().clone();
    let env = OpenEnv {
        store: Some(store.clone()),
        ..Default::default()
    };
    assert_eq!(
        Repository::open(&cfg, &env).await.unwrap_err().code(),
        Code::RepositoryKeyRequired
    );
    let error = Repository::open_encrypted(&cfg, &env, &options(15))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::WrongRepositoryKey);
    assert!(!error.to_string().contains(&"0f".repeat(32)));
    let manifest = repo.manifest("first").await.unwrap();
    let blob = &manifest.files[0].blobs[0];
    let key = layout::blob_key(&blob.id);
    let original = store.get(&key).await.unwrap().bytes().await.unwrap();
    let mut bad = original.to_vec();
    bad[32] ^= 1;
    store.put(&key, PutPayload::from(bad)).await.unwrap();
    let restore = tmp.path().join("failed");
    assert!(
        repo.restore("first", &restore, &RestoreOptions::default())
            .await
            .is_err()
    );
    assert!(!restore.exists());
    let report = repo
        .verify(
            &["first".into()],
            &VerifyOptions {
                level: VerifyLevel::Data,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.status, VerifyStatus::Error);
    assert!(report.backups[0].corrupt.contains(&blob.id));
    store.put(&key, PutPayload::from(original)).await.unwrap();
    let bytes = store
        .get(&layout::manifest_key("first"))
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    store
        .put(
            &layout::manifest_key("renamed"),
            PutPayload::from(bytes.clone()),
        )
        .await
        .unwrap();
    assert!(repo.manifest("renamed").await.is_err());
    assert!(
        repo.gc(&GcOptions {
            grace: Duration::ZERO,
            ..Default::default()
        })
        .await
        .is_err()
    );
    assert!(store.head(&key).await.is_ok());
}

#[tokio::test]
async fn rotation_keeps_historical_backups_readable_disables_cross_epoch_reuse_and_retires_safely()
{
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("db");
    fixture::make_db(&path);
    let store = Arc::new(InMemory::new());
    let opts = options(19);
    let repo = open(store.clone(), None, &opts).await;
    backup(&repo, &path, "old").await;
    let stale = open(store.clone(), None, &opts).await;
    let old = repo.manifest("old").await.unwrap();
    let slots = repo.key_list().await.unwrap();
    assert_eq!(
        repo.key_remove(slots[0].id, &Ctl::default())
            .await
            .unwrap_err()
            .code(),
        Code::LastKeySlot
    );
    let recovery = LocalKey::new("recovery", LocalKeySource::File, [20; 32]).unwrap();
    let added = repo.key_add(&recovery, &Ctl::default()).await.unwrap();
    let second = EncryptionOptions {
        keys: vec![recovery],
        single_key_ok: true,
        ..Default::default()
    };
    assert!(
        Repository::open_encrypted(
            repo.config(),
            &OpenEnv {
                store: Some(store.clone()),
                ..Default::default()
            },
            &second
        )
        .await
        .is_ok()
    );
    assert!(repo.key_remove(added[0].id, &Ctl::default()).await.unwrap());
    assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
    backup(&stale, &path, "new").await;
    let new = repo.manifest("new").await.unwrap();
    assert_eq!(new.encryption.as_ref().unwrap()["epoch"], 2);
    assert!(new.parent.is_none());
    let old_ids: std::collections::HashSet<_> = old
        .files
        .iter()
        .flat_map(|f| f.blobs.iter().map(|b| &b.id))
        .collect();
    assert!(
        new.files
            .iter()
            .flat_map(|f| &f.blobs)
            .all(|b| !old_ids.contains(&b.id))
    );
    repo.restore(
        "old",
        &tmp.path().join("restored"),
        &RestoreOptions::default(),
    )
    .await
    .unwrap();
    assert!(repo.key_retire(1, &Ctl::default()).await.is_err());
    repo.delete("old").await.unwrap();
    assert!(repo.key_retire(1, &Ctl::default()).await.is_err());
    repo.gc(&GcOptions {
        grace: Duration::ZERO,
        ..Default::default()
    })
    .await
    .unwrap();
    repo.key_retire(1, &Ctl::default()).await.unwrap();
    let fresh = open(store.clone(), None, &opts).await;
    assert_eq!(fresh.manifest("new").await.unwrap(), new);
    assert!(fresh.key_list().await.unwrap().iter().all(|s| s.epoch == 2));
}

#[tokio::test]
#[cfg(feature = "fs")]
async fn passphrase_and_filesystem_repository_restore_and_readonly_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    fixture::make_db(&db);
    let opts = EncryptionOptions {
        passphrases: vec![
            Passphrase::new(
                "recovery",
                b"test passphrase with private material".to_vec(),
            )
            .unwrap(),
        ],
        single_key_ok: true,
        ..Default::default()
    };
    let cfg = RepoConfig {
        name: "fs-encrypted".into(),
        kind: RepoType::Fs,
        path: Some(tmp.path().join("repository").display().to_string()),
        conditional_writes: true,
        ..Default::default()
    };
    let repo = Repository::open_encrypted(&cfg, &OpenEnv::default(), &opts)
        .await
        .unwrap();
    backup(&repo, &db, "first").await;
    let mut readonly = cfg.clone();
    readonly.readonly = true;
    let reader = Repository::open_encrypted(&readonly, &OpenEnv::default(), &opts)
        .await
        .unwrap();
    reader
        .restore(
            "first",
            &tmp.path().join("restore"),
            &RestoreOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        reader.key_rotate(&Ctl::default()).await.unwrap_err().code(),
        Code::RepositoryReadOnly
    );
    let ctl = Ctl::default();
    ctl.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(repo.key_rotate(&ctl).await.unwrap_err().is_cancelled());
    let mut wrong = opts.clone();
    wrong.passphrases = vec![Passphrase::new("recovery", b"wrong".to_vec()).unwrap()];
    assert_eq!(
        Repository::open_encrypted(&cfg, &OpenEnv::default(), &wrong)
            .await
            .unwrap_err()
            .code(),
        Code::WrongRepositoryKey
    );
}

#[tokio::test]
async fn concurrent_initializers_open_the_same_durable_winner() {
    let store = Arc::new(InMemory::new());
    let options = options(23);
    let cfg = RepoConfig {
        name: "race".into(),
        kind: RepoType::Memory,
        conditional_writes: true,
        ..Default::default()
    };
    let env = OpenEnv {
        store: Some(store),
        ..Default::default()
    };
    let (a, b) = tokio::join!(
        Repository::open_encrypted(&cfg, &env, &options),
        Repository::open_encrypted(&cfg, &env, &options)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.id(), b.id());
    assert_eq!(a.key_list().await.unwrap().len(), 1);
    assert_eq!(b.key_list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn one_request_limit_does_not_deadlock_encrypted_operations() {
    let store = Arc::new(InMemory::new());
    let opts = options(24);
    let cfg = RepoConfig {
        name: "serial".into(),
        kind: RepoType::Memory,
        conditional_writes: true,
        max_concurrency: Some(1),
        ..Default::default()
    };
    let env = OpenEnv {
        store: Some(store),
        ..Default::default()
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        let repo = Repository::open_encrypted(&cfg, &env, &opts).await.unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db");
        fixture::make_db(&db);
        backup(&repo, &db, "first").await;
        assert_eq!(
            repo.verify(
                &["first".into()],
                &VerifyOptions {
                    level: VerifyLevel::Data,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .status,
            VerifyStatus::Ok
        );
        repo.key_rotate(&Ctl::default()).await.unwrap();
        repo.delete("first").await.unwrap();
        repo.gc(&GcOptions {
            grace: Duration::ZERO,
            ..Default::default()
        })
        .await
        .unwrap();
        repo.key_retire(1, &Ctl::default()).await.unwrap();
    })
    .await
    .expect("serial encrypted operations must release storage permits");
}

#[tokio::test]
async fn interrupted_key_add_retries_missing_epochs_without_duplicate_slots() {
    let store = Arc::new(InMemory::new());
    let opts = options(45);
    let repo = open(store, None, &opts).await;
    repo.key_rotate(&Ctl::default()).await.unwrap();
    let recovery = LocalKey::new("recovery", LocalKeySource::Env, [46; 32]).unwrap();
    let snapshot = repo.security().await.unwrap().sealed.unwrap();
    let partial = slots::Slot::local(snapshot.key(1).unwrap(), &recovery).unwrap();
    repository::put_slot(&repo.store, &partial).await.unwrap();
    let completed = repo.key_add(&recovery, &Ctl::default()).await.unwrap();
    assert_eq!(completed.len(), 2);
    assert!(completed.iter().any(|s| s.id == partial.id));
    let again = repo.key_add(&recovery, &Ctl::default()).await.unwrap();
    assert_eq!(
        completed.iter().map(|s| s.id).collect::<Vec<_>>(),
        again.iter().map(|s| s.id).collect::<Vec<_>>()
    );
    assert_eq!(repo.key_list().await.unwrap().len(), 4);
    let different = LocalKey::new("recovery", LocalKeySource::Env, [47; 32]).unwrap();
    assert!(repo.key_add(&different, &Ctl::default()).await.is_err());
    let mut invalid = different.clone();
    invalid.label = "\ninvalid".into();
    assert!(repo.key_add(&invalid, &Ctl::default()).await.is_err());
    assert_eq!(repo.key_list().await.unwrap().len(), 4);
}

#[tokio::test]
async fn independently_initialized_repositories_cannot_exchange_encrypted_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    fixture::make_db(&db);
    let opts = options(48);
    let a = Arc::new(InMemory::new());
    let b = Arc::new(InMemory::new());
    let left = open(a.clone(), None, &opts).await;
    let right = open(b.clone(), None, &opts).await;
    backup(&left, &db, "first").await;
    backup(&right, &db, "first").await;
    let lm = left.manifest("first").await.unwrap();
    let rm = right.manifest("first").await.unwrap();
    assert_ne!(left.id(), right.id());
    assert_ne!(lm.files[0].blobs[0].id, rm.files[0].blobs[0].id);
    let sealed = a
        .get(&layout::manifest_key("first"))
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    b.put(&layout::manifest_key("first"), PutPayload::from(sealed))
        .await
        .unwrap();
    assert!(right.manifest("first").await.is_err());
    assert!(
        right
            .gc(&GcOptions {
                grace: Duration::ZERO,
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert!(
        b.head(&layout::blob_key(&rm.files[0].blobs[0].id))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn rotation_waits_for_an_admitted_create_and_new_writers_capture_the_new_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    fixture::make_db(&db);
    let store = Arc::new(InMemory::new());
    let opts = options(50);
    let repo = Arc::new(open(store, None, &opts).await);
    let admitted = crate::lock::acquire(
        &repo,
        crate::LockKind::Shared,
        crate::LockOperation::Create,
        &Ctl::default(),
    )
    .await
    .unwrap();
    let other = repo.clone();
    let rotating = tokio::spawn(async move { other.key_rotate(&Ctl::default()).await });
    tokio::task::yield_now().await;
    assert!(!rotating.is_finished());
    backup(&repo, &db, "before").await;
    assert_eq!(
        repo.manifest("before").await.unwrap().encryption.unwrap()["epoch"],
        1
    );
    admitted.release().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), rotating)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        2
    );
    backup(&repo, &db, "after").await;
    assert_eq!(
        repo.manifest("after").await.unwrap().encryption.unwrap()["epoch"],
        2
    );
}
