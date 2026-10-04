//! The dataset's backups in a repository (feature `backup`).

use crate::Dataset;
use crate::backup::{
    BackupSummary, Code, ListFilter, Manifest, Repository, VerifyOptions, VerifyReport, block_on,
    error,
};
use crate::error::Result;
use crate::task::Control;

/// The dataset's backups in one repository (from [`Dataset::backups`]). It borrows the
/// repository, which is the caller's. Its calls block (see [`crate::backup`]).
#[derive(Clone)]
pub struct Backups<'r> {
    pub(crate) ds: Dataset,
    pub(crate) repo: &'r Repository,
}

impl Backups<'_> {
    /// The dataset's backups matching `filter`, newest first. The filter's
    /// `dataset_id` is the dataset's id.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<BackupSummary>> {
        let f = ListFilter {
            dataset_id: Some(self.ds.dataset_id()),
            ..filter.clone()
        };
        block_on(self.repo.list(&f))?.map_err(error)
    }

    /// The manifest of backup `name`, or `None` when the repository has no such backup
    /// of this dataset.
    pub fn get(&self, name: &str) -> Result<Option<Manifest>> {
        match block_on(self.repo.manifest(name))? {
            Ok(m) if m.dataset.id == self.ds.dataset_id() => Ok(Some(m)),
            Ok(_) => Ok(None),
            Err(e) if e.code() == Code::NoSuchBackup => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    /// Delete backup `name` of this dataset; whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool> {
        if self.get(name)?.is_none() {
            return Ok(false);
        }
        block_on(self.repo.delete(name))?.map_err(error)
    }

    /// Verify backup `name` at the level of `opts`, with the cancellation and progress
    /// of `ctl`.
    pub fn verify_with(
        &self,
        name: &str,
        opts: &VerifyOptions,
        ctl: &Control,
    ) -> Result<VerifyReport> {
        let o = VerifyOptions {
            ctl: ctl.into(),
            ..opts.clone()
        };
        block_on(self.repo.verify(&[name.to_string()], &o))?.map_err(error)
    }
}
