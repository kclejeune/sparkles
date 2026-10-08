//! Durable publication of restored catalog datasets. New datasets are renamed into
//! place and registered under their reservation. Replacement waits for live dataset
//! and branch handles, closes the store, swaps two directories and reopens it. A failed
//! open rolls back both renames. Catalog::open repairs interrupted swaps while holding
//! the directory lock.

use super::restore::{KEPT_PREFIX, REPLACED_PREFIX, RESTORE_PREFIX};
use super::{Catalog, Reservation, ReservationKind, sync_dir};
use crate::Dataset;
use sparkles_backup::{BackupError, Code};
use std::path::Path;
use std::time::{Duration, Instant};

/// How long an in-place restore waits for requests to release the dataset.
pub const DRAIN: Duration = Duration::from_secs(30);

fn io(what: &str, path: &Path, e: std::io::Error) -> BackupError {
    BackupError::internal(format!("{what} {}: {e}", path.display()))
}

/// Publish `restored` as the new dataset of `reservation` (renamed into
/// `databases/<name>`, then adopted). On failure nothing stays registered, and the
/// restored directory is removed.
pub fn publish_new(
    st: &Catalog,
    reservation: Reservation,
    restored: &Path,
) -> Result<Dataset, BackupError> {
    st.check_reservation(&reservation)
        .map_err(|e| BackupError::internal(e.to_string()))?;
    let name = reservation.name().to_string();
    let _span = tracing::info_span!("restore.swap", "db.namespace" = name.as_str()).entered();
    let databases = st
        .dir()
        .ok_or_else(|| BackupError::new(Code::NotManaged, "restore needs a catalog directory"))?
        .join("databases");
    let dst = databases.join(&name);
    if let Err(e) = std::fs::rename(restored, &dst) {
        let _ = std::fs::remove_dir_all(restored);
        return Err(io("publishing", &dst, e));
    }
    sync_dir(&databases).map_err(|e| BackupError::internal(format!("{e:#}")))?;
    // `adopt` removes the directory if it cannot open or register it
    st.adopt(reservation)
        .map_err(|e| BackupError::internal(format!("registering /{name}: {e:#}")))
}

/// Replace the registered persistent dataset `name` with `restored` (see the module
/// docs). `task` names the `.replaced-` directory; `keep_replaced` keeps it.
pub fn replace_in_place(
    st: &Catalog,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
) -> Result<Dataset, BackupError> {
    replace_with(st, name, restored, task, keep_replaced, DRAIN)
}

#[cfg(test)]
thread_local! {
    /// Stop the swap after rename 1 or 2 without cleaning up, as a crash would.
    pub(crate) static CRASH_AFTER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

fn crash_point(_n: u8) -> Result<(), BackupError> {
    #[cfg(test)]
    if CRASH_AFTER.get() == _n {
        return Err(BackupError::internal(format!("crashed after rename {_n}")));
    }
    Ok(())
}

/// [`replace_in_place`] with its drain time.
pub(crate) fn replace_with(
    st: &Catalog,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
    drain: Duration,
) -> Result<Dataset, BackupError> {
    let _span = tracing::info_span!(
        "restore.swap",
        "db.namespace" = name,
        "sparkles.task" = task
    )
    .entered();
    let _restoring = if st.restoring_by(name).as_deref() == Some(task) {
        None
    } else {
        Some(
            st.reserve(name, ReservationKind::Restore, task)
                .map_err(|e| BackupError::new(Code::DatasetBusy, e.to_string()))?,
        )
    };
    let t0 = Instant::now();
    while st.get(name).is_some_and(|d| d.in_use_without_self()) && t0.elapsed() < drain {
        std::thread::sleep(Duration::from_millis(5));
    }
    let Some(old) = st.detach_for_swap(name) else {
        return Err(match st.get(name) {
            Some(_) => BackupError::new(
                Code::NotManaged,
                format!("/{name} is not a persistent dataset of this server's data directory"),
            ),
            None => BackupError::internal(format!("no dataset /{name}")),
        });
    };
    while old.in_use() {
        if t0.elapsed() >= drain {
            // it stays as it was
            st.reinsert(name, old);
            return Err(BackupError::new(
                Code::DatasetBusy,
                format!(
                    "requests to /{name} did not finish within {} s; the dataset is unchanged",
                    drain.as_secs()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let Some(root) = old.store().root().map(Path::to_path_buf) else {
        st.reinsert(name, old);
        return Err(BackupError::new(
            Code::NotManaged,
            format!("/{name} is an in-memory dataset"),
        ));
    };
    // closing the store releases its `sparkles.lock`
    drop(old);
    // the storage quota is the operator's, not the backup's: the restored dataset keeps
    // the one it replaces
    let quota = root.join(crate::store::QUOTA_FILE);
    if quota.exists()
        && let Err(e) = std::fs::copy(&quota, restored.join(crate::store::QUOTA_FILE))
    {
        tracing::warn!(target: "sparkles::backup", "keeping the storage quota of /{name}: {e}");
    }
    let databases = root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| st.dir().expect("persistent catalog").join("databases"));
    let replaced = databases.join(format!("{REPLACED_PREFIX}{name}-{task}"));
    if let Err(e) = std::fs::rename(&root, &replaced) {
        let err = io("moving aside", &root, e);
        return Err(reopen_after(st, name, err));
    }
    crash_point(1)?;
    if let Err(e) = std::fs::rename(restored, &root) {
        let err = io("moving the restored copy to", &root, e);
        if let Err(e2) = std::fs::rename(&replaced, &root) {
            return Err(BackupError::internal(format!(
                "{err}; putting back {} failed too: {e2} (the next start repairs it)",
                replaced.display()
            )));
        }
        return Err(reopen_after(st, name, err));
    }
    crash_point(2)?;
    if let Err(e) = sync_dir(&databases) {
        tracing::warn!(target: "sparkles::backup", "syncing {}: {e:#}", databases.display());
    }
    match st.reattach(name) {
        Ok(ds) => {
            let cleanup = if keep_replaced {
                let kept = databases.join(format!("{KEPT_PREFIX}{name}-{task}"));
                std::fs::rename(&replaced, &kept)
            } else {
                std::fs::remove_dir_all(&replaced)
            };
            if let Err(e) = cleanup {
                tracing::warn!(
                    target: "sparkles::backup",
                    "{}: {e} (the next start removes it)",
                    replaced.display()
                );
            }
            // the registry embeds the reasoning status, which the restore may change
            if let Err(e) = st.save() {
                tracing::warn!(target: "sparkles::backup", "saving the dataset registry: {e:#}");
            }
            Ok(ds)
        }
        Err(e) => {
            // the restored store does not open: put the old one back
            let failed = databases.join(format!("{RESTORE_PREFIX}{name}-{task}"));
            let back =
                std::fs::rename(&root, &failed).and_then(|()| std::fs::rename(&replaced, &root));
            let _ = std::fs::remove_dir_all(&failed);
            let err = BackupError::internal(format!("the restored /{name} does not open: {e:#}"));
            match back {
                Ok(()) => Err(reopen_after(st, name, err)),
                Err(e2) => Err(BackupError::internal(format!(
                    "{err}; putting back the old copy failed: {e2} (the next start repairs it)"
                ))),
            }
        }
    }
}

/// Reopen the old dataset `name` after a failed swap and return `err` (with the reopen
/// failure, if any). The registry is saved again with the reopened dataset's record.
fn reopen_after(st: &Catalog, name: &str, err: BackupError) -> BackupError {
    match st.reattach(name) {
        Ok(_) => {
            if let Err(e) = st.save() {
                tracing::warn!(target: "sparkles::backup", "saving the dataset registry: {e:#}");
            }
            err
        }
        Err(e) => BackupError::internal(format!(
            "{err}; reopening the old /{name} failed too: {e:#}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::CreateDataset;
    use super::*;
    use crate::sparql::QueryOptions;
    use crate::store::{Store, StoreOptions};

    fn state(dir: &Path) -> Catalog {
        Catalog::open(dir, StoreOptions::default().into()).unwrap()
    }

    fn update(ds: &Dataset, u: &str) {
        crate::sparql::update::update(ds.store(), u, &QueryOptions::default()).unwrap();
    }

    /// A closed database at `dir` with `n` quads.
    fn restored(dir: &Path, n: usize) -> uuid::Uuid {
        let s = Store::open(dir, StoreOptions::default()).unwrap();
        for i in 0..n {
            crate::sparql::update::update(
                &s,
                &format!("INSERT DATA {{ <urn:r{i}> <urn:p> {i} }}"),
                &QueryOptions::default(),
            )
            .unwrap();
        }
        s.dataset_id()
    }

    #[test]
    fn publishes_a_new_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let tmp = dir.path().join("databases").join(".restore-x-3");
        let id = restored(&tmp, 2);
        let r = st.reserve("x", ReservationKind::Restore, "3").unwrap();
        let ds = publish_new(&st, r, &tmp).unwrap();
        assert_eq!(ds.store().dataset_id(), id);
        assert_eq!(ds.store().snapshot().len(), 2);
        assert!(!tmp.exists() && st.reserved_by("x").is_none());
        // registered: a restart opens it
        drop(ds);
        drop(st);
        let st = state(dir.path());
        assert_eq!(st.get("x").unwrap().store().dataset_id(), id);
    }

    #[test]
    fn replaces_in_place_and_keeps_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        ds.store().set_quota(Some(1 << 30)).unwrap();
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-7");
        let id = restored(&tmp, 3);
        let ds = replace_in_place(&st, "ds", &tmp, "7", true).unwrap();
        assert_eq!(ds.store().dataset_id(), id);
        // the restored dataset keeps the storage quota of the one it replaced
        assert_eq!(ds.store().quota().max_bytes, Some(1 << 30));
        assert_eq!(st.get("ds").unwrap().store().snapshot().len(), 3);
        assert!(st.restoring_by("ds").is_none());
        let db = dir.path().join("databases");
        assert!(!tmp.exists() && !db.join(".replaced-ds-7").exists());
        assert!(db.join(".kept-ds-7").join("dataset.json").exists());
    }

    #[test]
    fn busy_datasets_stay_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        let before = ds.store().dataset_id();
        let tmp = dir.path().join("databases").join(".restore-ds-8");
        restored(&tmp, 3);
        // `ds` is held (a request in flight)
        let e = replace_with(&st, "ds", &tmp, "8", false, Duration::from_millis(50))
            .err()
            .unwrap();
        assert_eq!(e.code(), Code::DatasetBusy, "{e}");
        assert_eq!(st.get("ds").unwrap().store().dataset_id(), before);
        assert!(st.restoring_by("ds").is_none());
        assert!(tmp.exists());
        drop(ds);
        // an in-memory or unknown dataset cannot be replaced
        st.attach("m", super::super::Attach::Memory).unwrap();
        let e = replace_with(&st, "m", &tmp, "8", false, Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(e.code(), Code::NotManaged);
        assert!(replace_with(&st, "none", &tmp, "8", false, Duration::ZERO).is_err());
    }

    #[test]
    fn a_restored_copy_that_does_not_open_is_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        let before = ds.store().dataset_id();
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-9");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("CURRENT"), b"gen-0042\n").unwrap();
        let e = replace_in_place(&st, "ds", &tmp, "9", false).err().unwrap();
        assert!(e.message().contains("does not open"), "{e}");
        let ds = st.get("ds").unwrap();
        assert_eq!(ds.store().dataset_id(), before);
        assert_eq!(ds.store().snapshot().len(), 1);
        let db = dir.path().join("databases");
        assert!(!db.join(".replaced-ds-9").exists() && !tmp.exists());
    }

    fn registered(dir: &Path) -> Vec<String> {
        super::super::read_registry(dir)
            .unwrap()
            .datasets
            .into_iter()
            .map(|e| e.name)
            .collect()
    }

    /// A registry save while a dataset is detached for its swap, by a create, delete
    /// or rename of another dataset, keeps the detached dataset registered.
    #[test]
    fn saves_during_the_drain_keep_the_swapped_dataset_registered() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        let id = ds.store().dataset_id();
        drop(ds);
        let restoring = st.reserve("ds", ReservationKind::Restore, "1").unwrap();
        let old = st.detach_for_swap("ds").unwrap();
        st.create("other", &CreateDataset::default()).unwrap();
        assert_eq!(registered(dir.path()), ["ds", "other"]);
        st.rename("other", "renamed").unwrap();
        assert!(st.delete("renamed").unwrap());
        assert_eq!(registered(dir.path()), ["ds"]);
        // a crash now still finds the dataset
        drop((old, restoring));
        drop(st);
        let st = state(dir.path());
        assert_eq!(st.get("ds").unwrap().store().dataset_id(), id);
    }

    /// A crash after the first rename: the next start puts the old copy back.
    #[test]
    fn a_crash_between_the_renames_is_repaired_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        update(&ds, "INSERT DATA { <urn:b> <urn:p> 2 }");
        let before = (ds.store().dataset_id(), ds.store().head_commit().seq);
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-4");
        restored(&tmp, 5);
        CRASH_AFTER.set(1);
        let r = replace_in_place(&st, "ds", &tmp, "4", false);
        CRASH_AFTER.set(0);
        assert!(r.is_err());
        drop(st);
        let db = dir.path().join("databases");
        assert!(!db.join("ds").exists() && db.join(".replaced-ds-4").exists());
        super::super::restore::recover(dir.path()).unwrap();
        let st = state(dir.path());
        let ds = st.get("ds").unwrap();
        assert_eq!(
            (ds.store().dataset_id(), ds.store().head_commit().seq),
            before
        );
        assert!(!tmp.exists() && !db.join(".replaced-ds-4").exists());
    }

    /// A crash after both renames: the restored copy is the dataset, the old one goes.
    #[test]
    fn a_crash_after_both_renames_keeps_the_restored_copy() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-5");
        let id = restored(&tmp, 2);
        CRASH_AFTER.set(2);
        let r = replace_in_place(&st, "ds", &tmp, "5", false);
        CRASH_AFTER.set(0);
        assert!(r.is_err());
        drop(st);
        let db = dir.path().join("databases");
        assert!(db.join(".replaced-ds-5").exists());
        super::super::restore::recover(dir.path()).unwrap();
        let st = state(dir.path());
        assert_eq!(st.get("ds").unwrap().store().dataset_id(), id);
        assert!(!db.join(".replaced-ds-5").exists());
    }
}
