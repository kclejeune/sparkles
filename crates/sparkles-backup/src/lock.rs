//! Repository locks: lease objects `locks/<uuid>.json` ([`LockObject`]).
//!
//! * **shared** (create, delete, restore, verify): `PUT` the own lock
//!   (`PutMode::Create`), then `LIST locks/`; a non-stale exclusive lock means: delete
//!   the own lock, back off with jitter (1 s → 30 s), retry up to `lockWait`, then
//!   `409 repository-locked` naming the holder.
//! * **exclusive** (GC's sweep): the same, conflicting with any other non-stale lock.
//! * **refresh** every [`REFRESH`] (re-`PUT` with `PutMode::Overwrite`) while held.
//! * **stale**: `last_modified` (the storage server's clock, from LIST; the file mtime
//!   for `fs`) older than [`STALE`]: ignored by acquirers, removed by GC and
//!   `break_lock`.
//! * **release**: `DELETE` the own key, best effort (a leftover goes stale).
//! * Read-only repositories take no locks: [`acquire`] returns a guard that holds none.

use crate::error::Result;
use crate::{BackupError, Ctl, LockInfo, LockKind, LockOperation, Repository};
use std::time::Duration;

/// How often a held lock is re-written.
pub const REFRESH: Duration = Duration::from_secs(5 * 60);
/// Age (by `last_modified`) after which a lock is ignored.
pub const STALE: Duration = Duration::from_secs(30 * 60);
/// Default `lockWait`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10 * 60);

/// A held lock (or none, for read-only repositories). Release it with
/// [`release`](LockGuard::release); dropping it stops the refresh and deletes the lock
/// object in the background when a Tokio runtime is available (else it goes stale).
#[derive(Debug)]
pub struct LockGuard {
    /// the `<uuid>` of `locks/<uuid>.json`; `None` when nothing is held
    pub(crate) id: Option<String>,
}

impl LockGuard {
    /// A guard that holds nothing.
    pub fn none() -> LockGuard {
        LockGuard { id: None }
    }

    /// The lock id, if a lock is held.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Delete the lock object (best effort: a failure is logged, the lock goes stale).
    pub async fn release(self) -> Result<()> {
        Ok(())
    }
}

/// Acquire a `kind` lock on `repo` for `op`, waiting up to the repository's
/// `lock_wait` (`409 repository-locked` with a `holder` field after that; `cancelled`
/// when `ctl` is cancelled while waiting). Read-only repositories get
/// [`LockGuard::none`] without any request.
///
/// Until locking is implemented, this takes no lock and returns [`LockGuard::none`].
pub async fn acquire(
    repo: &Repository,
    kind: LockKind,
    op: LockOperation,
    ctl: &Ctl,
) -> Result<LockGuard> {
    let _ = (repo, kind, op);
    ctl.check()?;
    Ok(LockGuard::none())
}

impl Repository {
    /// Every lock object, with staleness (`GET /$/repositories/{repo}/locks`).
    pub async fn locks(&self) -> Result<Vec<LockInfo>> {
        Err(BackupError::unsupported("listing locks"))
    }

    /// Delete lock `id` (audited by the caller). `Ok(false)` if it did not exist;
    /// `409 repository-read-only` on a read-only repository.
    pub async fn break_lock(&self, id: &str) -> Result<bool> {
        let _ = id;
        Err(BackupError::unsupported("breaking a lock"))
    }
}
