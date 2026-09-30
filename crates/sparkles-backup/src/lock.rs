//! Repository locks: lease objects `locks/<uuid>.json` ([`LockObject`]).
//!
//! * **shared** (create, delete, restore, verify): `PUT` the own lock
//!   (`PutMode::Create`), then `LIST locks/`; a non-stale exclusive lock means: delete
//!   the own lock, back off with jitter (1 s → 30 s), retry up to `lockWait`, then
//!   `409 repository-locked` naming the holder.
//! * **exclusive** (GC's sweep): the same, conflicting with any other non-stale lock.
//! * **refresh** every [`REFRESH`] (re-`PUT` with `PutMode::Overwrite`) while held.
//! * **stale**: `last_modified` (the storage server's clock, from LIST; the file mtime
//!   for `fs`) older than [`STALE`]: ignored by acquirers, removed by GC and
//!   `break_lock`.
//! * **release**: `DELETE` the own key, best effort (a leftover goes stale).
//! * Read-only repositories take no locks: [`acquire`] returns a guard that holds none.
//!
//! Staleness never compares the storage server's clock with this host's: an acquirer
//! measures other locks' ages against the `last_modified` of the lock object it has
//! just written, which is the storage server's "now".

use crate::error::Result;
use crate::layout::{LOCKS, lock_id_of, lock_key};
use crate::{
    BackupError, Code, Ctl, LockHolder, LockInfo, LockKind, LockObject, LockOperation, Repository,
};
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use object_store::path::Path as Key;
use object_store::{ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutPayload};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How often a held lock is re-written.
pub const REFRESH: Duration = Duration::from_secs(5 * 60);
/// Age (by `last_modified`) after which a lock is ignored.
pub const STALE: Duration = Duration::from_secs(30 * 60);
/// Default `lockWait`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10 * 60);
/// First and largest back-off between acquisition attempts (jittered).
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A held lock (or none, for read-only repositories). Release it with
/// [`release`](LockGuard::release); dropping it stops the refresh and deletes the lock
/// object in the background when a Tokio runtime is available (else it goes stale).
#[derive(Debug)]
pub struct LockGuard {
    /// the `<uuid>` of `locks/<uuid>.json`; `None` when nothing is held
    pub(crate) id: Option<String>,
    /// the store holding the lock object (`None` when nothing is held)
    store: Option<Arc<dyn ObjectStore>>,
    /// the lock object's `last_modified` when it was acquired: the storage server's
    /// clock, the reference for other objects' ages (GC's grace period)
    server_time: Option<DateTime<Utc>>,
    /// the task re-writing the lock every [`REFRESH`]
    refresh: Option<tokio::task::JoinHandle<()>>,
}

impl LockGuard {
    /// A guard that holds nothing.
    pub fn none() -> LockGuard {
        LockGuard {
            id: None,
            store: None,
            server_time: None,
            refresh: None,
        }
    }

    /// The lock id, if a lock is held.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The storage server's time when the lock was acquired (its `last_modified`), if
    /// a lock is held: the clock that ages locks and blobs.
    pub fn server_time(&self) -> Option<DateTime<Utc>> {
        self.server_time
    }

    /// Delete the lock object (best effort: a failure is logged, the lock goes stale).
    pub async fn release(mut self) -> Result<()> {
        if let Some(t) = self.refresh.take() {
            t.abort();
        }
        if let (Some(id), Some(store)) = (self.id.take(), self.store.take()) {
            delete_quietly(store.as_ref(), &id).await;
        }
        Ok(())
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(t) = self.refresh.take() {
            t.abort();
        }
        if let (Some(id), Some(store)) = (self.id.take(), self.store.take())
            && let Ok(h) = tokio::runtime::Handle::try_current()
        {
            h.spawn(async move { delete_quietly(store.as_ref(), &id).await });
        }
    }
}

async fn delete_quietly(store: &dyn ObjectStore, id: &str) {
    match store.delete(&lock_key(id)).await {
        Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
        Err(e) => tracing::warn!(
            target: "sparkles::backup",
            lock = id,
            error = %BackupError::from(e),
            "could not delete a repository lock; it goes stale"
        ),
    }
}

/// This process as a lock holder.
fn holder(server: &str) -> LockHolder {
    LockHolder {
        host: hostname(),
        pid: std::process::id(),
        server: server.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[cfg(unix)]
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for writes of its length; gethostname NUL-terminates the
    // name when it fits and returns 0
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        if let Ok(s) = std::str::from_utf8(&buf[..end])
            && !s.is_empty()
        {
            return s.to_string();
        }
    }
    "unknown".into()
}

#[cfg(not(unix))]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into())
}

/// Whether a lock last written at `modified` is stale at the storage server's `now`.
pub(crate) fn is_stale(modified: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    (now - modified).to_std().is_ok_and(|age| age > STALE)
}

/// A jittered back-off: a uniformly random duration in `[d/2, d]`.
fn jitter(d: Duration) -> Duration {
    let r = (Uuid::new_v4().as_u128() % 1000) as u32;
    d / 2 + d / 2 * r / 1000
}

/// Wait `d`, in slices of at most 100 ms, failing as soon as `ctl` is cancelled.
pub(crate) async fn sleep_cancellable(d: Duration, ctl: &Ctl) -> Result<()> {
    let end = Instant::now() + d;
    loop {
        ctl.check()?;
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        tokio::time::sleep(left.min(Duration::from_millis(100))).await;
    }
}

/// Every object under `locks/`.
pub(crate) async fn list_locks(store: &dyn ObjectStore) -> Result<Vec<ObjectMeta>> {
    Ok(store
        .list(Some(&Key::from(LOCKS)))
        .try_collect::<Vec<_>>()
        .await?)
}

/// The lock object at `key` (`None` if it is gone, or does not parse: `Some(None)`).
async fn read_lock(store: &dyn ObjectStore, key: &Key) -> Result<Option<Option<LockObject>>> {
    match store.get(key).await {
        Ok(g) => {
            let b = g.bytes().await?;
            Ok(Some(serde_json::from_slice(&b).ok()))
        }
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Acquire a `kind` lock on `repo` for `op`, waiting up to the repository's
/// `lock_wait` (`409 repository-locked` with a `holder` field after that; `cancelled`
/// when `ctl` is cancelled while waiting). Read-only repositories get
/// [`LockGuard::none`] without any request.
pub async fn acquire(
    repo: &Repository,
    kind: LockKind,
    op: LockOperation,
    ctl: &Ctl,
) -> Result<LockGuard> {
    ctl.check()?;
    if repo.readonly() {
        return Ok(LockGuard::none());
    }
    let store = repo.store.clone();
    let id = Uuid::new_v4().to_string();
    let key = lock_key(&id);
    let body = serde_json::to_vec_pretty(&LockObject {
        kind,
        holder: holder(&repo.env.server_id),
        operation: op,
        created: crate::now_rfc3339(),
    })
    .expect("a lock object serializes");
    let mode = if repo.config.conditional_writes {
        PutMode::Create
    } else {
        PutMode::Overwrite
    };
    let t0 = Instant::now();
    let mut backoff = BACKOFF_MIN;
    // lock objects never change their content, so their kinds are read once
    let mut kinds: HashMap<String, Option<LockObject>> = HashMap::new();
    loop {
        ctl.check()?;
        store
            .put_opts(&key, PutPayload::from(body.clone()), mode.clone().into())
            .await?;
        let listed = match list_locks(store.as_ref()).await {
            Ok(l) => l,
            Err(e) => {
                delete_quietly(store.as_ref(), &id).await;
                return Err(e);
            }
        };
        let now = listed
            .iter()
            .find(|m| m.location == key)
            .map(|m| m.last_modified);
        let reference = now.unwrap_or_else(Utc::now);
        let mut conflict = None;
        for m in &listed {
            let Some(other) = lock_id_of(&m.location) else {
                continue;
            };
            if other == id || is_stale(m.last_modified, reference) {
                continue;
            }
            if !kinds.contains_key(other) {
                match read_lock(store.as_ref(), &m.location).await {
                    // released meanwhile
                    Ok(None) => continue,
                    Ok(Some(obj)) => {
                        kinds.insert(other.to_string(), obj);
                    }
                    Err(e) => {
                        delete_quietly(store.as_ref(), &id).await;
                        return Err(e);
                    }
                }
            }
            let obj = &kinds[other];
            // an unreadable lock object counts as exclusive until it goes stale
            let conflicts = kind == LockKind::Exclusive
                || obj.as_ref().is_none_or(|o| o.kind == LockKind::Exclusive);
            if conflicts {
                conflict = Some((other.to_string(), obj.clone()));
                break;
            }
        }
        let Some((other, obj)) = conflict else {
            let refresh = tokio::runtime::Handle::try_current().ok().map(|h| {
                let (store, key, body) = (store.clone(), key.clone(), body.clone());
                h.spawn(async move {
                    loop {
                        tokio::time::sleep(REFRESH).await;
                        if let Err(e) = store
                            .put_opts(
                                &key,
                                PutPayload::from(body.clone()),
                                PutMode::Overwrite.into(),
                            )
                            .await
                        {
                            tracing::warn!(
                                target: "sparkles::backup",
                                error = %BackupError::from(e),
                                "could not refresh a repository lock"
                            );
                        }
                    }
                })
            });
            return Ok(LockGuard {
                id: Some(id),
                store: Some(store),
                server_time: now,
                refresh,
            });
        };
        delete_quietly(store.as_ref(), &id).await;
        let waited = t0.elapsed();
        if waited >= repo.env.lock_wait {
            return Err(locked(&other, obj.as_ref()));
        }
        let pause = jitter(backoff).min(repo.env.lock_wait - waited);
        tracing::debug!(
            target: "sparkles::backup",
            lock = %other,
            wait_ms = pause.as_millis() as u64,
            "repository locked; retrying"
        );
        sleep_cancellable(pause, ctl).await?;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// `409 repository-locked` naming the lock `id` and its holder.
fn locked(id: &str, obj: Option<&LockObject>) -> BackupError {
    match obj {
        Some(o) => BackupError::new(
            Code::RepositoryLocked,
            format!(
                "the repository is locked ({} lock {id} for {} by {} pid {}, since {})",
                kind_name(o.kind),
                op_name(o.operation),
                o.holder.host,
                o.holder.pid,
                o.created
            ),
        )
        .with("lock", id)
        .with(
            "holder",
            serde_json::to_value(&o.holder).expect("a holder serializes"),
        ),
        None => BackupError::new(
            Code::RepositoryLocked,
            format!("the repository is locked (unreadable lock object {id})"),
        )
        .with("lock", id),
    }
}

fn kind_name(k: LockKind) -> &'static str {
    match k {
        LockKind::Shared => "shared",
        LockKind::Exclusive => "exclusive",
    }
}

fn op_name(o: LockOperation) -> &'static str {
    match o {
        LockOperation::Create => "create",
        LockOperation::Restore => "restore",
        LockOperation::Verify => "verify",
        LockOperation::Delete => "delete",
        LockOperation::Gc => "gc",
    }
}

impl Repository {
    /// Every lock object, with staleness (`GET /$/repositories/{repo}/locks`), oldest
    /// first. Staleness is judged by this host's clock here (listing writes nothing);
    /// lock objects that do not parse are left out.
    pub async fn locks(&self) -> Result<Vec<LockInfo>> {
        let now = Utc::now();
        let mut out = Vec::new();
        for m in list_locks(self.store.as_ref()).await? {
            let Some(id) = lock_id_of(&m.location) else {
                continue;
            };
            let Some(Some(o)) = read_lock(self.store.as_ref(), &m.location).await? else {
                continue;
            };
            out.push(LockInfo {
                id: id.to_string(),
                kind: o.kind,
                operation: o.operation,
                holder: o.holder,
                created: o.created,
                last_modified: m
                    .last_modified
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                stale: is_stale(m.last_modified, now),
            });
        }
        out.sort_by(|a, b| a.last_modified.cmp(&b.last_modified).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Delete lock `id` (audited by the caller). `Ok(false)` if it did not exist;
    /// `409 repository-read-only` on a read-only repository.
    pub async fn break_lock(&self, id: &str) -> Result<bool> {
        if self.readonly() {
            return Err(BackupError::new(
                Code::RepositoryReadOnly,
                format!("repository {} is read-only", self.config.name),
            ));
        }
        let key = lock_key(id);
        if id.is_empty() || lock_id_of(&key) != Some(id) {
            return Ok(false);
        }
        match self.store.head(&key).await {
            Ok(_) => {}
            Err(object_store::Error::NotFound { .. }) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        match self.store.delete(&key).await {
            Ok(()) => {
                tracing::warn!(target: "sparkles::backup", lock = id, repository = %self.config.name, "repository lock broken");
                Ok(true)
            }
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    fn plant(kind: LockKind) -> Vec<u8> {
        serde_json::to_vec(&LockObject {
            kind,
            holder: LockHolder {
                host: "elsewhere".into(),
                pid: 42,
                server: "abc".into(),
                version: "0.0.1".into(),
            },
            operation: LockOperation::Gc,
            created: "2026-09-30T14:00:00.000Z".into(),
        })
        .unwrap()
    }

    async fn lock_count(repo: &Repository) -> usize {
        list_locks(repo.store.as_ref()).await.unwrap().len()
    }

    #[tokio::test]
    async fn a_shared_lock_exists_while_held_and_is_gone_after() {
        let repo = fixture::memory_repo().await;
        let ctl = Ctl::default();
        let g = acquire(&repo, LockKind::Shared, LockOperation::Create, &ctl)
            .await
            .unwrap();
        assert!(g.id().is_some() && g.server_time().is_some());
        let locks = repo.locks().await.unwrap();
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].id, g.id().unwrap());
        assert_eq!(
            (locks[0].kind, locks[0].operation, locks[0].stale),
            (LockKind::Shared, LockOperation::Create, false)
        );
        assert_eq!(locks[0].holder.pid, std::process::id());
        // shared locks do not exclude each other
        let g2 = acquire(&repo, LockKind::Shared, LockOperation::Restore, &ctl)
            .await
            .unwrap();
        assert_eq!(lock_count(&repo).await, 2);
        g.release().await.unwrap();
        g2.release().await.unwrap();
        assert_eq!(lock_count(&repo).await, 0);
    }

    #[tokio::test]
    async fn dropping_a_guard_deletes_the_lock_in_the_background() {
        let repo = fixture::memory_repo().await;
        let g = acquire(
            &repo,
            LockKind::Shared,
            LockOperation::Verify,
            &Ctl::default(),
        )
        .await
        .unwrap();
        drop(g);
        for _ in 0..50 {
            if lock_count(&repo).await == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the lock object was not deleted");
    }

    #[tokio::test]
    async fn read_only_repositories_take_no_locks() {
        let repo = fixture::repo_with(fixture::memory_store(), |c| c.readonly = true).await;
        let g = acquire(
            &repo,
            LockKind::Exclusive,
            LockOperation::Gc,
            &Ctl::default(),
        )
        .await
        .unwrap();
        assert!(g.id().is_none());
        assert_eq!(lock_count(&repo).await, 0);
        assert_eq!(
            repo.break_lock("x").await.unwrap_err().code(),
            Code::RepositoryReadOnly
        );
    }

    #[tokio::test]
    async fn a_fresh_exclusive_lock_blocks_until_lock_wait() {
        let store = fixture::memory_store();
        let repo = fixture::repo_with(store.clone(), |_| {}).await;
        let mut repo = repo;
        repo.env.lock_wait = Duration::from_secs(2);
        store
            .put(&lock_key("x"), PutPayload::from(plant(LockKind::Exclusive)))
            .await
            .unwrap();
        let t0 = Instant::now();
        let e = acquire(
            &repo,
            LockKind::Shared,
            LockOperation::Create,
            &Ctl::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code(), Code::RepositoryLocked);
        assert!(t0.elapsed() >= Duration::from_secs(2), "{:?}", t0.elapsed());
        assert_eq!(e.body()["holder"]["host"], "elsewhere");
        assert_eq!(e.body()["lock"], "x");
        // the waiter's own lock is gone
        assert_eq!(lock_count(&repo).await, 1);

        // an exclusive acquirer conflicts with a shared lock too
        store.delete(&lock_key("x")).await.unwrap();
        store
            .put(&lock_key("s"), PutPayload::from(plant(LockKind::Shared)))
            .await
            .unwrap();
        repo.env.lock_wait = Duration::ZERO;
        let e = acquire(
            &repo,
            LockKind::Exclusive,
            LockOperation::Gc,
            &Ctl::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code(), Code::RepositoryLocked);
        acquire(
            &repo,
            LockKind::Shared,
            LockOperation::Create,
            &Ctl::default(),
        )
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn waiting_can_be_cancelled() {
        let store = fixture::memory_store();
        let mut repo = fixture::repo_with(store.clone(), |_| {}).await;
        repo.env.lock_wait = Duration::from_secs(60);
        store
            .put(&lock_key("x"), PutPayload::from(plant(LockKind::Exclusive)))
            .await
            .unwrap();
        let ctl = Ctl::default();
        let c = ctl.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let e = acquire(&repo, LockKind::Shared, LockOperation::Create, &ctl)
            .await
            .unwrap_err();
        assert_eq!(e.code(), Code::Cancelled);
    }

    #[tokio::test]
    async fn stale_locks_are_ignored_listed_and_broken() {
        // an `fs` repository, so the planted lock's mtime can be set in the past
        let dir = tempfile::tempdir().unwrap();
        let mut repo = fixture::fs_repo(dir.path()).await;
        repo.env.lock_wait = Duration::from_secs(2);
        let path = dir.path().join("locks").join("x.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, plant(LockKind::Exclusive)).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(31 * 60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let t0 = Instant::now();
        let g = acquire(
            &repo,
            LockKind::Shared,
            LockOperation::Create,
            &Ctl::default(),
        )
        .await
        .unwrap();
        assert!(t0.elapsed() < Duration::from_secs(1));
        let locks = repo.locks().await.unwrap();
        assert_eq!(locks.len(), 2);
        let planted = locks.iter().find(|l| l.id == "x").unwrap();
        assert!(planted.stale);
        assert_eq!(planted.kind, LockKind::Exclusive);
        assert!(!locks.iter().find(|l| l.id != "x").unwrap().stale);

        // breaking the stale lock, the own lock, then a missing one
        assert!(repo.break_lock("x").await.unwrap());
        assert_eq!(repo.locks().await.unwrap().len(), 1);
        let own = g.id().unwrap().to_string();
        assert!(repo.break_lock(&own).await.unwrap());
        assert!(!repo.break_lock(&own).await.unwrap());
        assert!(!repo.break_lock("../sparkles-repo").await.unwrap());
        g.release().await.unwrap();

        // a fresh exclusive lock on fs blocks
        std::fs::write(&path, plant(LockKind::Exclusive)).unwrap();
        let e = acquire(
            &repo,
            LockKind::Shared,
            LockOperation::Create,
            &Ctl::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code(), Code::RepositoryLocked);
    }

    #[test]
    fn staleness_and_jitter() {
        let now = Utc::now();
        assert!(!is_stale(now, now));
        assert!(!is_stale(now - chrono::Duration::minutes(29), now));
        assert!(is_stale(now - chrono::Duration::minutes(31), now));
        // a lock written "after" the reference (clock granularity) is fresh
        assert!(!is_stale(now + chrono::Duration::seconds(1), now));
        for _ in 0..100 {
            let j = jitter(Duration::from_secs(4));
            assert!(j >= Duration::from_secs(2) && j <= Duration::from_secs(4));
        }
    }
}
