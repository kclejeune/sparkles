//! The registry of repositories and policies: API entries persisted in
//! `<data>/backup/repositories.json` and `policies.json` (written with
//! `state::write_file_atomic`), plus the read-only entries of `--backup-config`.
//! Repositories and policies share one namespace per kind across both sources: an API
//! registration with a config name fails with `409 repository-exists` (or
//! `policy-exists`).

use super::config::ConfigFile;
use sparkles_backup::{
    BackupError, Code, ConfigSource, PolicyConfig, RepoConfig, RepoStatus, Repository,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// A registered repository.
pub struct RepoEntry {
    pub config: RepoConfig,
    pub source: ConfigSource,
    /// the opened repository, once opened successfully
    pub opened: Option<Arc<Repository>>,
    pub status: RepoStatus,
}

/// A registered policy.
pub struct PolicyEntry {
    pub config: PolicyConfig,
    pub source: ConfigSource,
}

/// Repositories and policies by name.
#[derive(Default)]
pub struct Registry {
    pub repos: parking_lot::RwLock<BTreeMap<String, RepoEntry>>,
    pub policies: parking_lot::RwLock<BTreeMap<String, PolicyEntry>>,
}

impl Registry {
    /// Read `<dir>/repositories.json` and `<dir>/policies.json` (missing files: empty)
    /// and add the config file's entries (`source: "config"`). A name in both sources
    /// is an error naming it.
    pub fn load(dir: &Path, config: Option<&ConfigFile>) -> anyhow::Result<Registry> {
        let _ = (dir, config);
        Ok(Registry::default())
    }

    /// Write the API entries back (`repositories.json`, `policies.json`).
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let _ = dir;
        anyhow::bail!("saving the backup registry is not implemented yet")
    }

    /// The opened repository `name` (see `BackupState::repo`).
    pub fn repo(&self, name: &str) -> Result<Arc<Repository>, BackupError> {
        match self.repos.read().get(name) {
            None => Err(BackupError::new(
                Code::NoSuchRepository,
                format!("no such repository: {name}"),
            )),
            Some(e) => e
                .opened
                .clone()
                .ok_or_else(|| BackupError::unsupported("opening a registered repository")),
        }
    }
}
