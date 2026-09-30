//! Creating a backup: plan the blobs, upload what the repository lacks, write the
//! manifest last.
//!
//! **Immutable and meta files** are cut into `pieceBytes` pieces. Each piece is read
//! from the captured handle and hashed; it is then reused without a request when the
//! parent manifest (the newest backup of the same dataset id) references its id,
//! checked with a `HEAD` when it is over 1 MiB (dedup across datasets and after an
//! interrupted run), and otherwise created with `PutMode::Create`, where "already
//! exists" is success.
//!
//! **Append-only files** (`wal.log`, `delta.vocab`, `commits.bin`) reuse the parent's
//! segments when the parent has the same dataset id and generation, the local file is
//! at least as long, and the local bytes hash to the parent's segment ids; the bytes
//! past the parent's end become new segments. Blobs are content-addressed, so matching
//! hashes are all reuse needs. A mismatch (a catalog restarted by repair, a hand-edited
//! file) or more than [`MAX_SEGMENTS`] segments stores the file from scratch.
//!
//! Pieces upload in order with `maxConcurrency` in flight (memory: that many pieces),
//! under the upload throttle. Cancellation is checked between requests and every
//! 8 MiB read. The manifest is created last, so a failed or cancelled backup leaves
//! only unreferenced blobs, which the next attempt reuses (or GC removes).

use crate::blob::{self, Hasher};
use crate::error::{Code, Result};
use crate::layout::{self, MAX_SEGMENTS};
use crate::repo::{is_already_exists, is_not_found, read_only};
use crate::{
    BackupError, BackupSummary, BlobRef, CreateOptions, Ctl, Derived, FileEntry, LockKind,
    LockOperation, Manifest, ManifestCommit, ManifestDataset, ManifestStats, Repository,
    ServerInfo, Source, TextDerived, lock,
};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use object_store::{ObjectStoreExt, PutMode, PutOptions, PutPayload};
use sparkles::store::{CapturedFile, FileKind, FileSource};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::Instrument;

/// Pieces larger than this get a `HEAD` before they are sent.
const HEAD_ABOVE: u64 = 1 << 20;
/// Reads are split into chunks of this size, with a cancellation check between them.
const CHUNK: usize = 8 << 20;

/// How one file is stored.
struct FilePlan {
    /// the parent's segments this file starts with
    reused: Vec<BlobRef>,
    /// the whole-file hash when it was computed while checking the parent's segments
    /// (otherwise the uploaded pieces feed it)
    sha256: Option<String>,
    /// pieces to upload: (offset, length)
    pieces: Vec<(u64, u64)>,
}

/// One piece to upload.
#[derive(Clone, Copy)]
struct Unit {
    file: usize,
    off: u64,
    len: u64,
    /// feed the piece to the file's hasher
    feed: bool,
}

/// What happened to one piece.
struct Uploaded {
    unit: Unit,
    blob: BlobRef,
    /// stored bytes if this backup created the blob, `None` if it was there already
    created: Option<u64>,
    /// the plaintext, when the file hasher needs it
    plain: Option<Bytes>,
}

/// Counters of an upload (progress and the manifest's `stats`).
#[derive(Default)]
struct Tally {
    done_bytes: u64,
    new_blobs: u64,
    reused_blobs: u64,
    added_bytes: u64,
}

fn io_error(path: &str, e: std::io::Error) -> BackupError {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        BackupError::internal(format!(
            "{path}: the file is shorter than captured (it changed during the backup)"
        ))
    } else {
        BackupError::internal(format!("reading {path}: {e}"))
    }
}

/// Read `[off, off + len)` of `f` in [`CHUNK`]s, checking `cancel` between them.
fn read_piece(f: &CapturedFile, off: u64, len: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    let mut pos = off;
    for chunk in buf.chunks_mut(CHUNK) {
        if cancel.load(Ordering::Relaxed) {
            return Err(BackupError::cancelled());
        }
        f.read_exact_at(pos, chunk)
            .map_err(|e| io_error(&f.path, e))?;
        pos += chunk.len() as u64;
    }
    Ok(buf)
}

/// Hash `f` over `[0, len)` in one pass: the ids of the segments ending at `ends`
/// (ascending, the last at most `len`), and the whole-file hash.
fn hash_segments(
    f: &CapturedFile,
    ends: &[u64],
    cancel: &AtomicBool,
) -> Result<(Vec<String>, String)> {
    let mut whole = Hasher::new();
    let mut seg = Hasher::new();
    let mut ids = Vec::with_capacity(ends.len());
    let mut next = ends.iter().copied().peekable();
    let mut buf = vec![0u8; CHUNK.min(f.len.max(1) as usize)];
    let mut pos = 0u64;
    while pos < f.len {
        if cancel.load(Ordering::Relaxed) {
            return Err(BackupError::cancelled());
        }
        let n = (buf.len() as u64).min(f.len - pos) as usize;
        f.read_exact_at(pos, &mut buf[..n])
            .map_err(|e| io_error(&f.path, e))?;
        whole.update(&buf[..n]);
        let mut at = 0usize;
        while at < n {
            let chunk_end = pos + n as u64;
            match next.peek() {
                Some(&end) if end <= chunk_end => {
                    let take = (end - (pos + at as u64)) as usize;
                    seg.update(&buf[at..at + take]);
                    ids.push(std::mem::take(&mut seg).finish());
                    at += take;
                    next.next();
                }
                _ => {
                    seg.update(&buf[at..n]);
                    at = n;
                }
            }
        }
        pos += n as u64;
    }
    // segments ending at the current position (empty ones)
    while next.next_if(|&e| e <= pos).is_some() {
        ids.push(std::mem::take(&mut seg).finish());
    }
    Ok((ids, whole.finish()))
}

/// The `MB` of progress messages.
fn mb(b: u64) -> String {
    let m = b as f64 / 1e6;
    if m < 10.0 {
        format!("{m:.1}")
    } else {
        format!("{m:.0}")
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BackupError::internal(format!("a backup worker failed: {e}")))?
}

impl Repository {
    /// Back up `src` as `o.name`:
    /// 1. a shared lock; `409 backup-exists` if `backups/<name>.json` exists (an early
    ///    check; the final conditional create decides); `409 repository-read-only`;
    /// 2. plan: immutable files in `pieceBytes` pieces, reused when the parent manifest
    ///    (the newest backup of the same dataset id) has the id, else `HEAD` (pieces
    ///    over 1 MiB) and a `PutMode::Create` (`AlreadyExists` counts as success);
    ///    append-only files as the parent's segments plus new ones (from scratch when a
    ///    segment differs or there would be more than 64); meta files one blob each;
    ///    `o.extra` added as meta files;
    /// 3. upload with `maxConcurrency` requests in flight and the upload throttle,
    ///    checking `o.ctl` between requests and every 8 MiB; progress `0.05..0.95` by
    ///    bytes, message `uploading 120/310 MB · 14 new blobs · 3 reused`;
    /// 4. `PUT backups/<name>.json` with `PutMode::Create` (`409 backup-exists` when a
    ///    concurrent writer took the name); progress `0.97`;
    /// 5. cache the manifest, release the lock, drop `src` (its lease).
    ///
    /// A failure or cancellation leaves no manifest, only unreferenced blobs.
    pub async fn create(&self, src: Source, o: &CreateOptions) -> Result<BackupSummary> {
        o.ctl.check()?;
        if self.config.readonly {
            return Err(read_only(&self.config.name));
        }
        if !layout::valid_backup_name(&o.name) {
            return Err(BackupError::new(
                Code::InvalidName,
                format!(
                    "invalid backup name {:?}: [A-Za-z0-9][A-Za-z0-9._-]{{0,63}}",
                    o.name
                ),
            ));
        }
        let started = Instant::now();
        let created = crate::now_rfc3339();
        let guard = lock::acquire(self, LockKind::Shared, LockOperation::Create, &o.ctl).await?;
        let r = self.create_locked(src, o, started, created).await;
        if let Err(e) = guard.release().await {
            tracing::warn!(target: "sparkles::backup", "releasing a lock: {e}");
        }
        r
    }

    async fn create_locked(
        &self,
        src: Source,
        o: &CreateOptions,
        started: Instant,
        created: String,
    ) -> Result<BackupSummary> {
        let ctl = &o.ctl;
        let key = layout::manifest_key(&o.name);
        match self.store.head(&key).await {
            Ok(_) => return Err(exists(&o.name)),
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(e.into()),
        }
        ctl.report(0.05, "planning");
        // the lease is dropped once the manifest is written (or on failure)
        let Source {
            dataset_id,
            commit,
            generation,
            index_format,
            files,
            lease,
            ..
        } = src;
        let mut files = files;
        for (path, content) in &o.extra {
            if !layout::ROOT_FILES.contains(&path.as_str()) {
                return Err(BackupError::new(
                    Code::InvalidRequest,
                    format!("{path:?} cannot be added to a backup"),
                ));
            }
            files.retain(|f| &f.path != path);
            files.push(CapturedFile {
                path: path.clone(),
                kind: FileKind::Meta,
                len: content.len() as u64,
                src: FileSource::Bytes(Arc::from(content.as_slice())),
            });
        }
        let mut paths = HashSet::new();
        for f in &files {
            if !layout::valid_backup_path(&f.path) || !paths.insert(f.path.as_str()) {
                return Err(BackupError::internal(format!(
                    "captured file {:?} cannot be backed up",
                    f.path
                )));
            }
        }
        let files = Arc::new(files);
        let piece = self.marker.piece_bytes;

        // plan
        let span = tracing::info_span!("backup.plan", sparkles.repository = %self.config.name,
            sparkles.backup.name = %o.name, files = files.len(), reused = tracing::field::Empty);
        let (parent, plans) = async {
            let mut ms = self.manifests().await?;
            ms.retain(|m| m.dataset.id == dataset_id);
            crate::repo::sort_newest_first(&mut ms);
            let parent = ms.into_iter().next();
            let mut plans = Vec::with_capacity(files.len());
            for (i, f) in files.iter().enumerate() {
                let prior = parent
                    .as_ref()
                    .filter(|p| p.generation == generation)
                    .and_then(|p| {
                        p.files
                            .iter()
                            .find(|e| e.path == f.path && e.kind == FileKind::Append)
                    });
                let plan = match (f.kind, prior) {
                    (FileKind::Append, Some(prior)) if prior.size > 0 && prior.size <= f.len => {
                        self.plan_append(&files, i, prior, piece, ctl).await?
                    }
                    _ => FilePlan {
                        reused: Vec::new(),
                        sha256: None,
                        pieces: blob::pieces(f.len, piece).collect(),
                    },
                };
                plans.push(plan);
            }
            Ok::<_, BackupError>((parent, plans))
        }
        .instrument(span.clone())
        .await?;
        let reused_segments: usize = plans.iter().map(|p| p.reused.len()).sum();
        span.record("reused", reused_segments);

        // upload
        let parent_ids: Arc<HashSet<String>> = Arc::new(
            parent
                .iter()
                .flat_map(|p| {
                    p.files
                        .iter()
                        .flat_map(|f| f.blobs.iter().map(|b| b.id.clone()))
                })
                .collect(),
        );
        let units: Vec<Unit> = plans
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                let feed = p.sha256.is_none();
                p.pieces.iter().map(move |&(off, len)| Unit {
                    file: i,
                    off,
                    len,
                    feed,
                })
            })
            .collect();
        let total: u64 = files.iter().map(|f| f.len).sum();
        let mut tally = Tally {
            reused_blobs: reused_segments as u64,
            done_bytes: plans
                .iter()
                .map(|p| p.reused.iter().map(|b| b.size).sum::<u64>())
                .sum(),
            ..Default::default()
        };
        let mut hashers: Vec<Option<Hasher>> = plans
            .iter()
            .map(|p| p.sha256.is_none().then(Hasher::new))
            .collect();
        let mut blobs: Vec<Vec<BlobRef>> = plans.iter().map(|p| p.reused.clone()).collect();
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let span = tracing::info_span!("backup.upload", sparkles.repository = %self.config.name,
            sparkles.backup.name = %o.name, bytes = tracing::field::Empty,
            blobs = tracing::field::Empty);
        async {
            let mut results = futures::stream::iter(units)
                .map(|u| self.upload_unit(files.clone(), u, parent_ids.clone(), seen.clone(), ctl))
                .buffered(self.config.concurrency());
            while let Some(up) = results.try_next().await? {
                if let (Some(h), Some(p)) = (&mut hashers[up.unit.file], &up.plain) {
                    h.update(p);
                }
                blobs[up.unit.file].push(up.blob);
                tally.done_bytes += up.unit.len;
                match up.created {
                    Some(stored) => {
                        tally.new_blobs += 1;
                        tally.added_bytes += stored;
                    }
                    None => tally.reused_blobs += 1,
                }
                let frac = if total == 0 {
                    1.0
                } else {
                    tally.done_bytes as f64 / total as f64
                };
                ctl.report(
                    0.05 + 0.9 * frac as f32,
                    &format!(
                        "uploading {}/{} MB · {} new blobs · {} reused",
                        mb(tally.done_bytes),
                        mb(total),
                        tally.new_blobs,
                        tally.reused_blobs
                    ),
                );
            }
            Ok::<_, BackupError>(())
        }
        .instrument(span.clone())
        .await?;
        span.record("bytes", tally.added_bytes);
        span.record("blobs", tally.new_blobs);

        // manifest
        let entries: Vec<FileEntry> = files
            .iter()
            .zip(plans)
            .zip(hashers)
            .zip(blobs)
            .map(|(((f, plan), hasher), blobs)| FileEntry {
                path: f.path.clone(),
                kind: f.kind,
                size: f.len,
                sha256: plan
                    .sha256
                    .or_else(|| hasher.map(Hasher::finish))
                    .unwrap_or_default(),
                blobs,
            })
            .collect();
        let has_text = entries.iter().any(|e| e.path == "text.json");
        let manifest = Manifest {
            format: layout::FORMAT,
            kind: layout::MANIFEST_KIND.to_string(),
            name: o.name.clone(),
            id: uuid::Uuid::new_v4(),
            repository_id: self.marker.id,
            dataset: ManifestDataset {
                name: o.dataset_name.clone(),
                id: dataset_id,
                kind: "persistent".to_string(),
            },
            commit: ManifestCommit::from(&commit),
            generation,
            index_format,
            created,
            completed: crate::now_rfc3339(),
            millis: started.elapsed().as_millis() as u64,
            server: ServerInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            parent: parent.as_ref().map(|p| p.name.clone()),
            policy: o.policy.as_ref().map(|p| p.0.clone()),
            run: o.policy.as_ref().map(|p| p.1.clone()),
            note: o.note.clone(),
            stats: ManifestStats {
                logical_bytes: total,
                added_bytes: tally.added_bytes,
                files: entries.len() as u64,
                blobs: entries.iter().map(|e| e.blobs.len() as u64).sum(),
                new_blobs: tally.new_blobs,
                reused_blobs: tally.reused_blobs,
            },
            files: entries,
            derived: Derived {
                text: has_text.then_some(TextDerived {
                    rebuild_on_restore: true,
                }),
            },
            encryption: None,
        };
        ctl.check()?;
        ctl.report(0.97, "writing the manifest");
        let body = Bytes::from(
            serde_json::to_vec(&manifest)
                .map_err(|e| BackupError::internal(format!("manifest: {e}")))?,
        );
        let size = body.len() as u64;
        let span = tracing::info_span!("backup.manifest", sparkles.repository = %self.config.name,
            sparkles.backup.name = %o.name);
        let e_tag = async {
            match self.create_object(&key, PutPayload::from(body)).await? {
                Some(put) => Ok(put.e_tag),
                None => {
                    // ours after a retried request, or a concurrent writer's
                    match self.manifest(&o.name).await {
                        Ok(m) if m.id == manifest.id => Ok(None),
                        _ => Err(exists(&o.name)),
                    }
                }
            }
        }
        .instrument(span)
        .await?;
        if let Some(tag) = &e_tag {
            self.cache.put(&o.name, tag, size, &manifest);
        }
        drop(lease);
        tracing::info!(
            target: "sparkles::backup",
            repository = %self.config.name,
            backup = %o.name,
            dataset = %manifest.dataset.name,
            seq = manifest.commit.seq,
            logical_bytes = manifest.stats.logical_bytes,
            added_bytes = manifest.stats.added_bytes,
            new_blobs = manifest.stats.new_blobs,
            reused_blobs = manifest.stats.reused_blobs,
            millis = manifest.millis,
            "backup created"
        );
        Ok(manifest.summary(&self.config.name))
    }

    /// The plan of append-only file `i` whose parent entry is `prior` (with
    /// `0 < prior.size <= len`): the parent's segments plus new ones if every segment
    /// matches and there are at most [`MAX_SEGMENTS`] in all, else from scratch.
    async fn plan_append(
        &self,
        files: &Arc<Vec<CapturedFile>>,
        i: usize,
        prior: &FileEntry,
        piece: u64,
        ctl: &Ctl,
    ) -> Result<FilePlan> {
        let f = &files[i];
        let scratch = || FilePlan {
            reused: Vec::new(),
            sha256: None,
            pieces: blob::pieces(f.len, piece).collect(),
        };
        let x = prior.size;
        let new: Vec<(u64, u64)> = if f.len > x {
            blob::pieces(f.len - x, piece)
                .map(|(off, len)| (x + off, len))
                .collect()
        } else {
            Vec::new()
        };
        if prior.blobs.len() + new.len() > MAX_SEGMENTS {
            return Ok(scratch());
        }
        let ends: Vec<u64> = prior
            .blobs
            .iter()
            .scan(0u64, |end, b| {
                *end += b.size;
                Some(*end)
            })
            .collect();
        if ends.last() != Some(&x) {
            // a parent entry whose blobs do not add up: do not trust it
            return Ok(scratch());
        }
        let (files2, cancel) = (files.clone(), ctl.cancel.clone());
        let (ids, whole) = blocking(move || hash_segments(&files2[i], &ends, &cancel)).await?;
        if ids.iter().zip(&prior.blobs).any(|(id, b)| id != &b.id) {
            tracing::warn!(
                target: "sparkles::backup",
                repository = %self.config.name,
                file = %f.path,
                "the file differs from the parent backup's copy: stored from scratch"
            );
            return Ok(FilePlan {
                reused: Vec::new(),
                sha256: Some(whole),
                pieces: blob::pieces(f.len, piece).collect(),
            });
        }
        Ok(FilePlan {
            reused: prior.blobs.clone(),
            sha256: Some(whole),
            pieces: new,
        })
    }

    /// Read, hash and (unless it is known to exist) upload one piece.
    async fn upload_unit(
        &self,
        files: Arc<Vec<CapturedFile>>,
        unit: Unit,
        parent_ids: Arc<HashSet<String>>,
        seen: Arc<Mutex<HashSet<String>>>,
        ctl: &Ctl,
    ) -> Result<Uploaded> {
        ctl.check()?;
        let cancel = ctl.cancel.clone();
        let (plain, id) = blocking(move || {
            let plain = read_piece(&files[unit.file], unit.off, unit.len, &cancel)?;
            let id = blob::blob_id(&plain);
            Ok((Bytes::from(plain), id))
        })
        .await?;
        let blob = BlobRef {
            id: id.clone(),
            size: unit.len,
        };
        let done = |created: Option<u64>, plain: Bytes| Uploaded {
            unit,
            blob: blob.clone(),
            created,
            plain: unit.feed.then_some(plain),
        };
        // known: referenced by the parent, or already handled by this backup
        if parent_ids.contains(&id) || !seen.lock().unwrap().insert(id.clone()) {
            return Ok(done(None, plain));
        }
        let key = layout::blob_key(&id);
        let single = self.single_writer();
        if unit.len > HEAD_ABOVE || single {
            ctl.check()?;
            match self.store.head(&key).await {
                Ok(_) => return Ok(done(None, plain)),
                Err(e) if is_not_found(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let p2 = plain.clone();
        let encoded = blocking(move || Ok(blob::encode_with_id(&p2, id, true))).await?;
        let stored = encoded.bytes.len() as u64;
        self.upload.take(stored, ctl).await?;
        ctl.check()?;
        let payload = PutPayload::from(encoded.bytes);
        if single {
            self.store.put(&key, payload).await?;
            return Ok(done(Some(stored), plain));
        }
        match self
            .store
            .put_opts(&key, payload.clone(), PutOptions::from(PutMode::Create))
            .await
        {
            Ok(_) => Ok(done(Some(stored), plain)),
            Err(e) if is_already_exists(&e) => Ok(done(None, plain)),
            Err(object_store::Error::NotImplemented { .. }) => {
                self.no_conditional_writes();
                self.store.put(&key, payload).await?;
                Ok(done(Some(stored), plain))
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn exists(name: &str) -> BackupError {
    BackupError::new(Code::BackupExists, format!("backup {name} already exists"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(bytes: &[u8]) -> CapturedFile {
        CapturedFile {
            path: "gen-0001/wal.log".into(),
            kind: FileKind::Append,
            len: bytes.len() as u64,
            src: FileSource::Bytes(Arc::from(bytes)),
        }
    }

    #[test]
    fn segment_hashes_follow_the_boundaries() {
        let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let f = file(&data);
        let no = AtomicBool::new(false);
        let (ids, whole) = hash_segments(&f, &[10, 300, 1000], &no).unwrap();
        assert_eq!(
            ids,
            [
                blob::blob_id(&data[..10]),
                blob::blob_id(&data[10..300]),
                blob::blob_id(&data[300..])
            ]
        );
        assert_eq!(whole, blob::blob_id(&data));
        // boundaries short of the end
        let (ids, _) = hash_segments(&f, &[500], &no).unwrap();
        assert_eq!(ids, [blob::blob_id(&data[..500])]);
        let (ids, whole) = hash_segments(&file(b""), &[0], &no).unwrap();
        assert_eq!(ids, [blob::blob_id(b"")]);
        assert_eq!(whole, blob::blob_id(b""));
        assert!(
            hash_segments(&f, &[10], &AtomicBool::new(true))
                .unwrap_err()
                .is_cancelled()
        );
    }

    #[test]
    fn pieces_are_read_whole() {
        let data: Vec<u8> = (0..100u8).collect();
        let f = file(&data);
        let no = AtomicBool::new(false);
        assert_eq!(read_piece(&f, 10, 20, &no).unwrap(), &data[10..30]);
        assert!(read_piece(&f, 90, 20, &no).is_err());
        assert_eq!(mb(120_400_000), "120");
        assert_eq!(mb(1_234_567), "1.2");
    }
}
