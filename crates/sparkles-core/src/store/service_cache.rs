//! A dataset's setting for its cache of remote SERVICE results (spec G10, roadmap item
//! 38): whether a persistent dataset keeps the entries on disk too, how much disk they
//! may take, and how long an entry answers. A persistent store keeps a setting other than
//! the defaults in `service-cache.json`, and applies it when it opens and when it is set.
//! An in-memory store applies only the time to live.

use super::{Store, sync_dir, write_atomic};
use crate::error::{Error, Result};
use crate::sparql::svcdisk::{self, DiskTier};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The file in the dataset directory that holds the setting.
pub const SERVICE_CACHE_FILE: &str = "service-cache.json";

/// A dataset's setting for its cache of remote SERVICE results. An unset member follows
/// the store's options ([`StoreOptions::service_cache_disk_bytes`] and
/// [`StoreOptions::service_cache_ttl`]).
///
/// [`StoreOptions::service_cache_disk_bytes`]: super::StoreOptions::service_cache_disk_bytes
/// [`StoreOptions::service_cache_ttl`]: super::StoreOptions::service_cache_ttl
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCacheSettings {
    /// Keep the entries in `service-cache/` under the dataset's directory too.
    #[serde(default)]
    pub disk: bool,
    /// The MiB the files may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_mb: Option<u64>,
    /// The seconds an entry answers after it was stored, in memory and on disk (0: until
    /// it is evicted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
}

impl ServiceCacheSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The SERVICE cache setting in force for a store, with the defaults applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCacheStatus {
    /// The dataset's own setting.
    pub setting: ServiceCacheSettings,
    /// Whether the entries are kept on disk.
    pub disk: bool,
    /// The bytes the files may take, and take.
    pub disk_capacity_bytes: u64,
    pub disk_bytes: u64,
    pub disk_entries: usize,
    /// The seconds an entry answers (0: until it is evicted).
    pub ttl_seconds: u64,
}

impl Store {
    /// The dataset's SERVICE cache setting (the defaults without one).
    pub fn service_cache_settings(&self) -> Result<ServiceCacheSettings> {
        match &self.root {
            Some(root) => read_settings(root),
            None => Ok(self.results.service.memory_settings()),
        }
    }

    /// The setting with the defaults applied, and what the disk tier holds.
    pub fn service_cache_status(&self) -> Result<ServiceCacheStatus> {
        let setting = self.service_cache_settings()?;
        let disk = self.results.service.disk();
        Ok(ServiceCacheStatus {
            setting,
            disk: disk.is_some(),
            disk_capacity_bytes: disk.as_ref().map_or(0, |d| d.capacity()),
            disk_bytes: disk.as_ref().map_or(0, |d| d.bytes()),
            disk_entries: disk.as_ref().map_or(0, |d| d.entries()),
            ttl_seconds: self.results.service.ttl().as_secs(),
        })
    }

    /// Replace the dataset's SERVICE cache setting (`None` or the defaults: remove it).
    /// The setting is checked, and a disk tier it calls for is opened, before a
    /// persistent store writes `service-cache.json`, so a setting that is refused leaves
    /// the store and its file as they were. Turning the disk tier off removes its files.
    /// A store that keeps no SERVICE cache files
    /// ([`StoreOptions::service_cache_files`](super::StoreOptions::service_cache_files))
    /// refuses a setting.
    pub fn set_service_cache_settings(&self, s: Option<ServiceCacheSettings>) -> Result<()> {
        let s = s.unwrap_or_default();
        let staged = self.stage_service_cache_settings(&s)?;
        if let Some(root) = &self.root {
            if !self.opts.service_cache_files {
                return Err(Error::NotPermitted(
                    "the store is read-only and keeps its SERVICE cache setting as it is".into(),
                ));
            }
            let tier = self.open_service_cache_tier(&staged)?;
            let path = root.join(SERVICE_CACHE_FILE);
            if s.is_default() {
                match std::fs::remove_file(&path) {
                    Ok(()) => sync_dir(root)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                write_atomic(&path, &settings_bytes(&s))?;
            }
            self.install_service_cache_settings(&staged, tier)
        } else {
            self.results.service.set_memory_settings(s.clone());
            self.install_service_cache_settings(&staged, None)
        }
    }

    /// Apply the setting a store reads when it opens. A disk tier that cannot be opened
    /// is left out with a warning, since its entries are a cache.
    pub(super) fn apply_service_cache_settings(&self, s: &ServiceCacheSettings) -> Result<()> {
        let staged = self.stage_service_cache_settings(s)?;
        let tier = match self.open_service_cache_tier(&staged) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "the SERVICE cache's disk tier is left out");
                None
            }
        };
        self.install_service_cache_settings(&staged, tier)
    }

    /// Check `s` and work out what it applies, changing nothing.
    fn stage_service_cache_settings(&self, s: &ServiceCacheSettings) -> Result<Staged> {
        let ttl = s
            .ttl_seconds
            .map_or(self.opts.service_cache_ttl, Duration::from_secs);
        let cap = match s.disk_mb {
            Some(mb) => mb.checked_mul(1 << 20).ok_or_else(|| {
                Error::invalid(format!("{SERVICE_CACHE_FILE}: diskMb {mb} is too large"))
            })?,
            None => self.opts.service_cache_disk_bytes,
        };
        let disk = match &self.root {
            Some(root) if self.opts.service_cache_files => {
                let dir = root.join(svcdisk::DIR);
                if s.disk && cap > 0 {
                    Tier::On(dir, cap)
                } else {
                    Tier::Off(dir)
                }
            }
            _ => Tier::Untouched,
        };
        Ok(Staged { ttl, disk })
    }

    /// The disk tier `staged` calls for when it is not the one in use, opened.
    fn open_service_cache_tier(&self, staged: &Staged) -> Result<Option<Arc<DiskTier>>> {
        let Tier::On(dir, cap) = &staged.disk else {
            return Ok(None);
        };
        let cache = &self.results.service;
        if cache
            .disk()
            .is_some_and(|d| d.dir() == dir && d.capacity() == *cap)
        {
            return Ok(None);
        }
        Ok(Some(Arc::new(DiskTier::open(dir.clone(), *cap)?)))
    }

    /// Apply `staged`, with `tier` when a new disk tier was opened for it. Turning the
    /// tier off removes its directory.
    fn install_service_cache_settings(
        &self,
        staged: &Staged,
        tier: Option<Arc<DiskTier>>,
    ) -> Result<()> {
        let cache = &self.results.service;
        cache.set_ttl(staged.ttl);
        match &staged.disk {
            Tier::Untouched => {}
            Tier::On(..) => {
                if let Some(t) = tier {
                    cache.set_disk(Some(t));
                }
            }
            Tier::Off(dir) => {
                if cache.disk().is_some() {
                    cache.set_disk(None);
                }
                match std::fs::remove_dir_all(dir) {
                    Ok(()) => {
                        if let Some(root) = &self.root {
                            sync_dir(root)?;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }
}

/// What a checked setting applies.
struct Staged {
    ttl: Duration,
    disk: Tier,
}

/// What a setting does to the disk tier.
enum Tier {
    /// an in-memory or read-only store: nothing
    Untouched,
    /// keep the tier in this directory, with this cap
    On(std::path::PathBuf, u64),
    /// no tier, and no directory
    Off(std::path::PathBuf),
}

/// The contents of `service-cache.json` for setting `s`.
pub(crate) fn settings_bytes(s: &ServiceCacheSettings) -> Vec<u8> {
    let mut v = serde_json::to_value(s).expect("settings serialize");
    v.as_object_mut()
        .expect("an object")
        .insert("format".into(), 1.into());
    let mut bytes = serde_json::to_vec_pretty(&v).expect("json serializes");
    bytes.push(b'\n');
    bytes
}

/// Read a dataset's `service-cache.json` (the defaults without one).
pub(crate) fn read_settings(root: &Path) -> Result<ServiceCacheSettings> {
    match std::fs::read(root.join(SERVICE_CACHE_FILE)) {
        Ok(b) => {
            let v: serde_json::Value = serde_json::from_slice(&b)
                .map_err(|e| Error::Corrupt(format!("{SERVICE_CACHE_FILE}: {e}")))?;
            crate::formats::check_settings(SERVICE_CACHE_FILE, &v)?;
            serde_json::from_value(v)
                .map_err(|e| Error::Corrupt(format!("{SERVICE_CACHE_FILE}: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ServiceCacheSettings::default()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql::svccache::{Parts, Rows, Slice, key};
    use crate::store::StoreOptions;

    #[test]
    fn a_newer_service_cache_file_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SERVICE_CACHE_FILE), br#"{"format": 2}"#).unwrap();
        let e = read_settings(dir.path()).expect_err("refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    }

    fn put(s: &Store, pattern: &str) -> String {
        let k = key("scope", "http://e/sparql", pattern, &[]);
        let rows = Arc::new(Rows {
            vars: vec!["x".into()],
            rows: vec![vec![Some(oxrdf::Literal::from(1).into())]],
        });
        s.result_cache().service.put(
            k.clone(),
            Parts::of("scope", "http://e/sparql", pattern),
            Slice::ALL,
            rows,
        );
        k
    }

    /// A dataset that sets `disk` keeps its entries across a reopen, a reset removes
    /// them, and backups and clones carry neither the files nor the directory.
    #[test]
    fn disk_entries_outlive_the_store_and_stay_out_of_backups_and_clones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let opts = || StoreOptions {
            service_cache_bytes: 1 << 20,
            ..Default::default()
        };
        let k = {
            let s = Store::open(&root, opts()).unwrap();
            s.set_service_cache_settings(Some(ServiceCacheSettings {
                disk: true,
                disk_mb: Some(4),
                ttl_seconds: Some(600),
            }))
            .unwrap();
            put(&s, "p")
        };
        assert_eq!(
            std::fs::read_dir(root.join(svcdisk::DIR)).unwrap().count(),
            1
        );
        let s = Store::open(&root, opts()).unwrap();
        let st = s.service_cache_status().unwrap();
        assert!(st.disk);
        assert_eq!((st.disk_capacity_bytes, st.ttl_seconds), (4 << 20, 600));
        assert_eq!(st.disk_entries, 1);
        assert!(
            s.result_cache().service.get(&k, Slice::ALL).is_some(),
            "the entry is read back after a reopen"
        );
        // a backup and a clone leave the tier out
        let backup = dir.path().join("backup");
        s.backup_capture("t").unwrap().write_to(&backup).unwrap();
        let clone = dir.path().join("clone");
        s.clone_to(&clone, &Default::default()).unwrap();
        for d in [&backup, &clone] {
            let mut stack = vec![d.to_path_buf()];
            while let Some(p) = stack.pop() {
                for e in std::fs::read_dir(&p).unwrap() {
                    let e = e.unwrap();
                    let name = e.file_name().to_string_lossy().into_owned();
                    assert!(
                        name != svcdisk::DIR && !name.ends_with(".svc"),
                        "{} holds {name}",
                        d.display()
                    );
                    if e.file_type().unwrap().is_dir() {
                        stack.push(e.path());
                    }
                }
            }
        }
        // clearing the cache empties the tier, and a reset removes the directory
        put(&s, "q");
        s.result_cache().service.clear();
        assert_eq!(s.service_cache_status().unwrap().disk_entries, 0);
        put(&s, "r");
        s.set_service_cache_settings(None).unwrap();
        assert!(!root.join(svcdisk::DIR).exists());
        assert!(!root.join(SERVICE_CACHE_FILE).exists());
        assert!(!s.service_cache_status().unwrap().disk);
    }

    #[test]
    fn an_in_memory_store_applies_the_time_to_live_only() {
        let s = Store::in_memory(StoreOptions::default());
        assert_eq!(s.result_cache().service.ttl(), Duration::from_secs(3600));
        s.set_service_cache_settings(Some(ServiceCacheSettings {
            disk: true,
            disk_mb: None,
            ttl_seconds: Some(5),
        }))
        .unwrap();
        assert_eq!(s.result_cache().service.ttl(), Duration::from_secs(5));
        assert!(s.result_cache().service.disk().is_none());
        assert_eq!(s.service_cache_settings().unwrap().ttl_seconds, Some(5));
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            out.push((
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            ));
        }
        out.sort();
        out
    }

    /// A setting that is refused changes neither the file nor the setting in force, and
    /// the store opens again afterwards.
    #[test]
    fn a_refused_setting_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        let e = s
            .set_service_cache_settings(Some(ServiceCacheSettings {
                disk: true,
                disk_mb: Some(u64::MAX),
                ttl_seconds: Some(5),
            }))
            .expect_err("too large");
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(!root.join(SERVICE_CACHE_FILE).exists());
        assert_eq!(s.result_cache().service.ttl(), Duration::from_secs(3600));
        drop(s);
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert!(s.service_cache_settings().unwrap().is_default());
    }

    /// A store that keeps no SERVICE cache files leaves the directory as it is, byte for
    /// byte, reads none of it, and refuses a new setting.
    #[test]
    fn a_read_only_store_leaves_the_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let opts = |files| StoreOptions {
            service_cache_bytes: 1 << 20,
            service_cache_files: files,
            ..Default::default()
        };
        let k = {
            let s = Store::open(&root, opts(true)).unwrap();
            s.set_service_cache_settings(Some(ServiceCacheSettings {
                disk: true,
                disk_mb: Some(4),
                ttl_seconds: Some(600),
            }))
            .unwrap();
            put(&s, "p")
        };
        let tier = root.join(svcdisk::DIR);
        // a leftover temporary file, which a writable store would remove when it opens
        std::fs::write(tier.join(".x.tmp-1-1"), b"partial").unwrap();
        let (before, settings) = (
            snapshot(&tier),
            std::fs::read(root.join(SERVICE_CACHE_FILE)).unwrap(),
        );
        let s = Store::open(&root, opts(false)).unwrap();
        assert!(s.result_cache().service.disk().is_none());
        assert!(s.result_cache().service.get(&k, Slice::ALL).is_none());
        put(&s, "q");
        s.result_cache().service.clear();
        assert!(s.set_service_cache_settings(None).is_err());
        assert!(
            s.set_service_cache_settings(Some(ServiceCacheSettings::default()))
                .is_err()
        );
        assert_eq!(s.result_cache().service.ttl(), Duration::from_secs(600));
        drop(s);
        assert_eq!(snapshot(&tier), before);
        assert_eq!(
            std::fs::read(root.join(SERVICE_CACHE_FILE)).unwrap(),
            settings
        );
    }
}
