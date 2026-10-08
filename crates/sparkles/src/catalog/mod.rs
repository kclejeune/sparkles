//! A directory of named datasets, with an exclusive registry lock.
//!
//! Dataset names are aliases for their UUIDs. Branches belong to each dataset and
//! never enter the catalog namespace. A catalog is cheap to clone and shares its
//! stores, reservations and lock with every clone.

use crate::store::{Store, StoreOptions};
use crate::{Dataset, DatasetOptions, Error, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

mod rename;
#[cfg(feature = "backup")]
mod restore;
#[cfg(feature = "backup")]
pub use restore::RESTORE_PREFIX;
#[cfg(feature = "backup")]
mod swap;

#[derive(Clone, Debug, Default)]
pub struct CatalogOptions {
    pub dataset: DatasetOptions,
}
impl From<StoreOptions> for CatalogOptions {
    fn from(store: StoreOptions) -> Self {
        Self {
            dataset: store.into(),
        }
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
    reasoning: Option<crate::reasoning::ReasoningRecord>,
}

pub(crate) struct Inner {
    dir: Option<PathBuf>,
    options: RwLock<CatalogOptions>,
    entries: RwLock<BTreeMap<String, Entry>>,
    reservations: Mutex<BTreeMap<String, (ReservationKind, String)>>,
    /// Persistent datasets taken out of `entries` for an in-place restore, or left
    /// closed by a rename whose rollback could not reopen them.
    detached: Mutex<BTreeMap<String, Detached>>,
    manage: Mutex<()>,
    #[cfg(feature = "backup")]
    pub(crate) repositories: Mutex<Option<crate::backup::Repositories>>,
    // Drop the lock only after the stores have closed.
    _lock: Option<File>,
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
                options: RwLock::new(options),
                entries: RwLock::new(BTreeMap::new()),
                reservations: Mutex::new(BTreeMap::new()),
                detached: Mutex::new(BTreeMap::new()),
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
            let ds = catalog.open_dataset(&e.name, e.kind, None)?;
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
                options: RwLock::new(options),
                entries: RwLock::new(BTreeMap::new()),
                reservations: Mutex::new(BTreeMap::new()),
                detached: Mutex::new(BTreeMap::new()),
                manage: Mutex::new(()),
                #[cfg(feature = "backup")]
                repositories: Mutex::new(None),
                _lock: None,
            }),
        }
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
    fn database_dir(&self, name: &str) -> Result<PathBuf> {
        self.dir()
            .map(|d| d.join("databases").join(name))
            .ok_or_else(|| Error::invalid("a persistent dataset needs a catalog directory"))
    }
    fn open_dataset(&self, name: &str, kind: DatasetKind, loc: Option<&Path>) -> Result<Dataset> {
        let opts = DatasetOptions {
            name: Some(name.into()),
            ..self.inner.options.read().dataset.clone()
        };
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
            let entry = {
                let mut map = self.inner.entries.write();
                let Some(entry) = map.get(name) else {
                    return Ok(false);
                };
                if entry.kind == DatasetKind::Persistent
                    && !entry.attached
                    && entry.dataset.in_use()
                {
                    return Err(Error::Conflict(format!(
                        "dataset /{name} still has live handles"
                    )));
                }
                map.remove(name).expect("checked")
            };
            if let Err(e) = self.save_locked() {
                self.inner.entries.write().insert(name.into(), entry);
                return Err(e);
            }
            if entry.kind == DatasetKind::Persistent && !entry.attached {
                let root = entry.dataset.store().root().map(Path::to_path_buf);
                // The handle was the last one, so dropping it closes the store.
                drop(entry);
                match root {
                    Some(root) => Some(move_to_trash(&root)?),
                    None => None,
                }
            } else {
                None
            }
        };
        delete_hook();
        if let Some(trash) = trash
            && let Err(e) = std::fs::remove_dir_all(&trash)
        {
            tracing::warn!(
                "removing {}: {e} (the next open of the catalog removes it)",
                trash.display()
            );
        }
        Ok(true)
    }
    /// Rename an alias, retaining its UUID. Persistent datasets must have no live
    /// handles, including branch handles, so that the old store can be closed.
    pub fn rename(&self, from: &str, to: &str) -> Result<Dataset> {
        let _g = self.inner.manage.lock();
        self.available(to, true)?;
        if self.reserved_by(from).is_some() {
            return Err(Error::Conflict(format!("dataset /{from} is reserved")));
        }
        let mut entry = {
            let mut map = self.inner.entries.write();
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
            map.remove(from).expect("checked")
        };
        if entry.kind == DatasetKind::Memory {
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
        let reasoning = entry.dataset.reasoning_record();
        if let Err(e) = rename::prepare(dir, from, to, entry.dataset.dataset_id()) {
            self.inner.entries.write().insert(from.into(), entry);
            return Err(e);
        }
        let old = self.database_dir(from)?;
        let new = self.database_dir(to)?;
        // The handle was the last one, so dropping it closes the store.
        drop(entry);
        rename::crash_point(1)?;
        let moved = std::fs::rename(&old, &new);
        if moved.is_ok() {
            rename::crash_point(2)?;
        }
        let result = moved.map_err(Error::from).and_then(|()| {
            sync_dir(old.parent().expect("database parent"))?;
            rename::fail_point(rename::FAIL_AFTER_MOVE)?;
            // Publish the reopened store only once the registry names it.
            let ds = self.open_dataset(to, DatasetKind::Persistent, None)?;
            self.inner.detached.lock().insert(
                to.into(),
                Detached {
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
            return Err(self.roll_back_rename(from, to, reasoning, e));
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
            self.reattach_locked(&name)
        })();
        match reopened {
            Ok(()) => err,
            Err(again) => {
                let (name, reasoning) = match registered {
                    Some(r) => (r.name, r.reasoning),
                    None => (from.to_string(), reasoning),
                };
                self.inner
                    .detached
                    .lock()
                    .insert(name.clone(), Detached { reasoning });
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
        let ds = self.open_dataset(name, DatasetKind::Persistent, None);
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
            name: Some(name.into()),
            origin: Some(origin),
            ..self.inner.options.read().dataset.clone()
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
            &dir.join("config.json"),
            &serde_json::to_vec_pretty(&reg).map_err(|e| Error::invalid(e.to_string()))?,
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
        self.reattach_locked(name)?;
        Ok(self.get(name).expect("reattached"))
    }
    fn reattach_locked(&self, name: &str) -> Result<()> {
        if self.get(name).is_some() {
            return Err(Error::Conflict(format!("dataset '{name}' already exists")));
        }
        let ds = self.open_dataset(name, DatasetKind::Persistent, None)?;
        self.inner.entries.write().insert(
            name.into(),
            Entry {
                dataset: ds,
                kind: DatasetKind::Persistent,
                attached: false,
            },
        );
        self.inner.detached.lock().remove(name);
        Ok(())
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
    match std::fs::read(dir.join("config.json")) {
        Ok(b) => {
            serde_json::from_slice(&b).map_err(|e| Error::Corrupt(format!("config.json: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn lock_dir(dir: &Path) -> Result<File> {
    let path = dir.join("catalog.lock");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            writeln!(file, "{}", std::process::id())?;
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

/// Rename a deleted dataset's directory aside, so that its name is free at once and
/// removing its files needs no lock.
fn move_to_trash(root: &Path) -> Result<PathBuf> {
    let parent = root.parent().expect("database parent");
    let trash = parent.join(format!("{TRASH_PREFIX}{}", Uuid::new_v4()));
    std::fs::rename(root, &trash)?;
    sync_dir(parent)?;
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
}
