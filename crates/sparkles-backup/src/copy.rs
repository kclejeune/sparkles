//! Verified transfer of complete backups between repositories.

use crate::blob::Hasher;
use crate::chunking::{self, Boundaries, Chunker};
use crate::layout::{blob_key, valid_backup_name};
use crate::lock::LockGuard;
use crate::repo::{is_not_found, parse_time, read_only, sort_newest_first};
use crate::security::Security;
use crate::{
    BackupError, BlobRef, Code, Ctl, FileEntry, LockKind, LockOperation, Manifest, Repository,
    Result,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use object_store::{ObjectStoreExt, PutPayload};
use sparkles_core::formats::READABLE_INDEX_FORMATS;
use std::collections::HashSet;
use std::sync::Arc;

/// Selects backups to copy. All supplied conditions must match.
#[derive(Clone, Debug, Default)]
pub struct CopyOptions {
    /// An empty name list scans every source manifest. An unreadable manifest
    /// cannot be classified by dataset or completion time, so that scan fails.
    pub names: Vec<String>,
    pub dataset: Option<String>,
    /// Include backups completed at or after this RFC 3339 time.
    pub since: Option<String>,
    /// Explicitly allow writing the plaintext of an encrypted backup to an unencrypted repository.
    pub allow_plaintext_destination: bool,
    pub ctl: Ctl,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyReport {
    pub copied: Vec<String>,
    pub already_present: Vec<String>,
    pub uploaded_blobs: u64,
    pub uploaded_bytes: u64,
}

impl Repository {
    /// Copy selected backups to `destination`. Publish each manifest only after all its
    /// source contents verify. A retry checks completed manifests and blob metadata.
    /// Reuse of archived destination blobs checks presence and stored size with HEAD.
    /// A read-only source must stay readable for the duration of the copy, as for restore.
    pub async fn copy_to(
        &self,
        destination: &Repository,
        options: &CopyOptions,
    ) -> Result<CopyReport> {
        if destination.readonly() {
            return Err(read_only(&destination.config.name));
        }
        if self.id() == destination.id() {
            return Err(BackupError::new(
                Code::InvalidRequest,
                "source and destination are the same repository",
            ));
        }
        if self
            .marker
            .encryption
            .as_ref()
            .is_some_and(|value| !value.is_null())
            && destination
                .marker
                .encryption
                .as_ref()
                .is_none_or(serde_json::Value::is_null)
            && !options.allow_plaintext_destination
        {
            return Err(BackupError::new(
                Code::InvalidRequest,
                "copying an encrypted backup to plaintext requires explicit opt-in",
            ));
        }
        let since = options
            .since
            .as_deref()
            .map(|s| {
                parse_time(s).ok_or_else(|| {
                    BackupError::new(Code::InvalidRequest, "since must be an RFC 3339 time")
                })
            })
            .transpose()?;
        let mut names = HashSet::new();
        for name in &options.names {
            if !valid_backup_name(name) {
                return Err(BackupError::new(
                    Code::InvalidName,
                    format!("invalid backup name {name:?}"),
                ));
            }
            names.insert(name.as_str());
        }

        // Take both locks in a consistent order even if two copies run in opposite directions.
        let source = (self, LockOperation::Verify);
        let target = (destination, LockOperation::Create);
        let source_first = self.id() < destination.id();
        let (first, second) = if source_first {
            (source, target)
        } else {
            (target, source)
        };
        let ctl = &options.ctl;
        let first = crate::lock::acquire(first.0, LockKind::Shared, first.1, ctl).await?;
        let second = crate::lock::acquire(second.0, LockKind::Shared, second.1, ctl).await;
        let result = match second {
            Ok(second) => {
                let guards = if source_first {
                    (&first, &second)
                } else {
                    (&second, &first)
                };
                let result = self
                    .copy_locked(destination, options, &names, since, guards)
                    .await;
                if let Err(error) = second.release().await {
                    tracing::warn!(%error, "releasing repository copy lock");
                }
                result
            }
            Err(error) => Err(error),
        };
        if let Err(error) = first.release().await {
            tracing::warn!(%error, "releasing repository copy lock");
        }
        result
    }

    async fn copy_locked(
        &self,
        destination: &Repository,
        options: &CopyOptions,
        names: &HashSet<&str>,
        since: Option<DateTime<Utc>>,
        guards: (&LockGuard, &LockGuard),
    ) -> Result<CopyReport> {
        let ctl = &options.ctl;
        let mut manifests = Vec::new();
        if names.is_empty() {
            // A corrupt manifest may be part of the requested selection.
            for entry in self.scan_manifests().await?.0 {
                manifests.push(entry.manifest?);
            }
        } else {
            for name in names {
                ctl.check()?;
                manifests.push(Arc::new(self.manifest(name).await?));
            }
        }
        sort_newest_first(&mut manifests);
        let mut report = CopyReport::default();
        let target_security = destination.security().await?;
        let gear = destination
            .marker
            .chunking
            .as_ref()
            .map(|_| chunking::gear(destination.id(), &target_security))
            .transpose()?;
        let mut transfer = Transfer {
            source: self,
            destination,
            source_security: self.security().await?,
            target_security,
            gear,
            verified: HashSet::new(),
            ctl,
            until: None,
            added_bytes: 0,
            added_blobs: 0,
        };
        for source in manifests {
            ctl.check()?;
            if options
                .dataset
                .as_ref()
                .is_some_and(|dataset| dataset != &source.dataset.name)
            {
                continue;
            }
            if let Some(since) = since {
                let completed = parse_time(&source.completed).ok_or_else(|| {
                    BackupError::invalid_backup("completed", "not an RFC 3339 time")
                })?;
                if completed < since {
                    continue;
                }
            }
            check_manifest(self, &source)?;
            if let Some(existing) = find_manifest(destination, &source.name).await? {
                verify_existing(
                    destination,
                    &source,
                    existing,
                    &transfer.target_security,
                    ctl,
                )
                .await?;
                report.already_present.push(source.name.clone());
                continue;
            }

            let mut copied = (*source).clone();
            copied.repository_id = destination.id();
            copied.encryption = transfer.target_security.manifest_encryption();
            transfer.until = destination.retention_deadline(guards.1)?;
            let (prior_blobs, prior_bytes) = (transfer.added_blobs, transfer.added_bytes);
            ctl.report(0.0, &format!("copying {}", source.name));
            for (file, original) in copied.files.iter_mut().zip(&source.files) {
                transfer.copy_file(original, file).await?;
            }
            copied
                .requires
                .retain(|feature| feature != chunking::FEATURE);
            if copied.files.iter().any(|file| file.chunker.is_some()) {
                copied.requires.push(chunking::FEATURE.into());
            }
            copied.stats.blobs = copied
                .files
                .iter()
                .map(|file| file.blobs.len() as u64)
                .sum();
            copied.stats.new_blobs = transfer.added_blobs - prior_blobs;
            copied.stats.reused_blobs = copied.stats.blobs - copied.stats.new_blobs;
            copied.stats.added_bytes = transfer.added_bytes - prior_bytes;
            check_manifest(destination, &copied)?;
            guards.0.check_live().await?;
            ctl.check()?;
            let json = serde_json::to_vec(&copied)
                .map_err(|error| BackupError::internal(error.to_string()))?;
            if json.len() as u64 > crate::layout::MAX_MANIFEST_BYTES {
                return Err(BackupError::invalid_backup(
                    "manifest",
                    "copied manifest exceeds the size limit",
                ));
            }
            let body = transfer.target_security.seal_manifest(&copied.name, json)?;
            if destination
                .publish_manifest(&copied, body.into(), ctl, guards.1)
                .await?
                .is_some()
            {
                report.copied.push(copied.name);
            } else {
                // A concurrent copy may have published the same backup.
                let existing = destination.manifest(&copied.name).await?;
                verify_existing(
                    destination,
                    &source,
                    existing,
                    &transfer.target_security,
                    ctl,
                )
                .await?;
                report.already_present.push(copied.name);
            }
        }
        report.uploaded_blobs = transfer.added_blobs;
        report.uploaded_bytes = transfer.added_bytes;
        ctl.report(1.0, "copy complete");
        Ok(report)
    }

    async fn copy_blob(
        &self,
        blob: &crate::BlobRef,
        path: &str,
        plain: Vec<u8>,
        security: &Security,
        ctl: &Ctl,
        until: Option<DateTime<Utc>>,
    ) -> Result<Option<u64>> {
        let key = blob_key(&blob.id);
        let existing = match self.store.head(&key).await {
            Ok(meta) => meta,
            Err(error) if is_not_found(&error) => {
                let payload: PutPayload = security.encode(Bytes::from(plain))?;
                let stored = payload.content_length() as u64;
                self.check_space(stored, None, true)?;
                self.upload.take(stored, ctl).await?;
                ctl.check()?;
                if self
                    .create_object_until(&key, payload, until)
                    .await?
                    .is_some()
                {
                    return Ok(Some(stored));
                }
                self.store.head(&key).await?
            }
            Err(error) => return Err(error.into()),
        };
        check_blob_size(&blob.id, blob.size, existing.size, security)?;
        let may_archive = self.marker.s3_policy.as_ref().is_some_and(|policy| {
            matches!(
                policy.storage_class,
                Some(
                    crate::StorageClass::Glacier
                        | crate::StorageClass::DeepArchive
                        | crate::StorageClass::IntelligentTiering
                )
            )
        });
        if !may_archive {
            match self
                .fetch_blob(&blob.id, blob.size, path, ctl, security)
                .await
            {
                Ok(_) => {}
                // The bucket's lifecycle may have archived a previously readable blob.
                Err(crate::restore::Failure::Other(error))
                    if error.code() == Code::ArchiveRestoreRequired => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }
}

struct Transfer<'a> {
    source: &'a Repository,
    destination: &'a Repository,
    source_security: Security,
    target_security: Security,
    gear: Option<zeroize::Zeroizing<[u64; 256]>>,
    verified: HashSet<String>,
    ctl: &'a Ctl,
    until: Option<DateTime<Utc>>,
    added_bytes: u64,
    added_blobs: u64,
}

impl Transfer<'_> {
    async fn copy_file(&mut self, source: &FileEntry, target: &mut FileEntry) -> Result<()> {
        let eligible = chunking::eligible(&source.path, source.kind);
        let cdc = eligible && self.gear.is_some();
        let piece = self.destination.marker.piece_bytes;
        let rechunk = eligible || source.blobs.iter().any(|blob| blob.size > piece);
        let piece = usize::try_from(piece).map_err(|_| {
            BackupError::new(
                Code::IncompatibleRepository,
                "destination piece size is too large for this platform",
            )
        })?;
        let mut hash = Hasher::new();
        let mut boundaries = Boundaries::default();
        let mut pending = Vec::new();
        target.blobs.clear();
        target.chunker = cdc.then_some(Chunker::FastCdc2016);
        for blob in &source.blobs {
            let plain = self
                .source
                .fetch_blob(
                    &blob.id,
                    blob.size,
                    &source.path,
                    self.ctl,
                    &self.source_security,
                )
                .await?;
            hash.update(&plain);
            if !rechunk {
                target.blobs.push(self.put_blob(&source.path, plain).await?);
                continue;
            }
            let mut remaining = plain.as_slice();
            while !remaining.is_empty() {
                self.ctl.check()?;
                let (length, cut) = if let Some(gear) = self.gear.as_ref().filter(|_| cdc) {
                    boundaries.take(remaining, gear)
                } else {
                    let room = piece - pending.len();
                    (room.min(remaining.len()), remaining.len() >= room)
                };
                pending.extend_from_slice(&remaining[..length]);
                remaining = &remaining[length..];
                if cut {
                    target.blobs.push(
                        self.put_blob(&source.path, std::mem::take(&mut pending))
                            .await?,
                    );
                }
            }
        }
        if rechunk && (!pending.is_empty() || target.blobs.is_empty()) {
            target
                .blobs
                .push(self.put_blob(&source.path, pending).await?);
        }
        check_file_hash(source, hash)
    }

    async fn put_blob(&mut self, path: &str, plain: Vec<u8>) -> Result<BlobRef> {
        let blob = BlobRef {
            id: self.target_security.blob_id(&plain),
            size: plain.len() as u64,
        };
        if self.verified.insert(blob.id.clone())
            && let Some(bytes) = self
                .destination
                .copy_blob(
                    &blob,
                    path,
                    plain,
                    &self.target_security,
                    self.ctl,
                    self.until,
                )
                .await?
        {
            self.added_blobs += 1;
            self.added_bytes += bytes;
        }
        Ok(blob)
    }
}

fn check_manifest(repo: &Repository, manifest: &Manifest) -> Result<()> {
    if manifest.repository_id != repo.id() {
        return Err(BackupError::invalid_backup(
            "repositoryId",
            "does not match the repository marker",
        ));
    }
    crate::manifest::validate_for_repo(manifest, &repo.marker, READABLE_INDEX_FORMATS)
}

async fn find_manifest(repo: &Repository, name: &str) -> Result<Option<Manifest>> {
    match repo.manifest(name).await {
        Ok(manifest) => Ok(Some(manifest)),
        Err(error) if error.code() == Code::NoSuchBackup => Ok(None),
        Err(error) => Err(error),
    }
}

async fn verify_existing(
    repo: &Repository,
    source: &Manifest,
    existing: Manifest,
    security: &Security,
    ctl: &Ctl,
) -> Result<()> {
    check_manifest(repo, &existing)?;
    // A copy preserves source metadata. Repository ids, encryption and blob ids
    // depend on the destination. Compare the complete plaintext file descriptions.
    let mut comparable = existing.clone();
    comparable.repository_id = source.repository_id;
    comparable.encryption.clone_from(&source.encryption);
    comparable.stats = source.stats.clone();
    comparable
        .requires
        .retain(|feature| feature != chunking::FEATURE);
    let mut source_comparable = source.clone();
    source_comparable
        .requires
        .retain(|feature| feature != chunking::FEATURE);
    for (file, source_file) in comparable.files.iter_mut().zip(&source.files) {
        file.blobs.clone_from(&source_file.blobs);
        file.chunker.clone_from(&source_file.chunker);
    }
    if comparable != source_comparable {
        return Err(BackupError::new(
            Code::BackupExists,
            format!(
                "destination backup {:?} differs from the source",
                source.name
            ),
        ));
    }
    let blobs = existing
        .files
        .iter()
        .flat_map(|file| &file.blobs)
        .map(|blob| (&blob.id, blob.size))
        .collect::<HashSet<_>>();
    futures::stream::iter(blobs)
        .map(|(id, size)| async move {
            ctl.check()?;
            let stored = match repo.store.head(&blob_key(id)).await {
                Ok(meta) => meta.size,
                Err(error) if is_not_found(&error) => {
                    return Err(BackupError::invalid_backup(
                        "blobs",
                        format!("missing blob {id}"),
                    ));
                }
                Err(error) => return Err(error.into()),
            };
            check_blob_size(id, size, stored, security)
        })
        .buffer_unordered(repo.config.concurrency())
        .try_for_each(|()| async { Ok(()) })
        .await
}

fn check_blob_size(id: &str, plain: u64, stored: u64, security: &Security) -> Result<()> {
    if stored <= security.blob_limit(plain)? && security.plausible_size(plain, stored) {
        Ok(())
    } else {
        Err(BackupError::invalid_backup(
            "blobs",
            format!("blob {id} has an invalid stored size"),
        ))
    }
}

fn check_file_hash(file: &crate::FileEntry, hash: Hasher) -> Result<()> {
    if hash.finish() != file.sha256 {
        return Err(BackupError::invalid_backup(
            "sha256",
            format!("checksum mismatch in file {:?}", file.path),
        ));
    }
    Ok(())
}
