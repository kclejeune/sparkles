//! Repository backups bound to a dataset owner on the JVM side.
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiResult};
use std::sync::Arc;

#[derive(uniffi::Object)]
pub struct FfiBackupRepository {
    #[cfg(feature = "backup")]
    pub(crate) inner: Arc<sparkles::backup::Repository>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct BackupInfo {
    pub name: String,
    pub repository: String,
    pub dataset_id: String,
    pub commit: u64,
    pub quads: u64,
    pub logical_bytes: u64,
    pub added_bytes: u64,
    pub note: Option<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct BackupVerification {
    pub name: String,
    pub status: String,
    pub missing: Vec<String>,
    pub corrupt: Vec<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct VerifyInfo {
    pub level: String,
    pub status: String,
    pub backups: Vec<BackupVerification>,
    pub millis: u64,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct RepositoryTest {
    pub ok: bool,
    pub conditional_writes: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct GcInfo {
    pub dry_run: bool,
    pub candidates: u64,
    pub deleted: u64,
    pub deleted_bytes: u64,
    pub kept_young: u64,
    pub stored_bytes_after: u64,
    pub millis: u64,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct LockInfo {
    pub id: String,
    pub kind: String,
    pub operation: String,
    pub host: String,
    pub pid: u32,
    pub created: String,
    pub stale: bool,
}
#[allow(dead_code)]
fn unsupported() -> FfiError {
    FfiError::new(
        ErrorKind::Unsupported,
        "this native library was built without backup",
    )
}
#[cfg(feature = "backup")]
fn info(s: sparkles::backup::BackupSummary) -> BackupInfo {
    BackupInfo {
        name: s.name,
        repository: s.repository,
        dataset_id: s.dataset.id.to_string(),
        commit: s.commit.seq,
        quads: s.commit.quads,
        logical_bytes: s.logical_bytes,
        added_bytes: s.added_bytes,
        note: s.note,
    }
}
#[cfg(feature = "backup")]
fn verify_info(r: sparkles::backup::VerifyReport) -> VerifyInfo {
    VerifyInfo {
        level: format!("{:?}", r.level).to_lowercase(),
        status: format!("{:?}", r.status).to_lowercase(),
        backups: r
            .backups
            .into_iter()
            .map(|b| BackupVerification {
                name: b.name,
                status: format!("{:?}", b.status).to_lowercase(),
                missing: b.missing,
                corrupt: b.corrupt,
            })
            .collect(),
        millis: r.millis,
    }
}
#[cfg(feature = "backup")]
fn verify_options(level: &str, op: &FfiOperation) -> FfiResult<sparkles::backup::VerifyOptions> {
    use sparkles::backup::VerifyLevel;
    let level = match level {
        "exists" => VerifyLevel::Exists,
        "data" => VerifyLevel::Data,
        "restore" => VerifyLevel::Restore,
        _ => {
            return Err(FfiError::new(
                ErrorKind::Invalid,
                "unknown verification level",
            ));
        }
    };
    Ok(sparkles::backup::VerifyOptions {
        level,
        ctl: (&op.control).into(),
        ..Default::default()
    })
}

#[uniffi::export]
impl FfiBackupRepository {
    #[uniffi::constructor]
    pub fn open(url: String, initialize: bool) -> FfiResult<Arc<Self>> {
        #[cfg(feature = "backup")]
        {
            let cfg = sparkles::backup::RepoConfig::from_url("jvm", &url)
                .map_err(sparkles::backup::error)?;
            let env = sparkles::backup::OpenEnv {
                init: initialize,
                ..Default::default()
            };
            Ok(Arc::new(Self {
                inner: Arc::new(sparkles::backup::open(&cfg, &env)?),
            }))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (url, initialize);
            Err(unsupported())
        }
    }
    pub fn list(&self) -> FfiResult<Vec<BackupInfo>> {
        #[cfg(feature = "backup")]
        {
            Ok(sparkles::backup::blocking(&self.inner)
                .list(&Default::default())?
                .into_iter()
                .map(info)
                .collect())
        }
        #[cfg(not(feature = "backup"))]
        {
            Err(unsupported())
        }
    }
    pub fn test(&self) -> FfiResult<RepositoryTest> {
        #[cfg(feature = "backup")]
        {
            let r = sparkles::backup::blocking(&self.inner).test()?;
            Ok(RepositoryTest {
                ok: r.ok,
                conditional_writes: r.conditional_writes,
            })
        }
        #[cfg(not(feature = "backup"))]
        {
            Err(unsupported())
        }
    }
    pub fn verify(
        &self,
        names: Vec<String>,
        level: String,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<VerifyInfo> {
        operation.control.check()?;
        #[cfg(feature = "backup")]
        {
            Ok(verify_info(
                sparkles::backup::blocking(&self.inner)
                    .verify(&names, &verify_options(&level, &operation)?)?,
            ))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (names, level);
            Err(unsupported())
        }
    }
    pub fn gc(
        &self,
        dry_run: bool,
        grace_seconds: u64,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<GcInfo> {
        operation.control.check()?;
        #[cfg(feature = "backup")]
        {
            let opts = sparkles::backup::GcOptions {
                dry_run,
                grace: std::time::Duration::from_secs(grace_seconds),
                ctl: (&operation.control).into(),
            };
            let r = sparkles::backup::blocking(&self.inner).gc(&opts)?;
            Ok(GcInfo {
                dry_run: r.dry_run,
                candidates: r.candidates,
                deleted: r.deleted,
                deleted_bytes: r.deleted_bytes,
                kept_young: r.kept_young,
                stored_bytes_after: r.stored_bytes_after,
                millis: r.millis,
            })
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (dry_run, grace_seconds);
            Err(unsupported())
        }
    }
    pub fn locks(&self) -> FfiResult<Vec<LockInfo>> {
        #[cfg(feature = "backup")]
        {
            Ok(sparkles::backup::blocking(&self.inner)
                .locks()?
                .into_iter()
                .map(|r| LockInfo {
                    id: r.id,
                    kind: format!("{:?}", r.kind).to_lowercase(),
                    operation: format!("{:?}", r.operation).to_lowercase(),
                    host: r.holder.host,
                    pid: r.holder.pid,
                    created: r.created,
                    stale: r.stale,
                })
                .collect())
        }
        #[cfg(not(feature = "backup"))]
        {
            Err(unsupported())
        }
    }
    pub fn break_lock(&self, id: String) -> FfiResult<bool> {
        #[cfg(feature = "backup")]
        {
            Ok(sparkles::backup::blocking(&self.inner).break_lock(&id)?)
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = id;
            Err(unsupported())
        }
    }
}
#[uniffi::export]
impl FfiDataset {
    pub fn backups_get(
        &self,
        repository: Arc<FfiBackupRepository>,
        name: String,
    ) -> FfiResult<Option<BackupInfo>> {
        #[cfg(feature = "backup")]
        {
            Ok(self
                .inner
                .ds
                .backups(&repository.inner)
                .get(&name)?
                .map(|m| info(m.summary("jvm"))))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (repository, name);
            Err(unsupported())
        }
    }
    pub fn backups_verify(
        &self,
        repository: Arc<FfiBackupRepository>,
        name: String,
        level: String,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<VerifyInfo> {
        operation.control.check()?;
        #[cfg(feature = "backup")]
        {
            if self
                .inner
                .ds
                .backups(&repository.inner)
                .get(&name)?
                .is_none()
            {
                return Err(FfiError::new(
                    ErrorKind::NotFound,
                    "no backup of this dataset with that name",
                ));
            }
            Ok(verify_info(
                self.inner.ds.backups(&repository.inner).verify_with(
                    &name,
                    &verify_options(&level, &operation)?,
                    &operation.control,
                )?,
            ))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (repository, name, level);
            Err(unsupported())
        }
    }
    pub fn backups_list(&self, repository: Arc<FfiBackupRepository>) -> FfiResult<Vec<BackupInfo>> {
        #[cfg(feature = "backup")]
        {
            Ok(self
                .inner
                .ds
                .backups(&repository.inner)
                .list(&Default::default())?
                .into_iter()
                .map(info)
                .collect())
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = repository;
            Err(unsupported())
        }
    }
    pub fn backups_create(
        &self,
        repository: Arc<FfiBackupRepository>,
        name: String,
        note: Option<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<BackupInfo> {
        operation.control.check()?;
        #[cfg(feature = "backup")]
        {
            let options = sparkles::backup::CreateOptions {
                name,
                note,
                dataset_name: "jvm".into(),
                ..Default::default()
            };
            Ok(info(
                self.inner
                    .ds
                    .backups(&repository.inner)
                    .create_with(&options, &operation.control)?,
            ))
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (repository, name, note);
            Err(unsupported())
        }
    }
    pub fn backups_delete(
        &self,
        repository: Arc<FfiBackupRepository>,
        name: String,
    ) -> FfiResult<bool> {
        #[cfg(feature = "backup")]
        {
            Ok(self.inner.ds.backups(&repository.inner).delete(&name)?)
        }
        #[cfg(not(feature = "backup"))]
        {
            let _ = (repository, name);
            Err(unsupported())
        }
    }
}
