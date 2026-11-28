mod common;

use common::*;
use object_store::{ObjectStoreExt, PutPayload};
use sparkles_backup::{Code, CopyOptions, layout};
#[cfg(feature = "fs")]
use sparkles_backup::{OpenEnv, RepoConfig, Repository, RestoreOptions};

#[cfg(feature = "fs")]
#[tokio::test]
async fn copies_selected_snapshots_and_restores_from_a_local_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    make_db(&db);
    let (source, _) = memory_repo().await;
    source
        .create(closed_source(&db), &opts("first", "ds"))
        .await
        .unwrap();
    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    source
        .create(closed_source(&db), &opts("second", "ds"))
        .await
        .unwrap();
    source
        .create(closed_source(&db), &opts("unselected", "other"))
        .await
        .unwrap();

    let config = RepoConfig {
        path: Some(tmp.path().join("repository").display().to_string()),
        ..RepoConfig::from_url("target", "file:///unused").unwrap()
    };
    let target = Repository::open(&config, &OpenEnv::default())
        .await
        .unwrap();
    let options = CopyOptions {
        dataset: Some("ds".into()),
        since: Some("2026-01-01T00:00:00Z".into()),
        ..Default::default()
    };
    let report = source.copy_to(&target, &options).await.unwrap();
    assert_eq!(report.copied.len(), 2);
    let first = target.manifest("first").await.unwrap();
    let second = target.manifest("second").await.unwrap();
    assert_eq!(first.id, source.manifest("first").await.unwrap().id);
    assert_eq!(first.repository_id, target.id());
    assert!(report.uploaded_blobs < first.stats.blobs + second.stats.blobs);
    assert_eq!(
        report.uploaded_blobs,
        count(target.store().as_ref(), "blobs").await as u64
    );
    assert_eq!(
        target.manifest("unselected").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    let retry = source.copy_to(&target, &options).await.unwrap();
    assert_eq!(retry.already_present, report.copied);
    assert_eq!(retry.uploaded_blobs, 0);
    let restored = tmp.path().join("restored");
    let restore = target
        .restore("second", &restored, &RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(restore.backup.commit.seq, 4);
    assert_eq!(restore.backup.commit.quads, 2);
}

#[tokio::test]
async fn corruption_and_name_conflicts_do_not_publish_or_replace_a_manifest() {
    let (source, source_store) = memory_repo().await;
    let (target, target_store) = memory_repo().await;
    let data = || {
        synthetic(vec![(
            "CURRENT",
            sparkles_backup::FileKind::Meta,
            b"gen-0001\n".to_vec(),
        )])
    };
    source.create(data(), &opts("copy", "ds")).await.unwrap();
    let source_manifest = source.manifest("copy").await.unwrap();
    let key = layout::blob_key(&source_manifest.files[0].blobs[0].id);
    let stored = source_store.get(&key).await.unwrap().bytes().await.unwrap();
    let select = CopyOptions {
        names: vec!["copy".into()],
        ..Default::default()
    };
    target_store
        .put(&key, PutPayload::from_static(b"corrupt"))
        .await
        .unwrap();
    assert!(source.copy_to(&target, &select).await.is_err());
    assert_eq!(
        target.manifest("copy").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    target_store.delete(&key).await.unwrap();

    source_store.delete(&key).await.unwrap();
    assert!(source.copy_to(&target, &select).await.is_err());
    assert_eq!(
        target.manifest("copy").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    let mut unsupported = stored.to_vec();
    unsupported[5] = 2;
    source_store.put(&key, unsupported.into()).await.unwrap();
    assert_eq!(
        source.copy_to(&target, &select).await.unwrap_err().code(),
        Code::IncompatibleFormat
    );
    assert_eq!(
        target.manifest("copy").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    source_store.put(&key, stored.into()).await.unwrap();

    let manifest_key = layout::manifest_key("copy");
    let mut corrupt_manifest = source_manifest.clone();
    corrupt_manifest.files[0].sha256 = "aa".repeat(32);
    source_store
        .put(
            &manifest_key,
            serde_json::to_vec(&corrupt_manifest).unwrap().into(),
        )
        .await
        .unwrap();
    assert_eq!(
        source.copy_to(&target, &select).await.unwrap_err().code(),
        Code::InvalidBackup
    );
    assert_eq!(
        target.manifest("copy").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    source_store
        .put(
            &manifest_key,
            serde_json::to_vec(&source_manifest).unwrap().into(),
        )
        .await
        .unwrap();

    target.create(data(), &opts("copy", "other")).await.unwrap();
    let existing = target.manifest("copy").await.unwrap();
    assert_eq!(
        source.copy_to(&target, &select).await.unwrap_err().code(),
        Code::BackupExists
    );
    assert_eq!(target.manifest("copy").await.unwrap(), existing);
}

#[tokio::test]
async fn manifest_put_failure_leaves_reusable_blobs_and_releases_locks() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let (source, _) = memory_repo().await;
    source
        .create(
            synthetic(vec![(
                "CURRENT",
                sparkles_backup::FileKind::Meta,
                b"gen-0001\n".to_vec(),
            )]),
            &opts("copy", "ds"),
        )
        .await
        .unwrap();

    let mem = Arc::new(object_store::memory::InMemory::new());
    let denied = Arc::new(AtomicBool::new(true));
    let mut fault = Faulty::new(mem.clone());
    let deny = denied.clone();
    fault.hook = Some(Box::new(move |key, _| {
        (key == &layout::manifest_key("copy") && deny.load(Ordering::Relaxed))
            .then(|| generic("manifest denied"))
    }));
    let target = open_on(Arc::new(fault), &memory_config("target")).await;
    assert!(
        source
            .copy_to(&target, &CopyOptions::default())
            .await
            .is_err()
    );
    assert_eq!(count(mem.as_ref(), "backups").await, 0);
    assert_eq!(count(mem.as_ref(), "blobs").await, 1);
    assert_eq!(count(mem.as_ref(), "locks").await, 0);
    denied.store(false, Ordering::Relaxed);
    let retry = source
        .copy_to(&target, &CopyOptions::default())
        .await
        .unwrap();
    assert_eq!(retry.copied, ["copy"]);
    assert_eq!(retry.uploaded_blobs, 0);
}
