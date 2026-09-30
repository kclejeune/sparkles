//! Garbage collection of unreferenced blobs: two-phase mark and sweep.

use crate::error::Result;
use crate::{BackupError, GcOptions, GcReport, Repository};

impl Repository {
    /// Collect blobs no manifest references:
    /// 1. **mark** under a shared lock: list `backups/`, read every manifest (cached),
    ///    list `blobs/`; candidates are unreferenced blobs whose `last_modified` is
    ///    older than `o.grace` (younger ones count as `keptYoung`);
    /// 2. **sweep** under an exclusive lock: re-list `backups/`, drop from the
    ///    candidates the blobs of manifests not seen in the mark, delete the rest
    ///    (`delete_stream`, batches of up to 1000), delete stale locks and `probe/`
    ///    leftovers, write `gc/last.json` ([`crate::LastGc`]).
    ///
    /// `o.dry_run` stops before deleting and reports the candidates. `409
    /// repository-read-only` on a read-only repository.
    pub async fn gc(&self, o: &GcOptions) -> Result<GcReport> {
        let _ = o;
        Err(BackupError::unsupported("garbage collection"))
    }
}
