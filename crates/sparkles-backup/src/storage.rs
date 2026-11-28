//! Placement and retention settings of immutable S3 backup objects.

use serde::{Deserialize, Serialize};

/// S3 storage classes that can hold repository blobs. Repository metadata uses the
/// backend default storage class so a backup can still be listed while blobs thaw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StorageClass {
    Standard,
    StandardIa,
    OnezoneIa,
    IntelligentTiering,
    GlacierIr,
    Glacier,
    DeepArchive,
}

#[cfg(feature = "s3")]
impl StorageClass {
    fn header(self) -> &'static str {
        match self {
            Self::Standard => "STANDARD",
            Self::StandardIa => "STANDARD_IA",
            Self::OnezoneIa => "ONEZONE_IA",
            Self::IntelligentTiering => "INTELLIGENT_TIERING",
            Self::GlacierIr => "GLACIER_IR",
            Self::Glacier => "GLACIER",
            Self::DeepArchive => "DEEP_ARCHIVE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectLockMode {
    Governance,
    Compliance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectLock {
    pub mode: ObjectLockMode,
    /// Retain each newly written blob and manifest for this many days.
    pub retention_days: u16,
    /// Apply a legal hold as well. Only the storage operator can remove it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legal_hold: bool,
}

/// Immutable storage policy of a repository initialized on S3.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct S3WritePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_class: Option<StorageClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_lock: Option<ObjectLock>,
}

impl S3WritePolicy {
    pub(crate) fn valid(&self) -> bool {
        (self.storage_class.is_some() || self.object_lock.is_some())
            && self.object_lock.is_none_or(|lock| lock.retention_days > 0)
    }
}

#[cfg(feature = "s3")]
mod s3;
#[cfg(feature = "s3")]
pub(crate) use s3::{Controls, RetentionHead, WriteHeaders};

#[cfg(feature = "s3")]
pub(crate) enum DeleteOutcome {
    Missing,
    Retained,
    Eligible,
    Deleted,
}

impl crate::Repository {
    fn object_lock(&self) -> Option<ObjectLock> {
        self.marker.s3_policy.as_ref()?.object_lock
    }

    pub(crate) fn has_object_lock(&self) -> bool {
        self.object_lock().is_some()
    }

    pub(crate) fn retention_deadline(
        &self,
        guard: &crate::lock::LockGuard,
    ) -> crate::Result<Option<chrono::DateTime<chrono::Utc>>> {
        self.retention_deadline_from(guard.estimated_server_time())
    }

    fn retention_deadline_from(
        &self,
        now: Option<chrono::DateTime<chrono::Utc>>,
    ) -> crate::Result<Option<chrono::DateTime<chrono::Utc>>> {
        let Some(lock) = self.object_lock() else {
            return Ok(None);
        };
        let now = now.ok_or_else(|| {
            crate::BackupError::new(
                crate::Code::RepositoryUnavailable,
                "cannot assign Object Lock retention without a storage server clock",
            )
        })?;
        let until = now + chrono::Duration::days(i64::from(lock.retention_days));
        Ok(chrono::DateTime::from_timestamp_secs(until.timestamp() + 1))
    }
}

#[cfg(feature = "s3")]
impl crate::Repository {
    pub(crate) async fn publish_retention_deadline(
        &self,
        guard: &crate::lock::LockGuard,
    ) -> crate::Result<Option<chrono::DateTime<chrono::Utc>>> {
        if !self.has_object_lock() {
            return Ok(None);
        }
        self.retention_deadline_from(guard.fresh_server_time().await?)
    }

    pub(crate) async fn retain_manifest_blobs(
        &self,
        manifest: &crate::Manifest,
        ctl: &crate::Ctl,
        until: chrono::DateTime<chrono::Utc>,
    ) -> crate::Result<()> {
        use futures::{StreamExt, TryStreamExt};
        let lock = self.object_lock().expect("repository retention configured");
        let controls = self
            .s3_controls
            .as_ref()
            .ok_or_else(|| s3::unavailable("no version-aware S3 transport"))?;
        let ids: std::collections::HashSet<String> = manifest
            .files
            .iter()
            .flat_map(|file| &file.blobs)
            .map(|blob| blob.id.clone())
            .collect();
        futures::stream::iter(ids)
            .map(|id| async move {
                ctl.check()?;
                let key = crate::layout::blob_key(&id);
                let inspect = || self.store.get_opts(&key, RetentionHead::options());
                let (head, version) = retention(inspect().await?)?;
                if lock.legal_hold && !head.legal_hold()? {
                    return Err(s3::unavailable(format!(
                        "required legal hold is missing on blob {id}"
                    )));
                }
                let mode = head
                    .mode()?
                    .ok_or_else(|| s3::unavailable("blob did not report an Object Lock mode"))?;
                if mode != lock.mode && mode != ObjectLockMode::Compliance {
                    return Err(s3::unavailable(
                        "blob has a weaker Object Lock mode than the repository requires",
                    ));
                }
                if head.until()?.is_some_and(|previous| previous >= until) {
                    return Ok(());
                }
                if let Err(error) = controls
                    .extend_retention(&head, &version, mode, until, &self.requests)
                    .await
                {
                    // S3 refuses a shorter date if a concurrent writer already extended it.
                    let (current, current_version) = retention(inspect().await?)?;
                    if version != current_version
                        || current.mode()?.is_none_or(|mode| {
                            mode != lock.mode && mode != ObjectLockMode::Compliance
                        })
                        || (lock.legal_hold && !current.legal_hold()?)
                        || current.until()?.is_none_or(|date| date < until)
                    {
                        return Err(error);
                    }
                }
                Ok(())
            })
            .buffer_unordered(self.config.concurrency())
            .try_collect::<Vec<()>>()
            .await?;
        Ok(())
    }

    pub(crate) async fn delete_locked_object(
        &self,
        key: &object_store::path::Path,
        dry_run: bool,
        requests: &mut crate::GcRequests,
        guard: Option<&crate::lock::LockGuard>,
    ) -> crate::Result<DeleteOutcome> {
        use crate::repo::is_not_found;

        let inspect = || self.store.get_opts(key, RetentionHead::options());
        requests.head += 1;
        let get = match inspect().await {
            Ok(get) => get,
            Err(error) if is_not_found(&error) => return Ok(DeleteOutcome::Missing),
            Err(error) => return Err(error.into()),
        };
        let (head, version) = retention(get)?;
        if head.retained()? {
            return Ok(DeleteOutcome::Retained);
        }
        if dry_run {
            return Ok(DeleteOutcome::Eligible);
        }
        let controls = self
            .s3_controls
            .as_ref()
            .ok_or_else(|| s3::unavailable("no version-aware S3 transport"))?;
        requests.delete += 1;
        match controls
            .delete_version(&head, &version, &self.requests, guard)
            .await
        {
            Ok(true) => Ok(DeleteOutcome::Deleted),
            Ok(false) => Ok(DeleteOutcome::Missing),
            Err(error) if error.code() == crate::Code::RepositoryLocked => Err(error),
            Err(error) => {
                // Retention or a legal hold can be changed after HEAD. The conditional
                // version delete cannot hide the protected version behind a marker.
                requests.head += 1;
                if let Ok(get) = inspect().await
                    && let Ok((head, current)) = retention(get)
                    && current == version
                    && head.retained().unwrap_or(false)
                {
                    return Ok(DeleteOutcome::Retained);
                }
                Err(error)
            }
        }
    }
}

#[cfg(feature = "s3")]
fn retention(get: object_store::GetResult) -> crate::Result<(RetentionHead, String)> {
    let meta = get
        .extensions
        .get::<RetentionHead>()
        .cloned()
        .ok_or_else(|| s3::unavailable("HEAD did not provide Object Lock information"))?;
    let version = get
        .meta
        .version
        .filter(|version| !version.is_empty() && version != "null")
        .ok_or_else(|| s3::unavailable("HEAD did not identify a versioned object"))?;
    Ok((meta, version))
}
