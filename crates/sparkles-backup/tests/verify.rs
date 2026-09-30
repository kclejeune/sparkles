//! Verifying backups at every level, and restoring what `create` wrote.

mod common;

use common::*;
use object_store::path::Path as Key;
use object_store::{ObjectStoreExt, PutPayload};
use sparkles::store::{Store, StoreOptions};
use sparkles_backup::{
    Code, OpenEnv, RepoConfig, Repository, RestoreOptions, VerifyLevel, VerifyOptions, VerifyStatus,
};

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
