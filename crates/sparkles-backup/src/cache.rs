//! The local manifest cache: `<cache_dir>/<repository id>/<name>.json`, keyed by
//! `(name, e_tag or last_modified, size)` so a deleted and re-created name is fetched
//! again. There is no mutable index object in the repository; a warm listing is one
//! `LIST backups/`.
//!
//! Entries also live in memory for the life of the [`Repository`](crate::Repository),
//! so a repository without a cache directory (tests, `memory://`) still lists warm.

use crate::Manifest;
use object_store::ObjectMeta;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

/// The manifest cache of one repository. Best effort: I/O errors make misses (and a
/// WARN), never failures.
pub struct ManifestCache {
    /// `<cache_dir>/<repository id>`; `None`: memory only
    pub(crate) dir: Option<PathBuf>,
    mem: Mutex<HashMap<String, Entry>>,
    sealed: Mutex<HashMap<String, SealedEntry>>,
}

/// One cached manifest, as kept in memory and written to `<name>.json`.
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    version: String,
    size: u64,
    manifest: Manifest,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedEntry {
    version: String,
    size: u64,
    envelope: Vec<u8>,
}

impl std::fmt::Debug for ManifestCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManifestCache")
            .field("dir", &self.dir)
            .field("entries", &self.mem.lock().map(|m| m.len()).unwrap_or(0))
            .finish()
    }
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
        ManifestCache {
            dir,
            mem: Mutex::default(),
            sealed: Mutex::default(),
        }
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        // backup names are plain file names (their grammar has no `/` and no leading `.`)
        crate::layout::valid_backup_name(name)
            .then(|| self.dir.as_ref().map(|d| d.join(format!("{name}.json"))))
            .flatten()
    }

    /// The cached manifest of `name` if its entry was stored with the same `version`
    /// ([`version_of`]) and `size`.
    pub fn get(&self, name: &str, version: &str, size: u64) -> Option<Manifest> {
        let matches = |e: &Entry| e.version == version && e.size == size;
        if let Some(e) = self.mem.lock().unwrap().get(name)
            && matches(e)
        {
            return Some(e.manifest.clone());
        }
        let path = self.file(name)?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(target: "sparkles::backup", "manifest cache {}: {e}", path.display());
                return None;
            }
        };
        let e: Entry = match serde_json::from_slice(&bytes) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(target: "sparkles::backup", "manifest cache {}: {e}", path.display());
                return None;
            }
        };
        if !matches(&e) || e.manifest.name != name {
            return None;
        }
        let m = e.manifest.clone();
        self.mem.lock().unwrap().insert(name.to_string(), e);
        Some(m)
    }

    /// Store `m` as the entry of `name` at `version` and `size` (atomically: a
    /// temporary file and a rename).
    pub fn put(&self, name: &str, version: &str, size: u64, m: &Manifest) {
        let e = Entry {
            version: version.to_string(),
            size,
            manifest: m.clone(),
        };
        if let Some(path) = self.file(name)
            && let Err(err) = write_atomic(&path, &e)
        {
            tracing::warn!(target: "sparkles::backup", "manifest cache {}: {err}", path.display());
        }
        self.mem.lock().unwrap().insert(name.to_string(), e);
    }

    // The disk contains only authenticated ciphertext for an encrypted repository.
    pub(crate) fn get_sealed(&self, name: &str, version: &str, size: u64) -> Option<Vec<u8>> {
        let matches = |e: &SealedEntry| {
            e.version == version && e.size == size && e.envelope.len() as u64 == size
        };
        if let Some(e) = self.sealed.lock().unwrap().get(name).filter(|e| matches(e)) {
            return Some(e.envelope.clone());
        }
        let path = self.file(name)?;
        // JSON byte arrays can take four times their binary length.
        let limit = crate::layout::MAX_MANIFEST_BYTES.saturating_mul(8);
        let file = std::fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() > limit {
            return None;
        }
        let mut bytes = Vec::new();
        std::io::Read::take(file, limit + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > limit {
            return None;
        }
        let e: SealedEntry = serde_json::from_slice(&bytes).ok()?;
        if !matches(&e) {
            return None;
        }
        let envelope = e.envelope.clone();
        self.sealed.lock().unwrap().insert(name.to_string(), e);
        Some(envelope)
    }

    pub(crate) fn put_sealed(&self, name: &str, version: &str, size: u64, envelope: &[u8]) {
        let e = SealedEntry {
            version: version.to_string(),
            size,
            envelope: envelope.to_vec(),
        };
        if let Some(path) = self.file(name)
            && let Err(err) = write_atomic(&path, &e)
        {
            tracing::warn!(target: "sparkles::backup", "manifest cache {}: {err}", path.display());
        }
        self.sealed.lock().unwrap().insert(name.to_string(), e);
    }

    /// Forget `name` (after a delete).
    pub fn remove(&self, name: &str) {
        self.mem.lock().unwrap().remove(name);
        self.sealed.lock().unwrap().remove(name);
        if let Some(path) = self.file(name)
            && let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(target: "sparkles::backup", "manifest cache {}: {e}", path.display());
        }
    }
}

fn write_atomic(path: &std::path::Path, e: &impl Serialize) -> std::io::Result<()> {
    let dir = path.parent().expect("a cache file has a directory");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let r = serde_json::to_vec(e)
        .map_err(std::io::Error::other)
        .and_then(|b| std::fs::write(&tmp, b))
        .and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(name: &str) -> Manifest {
        serde_json::from_value(json!({
            "format": 1, "kind": "sparkles-backup", "name": name,
            "id": "0b6e5c1a-0000-4000-8000-000000000001",
            "repositoryId": "7d0e5c1a-0000-4000-8000-000000000002",
            "dataset": {"name": "ds", "id": "3f1c9a2e-0000-4000-8000-000000000003", "type": "persistent"},
            "commit": {"seq": 4, "parent": 3, "ref": "commit:4", "timestamp": "2026-09-30T14:05:11.990Z",
                       "kind": "update", "inserted": 1, "deleted": 0, "quads": 3,
                       "generation": "gen-0001", "bulk": false, "exact": true},
            "generation": "gen-0001", "indexFormat": 2,
            "created": "2026-09-30T14:05:11.995Z", "completed": "2026-09-30T14:05:12.101Z",
            "millis": 106, "server": {"version": "0.1.0"}, "parent": null,
            "policy": null, "run": null, "note": null, "files": [],
            "stats": {"logicalBytes": 0, "addedBytes": 0, "files": 0, "blobs": 0,
                      "newBlobs": 0, "reusedBlobs": 0},
            "derived": {"text": null}, "encryption": null
        }))
        .unwrap()
    }

    #[test]
    fn keyed_by_version_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let c = ManifestCache::new(Some(dir.path().join("repo")));
        assert!(c.get("b1", "v1", 10).is_none());
        c.put("b1", "v1", 10, &manifest("b1"));
        assert_eq!(c.get("b1", "v1", 10).unwrap().name, "b1");
        assert!(c.get("b1", "v2", 10).is_none());
        assert!(c.get("b1", "v1", 11).is_none());
        // a fresh cache over the same directory reads the file
        let c2 = ManifestCache::new(Some(dir.path().join("repo")));
        assert_eq!(c2.get("b1", "v1", 10).unwrap().name, "b1");
        c2.remove("b1");
        assert!(!dir.path().join("repo/b1.json").exists());
        assert!(c2.get("b1", "v1", 10).is_none());
        // memory only
        let m = ManifestCache::new(None);
        m.put("b2", "v", 1, &manifest("b2"));
        assert!(m.get("b2", "v", 1).is_some());
        // an entry under another name (a renamed file) is not used
        let c3 = ManifestCache::new(Some(dir.path().join("repo")));
        c3.put("b3", "v", 1, &manifest("other"));
        std::fs::copy(
            dir.path().join("repo/b3.json"),
            dir.path().join("repo/b4.json"),
        )
        .unwrap();
        assert!(
            ManifestCache::new(Some(dir.path().join("repo")))
                .get("b4", "v", 1)
                .is_none()
        );
    }
}
