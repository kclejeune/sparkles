//! Files tied to a source load by a digest in its commit annotation.

use super::Store;
use std::fs;
use std::io;
use std::path::Path;

pub const DIRECTORY: &str = "source-manifests";
pub const ANNOTATION: &str = "source manifest sha256:";

pub fn file_seq(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".json")?;
    let seq: u64 = stem.parse().ok()?;
    (seq != 0 && seq.to_string() == stem).then_some(seq)
}

fn temporary(name: &str) -> bool {
    name.strip_suffix(".tmp").is_some_and(|stem| {
        stem.parse::<u64>()
            .is_ok_and(|seq| seq != 0 && seq.to_string() == stem)
    })
}

/// The store's writer and its annotations have recovered before this runs. Files whose
/// commit did not publish or whose annotation was pruned have no live reference.
pub(super) fn sweep(store: &Store, root: &Path, head: u64) -> io::Result<()> {
    let dir = root.join(DIRECTORY);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut removed = false;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let unreferenced = file_seq(&name).is_some_and(|seq| {
            seq > head
                || !store.annotation(seq).is_some_and(|a| {
                    a.message
                        .as_deref()
                        .is_some_and(|m| m.starts_with(ANNOTATION))
                })
        });
        if unreferenced || temporary(&name) {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        crate::fsutil::sync_dir(&dir)?;
    }
    Ok(())
}
