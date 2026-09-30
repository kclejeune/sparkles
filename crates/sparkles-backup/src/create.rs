//! Creating a backup: plan the blobs, upload what the repository lacks, write the
//! manifest last.

use crate::error::Result;
use crate::{BackupError, BackupSummary, CreateOptions, Repository, Source};

impl Repository {
    /// Back up `src` as `o.name`:
    /// 1. a shared lock; `409 backup-exists` if `backups/<name>.json` exists (an early
    ///    check; the final conditional create decides); `409 repository-read-only`;
    /// 2. plan: immutable files in `pieceBytes` pieces, reused when the parent manifest
    ///    (the newest backup of the same dataset id) has the id, else `HEAD` (pieces
    ///    over 1 MiB) and a `PutMode::Create` (`AlreadyExists` counts as success);
    ///    append-only files as the parent's segments plus new ones (from scratch when a
    ///    segment differs or there would be more than 64); meta files one blob each;
    ///    `o.extra` added as meta files;
    /// 3. upload with `maxConcurrency` requests in flight and the upload throttle,
    ///    checking `o.ctl` between requests and every 8 MiB; progress `0.05..0.95` by
    ///    bytes, message `uploading 120/310 MB · 14 new blobs · 3 reused`;
    /// 4. `PUT backups/<name>.json` with `PutMode::Create` (`409 backup-exists` when a
    ///    concurrent writer took the name); progress `0.97`;
    /// 5. cache the manifest, release the lock, drop `src` (its lease).
    ///
    /// A failure or cancellation leaves no manifest, only unreferenced blobs.
    pub async fn create(&self, src: Source, o: &CreateOptions) -> Result<BackupSummary> {
        let _ = (src, o);
        Err(BackupError::unsupported("creating a backup"))
    }
}
