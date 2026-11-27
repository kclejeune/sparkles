//! Commit evidence for the facade's pending reasoning imports.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE: &str = "reasoning-imports.json";

#[derive(Deserialize, Serialize)]
pub struct Saved<Names, Intents> {
    pub imports: Names,
    pub publishing: Intents,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum Intent {
    Legacy(String),
    Tracked(Publication<String>),
}

#[derive(Deserialize, Serialize)]
pub struct Publication<I> {
    pub iri: I,
    pub commit: u64,
}

pub type Journal = Saved<Vec<String>, Vec<Intent>>;

pub fn read(root: &Path) -> Result<Option<Journal>> {
    let bytes = match std::fs::read(root.join(FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let (journal, format): (Journal, _) =
        crate::formats::settings_from_slice_with_format(FILE, &bytes)?;
    for intent in &journal.publishing {
        match (format, intent) {
            (1, Intent::Legacy(_)) => {}
            (2, Intent::Tracked(p)) if p.commit != 0 => {}
            _ => {
                return Err(Error::Corrupt(format!(
                    "{FILE}: invalid publication intent"
                )));
            }
        }
    }
    Ok(Some(journal))
}

/// Discard aborted reservations before any new commit can reuse their numbers,
/// including when the store is opened without the facade.
///
/// The dataset root is synced on every call. An earlier recovery can stop after its
/// unlink or rename reached the live directory and before that directory was
/// synced. This open then finds the journal absent or already filtered, and a power
/// loss after its first commit could still bring the aborted intent back beside a
/// commit that reused its number.
pub(super) fn discard_uncommitted(root: &Path, head: u64) -> Result<()> {
    if let Some(mut journal) = read(root)? {
        let count = journal.publishing.len();
        journal
            .publishing
            .retain(|intent| !matches!(intent, Intent::Tracked(p) if p.commit > head));
        if journal.publishing.len() != count {
            let path = root.join(FILE);
            if journal.publishing.is_empty() && journal.imports.is_empty() {
                std::fs::remove_file(path)?;
            } else {
                let format = if journal.publishing.is_empty() { 1 } else { 2 };
                let bytes = crate::formats::settings_to_vec_at(FILE, &journal, format)?;
                crate::fsutil::replace_via(&path, &path.with_extension("tmp"), &bytes)?;
            }
        }
    }
    crate::fsutil::sync_dir(root)?;
    Ok(())
}

// Directory syncs are recorded only where `sync_dir` performs them.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn intents(root: &Path) -> Vec<u64> {
        read(root)
            .unwrap()
            .map(|j| {
                j.publishing
                    .iter()
                    .map(|i| match i {
                        Intent::Tracked(p) => p.commit,
                        Intent::Legacy(_) => 0,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn write(root: &Path, imports: &[&str], commits: &[u64]) {
        let journal = Saved {
            imports: imports.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            publishing: commits
                .iter()
                .map(|&commit| {
                    Intent::Tracked(Publication {
                        iri: format!("urn:i{commit}"),
                        commit,
                    })
                })
                .collect::<Vec<_>>(),
        };
        let format = if commits.is_empty() { 1 } else { 2 };
        let bytes = crate::formats::settings_to_vec_at(FILE, &journal, format).unwrap();
        std::fs::write(root.join(FILE), bytes).unwrap();
    }

    /// Each shape an interrupted recovery can leave, and the edits this one makes,
    /// ends with the dataset root synced after the journal reached its final state.
    #[test]
    fn every_recovery_fences_the_dataset_root() {
        type Setup = fn(&Path);
        let shapes: [(&str, Setup, Vec<u64>, bool); 4] = [
            // a removal that may not be durable yet
            ("absent", |_| {}, vec![], false),
            // a filtered rename that may not be durable yet
            ("filtered", |r| write(r, &["urn:a"], &[3]), vec![3], true),
            ("removes", |r| write(r, &[], &[4]), vec![], false),
            ("filters", |r| write(r, &["urn:a"], &[3, 4]), vec![3], true),
        ];
        for (name, setup, kept, exists) in shapes {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            setup(root);
            crate::fsutil::take_dir_syncs();
            discard_uncommitted(root, 3).unwrap();
            let syncs = crate::fsutil::take_dir_syncs();
            let (synced, names) = syncs.last().expect(name);
            assert_eq!(synced, root, "{name}");
            assert_eq!(names.iter().any(|n| n == FILE), exists, "{name}");
            assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{name}");
            assert_eq!(intents(root), kept, "{name}");
        }
    }
}
