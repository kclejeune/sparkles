//! Restoring a backup into a directory, and the offline directory swap.

use crate::error::Result;
use crate::{BackupError, Repository, RestoreOptions, RestoreReport};
use std::path::Path;

impl Repository {
    /// Restore backup `name` into `tmp`, which must not exist (the caller picks a
    /// sibling of the final location and publishes it by rename):
    /// 1. a shared lock (none for read-only repositories); `manifest::validate`;
    ///    `422 incompatible-format` unless `indexFormat == builder::FORMAT_VERSION`;
    ///    `507 insufficient-storage` unless the free space of `tmp`'s filesystem is at
    ///    least 1.1 × the logical size;
    /// 2. download every file (parallel, download throttle), verifying each blob's
    ///    length and SHA-256 before writing it (a mismatch is fetched again up to 2
    ///    times, then fails with `checksum mismatch in blob <id> (<path>)`), then each
    ///    file's own `sha256`; files are created with `create_new` inside `tmp` only;
    /// 3. the identity rule (`o.identity`, `o.id_in_use`, `o.in_place_head`;
    ///    `409 duplicate-dataset-id`), with `sparkles::commit::reidentify` for `new`;
    /// 4. fsync every file and directory; write `restore.json` ([`crate::RestoreRecord`]);
    /// 5. `sparkles::check` (quick or full per `o.check`; `Warning` is success);
    /// 6. open the store with `o.store_opts`: its head must be `commit.seq` and its
    ///    quad count `commit.quads` (`500 restore-mismatch`); close it.
    ///
    /// Any failure (or cancellation, checked between requests) removes `tmp`.
    pub async fn restore(
        &self,
        name: &str,
        tmp: &Path,
        o: &RestoreOptions,
    ) -> Result<RestoreReport> {
        let _ = (name, tmp, o);
        Err(BackupError::unsupported("restoring a backup"))
    }
}

/// Replace the database directory `target` with the restored directory `restored` (a
/// sibling): hold `target`'s `sparkles.lock` (the "in use by another process" error if
/// someone holds it), rename `target` → `<target>.replaced-<pid>`, rename `restored` →
/// `target`, fsync the parent, then remove the replaced directory unless
/// `keep_replaced`. If the second rename fails, the first is undone. For the offline
/// `backup restore --to DIR --replace` (the server does its own swap).
pub fn swap_dir(target: &Path, restored: &Path, keep_replaced: bool) -> Result<()> {
    let _ = (target, restored, keep_replaced);
    Err(BackupError::unsupported("replacing a database directory"))
}
