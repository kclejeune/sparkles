//! Recover alias renames before opening any registered store. The durable registry
//! selects the committed alias; the intent identifies the directory's dataset.
use super::{DatasetKind, check_name, read_registry, sync_dir, write_file_atomic};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

const INTENT: &str = "rename.json";

#[derive(Serialize, Deserialize)]
struct Intent {
    from: String,
    to: String,
    id: Uuid,
}

pub(super) fn prepare(dir: &Path, from: &str, to: &str, id: Uuid) -> Result<()> {
    if dir.join(INTENT).exists() {
        return Err(Error::Conflict(
            "an unfinished dataset rename needs recovery".into(),
        ));
    }
    if !read_registry(dir)?
        .datasets
        .iter()
        .any(|e| e.name == from && e.kind == DatasetKind::Persistent)
    {
        return Err(Error::Corrupt("renamed dataset is not registered".into()));
    }
    let intent = Intent {
        from: from.into(),
        to: to.into(),
        id,
    };
    write_file_atomic(
        &dir.join(INTENT),
        &serde_json::to_vec(&intent).map_err(|e| Error::invalid(e.to_string()))?,
    )
}

pub(super) fn finish(dir: &Path) -> Result<()> {
    std::fs::remove_file(dir.join(INTENT))?;
    sync_dir(dir)
}

pub(super) fn recover(dir: &Path) -> Result<()> {
    let bytes = match std::fs::read(dir.join(INTENT)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let intent: Intent =
        serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("{INTENT}: {e}")))?;
    check_name(&intent.from)?;
    check_name(&intent.to)?;
    if intent.from == intent.to {
        return Err(Error::Corrupt("rename aliases are identical".into()));
    }
    let reg = read_registry(dir)?;
    let aliases: Vec<_> = reg
        .datasets
        .iter()
        .filter(|e| e.name == intent.from || e.name == intent.to)
        .collect();
    if aliases.len() != 1 || aliases[0].kind != DatasetKind::Persistent {
        return Err(Error::Corrupt(
            "rename intent disagrees with the dataset registry".into(),
        ));
    }
    let databases = dir.join("databases");
    let from = databases.join(&intent.from);
    let to = databases.join(&intent.to);
    let existing = match (from.exists(), to.exists()) {
        (true, false) => &from,
        (false, true) => &to,
        _ => {
            return Err(Error::Corrupt(
                "rename needs exactly one dataset directory".into(),
            ));
        }
    };
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(existing.join("dataset.json"))?)
            .map_err(|e| Error::Corrupt(format!("rename dataset metadata: {e}")))?;
    let id: Uuid = serde_json::from_value(metadata["id"].clone())
        .map_err(|e| Error::Corrupt(format!("rename dataset identity: {e}")))?;
    if id != intent.id {
        return Err(Error::Corrupt(
            "rename directory has another dataset identity".into(),
        ));
    }
    let registered = databases.join(&aliases[0].name);
    if existing != &registered {
        std::fs::rename(existing, registered)?;
    }
    sync_dir(&databases)?;
    finish(dir)
}

#[cfg(test)]
thread_local! {
    static CRASH_AFTER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static FAIL_AT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// A failure after the directory moved, which the rename rolls back.
pub(super) const FAIL_AFTER_MOVE: u8 = 1;
/// A failure to reopen the store while rolling back.
pub(super) const FAIL_ROLLBACK_REOPEN: u8 = 2;

/// Fail like an I/O error would at `_point` in tests. Unlike [`crash_point`], the
/// rename handles the error as it handles a real one.
pub(super) fn fail_point(_point: u8) -> Result<()> {
    #[cfg(test)]
    if FAIL_AT.get() & (1 << _point) != 0 {
        return Err(Error::Io(std::io::Error::other(format!(
            "injected failure {_point}"
        ))));
    }
    Ok(())
}

pub(super) fn crash_point(_stage: u8) -> Result<()> {
    #[cfg(test)]
    if CRASH_AFTER.get() == _stage {
        return Err(Error::invalid(format!(
            "interrupted rename at stage {_stage}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    #[test]
    fn interrupted_renames_preserve_data_identity_and_branches() {
        for stage in 1..=3 {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = cat.create("before", &Default::default()).unwrap();
            ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
            let id = ds.dataset_id();
            ds.create_branch("work", &Default::default()).unwrap();
            ds.branch("work")
                .unwrap()
                .update("INSERT DATA { <urn:t> <urn:p> 2 }")
                .unwrap();
            drop(ds);
            CRASH_AFTER.set(stage);
            assert!(cat.rename("before", "after").is_err());
            CRASH_AFTER.set(0);
            drop(cat);
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            let (kept, absent) = if stage == 3 {
                ("after", "before")
            } else {
                ("before", "after")
            };
            let ds = cat.get(kept).unwrap();
            assert_eq!(ds.dataset_id(), id);
            assert_eq!(ds.len(), 1);
            assert_eq!(ds.branch("work").unwrap().len(), 2);
            assert!(cat.get(absent).is_none());
            assert!(!dir.path().join("databases").join(absent).exists());
            assert!(!dir.path().join(INTENT).exists());
            drop(ds);
            drop(cat);
            assert_eq!(
                Catalog::open(dir.path(), Default::default())
                    .unwrap()
                    .get(kept)
                    .unwrap()
                    .dataset_id(),
                id
            );
        }
    }

    fn renamed_fixture() -> (tempfile::TempDir, Catalog, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("before", &Default::default()).unwrap();
        ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
        let id = ds.dataset_id();
        (dir, cat, id)
    }

    #[test]
    fn a_failure_after_the_move_rolls_back_to_the_old_alias() {
        let (dir, cat, id) = renamed_fixture();
        FAIL_AT.set(1 << FAIL_AFTER_MOVE);
        let e = cat.rename("before", "after");
        FAIL_AT.set(0);
        assert!(matches!(e, Err(Error::Io(_))), "{:?}", e.err());
        let ds = cat.get("before").unwrap();
        assert_eq!((ds.dataset_id(), ds.len()), (id, 1));
        assert!(cat.get("after").is_none());
        assert!(!dir.path().join("databases/after").exists());
        assert!(!dir.path().join(INTENT).exists());
        drop(ds);
        // the rollback leaves a catalog that can rename again
        assert_eq!(cat.rename("before", "after").unwrap().dataset_id(), id);
    }

    #[test]
    fn a_rollback_that_cannot_reopen_keeps_the_dataset_registered() {
        let (dir, cat, id) = renamed_fixture();
        FAIL_AT.set(1 << FAIL_AFTER_MOVE | 1 << FAIL_ROLLBACK_REOPEN);
        let e = cat.rename("before", "after");
        FAIL_AT.set(0);
        let Err(Error::Corrupt(m)) = e else {
            panic!("{:?}", e.err())
        };
        assert!(m.contains("registers it as /before"), "{m}");
        assert!(cat.get("before").is_none() && cat.get("after").is_none());
        // the name stays taken, and later saves keep the registration
        assert!(matches!(
            cat.create("before", &Default::default()),
            Err(Error::Conflict(_))
        ));
        cat.create("other", &Default::default()).unwrap();
        let names: Vec<_> = read_registry(dir.path())
            .unwrap()
            .datasets
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["before", "other"]);
        drop(cat);
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(cat.get("before").unwrap().dataset_id(), id);
        assert_eq!(cat.get("before").unwrap().len(), 1);
    }

    #[test]
    fn recovery_refuses_conflicting_directories_and_wrong_identities() {
        for conflict in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = cat.create("before", &Default::default()).unwrap();
            let id = if conflict {
                ds.dataset_id()
            } else {
                Uuid::new_v4()
            };
            prepare(dir.path(), "before", "after", id).unwrap();
            if conflict {
                std::fs::create_dir(dir.path().join("databases/after")).unwrap();
            }
            drop(ds);
            drop(cat);
            assert!(matches!(
                Catalog::open(dir.path(), Default::default()),
                Err(Error::Corrupt(_))
            ));
            assert!(dir.path().join("databases/before/dataset.json").exists());
            assert!(dir.path().join(INTENT).exists());
        }
    }
}
