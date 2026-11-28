//! The storage quota of a persistent dataset: a limit on the on-disk bytes of its
//! directory (index generations, write-ahead logs, delta vocabulary, the commit
//! catalog, the full-text and spatial indexes, everything under the root).
//!
//! The quota is [`StoreOptions::max_disk_bytes`](super::StoreOptions::max_disk_bytes),
//! unless the dataset's `quota.json` overrides it. A commit that adds quads is refused
//! with [`Error::BudgetExceeded`] (kind [`BudgetKind::DatasetBytes`]) when it would take
//! the dataset over its quota, before anything is written. Reads, compactions and
//! commits that only delete are never refused.
//!
//! Size is measured by walking the directory, at most once a second while a quota is
//! set, and after every rebuild. Between walks, small commits account for growth in
//! their files as they write them. A rebuild is checked with walks of its own.

use super::{Store, dir_size, write_atomic};
use crate::error::{Budget, BudgetKind, Error, Result};
use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// The file in the dataset directory that holds a dataset's own quota.
pub const QUOTA_FILE: &str = "quota.json";

/// How old a measurement may be before a commit walks the directory again.
const MAX_AGE: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaFile {
    format: u32,
    /// 0: unlimited, whatever the default
    max_bytes: u64,
}

/// Where a dataset's quota comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuotaSource {
    /// the dataset's own `quota.json`
    Dataset,
    /// [`StoreOptions::max_disk_bytes`](super::StoreOptions::max_disk_bytes) (the
    /// server's `--max-dataset-mb`)
    Default,
}

/// A dataset's quota and what it uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStatus {
    /// the quota in effect (`None`: unlimited)
    pub max_bytes: Option<u64>,
    pub source: QuotaSource,
    /// the default the dataset falls back to without an override (`None`: unlimited)
    pub default_max_bytes: Option<u64>,
    /// on-disk bytes of the dataset directory (0 for an in-memory store)
    pub used_bytes: u64,
}

/// Unused file tails and the staged compaction of one writable store.
#[derive(Default)]
struct StoreUsage {
    generation: u64,
    annotations: u64,
    changes: u64,
    building: Option<PathBuf>,
}

impl StoreUsage {
    fn total(&self) -> u64 {
        self.generation + self.annotations + self.changes
    }
}

#[derive(Default)]
struct Measurement {
    at: Option<Instant>,
    physical_bytes: u64,
    stores: FxHashMap<Uuid, StoreUsage>,
    padding: u64,
}

impl Measurement {
    fn update_padding(&mut self, store: Uuid, update: impl FnOnce(&mut StoreUsage)) -> bool {
        let Some(padding) = self.stores.get_mut(&store) else {
            return false;
        };
        self.padding -= padding.total();
        update(padding);
        self.padding += padding.total();
        true
    }

    fn building_bytes(&self) -> u64 {
        self.stores
            .values()
            .filter_map(|p| p.building.as_deref())
            .map(dir_size)
            .sum()
    }
}

pub(crate) struct Quota {
    root: Option<PathBuf>,
    default: Option<u64>,
    /// the dataset's own quota (`Some(0)`: unlimited)
    own: RwLock<Option<u64>>,
    /// The cached walk and the padding of the stores it measures.
    measured: Mutex<Measurement>,
    /// the records of grouped commits queued for their fence, which the leader writes to
    /// the log later: counted as if they were in it
    queued: AtomicU64,
    /// the space that running group commit fences may preallocate, which the free-space
    /// reserve counts with the queued commits until the fence has written
    promised: AtomicU64,
    /// directory walks completed so far
    measures: AtomicU64,
    /// runs inside the next walk, before it reads the directory (for tests)
    #[cfg(test)]
    walking: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Quota {
    /// The quota of a store rooted at `root` (none for an in-memory store), reading the
    /// dataset's `quota.json` if there is one.
    pub(crate) fn open(root: Option<&Path>, default: Option<u64>) -> Result<Quota> {
        let own = match root {
            Some(r) => read_file(r)?,
            None => None,
        };
        Ok(Quota {
            root: root.map(Path::to_path_buf),
            default: root.and(default),
            own: RwLock::new(own),
            measured: Mutex::new(Measurement::default()),
            queued: AtomicU64::new(0),
            promised: AtomicU64::new(0),
            measures: AtomicU64::new(0),
            #[cfg(test)]
            walking: Mutex::new(None),
        })
    }

    /// The quota in effect (`None`: unlimited).
    pub(crate) fn limit(&self) -> Option<u64> {
        match *self.own.read() {
            Some(0) => None,
            Some(n) => Some(n),
            None => self.default,
        }
    }

    /// On-disk bytes of the directory, walked again when the last walk is older than
    /// a second.
    pub(crate) fn used(&self) -> u64 {
        let Some(root) = &self.root else { return 0 };
        let mut m = self.measured.lock();
        if m.at.is_none_or(|t| t.elapsed() >= MAX_AGE) {
            #[cfg(test)]
            if let Some(hook) = self.walking.lock().take() {
                hook();
            }
            let building = m.building_bytes();
            m.physical_bytes = dir_size(root).saturating_sub(building);
            m.at = Some(Instant::now());
            // counted once the walk is done, so that a fence that read the count while
            // this walk was reading the directory sees it change
            self.measures.fetch_add(1, Ordering::Relaxed);
        }
        m.physical_bytes
            .saturating_sub(m.padding)
            .saturating_add(self.queued.load(Ordering::Relaxed))
    }

    /// How many walks of the directory have finished, so that a group commit fence can
    /// tell whether a walk saw part of its growth. A walk that finishes after the fence
    /// read this count may have seen it, and a walk that finished before cannot have.
    pub(crate) fn measures(&self) -> u64 {
        self.measures.load(Ordering::Relaxed)
    }

    /// Measure the directory again on the next use: a walk during a fence may have seen
    /// the fence's growth, which the fence then counted again.
    #[cfg(test)]
    pub(crate) fn remeasure(&self) {
        self.measured.lock().at = None;
    }

    /// Run `hook` inside the next directory walk, while it holds the measurement's lock
    /// and before it reads the directory (for tests).
    #[cfg(test)]
    pub(crate) fn on_next_walk(&self, hook: impl FnOnce() + Send + 'static) {
        *self.walking.lock() = Some(Box::new(hook));
    }

    /// The bytes of records and terms that queued grouped commits will write.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> u64 {
        self.queued.load(Ordering::Relaxed)
    }

    /// The bytes that queued grouped commits and the preallocations of running fences
    /// will add to the disk, which the free-space reserve counts. A commit reserves its
    /// bytes, and a fence promises its preallocation, before checking the reserve with
    /// this sum, so of two that race the later check sees both. The increments and these
    /// loads are sequentially consistent, which that argument needs on a weakly ordered
    /// CPU: with relaxed ones, each could read the other's counter before its increment.
    pub(crate) fn unwritten(&self) -> u64 {
        self.queued
            .load(Ordering::SeqCst)
            .saturating_add(self.promised.load(Ordering::SeqCst))
    }

    /// A group commit fence may preallocate up to `bytes` after its prefix. The
    /// free-space reserve counts them until the promise is dropped, after the fence's
    /// write, but the quota does not, since it never counts preallocated space.
    pub(crate) fn promise(self: &std::sync::Arc<Self>, bytes: u64) -> Promise {
        self.promised.fetch_add(bytes, Ordering::SeqCst);
        Promise {
            quota: self.clone(),
            bytes,
        }
    }

    /// A grouped commit queued `bytes` of records and new terms for its fence to write.
    /// The quota counts them until the fence has written them, or until the reservation
    /// is dropped with a commit that is never written.
    pub(crate) fn reserve(self: &std::sync::Arc<Self>, bytes: u64) -> Reservation {
        self.queued.fetch_add(bytes, Ordering::SeqCst);
        Reservation {
            quota: self.clone(),
            bytes: AtomicU64::new(bytes),
        }
    }

    /// A fence wrote the commits of `reservations`: their bytes leave the queued count as
    /// the files' growth `grew` enters the measured one, with the new preallocated space
    /// of the log, the vocabulary and, when the fence wrote annotation records, the
    /// annotations file. One step under the measurement's lock, so that no use of the
    /// quota counts the prefix twice or not at all.
    pub(crate) fn written<'a>(
        &self,
        store: Uuid,
        reservations: impl IntoIterator<Item = &'a Reservation>,
        grew: u64,
        generation_padding: u64,
        annotation_padding: Option<u64>,
        measures_before: u64,
    ) {
        let mut measured = self.measured.lock();
        let released: u64 = reservations.into_iter().map(Reservation::take).sum();
        self.dequeue(released);
        if measured.update_padding(store, |pad| {
            pad.generation = generation_padding;
            if let Some(bytes) = annotation_padding {
                pad.annotations = bytes;
            }
        }) {
            self.account_growth(&mut measured, grew, measures_before);
        }
    }

    fn dequeue(&self, bytes: u64) {
        let mut q = self.queued.load(Ordering::Relaxed);
        while let Err(now) = self.queued.compare_exchange_weak(
            q,
            q.saturating_sub(bytes),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            q = now;
        }
    }

    /// Leave `dir` (a generation being built in the background) out of the measured
    /// size, or stop leaving one out (`None`).
    pub(crate) fn exclude(&self, store: Uuid, dir: Option<&Path>) {
        let mut measured = self.measured.lock();
        if let Some(pad) = measured.stores.get_mut(&store) {
            pad.building = dir.map(Path::to_path_buf);
            measured.at = None;
        }
    }

    /// The next [`used`](Self::used) walks the directory (after a rebuild).
    pub(crate) fn invalidate(&self) {
        self.measured.lock().at = None;
    }

    /// Writable open truncates the previous writer's unused tails.
    pub(super) fn register_store(&self, store: Uuid) {
        let mut measured = self.measured.lock();
        if let Some(old) = measured.stores.insert(store, StoreUsage::default()) {
            measured.padding -= old.total();
        }
        measured.at = None;
    }

    /// Drop a deleted branch's credit before unlinking its files. Retained handles
    /// can still finish work but cannot update the dataset's measured size.
    pub(super) fn unregister_store(&self, store: Uuid) {
        let mut measured = self.measured.lock();
        if let Some(old) = measured.stores.remove(&store) {
            measured.padding -= old.total();
        }
        measured.at = None;
    }

    pub(super) fn set_generation_padding(&self, store: Uuid, bytes: u64, adopted: Option<&Path>) {
        let mut measured = self.measured.lock();
        if measured.update_padding(store, |pad| {
            pad.generation = bytes;
            if adopted.is_some() && adopted == pad.building.as_deref() {
                pad.building = None;
            }
        }) {
            measured.at = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn ann_preallocated(&self) -> u64 {
        self.measured
            .lock()
            .stores
            .values()
            .map(|p| p.annotations)
            .sum()
    }

    /// Charge an append and its new padding together. Rewrites, removals, and
    /// failures invalidate the walk rather than estimating a size change.
    fn account_padding(
        &self,
        store: Uuid,
        update: impl FnOnce(&mut StoreUsage),
        growth: Option<(u64, u64)>,
    ) {
        let mut measured = self.measured.lock();
        if measured.update_padding(store, update) {
            if let Some((bytes, measures_before)) = growth {
                self.account_growth(&mut measured, bytes, measures_before);
            } else {
                measured.at = None;
            }
        }
    }

    pub(super) fn account_annotations(&self, store: Uuid, bytes: u64, growth: Option<(u64, u64)>) {
        self.account_padding(store, |pad| pad.annotations = bytes, growth);
    }

    pub(super) fn account_changes(&self, store: Uuid, bytes: u64, growth: Option<(u64, u64)>) {
        self.account_padding(store, |pad| pad.changes = bytes, growth);
    }

    /// Account for an append outside the WAL fence. A walk that finished since
    /// the append began may already include it, so refresh the cache in that case.
    pub(super) fn account_append(&self, store: Uuid, bytes: u64, measures_before: u64) {
        let mut measured = self.measured.lock();
        if measured.stores.contains_key(&store) {
            self.account_growth(&mut measured, bytes, measures_before);
        }
    }

    fn account_growth(&self, measured: &mut Measurement, bytes: u64, measures_before: u64) {
        if self.measures() == measures_before {
            measured.physical_bytes += bytes;
        } else {
            measured.at = None;
        }
    }

    fn exceeded(limit: u64, requested: u64) -> Error {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::DatasetBytes,
            limit,
            requested,
        })
    }

    /// Check the quota before a small commit that inserts quads.
    pub(crate) fn check_commit(&self, pending_bytes: u64) -> Result<()> {
        let Some(limit) = self.limit() else {
            return Ok(());
        };
        let projected = self.used().saturating_add(pending_bytes);
        if projected > limit {
            return Err(Self::exceeded(limit, projected));
        }
        Ok(())
    }

    /// Before a rebuild that commits (a bulk load or replace) publishes the generation
    /// it built in `new`. Once published, the dataset takes the directory as it is now,
    /// less the generation `old` that `new` replaces. Before, it takes the directory less
    /// `new`. The rebuild is refused only when it would be over the quota and larger than
    /// before, so a replace that shrinks a dataset over its quota goes through. Both
    /// sizes come from walks made here. The cached measurement of [`used`](Self::used) is
    /// not used, since a walk made during a long build would count `new` in it.
    pub(crate) fn check_rebuild(
        &self,
        generation_store: Option<Uuid>,
        old: Option<&Path>,
        new: &Path,
    ) -> Result<()> {
        let Some((limit, before, projected)) = self.project_rebuild(generation_store, old, new)
        else {
            return Ok(());
        };
        if projected > limit && projected > before {
            return Err(Self::exceeded(limit, projected));
        }
        Ok(())
    }

    /// For a preview of a small commit: the quota, the bytes used, and the bytes after
    /// writing its pending records (`None` without a quota).
    pub(crate) fn project_commit(&self, pending_bytes: u64) -> Option<(u64, u64, u64)> {
        let limit = self.limit()?;
        let used = self.used();
        Some((limit, used, used.saturating_add(pending_bytes)))
    }

    /// For a preview of a rebuild: the quota, the bytes used before the generation in
    /// `new` was built, and the bytes once it replaced `old`, as
    /// [`check_rebuild`](Self::check_rebuild) measures them (`None` without a quota).
    pub(crate) fn project_rebuild(
        &self,
        generation_store: Option<Uuid>,
        old: Option<&Path>,
        new: &Path,
    ) -> Option<(u64, u64, u64)> {
        let (Some(limit), Some(root)) = (self.limit(), &self.root) else {
            return None;
        };
        let measured = self.measured.lock();
        let now = dir_size(root)
            .saturating_sub(measured.building_bytes())
            .saturating_sub(measured.padding);
        let old = old.map_or(0, |path| {
            let padding = generation_store
                .and_then(|store| measured.stores.get(&store))
                .map_or(0, |p| p.generation);
            dir_size(path).saturating_sub(padding)
        });
        Some((
            limit,
            now.saturating_sub(dir_size(new)),
            now.saturating_sub(old),
        ))
    }

    pub(crate) fn status(&self) -> QuotaStatus {
        let own = *self.own.read();
        QuotaStatus {
            max_bytes: self.limit(),
            source: if own.is_some() {
                QuotaSource::Dataset
            } else {
                QuotaSource::Default
            },
            default_max_bytes: self.default,
            used_bytes: self.used(),
        }
    }

    /// Set the dataset's own quota (`Some(0)`: unlimited) or remove it (`None`: back to
    /// the default), durably.
    pub(crate) fn set(&self, own: Option<u64>) -> Result<()> {
        let Some(root) = &self.root else {
            return Err(Error::invalid(
                "a storage quota needs a persistent dataset (in-memory datasets have --max-mem-dataset-mb)",
            ));
        };
        let mut cur = self.own.write();
        let path = root.join(QUOTA_FILE);
        match own {
            Some(n) => write_atomic(
                &path,
                &serde_json::to_vec_pretty(&QuotaFile {
                    format: 1,
                    max_bytes: n,
                })
                .map_err(|e| Error::invalid(e.to_string()))?,
            )?,
            None => match std::fs::remove_file(&path) {
                Ok(()) => crate::store::sync_dir(root)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            },
        }
        *cur = own;
        Ok(())
    }
}

fn read_file(root: &Path) -> Result<Option<u64>> {
    let bytes = match std::fs::read(root.join(QUOTA_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("{QUOTA_FILE}: {e}")))?;
    crate::formats::check_settings(QUOTA_FILE, &v)?;
    let f: QuotaFile =
        serde_json::from_value(v).map_err(|e| Error::Corrupt(format!("{QUOTA_FILE}: {e}")))?;
    Ok(Some(f.max_bytes))
}

impl Store {
    /// On-disk bytes of the dataset directory (0 for an in-memory store), measured at
    /// most once a second and kept up to date by commits in between. Cheaper than
    /// [`disk_bytes`](Store::disk_bytes), which walks the directory on every call.
    pub fn disk_usage(&self) -> u64 {
        self.quota.used()
    }

    /// The storage quota in effect and the bytes the dataset uses.
    pub fn quota(&self) -> QuotaStatus {
        self.quota.status()
    }

    /// Give the dataset a quota of its own (`Some(0)`: unlimited), or remove it
    /// (`None`), so that [`StoreOptions::max_disk_bytes`](super::StoreOptions::max_disk_bytes)
    /// applies. Kept in `quota.json` in the dataset directory. Fails for an in-memory
    /// store.
    pub fn set_quota(&self, max_bytes: Option<u64>) -> Result<QuotaStatus> {
        // the writer lock orders the change after any commit being checked
        let _w = self.guarded_writer();
        self.quota.set(max_bytes)?;
        Ok(self.quota.status())
    }
}

/// Bytes of a queued grouped commit that the quota counts before its fence writes them
/// (see [`Quota::reserve`]). Dropping it releases what was not written.
pub(crate) struct Reservation {
    quota: std::sync::Arc<Quota>,
    bytes: AtomicU64,
}

impl Reservation {
    fn take(&self) -> u64 {
        self.bytes.swap(0, Ordering::Relaxed)
    }
}

/// Space a fence may preallocate (see [`Quota::promise`]).
pub(crate) struct Promise {
    quota: std::sync::Arc<Quota>,
    bytes: u64,
}

impl Drop for Promise {
    fn drop(&mut self) {
        self.quota.promised.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let bytes = self.take();
        if bytes > 0 {
            self.quota.dequeue(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::store::{QuotaSource, ReplaceTarget, Store, StoreOptions};
    use crate::{BudgetKind, Error};

    fn ttl(n: usize, tag: &str) -> Source {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!(
                "<http://ex.org/{tag}{i}> <http://ex.org/p> \"value {tag} {i} with some padding\" .\n"
            ));
        }
        Source::from_bytes(s.into_bytes(), RdfFormat::Turtle, None)
    }

    fn quota_err(e: Error) -> (u64, u64) {
        match e {
            Error::BudgetExceeded(b) if b.kind == BudgetKind::DatasetBytes => {
                (b.limit, b.requested)
            }
            e => panic!("expected a dataset-bytes budget error, got {e}"),
        }
    }

    #[test]
    fn small_commits_stop_at_the_quota_and_deletes_go_through() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        s.load(&[ttl(10, "a")]).unwrap();
        let st = s.quota();
        assert_eq!((st.max_bytes, st.source), (None, QuotaSource::Default));
        assert!(st.used_bytes > 0);
        // a quota just above what the dataset uses now
        let used = s.disk_usage();
        s.set_quota(Some(used + 200)).unwrap();
        assert_eq!(s.quota().source, QuotaSource::Dataset);
        let mut refused = None;
        for i in 0..50 {
            let before = s.snapshot().len();
            match s.load(&[ttl(1, &format!("b{i}-"))]) {
                Ok(_) => {}
                Err(e) => {
                    let (limit, requested) = quota_err(e);
                    assert_eq!(limit, used + 200);
                    assert!(requested > limit);
                    assert_eq!(s.snapshot().len(), before, "nothing was committed");
                    refused = Some(i);
                    break;
                }
            }
        }
        assert!(refused.is_some(), "the quota never stopped a commit");
        // a commit that only deletes is not refused, and neither is a compaction
        let del = "DELETE WHERE { <http://ex.org/a0> ?p ?o }";
        crate::sparql::update::update(&s, del, &Default::default()).unwrap();
        s.compact().unwrap();
        // the quota survives a reopen; removing it brings the default back
        drop(s);
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        assert_eq!(s.quota().max_bytes, Some(used + 200));
        s.set_quota(None).unwrap();
        assert_eq!(s.quota().source, QuotaSource::Default);
        s.load(&[ttl(5, "c")]).unwrap();
        // unlimited on the dataset beats a default
        drop(s);
        let opts = StoreOptions {
            max_disk_bytes: Some(1),
            ..Default::default()
        };
        let s = Store::open(dir.path(), opts).unwrap();
        quota_err(s.load(&[ttl(1, "d")]).unwrap_err());
        s.set_quota(Some(0)).unwrap();
        assert_eq!(s.quota().max_bytes, None);
        s.load(&[ttl(1, "d")]).unwrap();
    }

    #[test]
    fn rebuilds_are_checked_before_they_are_published() {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOptions {
            bulk_threshold: 100,
            ..Default::default()
        };
        let s = Store::open(dir.path(), opts).unwrap();
        s.load(&[ttl(500, "a")]).unwrap();
        s.flush_change_log().unwrap();
        let used = s.disk_usage();
        s.set_quota(Some(used + used / 2)).unwrap();
        let commit = s.head_commit().seq;
        // A bulk load that would more than double the dataset is built, then refused.
        // It is refused even when the size measured above is taken again after the new
        // generation is written, as happens when the build takes over a second.
        s.quota.invalidate();
        let e = s.load(&[ttl(5000, "b")]).unwrap_err();
        quota_err(e);
        assert_eq!(s.head_commit().seq, commit);
        assert_eq!(s.snapshot().len(), 500);
        // the unpublished generation's files are gone
        assert!(s.disk_usage() <= used + used / 2);
        // a replace that shrinks the dataset goes through, even over the quota
        s.set_quota(Some(1)).unwrap();
        let n = s.replace(ReplaceTarget::Default, &[ttl(200, "c")]).unwrap();
        assert_eq!(n, 200);
        assert_eq!(s.snapshot().len(), 200);
        // in memory there is no quota
        let m = Store::in_memory(StoreOptions::default());
        assert!(m.set_quota(Some(1)).is_err());
        assert_eq!(m.quota().used_bytes, 0);
    }

    #[test]
    fn deleted_store_reports_release_reservations_without_changing_usage() {
        let dir = tempfile::tempdir().unwrap();
        let quota = std::sync::Arc::new(Quota::open(Some(dir.path()), None).unwrap());
        let id = Uuid::new_v4();
        quota.register_store(id);
        let before = quota.used();
        let reservation = quota.reserve(100);
        assert_eq!(quota.used(), before + 100);

        quota.unregister_store(id);
        assert_eq!(quota.used(), before + 100);
        let measures = quota.measures();
        quota.written(id, [&reservation], 10_000, 3_000, Some(500), measures);
        quota.account_changes(id, 1_000, Some((2_000, measures)));
        assert_eq!(quota.queued(), 0);
        assert_eq!(quota.used(), before);
    }

    fn insert(s: &Store, tag: &str) -> Result<()> {
        let update = format!("INSERT DATA {{ <urn:{tag}> <urn:p> \"{tag}\" }}");
        crate::sparql::update::update(s, &update, &Default::default()).map(|_| ())
    }

    fn generation_padding(s: &Store) -> (u64, u64) {
        let view = s.snapshot();
        let dir = view.generation.dir.as_ref().unwrap();
        let length = |file| dir.join(file).metadata().unwrap().len();
        let wal = length("wal.log") - s.wal_bytes();
        let vocab = length("delta.vocab") - view.generation.dvocab.file_state().0;
        (wal, vocab)
    }

    fn change_padding(s: &Store) -> u64 {
        let path = s.root.as_ref().unwrap().join(super::super::CHANGES_DIR);
        let logs = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .map(|e| e.unwrap());
        let allocated: u64 = logs
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
            .map(|e| e.metadata().unwrap().len())
            .sum();
        allocated - s.change_log_status().unwrap().bytes
    }

    fn assert_quota_usage(main: &Store, stores: &[&Store]) {
        let padding: u64 = stores
            .iter()
            .map(|s| {
                let (wal, vocab) = generation_padding(s);
                wal + vocab + change_padding(s)
            })
            .sum();
        main.quota.invalidate();
        assert_eq!(main.disk_usage(), main.disk_bytes() - padding);
    }

    #[test]
    fn branch_preallocation_quota_tracks_each_store_through_compaction_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOptions {
            wal_prealloc_bytes: 128 << 10,
            change_log: false,
            ..Default::default()
        };
        let main = Store::open(dir.path(), opts.clone()).unwrap();
        insert(&main, "main-first").unwrap();
        main.create_branch("left", &Default::default()).unwrap();
        main.create_branch("right", &Default::default()).unwrap();
        let left = main.branch("left").unwrap();
        let right = main.branch("right").unwrap();
        assert_quota_usage(&main, &[&main, &left, &right]);

        std::thread::scope(|scope| {
            let write_left = scope.spawn(|| insert(&left, "left-first"));
            let write_right = scope.spawn(|| insert(&right, "right-first"));
            write_left.join().unwrap().unwrap();
            write_right.join().unwrap().unwrap();
        });
        for store in [&main, &*left, &*right] {
            let (wal, vocab) = generation_padding(store);
            assert!(wal > 0 && vocab > 0, "WAL {wal}, vocabulary {vocab}");
        }
        assert_quota_usage(&main, &[&main, &left, &right]);

        // A rebuild replaces its own generation at its logical size.
        main.set_quota(Some(u64::MAX)).unwrap();
        main.quota.invalidate();
        let used = main.disk_usage();
        let live = left.snapshot();
        let old = live.generation.dir.as_deref().unwrap();
        let staging = dir.path().join("quota-test-rebuild");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("data"), [0; 128]).unwrap();
        let (_, before, projected) = main
            .quota
            .project_rebuild(Some(left.dataset_id()), Some(old), &staging)
            .unwrap();
        let (wal, vocab) = generation_padding(&left);
        assert_eq!(before, used);
        assert_eq!(projected, used + 128 - (dir_size(old) - wal - vocab));
        std::fs::remove_dir_all(staging).unwrap();
        main.set_quota(None).unwrap();

        let right_padding = generation_padding(&right);
        left.compact().unwrap();
        assert_eq!(generation_padding(&left), (0, 0));
        assert_eq!(generation_padding(&right), right_padding);
        assert_quota_usage(&main, &[&main, &left, &right]);
        insert(&left, "left-after-compaction").unwrap();
        insert(&right, "right-second").unwrap();
        assert_quota_usage(&main, &[&main, &left, &right]);

        main.delete_branch("left", true).unwrap();
        assert!(!left.root.as_ref().unwrap().exists());
        assert_quota_usage(&main, &[&main, &right]);
        drop(left);
        assert_quota_usage(&main, &[&main, &right]);
        drop(right);
        drop(main);

        let reopened = Store::open(dir.path(), opts).unwrap();
        assert_eq!(reopened.disk_usage(), reopened.disk_bytes());
        let right = reopened.branch("right").unwrap();
        insert(&right, "right-reopened").unwrap();
        assert_quota_usage(&reopened, &[&reopened, &right]);
    }

    #[test]
    fn change_log_preallocation_quota_tracks_roll_reopen_disable_and_branch_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let options = StoreOptions {
            wal_prealloc_bytes: 0,
            change_log_segment_bytes: 4096,
            ..Default::default()
        };
        let main = Store::open(dir.path(), options.clone()).unwrap();
        main.change_log().unwrap().set_background(false);
        insert(&main, "small").unwrap();
        main.flush_change_log().unwrap();
        let padding = change_padding(&main);
        assert!(padding > 32 << 10, "open segment padding {padding}");
        assert_quota_usage(&main, &[&main]);

        let limit = main.disk_usage() + (16 << 10);
        main.set_quota(Some(limit)).unwrap();
        assert!(main.disk_bytes() > limit);
        insert(&main, "fits-below-quota").unwrap();
        main.flush_change_log().unwrap();
        assert_quota_usage(&main, &[&main]);

        // This record rolls the segment. The index and all records count in the quota.
        insert(&main, &"large".repeat(600)).unwrap();
        main.flush_change_log().unwrap();
        assert_eq!(change_padding(&main), 0);
        assert_quota_usage(&main, &[&main]);

        insert(&main, "after-roll").unwrap();
        main.flush_change_log().unwrap();
        let status = main.change_log_status().unwrap();
        assert_eq!(status.segments, 2);
        assert!(change_padding(&main) > 32 << 10);
        assert_quota_usage(&main, &[&main]);

        // Copy the durable but still padded segment, as it would be after a restart
        // that occurred before clean shutdown could trim it.
        let restart = tempfile::tempdir().unwrap();
        super::super::compaction_tests::copy_dir(dir.path(), restart.path());
        let recovered = Store::open(restart.path(), options).unwrap();
        assert_eq!(recovered.change_log_status().unwrap().bytes, status.bytes);
        assert_eq!(recovered.change_log_status().unwrap().last, status.last);
        assert_eq!(change_padding(&recovered), 0);
        assert_quota_usage(&recovered, &[&recovered]);

        main.set_quota(None).unwrap();
        main.create_branch("dev", &Default::default()).unwrap();
        let dev = main.branch("dev").unwrap();
        dev.change_log().unwrap().set_background(false);
        insert(&dev, "on-branch").unwrap();
        dev.flush_change_log().unwrap();
        assert!(change_padding(&dev) > 32 << 10);
        assert_quota_usage(&main, &[&main, &dev]);
        main.delete_branch("dev", true).unwrap();
        assert_quota_usage(&main, &[&main]);
        drop(dev);

        main.set_change_log_settings(super::super::ChangeLogSettings {
            enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(change_padding(&main), 0);
        assert_quota_usage(&main, &[&main]);
    }

    #[test]
    fn tight_dataset_quota_admits_real_growth_and_refuses_excess_across_branches() {
        let dir = tempfile::tempdir().unwrap();
        let main = Store::open(dir.path(), StoreOptions::default()).unwrap();
        insert(&main, "seed").unwrap();
        main.create_branch("dev", &Default::default()).unwrap();
        let dev = main.branch("dev").unwrap();
        insert(&dev, "dev-seed").unwrap();
        main.flush_change_log().unwrap();
        dev.flush_change_log().unwrap();
        assert_quota_usage(&main, &[&main, &dev]);
        let limit = main.disk_usage() + (16 << 10);
        main.set_quota(Some(limit)).unwrap();
        assert!(main.disk_bytes() > limit);

        insert(&main, "main-fits").unwrap();
        insert(&dev, "dev-fits").unwrap();
        let head = dev.head_commit();
        let (refused_limit, wanted) = quota_err(insert(&dev, &"x".repeat(32 << 10)).unwrap_err());
        assert_eq!(refused_limit, limit);
        assert!(wanted > limit);
        assert_eq!(dev.head_commit().seq, head.seq);
    }

    #[test]
    fn a_newer_quota_file_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(QUOTA_FILE),
            br#"{"format": 2, "maxBytes": 1}"#,
        )
        .unwrap();
        let e = read_file(dir.path()).expect_err("refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    }
}
