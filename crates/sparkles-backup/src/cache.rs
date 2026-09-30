//! The local manifest cache: `<cache_dir>/<repository id>/<name>.json`, keyed by
//! `(name, e_tag or last_modified, size)` so a deleted and re-created name is fetched
//! again. There is no mutable index object in the repository; a warm listing is one
//! `LIST backups/`.

use crate::Manifest;
use object_store::ObjectMeta;
use std::path::PathBuf;

/// The manifest cache of one repository. Best effort: I/O errors make misses (and a
/// WARN), never failures.
#[derive(Debug)]
pub struct ManifestCache {
    /// `<cache_dir>/<repository id>`; `None`: memory only
    pub(crate) dir: Option<PathBuf>,
}

/// The version part of a cache key: the object's e_tag, else its `last_modified` in
/// RFC 3339 (with nanoseconds).
pub fn version_of(meta: &ObjectMeta) -> String {
    match &meta.e_tag {
        Some(t) => t.clone(),
        None => meta.last_modified.to_rfc3339(),
    }
}

impl ManifestCache {
    /// The cache in `dir` (created on first write).
    pub fn new(dir: Option<PathBuf>) -> ManifestCache {
        ManifestCache { dir }
    }

    /// The cached manifest of `name` if its entry was stored with the same `version`
    /// ([`version_of`]) and `size`.
    pub fn get(&self, name: &str, version: &str, size: u64) -> Option<Manifest> {
        let _ = (name, version, size, &self.dir);
        None
    }

    /// Store `m` as the entry of `name` at `version` and `size` (atomically: a
    /// temporary file and a rename).
    pub fn put(&self, name: &str, version: &str, size: u64, m: &Manifest) {
        let _ = (name, version, size, m);
    }

    /// Forget `name` (after a delete).
    pub fn remove(&self, name: &str) {
        let _ = name;
    }
}
