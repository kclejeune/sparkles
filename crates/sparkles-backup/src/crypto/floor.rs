//! The highest key epoch this host has accepted for each repository. A leaked old
//! master key lets its holder sign a descriptor that drops every newer epoch, so
//! the descriptor tag alone cannot stop a rollback. This record can.
//!
//! The record lives in memory for the life of the process and, when the repository
//! has a cache directory, in `<cache_dir>/<repository id>/.key-epoch.json`. Deleting
//! that file and restarting resets it, which an operator may need after restoring a
//! whole repository from an older copy of its bucket.
use super::slots::{Descriptor, tampered};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};
use uuid::Uuid;

const FILE: &str = ".key-epoch.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    /// the highest epoch listed by an accepted descriptor
    epoch: u32,
    /// an accepted descriptor carried a valid tag
    authenticated: bool,
}
impl Record {
    fn merge(self, other: Record) -> Record {
        Record {
            epoch: self.epoch.max(other.epoch),
            authenticated: self.authenticated || other.authenticated,
        }
    }
}

static SEEN: LazyLock<Mutex<HashMap<Uuid, Record>>> = LazyLock::new(Default::default);
/// Serializes writes of the floor files.
static PERSIST: Mutex<()> = Mutex::new(());

fn file(dir: Option<&Path>) -> Option<PathBuf> {
    dir.map(|dir| dir.join(FILE))
}

/// The merged memory and disk record. The disk record also raises the memory one,
/// so handles of the same repository without a cache directory benefit from it.
fn load(repository: Uuid, dir: Option<&Path>) -> Result<Record> {
    // An unreadable or malformed file counts as absent. A local attacker who can
    // edit the cache directory is outside what this check defends against. A file that
    // a newer release wrote is refused, since this build cannot tell what it records.
    let disk = match file(dir) {
        Some(path) => read(&path)?.unwrap_or_default(),
        None => Record::default(),
    };
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let entry = seen.entry(repository).or_default();
    *entry = entry.merge(disk);
    Ok(*entry)
}

/// Refuse a descriptor older than one this host accepted before, or an
/// unauthenticated descriptor after an authenticated one.
pub(crate) fn check(repository: Uuid, dir: Option<&Path>, descriptor: &Descriptor) -> Result<()> {
    let seen = load(repository, dir)?;
    validate(seen, descriptor)
}

fn validate(seen: Record, descriptor: &Descriptor) -> Result<()> {
    if descriptor.max_epoch() < seen.epoch {
        return Err(tampered(
            "repository key epochs are older than this host has already seen",
        ));
    }
    if seen.authenticated && descriptor.mac.is_none() {
        return Err(tampered(
            "repository key epoch metadata lost its authentication",
        ));
    }
    Ok(())
}

/// Raise the floor after `descriptor` has been unlocked and its tag verified, and refuse
/// it if this host has seen a newer one meanwhile. In particular, an unlock started
/// before a rotation must not accept its old descriptor after that rotation.
pub(crate) fn record(repository: Uuid, dir: Option<&Path>, descriptor: &Descriptor) -> Result<()> {
    accept(repository, dir, descriptor, true)
}

/// Raise the floor to a rotation this handle has just published. The rotation stands
/// even if a newer epoch was seen meanwhile, and the floor never goes down, so this
/// does not check the descriptor. The only failure is a floor file that a newer release
/// wrote, which is left alone and logged.
pub(crate) fn raise(repository: Uuid, dir: Option<&Path>, descriptor: &Descriptor) {
    if let Err(error) = accept(repository, dir, descriptor, false) {
        tracing::warn!(target: "sparkles::backup", "repository key epoch record: {error}");
    }
}

fn accept(
    repository: Uuid,
    dir: Option<&Path>,
    descriptor: &Descriptor,
    checked: bool,
) -> Result<()> {
    let accepted = Record {
        epoch: descriptor.max_epoch(),
        authenticated: descriptor.mac.is_some(),
    };
    let path = file(dir);
    let stored = match &path {
        Some(path) => read(path)?,
        None => None,
    };
    // Check and raise under one lock, so that two handles cannot both accept against
    // the same floor. File I/O stays outside it.
    let merged = {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let entry = seen.entry(repository).or_default();
        *entry = entry.merge(stored.unwrap_or_default());
        if checked {
            validate(*entry, descriptor)?;
        }
        *entry = entry.merge(accepted);
        *entry
    };
    let Some(path) = path else {
        return Ok(());
    };
    // Writers take turns and merge with the file as it is now, so a lower record never
    // replaces a higher one written in between.
    let _turn = PERSIST.lock().unwrap_or_else(|e| e.into_inner());
    let stored = read(&path)?;
    let merged = stored.map_or(merged, |stored| stored.merge(merged));
    if stored == Some(merged) {
        return Ok(());
    }
    // Best effort, like the manifest cache next to it. The in-memory floor still
    // protects this process when the directory cannot be written.
    let write = || -> std::io::Result<()> {
        let dir = path.parent().expect("file has a parent");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.{}.tmp", Uuid::new_v4()));
        let bytes = sparkles_core::formats::settings_to_vec(FILE, &merged)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    };
    if let Err(error) = write() {
        tracing::warn!(
            target: "sparkles::backup",
            "repository key epoch record {}: {error}",
            path.display()
        );
    }
    Ok(())
}

/// The record in `path`: `None` when the file is absent or does not parse, and an error
/// when its version is newer than this build reads.
fn read(path: &Path) -> Result<Option<Record>> {
    let Some(v) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return Ok(None);
    };
    sparkles_core::formats::check_settings(FILE, &v)?;
    Ok(serde_json::from_value(v).ok())
}

/// Forget the in-memory floor, as a process restart does.
#[cfg(test)]
pub(crate) fn forget(repository: Uuid) {
    SEEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&repository);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_floor_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let writes = sparkles_core::formats::settings_file(FILE).unwrap().writes;
        std::fs::write(
            &path,
            format!(r#"{{"format": {}, "epoch": 3}}"#, writes + 1),
        )
        .unwrap();
        assert!(read(&path).is_err());
        assert!(load(Uuid::new_v4(), Some(dir.path())).is_err());
        std::fs::write(&path, r#"{"epoch": 3, "authenticated": true}"#).unwrap();
        assert_eq!(read(&path).unwrap().unwrap().epoch, 3);
    }
}
