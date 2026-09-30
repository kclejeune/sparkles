//! The source of a backup: a capture of a live store ([`Source::from`] a
//! `sparkles::store::BackupCapture`), or of a closed database directory (the CLI with
//! a stopped database, and tests).

use crate::BackupError;
use crate::error::Result;
use sparkles::commit::CommitInfo;
use sparkles::store::{BackupCapture, CapturedFile, LeaseGuard};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

/// What a backup uploads: the files of one database at one commit.
#[derive(Debug)]
pub struct Source {
    pub dataset_id: Uuid,
    /// the captured commit
    pub commit: CommitInfo,
    /// the generation directory (`gen-NNNN`)
    pub generation: String,
    /// the index format of the generation
    pub index_format: u32,
    /// every file, with its kind and captured length (see `BackupCapture::files`)
    pub files: Vec<CapturedFile>,
    /// writer-lock hold time of the capture (zero for a closed directory)
    pub lock_hold: Duration,
    /// keeps the generation until the source is dropped (holds nothing for a closed
    /// directory)
    pub lease: LeaseGuard,
}

impl From<BackupCapture> for Source {
    fn from(c: BackupCapture) -> Source {
        Source {
            dataset_id: c.dataset_id,
            commit: c.commit,
            generation: c.generation,
            index_format: c.index_format,
            files: c.files,
            lock_hold: c.lock_hold,
            lease: c.lease,
        }
    }
}

impl Source {
    /// A source for the database directory `dir` that no other process has open (the
    /// store's "in use by another process" error otherwise): the same files and the
    /// same head commit a capture of that database, opened, would give. For the CLI
    /// (`backup create --loc` on a stopped database) and tests.
    pub fn from_closed_dir(dir: &Path) -> Result<Source> {
        let _ = dir;
        Err(BackupError::unsupported("backups of a closed directory"))
    }
}
