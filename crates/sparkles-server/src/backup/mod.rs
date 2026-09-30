//! Backup repositories on the server: the registry of repositories and policies
//! (`<data>/backup/repositories.json`, `policies.json`, and `serve --backup-config`),
//! the task slots of backup, restore, verify and GC tasks, the HTTP routes
//! (`/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`), the policy scheduler,
//! in-place restores, startup recovery, metrics, and the `sparkles repo` and
//! `sparkles backup` subcommands. The repository engine is the `sparkles-backup` crate.
//!
//! Server tasks run on std threads (as every task) and drive the async engine with
//! `Handle::block_on` on the server's runtime ([`BackupState::handle`]).

// the operations are filled in by later work; until then most of this is unused
#![allow(dead_code)]

pub mod cli;
pub mod config;
pub mod http;
pub mod metrics;
pub mod policies;
pub mod recover;
pub mod registry;
pub mod scheduler;
pub mod swap;

use crate::state::AppState;
pub use sparkles_backup::{BackupError, Repository};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::runtime::Handle;

/// Everything the server keeps about backups (`AppState::backup`).
pub struct BackupState {
    /// `<data>/backup`: `repositories.json`, `policies.json`, `policy-state.json`,
    /// `runs.json`, `verify.json`, `cache/<repository id>/`
    pub dir: PathBuf,
    /// `serve --backup-config`
    pub config_path: Option<PathBuf>,
    /// `serve --backup-max-tasks`: backup, restore, verify and GC tasks running at once
    pub max_tasks: usize,
    /// the server's runtime, set by [`start`] once it exists
    handle: OnceLock<Handle>,
    pub registry: registry::Registry,
}

impl BackupState {
    /// Load the registry (`<data>/backup/*.json`) and the config file. A config file
    /// that does not parse or validate stops the server (errors name the line and
    /// column). Does not touch any repository (they are opened lazily, and reachability
    /// is checked in the background by [`start`]).
    pub fn new(
        data_dir: &Path,
        config_path: Option<PathBuf>,
        max_tasks: usize,
    ) -> anyhow::Result<BackupState> {
        let dir = data_dir.join("backup");
        let file = match &config_path {
            Some(p) => Some(config::load(p)?),
            None => None,
        };
        let registry = registry::Registry::load(&dir, file.as_ref())?;
        Ok(BackupState {
            dir,
            config_path,
            max_tasks: max_tasks.max(1),
            handle: OnceLock::new(),
            registry,
        })
    }

    /// The server's runtime (`None` before [`start`], e.g. in router tests that did not
    /// call it).
    pub fn handle(&self) -> Option<Handle> {
        self.handle.get().cloned()
    }

    /// The opened repository registered as `name`: `404 no-such-repository`; a
    /// repository that cannot be opened is `502 repository-unavailable` (and marked
    /// unreachable). Opening is lazy and cached; a config reload drops the cache of
    /// changed entries.
    pub fn repo(&self, name: &str) -> Result<Arc<Repository>, BackupError> {
        self.registry.repo(name)
    }
}

/// Start the background parts once the server's runtime exists: remember `h`, check
/// the repositories' reachability, start the policy scheduler, and re-read
/// `--backup-config` on SIGHUP (like the auth and rate-limit files). A no-op without
/// [`AppState::backup`].
pub fn start(st: &Arc<AppState>, h: &Handle) {
    let Some(b) = &st.backup else { return };
    let _ = b.handle.set(h.clone());
    scheduler::spawn(st.clone(), Arc::new(scheduler::SystemClock));
}
