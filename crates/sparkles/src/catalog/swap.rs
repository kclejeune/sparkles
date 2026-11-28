//! Durable publication of restored catalog datasets. New datasets are renamed into
//! place and registered under their reservation. Replacement waits for live dataset
//! and branch handles, closes the store, swaps two directories and reopens it. A failed
//! open rolls back both renames. Catalog::open repairs interrupted swaps while holding
//! the directory lock.

use super::delete::KEEP_REPLACED_MARKER;
use super::restore::{
    KEPT_PREFIX, REPLACED_PREFIX, RESTORE_PREFIX, SWAP_MARKER, SwapMarker, remove_keep_marker,
    remove_swap_marker, write_keep_marker, write_swap_marker,
};
use super::{Catalog, RegistryCommitError, Reservation, ReservationKind, sync_dir};
use crate::Dataset;
use sparkles_backup::{BackupError, Code};
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long an in-place restore waits for requests to release the dataset.
pub const DRAIN: Duration = Duration::from_secs(30);

fn io(what: &str, path: &Path, e: std::io::Error) -> BackupError {
    BackupError::internal(format!("{what} {}: {e}", path.display()))
}

fn check_marker_path(root: &Path, name: &str) -> Result<(), BackupError> {
    let path = root.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Ok(_) => Err(BackupError::internal(format!(
            "cannot publish restore swap marker at {}: path exists and is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io("checking restore swap marker", &path, error)),
    }
}

fn check_marker_paths(root: &Path, keep_replaced: bool) -> Result<(), BackupError> {
    check_marker_path(root, SWAP_MARKER)?;
    if keep_replaced {
        check_marker_path(root, KEEP_REPLACED_MARKER)?;
    }
    Ok(())
}

/// Publish `restored` as the new dataset of `reservation` (renamed into
/// `databases/<name>`, then adopted). If registry publication becomes indeterminate,
/// the restored directory stays in place and the alias requires startup recovery.
pub fn publish_new(
    st: &Catalog,
    reservation: Reservation,
    restored: &Path,
) -> Result<Dataset, BackupError> {
    st.check_reservation(&reservation)
        .map_err(|e| BackupError::internal(e.to_string()))?;
    // A refused registration must not follow a published directory.
    st.check_manageable()
        .map_err(|e| BackupError::new(Code::DatasetBusy, e.to_string()))?;
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
    // Only a failure before the registry replacement permits removing the root.
    st.adopt(reservation)
        .map_err(|e| BackupError::internal(format!("registering /{name}: {e:#}")))
}

/// Replace the registered persistent dataset `name` with `restored` (see the module
/// docs). `task` names the `.replaced-` directory; `keep_replaced` keeps it.
#[cfg(test)]
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
type RollbackHook = Box<dyn FnMut()>;
#[cfg(test)]
thread_local! {
    /// Stop the swap after rename 1, 2 or 3 without cleaning up, as a crash would.
    pub(crate) static CRASH_AFTER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// Inject a failure on the first live rollback rename after a pre-replacement error.
    pub(crate) static FAIL_ROLLBACK_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Runs at the start of live `BeforeReplace` rollback before moving `root` aside.
    pub(crate) static ROLLBACK_HOOK: std::cell::RefCell<Option<RollbackHook>> =
        const { std::cell::RefCell::new(None) };
}

fn rollback_rename_fail_point() -> std::io::Result<()> {
    #[cfg(test)]
    ROLLBACK_HOOK.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook();
        }
    });
    #[cfg(test)]
    if FAIL_ROLLBACK_RENAME.replace(false) {
        return Err(std::io::Error::other(
            "injected swap rollback rename failure",
        ));
    }
    Ok(())
}

fn crash_point(_n: u8) -> Result<(), BackupError> {
    #[cfg(test)]
    if CRASH_AFTER.get() == _n {
        return Err(BackupError::internal(format!("crashed after rename {_n}")));
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseStage {
    /// Before the drain of an attempt.
    Attempt,
    /// After the drain, immediately before the ownership proof.
    Proof,
}
#[cfg(test)]
type CloseHook = Box<dyn FnMut(CloseStage, u32)>;
#[cfg(test)]
thread_local! {
    /// Runs at each stage of each close attempt of an in-place swap.
    pub(crate) static CLOSE_HOOK: std::cell::RefCell<Option<CloseHook>> =
        const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn close_hook(stage: CloseStage, attempt: u32) {
    CLOSE_HOOK.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(stage, attempt);
        }
    });
}

/// Replace the registered persistent dataset of `reservation` with `restored` (see
/// the module docs), under that restore reservation. The swap owns the reservation
/// and releases it before the operations that close callbacks deferred run.
pub(crate) fn replace_reserved(
    st: &Catalog,
    reservation: Reservation,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
    on_closed: impl FnMut(u64, &crate::store::Store),
) -> Result<Dataset, BackupError> {
    let name = reservation.name().to_string();
    swap(
        st,
        &name,
        restored,
        task,
        keep_replaced,
        DRAIN,
        (Some(reservation), on_closed),
    )
}

/// [`replace_in_place`] with its drain time, without a reservation of its own.
#[cfg(test)]
pub(crate) fn replace_with(
    st: &Catalog,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
    drain: Duration,
) -> Result<Dataset, BackupError> {
    swap(
        st,
        name,
        restored,
        task,
        keep_replaced,
        drain,
        (None, |_, _| {}),
    )
}

fn swap(
    st: &Catalog,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
    drain: Duration,
    (reservation, mut on_closed): (Option<Reservation>, impl FnMut(u64, &crate::store::Store)),
) -> Result<Dataset, BackupError> {
    let _span = tracing::info_span!(
        "restore.swap",
        "db.namespace" = name,
        "sparkles.task" = task
    )
    .entered();
    // Operations that close callbacks defer run once the whole swap returns, after
    // the reservation below is released, because it is declared after the scope.
    let _batch = st.batch_scope();
    let _restoring = if reservation.is_some() {
        reservation
    } else if st.restoring_by(name).as_deref() == Some(task) {
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
    let detached = st
        .detach_for_swap(name)
        .map_err(|e| BackupError::new(Code::DatasetBusy, e.to_string()))?;
    let Some(old) = detached else {
        return Err(match st.get(name) {
            Some(_) => BackupError::new(
                Code::NotManaged,
                format!("/{name} is not a persistent dataset of this server's data directory"),
            ),
            None => BackupError::internal(format!("no dataset /{name}")),
        });
    };
    let old_id = old.store().dataset_id();
    let Some(root) = old.store().root().map(Path::to_path_buf) else {
        st.reinsert(name, old);
        return Err(BackupError::new(
            Code::NotManaged,
            format!("/{name} is an in-memory dataset"),
        ));
    };
    if let Err(err) = check_marker_paths(&root, keep_replaced) {
        st.reinsert(name, old);
        return Err(err);
    }
    // A restore intentionally publishes another identity. Read only its scalar
    // identity before closing the old graph; the existing-only opener validates
    // the complete header and the same UUID under its process lock later.
    #[derive(serde::Deserialize)]
    struct RestoredIdentity {
        id: uuid::Uuid,
    }
    let restored_id = (|| {
        let file = match std::fs::File::open(restored.join("dataset.json")) {
            Ok(file) => file,
            // Genuine legacy CURRENT roots have no header. The existing-only
            // opener retains their supported metadata upgrade, without ever
            // creating a missing root or empty generation.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        serde_json::from_reader::<_, RestoredIdentity>(std::io::BufReader::new(file))
            .map(|header| Some(header.id))
            .map_err(std::io::Error::other)
    })();
    let restored_id = match restored_id {
        Ok(id) => id,
        Err(error) => {
            st.reinsert(name, old);
            return Err(io("reading restored identity from", restored, error));
        }
    };
    // Drain is only a preflight. The final nonblocking proof must also exclude
    // raw Strong/Weak Store and BranchSet aliases before directory publication.
    // Built-in owners such as a views worker round take short-lived strong
    // references that the preflight cannot exclude, so a refused proof waits and
    // retries within the same drain time. Refusal restores every owner exactly.
    let mut old = old;
    #[cfg(test)]
    let mut attempt = 0u32;
    let reasoning = loop {
        #[cfg(test)]
        {
            attempt += 1;
            close_hook(CloseStage::Attempt, attempt);
        }
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
        #[cfg(test)]
        close_hook(CloseStage::Proof, attempt);
        let closed = {
            let _disposing = st.disposal_scope();
            old.close_for_disposal(|| {}, &mut on_closed)
        };
        match closed {
            Ok(reasoning) => break reasoning,
            Err(refused) => {
                old = refused.dataset;
                if let Some(panic) = refused.panic {
                    st.reinsert(name, old);
                    std::panic::resume_unwind(panic);
                }
                if t0.elapsed() >= drain {
                    st.reinsert(name, old);
                    return Err(BackupError::new(
                        Code::DatasetBusy,
                        "the source family still has live owners; the dataset is unchanged",
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    if let Some(detached) = st.inner.detached.lock().get_mut(name) {
        detached.reasoning = reasoning.into();
    }
    let predecessor_revision = match st.bind_swap_revision(name, old_id) {
        Ok(rev) => rev,
        Err(error) => {
            let err = BackupError::internal(format!(
                "recording the predecessor revision of /{name}: {error:#}"
            ));
            return Err(reopen_after(st, name, err));
        }
    };
    let restored_revision = Uuid::new_v4();
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
    let kept_name = keep_replaced.then(|| format!("{KEPT_PREFIX}{name}-{task}"));
    if let Err(err) = check_marker_paths(&root, keep_replaced) {
        return Err(reopen_after(st, name, err));
    }
    if let Err(e) = std::fs::rename(&root, &replaced) {
        let err = io("moving aside", &root, e);
        return Err(reopen_after(st, name, err));
    }
    crash_point(1)?;
    let marker = SwapMarker::new(
        name,
        old_id,
        predecessor_revision,
        restored_id,
        restored_revision,
    );
    // Make rename 1 durable before writing markers into `replaced` so a power
    // loss cannot lose rename 1 while leaving markers inside `root`. Publish
    // and sync `.keep-replaced-v1` before publishing the replacement or
    // committing the registry so a `keepReplaced` swap has a durable retention
    // decision before the swap can commit or clear `.restore-swap.json`.
    let prepared = sync_dir(&databases)
        .and_then(|()| write_swap_marker(&replaced, &marker))
        .and_then(|()| {
            if let Some(kept_name) = &kept_name {
                write_keep_marker(&replaced, kept_name)
            } else {
                Ok(())
            }
        });
    if let Err(error) = prepared {
        let err = BackupError::internal(format!(
            "marking the in-place restore swap of /{name}: {error:#}"
        ));
        let rollback = rollback_rename_fail_point()
            .map_err(crate::Error::from)
            .and_then(|()| {
                if keep_replaced {
                    remove_keep_marker(&replaced)
                } else {
                    Ok(())
                }
            })
            .and_then(|()| remove_swap_marker(&replaced))
            .and_then(|()| std::fs::rename(&replaced, &root).map_err(crate::Error::from))
            .and_then(|()| sync_dir(&databases));
        if let Err(e2) = rollback {
            let message = format!(
                "{err}; putting back {} failed too: {e2} (the next start repairs it)",
                replaced.display()
            );
            st.mark_swap_rollback_failed(name, message.clone());
            return Err(BackupError::internal(message));
        }
        return Err(reopen_after(st, name, err));
    }
    if let Err(e) = std::fs::rename(restored, &root) {
        let err = io("moving the restored copy to", &root, e);
        let rollback = if keep_replaced {
            remove_keep_marker(&replaced)
        } else {
            Ok(())
        }
        .and_then(|()| remove_swap_marker(&replaced))
        .and_then(|()| std::fs::rename(&replaced, &root).map_err(crate::Error::from))
        .and_then(|()| sync_dir(&databases));
        if let Err(e2) = rollback {
            let message = format!(
                "{err}; putting back {} failed too: {e2} (the next start repairs it)",
                replaced.display()
            );
            st.mark_swap_rollback_failed(name, message.clone());
            return Err(BackupError::internal(message));
        }
        return Err(reopen_after(st, name, err));
    }
    crash_point(2)?;
    let published = sync_dir(&databases)
        .map_err(RegistryCommitError::BeforeReplace)
        .and_then(|()| st.commit_restored_dataset(name, restored_id, restored_revision));
    match published {
        Ok(ds) => {
            crash_point(3)?;
            let cleanup = if let Some(kept_name) = kept_name {
                let kept = databases.join(&kept_name);
                // `.keep-replaced-v1` is already durably synced inside `replaced`.
                // Clear `.restore-swap.json` first so the retained copy never
                // carries a stale swap marker into `.kept-*`.
                remove_swap_marker(&replaced)
                    .and_then(|()| {
                        if std::fs::symlink_metadata(&kept).is_ok() {
                            return Err(crate::Error::from(std::io::Error::new(
                                std::io::ErrorKind::AlreadyExists,
                                format!(
                                    "keepReplaced destination {} already exists",
                                    kept.display()
                                ),
                            )));
                        }
                        std::fs::rename(&replaced, &kept).map_err(crate::Error::from)
                    })
                    .and_then(|()| sync_dir(&databases))
            } else {
                std::fs::remove_dir_all(&replaced)
                    .map_err(crate::Error::from)
                    .and_then(|()| sync_dir(&databases))
            };
            if let Err(e) = cleanup {
                if keep_replaced {
                    tracing::warn!(
                        target: "sparkles::backup",
                        "{}: {e} (the next start finishes retaining it)",
                        replaced.display()
                    );
                } else {
                    tracing::warn!(
                        target: "sparkles::backup",
                        "{}: {e} (the next start removes it)",
                        replaced.display()
                    );
                }
            }
            Ok(ds)
        }
        Err(RegistryCommitError::Indeterminate(error)) => {
            // Both names were synchronized before the register write. Keep
            // the predecessor available for startup to reconcile the swap.
            Err(BackupError::internal(format!(
                "publishing the restored /{name}: {error}"
            )))
        }
        Err(RegistryCommitError::BeforeReplace(error)) => {
            let failed = databases.join(format!("{RESTORE_PREFIX}{name}-{task}"));
            // Synchronize moving the restored copy aside before removing markers
            // so power loss cannot leave both live C and unmarked staged B.
            // Clear `.keep-replaced-v1` before `.restore-swap.json` so power loss
            // cannot leave an orphan keep marker without its swap marker.
            let back = rollback_rename_fail_point()
                .and_then(|()| std::fs::rename(&root, &failed))
                .map_err(crate::Error::from)
                .and_then(|()| sync_dir(&databases))
                .and_then(|()| {
                    if keep_replaced {
                        remove_keep_marker(&replaced)
                    } else {
                        Ok(())
                    }
                })
                .and_then(|()| remove_swap_marker(&replaced))
                .and_then(|()| std::fs::rename(&replaced, &root).map_err(crate::Error::from))
                .and_then(|()| sync_dir(&databases));
            let err = BackupError::internal(format!("publishing the restored /{name}: {error}"));
            match back {
                Ok(()) => {
                    let _ = std::fs::remove_dir_all(&failed);
                    Err(reopen_after(st, name, err))
                }
                Err(e2) => {
                    let message = format!(
                        "{err}; putting back the old copy failed: {e2} (the next start repairs it)"
                    );
                    st.mark_swap_rollback_failed(name, message.clone());
                    Err(BackupError::internal(message))
                }
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
    fn a_transient_built_in_owner_at_the_proof_is_waited_for() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        let reference = ds.state_reference();
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-8");
        restored(&tmp, 3);
        // A views worker round upgrades the state for its duration. Take that
        // strong reference after the drain and release it before the next attempt.
        let stages = Rc::new(RefCell::new(Vec::new()));
        let seen = stages.clone();
        let alias = Rc::new(RefCell::new(None));
        let held = alias.clone();
        super::CLOSE_HOOK.set(Some(Box::new(move |stage, attempt| {
            seen.borrow_mut().push((stage, attempt));
            match (stage, attempt) {
                (super::CloseStage::Proof, 1) => *held.borrow_mut() = reference.upgrade(),
                (super::CloseStage::Attempt, 2) => drop(held.borrow_mut().take().unwrap()),
                _ => {}
            }
        })));
        let swapped = replace_with(&st, "ds", &tmp, "8", false, Duration::from_secs(10));
        super::CLOSE_HOOK.set(None);
        assert!(alias.borrow().is_none());
        let ds = swapped.unwrap();
        assert_eq!(ds.len(), 3);
        assert_eq!(
            *stages.borrow(),
            [
                (super::CloseStage::Attempt, 1),
                (super::CloseStage::Proof, 1),
                (super::CloseStage::Attempt, 2),
                (super::CloseStage::Proof, 2),
            ]
        );
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
        assert_eq!(st.closed(), ["ds"]);
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
        let st = state(dir.path());
        let ds = st.get("ds").unwrap();
        assert_eq!(
            (ds.store().dataset_id(), ds.store().head_commit().seq),
            before
        );
        assert!(!tmp.exists() && !db.join(".replaced-ds-4").exists());
    }

    /// A crash after both renames and before registry commit rolls back to the predecessor.
    #[test]
    fn a_crash_after_both_renames_before_commit_restores_the_predecessor() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        let before = (ds.store().dataset_id(), ds.store().head_commit().seq);
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-5");
        let new_id = restored(&tmp, 2);
        assert_ne!(new_id, before.0);
        CRASH_AFTER.set(2);
        let r = replace_in_place(&st, "ds", &tmp, "5", false);
        CRASH_AFTER.set(0);
        assert!(r.is_err());
        drop(st);
        let db = dir.path().join("databases");
        assert!(db.join("ds").exists() && db.join(".replaced-ds-5").exists());
        let st = state(dir.path());
        let ds = st.get("ds").unwrap();
        assert_eq!(
            (ds.store().dataset_id(), ds.store().head_commit().seq),
            before
        );
        assert!(!db.join(".replaced-ds-5").exists());
        assert!(
            !db.join("ds")
                .join(super::super::restore::SWAP_MARKER)
                .exists()
        );
    }

    /// A crash after the registry commit keeps the restored copy and removes the predecessor.
    #[test]
    fn a_crash_after_registry_commit_keeps_the_restored_copy() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-6");
        let id = restored(&tmp, 2);
        CRASH_AFTER.set(3);
        let r = replace_in_place(&st, "ds", &tmp, "6", false);
        CRASH_AFTER.set(0);
        assert!(r.is_err());
        drop(st);
        let db = dir.path().join("databases");
        assert!(db.join("ds").exists() && db.join(".replaced-ds-6").exists());
        let st = state(dir.path());
        let ds = st.get("ds").unwrap();
        assert_eq!((ds.store().dataset_id(), ds.len()), (id, 2));
        assert!(!db.join(".replaced-ds-6").exists());
    }

    /// An older unmarked swap without a swap marker preserves legacy roll-forward behavior.
    #[test]
    fn an_unmarked_legacy_swap_preserves_roll_forward() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        drop(ds);
        drop(st);
        let db = dir.path().join("databases");
        std::fs::rename(db.join("ds"), db.join(".replaced-ds-5")).unwrap();
        let id = restored(&db.join("ds"), 2);
        // Legacy format 1 registry had no dataset UUID or swap revision.
        std::fs::write(
            dir.path().join(super::super::CONFIG_FILE),
            r#"{"format":1,"datasets":[{"name":"ds","type":"persistent"}]}"#,
        )
        .unwrap();
        let st = state(dir.path());
        let ds = st.get("ds").unwrap();
        assert_eq!((ds.store().dataset_id(), ds.len()), (id, 2));
        assert!(!db.join(".replaced-ds-5").exists());
    }

    /// Missing config.json or a future swap marker format fails closed without discarding copies.
    #[test]
    fn marked_swap_requires_registry_and_rejects_future_marker_format() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.create("ds", &CreateDataset::default()).unwrap();
        update(&ds, "INSERT DATA { <urn:a> <urn:p> 1 }");
        drop(ds);
        let tmp = dir.path().join("databases").join(".restore-ds-7");
        restored(&tmp, 2);
        CRASH_AFTER.set(2);
        assert!(replace_in_place(&st, "ds", &tmp, "7", false).is_err());
        CRASH_AFTER.set(0);
        drop(st);

        let config = dir.path().join(super::super::CONFIG_FILE);
        let saved_config = std::fs::read(&config).unwrap();
        let db = dir.path().join("databases");
        let replaced = db.join(".replaced-ds-7");
        let marker = replaced.join(super::super::restore::SWAP_MARKER);

        // Missing config.json fails closed and preserves both directories.
        std::fs::remove_file(&config).unwrap();
        let err = Catalog::open(dir.path(), StoreOptions::default().into())
            .err()
            .unwrap();
        assert!(matches!(err, crate::Error::Corrupt(_)), "{err:?}");
        assert!(db.join("ds/CURRENT").exists() && replaced.join("CURRENT").exists());
        std::fs::write(&config, &saved_config).unwrap();

        // Future swap marker format fails closed with Unsupported and preserves both directories.
        let saved_marker = std::fs::read(&marker).unwrap();
        std::fs::write(&marker, br#"{"format":2,"dataset":"ds"}"#).unwrap();
        let err = Catalog::open(dir.path(), StoreOptions::default().into())
            .err()
            .unwrap();
        assert!(matches!(err, crate::Error::Unsupported(_)), "{err:?}");
        assert!(db.join("ds/CURRENT").exists() && replaced.join("CURRENT").exists());

        // Restoring the valid marker allows clean rollback recovery.
        std::fs::write(&marker, &saved_marker).unwrap();
        let st = state(dir.path());
        assert_eq!(st.get("ds").unwrap().len(), 1);
        assert!(!replaced.exists());
    }
}
