//! Repository locks: lease objects `locks/<uuid>.json` ([`LockObject`]).
//!
//! * **shared** (create, delete, restore, verify): `PUT` the own lock
//!   (`PutMode::Create`), then `LIST locks/`; a non-stale exclusive lock means: delete
//!   the own lock, back off with jitter (1 s → 30 s), retry up to `lockWait`, then
//!   `409 repository-locked` naming the holder.
//! * **exclusive** (GC's sweep): the same, conflicting with any other non-stale lock.
//! * **refresh** every [`REFRESH`] with a conditional update, an open-inode touch
//!   on the filesystem, or an overwrite in single-writer object stores.
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
    BackupError, Code, Ctl, LockHolder, LockInfo, LockKind, LockObject, LockOperation, RepoType,
    Repository,
};
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt, TryStreamExt};
use object_store::path::Path as Key;
use object_store::{
    GetOptions, GetResult, ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutPayload,
    UpdateVersion,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
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

#[derive(Clone, Copy, Debug)]
struct LeaseClock {
    monotonic: Instant,
    wall: SystemTime,
}

impl LeaseClock {
    fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    fn elapsed(self) -> Duration {
        // The wall clock also advances when the host is suspended.
        self.monotonic
            .elapsed()
            .max(self.wall.elapsed().unwrap_or(STALE))
    }
}

type Renewal = Arc<Mutex<Option<LeaseClock>>>;

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
    acquired_at: LeaseClock,
    renewed: Renewal,
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
            acquired_at: LeaseClock::now(),
            renewed: Arc::new(Mutex::new(None)),
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

    pub(crate) fn estimated_server_time(&self) -> Option<DateTime<Utc>> {
        let elapsed = chrono::Duration::from_std(self.acquired_at.elapsed()).ok()?;
        self.server_time?.checked_add_signed(elapsed)
    }

    /// Operations may use the protected objects only while their lease is live.
    pub(crate) async fn check_live(&self) -> Result<()> {
        self.live_head(GetOptions::default()).await?;
        Ok(())
    }

    /// Check the lease before admitting each batch of reader requests.
    pub(crate) fn read_batches<I: IntoIterator>(
        &self,
        requests: I,
        batch_size: usize,
        ctl: &Ctl,
    ) -> impl Stream<Item = Result<I::Item>> {
        futures::stream::iter(requests.into_iter().enumerate()).then(
            move |(index, request)| async move {
                if index.is_multiple_of(batch_size) {
                    ctl.check()?;
                    self.check_live().await?;
                }
                Ok(request)
            },
        )
    }

    async fn live_head(&self, mut options: GetOptions) -> Result<Option<GetResult>> {
        let (Some(id), Some(store)) = (self.id(), &self.store) else {
            return Ok(None);
        };
        let check = || {
            let mut renewed = self.renewed.lock().unwrap();
            if renewed.is_some_and(|clock| clock.elapsed() < STALE) {
                Ok(())
            } else {
                *renewed = None;
                Err(lost_lease(id))
            }
        };
        check()?;
        options.head = true;
        let found = store.get_opts(&lock_key(id), options).await;
        check()?;
        match found {
            Ok(head)
                if self
                    .estimated_server_time()
                    .is_none_or(|now| !is_stale(head.meta.last_modified, now)) =>
            {
                Ok(Some(head))
            }
            Ok(_) | Err(object_store::Error::NotFound { .. }) => {
                *self.renewed.lock().unwrap() = None;
                Err(lost_lease(id))
            }
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(feature = "s3")]
    pub(crate) async fn fresh_server_time(&self) -> Result<Option<DateTime<Utc>>> {
        let Some(head) = self
            .live_head(crate::storage::RetentionHead::options())
            .await?
        else {
            return Ok(None);
        };
        let now = head
            .extensions
            .get::<crate::storage::RetentionHead>()
            .map(crate::storage::RetentionHead::server_time)
            .transpose()?;
        if now.is_some_and(|now| is_stale(head.meta.last_modified, now)) {
            *self.renewed.lock().unwrap() = None;
            return Err(lost_lease(self.id().expect("lease HEAD has an id")));
        }
        Ok(now)
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
    acquire_with_refresh(repo, kind, op, ctl, REFRESH).await
}

async fn acquire_with_refresh(
    repo: &Repository,
    kind: LockKind,
    op: LockOperation,
    ctl: &Ctl,
    refresh_delay: Duration,
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
    let t0 = Instant::now();
    let mut backoff = BACKOFF_MIN;
    // lock objects never change their content, so their kinds are read once
    let mut kinds: HashMap<String, Option<LockObject>> = HashMap::new();
    loop {
        ctl.check()?;
        // a fresh key: a conditional create only guards against a colliding id, and a
        // backend without conditional creates (single writer) gets a plain put
        let mode = if repo.single_writer() {
            PutMode::Overwrite
        } else {
            PutMode::Create
        };
        let acquired_at = LeaseClock::now();
        let version = match store
            .put_opts(&key, PutPayload::from(body.clone()), mode.into())
            .await
        {
            Err(object_store::Error::NotImplemented { .. }) => {
                repo.no_conditional_writes();
                store.put(&key, PutPayload::from(body.clone())).await?
            }
            result => result?,
        }
        .into();
        let listed = match list_locks(store.as_ref()).await {
            Ok(l) => l,
            Err(e) => {
                delete_quietly(store.as_ref(), &id).await;
                return Err(e);
            }
        };
        let Some(now) = listed
            .iter()
            .find(|m| m.location == key)
            .map(|m| m.last_modified)
        else {
            delete_quietly(store.as_ref(), &id).await;
            return Err(BackupError::new(
                Code::RepositoryLocked,
                "the new repository lease was not visible in the lock listing",
            ));
        };
        let mut conflict = None;
        for m in &listed {
            let Some(other) = lock_id_of(&m.location) else {
                continue;
            };
            if other == id || is_stale(m.last_modified, now) {
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
            let renewed = Arc::new(Mutex::new(Some(acquired_at)));
            let overwrite = repo.config.kind != RepoType::Fs && repo.single_writer();
            let refresh = tokio::runtime::Handle::try_current().ok().map(|h| {
                h.spawn(refresh_lock(
                    store.clone(),
                    key.clone(),
                    PutPayload::from(body.clone()),
                    version,
                    overwrite,
                    renewed.clone(),
                    refresh_delay,
                ))
            });
            return Ok(LockGuard {
                id: Some(id),
                store: Some(store),
                server_time: Some(now),
                acquired_at,
                renewed,
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

async fn refresh_lock(
    store: Arc<dyn ObjectStore>,
    key: Key,
    body: PutPayload,
    mut version: UpdateVersion,
    overwrite: bool,
    renewed: Renewal,
    delay: Duration,
) {
    #[cfg(feature = "fs")]
    let mut file = None;
    loop {
        tokio::time::sleep(delay).await;
        let Some(previous) = *renewed.lock().unwrap() else {
            return;
        };
        let started = LeaseClock::now();
        // Leave room for storage's second-resolution timestamps and stop an
        // in-flight retry before the preceding lease can go stale.
        let budget = (STALE - Duration::from_secs(1)).saturating_sub(previous.elapsed());
        if budget.is_zero() {
            break;
        }
        let update = async {
            #[cfg(feature = "fs")]
            if let Some(file) = &file {
                return touch_lease(file).await.map(|()| None);
            }
            if overwrite {
                // Single-writer stores cannot make this check atomic with the PUT.
                store.head(&key).await?;
                store.put(&key, body.clone()).await?;
                return Ok(None);
            }
            let mut result = store
                .put_opts(&key, body.clone(), PutMode::Update(version.clone()).into())
                .await;
            if matches!(result, Err(object_store::Error::Precondition { .. })) {
                // This UUID is written only by its holder. A previous renewal
                // may have committed even though its response was lost.
                let head = store.head(&key).await?;
                version = UpdateVersion {
                    e_tag: head.e_tag,
                    version: head.version,
                };
                result = store
                    .put_opts(&key, body.clone(), PutMode::Update(version.clone()).into())
                    .await;
            }
            match result {
                Ok(result) => Ok(Some(result.into())),
                #[cfg(feature = "fs")]
                Err(error @ object_store::Error::NotImplemented { .. }) => {
                    // LocalFileSystem has no conditional PUT. Touch the open
                    // inode so a concurrent unlink cannot recreate the key.
                    let get = store
                        .get_opts(
                            &key,
                            GetOptions {
                                if_match: version.e_tag.clone(),
                                head: true,
                                ..Default::default()
                            },
                        )
                        .await?;
                    let object_store::GetResultPayload::File(handle, _) = get.payload else {
                        return Err(error);
                    };
                    let handle = Arc::new(handle);
                    touch_lease(&handle).await?;
                    file = Some(handle);
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        };
        match tokio::time::timeout(budget, update).await {
            Ok(Ok(next)) if previous.elapsed() < STALE => {
                let mut lease = renewed.lock().unwrap();
                if lease.is_none() {
                    return;
                }
                *lease = Some(started);
                if let Some(next) = next {
                    version = next;
                }
            }
            Ok(Err(error)) => {
                let lost = matches!(error, object_store::Error::NotFound { .. });
                let unsupported = matches!(error, object_store::Error::NotImplemented { .. });
                tracing::warn!(
                    target: "sparkles::backup",
                    error = %BackupError::from(error),
                    "could not refresh a repository lock"
                );
                if lost {
                    break;
                }
                if unsupported {
                    // Keep the original lease until it expires. There is no safe overwrite.
                    return;
                }
            }
            _ => break,
        }
    }
    *renewed.lock().unwrap() = None;
}

#[cfg(feature = "fs")]
async fn touch_lease(file: &Arc<std::fs::File>) -> object_store::Result<()> {
    let file = file.clone();
    let result = tokio::task::spawn_blocking(move || file.set_modified(SystemTime::now())).await?;
    result.map_err(|source| object_store::Error::Generic {
        store: "filesystem",
        source: Box::new(source),
    })
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

fn lost_lease(id: &str) -> BackupError {
    BackupError::new(
        Code::RepositoryLocked,
        format!("the repository lease {id} was lost or expired"),
    )
    .with("lock", id)
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
        #[cfg(feature = "encryption")]
        LockOperation::KeyAdd => "keyadd",
        #[cfg(feature = "encryption")]
        LockOperation::KeyRemove => "keyremove",
        #[cfg(feature = "encryption")]
        LockOperation::KeyRotate => "keyrotate",
        #[cfg(feature = "encryption")]
        LockOperation::KeyRetire => "keyretire",
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
    use futures::stream::BoxStream;
    use object_store::{
        CopyOptions, ListResult, MultipartUpload, PutMultipartOptions, PutOptions, PutResult,
    };

    #[derive(Debug)]
    struct LeaseStore {
        inner: Arc<dyn ObjectStore>,
        hide_locks: bool,
        disable_conditionals: bool,
    }

    impl std::fmt::Display for LeaseStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "LeaseStore({})", self.inner)
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for LeaseStore {
        async fn put_opts(
            &self,
            key: &Key,
            payload: PutPayload,
            opts: PutOptions,
        ) -> object_store::Result<PutResult> {
            if self.disable_conditionals && !matches!(opts.mode, PutMode::Overwrite) {
                return Err(object_store::Error::NotImplemented {
                    operation: "conditional put".into(),
                    implementer: "LeaseStore".into(),
                });
            }
            self.inner.put_opts(key, payload, opts).await
        }

        async fn put_multipart_opts(
            &self,
            key: &Key,
            opts: PutMultipartOptions,
        ) -> object_store::Result<Box<dyn MultipartUpload>> {
            self.inner.put_multipart_opts(key, opts).await
        }

        async fn get_opts(&self, key: &Key, opts: GetOptions) -> object_store::Result<GetResult> {
            self.inner.get_opts(key, opts).await
        }

        fn delete_stream(
            &self,
            keys: BoxStream<'static, object_store::Result<Key>>,
        ) -> BoxStream<'static, object_store::Result<Key>> {
            self.inner.delete_stream(keys)
        }

        fn list(
            &self,
            prefix: Option<&Key>,
        ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
            if self.hide_locks && prefix.is_some_and(|key| key.as_ref() == LOCKS) {
                return Box::pin(futures::stream::empty());
            }
            self.inner.list(prefix)
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&Key>,
        ) -> object_store::Result<ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy_opts(
            &self,
            from: &Key,
            to: &Key,
            opts: CopyOptions,
        ) -> object_store::Result<()> {
            self.inner.copy_opts(from, to, opts).await
        }
    }

    async fn wait_for_renewal(guard: &LockGuard, previous: LeaseClock) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = guard.renewed.lock().unwrap().expect("lease was lost");
                if current.monotonic > previous.monotonic {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("lease was not renewed");
    }

    #[tokio::test]
    async fn single_writer_s3_lease_renews_without_conditional_put() {
        let store = Arc::new(LeaseStore {
            inner: fixture::memory_store(),
            hide_locks: false,
            disable_conditionals: true,
        });
        let repo = fixture::repo_with(store, |config| {
            config.kind = RepoType::S3;
            config.conditional_writes = false;
        })
        .await;
        let guard = acquire_with_refresh(
            &repo,
            LockKind::Exclusive,
            LockOperation::Gc,
            &Ctl::default(),
            Duration::ZERO,
        )
        .await
        .unwrap();
        wait_for_renewal(&guard, guard.acquired_at).await;
        let head = repo
            .store
            .head(&lock_key(guard.id().unwrap()))
            .await
            .unwrap();
        assert!(head.last_modified > guard.server_time().unwrap());
        guard.check_live().await.unwrap();
        guard.release().await.unwrap();
    }

    #[tokio::test]
    async fn acquisition_deletes_a_lease_missing_from_the_listing() {
        let inner = fixture::memory_store();
        let repo = fixture::repo_with(
            Arc::new(LeaseStore {
                inner: inner.clone(),
                hide_locks: true,
                disable_conditionals: false,
            }),
            |_| {},
        )
        .await;
        let error = acquire(
            &repo,
            LockKind::Exclusive,
            LockOperation::Gc,
            &Ctl::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), Code::RepositoryLocked);
        assert!(error.message().contains("not visible in the lock listing"));
        assert!(error.body().get("lock").is_none());
        assert!(list_locks(inner.as_ref()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn conditional_renewal_recovers_after_an_unacknowledged_write() {
        let repo = fixture::memory_repo().await;
        let guard = acquire_with_refresh(
            &repo,
            LockKind::Shared,
            LockOperation::Create,
            &Ctl::default(),
            Duration::ZERO,
        )
        .await
        .unwrap();
        // Advance the ETag without advancing the version known to the refresh task.
        let key = lock_key(guard.id().unwrap());
        let first = repo.store.get(&key).await.unwrap();
        let original = first.meta.e_tag.clone();
        let unacknowledged = repo
            .store
            .put(&key, first.bytes().await.unwrap().into())
            .await
            .unwrap();
        assert_ne!(unacknowledged.e_tag, original);
        wait_for_renewal(&guard, guard.acquired_at).await;
        assert_ne!(
            repo.store.head(&key).await.unwrap().e_tag,
            unacknowledged.e_tag
        );
        guard.check_live().await.unwrap();
        guard.release().await.unwrap();
    }

    #[tokio::test]
    async fn removed_lease_refuses_to_publish() {
        let repo = crate::fixture::memory_repo().await;
        let guard = acquire(
            &repo,
            LockKind::Shared,
            LockOperation::Create,
            &Ctl::default(),
        )
        .await
        .unwrap();
        guard.check_live().await.unwrap();
        repo.store
            .delete(&lock_key(guard.id().unwrap()))
            .await
            .unwrap();
        assert_eq!(
            guard.check_live().await.unwrap_err().code(),
            Code::RepositoryLocked
        );
        guard.release().await.unwrap();
    }

    #[tokio::test]
    async fn refresh_cannot_recreate_a_broken_lease() {
        let store = fixture::memory_store();
        let key = lock_key(&Uuid::new_v4().to_string());
        let body = PutPayload::from(plant(LockKind::Shared));
        let version = store.put(&key, body.clone()).await.unwrap().into();
        store.delete(&key).await.unwrap();
        let renewed = Arc::new(Mutex::new(Some(LeaseClock::now())));
        tokio::time::timeout(
            Duration::from_secs(1),
            refresh_lock(
                store.clone(),
                key.clone(),
                body,
                version,
                false,
                renewed.clone(),
                Duration::ZERO,
            ),
        )
        .await
        .expect("a failed precondition stops renewal");
        assert!(renewed.lock().unwrap().is_none());
        assert!(matches!(
            store.head(&key).await,
            Err(object_store::Error::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn stale_lease_stays_lost_even_if_the_key_still_exists() {
        let repo = fixture::memory_repo().await;
        let guard = acquire(
            &repo,
            LockKind::Exclusive,
            LockOperation::Gc,
            &Ctl::default(),
        )
        .await
        .unwrap();
        guard.check_live().await.unwrap();
        let mut clock = LeaseClock::now();
        clock.wall -= STALE;
        *guard.renewed.lock().unwrap() = Some(clock);
        assert_eq!(
            guard.check_live().await.unwrap_err().code(),
            Code::RepositoryLocked
        );
        assert!(guard.renewed.lock().unwrap().is_none());
        assert!(
            repo.store
                .head(&lock_key(guard.id().unwrap()))
                .await
                .is_ok()
        );
        guard.release().await.unwrap();
    }

    #[cfg(feature = "fs")]
    #[tokio::test]
    async fn filesystem_renewal_touches_only_the_original_inode() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::open(
            &crate::RepoConfig {
                name: "local".into(),
                kind: RepoType::Fs,
                path: Some(temp.path().display().to_string()),
                conditional_writes: true,
                ..Default::default()
            },
            &crate::OpenEnv::default(),
        )
        .await
        .unwrap();
        let guard = acquire_with_refresh(
            &repo,
            LockKind::Shared,
            LockOperation::Restore,
            &Ctl::default(),
            Duration::ZERO,
        )
        .await
        .unwrap();
        let key = lock_key(guard.id().unwrap());
        let path = temp.path().join(key.as_ref());
        let file = std::fs::File::open(&path).unwrap();
        let initial = file.metadata().unwrap().modified().unwrap();
        wait_for_renewal(&guard, guard.acquired_at).await;
        assert!(file.metadata().unwrap().modified().unwrap() > initial);
        guard.check_live().await.unwrap();
        std::fs::remove_file(&path).unwrap();
        let unlinked = file.metadata().unwrap().modified().unwrap();
        wait_for_renewal(&guard, LeaseClock::now()).await;
        assert!(file.metadata().unwrap().modified().unwrap() > unlinked);
        assert!(!path.exists());
        assert_eq!(
            guard.check_live().await.unwrap_err().code(),
            Code::RepositoryLocked
        );
        guard.release().await.unwrap();
    }

    #[cfg(feature = "fs")]
    #[tokio::test]
    async fn restore_and_verify_stop_reading_after_their_lease_is_broken() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let source = temp.path().join("source");
        let restored = temp.path().join("restored");
        let mut repo = fixture::fs_repo(&root).await;
        repo.config.max_concurrency = Some(1);
        fixture::make_db(&source);
        let manifest = fixture::put_backup(&repo, &source, "backup").await;
        assert!(
            manifest
                .files
                .iter()
                .filter(|file| !file.blobs.is_empty())
                .count()
                > 1
        );
        let progress = Arc::new(AtomicUsize::new(0));
        let break_on_first_read = |prefix: &'static str| Ctl {
            progress: Some(Arc::new({
                let lock_dir = root.join(LOCKS);
                let progress = progress.clone();
                move |_, message: &str| {
                    if message.starts_with(prefix) && progress.fetch_add(1, Ordering::Relaxed) == 0
                    {
                        let mut entries = std::fs::read_dir(&lock_dir).unwrap();
                        let lease = entries.next().unwrap().unwrap();
                        assert!(entries.next().is_none());
                        std::fs::remove_file(lease.path()).unwrap();
                    }
                }
            })),
            ..Default::default()
        };
        let error = repo
            .restore(
                "backup",
                &restored,
                &crate::RestoreOptions {
                    ctl: break_on_first_read("downloading "),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::RepositoryLocked);
        assert_eq!(progress.swap(0, Ordering::Relaxed), 1);
        assert!(!restored.exists());

        let error = repo
            .verify(
                &["backup".into()],
                &crate::VerifyOptions {
                    level: crate::VerifyLevel::Data,
                    ctl: break_on_first_read("checked "),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::RepositoryLocked);
        assert_eq!(progress.load(Ordering::Relaxed), 1);
        assert_eq!(lock_count(&repo).await, 0);
    }

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

    #[tokio::test]
    async fn a_create_holds_one_shared_lock_and_honours_exclusive_ones() {
        use crate::{CreateOptions, Source};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let mut repo = fixture::fs_repo(&root).await;
        repo.env.lock_wait = Duration::from_secs(2);
        let src = dir.path().join("src");
        fixture::make_db(&src);
        let locks = root.join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        let planted = locks.join("x.json");
        std::fs::write(&planted, plant(LockKind::Exclusive)).unwrap();

        // the most lock objects besides the planted one seen during the create
        let most = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (m2, l2) = (most.clone(), locks.clone());
        let opts = |name: &str| CreateOptions {
            name: name.into(),
            dataset_name: "ds".into(),
            ctl: Ctl {
                progress: Some(Arc::new({
                    let (m2, l2) = (m2.clone(), l2.clone());
                    move |_, _: &str| {
                        let n = std::fs::read_dir(&l2)
                            .unwrap()
                            .filter(|e| e.as_ref().unwrap().file_name() != "x.json")
                            .count();
                        m2.fetch_max(n, std::sync::atomic::Ordering::SeqCst);
                    }
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        let e = repo
            .create(Source::from_closed_dir(&src).unwrap(), &opts("b1"))
            .await
            .unwrap_err();
        assert_eq!(e.code(), Code::RepositoryLocked);

        let old = std::time::SystemTime::now() - Duration::from_secs(31 * 60);
        std::fs::File::options()
            .write(true)
            .open(&planted)
            .unwrap()
            .set_modified(old)
            .unwrap();
        repo.create(Source::from_closed_dir(&src).unwrap(), &opts("b1"))
            .await
            .unwrap();
        assert_eq!(most.load(std::sync::atomic::Ordering::SeqCst), 1);
        // afterwards only the planted lock is left
        assert_eq!(std::fs::read_dir(&locks).unwrap().count(), 1);
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
