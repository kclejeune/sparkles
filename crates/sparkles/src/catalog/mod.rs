//! A directory of named datasets, with an exclusive registry lock.
//!
//! Dataset names are aliases for their UUIDs. Branches belong to each dataset and
//! never enter the catalog namespace. A catalog is cheap to clone and shares its
//! stores, reservations and lock with every clone.

use crate::store::caches::{CacheSizes, SharedCaches};
use crate::store::{ProcessLock, Store, StoreOptions};
use crate::{Dataset, DatasetOptions, Error, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[cfg(test)]
mod lifecycle_tests;
mod rename;
#[cfg(feature = "backup")]
mod restore;
#[cfg(test)]
mod source_tests;
#[cfg(feature = "backup")]
pub use restore::RESTORE_PREFIX;
#[cfg(feature = "backup")]
mod swap;

/// How a catalog sizes the caches of its datasets (spec X08 §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CatalogCaches {
    /// One set of caches for all datasets, sized by the `cache_bytes`,
    /// `result_cache_bytes`, `service_cache_bytes` and `history_cache_bytes` of the
    /// dataset options the catalog is created with.
    #[default]
    FromStore,
    /// One set of caches for all datasets, of these sizes.
    Sizes(CacheSizes),
    /// Each dataset sizes its own caches by its store options, so memory grows with the
    /// number of datasets.
    PerDataset,
}

/// The memory a catalog's datasets share.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryOptions {
    pub caches: CatalogCaches,
    /// The bytes the estimated memory of all the catalog's queries and updates may
    /// reach together (the query memory pool, spec X08 §4). `None` has no pool, so only
    /// each query's own budget applies. A query that the pool cannot cover fails with
    /// [`BudgetKind::ServerMemory`](crate::BudgetKind).
    pub pool_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct CatalogOptions {
    pub dataset: DatasetOptions,
    pub memory: MemoryOptions,
}
impl From<StoreOptions> for CatalogOptions {
    fn from(store: StoreOptions) -> Self {
        Self {
            dataset: store.into(),
            memory: MemoryOptions::default(),
        }
    }
}

impl CatalogOptions {
    /// The caches the catalog's datasets share, if any.
    fn shared_caches(&self) -> Option<SharedCaches> {
        let store = &self.dataset.store;
        if store.shared.is_some() {
            return None;
        }
        let sizes = match self.memory.caches {
            CatalogCaches::PerDataset => return None,
            CatalogCaches::Sizes(s) => s,
            CatalogCaches::FromStore => CacheSizes {
                blocks: store.cache_bytes,
                results: store.result_cache_bytes,
                service: store.service_cache_bytes,
                history: store.history_cache_bytes,
            },
        };
        Some(SharedCaches::new(sizes))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatasetKind {
    Persistent,
    #[serde(rename = "mem")]
    Memory,
}
impl DatasetKind {
    /// Compatibility with the server's existing `type: mem` representation.
    #[allow(non_upper_case_globals)]
    pub const Mem: Self = Self::Memory;
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DatasetInfo {
    pub name: String,
    pub id: Uuid,
    pub kind: DatasetKind,
    pub path: Option<PathBuf>,
    pub attached: bool,
    pub reserved_by: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CreateDataset {
    pub kind: DatasetKind,
    pub geo: Option<crate::geo::config::GeoConfig>,
}
impl Default for CreateDataset {
    fn default() -> Self {
        Self {
            kind: DatasetKind::Persistent,
            geo: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Attach {
    Memory,
    Directory(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservationKind {
    Clone,
    Restore,
}

struct Entry {
    dataset: Dataset,
    kind: DatasetKind,
    attached: bool,
}
/// The catalog's registry of datasets in the data directory. Its `format` member
/// ([`sparkles_core::formats::CATALOG`]) is added on write and checked on read.
pub const CONFIG_FILE: &str = "config.json";

#[derive(Serialize, Deserialize, Default)]
struct Registry {
    datasets: Vec<RegistryEntry>,
}
#[derive(Serialize, Deserialize)]
struct RegistryEntry {
    name: String,
    #[serde(rename = "type")]
    kind: DatasetKind,
    #[serde(default)]
    reasoning: Option<crate::reasoning::ReasoningRecord>,
}

/// What `config.json` keeps for a persistent dataset whose store is closed while the
/// catalog works on its directory, so that saves made meanwhile keep it registered.
struct Detached {
    id: Option<Uuid>,
    reasoning: Option<crate::reasoning::ReasoningRecord>,
}

pub(crate) struct Inner {
    dir: Option<PathBuf>,
    options: RwLock<CatalogOptions>,
    /// the caches every dataset opened by this catalog reads through
    caches: Option<SharedCaches>,
    /// the pool its datasets' queries take grants from
    pool: Option<Arc<crate::sparql::pool::QueryPool>>,
    entries: RwLock<BTreeMap<String, Entry>>,
    reservations: Mutex<BTreeMap<String, (ReservationKind, String)>>,
    /// Persistent datasets taken out of `entries` for an in-place restore, or left
    /// closed by a rename whose rollback could not reopen them.
    detached: Mutex<BTreeMap<String, Detached>>,
    /// Datasets left closed at open because their files are in a format this release
    /// does not read, with the refusal. They are also in `detached`, so they stay
    /// registered.
    refused: Mutex<BTreeMap<String, String>>,
    manage: Mutex<()>,
    #[cfg(feature = "backup")]
    pub(crate) repositories: Mutex<Option<crate::backup::Repositories>>,
    // Drop the lock only after the stores have closed.
    _lock: Option<ProcessLock>,
}

#[derive(Clone)]
pub struct Catalog {
    pub(crate) inner: Arc<Inner>,
}

/// A reservation releases its name on drop, including cancellation or failure.
pub struct Reservation {
    catalog: Catalog,
    name: String,
    holder: String,
}
impl Reservation {
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut r = self.catalog.inner.reservations.lock();
        if r.get(&self.name).is_some_and(|(_, h)| h == &self.holder) {
            r.remove(&self.name);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CloneRequest {
    pub kind: Option<DatasetKind>,
    pub spec: crate::cloning::Spec,
}

impl Catalog {
    pub fn open(dir: impl AsRef<Path>, options: CatalogOptions) -> Result<Self> {
        let dir = std::path::absolute(dir)?;
        std::fs::create_dir_all(&dir)?;
        let lock = lock_dir(&dir)?;
        std::fs::create_dir_all(dir.join("databases"))?;
        rename::recover(&dir)?;
        #[cfg(feature = "backup")]
        restore::recover(&dir).map_err(component)?;
        for e in std::fs::read_dir(dir.join("databases"))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(".clone-") || name.starts_with(TRASH_PREFIX) {
                std::fs::remove_dir_all(e.path())?;
            }
        }
        let catalog = Self {
            inner: Arc::new(Inner {
                dir: Some(dir.clone()),
                caches: options.shared_caches(),
                pool: options
                    .memory
                    .pool_bytes
                    .map(crate::sparql::pool::QueryPool::new),
                options: RwLock::new(options),
                entries: RwLock::new(BTreeMap::new()),
                reservations: Mutex::new(BTreeMap::new()),
                detached: Mutex::new(BTreeMap::new()),
                refused: Mutex::new(BTreeMap::new()),
                manage: Mutex::new(()),
                #[cfg(feature = "backup")]
                repositories: Mutex::new(None),
                _lock: Some(lock),
            }),
        };
        let reg = read_registry(&dir)?;
        for e in reg.datasets {
            check_name(&e.name)?;
            if catalog.inner.entries.read().contains_key(&e.name) {
                return Err(Error::Corrupt(format!(
                    "duplicate dataset '{}' in config.json",
                    e.name
                )));
            }
            let opened = if e.kind == DatasetKind::Persistent {
                catalog.open_existing_dataset(&e.name, None)
            } else {
                catalog.open_dataset(&e.name, e.kind, None)
            };
            let ds = match opened {
                Ok(ds) => ds,
                // A dataset in a format this release does not read is refused alone: it
                // stays registered and closed, and the catalog serves the others.
                Err(Error::Unsupported(msg)) if e.kind == DatasetKind::Persistent => {
                    let msg = format!(
                        "dataset \"{}\" is not served: {msg} (`sparkles migrate --data {} --dataset {} --check` lists its formats)",
                        e.name,
                        dir.display(),
                        e.name
                    );
                    tracing::error!(target: "sparkles::catalog", "{msg}");
                    catalog.inner.detached.lock().insert(
                        e.name.clone(),
                        Detached {
                            id: None,
                            reasoning: e.reasoning,
                        },
                    );
                    catalog.inner.refused.lock().insert(e.name, msg);
                    continue;
                }
                Err(err) => return Err(err),
            };
            if ds.reasoning_record().is_none() {
                *ds.state().reasoning.write() = e.reasoning;
            }
            catalog.inner.entries.write().insert(
                e.name,
                Entry {
                    dataset: ds,
                    kind: e.kind,
                    attached: false,
                },
            );
        }
        Ok(catalog)
    }

    pub fn memory(options: CatalogOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir: None,
                caches: options.shared_caches(),
                pool: options
                    .memory
                    .pool_bytes
                    .map(crate::sparql::pool::QueryPool::new),
                options: RwLock::new(options),
                entries: RwLock::new(BTreeMap::new()),
                reservations: Mutex::new(BTreeMap::new()),
                detached: Mutex::new(BTreeMap::new()),
                refused: Mutex::new(BTreeMap::new()),
                manage: Mutex::new(()),
                #[cfg(feature = "backup")]
                repositories: Mutex::new(None),
                _lock: None,
            }),
        }
    }
    /// The caches that the catalog's datasets share, unless it sizes them per dataset.
    pub fn caches(&self) -> Option<&SharedCaches> {
        self.inner.caches.as_ref()
    }
    /// The pool that bounds the memory of the catalog's queries, if it has one.
    pub fn pool(&self) -> Option<&Arc<crate::sparql::pool::QueryPool>> {
        self.inner.pool.as_ref()
    }
    /// Replace the options that datasets created or attached from now on open with.
    /// Datasets that are already open keep their options.
    pub fn set_defaults(&self, dataset: DatasetOptions) {
        self.inner.options.write().dataset = dataset;
    }
    pub fn dir(&self) -> Option<&Path> {
        self.inner.dir.as_deref()
    }

    /// Read the atomically replaced registry without opening stores or taking locks.
    /// In-memory entries have no persisted identity, and report the nil UUID. A running
    /// catalog may be renaming or restoring a persistent dataset, so its directory can
    /// be missing for a moment. Such a dataset is left out of the listing.
    pub fn inspect(dir: impl AsRef<Path>) -> Result<Vec<DatasetInfo>> {
        let dir = dir.as_ref();
        let mut out = Vec::new();
        for e in read_registry(dir)?.datasets {
            check_name(&e.name)?;
            let path =
                (e.kind == DatasetKind::Persistent).then(|| dir.join("databases").join(&e.name));
            let id = match &path {
                Some(p) => {
                    let bytes = match std::fs::read(p.join("dataset.json")) {
                        Ok(bytes) => bytes,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e.into()),
                    };
                    let file: serde_json::Value = serde_json::from_slice(&bytes)
                        .map_err(|e| Error::Corrupt(e.to_string()))?;
                    serde_json::from_value(file["id"].clone())
                        .map_err(|e| Error::Corrupt(e.to_string()))?
                }
                None => Uuid::nil(),
            };
            out.push(DatasetInfo {
                name: e.name,
                id,
                kind: e.kind,
                path,
                attached: false,
                reserved_by: None,
            });
        }
        Ok(out)
    }
    pub fn list(&self) -> Vec<DatasetInfo> {
        let r = self.inner.reservations.lock();
        self.inner
            .entries
            .read()
            .iter()
            .map(|(name, e)| DatasetInfo {
                name: name.clone(),
                id: e.dataset.dataset_id(),
                kind: e.kind,
                path: e.dataset.store().root().map(Path::to_path_buf),
                attached: e.attached,
                reserved_by: r.get(name).map(|(_, h)| h.clone()),
            })
            .collect()
    }
    pub fn info(&self, name: &str) -> Option<DatasetInfo> {
        let reservations = self.inner.reservations.lock();
        let entries = self.inner.entries.read();
        let entry = entries.get(name)?;
        Some(DatasetInfo {
            name: name.into(),
            id: entry.dataset.dataset_id(),
            kind: entry.kind,
            path: entry.dataset.store().root().map(Path::to_path_buf),
            attached: entry.attached,
            reserved_by: reservations.get(name).map(|(_, holder)| holder.clone()),
        })
    }
    pub fn get(&self, name: &str) -> Option<Dataset> {
        self.inner
            .entries
            .read()
            .get(name)
            .map(|e| e.dataset.clone())
    }
    /// Look up a dataset for a request that must not reach a dataset being restored.
    /// The reservation check and the lookup happen under one lock, so a restore cannot
    /// start between them. The error is the holder of the restore reservation.
    pub fn get_for_request(&self, name: &str) -> std::result::Result<Option<Dataset>, String> {
        let r = self.inner.reservations.lock();
        if let Some((ReservationKind::Restore, h)) = r.get(name) {
            return Err(h.clone());
        }
        Ok(self.get(name))
    }
    /// Whether a registered dataset has handles besides the catalog's own, including
    /// branch handles. A rename or delete of a persistent dataset refuses while it has.
    pub fn in_use(&self, name: &str) -> bool {
        self.inner
            .entries
            .read()
            .get(name)
            .is_some_and(|e| e.dataset.in_use())
    }
    pub fn get_by_id(&self, id: Uuid) -> Option<Dataset> {
        self.inner
            .entries
            .read()
            .values()
            .find(|e| e.dataset.dataset_id() == id)
            .map(|e| e.dataset.clone())
    }

    /// Clone an already registered dataset by exact identity with controlled waiting.
    /// This never opens a dataset and grants no permission to read it.
    pub fn try_get_by_id(
        &self,
        id: Uuid,
        options: &crate::guard::WriteOptions,
    ) -> Result<Option<Dataset>> {
        loop {
            options.check()?;
            let entries = if options.no_wait {
                self.inner.entries.try_read()
            } else {
                self.inner
                    .entries
                    .try_read_for(std::time::Duration::from_millis(10))
            };
            if let Some(entries) = entries {
                options.check()?;
                let mut dataset = None;
                for entry in entries.values() {
                    options.check()?;
                    if entry.dataset.dataset_id() == id {
                        dataset = Some(entry.dataset.clone());
                        break;
                    }
                }
                options.check()?;
                return Ok(dataset);
            }
            if options.no_wait {
                return Err(Error::WriterBusy);
            }
        }
    }
    fn database_dir(&self, name: &str) -> Result<PathBuf> {
        self.dir()
            .map(|d| d.join("databases").join(name))
            .ok_or_else(|| Error::invalid("a persistent dataset needs a catalog directory"))
    }
    /// The catalog's default dataset options for the dataset `name`, under the catalog's
    /// query memory pool. A dataset that opens its own store also takes a cache scope.
    fn dataset_options(&self, name: &str) -> DatasetOptions {
        DatasetOptions {
            name: Some(name.into()),
            pool: self.inner.pool.clone(),
            ..self.inner.options.read().dataset.clone()
        }
    }
    fn open_dataset(&self, name: &str, kind: DatasetKind, loc: Option<&Path>) -> Result<Dataset> {
        let mut opts = self.dataset_options(name);
        if let Some(c) = &self.inner.caches {
            opts.store.shared = Some(c.scope());
        }
        match kind {
            DatasetKind::Memory => Ok(Dataset::from_store_with(
                Store::in_memory(opts.store.clone()),
                opts,
            )),
            DatasetKind::Persistent => Dataset::open_with(
                match loc {
                    Some(p) => p.to_path_buf(),
                    None => self.database_dir(name)?,
                },
                opts,
            ),
        }
    }
    /// Registered and recovery roots cannot acquire create semantics.
    fn open_existing_dataset(&self, name: &str, expected: Option<Uuid>) -> Result<Dataset> {
        let mut opts = self.dataset_options(name);
        if let Some(caches) = &self.inner.caches {
            opts.store.shared = Some(caches.scope());
        }
        let store = Store::open_existing(&self.database_dir(name)?, opts.store.clone(), expected)?;
        Ok(Dataset::from_store_with(store, opts))
    }
    fn available(&self, name: &str, check_directory: bool) -> Result<()> {
        check_name(name)?;
        if self.get(name).is_some() {
            return Err(Error::Conflict(format!("dataset '{name}' already exists")));
        }
        if let Some((kind, holder)) = self.inner.reservations.lock().get(name) {
            return Err(Error::Conflict(format!(
                "dataset /{name} is being {} by task {holder}",
                match kind {
                    ReservationKind::Clone => "created",
                    ReservationKind::Restore => "restored",
                }
            )));
        }
        if self.inner.detached.lock().contains_key(name) {
            return Err(Error::Conflict(format!(
                "dataset /{name} is registered but closed; reopen the catalog to recover it"
            )));
        }
        if check_directory && self.dir().is_some() && self.database_dir(name)?.exists() {
            return Err(Error::Conflict(format!(
                "directory databases/{name} exists but is not a registered dataset; remove it first"
            )));
        }
        Ok(())
    }
    pub fn create(&self, name: &str, req: &CreateDataset) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.available(name, true)?;
        let ds = self.open_dataset(name, req.kind, None)?;
        let result = (|| {
            if let Some(geo) = &req.geo {
                ds.indexes().geo().enable(geo.clone())?;
            }
            self.inner.entries.write().insert(
                name.into(),
                Entry {
                    dataset: ds.clone(),
                    kind: req.kind,
                    attached: false,
                },
            );
            self.save_locked()
        })();
        if let Err(e) = result {
            self.inner.entries.write().remove(name);
            let root = ds.store().root().map(Path::to_path_buf);
            drop(ds);
            if let Some(root) = root {
                let _ = std::fs::remove_dir_all(root);
            }
            return Err(e);
        }
        Ok(ds)
    }
    pub fn attach(&self, name: &str, source: Attach) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.available(name, false)?;
        let (kind, loc) = match &source {
            Attach::Memory => (DatasetKind::Memory, None),
            Attach::Directory(p) => (DatasetKind::Persistent, Some(p.as_path())),
        };
        let ds = self.open_dataset(name, kind, loc)?;
        self.inner.entries.write().insert(
            name.into(),
            Entry {
                dataset: ds.clone(),
                kind,
                attached: true,
            },
        );
        Ok(ds)
    }
    /// Unregister a dataset and remove the directory of a managed persistent one.
    /// Like [`rename`](Self::rename), it refuses while a managed persistent dataset has
    /// live handles, including branch handles, so that no old handle can write into a
    /// dataset later created under the same name. The directory is renamed aside while
    /// the registry is locked and removed after the lock is released, so deleting a
    /// large dataset does not hold up other catalog operations.
    pub fn delete(&self, name: &str) -> Result<bool> {
        let trash = {
            let _g = self.inner.manage.lock();
            if let Some(holder) = self.reserved_by(name) {
                return Err(Error::Conflict(format!(
                    "dataset /{name} is reserved by task {holder}"
                )));
            }
            let mut map = Some(self.inner.entries.write());
            let Some(entry) = map.as_ref().expect("entries").get(name) else {
                return Ok(false);
            };
            let managed = entry.kind == DatasetKind::Persistent && !entry.attached;
            if managed && entry.dataset.in_use() {
                return Err(Error::Conflict(format!(
                    "dataset /{name} still has live handles"
                )));
            }
            #[cfg(test)]
            lifecycle_tests::before_close_hook();
            let entry = map
                .as_mut()
                .expect("entries")
                .remove(name)
                .expect("checked");
            if managed {
                let root = entry.dataset.store().root().map(Path::to_path_buf);
                let id = entry.dataset.dataset_id();
                let reasoning = match entry.dataset.close_for_disposal(|| drop(map.take())) {
                    Ok(reasoning) => reasoning,
                    Err(refused) => {
                        let dataset = refused.dataset;
                        map.as_mut()
                            .expect("refusal retains entries exclusion")
                            .insert(
                                name.into(),
                                Entry {
                                    dataset,
                                    kind: entry.kind,
                                    attached: entry.attached,
                                },
                            );
                        drop(map.take());
                        if let Some(panic) = refused.panic {
                            std::panic::resume_unwind(panic);
                        }
                        return Err(Error::Conflict(format!(
                            "dataset /{name} still has live handles"
                        )));
                    }
                };
                if let Err(error) = self.save_locked() {
                    return Err(self.reopen_closed_locked(name, id, reasoning, error));
                }
                match root.map(|root| move_to_trash(&root)).transpose() {
                    Ok(trash) => trash,
                    Err(failure) => {
                        return Err(if failure.reopen {
                            self.reopen_closed_locked(name, id, reasoning, failure.error)
                        } else {
                            self.keep_closed_locked(name, id, reasoning, failure.error)
                        });
                    }
                }
            } else {
                drop(map);
                if let Err(error) = self.save_locked() {
                    self.inner.entries.write().insert(name.into(), entry);
                    return Err(error);
                }
                None
            }
        };
        delete_hook();
        if let Some(trash) = trash
            && let Err(error) = std::fs::remove_dir_all(&trash)
        {
            tracing::warn!(path=%trash.display(), %error,
                "deleted dataset cleanup failed; trash is retried on catalog open and retained paths require operator recovery");
        }
        Ok(true)
    }

    /// An irrevocably closed state is never revived on later I/O refusal.
    fn reopen_closed_locked(
        &self,
        name: &str,
        id: Uuid,
        reasoning: Option<crate::reasoning::ReasoningRecord>,
        error: Error,
    ) -> Error {
        let retained_reasoning = reasoning.clone();
        self.inner.detached.lock().insert(
            name.into(),
            Detached {
                id: Some(id),
                reasoning,
            },
        );
        match self.reattach_locked(name, Some(id)) {
            Ok(()) => match self.save_locked() {
                Ok(()) => error,
                Err(save) => Error::Corrupt(format!(
                    "{error}; reopening /{name} succeeded but restoring its registry failed: {save}"
                )),
            },
            Err(reopen) => self.keep_closed_locked(
                name,
                id,
                retained_reasoning,
                Error::Corrupt(format!(
                    "{error}; reopening /{name} after close failed: {reopen}"
                )),
            ),
        }
    }

    /// Keep original identity unavailable when a move cannot be synchronized
    /// back. The retained source path is in `error`; no constructor runs here.
    fn keep_closed_locked(
        &self,
        name: &str,
        id: Uuid,
        reasoning: Option<crate::reasoning::ReasoningRecord>,
        error: Error,
    ) -> Error {
        let message =
            format!("{error}; original dataset {id} remains closed and requires recovery");
        self.inner.detached.lock().insert(
            name.into(),
            Detached {
                id: Some(id),
                reasoning,
            },
        );
        self.inner
            .refused
            .lock()
            .insert(name.into(), message.clone());
        match self.save_locked() {
            Ok(()) => Error::Corrupt(message),
            Err(save) => Error::Corrupt(format!(
                "{message}; retaining its registry entry also failed: {save}"
            )),
        }
    }

    /// Rename an alias, retaining its UUID. Persistent datasets must have no live
    /// handles, including branch handles, so that the old store can be closed.
    pub fn rename(&self, from: &str, to: &str) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.available(to, true)?;
        if self.reserved_by(from).is_some() {
            return Err(Error::Conflict(format!("dataset /{from} is reserved")));
        }
        let mut map = Some(self.inner.entries.write());
        let mut entry = {
            let map = map.as_mut().expect("entries");
            let entry = map
                .get(from)
                .ok_or_else(|| Error::NotFound(format!("no dataset /{from}")))?;
            if entry.attached {
                return Err(Error::Conflict(
                    "an attached dataset cannot be renamed".into(),
                ));
            }
            if entry.kind == DatasetKind::Persistent && entry.dataset.in_use() {
                return Err(Error::Conflict(format!(
                    "dataset /{from} still has live handles"
                )));
            }
            if entry.kind == DatasetKind::Persistent {
                rename::check_prepare(self.dir().expect("persistent catalog"), from)?;
            }
            #[cfg(test)]
            lifecycle_tests::before_close_hook();
            map.remove(from).expect("checked")
        };
        if entry.kind == DatasetKind::Memory {
            drop(map);
            entry.dataset.alias = Some(Arc::from(to));
            self.inner.entries.write().insert(to.into(), entry);
            if let Err(e) = self.save_locked() {
                let mut entry = self.inner.entries.write().remove(to).expect("inserted");
                entry.dataset.alias = Some(Arc::from(from));
                self.inner.entries.write().insert(from.into(), entry);
                return Err(e);
            }
            return Ok(self.get(to).expect("renamed"));
        }
        let dir = self.dir().expect("persistent catalog");
        let id = entry.dataset.dataset_id();
        let reasoning = match entry.dataset.close_for_disposal(|| drop(map.take())) {
            Ok(reasoning) => reasoning,
            Err(refused) => {
                let dataset = refused.dataset;
                map.as_mut()
                    .expect("refusal retains entries exclusion")
                    .insert(
                        from.into(),
                        Entry {
                            dataset,
                            kind: entry.kind,
                            attached: entry.attached,
                        },
                    );
                drop(map.take());
                if let Some(panic) = refused.panic {
                    std::panic::resume_unwind(panic);
                }
                return Err(Error::Conflict(format!(
                    "dataset /{from} still has live handles"
                )));
            }
        };
        if let Err(error) = rename::prepare(dir, from, to, id) {
            return Err(self.roll_back_rename(from, to, id, reasoning, error));
        }
        let old = self.database_dir(from)?;
        let new = self.database_dir(to)?;
        rename::crash_point(1)?;
        let moved = std::fs::rename(&old, &new);
        if moved.is_ok() {
            rename::crash_point(2)?;
        }
        let result = moved.map_err(Error::from).and_then(|()| {
            sync_dir(old.parent().expect("database parent"))?;
            rename::fail_point(rename::FAIL_AFTER_MOVE)?;
            // Publish the reopened store only once the registry names it.
            let ds = self.open_existing_dataset(to, Some(id))?;
            self.inner.detached.lock().insert(
                to.into(),
                Detached {
                    id: Some(id),
                    reasoning: ds.reasoning_record(),
                },
            );
            let saved = self.save_locked();
            self.inner.detached.lock().remove(to);
            saved?;
            self.inner.entries.write().insert(
                to.into(),
                Entry {
                    dataset: ds,
                    kind: DatasetKind::Persistent,
                    attached: false,
                },
            );
            Ok(())
        });
        if let Err(e) = result {
            return Err(self.roll_back_rename(from, to, id, reasoning, e));
        }
        rename::crash_point(3)?;
        rename::finish(dir)?;
        Ok(self.get(to).expect("renamed"))
    }
    /// Put a failed persistent rename back as `config.json` records it. When the store
    /// cannot be reopened, the dataset stays registered in `config.json`, later saves
    /// keep it there, and the next [`Catalog::open`] recovers the directory.
    fn roll_back_rename(
        &self,
        from: &str,
        to: &str,
        id: Uuid,
        reasoning: Option<crate::reasoning::ReasoningRecord>,
        err: Error,
    ) -> Error {
        let dir = self.dir().expect("persistent catalog");
        let registered = read_registry(dir).ok().and_then(|r| {
            r.datasets
                .into_iter()
                .find(|e| e.name == from || e.name == to)
        });
        let reopened = (|| {
            rename::recover(dir)?;
            let name = registered
                .as_ref()
                .map(|r| r.name.clone())
                .ok_or_else(|| Error::Corrupt("renamed dataset is unregistered".into()))?;
            rename::fail_point(rename::FAIL_ROLLBACK_REOPEN)?;
            self.reattach_locked(&name, Some(id))
        })();
        match reopened {
            Ok(()) => err,
            Err(again) => {
                let (name, reasoning) = match registered {
                    Some(r) => (r.name, r.reasoning),
                    None => (from.to_string(), reasoning),
                };
                self.inner.detached.lock().insert(
                    name.clone(),
                    Detached {
                        id: Some(id),
                        reasoning,
                    },
                );
                Error::Corrupt(format!(
                    "renaming /{from} to /{to} failed: {err}; reopening the dataset failed too: \
                     {again}. config.json still registers it as /{name}, and opening the \
                     catalog again recovers its directory"
                ))
            }
        }
    }
    pub fn reserve(&self, name: &str, kind: ReservationKind, holder: &str) -> Result<Reservation> {
        let _g = self.inner.manage.lock();
        // Restore may reserve an existing managed dataset for an in-place swap.
        if kind == ReservationKind::Restore && self.get(name).is_some() {
            if self.reserved_by(name).is_some() {
                return Err(Error::Conflict(format!(
                    "dataset /{name} is already reserved"
                )));
            }
        } else {
            self.available(name, true)?;
        }
        self.inner
            .reservations
            .lock()
            .insert(name.into(), (kind, holder.into()));
        Ok(Reservation {
            catalog: self.clone(),
            name: name.into(),
            holder: holder.into(),
        })
    }
    pub fn reserved_by(&self, name: &str) -> Option<String> {
        self.inner
            .reservations
            .lock()
            .get(name)
            .map(|(_, h)| h.clone())
    }
    pub fn restoring_by(&self, name: &str) -> Option<String> {
        self.inner
            .reservations
            .lock()
            .get(name)
            .filter(|(k, _)| *k == ReservationKind::Restore)
            .map(|(_, h)| h.clone())
    }
    fn check_reservation(&self, r: &Reservation) -> Result<()> {
        if !Arc::ptr_eq(&self.inner, &r.catalog.inner) {
            return Err(Error::invalid("reservation belongs to another catalog"));
        }
        if self.get(r.name()).is_some() {
            return Err(Error::Conflict(format!(
                "dataset /{} already exists",
                r.name()
            )));
        }
        Ok(())
    }
    pub(crate) fn adopt(&self, reservation: Reservation) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.check_reservation(&reservation)?;
        let name = reservation.name();
        let ds = self.open_existing_dataset(name, None);
        let result = ds.and_then(|ds| {
            self.inner.entries.write().insert(
                name.into(),
                Entry {
                    dataset: ds.clone(),
                    kind: DatasetKind::Persistent,
                    attached: false,
                },
            );
            self.save_locked()?;
            Ok(ds)
        });
        if result.is_err() {
            self.inner.entries.write().remove(name);
            let _ = std::fs::remove_dir_all(self.database_dir(name)?);
        }
        result
    }
    pub(crate) fn adopt_memory(
        &self,
        reservation: Reservation,
        store: Store,
        reasoning: Option<crate::reasoning::ReasoningRecord>,
        origin: serde_json::Value,
    ) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.check_reservation(&reservation)?;
        let name = reservation.name();
        let opts = DatasetOptions {
            origin: Some(origin),
            ..self.dataset_options(name)
        };
        let ds = Dataset::from_store_with(store, opts);
        *ds.state().reasoning.write() = reasoning;
        self.inner.entries.write().insert(
            name.into(),
            Entry {
                dataset: ds.clone(),
                kind: DatasetKind::Memory,
                attached: false,
            },
        );
        if let Err(e) = self.save_locked() {
            self.inner.entries.write().remove(name);
            return Err(e);
        }
        Ok(ds)
    }
    pub fn clone_dataset(
        &self,
        src: &str,
        dst: &str,
        req: &CloneRequest,
        ctl: &crate::task::Control,
    ) -> Result<Dataset> {
        let r = self.reserve(dst, ReservationKind::Clone, "clone")?;
        self.clone_reserved(src, r, req, ctl)
    }
    /// Execute a clone under a reservation made before queuing a task.
    pub fn clone_reserved(
        &self,
        src: &str,
        r: Reservation,
        req: &CloneRequest,
        ctl: &crate::task::Control,
    ) -> Result<Dataset> {
        let ds = self
            .get(src)
            .ok_or_else(|| Error::NotFound(format!("no dataset /{src}")))?;
        self.clone_from_reserved(&ds, r, req, ctl).map(|(ds, _)| ds)
    }
    #[doc(hidden)]
    pub fn clone_from_reserved(
        &self,
        ds: &Dataset,
        r: Reservation,
        req: &CloneRequest,
        ctl: &crate::task::Control,
    ) -> Result<(Dataset, crate::store::CloneReport)> {
        ctl.check()?;
        self.check_reservation(&r)?;
        let kind = req.kind.unwrap_or(if ds.store().root().is_some() {
            DatasetKind::Persistent
        } else {
            DatasetKind::Memory
        });
        let (dataset, report) = match kind {
            DatasetKind::Persistent => {
                let dst = self.database_dir(r.name())?;
                let tmp = dst
                    .parent()
                    .expect("database parent")
                    .join(format!(".clone-{}", Uuid::new_v4()));
                let report = crate::cloning::clone_into_controlled(
                    ds.store(),
                    ds.name().unwrap_or(""),
                    ds.reasoning_record(),
                    &tmp,
                    &dst,
                    &req.spec,
                    ctl.part(0.0, 0.95).progress.as_fn(),
                    Some(ctl.cancel.flag()),
                    Some(ctl),
                )
                .map_err(component)?;
                (self.adopt(r)?, report)
            }
            DatasetKind::Memory => {
                let c = ds.clone_to_memory_with(r.name(), &req.spec, &ctl.part(0.0, 0.95))?;
                (
                    self.adopt_memory(r, c.store, c.reasoning, c.origin)?,
                    c.report,
                )
            }
        };
        ctl.progress.report(1.0, "registered");
        Ok((dataset, report))
    }
    pub fn backup_files(&self) -> Result<Vec<BackupFile>> {
        let Some(dir) = self.dir() else {
            return Ok(Vec::new());
        };
        let rd = match std::fs::read_dir(dir.join("backups")) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut files = Vec::new();
        for e in rd {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') && e.file_type()?.is_file() {
                files.push(BackupFile {
                    name,
                    path: e.path(),
                });
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }
    /// The registered datasets this catalog left closed when it opened, because their
    /// files are in a format this release does not read, with the reason (name, message).
    pub fn refused(&self) -> Vec<(String, String)> {
        self.inner
            .refused
            .lock()
            .iter()
            .map(|(n, m)| (n.clone(), m.clone()))
            .collect()
    }
    /// Write `config.json` again, with each dataset's current reasoning record. The
    /// catalog saves after its own changes. Programs that change a dataset's reasoning
    /// record outside the catalog call this to persist it.
    pub fn save(&self) -> Result<()> {
        let _g = self.inner.manage.lock();
        self.save_locked()
    }
    fn save_locked(&self) -> Result<()> {
        let Some(dir) = self.dir() else { return Ok(()) };
        let mut datasets: Vec<RegistryEntry> = self
            .inner
            .entries
            .read()
            .iter()
            .filter(|(_, e)| !e.attached)
            .map(|(name, e)| RegistryEntry {
                name: name.clone(),
                kind: e.kind,
                reasoning: e.dataset.reasoning_record(),
            })
            .collect();
        // A dataset closed for a swap or after a failed rename is still registered.
        for (name, d) in self.inner.detached.lock().iter() {
            if !datasets.iter().any(|e| &e.name == name) {
                datasets.push(RegistryEntry {
                    name: name.clone(),
                    kind: DatasetKind::Persistent,
                    reasoning: d.reasoning.clone(),
                });
            }
        }
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        let reg = Registry { datasets };
        write_file_atomic(
            &dir.join(CONFIG_FILE),
            &sparkles_core::formats::settings_to_vec(CONFIG_FILE, &reg)?,
        )
    }
    /// Take a managed persistent dataset out of the lookup map for an in-place
    /// restore. It stays registered in what [`save`](Self::save) writes until it is
    /// reinserted or reattached.
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    pub(crate) fn detach_for_swap(&self, name: &str) -> Option<Dataset> {
        let _g = self.inner.manage.lock();
        let mut map = self.inner.entries.write();
        if !map
            .get(name)
            .is_some_and(|e| e.kind == DatasetKind::Persistent && !e.attached)
        {
            return None;
        }
        let ds = map.remove(name)?.dataset;
        self.inner.detached.lock().insert(
            name.into(),
            Detached {
                id: Some(ds.dataset_id()),
                reasoning: ds.reasoning_record(),
            },
        );
        Some(ds)
    }
    /// Put back a dataset taken out by [`detach_for_swap`](Self::detach_for_swap).
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    pub(crate) fn reinsert(&self, name: &str, ds: Dataset) {
        let _g = self.inner.manage.lock();
        self.inner.entries.write().insert(
            name.into(),
            Entry {
                dataset: ds,
                kind: DatasetKind::Persistent,
                attached: false,
            },
        );
        self.inner.detached.lock().remove(name);
    }
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    pub(crate) fn reattach(&self, name: &str) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.reattach_locked(name, None)?;
        Ok(self.get(name).expect("reattached"))
    }
    #[cfg(feature = "backup")]
    fn reattach_expected(&self, name: &str, expected: Option<Uuid>) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        if self.get(name).is_some() {
            return Err(Error::Conflict(format!("dataset '{name}' already exists")));
        }
        // This is an intentional restore, so None means a genuine legacy
        // restored root, never a fallback to the replaced dataset's identity.
        let dataset = self.open_existing_dataset(name, expected)?;
        self.publish_reattached(name, dataset);
        Ok(self.get(name).expect("reattached"))
    }
    fn reattach_locked(&self, name: &str, expected: Option<Uuid>) -> Result<()> {
        if self.get(name).is_some() {
            return Err(Error::Conflict(format!("dataset '{name}' already exists")));
        }
        let expected = expected.or_else(|| {
            self.inner
                .detached
                .lock()
                .get(name)
                .and_then(|detached| detached.id)
        });
        let ds = self.open_existing_dataset(name, expected)?;
        self.publish_reattached(name, ds);
        Ok(())
    }
    fn publish_reattached(&self, name: &str, ds: Dataset) {
        self.inner.entries.write().insert(
            name.into(),
            Entry {
                dataset: ds,
                kind: DatasetKind::Persistent,
                attached: false,
            },
        );
        self.inner.detached.lock().remove(name);
        self.inner.refused.lock().remove(name);
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BackupFile {
    pub name: String,
    pub path: PathBuf,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "ui"
        && name != "$"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !name.starts_with('.')
}
fn check_name(name: &str) -> Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(Error::invalid(format!("invalid dataset name '{name}'")))
    }
}
fn read_registry(dir: &Path) -> Result<Registry> {
    match std::fs::read(dir.join(CONFIG_FILE)) {
        // `format` is absent in catalogs written before it existed, which reads as 1
        Ok(b) => sparkles_core::formats::settings_from_slice(CONFIG_FILE, &b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e.into()),
    }
}
/// Take the catalog lock of a data directory without opening its datasets, for offline
/// commands on a stopped server. The lock is held until the returned guard is dropped.
/// A running server holds it, so this fails with [`Error::Locked`] while one runs.
pub fn lock_data_dir(dir: &Path) -> Result<ProcessLock> {
    lock_dir(dir)
}

/// Take `catalog.lock` in `dir`. The guard unlocks the file when the process that took
/// it drops it, as the store's `sparkles.lock` does, so a child forked before exec cannot
/// keep the catalog locked after the owner has closed it.
pub(crate) fn lock_dir(dir: &Path) -> Result<ProcessLock> {
    let path = dir.join("catalog.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => {
            let file = ProcessLock::from_locked_file(file);
            file.set_len(0)?;
            writeln!(&*file, "{}", std::process::id())?;
            file.sync_all()?;
            Ok(file)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = File::open(&path).and_then(|mut f| f.read_to_string(&mut pid));
            Err(Error::Locked {
                path: dir.to_path_buf(),
                pid: pid.trim().parse().ok(),
            })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}
/// Prefix of a deleted dataset's directory while it is being removed.
const TRASH_PREFIX: &str = ".deleted-";
// Existing retained-restore convention: unresolved moves are never trash.
pub(super) const KEPT_PREFIX: &str = ".kept-";
struct DeleteMoveError {
    error: Error,
    reopen: bool,
}

const DELETE_BEFORE_MOVE: u8 = 1;
const DELETE_STAGE_SYNC: u8 = 2;
const DELETE_ROLLBACK_MOVE: u8 = 3;
const DELETE_ROLLBACK_SYNC: u8 = 4;
#[cfg(test)]
thread_local! {
    static DELETE_FAIL: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
fn delete_fail_point(point: u8) -> Result<()> {
    #[cfg(test)]
    if DELETE_FAIL.get() & (1 << point) != 0 {
        return Err(std::io::Error::other(format!("injected delete stage {point}")).into());
    }
    #[cfg(not(test))]
    let _ = point;
    Ok(())
}

/// Keep the actual moved source across uncertain directory synchronization.
/// Only synchronized original-name removal may become disposable trash.
fn move_to_trash(root: &Path) -> std::result::Result<PathBuf, DeleteMoveError> {
    let parent = root.parent().expect("database parent");
    let token = Uuid::new_v4();
    let kept = parent.join(format!("{KEPT_PREFIX}{token}"));
    delete_fail_point(DELETE_BEFORE_MOVE).map_err(|error| DeleteMoveError {
        error,
        reopen: true,
    })?;
    std::fs::rename(root, &kept).map_err(|error| DeleteMoveError {
        error: error.into(),
        reopen: true,
    })?;
    let synchronized = delete_fail_point(DELETE_STAGE_SYNC).and_then(|()| sync_dir(parent));
    if let Err(error) = synchronized {
        let restored = delete_fail_point(DELETE_ROLLBACK_MOVE)
            .and_then(|()| std::fs::rename(&kept, root).map_err(Error::from));
        if let Err(rollback) = restored {
            return Err(DeleteMoveError {
                error: Error::Corrupt(format!(
                    "deletion could not synchronize: {error}; restoring {} failed: {rollback}; original bytes remain at {}",
                    root.display(),
                    kept.display()
                )),
                reopen: false,
            });
        }
        if let Err(rollback) =
            delete_fail_point(DELETE_ROLLBACK_SYNC).and_then(|()| sync_dir(parent))
        {
            return Err(DeleteMoveError {
                error: Error::Corrupt(format!(
                    "deletion could not synchronize: {error}; original bytes returned to {} but rollback sync failed: {rollback}",
                    root.display()
                )),
                reopen: false,
            });
        }
        return Err(DeleteMoveError {
            error,
            reopen: true,
        });
    }
    // Registry removal and original-name removal are durable. Remaining names
    // concern cleanup, not the acknowledged dataset identity.
    let trash = parent.join(format!("{TRASH_PREFIX}{token}"));
    if let Err(error) = std::fs::rename(&kept, &trash) {
        tracing::warn!(path=%kept.display(), %error, "deleted dataset remains at its retained cleanup path");
        return Ok(kept);
    }
    if let Err(error) = sync_dir(parent) {
        tracing::warn!(path=%trash.display(), %error, "deleted dataset cleanup rename was not synchronized");
    }
    Ok(trash)
}

#[cfg(test)]
thread_local! {
    /// Runs after a delete released the registry lock, before it removes the files.
    static DELETE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

fn delete_hook() {
    #[cfg(test)]
    DELETE_HOOK.with_borrow_mut(|h| {
        if let Some(h) = h {
            h()
        }
    });
}

pub(crate) fn component(e: anyhow::Error) -> Error {
    match e.downcast::<Error>() {
        Ok(e) => e,
        Err(e) => Error::invalid(format!("{e:#}")),
    }
}
pub(crate) fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        sync_dir(path.parent().unwrap_or(Path::new(".")))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delete releases the registry lock before it removes the files, so other
    /// catalog operations go on while a large dataset is removed.
    #[test]
    fn a_delete_removes_the_files_without_the_registry_lock() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("big", &Default::default()).unwrap());
        let other = cat.clone();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let inside = seen.clone();
        let databases = dir.path().join("databases");
        DELETE_HOOK.set(Some(Box::new(move || {
            // would deadlock if the delete still held the registry lock
            drop(other.create("big", &Default::default()).unwrap());
            for e in std::fs::read_dir(&databases).unwrap() {
                inside
                    .borrow_mut()
                    .push(e.unwrap().file_name().to_string_lossy().into_owned());
            }
        })));
        let deleted = cat.delete("big");
        DELETE_HOOK.set(None);
        assert!(deleted.unwrap());
        let mut seen = seen.borrow().clone();
        seen.sort();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].starts_with(TRASH_PREFIX), "{seen:?}");
        assert_eq!(seen[1], "big");
        // the trash is gone, and the new dataset is intact
        let left: Vec<_> = std::fs::read_dir(dir.path().join("databases"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["big"]);
        assert!(cat.get("big").is_some());
    }

    #[test]
    fn trash_left_by_a_crash_is_removed_when_the_catalog_opens() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir
            .path()
            .join("databases")
            .join(format!("{TRASH_PREFIX}x"));
        std::fs::create_dir_all(trash.join("gen-0001")).unwrap();
        drop(Catalog::open(dir.path(), Default::default()).unwrap());
        assert!(!trash.exists());
    }

    #[test]
    fn inspect_skips_a_dataset_whose_directory_is_moving() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("a", &Default::default()).unwrap());
        drop(cat.create("b", &Default::default()).unwrap());
        drop(cat);
        let databases = dir.path().join("databases");
        std::fs::rename(databases.join("a"), databases.join(".replaced-a-1")).unwrap();
        let names: Vec<_> = Catalog::inspect(dir.path())
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["b"]);
    }

    #[test]
    fn reattaching_a_repaired_dataset_clears_its_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("b", &Default::default()).unwrap());
        drop(cat);
        let settings = dir.path().join("databases/b/describe.json");
        std::fs::write(&settings, r#"{"format": 2}"#).unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(cat.get("b").is_none());
        assert_eq!(cat.refused().len(), 1);
        std::fs::remove_file(settings).unwrap();
        drop(cat.reattach("b").unwrap());
        assert!(cat.get("b").is_some());
        assert!(cat.refused().is_empty());
        cat.save().unwrap();
        drop(cat);
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(cat.get("b").is_some());
        assert!(cat.refused().is_empty());
    }

    #[test]
    fn a_dataset_with_a_newer_settings_file_is_refused_alone() {
        for (file, body) in [
            ("describe.json", r#"{"format": 2}"#),
            ("rdfs.json", r#"{"rdfsFormat": 2, "graph": "default"}"#),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            drop(cat.create("a", &Default::default()).unwrap());
            drop(cat.create("b", &Default::default()).unwrap());
            drop(cat);
            std::fs::write(dir.path().join("databases/b").join(file), body).unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(cat.get("a").is_some(), "{file}");
            assert!(cat.get("b").is_none(), "{file}: b is served");
            let refused = cat.refused();
            assert_eq!(refused.len(), 1, "{file}: {refused:?}");
            assert_eq!(refused[0].0, "b");
            assert!(refused[0].1.contains(file), "{file}: {refused:?}");
            cat.save().unwrap();
            let reg = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
            assert!(reg.contains("\"b\""), "{file}: {reg}");
        }
    }
    #[test]
    fn a_dataset_with_an_incompatible_branch_is_refused_alone() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("a", &Default::default()).unwrap());
        let bad = cat.create("b", &Default::default()).unwrap();
        let info = bad
            .store()
            .create_branch("dev", &Default::default())
            .unwrap();
        drop(bad);
        drop(cat);
        let settings = dir
            .path()
            .join("databases/b/branches")
            .join(info.id.to_string())
            .join("compaction.json");
        std::fs::write(&settings, br#"{"format":2}"#).unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(cat.get("a").is_some());
        assert!(
            cat.get("b").is_none(),
            "an incompatible branch must refuse its dataset"
        );
        assert_eq!(cat.refused().len(), 1);
        assert_eq!(cat.refused()[0].0, "b");
        assert!(cat.refused()[0].1.contains("compaction.json"));
        cat.save().unwrap();
        let registration = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(registration.contains("\"b\""));
        std::fs::remove_file(settings).unwrap();
        drop(cat.reattach("b").unwrap());
        assert!(cat.refused().is_empty());
        cat.save().unwrap();
        drop(cat);
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(reopened.get("a").is_some());
        assert!(reopened.get("b").is_some());
    }

    /// A child forked from this process and held before exec, so that it shares every
    /// descriptor the process had open when it forked, until [`PreExecChild::release`].
    #[cfg(unix)]
    struct PreExecChild {
        resume: std::os::unix::net::UnixStream,
        worker: std::thread::JoinHandle<std::process::ExitStatus>,
    }

    #[cfg(unix)]
    impl PreExecChild {
        /// Fork a child that waits before exec, and return once it is waiting. After exec it
        /// runs this test binary with `--list` for a filter that matches no test.
        fn hold() -> Self {
            use std::io::Read;
            use std::os::fd::AsRawFd;
            use std::os::unix::{net::UnixStream, process::CommandExt};
            use std::time::{Duration, Instant};

            let (mut ready_parent, ready_child) = UnixStream::pair().unwrap();
            let (resume_parent, resume_child) = UnixStream::pair().unwrap();
            ready_parent
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let close_ready = ready_parent.as_raw_fd();
            let close_resume = resume_parent.as_raw_fd();
            let worker = std::thread::spawn(move || {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--list", "--exact", "no::such::test"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let ready = ready_child.as_raw_fd();
                let resume = resume_child.as_raw_fd();
                // SAFETY: only async-signal-safe close/write/poll/read syscalls run after
                // fork. The parent holds both pairs until the spawn has completed, and the
                // poll timeout bounds cleanup even if the parent fails before releasing it.
                unsafe {
                    command.pre_exec(move || {
                        libc::close(close_ready);
                        libc::close(close_resume);
                        if libc::write(ready, b"x".as_ptr().cast(), 1) != 1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        let mut poll = libc::pollfd {
                            fd: resume,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        if libc::poll(&mut poll, 1, 10_000) <= 0 {
                            return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
                        }
                        let mut byte = 0u8;
                        if libc::read(resume, (&mut byte as *mut u8).cast(), 1) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let spawned = command.spawn();
                drop(ready_child);
                drop(resume_child);
                let mut child = spawned.unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        return status;
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("the pre-exec child failed to exit");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            ready_parent
                .read_exact(&mut [0u8])
                .expect("the child reached pre-exec");
            PreExecChild {
                resume: resume_parent,
                worker,
            }
        }

        /// Let the child exec and exit, and reap it.
        fn release(self) {
            drop(self.resume);
            assert!(self.worker.join().unwrap().success());
        }
    }

    /// The catalog lock, whether a catalog or an offline command holds it, ends when its
    /// owner drops it, even while a child forked meanwhile still shares the locked
    /// descriptor before exec.
    #[cfg(unix)]
    #[test]
    fn catalog_lock_unlocks_during_an_unrelated_pre_exec_child() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("ds", &Default::default()).unwrap());
        let child = PreExecChild::hold();
        drop(cat);
        let reopened = Catalog::open(dir.path(), Default::default());
        // reaped before asserting, so the expected failure before the fix also cleans up
        child.release();
        let reopened = reopened.expect("a pre-exec child kept the catalog's lock");
        assert!(reopened.get("ds").is_some());
        assert!(matches!(
            lock_data_dir(dir.path()),
            Err(Error::Locked { .. })
        ));
        drop(reopened);

        let guard = lock_data_dir(dir.path()).unwrap();
        let child = PreExecChild::hold();
        drop(guard);
        let again = lock_data_dir(dir.path());
        child.release();
        let again = again.expect("a pre-exec child kept the offline catalog lock");
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Locked { .. })
        ));
        drop(again);
    }
}
