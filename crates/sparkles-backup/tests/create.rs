//! Creating, listing and deleting backups (library level, `memory://` and `fs`).

mod common;

use common::*;
use sparkles_backup::{
    Code, FileKind, ListFilter, OpenEnv, RepoConfig, Repository, VerifyLevel, VerifyOptions,
    VerifyStatus,
};
use std::collections::HashMap;

const PERMS: [&str; 7] = ["spo", "sop", "pso", "pos", "osp", "ops", "gspo"];

fn blob_ids(m: &sparkles_backup::Manifest) -> HashMap<String, Vec<String>> {
    m.files
        .iter()
        .map(|f| {
            (
                f.path.clone(),
                f.blobs.iter().map(|b| b.id.clone()).collect(),
            )
        })
        .collect()
}

/// The first backup of a database holds its files, and uploads about everything.
#[tokio::test]
async fn first_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, _) = memory_repo().await;
    let src = closed_source(&db);
    let id = src.dataset_id;
    let s = repo.create(src, &opts("b1", "ds")).await.unwrap();
    assert_eq!(s.commit.seq, 3);
    assert_eq!(s.commit.reference, "commit:3");
    assert_eq!(s.dataset.id, id);
    assert_eq!(s.dataset.name, "ds");
    let m = repo.manifest("b1").await.unwrap();
    let paths: Vec<&str> = m.files.iter().map(|f| f.path.as_str()).collect();
    for p in ["CURRENT", "commits.bin", "dataset.json", "gen-0001/wal.log"] {
        assert!(paths.contains(&p), "{p} in {paths:?}");
    }
    for perm in PERMS {
        for ext in ["dat", "meta"] {
            let p = format!("gen-0001/{perm}.{ext}");
            assert!(paths.contains(&p.as_str()), "{p} in {paths:?}");
        }
    }
    assert!(
        !paths
            .iter()
            .any(|p| p.contains("sparkles.lock") || p.starts_with("text/"))
    );
    let kind = |p: &str| m.files.iter().find(|f| f.path == p).unwrap().kind;
    assert_eq!(kind("gen-0001/wal.log"), FileKind::Append);
    assert_eq!(kind("commits.bin"), FileKind::Append);
    assert_eq!(kind("gen-0001/spo.dat"), FileKind::Immutable);
    assert_eq!(kind("CURRENT"), FileKind::Meta);
    assert_eq!(m.generation, "gen-0001");
    assert_eq!(m.repository_id, repo.id());
    assert_eq!(m.parent, None);
    // every file's hash is that of its bytes on disk (prefixes.json is rendered from
    // the store's prefixes, not read)
    for f in m.files.iter().filter(|f| f.path != "prefixes.json") {
        let bytes = std::fs::read(db.join(&f.path)).unwrap();
        assert_eq!(f.size, bytes.len() as u64, "{}", f.path);
        assert_eq!(
            f.sha256,
            sparkles_backup::blob::blob_id(&bytes),
            "{}",
            f.path
        );
        assert_eq!(f.blobs.iter().map(|b| b.size).sum::<u64>(), f.size);
    }
    // a fresh repository: nearly everything is new (a few small files repeat content)
    assert!(m.stats.new_blobs > 0);
    assert!(m.stats.added_bytes > 0);
    assert!(
        m.stats.added_bytes as f64 >= m.stats.logical_bytes as f64 * 0.1,
        "{:?}",
        m.stats
    );
    assert_eq!(
        m.stats.logical_bytes,
        m.files.iter().map(|f| f.size).sum::<u64>()
    );
    assert_eq!(m.stats.blobs, m.stats.new_blobs + m.stats.reused_blobs);
    let listed = repo.list(&ListFilter::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], s);
    assert_eq!(repo.requests().errors(), 0);
}

/// An incremental backup reuses every immutable blob and the WAL's segments.
#[tokio::test]
async fn incremental_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, _) = memory_repo().await;
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    let s2 = repo
        .create(closed_source(&db), &opts("b2", "ds"))
        .await
        .unwrap();
    assert_eq!(s2.commit.seq, 4);
    let m1 = repo.manifest("b1").await.unwrap();
    let m2 = repo.manifest("b2").await.unwrap();
    assert_eq!(m2.parent.as_deref(), Some("b1"));
    let (b1, b2) = (blob_ids(&m1), blob_ids(&m2));
    for f in m2.files.iter().filter(|f| f.kind == FileKind::Immutable) {
        assert_eq!(b2[&f.path], b1[&f.path], "{}", f.path);
    }
    let wal1 = &b1["gen-0001/wal.log"];
    let wal2 = &b2["gen-0001/wal.log"];
    assert_eq!(wal2.len(), wal1.len() + 1, "{wal1:?} {wal2:?}");
    assert_eq!(&wal2[..wal1.len()], &wal1[..]);
    assert!(m2.stats.new_blobs <= 5, "{:?}", m2.stats);
    assert!(m2.stats.added_bytes < 10_000, "{:?}", m2.stats);
    assert!(m2.stats.reused_blobs > 20, "{:?}", m2.stats);
    // newest first
    let names: Vec<String> = repo
        .list(&ListFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["b2", "b1"]);
    // the files hash to what is on disk now
    for f in m2.files.iter().filter(|f| f.path != "prefixes.json") {
        let bytes = std::fs::read(db.join(&f.path)).unwrap();
        assert_eq!(
            f.sha256,
            sparkles_backup::blob::blob_id(&bytes),
            "{}",
            f.path
        );
    }
}

/// Deleting a backup removes its manifest only; the name can be used again.
#[tokio::test]
async fn delete_keeps_blobs_and_frees_the_name() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, mem) = memory_repo().await;
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    repo.create(closed_source(&db), &opts("b2", "ds"))
        .await
        .unwrap();
    let blobs = count(mem.as_ref(), "blobs").await;
    let old_id = repo.manifest("b1").await.unwrap().id;
    assert!(repo.delete("b1").await.unwrap());
    assert!(!repo.delete("b1").await.unwrap());
    assert!(!repo.delete("../x").await.unwrap());
    let names: Vec<String> = repo
        .list(&ListFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["b2"]);
    assert_eq!(
        repo.manifest("b1").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    assert_eq!(count(mem.as_ref(), "blobs").await, blobs);
    let v = repo
        .verify(
            &["b2".to_string()],
            &VerifyOptions {
                level: VerifyLevel::Data,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
    // the name again: a new manifest, which listing and reading pick up
    commit(&db, "INSERT DATA { <urn:d> <urn:p> 4 }");
    let s = repo
        .create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    assert_eq!(s.commit.seq, 5);
    let m = repo.manifest("b1").await.unwrap();
    assert_ne!(m.id, old_id);
    assert_eq!(m.parent.as_deref(), Some("b2"));
    let listed = repo.list(&ListFilter::default()).await.unwrap();
    assert_eq!(listed[0].name, "b1");
    assert_eq!(listed[0].commit.seq, 5);
    // another handle (cold cache) sees the same
    let other = open_on(mem.clone(), &memory_config("mem2")).await;
    assert_eq!(other.id(), repo.id());
    let listed2 = other.list(&ListFilter::default()).await.unwrap();
    assert_eq!(listed2.len(), 2);
    assert_eq!(listed2[0].commit.seq, 5);
    assert_eq!(listed2[0].repository, "mem2");
}

/// The manifest cache on disk: a second handle with the same cache directory lists
/// without fetching manifests; a re-created name is fetched again.
#[tokio::test]
async fn warm_listing_uses_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let mem: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    let env = OpenEnv {
        store: Some(mem.clone()),
        cache_dir: Some(tmp.path().join("cache")),
        ..Default::default()
    };
    let cfg = memory_config("mem");
    let repo = Repository::open(&cfg, &env).await.unwrap();
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    assert!(
        tmp.path()
            .join(format!("cache/{}/b1.json", repo.id()))
            .exists()
    );
    let again = Repository::open(&cfg, &env).await.unwrap();
    assert_eq!(again.list(&ListFilter::default()).await.unwrap().len(), 1);
    assert_eq!(again.requests().get.ok, 1, "only the marker is read");
    // delete and re-create through the first handle: the second refetches
    repo.delete("b1").await.unwrap();
    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    // (the new entry is in the shared cache directory already)
    let l = again.list(&ListFilter::default()).await.unwrap();
    assert_eq!(l[0].commit.seq, 4);
    assert_eq!(again.requests().get.ok, 1);
    // without that cache, the new e_tag makes the stale entry a miss
    std::fs::remove_dir_all(tmp.path().join("cache")).unwrap();
    let fresh = Repository::open(&cfg, &env).await.unwrap();
    assert_eq!(
        fresh.list(&ListFilter::default()).await.unwrap()[0]
            .commit
            .seq,
        4
    );
    assert_eq!(fresh.requests().get.ok, 2, "the marker and the manifest");
}

/// Listing filters and paging.
#[tokio::test]
async fn list_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    make_db(&a);
    make_db(&b);
    let (repo, _) = memory_repo().await;
    let mut o = opts("a1", "a");
    o.policy = Some(("nightly".into(), "run-1".into()));
    o.note = Some("first".into());
    repo.create(closed_source(&a), &o).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let b_id = closed_source(&b).dataset_id;
    repo.create(closed_source(&b), &opts("b1", "b"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    commit(&a, "INSERT DATA { <urn:c> <urn:p> 3 }");
    repo.create(closed_source(&a), &opts("a2", "a"))
        .await
        .unwrap();
    let names = |l: Vec<sparkles_backup::BackupSummary>| -> Vec<String> {
        l.into_iter().map(|s| s.name).collect()
    };
    let all = repo.list(&ListFilter::default()).await.unwrap();
    assert_eq!(names(all.clone()), ["a2", "b1", "a1"]);
    let f = |f: ListFilter| {
        let repo = &repo;
        async move { names(repo.list(&f).await.unwrap()) }
    };
    assert_eq!(
        f(ListFilter {
            dataset: Some("a".into()),
            ..Default::default()
        })
        .await,
        ["a2", "a1"]
    );
    assert_eq!(
        f(ListFilter {
            dataset_id: Some(b_id),
            ..Default::default()
        })
        .await,
        ["b1"]
    );
    assert_eq!(
        f(ListFilter {
            policy: Some("nightly".into()),
            ..Default::default()
        })
        .await,
        ["a1"]
    );
    assert_eq!(
        f(ListFilter {
            limit: Some(2),
            ..Default::default()
        })
        .await,
        ["a2", "b1"]
    );
    // the next page: before the last one shown
    assert_eq!(
        f(ListFilter {
            limit: Some(2),
            before: Some(all[1].completed.clone()),
            ..Default::default()
        })
        .await,
        ["a1"]
    );
    let a1 = &all[2];
    assert_eq!(a1.policy.as_deref(), Some("nightly"));
    assert_eq!(a1.run.as_deref(), Some("run-1"));
    assert_eq!(a1.note.as_deref(), Some("first"));
    let bad = repo
        .list(&ListFilter {
            before: Some("yesterday".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code(), Code::InvalidRequest);
    // a2's parent is a1, not the other dataset's b1
    assert_eq!(
        repo.manifest("a2").await.unwrap().parent.as_deref(),
        Some("a1")
    );
    let st = repo.stats().await.unwrap();
    assert_eq!(st.backups, 3);
    assert_eq!(st.datasets, 2);
    assert!(st.stored_bytes > 0 && st.dedup_ratio > 1.0, "{st:?}");
}

/// Names: taken ones, invalid ones, and extra meta files.
#[tokio::test]
async fn names_and_extra_files() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, _) = memory_repo().await;
    let mut o = opts("b1", "ds");
    o.extra = vec![("reasoning.json".into(), br#"{"commit":3}"#.to_vec())];
    repo.create(closed_source(&db), &o).await.unwrap();
    let m = repo.manifest("b1").await.unwrap();
    let r = m.files.iter().find(|f| f.path == "reasoning.json").unwrap();
    assert_eq!(r.kind, FileKind::Meta);
    assert_eq!(r.size, 12);
    let e = repo
        .create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::BackupExists);
    assert_eq!(e.http_status(), 409);
    let e = repo
        .create(closed_source(&db), &opts("../b", "ds"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidName);
    let mut o = opts("b2", "ds");
    o.extra = vec![("../../etc/passwd".into(), vec![])];
    let e = repo.create(closed_source(&db), &o).await.unwrap_err();
    assert_eq!(e.code(), Code::InvalidRequest);
    assert_eq!(repo.list(&ListFilter::default()).await.unwrap().len(), 1);
}

/// Append-only files grow by one segment per backup, and are stored from scratch once
/// they would have more than 64.
#[tokio::test]
async fn segments_are_consolidated() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, _) = memory_repo().await;
    let wal = |m: &sparkles_backup::Manifest| {
        m.files
            .iter()
            .find(|f| f.path == "gen-0001/wal.log")
            .unwrap()
            .blobs
            .len()
    };
    let mut prev = 0;
    let mut consolidated = false;
    for i in 0..66 {
        let name = format!("b{i}");
        repo.create(closed_source(&db), &opts(&name, "ds"))
            .await
            .unwrap();
        let n = wal(&repo.manifest(&name).await.unwrap());
        assert!(n <= 64, "{name}: {n} segments");
        if i > 0 && n < prev {
            consolidated = true;
            assert_eq!(n, 1, "{name}");
        } else if i > 0 {
            assert_eq!(n, prev + 1, "{name}");
        }
        prev = n;
        commit(&db, &format!("INSERT DATA {{ <urn:x{i}> <urn:p> {i} }}"));
    }
    assert!(consolidated);
}

/// A changed prefix of an append-only file (here: a WAL rewritten with other content
/// of at least the parent's length) is stored from scratch.
#[tokio::test]
async fn a_changed_prefix_is_stored_from_scratch() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, _) = memory_repo().await;
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    let mut src = closed_source(&db);
    let wal = src
        .files
        .iter_mut()
        .find(|f| f.path == "gen-0001/wal.log")
        .unwrap();
    let mut bytes = std::fs::read(db.join("gen-0001/wal.log")).unwrap();
    bytes[0] ^= 0xff;
    bytes.extend_from_slice(b"more");
    wal.len = bytes.len() as u64;
    wal.src = sparkles_core::store::FileSource::Bytes(std::sync::Arc::from(bytes.as_slice()));
    repo.create(src, &opts("b2", "ds")).await.unwrap();
    let m = repo.manifest("b2").await.unwrap();
    let f = m
        .files
        .iter()
        .find(|f| f.path == "gen-0001/wal.log")
        .unwrap();
    assert_eq!(f.blobs.len(), 1);
    assert_eq!(f.blobs[0].id, sparkles_backup::blob::blob_id(&bytes));
    assert_eq!(f.sha256, f.blobs[0].id);
}

/// Read-only repositories refuse to write.
#[tokio::test]
async fn read_only_repositories_do_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let (repo, mem) = memory_repo().await;
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    let mut cfg = memory_config("ro");
    cfg.readonly = true;
    let ro = open_on(mem.clone(), &cfg).await;
    let objects = count(mem.as_ref(), "").await;
    assert_eq!(ro.list(&ListFilter::default()).await.unwrap().len(), 1);
    let e = ro
        .create(closed_source(&db), &opts("b2", "ds"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::RepositoryReadOnly);
    assert_eq!(
        ro.delete("b1").await.unwrap_err().code(),
        Code::RepositoryReadOnly
    );
    let t = ro.test().await.unwrap();
    assert!(t.ok);
    assert_eq!(t.steps.len(), 1);
    assert_eq!(
        ro.verify(&[], &VerifyOptions::default())
            .await
            .unwrap()
            .status,
        VerifyStatus::Ok
    );
    assert_eq!(count(mem.as_ref(), "").await, objects);
}

/// `fs` repositories: created with their directory, blobs as files.
#[tokio::test]
async fn fs_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("ds");
    make_db(&db);
    let dir = tmp.path().join("repo");
    let url = format!("file://{}", dir.display());
    let cfg = RepoConfig::from_url("local", &url).unwrap();
    let repo = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("sparkles-repo.json")).unwrap()).unwrap();
    assert_eq!(marker["format"], 1);
    assert_eq!(marker["id"], repo.id().to_string());
    let t = repo.test().await.unwrap();
    assert!(t.ok && t.conditional_writes, "{t:?}");
    assert!(!repo.single_writer());
    repo.create(closed_source(&db), &opts("b1", "ds"))
        .await
        .unwrap();
    assert!(dir.join("backups/b1.json").exists());
    // a disk reserve the repository's file system cannot keep: 507, no manifest
    let mut o = opts("b2", "ds");
    o.min_free_disk_bytes = Some(u64::MAX / 2);
    let e = repo.create(closed_source(&db), &o).await.unwrap_err();
    assert_eq!(
        (e.code(), e.http_status()),
        (Code::InsufficientStorage, 507)
    );
    assert!(e.message().contains("--min-free-disk-mb"), "{e}");
    assert!(!dir.join("backups/b2.json").exists());
    let m = repo.manifest("b1").await.unwrap();
    let spo = &m
        .files
        .iter()
        .find(|f| f.path == "gen-0001/spo.dat")
        .unwrap()
        .blobs[0]
        .id;
    assert!(dir.join(format!("blobs/{}/{spo}", &spo[..2])).exists());
    // attach again (a second registration of the same location)
    let again = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
    assert_eq!(again.id(), repo.id());
    assert_eq!(again.list(&ListFilter::default()).await.unwrap().len(), 1);
    // inside the server's data directory: refused
    let e = Repository::open(
        &cfg,
        &OpenEnv {
            forbid_under: vec![tmp.path().to_path_buf()],
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), Code::InvalidConfig);
}
