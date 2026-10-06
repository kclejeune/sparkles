//! The registry of repositories and policies: API entries persisted in
//! `<data>/backup/repositories.json` (written with `state::write_file_atomic`) and
//! `policies.json` (the policies module's), plus the read-only entries of
//! `--backup-config`.
//! Repositories and policies share one namespace per kind across both sources: an API
//! registration with a config name fails with `409 repository-exists` (or
//! `policy-exists`).
//!
//! Also here: the server-local verification history `<data>/backup/verify.json`, which
//! feeds `BackupSummary.verified`.
//!
//! For the policy scheduler and routes: [`Registry::policies`] holds every policy
//! (config and API), [`Registry::replace_config`] swaps in a reloaded config file, and
//! [`Registry::policy_users`] names the policies that back up into a repository.

use super::config::{ConfigFile, ConfiguredRepository, RepositoryEncryption};
use crate::catalog::write_file_atomic;
use anyhow::{Context, bail};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sparkles_backup::{
    BackupError, Code, ConfigSource, Credentials, LastGc, PolicyConfig, RepoConfig, RepoStats,
    RepoStatus, Repository, Verified,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .expect("repository generation exhausted")
}
use uuid::Uuid;

/// `repositories.json`
pub const REPOSITORIES_FILE: &str = "repositories.json";
/// `verify.json`
pub const VERIFY_FILE: &str = "verify.json";

/// A registered repository.
pub struct RepoEntry {
    generation: u64,
    opening: Arc<tokio::sync::Mutex<()>>,
    encryption: Option<RepositoryEncryption>,
    pub config: RepoConfig,
    pub source: ConfigSource,
    /// the opened repository, once opened successfully
    pub opened: Option<Arc<Repository>>,
    pub status: RepoStatus,
    /// the repository id: from the marker once opened, else as last seen (API entries
    /// remember it in `repositories.json`, so a location that lost its marker is not
    /// silently initialized again)
    pub id: Option<Uuid>,
    /// totals from the last listing or GC
    pub stats: Option<RepoStats>,
    /// `gc/last.json`, as last read
    pub last_gc: Option<LastGc>,
}

impl RepoEntry {
    pub fn new(config: RepoConfig, source: ConfigSource) -> RepoEntry {
        let status = RepoStatus {
            single_writer: !config.conditional_writes,
            ..Default::default()
        };
        RepoEntry {
            generation: next_generation(),
            opening: Arc::new(tokio::sync::Mutex::new(())),
            encryption: None,
            config,
            source,
            opened: None,
            status,
            id: None,
            stats: None,
            last_gc: None,
        }
    }

    /// Cache identity across replacement/removal/reload, including unchanged encrypted references.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Serialize cache misses for this generation without holding a registry lock.
    pub fn opening_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.opening.clone()
    }

    /// Trusted operator references, excluded from API views and API persistence.
    pub fn encryption(&self) -> Option<&RepositoryEncryption> {
        self.encryption.as_ref()
    }

    /// The API form (`Repository`), with the policies that use it.
    pub fn view(&self, policies: Vec<String>) -> sparkles_backup::types::Repository {
        sparkles_backup::types::Repository {
            config: self.config.clone(),
            source: self.source,
            id: self.id,
            status: self.status.clone(),
            stats: self.stats.clone(),
            last_gc: self.last_gc.clone(),
            policies,
            test: None,
        }
    }

    /// Record a successful contact (`conditionalWrites` from a connection test, if one
    /// ran).
    pub fn mark_reachable(&mut self, conditional_writes: Option<bool>) {
        self.status.reachable = true;
        self.status.checked = sparkles_backup::now_rfc3339();
        self.status.error = None;
        if conditional_writes.is_some() {
            self.status.conditional_writes = conditional_writes;
        }
        self.status.single_writer =
            !self.config.conditional_writes || self.status.conditional_writes == Some(false);
    }

    /// Record a failed contact.
    pub fn mark_unreachable(&mut self, error: &str) {
        self.status.reachable = false;
        self.status.checked = sparkles_backup::now_rfc3339();
        self.status.error = Some(error.to_string());
    }

    /// Not opened, and found unreachable less than a minute ago (listings over every
    /// repository skip it rather than wait for it again).
    pub fn recently_unreachable(&self) -> bool {
        self.opened.is_none()
            && !self.status.reachable
            && chrono::DateTime::parse_from_rfc3339(&self.status.checked).is_ok_and(|t| {
                chrono::Utc::now().signed_duration_since(t) < chrono::TimeDelta::minutes(1)
            })
    }
}

/// A registered policy.
pub struct PolicyEntry {
    pub config: PolicyConfig,
    pub source: ConfigSource,
}

/// What the config file allows repositories registered through the API.
#[derive(Clone, Debug, Default)]
pub struct ApiRules {
    /// `[credentials.<name>]`: the credential sources repositories may name
    pub credentials: BTreeMap<String, Credentials>,
    /// `[api] fs_roots` (empty: no restriction beyond the server's own directories)
    pub fs_roots: Vec<PathBuf>,
}

/// Repositories and policies by name.
#[derive(Default)]
pub struct Registry {
    pub repos: RwLock<BTreeMap<String, RepoEntry>>,
    pub policies: RwLock<BTreeMap<String, PolicyEntry>>,
    /// the config file's credential sources and API limits (replaced on reload)
    pub api: RwLock<ApiRules>,
    /// serializes the writes of `repositories.json`, so an older snapshot never
    /// overwrites a newer one
    saving: Mutex<()>,
    pub(crate) mutation: Mutex<()>,
}

/// An entry of `repositories.json`.
#[derive(Serialize)]
struct StoredRepo {
    #[serde(flatten)]
    config: RepoConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<Uuid>,
}

impl<'de> Deserialize<'de> for StoredRepo {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value
            .as_object()
            .is_some_and(|v| v.contains_key("encryption"))
        {
            return Err(serde::de::Error::custom(
                "API repository persistence does not support encryption metadata",
            ));
        }
        #[derive(Deserialize)]
        struct Plain {
            #[serde(flatten)]
            config: RepoConfig,
            #[serde(default)]
            id: Option<Uuid>,
        }
        let plain: Plain = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            config: plain.config,
            id: plain.id,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct RepositoriesFile {
    version: u32,
    #[serde(default)]
    repositories: Vec<StoredRepo>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_json<T: Serialize>(dir: &Path, file: &str, value: &T) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(file);
    write_file_atomic(&path, &serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("writing {}", path.display()))
}

impl Registry {
    /// Register a prepared repository after the caller's credential and network checks.
    pub fn insert_repository(
        &self,
        dir: Option<&Path>,
        entry: RepoEntry,
    ) -> Result<(), BackupError> {
        let _guard = self.mutation.lock();
        let name = entry.config.name.clone();
        let mut repos = self.repos.write();
        if repos.contains_key(&name)
            || repos.values().any(|e| {
                e.config.same_location(&entry.config) || entry.id.is_some_and(|id| e.id == Some(id))
            })
        {
            return Err(BackupError::new(
                Code::RepositoryExists,
                format!("a repository named {name} or at that location exists"),
            ));
        }
        repos.insert(name.clone(), entry);
        drop(repos);
        if let Err(e) = self.persist_repositories(dir) {
            self.repos.write().remove(&name);
            return Err(e);
        }
        Ok(())
    }
    /// Update mutable settings, retaining the original entry if persistence fails.
    pub fn replace_repository(
        &self,
        dir: Option<&Path>,
        name: &str,
        config: RepoConfig,
    ) -> Result<(), BackupError> {
        let _guard = self.mutation.lock();
        let mut repos = self.repos.write();
        let old = repos.get(name).ok_or_else(|| no_such_repository(name))?;
        if old.source == ConfigSource::Config {
            return Err(BackupError::new(
                Code::ReadOnlyConfig,
                format!("{name} comes from the config file"),
            ));
        }
        if config.name != name {
            return Err(BackupError::new(
                Code::InvalidName,
                "the name cannot change",
            ));
        }
        if !old.config.same_location(&config) {
            return Err(BackupError::new(
                Code::LocationImmutable,
                "the repository location cannot change",
            ));
        }
        let old = repos.remove(name).expect("checked");
        let mut entry = RepoEntry::new(config, ConfigSource::Api);
        entry.id = old.id;
        entry.status = old.status.clone();
        entry.stats = old.stats.clone();
        entry.last_gc = old.last_gc.clone();
        entry.status.single_writer =
            !entry.config.conditional_writes || entry.status.conditional_writes == Some(false);
        repos.insert(name.into(), entry);
        drop(repos);
        if let Err(e) = self.persist_repositories(dir) {
            self.repos.write().insert(name.into(), old);
            return Err(e);
        }
        Ok(())
    }
    /// Unregister an API repository, refusing fixed entries and policy users.
    pub fn remove_repository(&self, dir: Option<&Path>, name: &str) -> Result<bool, BackupError> {
        let _guard = self.mutation.lock();
        let users = self.policy_users(name);
        let mut repos = self.repos.write();
        let Some(entry) = repos.get(name) else {
            return Ok(false);
        };
        if entry.source == ConfigSource::Config {
            return Err(BackupError::new(
                Code::ReadOnlyConfig,
                format!("{name} comes from the config file"),
            ));
        }
        if !users.is_empty() {
            return Err(
                BackupError::new(Code::RepositoryInUse, "policies use this repository")
                    .with("policies", users),
            );
        }
        let entry = repos.remove(name).expect("checked");
        drop(repos);
        if let Err(e) = self.persist_repositories(dir) {
            self.repos.write().insert(name.into(), entry);
            return Err(e);
        }
        Ok(true)
    }
    fn persist_repositories(&self, dir: Option<&Path>) -> Result<(), BackupError> {
        if let Some(dir) = dir {
            self.save_repositories(dir)
                .map_err(|e| BackupError::internal(format!("{e:#}")))?;
        }
        Ok(())
    }

    /// Read `<dir>/repositories.json` and `<dir>/policies.json` (missing files: empty)
    /// and add the config file's entries (`source: "config"`). A name in both sources
    /// is an error naming it.
    pub fn load(dir: &Path, config: Option<&ConfigFile>) -> anyhow::Result<Registry> {
        let mut repos = BTreeMap::new();
        if let Some(f) = read_json::<RepositoriesFile>(&dir.join(REPOSITORIES_FILE))? {
            for s in f.repositories {
                let mut e = RepoEntry::new(s.config, ConfigSource::Api);
                e.id = s.id;
                if repos.insert(e.config.name.clone(), e).is_some() {
                    bail!("{}: a repository is listed twice", REPOSITORIES_FILE);
                }
            }
        }
        let mut policies = BTreeMap::new();
        for p in read_api_policies(dir)? {
            let e = PolicyEntry {
                config: p,
                source: ConfigSource::Api,
            };
            if policies.insert(e.config.name.clone(), e).is_some() {
                bail!("policies.json: a policy is listed twice");
            }
        }
        let reg = Registry {
            repos: RwLock::new(repos),
            policies: RwLock::new(policies),
            api: RwLock::default(),
            saving: Mutex::new(()),
            mutation: Mutex::new(()),
        };
        if let Some(c) = config {
            reg.replace_config(c)?;
        }
        Ok(reg)
    }

    /// Replace the entries of the config file with those of `config` (a reload): API
    /// entries stay; a config name that an API entry has is an error, and nothing
    /// changes then. Repositories whose settings did not change keep their opened
    /// handle and status.
    pub fn replace_config(&self, config: &ConfigFile) -> anyhow::Result<()> {
        // Preserve the legacy plaintext-only projection and its early rejection.
        let new_repos = config
            .repository_configs()?
            .into_iter()
            .map(|config| ConfiguredRepository {
                config,
                encryption: None,
            })
            .collect();
        self.replace_entries(config, new_repos)
    }

    /// Load trusted operator references without enabling providers for API entries.
    /// Legacy `load` remains plaintext-only.
    pub fn load_configured(dir: &Path, config: Option<&ConfigFile>) -> anyhow::Result<Registry> {
        let reg = Self::load(dir, None)?;
        if let Some(config) = config {
            reg.replace_configured(config)?;
        }
        Ok(reg)
    }

    /// Replace trusted TOML entries. Encrypted handles are invalidated on every reload,
    /// even if references are identical, so replaced key material is resolved again.
    pub fn replace_configured(&self, config: &ConfigFile) -> anyhow::Result<()> {
        let new_repos = config.configured_repositories()?;
        if !cfg!(feature = "backup-encryption") && new_repos.iter().any(|r| r.encryption.is_some())
        {
            bail!("encrypted repository configuration requires the backup-encryption feature");
        }
        self.replace_entries(config, new_repos)
    }

    fn replace_entries(
        &self,
        config: &ConfigFile,
        new_repos: Vec<ConfiguredRepository>,
    ) -> anyhow::Result<()> {
        let _guard = self.mutation.lock();
        let new_policies = config.policy_configs();
        let mut repos = self.repos.write();
        let mut policies = self.policies.write();
        for r in &new_repos {
            if repos
                .get(&r.name)
                .is_some_and(|e| e.source == ConfigSource::Api)
            {
                bail!(
                    "repository {:?} is defined in the backup config file and through the API (repositories.json)",
                    r.name
                );
            }
        }
        for p in &new_policies {
            if policies
                .get(&p.name)
                .is_some_and(|e| e.source == ConfigSource::Api)
            {
                bail!(
                    "policy {:?} is defined in the backup config file and through the API (policies.json)",
                    p.name
                );
            }
        }
        let from_config: Vec<String> = repos
            .iter()
            .filter(|(_, e)| e.source == ConfigSource::Config)
            .map(|(n, _)| n.clone())
            .collect();
        let mut old: BTreeMap<String, RepoEntry> = from_config
            .into_iter()
            .filter_map(|n| repos.remove(&n).map(|e| (n, e)))
            .collect();
        for r in new_repos {
            let e = match old.remove(&r.name) {
                // unchanged: keep the opened repository and what is known about it
                Some(e)
                    if r.encryption.is_none() && e.encryption.is_none() && e.config == r.config =>
                {
                    e
                }
                Some(e) => {
                    let mut n = RepoEntry::new(r.config, ConfigSource::Config);
                    n.encryption = r.encryption;
                    // the same location keeps its id (and the check that it did not change)
                    if n.config.same_location(&e.config) {
                        n.id = e.id;
                    }
                    n
                }
                None => {
                    let mut n = RepoEntry::new(r.config, ConfigSource::Config);
                    n.encryption = r.encryption;
                    n
                }
            };
            repos.insert(e.config.name.clone(), e);
        }
        policies.retain(|_, e| e.source == ConfigSource::Api);
        for p in new_policies {
            policies.insert(
                p.name.clone(),
                PolicyEntry {
                    config: p,
                    source: ConfigSource::Config,
                },
            );
        }
        let credentials = config.credential_sources();
        // a changed credential source reopens the repositories that name it
        let old = std::mem::replace(
            &mut *self.api.write(),
            ApiRules {
                credentials,
                fs_roots: config.api.fs_roots.iter().map(PathBuf::from).collect(),
            },
        );
        let api = self.api.read();
        for e in repos.values_mut() {
            if let Credentials::Named { name } = &e.config.credentials
                && old.credentials.get(name) != api.credentials.get(name)
            {
                e.opened = None;
                e.generation = next_generation();
                e.opening = Arc::new(tokio::sync::Mutex::new(()));
            }
        }
        Ok(())
    }

    /// Write `repositories.json` (the API repositories, with their ids).
    pub fn save_repositories(&self, dir: &Path) -> anyhow::Result<()> {
        let _g = self.saving.lock();
        let file = RepositoriesFile {
            version: 1,
            repositories: self
                .repos
                .read()
                .values()
                .filter(|e| e.source == ConfigSource::Api)
                .map(|e| StoredRepo {
                    config: e.config.clone(),
                    id: e.id,
                })
                .collect(),
        };
        write_json(dir, REPOSITORIES_FILE, &file)
    }

    /// The opened repository `name`, if it is registered and was opened
    /// (`404 no-such-repository` if it is not registered). See `BackupState::repo`,
    /// which opens it.
    pub fn repo(&self, name: &str) -> Result<Option<Arc<Repository>>, BackupError> {
        match self.repos.read().get(name) {
            None => Err(no_such_repository(name)),
            Some(e) => Ok(e.opened.clone()),
        }
    }

    /// The configuration of repository `name` (`404 no-such-repository`).
    pub fn config(&self, name: &str) -> Result<RepoConfig, BackupError> {
        self.repos
            .read()
            .get(name)
            .map(|e| e.config.clone())
            .ok_or_else(|| no_such_repository(name))
    }

    /// Change the entry of repository `name`, if registered.
    pub fn update<T>(&self, name: &str, f: impl FnOnce(&mut RepoEntry) -> T) -> Option<T> {
        self.repos.write().get_mut(name).map(f)
    }

    /// The policies that back up into repository `name`.
    pub fn policy_users(&self, name: &str) -> Vec<String> {
        self.policies
            .read()
            .values()
            .filter(|p| p.config.repository == name)
            .map(|p| p.config.name.clone())
            .collect()
    }

    /// The API form of repository `name` (`404 no-such-repository`).
    pub fn view(&self, name: &str) -> Result<sparkles_backup::types::Repository, BackupError> {
        let users = self.policy_users(name);
        self.repos
            .read()
            .get(name)
            .map(|e| e.view(users))
            .ok_or_else(|| no_such_repository(name))
    }
}

pub fn no_such_repository(name: &str) -> BackupError {
    BackupError::new(
        Code::NoSuchRepository,
        format!("no repository named \u{201c}{name}\u{201d}"),
    )
}

// ------------------------------------------------------------- verify history ------

/// The last verification of each backup on this server (`<data>/backup/verify.json`),
/// keyed by repository id and backup name, so a repository registered under another
/// name keeps its history. A backup deleted through this server loses its entry.
pub struct VerifyHistory {
    path: PathBuf,
    map: Mutex<BTreeMap<String, Verified>>,
}

impl VerifyHistory {
    /// Load `path` (missing or unreadable: empty, with a WARN for the latter).
    pub fn load(path: PathBuf) -> VerifyHistory {
        let map = match read_json::<BTreeMap<String, Verified>>(&path) {
            Ok(m) => m.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(target: "sparkles::backup", "ignoring the verification history: {e:#}");
                BTreeMap::new()
            }
        };
        VerifyHistory {
            path,
            map: Mutex::new(map),
        }
    }

    fn key(repo: Uuid, backup: &str) -> String {
        format!("{repo}/{backup}")
    }

    pub fn get(&self, repo: Uuid, backup: &str) -> Option<Verified> {
        self.map.lock().get(&Self::key(repo, backup)).cloned()
    }

    /// Record verifications of backups of repository `repo` (and save, best effort).
    pub fn record(&self, repo: Uuid, results: impl IntoIterator<Item = (String, Verified)>) {
        let mut m = self.map.lock();
        for (name, v) in results {
            m.insert(Self::key(repo, &name), v);
        }
        self.save(&m);
    }

    /// Forget a deleted backup.
    pub fn remove(&self, repo: Uuid, backup: &str) {
        let mut m = self.map.lock();
        if m.remove(&Self::key(repo, backup)).is_some() {
            self.save(&m);
        }
    }

    fn save(&self, m: &BTreeMap<String, Verified>) {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let file = self
            .path
            .file_name()
            .map_or(VERIFY_FILE.into(), |f| f.to_string_lossy().into_owned());
        if let Err(e) = write_json(dir, &file, m) {
            tracing::warn!(target: "sparkles::backup", "cannot save the verification history: {e:#}");
        }
    }

    /// Fill `verified` of summaries of repository `repo`.
    pub fn annotate(&self, repo: Option<Uuid>, list: &mut [sparkles_backup::BackupSummary]) {
        let Some(repo) = repo else { return };
        let m = self.map.lock();
        for b in list {
            b.verified = m.get(&Self::key(repo, &b.name)).cloned();
        }
    }
}

/// Read the server's API policies to preserve repository-use protection.
pub fn read_api_policies(dir: &Path) -> anyhow::Result<Vec<PolicyConfig>> {
    #[derive(Deserialize, Default)]
    struct File {
        #[serde(default)]
        policies: Vec<PolicyConfig>,
    }
    Ok(read_json::<File>(&dir.join("policies.json"))?
        .unwrap_or_default()
        .policies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_backup::{RepoType, VerifyLevel, VerifyStatus};

    #[test]
    fn api_persistence_rejects_encryption_even_null_but_keeps_unknown_field_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        for encryption in [
            serde_json::Value::Null,
            serde_json::json!({"keys": [{"key": {"source":"file", "path":"/private/never-read"}}]}),
        ] {
            std::fs::write(dir.path().join(REPOSITORIES_FILE), serde_json::to_vec(&serde_json::json!({"version":1,"repositories":[{"name":"api","type":"fs","path":"/srv/plain","encryption":encryption}]})).unwrap()).unwrap();
            let err = Registry::load(dir.path(), None).err().unwrap();
            let message = format!("{err:#}");
            assert!(
                message.contains("does not support encryption metadata"),
                "{message}"
            );
            assert!(!message.contains("/private/never-read"));
        }
        std::fs::write(dir.path().join(REPOSITORIES_FILE), br#"{"version":1,"repositories":[{"name":"api","type":"fs","path":"/srv/plain","futureSetting":true}]}"#).unwrap();
        assert!(Registry::load(dir.path(), None).is_ok());
    }

    #[test]
    fn trusted_encryption_projection_is_explicit_and_feature_checked_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(
            "version=1\n[repositories.enc]\ntype='fs'\npath='/srv/enc'\n[repositories.enc.encryption]\nsingle_key_ok=true\nkeys=[{label='online',key={source='file',path='/private/key'}}]\n",
        );
        assert!(Registry::load(dir.path(), Some(&c)).is_err());
        let reg = Registry::default();
        reg.repos.write().insert(
            "api".into(),
            RepoEntry::new(fs_repo("api", "/srv/api"), ConfigSource::Api),
        );
        let result = reg.replace_configured(&c);
        if !cfg!(feature = "backup-encryption") {
            assert!(result.is_err());
            assert_eq!(reg.repos.read().len(), 1);
            assert!(reg.repos.read().contains_key("api"));
        } else {
            result.unwrap();
            let id = Uuid::new_v4();
            reg.update("enc", |e| e.id = Some(id));
            let token = reg.repos.read()["enc"].generation();
            reg.replace_configured(&c).unwrap();
            let entries = reg.repos.read();
            assert_ne!(entries["enc"].generation(), token);
            assert_eq!(entries["enc"].id, Some(id));
            assert!(entries["enc"].encryption().is_some());
            assert!(
                !serde_json::to_string(&entries["enc"].view(Vec::new()))
                    .unwrap()
                    .contains("/private/key")
            );
            drop(entries);
            let plain = config("version=1\n[repositories.enc]\ntype='fs'\npath='/srv/enc'\n");
            reg.replace_configured(&plain).unwrap();
            assert_eq!(reg.repos.read()["enc"].id, Some(id));
            assert!(reg.repos.read()["enc"].encryption().is_none());
            let token = reg.repos.read()["enc"].generation();
            reg.replace_configured(&plain).unwrap();
            assert_eq!(reg.repos.read()["enc"].generation(), token);
        }
    }

    fn fs_repo(name: &str, path: &str) -> RepoConfig {
        RepoConfig {
            name: name.into(),
            kind: RepoType::Fs,
            path: Some(path.into()),
            conditional_writes: true,
            ..Default::default()
        }
    }

    fn config(text: &str) -> ConfigFile {
        ConfigFile::parse(text).unwrap()
    }

    #[test]
    fn api_entries_round_trip_and_config_entries_are_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(
            "version = 1\n[repositories.cfg]\ntype = \"fs\"\npath = \"/srv/c\"\n[policies.nightly]\nrepository = \"cfg\"\nschedule = \"30 2 * * *\"\n",
        );
        let reg = Registry::load(dir.path(), Some(&c)).unwrap();
        let mut e = RepoEntry::new(fs_repo("local", "/srv/r"), ConfigSource::Api);
        let id = Uuid::new_v4();
        e.id = Some(id);
        reg.repos.write().insert("local".into(), e);
        reg.save_repositories(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(REPOSITORIES_FILE)).unwrap();
        assert!(
            text.contains("\"local\"") && !text.contains("\"cfg\""),
            "{text}"
        );

        let again = Registry::load(dir.path(), Some(&c)).unwrap();
        let repos = again.repos.read();
        assert_eq!(repos["local"].id, Some(id));
        assert_eq!(repos["local"].source, ConfigSource::Api);
        assert_eq!(repos["cfg"].source, ConfigSource::Config);
        assert_eq!(again.policy_users("cfg"), ["nightly"]);
        assert!(again.policy_users("local").is_empty());
    }

    #[test]
    fn a_name_in_both_sources_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::default();
        reg.repos.write().insert(
            "local".into(),
            RepoEntry::new(fs_repo("local", "/srv/r"), ConfigSource::Api),
        );
        reg.save_repositories(dir.path()).unwrap();
        let c = config("version = 1\n[repositories.local]\ntype = \"fs\"\npath = \"/srv/x\"\n");
        let e = Registry::load(dir.path(), Some(&c)).err().unwrap();
        assert!(format!("{e:#}").contains("\"local\""), "{e:#}");
        // a reload with the clash changes nothing
        let reg = Registry::load(dir.path(), None).unwrap();
        assert!(reg.replace_config(&c).is_err());
        assert_eq!(
            reg.repos.read()["local"].config.path.as_deref(),
            Some("/srv/r")
        );
    }

    #[test]
    fn a_reload_keeps_unchanged_repositories_and_drops_removed_ones() {
        let reg = Registry::default();
        let c1 = config(
            "version = 1\n[repositories.a]\ntype = \"fs\"\npath = \"/srv/a\"\n[repositories.b]\ntype = \"fs\"\npath = \"/srv/b\"\n",
        );
        reg.replace_config(&c1).unwrap();
        let id = Uuid::new_v4();
        reg.update("a", |e| {
            e.id = Some(id);
            e.mark_reachable(Some(true));
        });
        reg.update("b", |e| e.id = Some(id));
        let c2 = config(
            "version = 1\n[repositories.a]\ntype = \"fs\"\npath = \"/srv/a\"\n[repositories.c]\ntype = \"fs\"\npath = \"/srv/c\"\nreadonly = true\n",
        );
        reg.replace_config(&c2).unwrap();
        let repos = reg.repos.read();
        assert!(repos["a"].status.reachable && repos["a"].id == Some(id));
        assert!(!repos.contains_key("b"));
        assert!(repos["c"].config.readonly);
    }

    #[test]
    fn status_marks() {
        let mut e = RepoEntry::new(fs_repo("a", "/srv/a"), ConfigSource::Api);
        assert!(!e.status.reachable && !e.status.single_writer);
        e.mark_reachable(Some(false));
        assert!(e.status.reachable && e.status.single_writer);
        e.mark_unreachable("boom");
        assert_eq!(e.status.error.as_deref(), Some("boom"));
        assert!(!e.status.reachable && !e.status.checked.is_empty());
        assert!(e.recently_unreachable());
        e.status.checked = "2020-01-01T00:00:00.000Z".into();
        assert!(!e.recently_unreachable());
        let v = serde_json::to_value(e.view(vec!["p".into()])).unwrap();
        assert_eq!(v["policies"][0], "p");
        assert_eq!(v["status"]["reachable"], false);
    }

    #[test]
    fn verify_history_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backup").join(VERIFY_FILE);
        let h = VerifyHistory::load(path.clone());
        let repo = Uuid::new_v4();
        let v = Verified {
            level: VerifyLevel::Data,
            status: VerifyStatus::Ok,
            at: "2026-09-30T14:05:12.101Z".into(),
        };
        h.record(repo, [("b1".to_string(), v.clone())]);
        let h = VerifyHistory::load(path);
        assert_eq!(h.get(repo, "b1"), Some(v));
        assert_eq!(h.get(Uuid::new_v4(), "b1"), None);
        h.remove(repo, "b1");
        assert_eq!(h.get(repo, "b1"), None);
    }
}
