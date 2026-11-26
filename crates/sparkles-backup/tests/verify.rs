//! Verifying backups at every level, and restoring what `create` wrote.

mod common;

use common::*;
use object_store::path::Path as Key;
use object_store::{ObjectStoreExt, PutPayload};
use sparkles_backup::{
    Code, OpenEnv, RepoConfig, Repository, RestoreOptions, VerifyLevel, VerifyOptions, VerifyStatus,
};
use sparkles_core::store::{Store, StoreOptions};

fn configuration_store(root: &std::path::Path) -> Store {
    drop(Store::open(root, StoreOptions::default()).unwrap());
    let preparation =
        sparkles_core::access::Preparation::new(sparkles_core::task::Control::none(), 64 << 20)
            .unwrap();
    sparkles_core::migrate::activate_configuration_history(root, false, &preparation).unwrap();
    let store = Store::open(root, StoreOptions::default()).unwrap();
    upd(&store, "INSERT DATA { <urn:history> <urn:p> 1 }");
    store
}

#[tokio::test]
async fn configuration_history_restores_exact_native_binding_without_optional_check() {
    let temporary = tempfile::tempdir().unwrap();
    let store = configuration_store(&temporary.path().join("source"));
    let preparation =
        sparkles_core::access::Preparation::new(sparkles_core::task::Control::none(), 64 << 20)
            .unwrap();
    let at = sparkles_core::history::At::Commit(store.head_commit().seq);
    let source = store.evidence_view(&at, &preparation).unwrap();
    let (repo, _) = memory_repo().await;
    repo.create(
        store.backup_capture("native").unwrap().into(),
        &opts("native", "ds"),
    )
    .await
    .unwrap();
    let manifest = repo.manifest("native").await.unwrap();
    assert!(
        manifest
            .requires
            .iter()
            .any(|feature| feature == "evidence-configuration-history/1")
    );
    assert_eq!(
        manifest
            .files
            .iter()
            .filter(|file| file.path.starts_with("evidence-history/"))
            .count(),
        2
    );
    let destination = temporary.path().join("kept");
    let report = repo
        .restore(
            "native",
            &destination,
            &RestoreOptions {
                identity: sparkles_backup::Identity::Keep,
                check: sparkles_backup::CheckLevel::None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.identity, "kept");
    for file in manifest
        .files
        .iter()
        .filter(|file| file.path.starts_with("evidence-history/"))
    {
        assert_eq!(
            std::fs::read(destination.join(&file.path)).unwrap(),
            std::fs::read(temporary.path().join("source").join(&file.path)).unwrap()
        );
        let mut parent = destination.join(&file.path);
        while parent.pop() && parent != destination {
            assert!(std::fs::symlink_metadata(&parent).unwrap().is_dir());
        }
    }
    let restored = Store::open(&destination, StoreOptions::default()).unwrap();
    assert_eq!(restored.dataset_id(), store.dataset_id());
    assert_eq!(restored.head_commit(), store.head_commit());
    assert_eq!(
        restored
            .evidence_view(&at, &preparation)
            .unwrap()
            .configuration(),
        source.configuration()
    );
    // A fresh identity establishes a new destination configuration-history root
    // with a format 2 origin descriptor authenticating the imported history.
    let fresh_dir = temporary.path().join("fresh");
    let fresh_report = repo
        .restore(
            "native",
            &fresh_dir,
            &RestoreOptions {
                identity: sparkles_backup::Identity::New,
                check: sparkles_backup::CheckLevel::Full,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(fresh_report.identity, "new");
    assert_ne!(fresh_report.dataset_id, store.dataset_id());
    assert_eq!(
        fresh_report.forked_from,
        Some(sparkles_core::commit::ForkedFrom {
            id: store.dataset_id(),
            seq: store.head_commit().seq,
        })
    );
    assert!(fresh_dir.join("evidence-history/origin.json").is_file());
    let fresh = Store::open(&fresh_dir, StoreOptions::default()).unwrap();
    assert_eq!(fresh.dataset_id(), fresh_report.dataset_id);
    assert_eq!(fresh.head_commit(), store.head_commit());
    // Head resolves to the fresh destination's own genesis generation while
    // retaining the imported source data binding as provenance.
    let fresh_head = fresh
        .evidence_view(&sparkles_core::history::At::Head, &preparation)
        .unwrap();
    assert_eq!(fresh_head.configuration().dataset_id(), fresh.dataset_id());
    assert_ne!(fresh_head.configuration(), source.configuration());
    assert_eq!(
        fresh_head.original_data_binding(),
        sparkles_core::history::ConfigurationBinding::ImportedUnbound {
            source: source.configuration(),
        }
    );
    // Old explicit imported revisions remain unavailable as native evidence of
    // the new identity.
    assert!(fresh.evidence_view(&at, &preparation).is_err());
    // Later own commits bind natively to the fresh destination's lineage.
    upd(&fresh, "INSERT DATA { <urn:fresh-own> <urn:p> 2 }");
    let own_seq = fresh.head_commit().seq;
    let own_view = fresh
        .evidence_view(&sparkles_core::history::At::Commit(own_seq), &preparation)
        .unwrap();
    assert_eq!(own_view.configuration(), fresh_head.configuration());
}

#[tokio::test]
async fn configuration_history_create_refuses_a_generation_changed_after_capture() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source");
    let store = configuration_store(&root);
    let capture = store.backup_capture("changed").unwrap();
    let generations = std::fs::read_dir(root.join("evidence-history"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("generations");
    let generation = std::fs::read_dir(&generations)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // Same length, in place: the retained descriptor reads the changed bytes.
    let mut bytes = std::fs::read(&generation).unwrap();
    let last = bytes.len() - 2;
    bytes[last] = if bytes[last] == b' ' { b'\t' } else { b' ' };
    std::fs::write(&generation, &bytes).unwrap();
    let (repo, _) = memory_repo().await;
    assert!(
        repo.create(capture.into(), &opts("changed", "ds"))
            .await
            .is_err()
    );
    assert!(repo.manifest("changed").await.is_err());
}

#[tokio::test]
async fn configuration_history_restore_cancellation_removes_staged_parents() {
    let temporary = tempfile::tempdir().unwrap();
    let store = configuration_store(&temporary.path().join("source"));
    let (repo, _) = memory_repo().await;
    repo.create(
        store.backup_capture("cancelled").unwrap().into(),
        &opts("cancelled", "ds"),
    )
    .await
    .unwrap();
    let destination = temporary.path().join("cancelled");
    let ctl = sparkles_backup::Ctl::default();
    let cancel = ctl.cancel.clone();
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = cancelled.clone();
    let staged = destination.clone();
    let error = repo
        .restore(
            "cancelled",
            &destination,
            &sparkles_backup::RestoreOptions {
                check: sparkles_backup::CheckLevel::None,
                ctl: sparkles_backup::Ctl {
                    progress: Some(std::sync::Arc::new(move |_, message: &str| {
                        if message.starts_with("downloading ") {
                            assert!(staged.join("evidence-history").is_dir());
                            observed.store(true, std::sync::atomic::Ordering::SeqCst);
                            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    })),
                    ..ctl
                },
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), sparkles_backup::Code::Cancelled);
    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!destination.exists());
    assert!(repo.locks().await.unwrap().is_empty());
}

#[tokio::test]
async fn configuration_history_missing_closure_never_publishes_a_restore() {
    let temporary = tempfile::tempdir().unwrap();
    let store = configuration_store(&temporary.path().join("source"));
    let mut capture = store.backup_capture("missing").unwrap();
    capture
        .files
        .retain(|file| !file.path.contains("/generations/"));
    let (repo, _) = memory_repo().await;
    repo.create(capture.into(), &opts("missing", "ds"))
        .await
        .unwrap();
    let destination = temporary.path().join("restored");
    assert!(
        repo.restore(
            "missing",
            &destination,
            &RestoreOptions {
                check: sparkles_backup::CheckLevel::None,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert!(!destination.exists());
}

fn enable_evidence_with(store: &Store, source_graph: &str, proposal_graph: &str) {
    let settings = sparkles_core::evidence::Settings {
        enabled: true,
        source_records: vec![source_graph.into()],
        proposals: vec![proposal_graph.into()],
        ..Default::default()
    };
    let canonical = settings
        .canonical(&sparkles_core::task::Control::none())
        .unwrap();
    store
        .enable_managed(
            sparkles_core::managed::OwnerKind::Evidence,
            sparkles_core::managed::OwnerEntry::for_settings(
                1,
                settings.managed_graphs(),
                canonical.bytes(),
            ),
            canonical.bytes(),
        )
        .unwrap();
}

#[tokio::test]
async fn new_identity_restore_supports_compaction_repeated_reidentification_and_clone_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = configuration_store(&src_root);
    let prep =
        sparkles_core::access::Preparation::new(sparkles_core::task::Control::none(), 64 << 20)
            .unwrap();
    enable_evidence_with(&src, "urn:src:a1", "urn:prop:a1");
    upd(&src, "INSERT DATA { <urn:a2> <urn:p> 2 }");
    let seq_a2 = src.head_commit().seq;
    enable_evidence_with(&src, "urn:src:a2", "urn:prop:a2");
    upd(&src, "INSERT DATA { <urn:a3> <urn:p> 3 }");
    let seq_a3 = src.head_commit().seq;
    let src_head_cfg = src
        .evidence_view(&sparkles_core::history::At::Head, &prep)
        .unwrap()
        .configuration();

    let (repo, _) = memory_repo().await;
    repo.create(
        src.backup_capture("b-src").unwrap().into(),
        &opts("b-src", "ds"),
    )
    .await
    .unwrap();

    // Restore with Identity::Auto when id_in_use is true -> new identity.
    let dst_b_root = tmp.path().join("dst-b");
    let src_id = src.dataset_id();
    let rep_b = repo
        .restore(
            "b-src",
            &dst_b_root,
            &RestoreOptions {
                identity: sparkles_backup::Identity::Auto,
                id_in_use: std::sync::Arc::new(move |id| id == src_id),
                check: sparkles_backup::CheckLevel::Full,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rep_b.identity, "new");
    assert_ne!(rep_b.dataset_id, src_id);

    // Compact BEFORE any own writes (at seq_a3) -> base carries ImportedUnbound; reopen & check pass.
    {
        let b = Store::open(&dst_b_root, StoreOptions::default()).unwrap();
        let b_head = b
            .evidence_view(&sparkles_core::history::At::Head, &prep)
            .unwrap();
        assert_eq!(b_head.configuration().dataset_id(), b.dataset_id());
        assert_eq!(
            b_head.original_data_binding(),
            sparkles_core::history::ConfigurationBinding::ImportedUnbound {
                source: src_head_cfg,
            }
        );
        assert!(
            b.evidence_view(&sparkles_core::history::At::Commit(seq_a2), &prep)
                .is_err()
        );
        assert!(
            b.evidence_view(&sparkles_core::history::At::Commit(seq_a3), &prep)
                .is_err()
        );
        b.compact().unwrap();
    }
    let check_b = sparkles_core::check::check(
        &dst_b_root,
        &sparkles_core::check::CheckOptions { quick: false },
    )
    .unwrap();
    assert_eq!(check_b.errors, 0, "{check_b:?}");
    {
        let b = Store::open(&dst_b_root, StoreOptions::default()).unwrap();
        let b_gen1 = b
            .evidence_view(&sparkles_core::history::At::Head, &prep)
            .unwrap()
            .configuration();
        upd(&b, "INSERT DATA { <urn:b-own1> <urn:p> 10 }");
        let seq_b1 = b.head_commit().seq;
        enable_evidence_with(&b, "urn:src:b2", "urn:prop:b2");
        let b_gen2 = b
            .evidence_view(&sparkles_core::history::At::Head, &prep)
            .unwrap()
            .configuration();
        upd(&b, "INSERT DATA { <urn:b-own2> <urn:p> 20 }");
        let seq_b2 = b.head_commit().seq;
        assert_eq!(
            b.evidence_view(&sparkles_core::history::At::Commit(seq_b1), &prep)
                .unwrap()
                .configuration(),
            b_gen1
        );
        assert_eq!(
            b.evidence_view(&sparkles_core::history::At::Commit(seq_b2), &prep)
                .unwrap()
                .configuration(),
            b_gen2
        );
    }

    // Repeated identity change WITHOUT compaction: restore A -> B2, write own suffix,
    // back up B2 (requires evidence-configuration-history/2), restore B2 -> C (new identity).
    let dst_b2_root = tmp.path().join("dst-b2");
    repo.restore(
        "b-src",
        &dst_b2_root,
        &RestoreOptions {
            identity: sparkles_backup::Identity::New,
            check: sparkles_backup::CheckLevel::Full,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let b2 = Store::open(&dst_b2_root, StoreOptions::default()).unwrap();
    upd(&b2, "INSERT DATA { <urn:b2-suffix> <urn:p> 30 }");
    let seq_b2_own = b2.head_commit().seq;
    repo.create(
        b2.backup_capture("b-b2").unwrap().into(),
        &opts("b-b2", "ds-b2"),
    )
    .await
    .unwrap();
    let m_b2 = repo.manifest("b-b2").await.unwrap();
    assert!(
        m_b2.requires
            .iter()
            .any(|f| f == "evidence-configuration-history/2"),
        "{:?}",
        m_b2.requires
    );

    // Keep-identity restore of B2 works.
    let dst_b2_kept = tmp.path().join("dst-b2-kept");
    repo.restore(
        "b-b2",
        &dst_b2_kept,
        &RestoreOptions {
            identity: sparkles_backup::Identity::Keep,
            check: sparkles_backup::CheckLevel::Full,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let kept_b2 = Store::open(&dst_b2_kept, StoreOptions::default()).unwrap();
    assert_eq!(kept_b2.dataset_id(), b2.dataset_id());
    assert_eq!(
        kept_b2
            .evidence_view(&sparkles_core::history::At::Commit(seq_b2_own), &prep)
            .unwrap()
            .configuration(),
        b2.evidence_view(&sparkles_core::history::At::Commit(seq_b2_own), &prep)
            .unwrap()
            .configuration()
    );

    // New-identity restore B2 -> C retains both prior sources and disjoint WAL intervals.
    let dst_c_root = tmp.path().join("dst-c");
    repo.restore(
        "b-b2",
        &dst_c_root,
        &RestoreOptions {
            identity: sparkles_backup::Identity::New,
            check: sparkles_backup::CheckLevel::Full,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let origin_c: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dst_c_root.join("evidence-history/origin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(origin_c["format"].as_u64(), Some(2));
    assert_eq!(origin_c["sources"].as_array().unwrap().len(), 2);
    assert_eq!(origin_c["ranges"].as_array().unwrap().len(), 2);
    let c = Store::open(&dst_c_root, StoreOptions::default()).unwrap();
    let c_head = c
        .evidence_view(&sparkles_core::history::At::Head, &prep)
        .unwrap();
    assert_eq!(c_head.configuration().dataset_id(), c.dataset_id());
    assert!(
        c.evidence_view(&sparkles_core::history::At::Commit(seq_b2_own), &prep)
            .is_err()
    );
    upd(&c, "INSERT DATA { <urn:c-own> <urn:p> 40 }");
    assert_eq!(
        c.evidence_view(
            &sparkles_core::history::At::Commit(c.head_commit().seq),
            &prep
        )
        .unwrap()
        .configuration(),
        c_head.configuration()
    );

    // Backup a clone at seq 0 and new-identity restore it (exercises creationCommit provenance).
    let clone_root = tmp.path().join("clone-a");
    src.clone_to(&clone_root, &Default::default()).unwrap();
    let cloned = Store::open(&clone_root, StoreOptions::default()).unwrap();
    repo.create(
        cloned.backup_capture("b-clone").unwrap().into(),
        &opts("b-clone", "ds-clone"),
    )
    .await
    .unwrap();
    let clone_restored_root = tmp.path().join("clone-restored-new");
    repo.restore(
        "b-clone",
        &clone_restored_root,
        &RestoreOptions {
            identity: sparkles_backup::Identity::New,
            check: sparkles_backup::CheckLevel::Full,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let clone_restored = Store::open(&clone_restored_root, StoreOptions::default()).unwrap();
    assert_eq!(
        clone_restored
            .evidence_view(&sparkles_core::history::At::Head, &prep)
            .unwrap()
            .configuration()
            .dataset_id(),
        clone_restored.dataset_id()
    );
}

#[tokio::test]
async fn new_identity_restore_refuses_tampered_imported_wal_and_preserves_paused_views() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = configuration_store(&src_root);
    // Add a running view definition to verify it comes back paused with reason "restored".
    let view_dir = src_root.join("views/v1");
    std::fs::create_dir_all(&view_dir).unwrap();
    std::fs::write(
        view_dir.join("view.json"),
        serde_json::json!({
            "format": 1,
            "query": "SELECT ?s WHERE { ?s <urn:p> ?o }",
            "principal": "urn:x-sparkles:principal:ops"
        })
        .to_string(),
    )
    .unwrap();

    let (repo, _) = memory_repo().await;
    repo.create(
        src.backup_capture("b-views").unwrap().into(),
        &opts("b-views", "ds"),
    )
    .await
    .unwrap();
    let restored = tmp.path().join("restored");
    repo.restore(
        "b-views",
        &restored,
        &RestoreOptions {
            identity: sparkles_backup::Identity::New,
            check: sparkles_backup::CheckLevel::Full,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Paused view invariant is preserved.
    let view_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(restored.join("views/v1/view.json")).unwrap())
            .unwrap();
    assert_eq!(view_json["paused"]["reason"], "restored");

    // Tamper: deleting the imported WAL is refused by both open and check.
    let current = std::fs::read_to_string(restored.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();
    let wal_path = restored.join(&current).join("wal.log");
    let wal_orig = std::fs::read(&wal_path).unwrap();
    std::fs::remove_file(&wal_path).unwrap();
    assert!(Store::open(&restored, StoreOptions::default()).is_err());
    assert!(
        sparkles_core::check::check(
            &restored,
            &sparkles_core::check::CheckOptions { quick: false }
        )
        .unwrap()
        .errors
            > 0
    );

    // Tamper: flipping a byte in the imported WAL interval is refused.
    let mut wal_bad = wal_orig.clone();
    *wal_bad.last_mut().unwrap() ^= 0x01;
    std::fs::write(&wal_path, &wal_bad).unwrap();
    assert!(Store::open(&restored, StoreOptions::default()).is_err());
    assert!(
        sparkles_core::check::check(
            &restored,
            &sparkles_core::check::CheckOptions { quick: false }
        )
        .unwrap()
        .errors
            > 0
    );

    // Restoring original WAL passes again.
    std::fs::write(&wal_path, &wal_orig).unwrap();
    assert_eq!(
        sparkles_core::check::check(
            &restored,
            &sparkles_core::check::CheckOptions { quick: false }
        )
        .unwrap()
        .errors,
        0
    );
    drop(Store::open(&restored, StoreOptions::default()).unwrap());
}

fn level(level: VerifyLevel) -> VerifyOptions {
    VerifyOptions {
        level,
        ..Default::default()
    }
}

fn dump(dir: &std::path::Path) -> (u64, Vec<String>) {
    let s = Store::open(dir, StoreOptions::default()).unwrap();
    let mut buf = Vec::new();
    s.dump_nquads(&mut buf).unwrap();
    let mut lines: Vec<String> = String::from_utf8(buf)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    (s.head_commit().seq, lines)
}

/// Verification levels on an `fs` repository: a missing blob, then a damaged one.
#[tokio::test]
async fn verify_levels() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let dir = tmp.path().join("repo");
    let cfg = RepoConfig::from_url("local", &format!("file://{}", dir.display())).unwrap();
    let repo = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    repo.create(closed_source(&db), &opts("b2", "ds"))
        .await
        .unwrap();
    let b1 = vec!["b1".to_string()];
    for l in [VerifyLevel::Exists, VerifyLevel::Data] {
        let v = repo.verify(&b1, &level(l)).await.unwrap();
        assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
        assert_eq!(v.level, l);
    }
    let v = repo.verify(&[], &level(VerifyLevel::Data)).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
    assert_eq!(v.backups.len(), 2);
    assert_eq!(v.orphans.unwrap().blobs, 0);

    // delete the blob of gen-0001/spo.dat, which both backups share
    let m = repo.manifest("b1").await.unwrap();
    let id = m
        .files
        .iter()
        .find(|f| f.path == "gen-0001/spo.dat")
        .unwrap()
        .blobs[0]
        .id
        .clone();
    let file = dir.join(format!("blobs/{}/{id}", &id[..2]));
    let saved = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    let v = repo.verify(&b1, &level(VerifyLevel::Exists)).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Error);
    assert_eq!(v.backups[0].missing, std::slice::from_ref(&id));
    assert!(v.backups[0].corrupt.is_empty());
    assert!(
        v.requests.head > 0 && v.requests.list == 0,
        "{:?}",
        v.requests
    );
    let v = repo.verify(&[], &level(VerifyLevel::Exists)).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Error);
    for b in &v.backups {
        assert_eq!(b.status, VerifyStatus::Error, "{}", b.name);
        assert_eq!(b.missing, std::slice::from_ref(&id));
    }
    assert_eq!(v.requests.list, 2);
    assert_eq!(v.requests.head, 0);
    let v = repo.verify(&b1, &level(VerifyLevel::Data)).await.unwrap();
    assert_eq!(v.backups[0].missing, std::slice::from_ref(&id));

    // put it back with one payload byte flipped: present, but corrupt
    let mut bad = saved.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    std::fs::write(&file, &bad).unwrap();
    let v = repo.verify(&b1, &level(VerifyLevel::Exists)).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
    let v = repo.verify(&b1, &level(VerifyLevel::Data)).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Error);
    assert_eq!(v.backups[0].corrupt, std::slice::from_ref(&id));
    assert!(v.backups[0].missing.is_empty());
    assert!(v.requests.get > 5, "{:?}", v.requests);
    // a truncated one is missing already at the exists level
    std::fs::write(&file, &saved[..saved.len() - 1]).unwrap();
    let v = repo.verify(&b1, &level(VerifyLevel::Exists)).await.unwrap();
    assert_eq!(v.backups[0].missing, std::slice::from_ref(&id));
    std::fs::write(&file, &saved).unwrap();
    assert_eq!(
        repo.verify(&[], &level(VerifyLevel::Data))
            .await
            .unwrap()
            .status,
        VerifyStatus::Ok
    );

    // unknown names
    let e = repo
        .verify(&["nope".to_string()], &VerifyOptions::default())
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::NoSuchBackup);
}

/// Orphans and unreadable manifests in a repository verify.
#[tokio::test]
async fn orphans_and_bad_manifests() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, mem) = memory_repo().await;
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    let stray = sparkles_backup::blob::encode(b"nobody's", false);
    mem.put(
        &sparkles_backup::layout::blob_key(&stray.id),
        PutPayload::from(stray.bytes.clone()),
    )
    .await
    .unwrap();
    let v = repo.verify(&[], &VerifyOptions::default()).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Warning);
    let o = v.orphans.unwrap();
    assert_eq!((o.blobs, o.bytes), (1, stray.bytes.len() as u64));
    // a manifest that does not parse, and one stored under another name
    mem.put(
        &Key::from("backups/junk.json"),
        PutPayload::from_static(b"{"),
    )
    .await
    .unwrap();
    let b1 = mem
        .get(&Key::from("backups/b1.json"))
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    mem.put(&Key::from("backups/copy.json"), PutPayload::from(b1))
        .await
        .unwrap();
    let v = repo.verify(&[], &VerifyOptions::default()).await.unwrap();
    assert_eq!(v.status, VerifyStatus::Error);
    let names: Vec<(&str, VerifyStatus)> = v
        .backups
        .iter()
        .map(|b| (b.name.as_str(), b.status))
        .collect();
    assert_eq!(
        names,
        [
            ("b1", VerifyStatus::Ok),
            ("copy", VerifyStatus::Error),
            ("junk", VerifyStatus::Error)
        ]
    );
    assert!(v.backups[2].check.as_ref().unwrap()["error"].is_string());
    // listing skips both
    let l = repo
        .list(&sparkles_backup::ListFilter::default())
        .await
        .unwrap();
    assert_eq!(l.len(), 1);
}

/// What `create` writes restores to the database it read (first, incremental, and after a delete).
#[tokio::test]
async fn backups_restore_to_their_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, _) = memory_repo().await;
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    let at3 = dump(&db);
    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    repo.create(closed_source(&db), &opts("b2", "ds"))
        .await
        .unwrap();
    let at4 = dump(&db);
    assert_eq!(at3.0, 3);
    assert_eq!(at4.0, 4);
    repo.delete("b1").await.unwrap();
    let r = repo
        .restore("b2", &tmp.path().join("r2"), &RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(r.identity, "kept");
    assert_eq!(dump(&tmp.path().join("r2")), at4);
    // b2 still restores with its parent gone, also at the restore level of verify
    let v = repo
        .verify(
            &["b2".to_string()],
            &VerifyOptions {
                level: VerifyLevel::Restore,
                tmp_dir: Some(tmp.path().join("vt")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
    assert_eq!(v.backups[0].check.as_ref().unwrap()["status"], "ok");
}

/// The same on an `fs` repository, with a WAL of several segments.
#[tokio::test]
async fn fs_backups_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let dir = tmp.path().join("repo");
    let cfg = RepoConfig::from_url("local", &format!("file://{}", dir.display())).unwrap();
    let repo = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
    for i in 0..4 {
        repo.create(closed_source(&db), &opts(&format!("b{i}"), "ds"))
            .await
            .unwrap();
        commit(&db, &format!("INSERT DATA {{ <urn:x{i}> <urn:p> {i} }}"));
    }
    let wal = repo.manifest("b3").await.unwrap();
    let segments = wal
        .files
        .iter()
        .find(|f| f.path == "gen-0001/wal.log")
        .unwrap()
        .blobs
        .len();
    assert_eq!(segments, 4);
    let expected = {
        // the state of b3: the database before the last commit
        let r = tmp.path().join("r3");
        repo.restore("b3", &r, &RestoreOptions::default())
            .await
            .unwrap();
        dump(&r)
    };
    assert_eq!(expected.0, 6);
    assert_eq!(expected.1.len(), 4, "{:?}", expected.1);
}

/// The dataset's DESCRIBE setting and its reading of errors in aggregates are part of a
/// backup, from a live store and from a closed directory alike, and a restore keeps them.
#[tokio::test]
async fn describe_and_aggregate_errors_settings_round_trip() {
    use sparkles_core::sparql::AggregateErrors;
    use sparkles_core::sparql::describe::{DescribeMode, DescribeOptions};
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let describe = DescribeOptions {
        mode: DescribeMode::Scbd,
        max_depth: Some(3),
        ..Default::default()
    };
    {
        let s = Store::open(&db, StoreOptions::default()).unwrap();
        s.set_describe_settings(Some(describe.clone())).unwrap();
        s.set_aggregate_errors_setting(Some(AggregateErrors::Spec))
            .unwrap();
    }
    let (repo, _) = memory_repo().await;
    let live = Store::open(&db, StoreOptions::default()).unwrap();
    repo.create(
        live.backup_capture("live").unwrap().into(),
        &opts("live", "ds"),
    )
    .await
    .unwrap();
    drop(live);
    repo.create(closed_source(&db), &opts("closed", "ds"))
        .await
        .unwrap();
    for name in ["live", "closed"] {
        let paths: Vec<String> = repo
            .manifest(name)
            .await
            .unwrap()
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        for p in ["describe.json", "aggregate-errors.json"] {
            assert!(paths.iter().any(|x| x == p), "{name}: {p} in {paths:?}");
        }
        let dest = tmp.path().join(format!("restored-{name}"));
        repo.restore(name, &dest, &RestoreOptions::default())
            .await
            .unwrap();
        let r = Store::open(&dest, StoreOptions::default()).unwrap();
        assert_eq!(
            r.aggregate_errors_setting(),
            Some(AggregateErrors::Spec),
            "{name}"
        );
        assert_eq!(r.describe_settings(), describe, "{name}");
    }
}
