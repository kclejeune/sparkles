//! Verifying backups and repositories.

use crate::blob::{self, HEADER_LEN};
use crate::error::{Code, Result};
use crate::layout;
use crate::{
    BackupError, BackupVerify, LockKind, LockOperation, Manifest, Orphans, Repository, VerifyLevel,
    VerifyOptions, VerifyReport, VerifyRequests, VerifyStatus, lock,
};
use futures::{StreamExt, TryStreamExt};
use object_store::ObjectStoreExt;
use object_store::path::Path as Key;
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

/// Up to this many distinct blobs, a verification of named backups checks presence with
/// one `HEAD` each instead of listing every blob of the repository.
const HEAD_LIMIT: usize = 64;

/// Whether a stored blob object of `stored` bytes can hold `plain` bytes: raw (header
/// plus plaintext), or LZ4 (kept only when it saves at least 10 %).
pub(crate) fn plausible_size(plain: u64, stored: u64) -> bool {
    let h = HEADER_LEN as u64;
    stored == h + plain || (plain > 0 && stored > h && (stored - h) * 10 <= plain * 9)
}

/// One backup under verification.
struct Item {
    name: String,
    manifest: Option<Arc<Manifest>>,
    /// why the manifest itself is unusable
    error: Option<String>,
}

/// The distinct blobs of `m` with their plaintext sizes.
fn blob_refs(m: &Manifest) -> impl Iterator<Item = (&str, u64)> {
    m.files
        .iter()
        .flat_map(|f| f.blobs.iter().map(|b| (b.id.as_str(), b.size)))
}

/// Checks the verification needs before a manifest's blobs are looked up (the full
/// validation is `manifest::validate`): blob ids that name objects.
fn check_blob_ids(m: &Manifest) -> std::result::Result<(), String> {
    for (i, f) in m.files.iter().enumerate() {
        for (j, b) in f.blobs.iter().enumerate() {
            if !layout::valid_blob_id(&b.id) {
                return Err(format!(
                    "invalid backup: files[{i}].blobs[{j}].id: {:?} is not a blob id",
                    b.id
                ));
            }
        }
    }
    Ok(())
}

impl Repository {
    /// Verify the backups `names`, or with no names the whole repository (every
    /// backup, plus `orphans`: blobs no manifest references):
    /// * `exists`: every manifest parses and validates, and every referenced blob is
    ///   present with its stored length (one `LIST blobs/`, or a `HEAD` per blob when
    ///   that is cheaper for a few backups);
    /// * `data`: and every blob is downloaded, decoded and hashed (`corrupt`);
    /// * `restore` (one backup at a time): and a full restore into a fresh directory
    ///   under `o.tmp_dir`, `check` in full mode, head and quad count, then removal.
    ///
    /// A missing backup name is `404 no-such-backup`. Problems are reported per backup
    /// (`status: error` with `missing` / `corrupt`), not as an `Err`; the report's
    /// status is `error` if any backup failed, `warning` for orphans only, else `ok`.
    ///
    /// A backup whose manifest is unusable has `status: error` and the reason in
    /// `check.error`. At level `restore`, a backup whose blobs all check out is then
    /// restored by [`Repository::verify_restore`], whose result is its entry.
    pub async fn verify(&self, names: &[String], o: &VerifyOptions) -> Result<VerifyReport> {
        o.ctl.check()?;
        let started = Instant::now();
        let guard = lock::acquire(self, LockKind::Shared, LockOperation::Verify, &o.ctl).await?;
        let r = self.verify_locked(names, o, started).await;
        if let Err(e) = guard.release().await {
            tracing::warn!(target: "sparkles::backup", "releasing a lock: {e}");
        }
        r
    }

    async fn verify_locked(
        &self,
        names: &[String],
        o: &VerifyOptions,
        started: Instant,
    ) -> Result<VerifyReport> {
        let ctl = &o.ctl;
        let mut req = VerifyRequests::default();
        let whole_repo = names.is_empty();

        // the manifests
        let mut items: Vec<Item> = Vec::new();
        if whole_repo {
            let (entries, gets) = self.scan_manifests().await?;
            req.list += 1;
            req.get += gets;
            for e in entries {
                items.push(match e.manifest {
                    Ok(m) => Item {
                        name: e.name,
                        manifest: Some(m),
                        error: None,
                    },
                    Err(err) => Item {
                        name: e.name,
                        manifest: None,
                        error: Some(err.message().to_string()),
                    },
                });
            }
            items.sort_by(|a, b| a.name.cmp(&b.name));
        } else {
            let mut seen = HashSet::new();
            for name in names {
                if !seen.insert(name.as_str()) {
                    continue;
                }
                ctl.check()?;
                req.get += 1;
                items.push(match self.manifest(name).await {
                    Ok(m) => Item {
                        name: name.clone(),
                        manifest: Some(Arc::new(m)),
                        error: None,
                    },
                    Err(e) if e.code() == Code::InvalidBackup => Item {
                        name: name.clone(),
                        manifest: None,
                        error: Some(e.message().to_string()),
                    },
                    Err(e) => return Err(e),
                });
            }
        }
        for it in &mut items {
            let Some(m) = &it.manifest else { continue };
            let bad = match crate::manifest::validate(
                m,
                self.marker.piece_bytes,
                sparkles_core::builder::FORMAT_VERSION,
            ) {
                // another index format is intact, only not restorable by this build
                Ok(()) => None,
                Err(e) if matches!(e.code(), Code::IncompatibleFormat | Code::NotImplemented) => {
                    None
                }
                Err(e) => Some(e.message().to_string()),
            }
            .or_else(|| check_blob_ids(m).err());
            if let Some(msg) = bad {
                it.error = Some(msg);
                it.manifest = None;
            }
        }

        // the blobs they need
        let mut needed: HashMap<String, u64> = HashMap::new();
        for m in items.iter().filter_map(|i| i.manifest.as_ref()) {
            for (id, size) in blob_refs(m) {
                needed.entry(id.to_string()).or_insert(size);
            }
        }
        let mut missing: HashSet<String> = HashSet::new();
        let mut corrupt: HashSet<String> = HashSet::new();
        let mut orphans = None;
        let list_blobs = whole_repo || needed.len() > HEAD_LIMIT;
        if list_blobs {
            ctl.check()?;
            let listed: HashMap<String, u64> = self
                .store
                .list(Some(&Key::from(layout::BLOBS)))
                .try_filter_map(|m| async move {
                    Ok(layout::blob_id_of(&m.location).map(|id| (id.to_string(), m.size)))
                })
                .try_collect()
                .await?;
            req.list += 1;
            for (id, &size) in &needed {
                if !listed.get(id).is_some_and(|&s| plausible_size(size, s)) {
                    missing.insert(id.clone());
                }
            }
            if whole_repo {
                let (mut blobs, mut bytes) = (0, 0);
                for (id, size) in &listed {
                    if !needed.contains_key(id) {
                        blobs += 1;
                        bytes += size;
                    }
                }
                orphans = Some(Orphans { blobs, bytes });
            }
        } else if o.level == VerifyLevel::Exists {
            let heads: Vec<(String, bool)> = futures::stream::iter(needed.iter())
                .map(|(id, &size)| async move {
                    ctl.check()?;
                    match self.store.head(&layout::blob_key(id)).await {
                        Ok(m) => Ok((id.clone(), plausible_size(size, m.size))),
                        Err(e) if crate::repo::is_not_found(&e) => Ok((id.clone(), false)),
                        Err(e) => Err(BackupError::from(e)),
                    }
                })
                .buffer_unordered(self.config.concurrency())
                .try_collect()
                .await?;
            req.head += heads.len() as u64;
            missing.extend(heads.into_iter().filter(|(_, ok)| !ok).map(|(id, _)| id));
        }

        // data: download and hash every blob not already known missing
        if o.level != VerifyLevel::Exists {
            let todo: Vec<(String, u64)> = needed
                .iter()
                .filter(|(id, _)| !missing.contains(*id))
                .map(|(id, &s)| (id.clone(), s))
                .collect();
            let total = todo.len();
            let mut done = 0usize;
            let mut results = futures::stream::iter(todo)
                .map(
                    |(id, size)| async move { (id.clone(), self.check_blob(&id, size, ctl).await) },
                )
                .buffer_unordered(self.config.concurrency());
            while let Some((id, r)) = results.next().await {
                req.get += 1;
                match r? {
                    BlobState::Ok => {}
                    BlobState::Missing => {
                        missing.insert(id);
                    }
                    BlobState::Corrupt => {
                        corrupt.insert(id);
                    }
                }
                done += 1;
                ctl.report(
                    done as f32 / total.max(1) as f32 * 0.9,
                    &format!("checked {done}/{total} blobs"),
                );
            }
        }

        // per backup
        let mut backups = Vec::with_capacity(items.len());
        for it in &items {
            let Some(m) = &it.manifest else {
                backups.push(BackupVerify {
                    name: it.name.clone(),
                    status: VerifyStatus::Error,
                    missing: Vec::new(),
                    corrupt: Vec::new(),
                    check: Some(json!({ "error": it.error.clone().unwrap_or_default() })),
                });
                continue;
            };
            let ids: BTreeSet<&str> = blob_refs(m).map(|(id, _)| id).collect();
            let miss: Vec<String> = ids
                .iter()
                .filter(|id| missing.contains(**id))
                .map(|s| s.to_string())
                .collect();
            let bad: Vec<String> = ids
                .iter()
                .filter(|id| corrupt.contains(**id))
                .map(|s| s.to_string())
                .collect();
            let v = BackupVerify {
                name: it.name.clone(),
                status: if miss.is_empty() && bad.is_empty() {
                    VerifyStatus::Ok
                } else {
                    VerifyStatus::Error
                },
                missing: miss,
                corrupt: bad,
                check: None,
            };
            if o.level == VerifyLevel::Restore && v.status == VerifyStatus::Ok {
                // only a backup whose blobs all check out is restored
                ctl.report(0.9, &format!("restoring {}", it.name));
                backups.push(self.verify_restore(&it.name, o).await?);
            } else {
                backups.push(v);
            }
        }
        let status = if backups.iter().any(|b| b.status == VerifyStatus::Error) {
            VerifyStatus::Error
        } else if orphans.is_some_and(|o| o.blobs > 0) {
            VerifyStatus::Warning
        } else {
            VerifyStatus::Ok
        };
        Ok(VerifyReport {
            level: o.level,
            status,
            backups,
            orphans,
            requests: req,
            millis: started.elapsed().as_millis() as u64,
        })
    }

    /// Download blob `id` and check it decodes to `size` bytes with that hash.
    async fn check_blob(&self, id: &str, size: u64, ctl: &crate::Ctl) -> Result<BlobState> {
        ctl.check()?;
        self.download.take(size, ctl).await?;
        let got = match self.store.get(&layout::blob_key(id)).await {
            Ok(g) => g,
            Err(e) if crate::repo::is_not_found(&e) => return Ok(BlobState::Missing),
            Err(e) => return Err(e.into()),
        };
        let stored = got.bytes().await?;
        let id = id.to_string();
        let ok = tokio::task::spawn_blocking(move || blob::decode(&stored, &id, size).is_ok())
            .await
            .map_err(|e| BackupError::internal(format!("a verify worker failed: {e}")))?;
        Ok(if ok {
            BlobState::Ok
        } else {
            BlobState::Corrupt
        })
    }
}

/// The outcome of downloading one blob.
enum BlobState {
    Ok,
    Missing,
    Corrupt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_sizes() {
        assert!(plausible_size(0, 16));
        assert!(!plausible_size(0, 17));
        assert!(plausible_size(100, 116));
        assert!(plausible_size(100, 16 + 90));
        assert!(!plausible_size(100, 16 + 91));
        assert!(!plausible_size(100, 117));
        assert!(!plausible_size(100, 10));
    }
}
