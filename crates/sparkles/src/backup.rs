//! Backup repositories (feature `backup`): the `sparkles-backup` crate, and blocking
//! forms of its repository-wide operations.
//!
//! `sparkles-backup` is async, and the library is not. The blocking calls here and on
//! [`Backups`](crate::handles::Backups) run on a two-thread Tokio runtime that the
//! library starts on first use and keeps for the process. Blocking inside an async task
//! would stall the runtime, so a blocking call made from one fails with
//! [`Error::Invalid`](crate::Error::Invalid); an async program awaits the
//! [`Repository`]'s own async methods instead.

pub use sparkles_backup::*;
pub mod config;
pub mod policy;
pub mod registry;
mod repositories;
pub use repositories::Repositories;

use crate::error::{ComponentError, Error};
use std::future::Future;
use std::sync::OnceLock;

/// Run `f` to completion on the library's backup runtime.
pub(crate) fn block_on<F: Future>(f: F) -> crate::Result<F::Output> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Error::invalid(
            "a blocking backup call was made inside an async runtime; await the Repository's async methods instead",
        ));
    }
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = match RT.get() {
        Some(rt) => rt,
        None => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("sparkles-backup")
                .enable_all()
                .build()?;
            RT.get_or_init(|| rt)
        }
    };
    Ok(rt.block_on(f))
}

/// The engine error of a backup error: a cancellation is [`Error::Cancelled`], and
/// anything else an [`Error::Component`] of component `backup` with the backup code
/// (`repository-locked`, `not-a-repository`, …) and the [`BackupError`] as its source.
pub fn error(e: BackupError) -> Error {
    if e.is_cancelled() {
        return Error::Cancelled;
    }
    let code = e.code().as_str();
    let message = e.message().to_string();
    Error::Component(Box::new(
        ComponentError::new("backup", code, message).with_source(e),
    ))
}

/// Open (or with `env.init`, initialize) the repository `cfg` describes, blocking.
pub fn open(cfg: &RepoConfig, env: &OpenEnv) -> crate::Result<Repository> {
    block_on(Repository::open(cfg, env))?.map_err(error)
}

/// Blocking forms of `repo`'s repository-wide operations.
pub fn blocking(repo: &Repository) -> Blocking<'_> {
    Blocking { repo }
}

/// Blocking forms of a [`Repository`]'s repository-wide operations (see [`blocking`]).
#[derive(Clone, Copy)]
pub struct Blocking<'r> {
    repo: &'r Repository,
}

impl Blocking<'_> {
    /// Download and verify a backup into an unregistered directory.
    pub fn restore_to_dir(
        &self,
        name: &str,
        directory: &std::path::Path,
        options: &RestoreOptions,
    ) -> crate::Result<RestoreReport> {
        block_on(self.repo.restore(name, directory, options))?.map_err(error)
    }
    /// Check that the repository can be read and written.
    pub fn test(&self) -> crate::Result<TestReport> {
        block_on(self.repo.test())?.map_err(error)
    }

    /// Totals of the repository's backups and blobs.
    pub fn stats(&self) -> crate::Result<RepoStats> {
        block_on(self.repo.stats())?.map_err(error)
    }

    /// The backups matching `f`, newest first.
    pub fn list(&self, f: &ListFilter) -> crate::Result<Vec<BackupSummary>> {
        block_on(self.repo.list(f))?.map_err(error)
    }

    /// Verify the backups `names` (every backup when empty).
    pub fn verify(&self, names: &[String], o: &VerifyOptions) -> crate::Result<VerifyReport> {
        block_on(self.repo.verify(names, o))?.map_err(error)
    }

    /// Delete the blobs no backup references.
    pub fn gc(&self, o: &GcOptions) -> crate::Result<GcReport> {
        block_on(self.repo.gc(o))?.map_err(error)
    }

    /// The repository's locks.
    pub fn locks(&self) -> crate::Result<Vec<LockInfo>> {
        block_on(self.repo.locks())?.map_err(error)
    }

    /// Remove lock `id`; whether it existed.
    pub fn break_lock(&self, id: &str) -> crate::Result<bool> {
        block_on(self.repo.break_lock(id))?.map_err(error)
    }
}
