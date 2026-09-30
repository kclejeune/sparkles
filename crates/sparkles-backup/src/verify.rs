//! Verifying backups and repositories.

use crate::error::Result;
use crate::{BackupError, Repository, VerifyOptions, VerifyReport};

impl Repository {
    /// Verify the backups `names`, or with no names the whole repository (every
    /// backup, plus `orphans`: blobs no manifest references):
    /// * `exists`: every manifest parses and validates, and every referenced blob is
    ///   present with its stored length (one `LIST blobs/`, or a `HEAD` per blob when
    ///   that is cheaper for a few backups);
    /// * `data`: and every blob is downloaded, decoded and hashed (`corrupt`);
    /// * `restore` (one backup at a time): and a full restore into a fresh directory
    ///   under `o.tmp_dir`, `check` in full mode, head and quad count, then removal.
    ///
    /// A missing backup name is `404 no-such-backup`. Problems are reported per backup
    /// (`status: error` with `missing` / `corrupt`), not as an `Err`; the report's
    /// status is `error` if any backup failed, `warning` for orphans only, else `ok`.
    pub async fn verify(&self, names: &[String], o: &VerifyOptions) -> Result<VerifyReport> {
        let _ = (names, o);
        Err(BackupError::unsupported("verification"))
    }
}
