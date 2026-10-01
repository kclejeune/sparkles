//! Garbage collection of unreferenced blobs: two-phase mark and sweep.
//!
//! The mark runs under a shared lock, so backups keep being created meanwhile; only the
//! sweep takes the exclusive lock, and it re-lists the manifests first, so a blob that
//! a backup created between the two phases references is never deleted. Blobs younger
//! than the grace period are kept whatever the manifests say: they may belong to a
//! backup still uploading under a lock that went stale, or to a writer that takes no
//! locks. Ages are measured against the storage server's clock (the `last_modified`
//! of GC's own lock object), never this host's.

use crate::cache::version_of;
use crate::error::Result;
use crate::layout::{BACKUPS, BLOBS, PROBE, backup_name_of, blob_id_of, gc_last_key, lock_id_of};
use crate::lock::{self, LockGuard, STALE};
use crate::manifest;
use crate::{
    BackupError, Code, GcOptions, GcReport, GcRequests, LastGc, LockKind, LockOperation, Manifest,
    Repository,
};
use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use object_store::path::Path as Key;
use object_store::{ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutPayload};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Keys per `delete_stream` batch (S3 `DeleteObjects` takes up to 1000).
const DELETE_BATCH: usize = 1000;

/// A test hook run between the mark and the sweep of the repository with this id.
#[cfg(test)]
pub(crate) type Hook = Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>;
#[cfg(test)]
pub(crate) static BETWEEN_MARK_AND_SWEEP: std::sync::Mutex<Option<HashMap<uuid::Uuid, Hook>>> =
    std::sync::Mutex::new(None);

/// What the mark found.
#[derive(Default)]
struct Mark {
    /// manifests read: name → version (e_tag or last_modified)
    seen: HashMap<String, String>,
    /// blob ids the manifests reference
    referenced: HashSet<String>,
    /// unreferenced blobs past the grace period: key → stored size
    candidates: HashMap<Key, u64>,
    listed: u64,
    stored_bytes: u64,
    kept_young: u64,
}

impl Mark {
    fn reference(&mut self, m: &Manifest) {
        for f in &m.files {
            for b in &f.blobs {
                self.referenced.insert(b.id.clone());
            }
        }
    }
}

/// `1 +` a request per 1000 listed keys (the pages of an S3 listing).
fn list_requests(n: usize) -> u64 {
    1 + (n / 1000) as u64
}

/// Every object under `prefix`.
async fn list_all(store: &dyn ObjectStore, prefix: &str) -> Result<Vec<ObjectMeta>> {
    Ok(store
        .list(Some(&Key::from(prefix)))
        .try_collect::<Vec<_>>()
        .await?)
}

/// `1 blob`, `2 blobs` (progress messages).
fn blobs(n: u64) -> String {
    format!("{n} {}", if n == 1 { "blob" } else { "blobs" })
}

/// Whether an object last written at `modified` is at least `grace` old at `now`.
fn older_than(modified: DateTime<Utc>, now: DateTime<Utc>, grace: Duration) -> bool {
    (now - modified).to_std().is_ok_and(|age| age >= grace)
}

impl Repository {
    /// Collect blobs no manifest references:
    /// 1. **mark** under a shared lock: list `backups/`, read every manifest (cached),
    ///    list `blobs/`; candidates are unreferenced blobs whose `last_modified` is
    ///    older than `o.grace` (younger ones count as `keptYoung`);
    /// 2. **sweep** under an exclusive lock: re-list `backups/`, drop from the
    ///    candidates the blobs of manifests not seen in the mark, delete the rest
    ///    (`delete_stream`, batches of up to 1000), delete stale locks and `probe/`
    ///    leftovers, write `gc/last.json` ([`crate::LastGc`]).
    ///
    /// `o.dry_run` stops before deleting and reports the candidates: `deleted` and
    /// `deletedBytes` are what a real run would delete and `storedBytesAfter` the
    /// projected size; the re-listing then runs under the shared lock, no exclusive
    /// lock is taken, and `gc/last.json` is not written. `409 repository-read-only` on
    /// a read-only repository. A manifest that does not parse stops the collection (its
    /// blobs are unknown): `422 invalid-backup` naming it.
    pub async fn gc(&self, o: &GcOptions) -> Result<GcReport> {
        if self.readonly() {
            return Err(BackupError::new(
                Code::RepositoryReadOnly,
                format!("repository {} is read-only", self.config.name),
            ));
        }
        let t0 = Instant::now();
        let mut req = GcRequests::default();
        let mut wait = Duration::ZERO;

        let tl = Instant::now();
        let shared = lock::acquire(self, LockKind::Shared, LockOperation::Gc, &o.ctl).await?;
        wait += tl.elapsed();
        let now = shared.server_time().unwrap_or_else(Utc::now);
        let marked = self.mark(o, now, &mut req).await;
        let mut mark = match marked {
            Ok(m) => m,
            Err(e) => {
                let _ = shared.release().await;
                return Err(e);
            }
        };
        if o.dry_run {
            let r = self.relist(&mut mark, &mut req).await;
            let _ = shared.release().await;
            r?;
            let bytes: u64 = mark.candidates.values().sum();
            return Ok(report(
                o,
                &mark,
                &req,
                bytes,
                mark.candidates.len() as u64,
                t0,
                wait,
            ));
        }
        let _ = shared.release().await;

        #[cfg(test)]
        {
            let hook = BETWEEN_MARK_AND_SWEEP
                .lock()
                .unwrap()
                .as_mut()
                .and_then(|h| h.remove(&self.id()));
            if let Some(h) = hook {
                h().await;
            }
        }

        o.ctl.check()?;
        let tl = Instant::now();
        let exclusive = lock::acquire(self, LockKind::Exclusive, LockOperation::Gc, &o.ctl).await?;
        wait += tl.elapsed();
        let r = self
            .sweep(o, &mut mark, &mut req, &exclusive, t0, wait)
            .await;
        let _ = exclusive.release().await;
        r
    }

    /// The mark: every manifest's blobs, and the unreferenced blobs older than the
    /// grace period at the storage server's `now`.
    async fn mark(&self, o: &GcOptions, now: DateTime<Utc>, req: &mut GcRequests) -> Result<Mark> {
        let mut mark = Mark::default();
        o.ctl.report(0.05, "reading manifests");
        self.read_new_manifests(&mut mark, req).await?;
        o.ctl.check()?;
        o.ctl.report(0.4, "listing blobs");
        let blobs = list_all(self.store.as_ref(), BLOBS).await?;
        req.list += list_requests(blobs.len());
        for b in blobs {
            let Some(id) = blob_id_of(&b.location) else {
                continue;
            };
            mark.listed += 1;
            mark.stored_bytes += b.size;
            if mark.referenced.contains(id) {
                continue;
            }
            if older_than(b.last_modified, now, o.grace) {
                mark.candidates.insert(b.location, b.size);
            } else {
                mark.kept_young += 1;
            }
        }
        Ok(mark)
    }

    /// Read the manifests under `backups/` that `mark` has not seen (at their current
    /// version), adding their blobs to the referenced set.
    async fn read_new_manifests(
        &self,
        mark: &mut Mark,
        req: &mut GcRequests,
    ) -> Result<Vec<String>> {
        let listed = list_all(self.store.as_ref(), BACKUPS).await?;
        req.list += list_requests(listed.len());
        let todo: Vec<(String, ObjectMeta)> = listed
            .into_iter()
            .filter_map(|meta| {
                let name = backup_name_of(&meta.location)?.to_string();
                (mark.seen.get(&name) != Some(&version_of(&meta))).then_some((name, meta))
            })
            .collect();
        let read: Vec<Option<(String, String, Manifest, bool)>> = futures::stream::iter(todo)
            .map(|(name, meta)| async move {
                match manifest::fetch(self, &name, Some(&meta)).await {
                    Ok((m, meta, fetched)) => Ok(Some((name, version_of(&meta), m, fetched))),
                    // deleted since the listing
                    Err(e) if e.code() == Code::NoSuchBackup => Ok(None),
                    Err(e) if e.code() == Code::InvalidBackup => Err(BackupError::new(
                        Code::InvalidBackup,
                        format!(
                            "backup {name}: {}; garbage collection needs every manifest \
                             (delete it or restore a valid copy)",
                            e.message()
                        ),
                    )
                    .with("backup", name.as_str())),
                    Err(e) => Err(e),
                }
            })
            .buffer_unordered(self.config.concurrency())
            .try_collect()
            .await?;
        let mut names = Vec::new();
        for (name, version, m, fetched) in read.into_iter().flatten() {
            if fetched {
                req.get += 1;
            }
            mark.reference(&m);
            mark.seen.insert(name.clone(), version);
            names.push(name);
        }
        Ok(names)
    }

    /// Re-list the manifests and keep the blobs of those created since the mark.
    async fn relist(&self, mark: &mut Mark, req: &mut GcRequests) -> Result<()> {
        let new = self.read_new_manifests(mark, req).await?;
        if !new.is_empty() {
            tracing::info!(
                target: "sparkles::backup",
                repository = %self.config.name,
                backups = ?new,
                "backups created during garbage collection keep their blobs"
            );
            let referenced = &mark.referenced;
            mark.candidates
                .retain(|k, _| blob_id_of(k).is_none_or(|id| !referenced.contains(id)));
        }
        Ok(())
    }

    async fn sweep(
        &self,
        o: &GcOptions,
        mark: &mut Mark,
        req: &mut GcRequests,
        lock: &LockGuard,
        t0: Instant,
        wait: Duration,
    ) -> Result<GcReport> {
        o.ctl.report(0.6, "re-reading manifests");
        self.relist(mark, req).await?;
        let mut keys: Vec<Key> = mark.candidates.keys().cloned().collect();
        keys.sort();
        let total = keys.len().max(1);
        let (mut deleted, mut deleted_bytes) = (0u64, 0u64);
        for (i, batch) in keys.chunks(DELETE_BATCH).enumerate() {
            o.ctl.check()?;
            req.delete += 1;
            let input = futures::stream::iter(batch.to_vec()).map(Ok).boxed();
            let mut out = self.store.delete_stream(input);
            while let Some(r) = out.next().await {
                match r {
                    Ok(k) => {
                        deleted += 1;
                        deleted_bytes += mark.candidates.get(&k).copied().unwrap_or(0);
                    }
                    Err(object_store::Error::NotFound { .. }) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let done = (i * DELETE_BATCH + batch.len()) as f32 / total as f32;
            o.ctl
                .report(0.65 + 0.25 * done, &format!("deleted {}", blobs(deleted)));
        }

        // leftovers of other operations: stale locks and connection-test probes
        let now = lock.server_time().unwrap_or_else(Utc::now);
        let locks = lock::list_locks(self.store.as_ref()).await?;
        req.list += 1;
        let probes = list_all(self.store.as_ref(), PROBE).await?;
        req.list += 1;
        let stale = locks
            .into_iter()
            .filter(|m| {
                lock_id_of(&m.location).is_some_and(|id| Some(id) != lock.id())
                    && lock::is_stale(m.last_modified, now)
            })
            .chain(
                probes
                    .into_iter()
                    .filter(|m| older_than(m.last_modified, now, STALE)),
            );
        for m in stale {
            req.delete += 1;
            match self.store.delete(&m.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }

        let rep = report(o, mark, req, deleted_bytes, deleted, t0, wait);
        let last = LastGc {
            report: rep.clone(),
            finished: crate::now_rfc3339(),
        };
        self.store
            .put_opts(
                &gc_last_key(),
                PutPayload::from(serde_json::to_vec_pretty(&last).expect("a report serializes")),
                PutMode::Overwrite.into(),
            )
            .await?;
        tracing::info!(
            target: "sparkles::backup",
            repository = %self.config.name,
            deleted,
            deleted_bytes,
            kept_young = rep.kept_young,
            "garbage collection finished"
        );
        o.ctl.report(1.0, &format!("deleted {}", blobs(deleted)));
        Ok(rep)
    }

    /// The report of the last garbage collection (`gc/last.json`), if one ran.
    pub async fn last_gc(&self) -> Result<Option<LastGc>> {
        match self.store.get(&gc_last_key()).await {
            Ok(g) => {
                let b = g.bytes().await?;
                Ok(serde_json::from_slice(&b).ok())
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

fn report(
    o: &GcOptions,
    mark: &Mark,
    req: &GcRequests,
    deleted_bytes: u64,
    deleted: u64,
    t0: Instant,
    wait: Duration,
) -> GcReport {
    GcReport {
        dry_run: o.dry_run,
        manifests: mark.seen.len() as u64,
        referenced_blobs: mark.referenced.len() as u64,
        listed_blobs: mark.listed,
        candidates: mark.candidates.len() as u64,
        deleted,
        deleted_bytes,
        kept_young: mark.kept_young,
        stored_bytes_after: mark.stored_bytes.saturating_sub(deleted_bytes),
        requests: *req,
        millis: t0.elapsed().as_millis() as u64,
        lock_wait_millis: wait.as_millis() as u64,
    }
}

#[cfg(test)]
mod tests;
