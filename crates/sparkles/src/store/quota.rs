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
//! set, and after every rebuild. Between walks, small commits add the bytes they append
//! to the write-ahead log. A rebuild is checked with walks of its own.

use super::{Store, dir_size, write_atomic};
use crate::error::{Budget, BudgetKind, Error, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

pub(crate) struct Quota {
    root: Option<PathBuf>,
    default: Option<u64>,
    /// the dataset's own quota (`Some(0)`: unlimited)
    own: RwLock<Option<u64>>,
    used: AtomicU64,
    /// when `used` was last measured by a walk (`None`: never, or invalidated)
    measured: Mutex<Option<Instant>>,
    /// a generation a background compaction is building: not counted, so that a
    /// compaction never makes the dataset refuse writes
    excluded: Mutex<Option<PathBuf>>,
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
            used: AtomicU64::new(0),
            measured: Mutex::new(None),
            excluded: Mutex::new(None),
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
        if m.is_none_or(|t| t.elapsed() >= MAX_AGE) {
            let building = self.excluded.lock().as_deref().map_or(0, dir_size);
            self.used
                .store(dir_size(root).saturating_sub(building), Ordering::Relaxed);
            *m = Some(Instant::now());
        }
        self.used.load(Ordering::Relaxed)
    }

    /// Leave `dir` (a generation being built in the background) out of the measured
    /// size, or stop leaving one out (`None`).
    pub(crate) fn exclude(&self, dir: Option<&Path>) {
        *self.excluded.lock() = dir.map(Path::to_path_buf);
        self.invalidate();
    }

    /// The next [`used`](Self::used) walks the directory (after a rebuild).
    pub(crate) fn invalidate(&self) {
        *self.measured.lock() = None;
    }

    /// A small commit appended `bytes` to the write-ahead log.
    pub(crate) fn add(&self, bytes: u64) {
        if self.root.is_some() {
            self.used.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    fn exceeded(limit: u64, requested: u64) -> Error {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::DatasetBytes,
            limit,
            requested,
        })
    }

    /// Before a small commit that inserts quads appends `wal_bytes` to the log.
    pub(crate) fn check_commit(&self, wal_bytes: u64) -> Result<()> {
        let Some(limit) = self.limit() else {
            return Ok(());
        };
        let projected = self.used().saturating_add(wal_bytes);
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
    pub(crate) fn check_rebuild(&self, old: Option<&Path>, new: &Path) -> Result<()> {
        let (Some(limit), Some(root)) = (self.limit(), &self.root) else {
            return Ok(());
        };
        let building = self.excluded.lock().as_deref().map_or(0, dir_size);
        let now = dir_size(root).saturating_sub(building);
        let before = now.saturating_sub(dir_size(new));
        let projected = now.saturating_sub(old.map_or(0, dir_size));
        if projected > limit && projected > before {
            return Err(Self::exceeded(limit, projected));
        }
        Ok(())
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
    let f: QuotaFile =
        serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("{QUOTA_FILE}: {e}")))?;
    if f.format != 1 {
        return Err(Error::Corrupt(format!(
            "{QUOTA_FILE} has format {}, this build reads 1",
            f.format
        )));
    }
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
        let _w = self.writer.lock();
        self.quota.set(max_bytes)?;
        Ok(self.quota.status())
    }
}

#[cfg(test)]
mod tests {
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
}
