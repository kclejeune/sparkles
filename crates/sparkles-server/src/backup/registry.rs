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

use super::config::ConfigFile;
use crate::state::write_file_atomic;
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
use uuid::Uuid;

/// `repositories.json`
pub const REPOSITORIES_FILE: &str = "repositories.json";
/// `verify.json`
pub const VERIFY_FILE: &str = "verify.json";

/// A registered repository.
pub struct RepoEntry {
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
            config,
            source,
            opened: None,
            status,
            id: None,
            stats: None,
            last_gc: None,
        }
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
}

/// An entry of `repositories.json`.
#[derive(Serialize, Deserialize)]
struct StoredRepo {
    #[serde(flatten)]
    config: RepoConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<Uuid>,
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
        for p in super::policies::read_api_policies(dir)? {
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
        let new_repos = config.repository_configs();
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
                Some(e) if e.config == r => e,
                Some(e) => {
                    let mut n = RepoEntry::new(r, ConfigSource::Config);
                    // the same location keeps its id (and the check that it did not change)
                    if n.config.same_location(&e.config) {
                        n.id = e.id;
                    }
                    n
                }
                None => RepoEntry::new(r, ConfigSource::Config),
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

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_backup::{RepoType, VerifyLevel, VerifyStatus};

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
