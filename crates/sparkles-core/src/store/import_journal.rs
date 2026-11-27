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
pub(super) fn discard_uncommitted(root: &Path, head: u64) -> Result<()> {
    let Some(mut journal) = read(root)? else {
        return Ok(());
    };
    let count = journal.publishing.len();
    journal
        .publishing
        .retain(|intent| !matches!(intent, Intent::Tracked(p) if p.commit > head));
    if journal.publishing.len() == count {
        return Ok(());
    }
    let path = root.join(FILE);
    if journal.publishing.is_empty() && journal.imports.is_empty() {
        std::fs::remove_file(path)?;
        crate::fsutil::sync_dir(root)?;
    } else {
        let format = if journal.publishing.is_empty() { 1 } else { 2 };
        let bytes = crate::formats::settings_to_vec_at(FILE, &journal, format)?;
        crate::fsutil::write_atomic(&path, &bytes)?;
    }
    Ok(())
}
