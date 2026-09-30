use super::*;
use crate::fixture;
use crate::layout::{self, lock_key, manifest_key};
use crate::{Ctl, LockHolder, LockObject, RestoreOptions};
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

/// Backups `b1` (commit 3) and `b2` (commit 4) of `gen-0001`, a compaction, then `b4`
/// (commit 5, `gen-0002`), and `b1` and `b2` deleted. Returns the manifests.
async fn history(repo: &Repository, src: &Path) -> (Manifest, Manifest, Manifest) {
    history_with_copy(repo, src, None).await
}

/// [`history`], copying the database as it was at `b2` to `copy`.
async fn history_with_copy(
    repo: &Repository,
    src: &Path,
    copy: Option<&Path>,
) -> (Manifest, Manifest, Manifest) {
    fixture::make_db(src);
    let b1 = fixture::put_backup(repo, src, "b1").await;
    {
        let s = Store::open(src, StoreOptions::default()).unwrap();
        fixture::upd(&s, "INSERT DATA { <urn:c> <urn:p> 3 }");
    }
    let b2 = fixture::put_backup(repo, src, "b2").await;
    if let Some(to) = copy {
        copy_dir(src, to);
    }
    {
        let s = Store::open(src, StoreOptions::default()).unwrap();
        s.compact().unwrap();
        fixture::upd(&s, "INSERT DATA { <urn:d> <urn:p> 4 }");
    }
    let b4 = fixture::put_backup(repo, src, "b4").await;
    assert_eq!(
        (b1.generation.as_str(), b4.generation.as_str()),
        ("gen-0001", "gen-0002")
    );
    for n in ["b1", "b2"] {
        repo.store.delete(&manifest_key(n)).await.unwrap();
    }
    (b1, b2, b4)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

async fn blob_ids(repo: &Repository) -> BTreeSet<String> {
    list_all(repo.store.as_ref(), BLOBS)
        .await
        .unwrap()
        .iter()
        .filter_map(|m| blob_id_of(&m.location).map(str::to_string))
        .collect()
}

async fn stored_bytes(repo: &Repository) -> u64 {
    list_all(repo.store.as_ref(), BLOBS)
        .await
        .unwrap()
        .iter()
        .map(|m| m.size)
        .sum()
}

fn now_gc() -> GcOptions {
    GcOptions {
        grace: Duration::ZERO,
        ..Default::default()
    }
}

fn plant_lock() -> PutPayload {
    PutPayload::from(
        serde_json::to_vec(&LockObject {
            kind: LockKind::Shared,
            holder: LockHolder {
                host: "h".into(),
                pid: 1,
                server: String::new(),
                version: "0".into(),
            },
            operation: LockOperation::Create,
            created: "2026-09-30T00:00:00.000Z".into(),
        })
        .unwrap(),
    )
}

fn set_mtime(path: &Path, ago: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - ago)
        .unwrap();
}

#[tokio::test]
async fn gc_deletes_exactly_the_unreferenced_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture::fs_repo(&dir.path().join("repo")).await;
    let (b1, b2, b4) = history(&repo, &dir.path().join("src")).await;
    let keep = fixture::blob_ids(&b4);
    let gone: BTreeSet<String> = fixture::blob_ids(&b1)
        .union(&fixture::blob_ids(&b2))
        .filter(|id| !keep.contains(*id))
        .cloned()
        .collect();
    assert!(!gone.is_empty());
    let before = stored_bytes(&repo).await;
    assert_eq!(blob_ids(&repo).await, &keep | &gone);

    // leftovers: a stale lock, a fresh lock, an old probe
    let root = dir.path().join("repo");
    repo.store
        .put(&lock_key("stale"), plant_lock())
        .await
        .unwrap();
    set_mtime(&root.join("locks/stale.json"), Duration::from_secs(31 * 60));
    repo.store
        .put(&Key::from("probe/old"), PutPayload::from(&b"x"[..]))
        .await
        .unwrap();
    set_mtime(&root.join("probe/old"), Duration::from_secs(31 * 60));

    // the dry run reports what a real run deletes, and deletes nothing
    let dry = repo
        .gc(&GcOptions {
            dry_run: true,
            ..now_gc()
        })
        .await
        .unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.manifests, 1);
    assert_eq!(dry.referenced_blobs, keep.len() as u64);
    assert_eq!(dry.listed_blobs, (keep.len() + gone.len()) as u64);
    assert_eq!(
        (dry.candidates, dry.deleted),
        (gone.len() as u64, gone.len() as u64)
    );
    assert!(dry.deleted_bytes > 0);
    assert_eq!(dry.stored_bytes_after, before - dry.deleted_bytes);
    assert_eq!(dry.kept_young, 0);
    assert_eq!(blob_ids(&repo).await, &keep | &gone);
    assert!(repo.last_gc().await.unwrap().is_none());
    // only the planted stale lock: the dry run's own lock is released
    assert_eq!(repo.locks().await.unwrap().len(), 1);

    let real = repo.gc(&now_gc()).await.unwrap();
    assert!(!real.dry_run);
    assert_eq!(real.deleted, gone.len() as u64);
    assert_eq!(real.deleted_bytes, dry.deleted_bytes);
    assert_eq!(real.stored_bytes_after, before - real.deleted_bytes);
    assert_eq!(stored_bytes(&repo).await, real.stored_bytes_after);
    assert_eq!(blob_ids(&repo).await, keep);
    assert!(real.requests.list >= 4 && real.requests.delete >= 1);
    // the stale lock and the probe are gone, and GC's own locks too
    assert!(repo.locks().await.unwrap().is_empty());
    assert!(!root.join("probe/old").exists());
    let last = repo.last_gc().await.unwrap().unwrap();
    assert_eq!(last.report, real);

    // what is left restores
    repo.restore("b4", &dir.path().join("r"), &RestoreOptions::default())
        .await
        .unwrap();

    // a second run finds nothing
    let again = repo.gc(&now_gc()).await.unwrap();
    assert_eq!((again.candidates, again.deleted), (0, 0));
}

#[tokio::test]
async fn young_orphans_are_kept_for_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture::memory_repo().await;
    fixture::make_db(&dir.path().join("src"));
    fixture::put_backup(&repo, &dir.path().join("src"), "b1").await;
    let e = crate::blob::encode(b"an orphan of an interrupted backup", false);
    repo.store
        .put(&layout::blob_key(&e.id), PutPayload::from(e.bytes.clone()))
        .await
        .unwrap();
    let r = repo.gc(&GcOptions::default()).await.unwrap();
    assert!(r.kept_young >= 1);
    assert_eq!((r.candidates, r.deleted), (0, 0));
    assert!(blob_ids(&repo).await.contains(&e.id));
    // past the grace period it goes
    let r = repo.gc(&now_gc()).await.unwrap();
    assert_eq!(r.deleted, 1);
    assert!(!blob_ids(&repo).await.contains(&e.id));
}

#[tokio::test]
async fn a_backup_created_between_mark_and_sweep_keeps_its_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(fixture::memory_repo().await);
    let (b1, b2, b4) = history(&repo, &dir.path().join("src")).await;
    let keep = fixture::blob_ids(&b4);
    let x: BTreeSet<String> = fixture::blob_ids(&b1).difference(&keep).cloned().collect();
    assert!(!x.is_empty());

    // a concurrent backup whose blobs already exist (it found them with HEAD or a
    // conditional create) lands its manifest after the mark
    let r2 = repo.clone();
    let b5 = Manifest {
        name: "b5".into(),
        ..b1.clone()
    };
    let hook: Hook = Box::new(move || {
        Box::pin(async move {
            fixture::put_manifest(&r2, &b5).await;
        })
    });
    BETWEEN_MARK_AND_SWEEP
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(repo.id(), hook);

    let r = repo.gc(&now_gc()).await.unwrap();
    let after = blob_ids(&repo).await;
    assert!(x.is_subset(&after), "a blob of b5 was deleted");
    assert_eq!(r.manifests, 2);
    // only b2's own blobs went
    let b2_only: BTreeSet<String> = fixture::blob_ids(&b2)
        .into_iter()
        .filter(|id| !keep.contains(id) && !x.contains(id))
        .collect();
    assert_eq!(r.deleted, b2_only.len() as u64);
    assert!(after.is_disjoint(&b2_only));
    repo.restore("b5", &dir.path().join("r"), &RestoreOptions::default())
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs S2 (Source::from_closed_dir, Repository::create, Repository::verify)"]
async fn a_real_backup_created_between_mark_and_sweep_verifies() {
    use crate::{CreateOptions, Source, VerifyLevel, VerifyOptions, VerifyStatus};
    let dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(fixture::memory_repo().await);
    let copy = dir.path().join("at-b2");
    let (_, b2, b4) = history_with_copy(&repo, &dir.path().join("src"), Some(&copy)).await;
    // the concurrent backup is of the database as it was at b2: its gen-0001 files
    // are blobs the mark found unreferenced
    let x: BTreeSet<String> = fixture::blob_ids(&b2)
        .difference(&fixture::blob_ids(&b4))
        .cloned()
        .collect();
    let (r2, src2) = (repo.clone(), copy.clone());
    let hook: Hook = Box::new(move || {
        Box::pin(async move {
            let o = CreateOptions {
                name: "b5".into(),
                dataset_name: "ds".into(),
                ..Default::default()
            };
            r2.create(Source::from_closed_dir(&src2).unwrap(), &o)
                .await
                .unwrap();
        })
    });
    BETWEEN_MARK_AND_SWEEP
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(repo.id(), hook);
    repo.gc(&now_gc()).await.unwrap();
    let b5 = repo.manifest("b5").await.unwrap();
    let kept: BTreeSet<String> = fixture::blob_ids(&b5).intersection(&x).cloned().collect();
    assert!(!kept.is_empty() && kept.is_subset(&blob_ids(&repo).await));
    let v = repo
        .verify(
            &["b5".to_string()],
            &VerifyOptions {
                level: VerifyLevel::Data,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(v.status, VerifyStatus::Ok);
}

#[tokio::test]
async fn the_sweep_waits_for_shared_locks() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = fixture::memory_repo().await;
    history(&repo, &dir.path().join("src")).await;
    let before = blob_ids(&repo).await;
    repo.env.lock_wait = Duration::from_millis(500);
    let held = lock::acquire(
        &repo,
        LockKind::Shared,
        LockOperation::Create,
        &Ctl::default(),
    )
    .await
    .unwrap();
    let e = repo.gc(&now_gc()).await.unwrap_err();
    assert_eq!(e.code(), Code::RepositoryLocked);
    assert_eq!(blob_ids(&repo).await, before);
    // a dry run takes no exclusive lock
    let dry = repo
        .gc(&GcOptions {
            dry_run: true,
            ..now_gc()
        })
        .await
        .unwrap();
    assert!(dry.candidates > 0);
    held.release().await.unwrap();
    let r = repo.gc(&now_gc()).await.unwrap();
    assert_eq!(r.deleted, dry.candidates);
    assert!(r.lock_wait_millis < 500);
}

#[tokio::test]
async fn an_unreadable_manifest_stops_the_collection() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture::memory_repo().await;
    history(&repo, &dir.path().join("src")).await;
    let before = blob_ids(&repo).await;
    repo.store
        .put(&manifest_key("broken"), PutPayload::from(&b"{"[..]))
        .await
        .unwrap();
    let e = repo.gc(&now_gc()).await.unwrap_err();
    assert_eq!(e.code(), Code::InvalidBackup);
    assert_eq!(e.body()["backup"], "broken");
    assert_eq!(blob_ids(&repo).await, before);
    assert!(repo.locks().await.unwrap().is_empty());
}

#[tokio::test]
async fn read_only_repositories_are_not_collected() {
    let repo = fixture::repo_with(fixture::memory_store(), |c| c.readonly = true).await;
    assert_eq!(
        repo.gc(&GcOptions::default()).await.unwrap_err().code(),
        Code::RepositoryReadOnly
    );
}

#[test]
fn ages() {
    let now = Utc::now();
    let g = Duration::from_secs(3600);
    assert!(older_than(now - chrono::Duration::hours(2), now, g));
    assert!(!older_than(now - chrono::Duration::minutes(59), now, g));
    assert!(older_than(now, now, Duration::ZERO));
    // written after the reference instant (another clock's rounding): young
    assert!(!older_than(
        now + chrono::Duration::seconds(1),
        now,
        Duration::ZERO
    ));
    assert_eq!(list_requests(0), 1);
    assert_eq!(list_requests(2500), 3);
}
