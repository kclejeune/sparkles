//! Backup repositories for Sparkles databases.
//!
//! A **repository** is a directory or an S3-compatible bucket prefix holding
//! deduplicated, content-addressed copies of dataset files (see [`layout`]). A
//! **backup** is one dataset at one commit, described by an immutable manifest
//! ([`types::Manifest`]). Generation files are immutable once built, so a backup uploads
//! only the files (32 MiB pieces) the repository lacks, and only the appended bytes of
//! the append-only files (`wal.log`, `delta.vocab`, `commits.bin`).
//!
//! The API is async ([`Repository`]); the server drives it from its task threads with
//! `Handle::block_on`, the CLI from a current-thread runtime.
//!
//! ```text
//! Repository::open(cfg, env)                       attach, or initialize an empty location
//!   .create(Source::from(store.backup_capture(name)?), &CreateOptions)
//!   .list(&ListFilter) / .manifest(name) / .delete(name) / .stats()
//!   .restore(name, tmp_dir, &RestoreOptions)       then restore::swap_dir or a rename
//!   .verify(&[names], &VerifyOptions)              empty names: the whole repository
//!   .gc(&GcOptions) / .locks() / .break_lock(id)
//! ```
//!
//! Every error carries a stable [`Code`] with its HTTP status ([`BackupError`]).

pub mod blob;
pub mod cache;
pub mod capture;
pub mod create;
pub mod error;
#[cfg(test)]
mod fixture;
pub mod gc;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod policy;
pub mod repo;
pub mod restore;
pub mod throttle;
pub mod types;
pub mod verify;

pub use capture::Source;
pub use error::{BackupError, Code, Result};
pub use object_store;
pub use repo::Repository;
pub use types::*;

use object_store::ObjectStore;
use sparkles_core::commit::ForkedFrom;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;

/// Progress callback: (fraction done in `[0, 1]`, message).
pub type ProgressFn = Arc<dyn Fn(f32, &str) + Send + Sync>;

/// How a [`Repository`] is opened.
#[derive(Clone, Debug)]
pub struct OpenEnv {
    /// the local manifest cache root; the repository's cache is `<cache_dir>/<repo id>/`
    /// (`None`: no on-disk cache)
    pub cache_dir: Option<PathBuf>,
    /// `fs` repositories may not lie inside these directories (the server's data
    /// directory): `400 invalid-config`
    pub forbid_under: Vec<PathBuf>,
    /// how long create, restore, verify and delete wait for a conflicting lock before
    /// `409 repository-locked` (default 10 minutes)
    pub lock_wait: Duration,
    /// initialize an empty location (`false`: attach only, `409 not-a-repository` for
    /// an empty one too)
    pub init: bool,
    /// identifies this process's locks (`holder.server`): a hash of the server's data
    /// directory, empty for the CLI
    pub server_id: String,
    /// use this store instead of building one from the configuration: tests wrap
    /// stores to inject failures or share one `InMemory` between two handles
    pub store: Option<Arc<dyn ObjectStore>>,
    /// where `s3`, `gcs` and `azure` repositories may connect: every address their
    /// endpoint resolves to must pass it, and connections go to exactly those
    /// addresses (`None`: anywhere; the server sets it for repositories registered
    /// through its API)
    pub outbound: Option<sparkles_core::outbound::OutboundPolicy>,
    /// count this repository's object requests here (`None`: counters of its own,
    /// [`Repository::requests`]); a server keeps one per repository name, so the
    /// counts survive reopening
    pub requests: Option<Arc<repo::RequestStats>>,
}

impl Default for OpenEnv {
    fn default() -> OpenEnv {
        OpenEnv {
            cache_dir: None,
            forbid_under: Vec::new(),
            lock_wait: lock::DEFAULT_WAIT,
            init: true,
            server_id: String::new(),
            store: None,
            outbound: None,
            requests: None,
        }
    }
}

/// Cancellation and progress of one operation.
#[derive(Clone, Default)]
pub struct Ctl {
    /// set to `true` to cancel; checked between object requests and every 8 MiB of
    /// streamed data (the operation then fails with [`Code::Cancelled`])
    pub cancel: Arc<AtomicBool>,
    pub progress: Option<ProgressFn>,
}

impl Ctl {
    /// A control that follows `cancel`, without progress reports.
    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Ctl {
        Ctl {
            cancel,
            progress: None,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// `Err(cancelled)` once cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(BackupError::cancelled())
        } else {
            Ok(())
        }
    }

    /// Report progress (a no-op without a callback).
    pub fn report(&self, fraction: f32, msg: &str) {
        if let Some(p) = &self.progress {
            p(fraction.clamp(0.0, 1.0), msg);
        }
    }
}

impl std::fmt::Debug for Ctl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctl")
            .field("cancelled", &self.is_cancelled())
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

/// Which backups [`Repository::list`] returns: those matching every given field,
/// newest `completed` first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListFilter {
    /// `dataset.name`
    pub dataset: Option<String>,
    /// `dataset.id`
    pub dataset_id: Option<Uuid>,
    /// the policy that made them
    pub policy: Option<String>,
    /// at most this many
    pub limit: Option<usize>,
    /// only those completed strictly before this RFC 3339 instant (the paging cursor)
    pub before: Option<String>,
}

/// Options of [`Repository::create`].
#[derive(Clone, Debug, Default)]
pub struct CreateOptions {
    /// the backup name (valid, see [`layout::valid_backup_name`])
    pub name: String,
    pub note: Option<String>,
    /// `(policy, run id)` of a policy run
    pub policy: Option<(String, String)>,
    /// the dataset's name on the source server (`dataset.name` of the manifest)
    pub dataset_name: String,
    /// extra meta files `(path, content)`, e.g. `reasoning.json`
    pub extra: Vec<(String, Vec<u8>)>,
    /// `fs` repositories: free space to keep on the repository's file system (the
    /// server's `--min-free-disk-mb`); a blob that would leave less fails the backup
    /// with `507 insufficient-storage` (`None`: only the blob itself must fit)
    pub min_free_disk_bytes: Option<u64>,
    pub ctl: Ctl,
}

/// Options of [`Repository::restore`].
#[derive(Clone)]
pub struct RestoreOptions {
    pub identity: Identity,
    pub check: CheckLevel,
    /// whether a dataset on the restoring side already has this id (the identity rule;
    /// `auto` keeps the id only when this is false)
    pub id_in_use: Arc<dyn Fn(Uuid) -> bool + Send + Sync>,
    /// in place: the head of the dataset being replaced (`keep` is refused when it is
    /// greater than the backup's commit)
    pub in_place_head: Option<u64>,
    /// options of the `Store::open` that verifies the restored directory
    pub store_opts: sparkles_core::store::StoreOptions,
    pub ctl: Ctl,
}

impl Default for RestoreOptions {
    fn default() -> RestoreOptions {
        RestoreOptions {
            identity: Identity::Auto,
            check: CheckLevel::Quick,
            id_in_use: Arc::new(|_| false),
            in_place_head: None,
            store_opts: Default::default(),
            ctl: Ctl::default(),
        }
    }
}

impl std::fmt::Debug for RestoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreOptions")
            .field("identity", &self.identity)
            .field("check", &self.check)
            .field("in_place_head", &self.in_place_head)
            .field("ctl", &self.ctl)
            .finish_non_exhaustive()
    }
}

/// What [`Repository::restore`] did.
#[derive(Clone, Debug)]
pub struct RestoreReport {
    pub backup: BackupSummary,
    /// the restored directory's dataset id
    pub dataset_id: Uuid,
    /// `kept` or `new`
    pub identity: &'static str,
    /// with `new`: the source id and the backup's commit
    pub forked_from: Option<ForkedFrom>,
    /// the `sparkles_core::check` report (JSON), unless the check was skipped
    pub check: Option<serde_json::Value>,
    pub millis: u64,
}

/// Options of [`Repository::verify`].
#[derive(Clone, Debug, Default)]
pub struct VerifyOptions {
    pub level: VerifyLevel,
    /// level `restore`: where to create the temporary restore directory (a fresh
    /// `verify-*` directory inside it, removed afterwards; default the system temp dir)
    pub tmp_dir: Option<PathBuf>,
    /// level `restore`: options of the verifying `Store::open`
    pub store_opts: sparkles_core::store::StoreOptions,
    pub ctl: Ctl,
}

/// Options of [`Repository::gc`].
#[derive(Clone, Debug)]
pub struct GcOptions {
    /// report the candidates, delete nothing
    pub dry_run: bool,
    /// unreferenced blobs younger than this are kept (default 24 h)
    pub grace: Duration,
    pub ctl: Ctl,
}

impl Default for GcOptions {
    fn default() -> GcOptions {
        GcOptions {
            dry_run: false,
            grace: Duration::from_secs(24 * 3600),
            ctl: Ctl::default(),
        }
    }
}

/// The current time as RFC 3339 with milliseconds (UTC).
pub fn now_rfc3339() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    sparkles_core::commit::rfc3339_ms(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctl_cancels_and_reports() {
        let seen = Arc::new(std::sync::Mutex::new((0.0, String::new())));
        let s2 = seen.clone();
        let ctl = Ctl {
            cancel: Arc::default(),
            progress: Some(Arc::new(move |f, m: &str| {
                *s2.lock().unwrap() = (f, m.to_string())
            })),
        };
        ctl.report(1.5, "done");
        assert_eq!(*seen.lock().unwrap(), (1.0, "done".to_string()));
        assert!(ctl.check().is_ok());
        ctl.cancel.store(true, Ordering::Relaxed);
        assert_eq!(ctl.check().unwrap_err().code(), Code::Cancelled);
        assert!(Ctl::with_cancel(ctl.cancel.clone()).is_cancelled());
    }
}
