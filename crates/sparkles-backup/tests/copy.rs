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
    let mut damaged = stored.to_vec();
    damaged[6] = 8;
    source_store.put(&key, damaged.into()).await.unwrap();
    let before = source.requests().get.ok;
    let error = source.copy_to(&target, &select).await.unwrap_err();
    assert!(error.message().contains("checksum mismatch"));
    assert_eq!(source.requests().get.ok - before, 4); // manifest and three blob reads
    let mut unsupported = stored.to_vec();
    unsupported[5] = 2;
    source_store.put(&key, unsupported.into()).await.unwrap();
    let before = source.requests().get.ok;
    assert_eq!(
        source.copy_to(&target, &select).await.unwrap_err().code(),
        Code::IncompatibleFormat
    );
    assert_eq!(source.requests().get.ok - before, 2); // manifest and one blob read
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

#[tokio::test]
async fn copies_across_fixed_piece_sizes_and_across_source_piece_boundaries() {
    use sparkles_backup::{FileKind, OpenEnv, Repository};
    use std::sync::Arc;

    async fn with_piece_size(name: &str, size: u64) -> Repository {
        let store = Arc::new(object_store::memory::InMemory::new());
        let mut marker = layout::Marker::new(uuid::Uuid::new_v4(), sparkles_backup::now_rfc3339());
        marker.piece_bytes = size;
        store
            .put(&layout::marker_key(), marker.to_bytes().into())
            .await
            .unwrap();
        open_on(store, &memory_config(name)).await
    }

    let source = with_piece_size("source", 7).await;
    let smaller = with_piece_size("smaller", 5).await;
    let larger = Repository::open(&memory_config("larger"), &OpenEnv::default())
        .await
        .unwrap();
    let path = "gen-0001/spo.dat";
    let data = noise(17, 5);
    let snapshot = || {
        synthetic(vec![
            ("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec()),
            (path, FileKind::Immutable, data.clone()),
        ])
    };
    source
        .create(snapshot(), &opts("copy", "ds"))
        .await
        .unwrap();
    smaller
        .create(snapshot(), &opts("native", "ds"))
        .await
        .unwrap();
    for target in [&smaller, &larger] {
        source
            .copy_to(target, &CopyOptions::default())
            .await
            .unwrap();
    }
    let copied = smaller.manifest("copy").await.unwrap();
    let smaller_file = copied.files.iter().find(|file| file.path == path).unwrap();
    assert_eq!(
        smaller_file
            .blobs
            .iter()
            .map(|blob| blob.size)
            .collect::<Vec<_>>(),
        [5, 5, 5, 2]
    );
    let native = smaller.manifest("native").await.unwrap();
    assert_eq!(
        smaller_file,
        native.files.iter().find(|file| file.path == path).unwrap()
    );
    assert_eq!(
        larger.manifest("copy").await.unwrap().files,
        source.manifest("copy").await.unwrap().files
    );
}

#[tokio::test]
async fn filtering_requires_readable_manifests_unless_names_are_explicit() {
    let (source, store) = memory_repo().await;
    let (target, _) = memory_repo().await;
    source
        .create(
            synthetic(vec![(
                "CURRENT",
                sparkles_backup::FileKind::Meta,
                b"gen-0001\n".to_vec(),
            )]),
            &opts("selected", "ds"),
        )
        .await
        .unwrap();
    store
        .put(
            &layout::manifest_key("corrupt"),
            PutPayload::from_static(b"corrupt"),
        )
        .await
        .unwrap();
    let options = CopyOptions {
        dataset: Some("ds".into()),
        ..Default::default()
    };
    assert_eq!(
        source.copy_to(&target, &options).await.unwrap_err().code(),
        Code::InvalidBackup
    );
    let named = CopyOptions {
        names: vec!["selected".into()],
        ..options
    };
    assert_eq!(
        source.copy_to(&target, &named).await.unwrap().copied,
        ["selected"]
    );
}
