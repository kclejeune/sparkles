//! Startup recovery of interrupted restores, run before the registry's datasets are
//! opened (a crash between the two renames of an in-place restore leaves
//! `databases/<ds>` missing, which would stop the server):
//! * `databases/.restore-*` (an unfinished download or discarded rollback copy) is removed;
//! * `databases/.replaced-<ds>-<task>` carrying a `.restore-swap.json` marker is
//!   reconciled against the durable `config.json` revision (rolling back to the
//!   predecessor when the predecessor revision survived, or removing `.replaced-*`
//!   when the restored revision or a later registration survived);
//! * an unmarked legacy `databases/.replaced-<ds>-<task>` is reconciled against
//!   `config.json`: for a persistent row with a UUID, `databases` is synchronized
//!   under the live store lock before removing `.replaced-*` when `databases/<ds>`
//!   matches the UUID and has `CURRENT`, a sole matching predecessor is restored
//!   when it does not, and ambiguous identities fail closed; for a format 1
//!   persistent row without a UUID, roll-forward synchronizes `databases` under
//!   the live store lock before removing `.replaced-*` and a sole predecessor is
//!   restored with a WARN when `databases/<ds>` is absent; with no persistent
//!   registry row, obsolete `.replaced-*` copies are retired without populating
//!   `databases/<ds>`. A copy kept on purpose (`keepReplaced`) is renamed by the
//!   restore to `.kept-<ds>-<task>`, which this rule does not match;
//! * `tmp/verify-*` (the scratch directory of a `restore`-level verification) and
//!   `tmp/memory-backup-*` (the temporary copy of an in-memory dataset being backed
//!   up) are removed.

use super::delete;
use super::delete_fs::{Kind, RecoveryDir};
use super::{CONFIG_FILE, Registry};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read};
#[cfg(feature = "backup")]
use std::path::Path;
use uuid::Uuid;

/// Prefix of a restore's download directory: `.restore-<target>-<task>`.
pub const RESTORE_PREFIX: &str = ".restore-";
/// Prefix of the replaced directory of an in-place restore: `.replaced-<ds>-<task>`.
pub const REPLACED_PREFIX: &str = ".replaced-";
/// Prefix of a replaced directory kept with `keepReplaced`: `.kept-<ds>-<task>`.
#[cfg(feature = "backup")]
pub const KEPT_PREFIX: &str = super::KEPT_PREFIX;
/// Versioned marker stored inside `.replaced-<ds>-<task>` for registry-backed swaps.
pub(super) const SWAP_MARKER: &str = ".restore-swap.json";
const SWAP_MARKER_FORMAT: u32 = sparkles_core::formats::RESTORE_SWAP_MARKER.writes;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SwapMarker {
    pub(super) format: u32,
    pub(super) dataset: String,
    pub(super) predecessor_id: Uuid,
    pub(super) predecessor_revision: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) restored_id: Option<Uuid>,
    pub(super) restored_revision: Uuid,
}

#[cfg(feature = "backup")]
impl SwapMarker {
    pub(super) fn new(
        dataset: impl Into<String>,
        predecessor_id: Uuid,
        predecessor_revision: Uuid,
        restored_id: Option<Uuid>,
        restored_revision: Uuid,
    ) -> Self {
        Self {
            format: SWAP_MARKER_FORMAT,
            dataset: dataset.into(),
            predecessor_id,
            predecessor_revision,
            restored_id,
            restored_revision,
        }
    }
}

#[cfg(feature = "backup")]
pub(super) fn write_swap_marker(replaced: &Path, marker: &SwapMarker) -> Result<()> {
    let bytes = serde_json::to_vec(marker).map_err(|e| Error::invalid(e.to_string()))?;
    super::write_file_atomic(&replaced.join(SWAP_MARKER), &bytes)
}

#[cfg(all(feature = "backup", test))]
type MarkerRemoveHook = Box<dyn FnMut(&Path)>;
#[cfg(all(feature = "backup", test))]
thread_local! {
    pub(crate) static BEFORE_SWAP_MARKER_REMOVE: std::cell::RefCell<Option<MarkerRemoveHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "backup")]
pub(super) fn remove_swap_marker(replaced: &Path) -> Result<()> {
    #[cfg(test)]
    BEFORE_SWAP_MARKER_REMOVE.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(replaced);
        }
    });
    match std::fs::remove_file(replaced.join(SWAP_MARKER)) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    super::sync_dir(replaced)
}

fn read_swap_marker(replaced: &RecoveryDir, dataset: &str) -> Result<Option<SwapMarker>> {
    #[derive(Deserialize)]
    struct Header {
        format: serde_json::Value,
    }
    let corrupt = |error: &dyn std::fmt::Display| {
        Error::Corrupt(format!(
            "{}/{SWAP_MARKER}: {error}",
            replaced.path().display()
        ))
    };
    let mut file = match replaced.open_file(OsStr::new(SWAP_MARKER), false) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == ErrorKind::InvalidData => return Err(corrupt(&error)),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(corrupt(&"expected a JSON object"));
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(|e| corrupt(&e))?;
    let Some(format) = header.format.as_u64() else {
        return Err(corrupt(&"invalid format version"));
    };
    if format == 0 {
        return Err(corrupt(&"invalid format version 0"));
    }
    if format > u64::from(SWAP_MARKER_FORMAT) {
        return Err(Error::Unsupported(format!(
            "{}/{SWAP_MARKER} uses format {format}, which is newer than supported format {SWAP_MARKER_FORMAT}",
            replaced.path().display()
        )));
    }
    let marker: SwapMarker = serde_json::from_slice(&bytes).map_err(|e| corrupt(&e))?;
    if marker.format != SWAP_MARKER_FORMAT {
        return Err(corrupt(&"format version mismatch"));
    }
    if marker.dataset != dataset {
        return Err(corrupt(&format!(
            "dataset {:?} does not match directory for /{dataset}",
            marker.dataset
        )));
    }
    if marker.predecessor_revision == marker.restored_revision {
        return Err(corrupt(&"swap revisions must be distinct"));
    }
    Ok(Some(marker))
}

/// Whether `databases` contains any `.replaced-<ds>-<task>` directory with a
/// registry-backed swap marker (requiring `config.json` for recovery).
pub(super) fn has_marked_swaps(databases: &RecoveryDir) -> Result<bool> {
    for entry in databases.names()? {
        let name = entry?;
        let text = name.to_string_lossy();
        let Some(ds) = replaced_dataset(&text) else {
            continue;
        };
        let dir = databases.child(&name)?;
        if read_swap_marker(&dir, ds)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The dataset name of `.replaced-<ds>-<task>` (the task id is numeric).
pub fn replaced_dataset(file_name: &str) -> Option<&str> {
    let rest = file_name.strip_prefix(REPLACED_PREFIX)?;
    let (ds, task) = rest.rsplit_once('-')?;
    (!ds.is_empty() && !task.is_empty() && task.bytes().all(|b| b.is_ascii_digit())).then_some(ds)
}

/// Recover `<data_dir>/databases` after the catalog lock is held and before opening
/// any registered datasets.
pub(super) fn recover(
    catalog: &RecoveryDir,
    databases: &RecoveryDir,
    registry: &Registry,
    protected: &BTreeSet<String>,
) -> Result<()> {
    let mut restore_leftovers = Vec::new();
    let mut replaced_entries: BTreeMap<String, Vec<(RecoveryDir, Option<SwapMarker>)>> =
        BTreeMap::new();
    for entry in databases.names()? {
        let name = entry?;
        let text = name.to_string_lossy().into_owned();
        if text.starts_with(RESTORE_PREFIX) {
            restore_leftovers.push(databases.path().join(&name));
        } else if let Some(ds) = replaced_dataset(&text) {
            if protected.contains(ds) {
                continue;
            }
            let dir = databases.child(&name)?;
            let marker = read_swap_marker(&dir, ds)?;
            replaced_entries
                .entry(ds.to_string())
                .or_default()
                .push((dir, marker));
        }
    }

    if !restore_leftovers.is_empty() {
        for path in restore_leftovers {
            tracing::info!(
                target: "sparkles::backup",
                "removing unfinished restore {}",
                path.display()
            );
            std::fs::remove_dir_all(&path)?;
        }
        databases.sync()?;
    }

    // Make the observed registry durable before any swap removes either copy.
    if !replaced_entries.is_empty() {
        catalog.sync()?;
    }

    for (ds, entries) in replaced_entries {
        let mut rollbacks = Vec::new();
        let mut completions = Vec::new();
        let mut unmarked = Vec::new();
        let registered = delete::registered_dataset(registry, &ds);
        for (dir, marker) in entries {
            let Some(marker) = marker else {
                unmarked.push(dir);
                continue;
            };
            match registered {
                Some(row) if row.revision.is_none() => {
                    return Err(Error::Corrupt(format!(
                        "{CONFIG_FILE} is missing the swap revision for /{ds}"
                    )));
                }
                Some(row) if row.revision == Some(marker.predecessor_revision) => {
                    rollbacks.push((dir, marker, row));
                }
                Some(row) if row.revision == Some(marker.restored_revision) => {
                    completions.push((dir, marker, Some(row)));
                }
                _ => {
                    completions.push((dir, marker, None));
                }
            }
        }
        if rollbacks.len() > 1 {
            return Err(Error::Corrupt(format!(
                "multiple staged restore swaps match the registered revision of /{ds}"
            )));
        }

        // Roll back any uncommitted swap before cleaning up older completed swap copies.
        for (replaced_dir, marker, row) in rollbacks {
            if row.id.is_some_and(|id| id != marker.predecessor_id) {
                return Err(Error::Corrupt(format!(
                    "{CONFIG_FILE} identity {:?} disagrees with staged restore predecessor {}",
                    row.id, marker.predecessor_id
                )));
            }
            let mut replaced = delete::LockedDeleteDir::open(replaced_dir, marker.predecessor_id)?;
            let discard =
                if let Some(mut live) = delete::child_if_present(databases, OsStr::new(&ds))? {
                    if let Some(expected_restored) = marker.restored_id
                        && let Ok(Some(found)) = delete::dataset_id(&live)
                        && found != expected_restored
                        && found != marker.predecessor_id
                    {
                        return Err(Error::Corrupt(format!(
                            "live dataset /{ds} identity {found} does not match staged restore swap"
                        )));
                    }
                    let live_lock = delete::lock_dataset(&live)?;
                    #[cfg(not(unix))]
                    let live_lock = {
                        drop(live_lock);
                        None
                    };
                    #[cfg(unix)]
                    let live_lock = Some(live_lock);
                    let discard_name =
                        OsString::from(format!("{RESTORE_PREFIX}{ds}-rollback-{}", Uuid::new_v4()));
                    databases.rename_child(&mut live, &discard_name)?;
                    Some((live, live_lock))
                } else {
                    None
                };
            // Establish that `live` is durably absent before clearing the swap marker,
            // even when an earlier failed live rollback left it visibly missing without a sync.
            databases.sync()?;
            tracing::warn!(
                target: "sparkles::backup",
                "an in-place restore of /{ds} did not commit: putting back {}",
                replaced.path().display()
            );
            match replaced.dir().remove_file(OsStr::new(SWAP_MARKER)) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            replaced.dir().sync()?;
            replaced.rename(databases, &ds)?;
            databases.sync()?;
            drop(replaced);
            if let Some((discard_dir, live_lock)) = discard {
                let lock = match live_lock {
                    Some(lock) => lock,
                    None => delete::lock_dataset(&discard_dir)?,
                };
                databases.remove_child(&discard_dir, lock)?;
                databases.sync()?;
            }
        }

        for (replaced_dir, marker, committed_row) in completions {
            if let Some(row) = committed_row
                && let Some(expected_id) = row.id
                && let Some(live) = delete::child_if_present(databases, OsStr::new(&ds))?
            {
                let _live_lock = delete::lock_candidate(&live, expected_id, false)?;
            }
            tracing::info!(
                target: "sparkles::backup",
                "removing {} (the restore of /{ds} had committed)",
                replaced_dir.path().display()
            );
            let lock = delete::lock_candidate(&replaced_dir, marker.predecessor_id, true)?;
            databases.remove_child(&replaced_dir, lock)?;
            databases.sync()?;
        }

        if !unmarked.is_empty() {
            match registered {
                Some(row) if row.id.is_some() => {
                    let reg_id = row.id.expect("checked above");
                    let live = delete::child_if_present(databases, OsStr::new(&ds))?;
                    let live_id = match &live {
                        Some(dir) => delete::dataset_id(dir)?,
                        None => None,
                    };
                    let mut candidates = Vec::with_capacity(unmarked.len());
                    for dir in unmarked {
                        let id = delete::dataset_id(&dir)?;
                        candidates.push((dir, id));
                    }
                    match (live, live_id) {
                        (Some(live_dir), Some(id)) if id == reg_id => {
                            if !live_dir
                                .entry(OsStr::new("CURRENT"))
                                .is_ok_and(|entry| entry.kind == Kind::File)
                            {
                                return Err(Error::Corrupt(format!(
                                    "live dataset /{ds} is missing CURRENT while unmarked restore copies remain"
                                )));
                            }
                            let _live_lock = delete::lock_candidate(&live_dir, reg_id, false)?;
                            // Make the selected live directory name durable before touching any predecessor.
                            databases.sync()?;
                            for (replaced_dir, rep_id) in candidates {
                                tracing::info!(
                                    target: "sparkles::backup",
                                    "removing {} (the restore of /{ds} had finished)",
                                    replaced_dir.path().display()
                                );
                                let lock = match rep_id {
                                    Some(id) => delete::lock_candidate(&replaced_dir, id, true)?,
                                    None => delete::lock_dataset(&replaced_dir)?,
                                };
                                databases.remove_child(&replaced_dir, lock)?;
                                databases.sync()?;
                            }
                        }
                        (live, _) => {
                            if candidates.len() != 1 || candidates[0].1 != Some(reg_id) {
                                return Err(Error::Corrupt(format!(
                                    "unmarked restore swap for /{ds} does not match registered identity {reg_id}"
                                )));
                            }
                            let (replaced_dir, _) = candidates.pop().expect("single candidate");
                            let mut replaced = delete::LockedDeleteDir::open(replaced_dir, reg_id)?;
                            let discard = if let Some(mut live_dir) = live {
                                let live_lock = delete::lock_dataset(&live_dir)?;
                                #[cfg(not(unix))]
                                let live_lock = {
                                    drop(live_lock);
                                    None
                                };
                                #[cfg(unix)]
                                let live_lock = Some(live_lock);
                                let discard_name = OsString::from(format!(
                                    "{RESTORE_PREFIX}{ds}-rollback-{}",
                                    Uuid::new_v4()
                                ));
                                databases.rename_child(&mut live_dir, &discard_name)?;
                                databases.sync()?;
                                Some((live_dir, live_lock))
                            } else {
                                None
                            };
                            tracing::warn!(
                                target: "sparkles::backup",
                                "an unmarked in-place restore of /{ds} did not commit: putting back {}",
                                replaced.path().display()
                            );
                            replaced.rename(databases, &ds)?;
                            databases.sync()?;
                            drop(replaced);
                            if let Some((discard_dir, live_lock)) = discard {
                                let lock = match live_lock {
                                    Some(lock) => lock,
                                    None => delete::lock_dataset(&discard_dir)?,
                                };
                                databases.remove_child(&discard_dir, lock)?;
                                databases.sync()?;
                            }
                        }
                    }
                }
                Some(_) => {
                    // Legacy format 1 persistent row without a recorded UUID.
                    if let Some(live_dir) = delete::child_if_present(databases, OsStr::new(&ds))? {
                        let _live_lock = delete::lock_dataset(&live_dir)?;
                        databases.sync()?;
                        for replaced_dir in unmarked {
                            tracing::info!(
                                target: "sparkles::backup",
                                "removing {} (the restore of /{ds} had finished)",
                                replaced_dir.path().display()
                            );
                            let lock = delete::lock_dataset(&replaced_dir)?;
                            databases.remove_child(&replaced_dir, lock)?;
                            databases.sync()?;
                        }
                    } else {
                        if unmarked.len() != 1 {
                            return Err(Error::Corrupt(format!(
                                "multiple unmarked restore copies remain for legacy dataset /{ds}"
                            )));
                        }
                        let mut replaced_dir = unmarked.pop().expect("single candidate");
                        let lock = delete::lock_dataset(&replaced_dir)?;
                        #[cfg(not(unix))]
                        drop(lock);
                        tracing::warn!(
                            target: "sparkles::backup",
                            "an in-place restore of /{ds} was interrupted: putting back {}",
                            replaced_dir.path().display()
                        );
                        databases.rename_child(&mut replaced_dir, OsStr::new(&ds))?;
                        databases.sync()?;
                        #[cfg(unix)]
                        drop(lock);
                    }
                }
                None => {
                    // No persistent registry row owns this alias: retire obsolete staged
                    // copies without populating the live path.
                    databases.sync()?;
                    for replaced_dir in unmarked {
                        tracing::info!(
                            target: "sparkles::backup",
                            "removing obsolete {} (/{ds} is not a registered persistent dataset)",
                            replaced_dir.path().display()
                        );
                        let lock = delete::lock_dataset(&replaced_dir)?;
                        databases.remove_child(&replaced_dir, lock)?;
                        databases.sync()?;
                    }
                }
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(catalog.path().join("tmp")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("verify-") || name.starts_with(crate::store::MEMORY_CAPTURE_PREFIX)
            {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

#[cfg(feature = "backup")]
impl super::Catalog {
    /// Publish a downloaded restore under a reservation made before queuing work.
    #[doc(hidden)]
    pub fn publish_restored(
        &self,
        reservation: super::Reservation,
        restored: &Path,
    ) -> crate::Result<crate::Dataset> {
        // A close callback's refusal keeps the staged directory and its error kind.
        self.check_manageable()?;
        super::swap::publish_new(self, reservation, restored).map_err(crate::backup::error)
    }
    #[doc(hidden)]
    pub fn replace_restored_reserved(
        &self,
        reservation: super::Reservation,
        restored: &Path,
        task: &str,
        keep: bool,
    ) -> crate::Result<crate::Dataset> {
        // A close callback's refusal comes before any drain or error translation.
        self.check_manageable()?;
        if !std::sync::Arc::ptr_eq(&self.inner, &reservation.catalog.inner)
            || self.restoring_by(reservation.name()).as_deref() != Some(task)
        {
            return Err(crate::Error::invalid("invalid restore reservation"));
        }
        super::swap::replace_reserved(self, reservation, restored, task, keep)
            .map_err(crate::backup::error)
    }
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn download_restore(
        &self,
        repo: &crate::backup::Repository,
        backup: &str,
        target: &str,
        req: &crate::backup::RestoreRequest,
        tmp: &Path,
        store_opts: crate::store::StoreOptions,
        ctl: &crate::task::Control,
    ) -> crate::Result<crate::backup::RestoreReport> {
        use crate::backup::{BackupError, Code, Identity, RestoreOptions, block_on, error};
        ctl.check()?;
        let manifest = block_on(repo.manifest(backup))?.map_err(error)?;
        if req.identity == Identity::Keep
            && self
                .list()
                .iter()
                .any(|d| d.id == manifest.dataset.id && !(req.replace && d.name == target))
        {
            return Err(error(BackupError::new(
                Code::DuplicateDatasetId,
                "another dataset has the backup's identity",
            )));
        }
        let in_place_head = self
            .get(target)
            .filter(|d| req.replace && d.dataset_id() == manifest.dataset.id)
            .map(|d| d.head_commit().seq);
        let cat = self.clone();
        let o = RestoreOptions {
            identity: req.identity,
            check: req.check,
            id_in_use: std::sync::Arc::new(move |id| cat.get_by_id(id).is_some()),
            in_place_head,
            store_opts,
            ctl: ctl.into(),
        };
        block_on(repo.restore(backup, tmp, &o))?.map_err(error)
    }
    /// Download, verify and publish a repository backup. For an in-place restore,
    /// existing handles must be released before the old store can be closed.
    pub fn restore(
        &self,
        repo: &crate::backup::Repository,
        backup: &str,
        req: &crate::backup::RestoreRequest,
        ctl: &crate::task::Control,
    ) -> crate::Result<crate::Dataset> {
        self.restore_report(repo, backup, req, ctl)
            .map(|(ds, _)| ds)
    }
    #[doc(hidden)]
    pub fn restore_report(
        &self,
        repo: &crate::backup::Repository,
        backup: &str,
        req: &crate::backup::RestoreRequest,
        ctl: &crate::task::Control,
    ) -> crate::Result<(crate::Dataset, crate::backup::RestoreReport)> {
        use crate::backup::{BackupError, Code, Identity, block_on, error};
        ctl.check()?;
        let manifest = block_on(repo.manifest(backup))?.map_err(error)?;
        let target = req.target.as_deref().unwrap_or(&manifest.dataset.name);
        super::check_name(target)?;
        let existing = self.get(target);
        if req.replace {
            let info = self
                .info(target)
                .ok_or_else(|| crate::Error::NotFound(format!("no dataset /{target}")))?;
            if info.kind != super::DatasetKind::Persistent || info.attached {
                return Err(error(BackupError::new(
                    Code::NotManaged,
                    "only managed persistent datasets can be replaced",
                )));
            }
        } else if existing.is_some() {
            return Err(crate::Error::Conflict(format!(
                "dataset /{target} is already registered"
            )));
        }
        if req.identity == Identity::Keep
            && self
                .list()
                .iter()
                .any(|d| d.id == manifest.dataset.id && !(req.replace && d.name == target))
        {
            return Err(error(BackupError::new(
                Code::DuplicateDatasetId,
                "another dataset has the backup's identity",
            )));
        }
        drop(existing);
        let holder = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let reservation = self.reserve(target, super::ReservationKind::Restore, &holder)?;
        let tmp = self.database_dir(&format!(".restore-{target}-{holder}"))?;
        let result = (|| {
            let report = self.download_restore(
                repo,
                backup,
                target,
                req,
                &tmp,
                self.inner.options.read().dataset.store.clone(),
                &ctl.part(0.0, 0.9),
            )?;
            ctl.check()?;
            let ds = if req.replace {
                self.replace_restored_reserved(reservation, &tmp, &holder, req.keep_replaced)?
            } else {
                self.publish_restored(reservation, &tmp)?
            };
            ctl.progress.report(1.0, "restored");
            Ok((ds, report))
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(replaced_dataset(".replaced-ds-12"), Some("ds"));
        assert_eq!(replaced_dataset(".replaced-my-ds.v2-3"), Some("my-ds.v2"));
        assert_eq!(replaced_dataset(".replaced-ds-x"), None);
        assert_eq!(replaced_dataset(".replaced--1"), None);
        assert_eq!(replaced_dataset(".kept-ds-1"), None);
        assert_eq!(replaced_dataset("ds"), None);
    }

    #[test]
    fn leftovers_are_cleaned_up() {
        use super::super::{DatasetKind, RegistryEntry};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("databases");
        for d in [
            ".restore-x-4",
            ".replaced-a-5",
            "a",
            ".replaced-b-6",
            ".replaced-z-8",
            ".kept-c-7",
        ] {
            std::fs::create_dir_all(db.join(d)).unwrap();
        }
        std::fs::write(db.join(".replaced-b-6").join("marker"), b"old b").unwrap();
        std::fs::create_dir_all(dir.path().join("tmp").join("verify-9")).unwrap();
        std::fs::create_dir_all(dir.path().join("tmp").join("memory-backup-x1")).unwrap();
        let catalog = RecoveryDir::open(dir.path()).unwrap();
        let databases = delete::open_databases(&catalog).unwrap();
        let legacy_registry = Registry {
            datasets: vec![
                RegistryEntry {
                    name: "a".into(),
                    kind: DatasetKind::Persistent,
                    id: None,
                    revision: None,
                    reasoning: None,
                },
                RegistryEntry {
                    name: "b".into(),
                    kind: DatasetKind::Persistent,
                    id: None,
                    revision: None,
                    reasoning: None,
                },
            ],
        };
        recover(&catalog, &databases, &legacy_registry, &Default::default()).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&db)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".kept-c-7", "a", "b"]);
        assert_eq!(
            std::fs::read(db.join("b").join("marker")).unwrap(),
            b"old b"
        );
        assert!(!dir.path().join("tmp").join("verify-9").exists());
        assert!(!dir.path().join("tmp").join("memory-backup-x1").exists());
        // nothing to do: fine on an empty databases directory
        recover(
            &catalog,
            &databases,
            &Registry::default(),
            &Default::default(),
        )
        .unwrap();
    }
}
