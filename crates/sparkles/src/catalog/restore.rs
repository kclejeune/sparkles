//! Startup recovery of interrupted restores, run before the registry's datasets are
//! opened (a crash between the two renames of an in-place restore leaves
//! `databases/<ds>` missing, which would stop the server):
//! * `databases/.restore-*` (an unfinished download) is removed;
//! * `databases/.replaced-<ds>-<task>` with no `databases/<ds>` is renamed back to
//!   `<ds>` with a WARN (the crash came between the renames);
//! * `databases/.replaced-<ds>-<task>` next to a `databases/<ds>` is removed (the swap
//!   finished before the crash). A copy kept on purpose (`keepReplaced`) is renamed
//!   by the restore to `.kept-<ds>-<task>`, which this rule does not match;
//! * `tmp/verify-*` (the scratch directory of a `restore`-level verification) and
//!   `tmp/memory-backup-*` (the temporary copy of an in-memory dataset being backed
//!   up) are removed.

use anyhow::Context;
use std::path::Path;

/// Prefix of a restore's download directory: `.restore-<target>-<task>`.
pub const RESTORE_PREFIX: &str = ".restore-";
/// Prefix of the replaced directory of an in-place restore: `.replaced-<ds>-<task>`.
pub const REPLACED_PREFIX: &str = ".replaced-";
/// Prefix of a replaced directory kept with `keepReplaced`: `.kept-<ds>-<task>`.
pub const KEPT_PREFIX: &str = ".kept-";

/// The dataset name of `.replaced-<ds>-<task>` (the task id is numeric).
pub fn replaced_dataset(file_name: &str) -> Option<&str> {
    let rest = file_name.strip_prefix(REPLACED_PREFIX)?;
    let (ds, task) = rest.rsplit_once('-')?;
    (!ds.is_empty() && !task.is_empty() && task.bytes().all(|b| b.is_ascii_digit())).then_some(ds)
}

/// Recover `<data_dir>/databases` after the catalog lock is held and before opening
/// any registered datasets.
pub fn recover(data_dir: &Path) -> anyhow::Result<()> {
    let databases = data_dir.join("databases");
    let entries = match std::fs::read_dir(&databases) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", databases.display())),
    };
    let mut changed = false;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name.starts_with(RESTORE_PREFIX) {
            tracing::info!(target: "sparkles::backup", "removing unfinished restore {}", path.display());
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("removing {}", path.display()))?;
            changed = true;
        } else if let Some(ds) = replaced_dataset(&name) {
            let target = databases.join(ds);
            if target.exists() {
                tracing::info!(
                    target: "sparkles::backup",
                    "removing {} (the restore of /{ds} had finished)",
                    path.display()
                );
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            } else {
                tracing::warn!(
                    target: "sparkles::backup",
                    "an in-place restore of /{ds} was interrupted: putting back {}",
                    path.display()
                );
                std::fs::rename(&path, &target).with_context(|| {
                    format!("renaming {} to {}", path.display(), target.display())
                })?;
            }
            changed = true;
        }
    }
    if changed {
        super::sync_dir(&databases)?;
    }
    if let Ok(entries) = std::fs::read_dir(data_dir.join("tmp")) {
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

impl super::Catalog {
    /// Publish a downloaded restore under a reservation made before queuing work.
    #[doc(hidden)]
    pub fn publish_restored(
        &self,
        reservation: super::Reservation,
        restored: &Path,
    ) -> crate::Result<crate::Dataset> {
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
        if !std::sync::Arc::ptr_eq(&self.inner, &reservation.catalog.inner)
            || self.restoring_by(reservation.name()).as_deref() != Some(task)
        {
            return Err(crate::Error::invalid("invalid restore reservation"));
        }
        super::swap::replace_in_place(self, reservation.name(), restored, task, keep)
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
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("databases");
        for d in [
            ".restore-x-4",
            ".replaced-a-5",
            "a",
            ".replaced-b-6",
            ".kept-c-7",
        ] {
            std::fs::create_dir_all(db.join(d)).unwrap();
        }
        std::fs::write(db.join(".replaced-b-6").join("marker"), b"old b").unwrap();
        std::fs::create_dir_all(dir.path().join("tmp").join("verify-9")).unwrap();
        std::fs::create_dir_all(dir.path().join("tmp").join("memory-backup-x1")).unwrap();
        recover(dir.path()).unwrap();
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
        // nothing to do: fine, also without a databases directory
        recover(dir.path()).unwrap();
        recover(&dir.path().join("none")).unwrap();
    }
}
