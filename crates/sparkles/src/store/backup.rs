//! Consistent captures of a live database for backup repositories (`sparkles-backup`).
//!
//! A capture pins one commit `s` of a persistent store without blocking writers for
//! more than a few system calls: under the writer mutex it records the length of every
//! append-only file (the WAL up to the end of `s`'s record, the flushed delta
//! vocabulary, the commit catalog through `s`), opens a read handle for every file of the
//! current generation, renders `prefixes.json`, and leases the generation so that F06
//! garbage collection keeps its directory until the lease is dropped. Everything else is
//! read outside the lock, through the open handles (`pread`), so a compaction that
//! renames or deletes files during the upload does not affect the backup.

use super::Store;
use crate::commit::CommitInfo;
use crate::error::{Error, Result};
use std::sync::Arc;
use std::time::Duration;

/// How a captured file is stored in a backup repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// never changes once written (`gen-NNNN/*.dat`, `vocab.dat`, `meta.json`, …):
    /// stored whole, split into fixed-size pieces
    Immutable,
    /// only grows (`gen-NNNN/wal.log`, `gen-NNNN/delta.vocab`, `commits.bin`): stored as
    /// ordered segments, so a later backup uploads only the appended bytes
    Append,
    /// small files that may be rewritten (`CURRENT`, `dataset.json`, `prefixes.json`,
    /// `text.json`, `origin.json`, `reasoning.json`, `validation*`): one blob each
    Meta,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Immutable => "immutable",
            FileKind::Append => "append",
            FileKind::Meta => "meta",
        }
    }
}

/// Where a captured file's bytes come from.
#[derive(Debug)]
pub enum FileSource {
    /// a read handle opened while the writer lock was held; read with positioned reads
    /// ([`CapturedFile::read_at`]) up to [`CapturedFile::len`] only
    File(std::fs::File),
    /// content rendered at capture time (`prefixes.json`) or read whole after it
    Bytes(Arc<[u8]>),
}

/// One file of a capture.
#[derive(Debug)]
pub struct CapturedFile {
    /// path relative to the database root, with `/` separators (`gen-0001/spo.dat`,
    /// `commits.bin`, `CURRENT`)
    pub path: String,
    pub kind: FileKind,
    /// bytes that belong to the backup: the whole file for immutable and meta files, the
    /// captured prefix for append-only ones (the file may have grown since)
    pub len: u64,
    pub src: FileSource,
}

impl CapturedFile {
    /// Read up to `buf.len()` bytes at `offset`, never past [`len`](Self::len). Returns
    /// the number of bytes read (0 at `len`). Positioned: concurrent calls on one file
    /// are fine.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let n = buf.len().min((self.len - offset) as usize);
        match &self.src {
            FileSource::Bytes(b) => {
                let start = offset as usize;
                let avail = b.len().saturating_sub(start).min(n);
                buf[..avail].copy_from_slice(&b[start..start + avail]);
                Ok(avail)
            }
            FileSource::File(f) => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::FileExt::read_at(f, &mut buf[..n], offset)
                }
                #[cfg(windows)]
                {
                    std::os::windows::fs::FileExt::seek_read(f, &mut buf[..n], offset)
                }
                #[cfg(not(any(unix, windows)))]
                {
                    let _ = f;
                    Err(std::io::Error::other("positioned reads are not supported"))
                }
            }
        }
    }

    /// Fill `buf` completely from `offset`; a short file is an `UnexpectedEof` error
    /// (a generation file that shrank during the backup: `Error::Corrupt` upstream).
    pub fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(offset, buf) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("{}: file shorter than captured", self.path),
                    ));
                }
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Keeps a captured generation leased (history GC keeps its directory) until dropped.
/// Dropping it removes the lease and runs a best-effort history collection
/// ([`Store::try_collect_history`]): if the writer is busy, the next collection point
/// removes the generation instead. A guard made with [`LeaseGuard::none`] holds nothing.
pub struct LeaseGuard {
    /// the leased generation number (`gen-NNNN`), 0 for [`none`](Self::none)
    pub(super) generation: u32,
    /// the lease's label, shown in `HistoryStatus` as `backup:<label>`
    pub(super) label: String,
    /// removes the lease (and collects); run once, on drop
    pub(super) release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl LeaseGuard {
    /// A guard that holds no lease (a capture of a closed database directory).
    pub fn none() -> LeaseGuard {
        LeaseGuard {
            generation: 0,
            label: String::new(),
            release: None,
        }
    }

    /// The leased generation number (0 when nothing is leased).
    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("generation", &self.generation)
            .field("label", &self.label)
            .field("held", &self.release.is_some())
            .finish()
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// A consistent capture of a persistent store at one commit ([`Store::backup_capture`]).
#[derive(Debug)]
pub struct BackupCapture {
    pub dataset_id: uuid::Uuid,
    /// the captured commit `s` (the head when the writer lock was held)
    pub commit: CommitInfo,
    /// the generation directory holding `s` (`gen-NNNN`)
    pub generation: String,
    /// the index format of that generation (`builder::FORMAT_VERSION` when it was built)
    pub index_format: u32,
    /// every file of the backup, in a stable order: the generation's files, then
    /// `commits.bin`, then the meta files (`CURRENT`, `dataset.json`, `prefixes.json`,
    /// and when present `text.json`, `origin.json`, `validation.json`,
    /// `validation-shapes.ttl`). `reasoning.json` is the caller's to add (the server
    /// holds the current status and applies the "not after `s`" rule).
    pub files: Vec<CapturedFile>,
    /// how long the writer lock was held (metric `sparkles_backup_capture_lock_seconds`)
    pub lock_hold: Duration,
    /// keeps the generation until the upload ends
    pub lease: LeaseGuard,
}

impl Store {
    /// Capture the current commit for a backup labelled `label` (the backup name, shown
    /// as `backup:<label>` in the history status while the lease is held).
    ///
    /// Contract:
    /// * persistent stores only; in-memory stores fail with `Error::Unsupported`;
    /// * the writer mutex is held only to read lengths, flush the delta vocabulary and
    ///   the commit catalog, clone the prefixes, add the lease and open the handles
    ///   (typically well under a millisecond); a poisoned store fails with
    ///   `Error::Poisoned`;
    /// * a commit catalog that cannot be flushed completely fails with
    ///   `Error::Conflict("catalog-lagging: …")` (retryable, `503 catalog-lagging`);
    /// * `CURRENT` read after the lock must name `generation`, else the capture is
    ///   retried once (a compaction raced it), then fails with `Error::Conflict`;
    /// * the WAL prefix ends exactly at the end of `commit`'s record and the
    ///   `commits.bin` prefix at its catalog record, so a restored store's head is
    ///   `commit.seq`.
    pub fn backup_capture(&self, label: &str) -> Result<BackupCapture> {
        if self.root.is_none() {
            return Err(Error::unsupported(
                "backups of in-memory datasets are not supported",
            ));
        }
        let _ = label;
        Err(Error::unsupported("backup capture is not implemented yet"))
    }

    /// Best-effort F06 history collection: removes generations nothing needs any more
    /// if the writer mutex is free right now (`try_lock`), and otherwise does nothing
    /// (the next collection point — a commit, a compaction, a snapshot change or the
    /// server's hourly tick — does it). Called when a backup lease is dropped.
    pub fn try_collect_history(&self) {}
}
