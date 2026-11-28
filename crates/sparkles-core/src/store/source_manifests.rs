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

pub(super) fn has_reference(a: &crate::annotations::Annotation) -> bool {
    a.message
        .as_deref()
        .is_some_and(|m| m.starts_with(ANNOTATION))
}

/// The writer and annotations have recovered. Replay can tolerate missing annotations,
/// so keep sidecars for retained commits until there is evidence they are unreferenced.
pub(super) fn sweep(store: &Store, root: &Path, head: u64) -> io::Result<()> {
    let dir = root.join(DIRECTORY);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let first = store.catalog.lock().first().map_or(0, |commit| commit.seq);
    let mut removed = false;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let unreferenced = file_seq(&name).is_some_and(|seq| {
            seq > head || seq < first || store.annotation(seq).is_some_and(|a| !has_reference(&a))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::WriteOptions;
    use crate::io::Source;
    use crate::store::{ReplaceTarget, StoreOptions};

    #[test]
    fn recovery_keeps_a_retained_manifest_when_its_annotation_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let store = Store::open(root, StoreOptions::default()).unwrap();
        let input = Source::from_bytes(
            b"<urn:s> <urn:p> <urn:o> .".to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        );
        let opts = WriteOptions {
            message: Some(format!("{ANNOTATION}{}", "0".repeat(64)).into()),
            ..Default::default()
        };
        store
            .replace_with(
                ReplaceTarget::Default,
                &[input],
                crate::commit::CommitKind::Load,
                &opts,
            )
            .unwrap();
        let seq = store.head_commit().seq;
        store
            .load(&[Source::from_bytes(
                b"<urn:s2> <urn:p> <urn:o> .".to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        assert!(store.annotation(seq).is_some());
        drop(store);
        fs::remove_file(root.join(crate::annotations::FILE)).unwrap();

        let manifests = root.join(DIRECTORY);
        fs::create_dir(&manifests).unwrap();
        let retained = manifests.join(format!("{seq}.json"));
        let unpublished = manifests.join(format!("{}.json", seq + 2));
        fs::write(&retained, b"retained").unwrap();
        fs::write(&unpublished, b"unpublished").unwrap();
        let reopened = Store::open(root, StoreOptions::default()).unwrap();
        assert_eq!(reopened.head_commit().seq, seq + 1);
        assert!(reopened.annotation(seq).is_none());
        assert_eq!(fs::read(retained).unwrap(), b"retained");
        assert!(!unpublished.exists());
    }
}
