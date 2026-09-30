//! Publishing a restored directory on the server.
//!
//! * **New dataset**: rename the restored directory into `databases/<target>` and
//!   `AppState::adopt` the reservation (the clone code path).
//! * **In place**: mark `<target>` restoring (`AppState::restoring`: requests get `503`
//!   with `Retry-After`), wait for the requests that passed that check before (they
//!   hold the dataset until they answer, and must still find it registered), take it
//!   out of the map (`AppState::detach_for_swap`), wait for what still holds it
//!   (`Arc::strong_count == 1`; both waits together at most 30 s, else put it back
//!   and fail `409 dataset-busy`), drop it (releasing `sparkles.lock`), rename
//!   `databases/<t>` → `databases/.replaced-<t>-<task>`, the restored directory →
//!   `databases/<t>`, fsync `databases/`, reopen (`AppState::reattach`), and remove the
//!   replaced copy unless kept (then it becomes `databases/.kept-<t>-<task>`). If the
//!   new store fails to open: rename back, reopen the old one, fail. The caller makes
//!   the task not cancellable before calling [`replace_in_place`].
//!
//! A crash between the renames is repaired at the next start (`recover::startup`).

use super::recover::{KEPT_PREFIX, REPLACED_PREFIX, RESTORE_PREFIX};
use crate::state::{AppState, Dataset, Reservation, sync_dir};
use sparkles_backup::{BackupError, Code};
use std::path::Path;
use std::sync::Arc;
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
    st: &Arc<AppState>,
    reservation: Reservation,
    restored: &Path,
) -> Result<Arc<Dataset>, BackupError> {
    let name = reservation.name().to_string();
    let _span = tracing::info_span!("restore.swap", "db.namespace" = name.as_str()).entered();
    let databases = st.data_dir.join("databases");
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
    st: &Arc<AppState>,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
) -> Result<Arc<Dataset>, BackupError> {
    replace_with(st, name, restored, task, keep_replaced, DRAIN)
}

/// Removes a dataset from `AppState::restoring` when the swap ends, however it ends.
struct Restoring<'a> {
    st: &'a AppState,
    name: &'a str,
}

impl Drop for Restoring<'_> {
    fn drop(&mut self) {
        self.st.restoring.lock().remove(self.name);
    }
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
    st: &Arc<AppState>,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
    drain: Duration,
) -> Result<Arc<Dataset>, BackupError> {
    let _span = tracing::info_span!(
        "restore.swap",
        "db.namespace" = name,
        "sparkles.task" = task
    )
    .entered();
    // from here on, new requests to the dataset get 503 (never a 404 while it is out
    // of the map)
    {
        let mut r = st.restoring.lock();
        if let Some(t) = r.get(name) {
            return Err(BackupError::new(
                Code::DatasetBusy,
                format!("dataset /{name} is being restored by task {t}"),
            )
            .with("task", t.as_str()));
        }
        r.insert(name.to_string(), task.to_string());
    }
    let _restoring = Restoring { st, name };
    // requests that passed the restoring check hold the dataset until they answer
    // (the router's restoring layer): wait for them while it is still registered, so
    // none looks it up after it left the map
    let t0 = Instant::now();
    while st
        .datasets
        .read()
        .get(name)
        .is_some_and(|d| Arc::strong_count(d) > 1)
        && t0.elapsed() < drain
    {
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
    while Arc::strong_count(&old) > 1 {
        if t0.elapsed() >= drain {
            // it stays as it was
            st.datasets.write().insert(name.to_string(), old);
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
    let Some(root) = old.store.root().map(Path::to_path_buf) else {
        st.datasets.write().insert(name.to_string(), old);
        return Err(BackupError::new(
            Code::NotManaged,
            format!("/{name} is an in-memory dataset"),
        ));
    };
    // closing the store releases its `sparkles.lock`
    drop(old);
    let databases = root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| st.data_dir.join("databases"));
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
            if let Err(e) = st.save_registry() {
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
/// failure, if any).
fn reopen_after(st: &AppState, name: &str, err: BackupError) -> BackupError {
    match st.reattach(name) {
        Ok(_) => err,
        Err(e) => BackupError::internal(format!(
            "{err}; reopening the old /{name} failed too: {e:#}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DbType;
    use sparkles::sparql::QueryOptions;
    use sparkles::store::{Store, StoreOptions};

    fn state(dir: &Path) -> Arc<AppState> {
        Arc::new(AppState::new(dir, StoreOptions::default(), Duration::from_secs(30)).unwrap())
    }

    fn update(ds: &Dataset, u: &str) {
        sparkles::sparql::update::update(&ds.store, u, &QueryOptions::default()).unwrap();
    }

    /// A closed database at `dir` with `n` quads.
    fn restored(dir: &Path, n: usize) -> uuid::Uuid {
        let s = Store::open(dir, StoreOptions::default()).unwrap();
        for i in 0..n {
            sparkles::sparql::update::update(
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
        let r = st.reserve("x", "3").unwrap();
        let ds = publish_new(&st, r, &tmp).unwrap();
        assert_eq!(ds.store.dataset_id(), id);
        assert_eq!(ds.store.snapshot().len(), 2);
        assert!(!tmp.exists() && st.reserved_by("x").is_none());
        // registered: a restart opens it
        drop(ds);
        drop(st);
        let st = state(dir.path());
        assert_eq!(st.get("x").unwrap().store.dataset_id(), id);
    }

    #[test]
    fn replaces_in_place_and_keeps_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", DbType::Persistent).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-7");
        let id = restored(&tmp, 3);
        let ds = replace_in_place(&st, "ds", &tmp, "7", true).unwrap();
        assert_eq!(ds.store.dataset_id(), id);
        assert_eq!(st.get("ds").unwrap().store.snapshot().len(), 3);
        assert!(st.restoring.lock().is_empty());
        let db = dir.path().join("databases");
        assert!(!tmp.exists() && !db.join(".replaced-ds-7").exists());
        assert!(db.join(".kept-ds-7").join("dataset.json").exists());
    }

    #[test]
    fn busy_datasets_stay_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", DbType::Persistent).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        let before = ds.store.dataset_id();
        let tmp = dir.path().join("databases").join(".restore-ds-8");
        restored(&tmp, 3);
        // `ds` is held (a request in flight)
        let e = replace_with(&st, "ds", &tmp, "8", false, Duration::from_millis(50))
            .err()
            .unwrap();
        assert_eq!(e.code(), Code::DatasetBusy, "{e}");
        assert_eq!(st.get("ds").unwrap().store.dataset_id(), before);
        assert!(st.restoring.lock().is_empty());
        assert!(tmp.exists());
        drop(ds);
        // an in-memory or unknown dataset cannot be replaced
        st.attach("m", DbType::Mem, None).unwrap();
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
        let ds = st.create("ds", DbType::Persistent).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        let before = ds.store.dataset_id();
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-9");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("CURRENT"), b"gen-0042\n").unwrap();
        let e = replace_in_place(&st, "ds", &tmp, "9", false).err().unwrap();
        assert!(e.message().contains("does not open"), "{e}");
        let ds = st.get("ds").unwrap();
        assert_eq!(ds.store.dataset_id(), before);
        assert_eq!(ds.store.snapshot().len(), 1);
        let db = dir.path().join("databases");
        assert!(!db.join(".replaced-ds-9").exists() && !tmp.exists());
    }

    /// A crash after the first rename: the next start puts the old copy back.
    #[test]
    fn a_crash_between_the_renames_is_repaired_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", DbType::Persistent).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        update(&ds, "INSERT DATA { <urn:b> <urn:p> 2 }");
        let before = (ds.store.dataset_id(), ds.store.head_commit().seq);
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
        super::super::recover::startup(dir.path()).unwrap();
        let st = state(dir.path());
        let ds = st.get("ds").unwrap();
        assert_eq!((ds.store.dataset_id(), ds.store.head_commit().seq), before);
        assert!(!tmp.exists() && !db.join(".replaced-ds-4").exists());
    }

    /// A crash after both renames: the restored copy is the dataset, the old one goes.
    #[test]
    fn a_crash_after_both_renames_keeps_the_restored_copy() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", DbType::Persistent).unwrap();
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
        super::super::recover::startup(dir.path()).unwrap();
        let st = state(dir.path());
        assert_eq!(st.get("ds").unwrap().store.dataset_id(), id);
        assert!(!db.join(".replaced-ds-5").exists());
    }
}
