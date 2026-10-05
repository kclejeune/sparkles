//! A typed registry of datasets; branches remain on their dataset.
use crate::{DatasetOptions, ErrorKind, FfiDataset, FfiError, FfiResult};
use std::sync::Arc;

#[derive(uniffi::Object)]
pub struct FfiCatalog {
    inner: sparkles::catalog::Catalog,
    options: DatasetOptions,
}
#[derive(uniffi::Object)]
pub struct FfiReservation {
    inner: parking_lot::Mutex<Option<sparkles::catalog::Reservation>>,
}
#[uniffi::export]
impl FfiReservation {
    pub fn release(&self) {
        self.inner.lock().take();
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct CatalogFile {
    pub name: String,
    pub path: String,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct DatasetInfo {
    pub name: String,
    pub id: String,
    pub memory: bool,
    pub path: Option<String>,
    pub attached: bool,
    pub reserved_by: Option<String>,
}
fn info(i: sparkles::catalog::DatasetInfo) -> DatasetInfo {
    DatasetInfo {
        name: i.name,
        id: i.id.to_string(),
        memory: i.kind == sparkles::catalog::DatasetKind::Memory,
        path: i.path.map(|p| p.display().to_string()),
        attached: i.attached,
        reserved_by: i.reserved_by,
    }
}
#[uniffi::export]
impl FfiCatalog {
    pub fn backup_files(&self) -> FfiResult<Vec<CatalogFile>> {
        Ok(self
            .inner
            .backup_files()?
            .into_iter()
            .map(|f| CatalogFile {
                name: f.name,
                path: f.path.display().to_string(),
            })
            .collect())
    }
    pub fn reserve(
        &self,
        name: String,
        kind: String,
        holder: String,
    ) -> FfiResult<Arc<FfiReservation>> {
        self.writable()?;
        let kind = match kind.as_str() {
            "clone" => sparkles::catalog::ReservationKind::Clone,
            "restore" => sparkles::catalog::ReservationKind::Restore,
            _ => {
                return Err(FfiError::new(
                    ErrorKind::Invalid,
                    "unknown reservation kind",
                ));
            }
        };
        Ok(Arc::new(FfiReservation {
            inner: parking_lot::Mutex::new(Some(self.inner.reserve(&name, kind, &holder)?)),
        }))
    }
    pub fn clone_dataset(
        &self,
        source: String,
        target: String,
        memory: Option<bool>,
        operation: Arc<crate::FfiOperation>,
    ) -> FfiResult<Arc<FfiDataset>> {
        self.writable()?;
        operation.control.check()?;
        let request = sparkles::catalog::CloneRequest {
            kind: memory.map(|m| {
                if m {
                    sparkles::catalog::DatasetKind::Memory
                } else {
                    sparkles::catalog::DatasetKind::Persistent
                }
            }),
            ..Default::default()
        };
        Ok(self.wrap(
            self.inner
                .clone_dataset(&source, &target, &request, &operation.control)?,
        ))
    }
    pub fn restore(
        &self,
        repository: Arc<crate::FfiBackupRepository>,
        backup: String,
        target: String,
        operation: Arc<crate::FfiOperation>,
    ) -> FfiResult<Arc<FfiDataset>> {
        self.writable()?;
        operation.control.check()?;
        #[cfg(feature = "backup")]
        {
            let request = sparkles::backup::RestoreRequest {
                target: Some(target),
                ..Default::default()
            };
            Ok(self.wrap(self.inner.restore(
                &repository.inner,
                &backup,
                &request,
                &operation.control,
            )?))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (repository, backup, target);
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn repositories_list(&self) -> FfiResult<Vec<u8>> {
        #[cfg(feature = "backup")]
        {
            crate::documents::encode(&self.inner.repositories()?.list())
        }
        #[cfg(not(feature = "backup"))]
        {
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn repositories_get(&self, name: String) -> FfiResult<Vec<u8>> {
        #[cfg(feature = "backup")]
        {
            crate::documents::encode(&self.inner.repositories()?.get(&name)?)
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = name;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn repositories_put(&self, name: String, url: String, update: bool) -> FfiResult<Vec<u8>> {
        self.writable()?;
        #[cfg(feature = "backup")]
        {
            let cfg = sparkles::backup::RepoConfig::from_url(&name, &url)
                .map_err(sparkles::backup::error)?;
            let repos = self.inner.repositories()?;
            crate::documents::encode(&if update {
                repos.update(&name, cfg)?
            } else {
                repos.add(cfg)?
            })
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (name, url, update);
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn repositories_remove(&self, name: String) -> FfiResult<bool> {
        self.writable()?;
        #[cfg(feature = "backup")]
        {
            Ok(self.inner.repositories()?.remove(&name)?)
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = name;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn repositories_open(&self, name: String) -> FfiResult<Arc<crate::FfiBackupRepository>> {
        #[cfg(feature = "backup")]
        {
            Ok(Arc::new(crate::FfiBackupRepository {
                inner: self.inner.repositories()?.open(&name)?,
            }))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = name;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn repositories_fixed(&self, configs: Vec<u8>) -> FfiResult<()> {
        self.writable()?;
        #[cfg(feature = "backup")]
        {
            self.inner
                .repositories()?
                .with_fixed(&crate::documents::decode::<
                    Vec<sparkles::backup::RepoConfig>,
                >(&configs)?)?;
            Ok(())
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = configs;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn run_policy(
        &self,
        config: Vec<u8>,
        operation: Arc<crate::FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.writable()?;
        operation.control.check()?;
        #[cfg(feature = "backup")]
        {
            crate::documents::encode(
                &self
                    .inner
                    .run_policy(&crate::documents::decode(&config)?, &operation.control)?,
            )
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = config;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
    pub fn apply_retention(&self, config: Vec<u8>, dry_run: bool) -> FfiResult<Vec<u8>> {
        self.writable()?;
        #[cfg(feature = "backup")]
        {
            crate::documents::encode(
                &self
                    .inner
                    .apply_retention(&crate::documents::decode(&config)?, dry_run)?,
            )
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (config, dry_run);
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without backup",
            ))
        }
    }
}
impl FfiCatalog {
    fn writable(&self) -> FfiResult<()> {
        if self.options.read_only {
            Err(FfiError::new(
                ErrorKind::NotPermitted,
                "the catalog was opened read-only",
            ))
        } else {
            Ok(())
        }
    }
    fn wrap(&self, ds: sparkles::Dataset) -> Arc<FfiDataset> {
        FfiDataset::new(ds, self.options.clone())
    }
}
#[uniffi::export]
impl FfiCatalog {
    #[uniffi::constructor]
    pub fn open(path: String, options: DatasetOptions) -> FfiResult<Arc<Self>> {
        Ok(Arc::new(Self {
            inner: sparkles::catalog::Catalog::open(path, Default::default())?,
            options,
        }))
    }
    #[uniffi::constructor]
    pub fn memory(options: DatasetOptions) -> Arc<Self> {
        Arc::new(Self {
            inner: sparkles::catalog::Catalog::memory(Default::default()),
            options,
        })
    }
    pub fn list(&self) -> Vec<DatasetInfo> {
        self.inner.list().into_iter().map(info).collect()
    }
    pub fn info(&self, name: String) -> Option<DatasetInfo> {
        self.inner.info(&name).map(info)
    }
    pub fn get(&self, name: String) -> Option<Arc<FfiDataset>> {
        self.inner.get(&name).map(|d| self.wrap(d))
    }
    pub fn get_by_id(&self, id: String) -> FfiResult<Option<Arc<FfiDataset>>> {
        let found = self
            .inner
            .list()
            .into_iter()
            .find(|d| d.id.to_string() == id);
        Ok(found
            .and_then(|i| self.inner.get(&i.name))
            .map(|d| self.wrap(d)))
    }
    pub fn create(&self, name: String, memory: bool) -> FfiResult<Arc<FfiDataset>> {
        self.writable()?;
        let req = sparkles::catalog::CreateDataset {
            kind: if memory {
                sparkles::catalog::DatasetKind::Memory
            } else {
                sparkles::catalog::DatasetKind::Persistent
            },
            geo: None,
        };
        Ok(self.wrap(self.inner.create(&name, &req)?))
    }
    pub fn attach(&self, name: String, path: Option<String>) -> FfiResult<Arc<FfiDataset>> {
        self.writable()?;
        let source = path
            .map(|p| sparkles::catalog::Attach::Directory(p.into()))
            .unwrap_or(sparkles::catalog::Attach::Memory);
        Ok(self.wrap(self.inner.attach(&name, source)?))
    }
    pub fn delete(&self, name: String) -> FfiResult<bool> {
        self.writable()?;
        Ok(self.inner.delete(&name)?)
    }
    pub fn rename(&self, from: String, to: String) -> FfiResult<Arc<FfiDataset>> {
        self.writable()?;
        Ok(self.wrap(self.inner.rename(&from, &to)?))
    }
}

#[uniffi::export]
pub fn catalog_inspect(path: String) -> FfiResult<Vec<DatasetInfo>> {
    Ok(sparkles::catalog::Catalog::inspect(path)?
        .into_iter()
        .map(info)
        .collect())
}
