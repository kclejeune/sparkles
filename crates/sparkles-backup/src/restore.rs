//! Restoring a backup into a directory, and the offline directory swap.
//!
//! A manifest and its blobs come from storage someone else may control. Before anything
//! is written the manifest is validated (`manifest::validate`); paths are joined only
//! after that, and every file is created with `create_new` inside the fresh restore
//! directory. Each blob is checked (header, length, SHA-256) before its bytes are
//! written, and each file's own SHA-256 after. The directory is published by the
//! caller with a rename, so a failed restore never leaves a partial database behind.

use crate::blob::Hasher;
use crate::error::Result;
use crate::layout::blob_key;
use crate::lock::{self};
use crate::manifest;
use crate::{
    BackupError, BackupVerify, Code, Ctl, Identity, LockKind, LockOperation, Manifest, Repository,
    RestoreOptions, RestoreRecord, RestoreReport, RestoreRepository, RestoreSource, VerifyOptions,
    VerifyStatus,
};
use futures::{StreamExt, TryStreamExt};
use object_store::{GetOptions, ObjectStore};
use serde_json::Value as J;
use sparkles_core::check::{CheckOptions, Status};
use sparkles_core::commit::ForkedFrom;
use sparkles_core::store::Store;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use uuid::Uuid;

/// A blob whose download does not verify is fetched again this many times.
const BLOB_RETRIES: usize = 2;
/// Cancellation is checked at least this often while a blob streams in.
const CANCEL_EVERY: usize = 8 << 20;

/// Why a restore failed, in the detail that verification at the `restore` level
/// reports (a missing or corrupt blob, a failed check) or as a plain error.
#[derive(Debug)]
pub(crate) enum Failure {
    /// a blob the manifest names does not exist
    Missing {
        id: String,
        path: String,
    },
    /// a blob does not decode to its length and id, also after the retries
    Corrupt {
        id: String,
        path: String,
    },
    /// the integrity check of the restored directory found errors
    Check {
        report: J,
    },
    Other(BackupError),
}

impl From<BackupError> for Failure {
    fn from(e: BackupError) -> Failure {
        Failure::Other(e)
    }
}

impl From<Failure> for BackupError {
    fn from(f: Failure) -> BackupError {
        match f {
            Failure::Missing { id, path } => {
                BackupError::internal(format!("missing blob {id} ({path})"))
                    .with("blob", id)
                    .with("path", path)
            }
            Failure::Corrupt { id, path } => {
                BackupError::internal(format!("checksum mismatch in blob {id} ({path})"))
                    .with("blob", id)
                    .with("path", path)
            }
            Failure::Check { report } => {
                BackupError::internal("the restored database fails its integrity check")
                    .with("check", report)
            }
            Failure::Other(e) => e,
        }
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Failure {
        Failure::Other(e.into())
    }
}

impl From<sparkles_core::Error> for Failure {
    fn from(e: sparkles_core::Error) -> Failure {
        Failure::Other(e.into())
    }
}

/// Run blocking file work on Tokio's blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::result::Result<T, Failure> + Send + 'static,
) -> std::result::Result<T, Failure> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BackupError::internal(format!("restore worker: {e}")))?
}

impl Repository {
    /// Restore backup `name` into `tmp`, which must not exist (the caller picks a
    /// sibling of the final location and publishes it by rename):
    /// 1. a shared lock (none for read-only repositories); `manifest::validate`;
    ///    `422 incompatible-format` unless `indexFormat == builder::FORMAT_VERSION`;
    ///    `507 insufficient-storage` unless the free space of `tmp`'s filesystem is at
    ///    least 1.1 × the logical size;
    /// 2. download every file (parallel, download throttle), verifying each blob's
    ///    length and SHA-256 before writing it (a mismatch is fetched again up to 2
    ///    times, then fails with `checksum mismatch in blob <id> (<path>)`), then each
    ///    file's own `sha256`; files are created with `create_new` inside `tmp` only;
    /// 3. the identity rule (`o.identity`, `o.id_in_use`, `o.in_place_head`;
    ///    `409 duplicate-dataset-id`), with `sparkles_core::commit::reidentify` for `new`;
    /// 4. fsync every file and directory; write `restore.json` ([`crate::RestoreRecord`]);
    /// 5. `sparkles_core::check` (quick or full per `o.check`; `Warning` is success);
    /// 6. open the store with `o.store_opts`: its head must be `commit.seq` and its
    ///    quad count `commit.quads` (`500 restore-mismatch`); close it.
    ///
    /// The identity rule: `auto` keeps the id unless `o.id_in_use` says a dataset has
    /// it (for an in-place restore, the replaced dataset counts), and then mints a new
    /// one. `keep` is refused (`409 duplicate-dataset-id`) when the id is in use, or, in
    /// place (`o.in_place_head` set), when the replaced dataset's head is ahead of the
    /// backup's commit; in place, a server passes `in_place_head` only for a replaced
    /// dataset with the backup's id or after checking no other dataset has it.
    ///
    /// Any failure (or cancellation, checked between requests) removes `tmp`.
    pub async fn restore(
        &self,
        name: &str,
        tmp: &Path,
        o: &RestoreOptions,
    ) -> Result<RestoreReport> {
        Ok(self
            .restore_as(name, tmp, o, LockOperation::Restore)
            .await?)
    }

    /// [`restore`](Self::restore), with the failure kept apart for verification and
    /// the lock taken for `op`.
    pub(crate) async fn restore_as(
        &self,
        name: &str,
        tmp: &Path,
        o: &RestoreOptions,
        op: LockOperation,
    ) -> std::result::Result<RestoreReport, Failure> {
        let t0 = Instant::now();
        o.ctl.check()?;
        // created first, so that everything written afterwards is inside it; an
        // existing directory is refused and left alone
        std::fs::create_dir(tmp).map_err(|e| {
            let msg = if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("the restore directory {} already exists", tmp.display())
            } else {
                format!("cannot create {}: {e}", tmp.display())
            };
            BackupError::new(Code::InvalidRequest, msg)
        })?;
        let r = match lock::acquire(self, LockKind::Shared, op, &o.ctl).await {
            Ok(lock) => {
                let r = self.restore_locked(name, tmp, o, t0).await;
                let _ = lock.release().await;
                r
            }
            Err(e) => Err(e.into()),
        };
        if r.is_err() {
            let dir = tmp.to_path_buf();
            let _ = tokio::task::spawn_blocking(move || remove_dir(&dir)).await;
        }
        r
    }

    async fn restore_locked(
        &self,
        name: &str,
        tmp: &Path,
        o: &RestoreOptions,
        t0: Instant,
    ) -> std::result::Result<RestoreReport, Failure> {
        let security = self.security().await?;
        let (m, _, _) = manifest::fetch_with_security(self, name, None, &security).await?;
        manifest::validate(
            &m,
            self.marker.piece_bytes,
            sparkles_core::builder::FORMAT_VERSION,
        )?;
        let keep = identity_rule(&m, o)?;
        check_free_space(
            tmp,
            manifest::logical_size(&m),
            o.store_opts.min_free_disk_bytes.unwrap_or(0),
            &format!("restoring {name}"),
        )?;
        o.ctl.report(0.02, "downloading");
        self.download(&m, tmp, &o.ctl, &security).await?;
        o.ctl.check()?;

        let source = ForkedFrom {
            id: m.dataset.id,
            seq: m.commit.seq,
        };
        let (dataset_id, forked_from) = if keep {
            (m.dataset.id, None)
        } else {
            (Uuid::new_v4(), Some(source))
        };
        let next_ordinal = m
            .dataset
            .next_ordinal
            .into_iter()
            .chain(m.dataset.branch.as_ref().map(|b| b.next_ordinal))
            .max()
            .filter(|n| *n > 1);
        let record = RestoreRecord {
            restore_format: 1,
            repository: RestoreRepository {
                name: self.config.name.clone(),
                id: self.id(),
            },
            backup: m.name.clone(),
            source: RestoreSource {
                dataset_id: m.dataset.id,
                seq: m.commit.seq,
                name: m.dataset.name.clone(),
            },
            identity: if keep { "kept" } else { "new" }.to_string(),
            time: crate::now_rfc3339(),
        };
        {
            let dir = tmp.to_path_buf();
            blocking(move || {
                // Refuse a newer dataset before reidentification or publication,
                // including restores that request no integrity check.
                sparkles_core::commit::check_dataset_compatibility(&dir)?;
                if let Some(from) = forked_from {
                    sparkles_core::commit::reidentify(&dir, dataset_id, from)?;
                }
                if let Some(next) = next_ordinal {
                    sparkles_core::commit::reserve_branch_ordinals(&dir, dataset_id, next as u64)?;
                }
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dir.join("restore.json"))?;
                f.write_all(&serde_json::to_vec_pretty(&record).expect("serializes"))?;
                sync_tree(&dir)?;
                Ok(())
            })
            .await?;
        }
        o.ctl.check()?;

        let check = match o.check {
            crate::CheckLevel::None => None,
            level => {
                o.ctl.report(0.9, "checking");
                let dir = tmp.to_path_buf();
                let quick = level == crate::CheckLevel::Quick;
                let report = blocking(move || {
                    Ok(sparkles_core::check::check(&dir, &CheckOptions { quick })?)
                })
                .await?;
                let json = serde_json::to_value(&report).expect("a check report serializes");
                if report.status == Status::Error {
                    return Err(Failure::Check { report: json });
                }
                Some(json)
            }
        };
        o.ctl.check()?;

        o.ctl.report(0.95, "opening");
        let (dir, opts) = (tmp.to_path_buf(), o.store_opts.clone());
        let (head, quads, id) = blocking(move || {
            let s = Store::open(&dir, opts)?;
            let r = (s.head_commit().seq, s.snapshot().len(), s.dataset_id());
            drop(s);
            Ok(r)
        })
        .await?;
        if head != m.commit.seq || quads != m.commit.quads || id != dataset_id {
            return Err(BackupError::new(
                Code::RestoreMismatch,
                format!(
                    "the restored database has head {head}, {quads} quads and dataset id {id}; \
                     the backup has commit {}, {} quads and dataset id {dataset_id}",
                    m.commit.seq, m.commit.quads
                ),
            )
            .into());
        }
        o.ctl.report(1.0, "restored");
        Ok(RestoreReport {
            backup: m.summary(&self.config.name),
            dataset_id,
            identity: if keep { "kept" } else { "new" },
            forked_from,
            check,
            millis: t0.elapsed().as_millis() as u64,
        })
    }

    /// Download every file of `m` into `dir` (which exists and is empty) and check the
    /// contents a manifest can only promise: each file's SHA-256, `CURRENT` naming the
    /// generation, and the generation's `commit.json` naming the dataset and a base
    /// commit no later than the backup's.
    async fn download(
        &self,
        m: &Manifest,
        dir: &Path,
        ctl: &Ctl,
        security: &crate::security::Security,
    ) -> std::result::Result<(), Failure> {
        // every path was validated: a root file, or a file of the one generation
        let files: Arc<Vec<(PathBuf, File)>> = {
            let (dir, m) = (dir.to_path_buf(), m.clone());
            Arc::new(
                blocking(move || {
                    if m.files.iter().any(|f| f.path.contains('/')) {
                        std::fs::create_dir(dir.join(&m.generation))?;
                    }
                    let mut out = Vec::with_capacity(m.files.len());
                    for f in &m.files {
                        let p = dir.join(&f.path);
                        let file = OpenOptions::new().write(true).create_new(true).open(&p)?;
                        out.push((p, file));
                    }
                    Ok(out)
                })
                .await?,
            )
        };
        let total = manifest::logical_size(m).max(1);
        let done = Arc::new(AtomicU64::new(0));
        let mut jobs = Vec::new();
        for (i, f) in m.files.iter().enumerate() {
            let mut off = 0;
            for b in &f.blobs {
                jobs.push((i, b.id.clone(), b.size, off, f.path.clone()));
                off += b.size;
            }
        }
        futures::stream::iter(jobs)
            .map(|(i, id, size, off, path)| {
                let (files, done) = (files.clone(), done.clone());
                async move {
                    let plain = self.fetch_blob(&id, size, &path, ctl, security).await?;
                    if !plain.is_empty() {
                        blocking(move || Ok(write_at(&files[i].1, &plain, off)?)).await?;
                    }
                    let d = done.fetch_add(size, Ordering::Relaxed) + size;
                    ctl.report(
                        0.02 + 0.85 * d as f32 / total as f32,
                        &format!("downloading {} / {} MB", d >> 20, total >> 20),
                    );
                    Ok::<(), Failure>(())
                }
            })
            .buffer_unordered(self.config.concurrency())
            .try_collect::<()>()
            .await?;

        let (m, dir) = (m.clone(), dir.to_path_buf());
        blocking(move || {
            let mut buf = vec![0u8; 1 << 20];
            for (i, (f, (p, file))) in m.files.iter().zip(files.iter()).enumerate() {
                file.sync_all()?;
                let mut h = Hasher::new();
                let mut r = File::open(p)?;
                loop {
                    let n = r.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                if h.finish() != f.sha256 {
                    return Err(BackupError::invalid_backup(
                        &format!("files[{i}].sha256"),
                        format!("{} does not hash to its recorded SHA-256", f.path),
                    )
                    .into());
                }
            }
            let current = std::fs::read(dir.join("CURRENT"))?;
            if String::from_utf8_lossy(&current).trim() != m.generation {
                return Err(BackupError::invalid_backup(
                    "CURRENT",
                    format!("does not name the generation {}", m.generation),
                )
                .into());
            }
            let cj = format!("{}/commit.json", m.generation);
            if let Ok(b) = std::fs::read(dir.join(&cj)) {
                let v: J =
                    serde_json::from_slice(&b).map_err(|e| BackupError::invalid_backup(&cj, e))?;
                let id = v["datasetId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok());
                if id != Some(m.dataset.id) {
                    return Err(BackupError::invalid_backup(
                        &cj,
                        format!("does not belong to dataset {}", m.dataset.id),
                    )
                    .into());
                }
                if v["baseSeq"].as_u64().is_none_or(|s| s > m.commit.seq) {
                    return Err(BackupError::invalid_backup(
                        &cj,
                        format!(
                            "its base commit is missing or after commit {}",
                            m.commit.seq
                        ),
                    )
                    .into());
                }
            }
            Ok(())
        })
        .await
    }

    /// Download blob `id` of `size` plaintext bytes (of the file `path`) and check it,
    /// fetching it again up to [`BLOB_RETRIES`] times when it does not verify.
    async fn fetch_blob(
        &self,
        id: &str,
        size: u64,
        path: &str,
        ctl: &Ctl,
        security: &crate::security::Security,
    ) -> std::result::Result<Vec<u8>, Failure> {
        // the largest stored form of `size` bytes this build writes (an LZ4 frame is
        // kept only when smaller than the plaintext); anything bigger is not read
        let cap = security.blob_limit(size)?;
        let mut attempt = 0;
        loop {
            ctl.check()?;
            self.download.take(size, ctl).await?;
            let got = match self
                .store
                .get_opts(&blob_key(id), GetOptions::default())
                .await
            {
                Ok(g) => g,
                Err(object_store::Error::NotFound { .. }) => {
                    return Err(Failure::Missing {
                        id: id.to_string(),
                        path: path.to_string(),
                    });
                }
                Err(e) => return Err(BackupError::from(e).into()),
            };
            let result = if got.meta.size > cap {
                Err(format!("stored size {} is too large", got.meta.size))
            } else {
                let mut stream = got.into_stream();
                let mut buf = Vec::with_capacity(size.min(cap) as usize);
                let mut since = 0;
                let mut too_big = false;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(BackupError::from)?;
                    if (buf.len() as u64).saturating_add(chunk.len() as u64) > cap {
                        too_big = true;
                        break;
                    }
                    buf.extend_from_slice(&chunk);
                    since += chunk.len();
                    if since >= CANCEL_EVERY {
                        since = 0;
                        ctl.check()?;
                    }
                }
                if too_big {
                    Err("stored size is too large".to_string())
                } else {
                    security.decode(&buf, id, size).map_err(|e| e.to_string())
                }
            };
            match result {
                Ok(plain) => return Ok(plain),
                Err(why) if attempt < BLOB_RETRIES => {
                    attempt += 1;
                    tracing::warn!(
                        target: "sparkles::backup",
                        blob = id,
                        path,
                        attempt,
                        "blob did not verify ({why}); fetching it again"
                    );
                }
                Err(_) => {
                    return Err(Failure::Corrupt {
                        id: id.to_string(),
                        path: path.to_string(),
                    });
                }
            }
        }
    }

    /// Verify backup `name` at the `restore` level: a full restore into a fresh
    /// `verify-<uuid>` directory under `o.tmp_dir` (default: the system temp
    /// directory), `check` in full mode, head and quad count compared, then the
    /// directory is removed. Takes a shared lock for `verify`.
    ///
    /// A missing blob, a corrupt blob (after the retries) and a failed check or
    /// comparison are reported in the result (`status: error`; the first bad blob in
    /// `missing` or `corrupt`, the check report or the error body in `check`), not as an
    /// `Err`. A repository that cannot be read, a lock that cannot be taken, a missing
    /// backup (`404 no-such-backup`) and cancellation are errors. Stops at the first bad
    /// blob, so `verify` runs its `data` pass first and restores only a clean backup.
    pub async fn verify_restore(&self, name: &str, o: &VerifyOptions) -> Result<BackupVerify> {
        let root = o.tmp_dir.clone().unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&root)?;
        let tmp = root.join(format!("verify-{}", Uuid::new_v4()));
        let ro = RestoreOptions {
            identity: Identity::Keep,
            check: crate::CheckLevel::Full,
            id_in_use: Arc::new(|_| false),
            in_place_head: None,
            store_opts: o.store_opts.clone(),
            ctl: o.ctl.clone(),
        };
        let r = self
            .restore_as(name, &tmp, &ro, LockOperation::Verify)
            .await;
        if r.is_ok() {
            let dir = tmp.clone();
            let _ = tokio::task::spawn_blocking(move || remove_dir(&dir)).await;
        }
        let mut v = BackupVerify {
            name: name.to_string(),
            status: VerifyStatus::Error,
            missing: Vec::new(),
            corrupt: Vec::new(),
            check: None,
        };
        match r {
            Ok(rep) => {
                v.status = VerifyStatus::Ok;
                v.check = rep.check;
            }
            Err(Failure::Missing { id, .. }) => v.missing.push(id),
            Err(Failure::Corrupt { id, .. }) => v.corrupt.push(id),
            Err(Failure::Check { report }) => v.check = Some(report),
            Err(Failure::Other(e))
                if matches!(
                    e.code(),
                    Code::InvalidBackup | Code::IncompatibleFormat | Code::RestoreMismatch
                ) =>
            {
                v.check = Some(e.body());
            }
            Err(Failure::Other(e)) => return Err(e),
        }
        Ok(v)
    }
}

/// Apply the identity rule: whether the restored directory keeps the backup's
/// dataset id (see [`Repository::restore`]).
fn identity_rule(m: &Manifest, o: &RestoreOptions) -> Result<bool> {
    let (id, seq) = (m.dataset.id, m.commit.seq);
    Ok(match o.identity {
        Identity::New => false,
        Identity::Auto => m.dataset.branch.is_none() && !(o.id_in_use)(id),
        Identity::Keep => {
            if m.dataset.branch.is_some() {
                return Err(BackupError::new(
                    Code::DuplicateDatasetId,
                    "a branch backup must be restored with a new dataset identity",
                ));
            }
            match o.in_place_head {
                Some(head) if head > seq => {
                    return Err(BackupError::new(
                        Code::DuplicateDatasetId,
                        format!(
                            "the dataset's head {head} is ahead of the backup's commit {seq}: \
                             keeping dataset id {id} would issue commits {} to {head} again",
                            seq + 1
                        ),
                    ));
                }
                Some(_) => {}
                None if (o.id_in_use)(id) => {
                    return Err(BackupError::new(
                        Code::DuplicateDatasetId,
                        format!("a dataset with id {id} exists; restore with a new identity"),
                    ));
                }
                None => {}
            }
            true
        }
    })
}

/// Remove a restore directory (a failed or verification restore), quietly.
fn remove_dir(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            target: "sparkles::backup",
            dir = %dir.display(),
            "could not remove a restore directory: {e}"
        ),
    }
}

/// `fsync` every file and directory under `dir`, and `dir` itself.
fn sync_tree(dir: &Path) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let t = e.file_type()?;
        if t.is_dir() {
            sync_tree(&e.path())?;
        } else if t.is_file() {
            File::open(e.path())?.sync_all()?;
        }
    }
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Write all of `buf` at `off` (positioned; concurrent writers to one file are fine).
fn write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(f, buf, off)
    }
    #[cfg(windows)]
    {
        let (mut buf, mut off) = (buf, off);
        while !buf.is_empty() {
            let n = std::os::windows::fs::FileExt::seek_write(f, buf, off)?;
            buf = &buf[n..];
            off += n as u64;
        }
        Ok(())
    }
}

/// `507 insufficient-storage` unless the file system of `dir` has room for a restore
/// of `logical` bytes: 1.1 × `logical`, plus `reserve` (the disk reserve the server or
/// the restored store keeps, `--min-free-disk-mb`) left free afterwards. `what` starts
/// the message ("restoring nightly-2026…").
pub fn check_free_space(dir: &Path, logical: u64, reserve: u64, what: &str) -> Result<()> {
    let need = logical.saturating_add(logical / 10);
    let free = sparkles_core::disk::free_bytes(dir)?;
    if free >= need.saturating_add(reserve) {
        return Ok(());
    }
    let h = sparkles_core::error::human_bytes;
    Err(BackupError::new(
        Code::InsufficientStorage,
        format!(
            "{what} needs {} of free disk space{}; {} has {}",
            h(need),
            if reserve > 0 {
                format!(" and {} kept free (--min-free-disk-mb)", h(reserve))
            } else {
                String::new()
            },
            dir.display(),
            h(free)
        ),
    ))
}

/// Replace the database directory `target` with the restored directory `restored` (a
/// sibling): hold `target`'s `sparkles.lock` (the "in use by another process" error if
/// someone holds it), rename `target` → `<target>.replaced-<pid>`, rename `restored` →
/// `target`, fsync the parent, then remove the replaced directory unless
/// `keep_replaced`. If the second rename fails, the first is undone. For the offline
/// `backup restore --to DIR --replace` (the server does its own swap).
///
/// A `target` that does not exist is simply `restored` renamed. The "in use" error is
/// `409 dataset-busy`; a leftover `<target>.replaced-<pid>` is refused
/// (`409 dataset-exists`).
pub fn swap_dir(target: &Path, restored: &Path, keep_replaced: bool) -> Result<()> {
    let parent = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if !restored.is_dir() {
        return Err(BackupError::new(
            Code::InvalidRequest,
            format!("{} is not a directory", restored.display()),
        ));
    }
    if !target.exists() {
        std::fs::rename(restored, target)?;
        sync_dir(&parent)?;
        return Ok(());
    }
    let lock = lock_database(target)?;
    let file_name = target
        .file_name()
        .ok_or_else(|| {
            BackupError::new(
                Code::InvalidRequest,
                format!("{} has no directory name", target.display()),
            )
        })?
        .to_string_lossy()
        .into_owned();
    let replaced = parent.join(format!("{file_name}.replaced-{}", std::process::id()));
    if replaced.exists() {
        return Err(BackupError::new(
            Code::DatasetExists,
            format!("{} exists; remove it first", replaced.display()),
        ));
    }
    std::fs::rename(target, &replaced)?;
    if let Err(e) = rename_second(restored, target) {
        // put the original back; the lock handle moved with it
        if let Err(back) = std::fs::rename(&replaced, target) {
            return Err(BackupError::internal(format!(
                "replacing {} failed ({e}), and so did moving the original back from {} ({back})",
                target.display(),
                replaced.display()
            )));
        }
        let _ = sync_dir(&parent);
        return Err(BackupError::internal(format!(
            "replacing {} failed: {e}",
            target.display()
        )));
    }
    sync_dir(&parent)?;
    drop(lock);
    if !keep_replaced && let Err(e) = std::fs::remove_dir_all(&replaced) {
        tracing::warn!(
            target: "sparkles::backup",
            dir = %replaced.display(),
            "could not remove the replaced database: {e}"
        );
    }
    Ok(())
}

/// The second rename of [`swap_dir`] (a failpoint in tests).
fn rename_second(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_SECOND_RENAME.with(|f| f.get()) {
        return Err(std::io::Error::other("failpoint: second rename"));
    }
    std::fs::rename(from, to)
}

/// Take the process lock of the database directory `root` (`sparkles.lock`, as
/// `Store::open` does), or fail with the "in use by another process" error.
fn lock_database(root: &Path) -> Result<File> {
    let path = root.join("sparkles.lock");
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => {
            let pid = std::fs::read_to_string(&path).unwrap_or_default();
            Err(BackupError::new(
                Code::DatasetBusy,
                format!(
                    "database {} is in use by another process (pid {}); stop it or talk to it over HTTP",
                    root.display(),
                    pid.trim()
                ),
            ))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests;
