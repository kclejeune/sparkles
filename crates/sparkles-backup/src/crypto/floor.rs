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

fn file(dir: Option<&Path>) -> Option<PathBuf> {
    dir.map(|dir| dir.join(FILE))
}

/// The merged memory and disk record. The disk record also raises the memory one,
/// so handles of the same repository without a cache directory benefit from it.
fn load(repository: Uuid, dir: Option<&Path>) -> Record {
    // An unreadable or malformed file counts as absent. A local attacker who can
    // edit the cache directory is outside what this check defends against.
    let disk = file(dir)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .unwrap_or_default();
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let entry = seen.entry(repository).or_default();
    *entry = entry.merge(disk);
    *entry
}

/// Refuse a descriptor older than one this host accepted before, or an
/// unauthenticated descriptor after an authenticated one.
pub(crate) fn check(repository: Uuid, dir: Option<&Path>, descriptor: &Descriptor) -> Result<()> {
    let seen = load(repository, dir);
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

/// Raise the floor after `descriptor` has been unlocked and its tag verified.
pub(crate) fn record(repository: Uuid, dir: Option<&Path>, descriptor: &Descriptor) {
    let accepted = Record {
        epoch: descriptor.max_epoch(),
        authenticated: descriptor.mac.is_some(),
    };
    let merged = {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let entry = seen.entry(repository).or_default();
        *entry = entry.merge(accepted);
        *entry
    };
    let Some(path) = file(dir) else {
        return;
    };
    let stored = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok());
    let merged = stored.map_or(merged, |stored| stored.merge(merged));
    if stored == Some(merged) {
        return;
    }
    // Best effort, like the manifest cache next to it. The in-memory floor still
    // protects this process when the directory cannot be written.
    let write = || -> std::io::Result<()> {
        let dir = path.parent().expect("file has a parent");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.{}.tmp", Uuid::new_v4()));
        std::fs::write(&tmp, serde_json::to_vec(&merged).expect("serializes"))?;
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
}

/// Forget the in-memory floor, as a process restart does.
#[cfg(test)]
pub(crate) fn forget(repository: Uuid) {
    SEEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&repository);
}
