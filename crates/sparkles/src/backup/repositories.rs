//! Repository handles and durable configuration, shared by a catalog's callers.
use super::registry::{Registry, RepoEntry};
use super::{BackupError, Code, ConfigSource, OpenEnv, RepoConfig, Repository, error, open};
use crate::Result;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct Repositories {
    pub(crate) dir: Option<PathBuf>,
    pub registry: Arc<Registry>,
    manage: Arc<Mutex<()>>,
    forbid: Vec<PathBuf>,
}
impl Repositories {
    pub(crate) fn load(dir: Option<PathBuf>, forbid: Vec<PathBuf>) -> Result<Self> {
        let registry = match &dir {
            Some(dir) => Registry::load(dir, None).map_err(crate::catalog::component)?,
            None => Registry::default(),
        };
        Ok(Self {
            dir,
            registry: Arc::new(registry),
            manage: Arc::new(Mutex::new(())),
            forbid,
        })
    }
    /// Add immutable operator-defined entries to the shared repository namespace.
    pub fn with_fixed(self, entries: &[RepoConfig]) -> Result<Self> {
        let _g = self.registry.mutation.lock();
        let mut map = self.registry.repos.write();
        let mut names = std::collections::BTreeSet::new();
        for cfg in entries {
            cfg.validate(&self.forbid).map_err(error)?;
            if map.contains_key(&cfg.name) || !names.insert(&cfg.name) {
                return Err(error(BackupError::new(
                    Code::RepositoryExists,
                    format!("repository {} already exists", cfg.name),
                )));
            }
        }
        for cfg in entries {
            map.insert(
                cfg.name.clone(),
                RepoEntry::new(cfg.clone(), ConfigSource::Config),
            );
        }
        drop(map);
        drop(_g);
        Ok(self)
    }
    pub fn list(&self) -> Vec<super::types::Repository> {
        let names: Vec<_> = self.registry.repos.read().keys().cloned().collect();
        names
            .iter()
            .filter_map(|n| self.registry.view(n).ok())
            .collect()
    }
    pub fn get(&self, name: &str) -> Result<super::types::Repository> {
        self.registry.view(name).map_err(error)
    }
    pub fn add(&self, cfg: RepoConfig) -> Result<super::types::Repository> {
        let _g = self.manage.lock();
        cfg.validate(&self.forbid).map_err(error)?;
        let name = cfg.name.clone();
        self.registry
            .insert_repository(self.dir.as_deref(), RepoEntry::new(cfg, ConfigSource::Api))
            .map_err(error)?;
        self.get(&name)
    }
    pub fn update(&self, name: &str, cfg: RepoConfig) -> Result<super::types::Repository> {
        cfg.validate(&self.forbid).map_err(error)?;
        self.registry
            .replace_repository(self.dir.as_deref(), name, cfg)
            .map_err(error)?;
        self.get(name)
    }
    pub fn remove(&self, name: &str) -> Result<bool> {
        self.registry
            .remove_repository(self.dir.as_deref(), name)
            .map_err(error)
    }
    pub fn open(&self, name: &str) -> Result<Arc<Repository>> {
        let _g = self.manage.lock();
        let _mutation = self.registry.mutation.lock();
        if let Some(r) = self.registry.repo(name).map_err(error)? {
            return Ok(r);
        }
        let cfg = self.registry.config(name).map_err(error)?;
        cfg.validate(&self.forbid).map_err(error)?;
        let known = self.registry.repos.read()[name].id;
        let env = OpenEnv {
            init: known.is_none() && !cfg.readonly,
            ..Default::default()
        };
        let r = Arc::new(open(&cfg, &env)?);
        if known.is_some_and(|id| id != r.id()) {
            return Err(error(BackupError::new(
                Code::RepositoryUnavailable,
                "the repository identity changed",
            )));
        }
        self.registry.update(name, |e| {
            e.id = Some(r.id());
            e.opened = Some(r.clone());
            e.mark_reachable(None);
        });
        if let Err(e) = self.save() {
            self.registry.update(name, |entry| {
                entry.id = known;
                entry.opened = None;
            });
            return Err(e);
        }
        Ok(r)
    }
    fn save(&self) -> Result<()> {
        if let Some(dir) = &self.dir {
            self.registry
                .save_repositories(dir)
                .map_err(crate::catalog::component)?;
        }
        Ok(())
    }
}
impl crate::Catalog {
    #[doc(hidden)]
    pub fn share_repositories(&self, registry: Arc<Registry>) -> Result<()> {
        if self
            .inner
            .repositories
            .lock()
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(&r.registry, &registry))
        {
            return Ok(());
        }
        let mut repositories = Repositories::load(
            None,
            self.dir()
                .map(|d| vec![d.to_path_buf()])
                .unwrap_or_default(),
        )?;
        repositories.dir = self.dir().map(|d| d.join("backup"));
        repositories.registry = registry;
        *self.inner.repositories.lock() = Some(repositories);
        Ok(())
    }
    pub fn repositories(&self) -> Result<Repositories> {
        let mut slot = self.inner.repositories.lock();
        if slot.is_none() {
            *slot = Some(Repositories::load(
                self.dir().map(|d| d.join("backup")),
                self.dir()
                    .map(|d| vec![d.to_path_buf()])
                    .unwrap_or_default(),
            )?);
        }
        Ok(slot.as_ref().expect("loaded").clone())
    }
}
