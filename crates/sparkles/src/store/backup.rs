//! Consistent captures of a live database for backup repositories (`sparkles-backup`).
//!
//! A capture pins one commit `s` of a persistent store without blocking writers for
//! more than a few system calls: under the writer mutex it records the length of every
//! append-only file (the WAL up to the end of `s`'s record, the flushed delta
//! vocabulary, the commit catalog through `s`), clones the prefixes, and leases the
//! generation so that history garbage collection keeps its directory until the lease is
//! dropped. After the lock it opens a read handle for every file of the generation and
//! reads through them (`pread`), so a compaction or bulk commit during the upload does
//! not affect the backup.

use super::Store;
use crate::commit::CommitInfo;
use crate::error::{Error, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How a captured file is stored in a backup repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// never changes once written (`gen-NNNN/*.dat`, `vocab.dat`, `meta.json`, …):
    /// stored whole, split into fixed-size pieces
    Immutable,
    /// only grows (`gen-NNNN/wal.log`, `gen-NNNN/delta.vocab`, `commits.bin`): stored as
    /// ordered segments, so a later backup uploads only the appended bytes
    Append,
    /// small files that may be rewritten (`CURRENT`, `dataset.json`, `prefixes.json`,
    /// `text.json`, `origin.json`, `reasoning.json`, `validation*`): one blob each
    Meta,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Immutable => "immutable",
            FileKind::Append => "append",
            FileKind::Meta => "meta",
        }
    }
}

/// Where a captured file's bytes come from.
#[derive(Debug)]
pub enum FileSource {
    /// a read handle opened while the writer lock was held; read with positioned reads
    /// ([`CapturedFile::read_at`]) up to [`CapturedFile::len`] only
    File(std::fs::File),
    /// content rendered at capture time (`prefixes.json`) or read whole after it
    Bytes(Arc<[u8]>),
}

/// One file of a capture.
#[derive(Debug)]
pub struct CapturedFile {
    /// path relative to the database root, with `/` separators (`gen-0001/spo.dat`,
    /// `commits.bin`, `CURRENT`)
    pub path: String,
    pub kind: FileKind,
    /// bytes that belong to the backup: the whole file for immutable and meta files, the
    /// captured prefix for append-only ones (the file may have grown since)
    pub len: u64,
    pub src: FileSource,
}

impl CapturedFile {
    /// Read up to `buf.len()` bytes at `offset`, never past [`len`](Self::len). Returns
    /// the number of bytes read (0 at `len`). Positioned: concurrent calls on one file
    /// are fine.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let n = buf.len().min((self.len - offset) as usize);
        match &self.src {
            FileSource::Bytes(b) => {
                let start = offset as usize;
                let avail = b.len().saturating_sub(start).min(n);
                buf[..avail].copy_from_slice(&b[start..start + avail]);
                Ok(avail)
            }
            FileSource::File(f) => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::FileExt::read_at(f, &mut buf[..n], offset)
                }
                #[cfg(windows)]
                {
                    std::os::windows::fs::FileExt::seek_read(f, &mut buf[..n], offset)
                }
                #[cfg(not(any(unix, windows)))]
                {
                    let _ = f;
                    Err(std::io::Error::other("positioned reads are not supported"))
                }
            }
        }
    }

    /// Fill `buf` completely from `offset`; a short file is an `UnexpectedEof` error
    /// (a generation file that shrank during the backup: `Error::Corrupt` upstream).
    pub fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(offset, buf) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("{}: file shorter than captured", self.path),
                    ));
                }
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Keeps a captured generation leased (history GC keeps its directory) until dropped.
/// Dropping it removes the lease and runs a best-effort history collection
/// ([`Store::try_collect_history`]): if the writer is busy, the next collection point
/// removes the generation instead. A guard made with [`LeaseGuard::none`] holds nothing.
pub struct LeaseGuard {
    /// the leased generation number (`gen-NNNN`), 0 for [`none`](Self::none)
    pub(super) generation: u32,
    /// the lease's label, shown in `HistoryStatus` as `backup:<label>`
    pub(super) label: String,
    /// removes the lease (and collects); run once, on drop
    pub(super) release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl LeaseGuard {
    /// A guard that holds no lease (a capture of a closed database directory).
    pub fn none() -> LeaseGuard {
        LeaseGuard {
            generation: 0,
            label: String::new(),
            release: None,
        }
    }

    /// The leased generation number (0 when nothing is leased).
    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("generation", &self.generation)
            .field("label", &self.label)
            .field("held", &self.release.is_some())
            .finish()
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// A consistent capture of a persistent store at one commit ([`Store::backup_capture`]).
#[derive(Debug)]
pub struct BackupCapture {
    pub dataset_id: uuid::Uuid,
    /// the captured commit `s` (the head when the writer lock was held)
    pub commit: CommitInfo,
    /// the generation directory holding `s` (`gen-NNNN`)
    pub generation: String,
    /// the index format of that generation (`builder::FORMAT_VERSION` when it was built)
    pub index_format: u32,
    /// every file of the backup, in a stable order: the generation's files, then
    /// `commits.bin`, then the meta files (`CURRENT`, `dataset.json`, `prefixes.json`,
    /// and when present `text.json`, `origin.json`, `validation.json`,
    /// `validation-shapes.ttl`, `validation-schema.shex`, `validation-schema.json`).
    /// `reasoning.json` is the caller's to add (the server
    /// holds the current status and applies the "not after `s`" rule).
    pub files: Vec<CapturedFile>,
    /// how long the writer lock was held (metric `sparkles_backup_capture_lock_seconds`)
    pub lock_hold: Duration,
    /// keeps the generation until the upload ends
    pub lease: LeaseGuard,
}

impl BackupCapture {
    /// The captured file at `path` (`gen-0001/wal.log`, `CURRENT`, …).
    pub fn file(&self, path: &str) -> Option<&CapturedFile> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Write every captured file (up to its captured length) into the directory `dir`,
    /// which must not exist: the result is the database as it was right after
    /// `commit`, which opens with head `commit.seq`. For tests and local copies; files
    /// are synced, and `dir` itself is created last by rename.
    pub fn write_to(&self, dir: &Path) -> Result<()> {
        if dir.exists() {
            return Err(Error::Invalid(format!("{} exists", dir.display())));
        }
        let parent = dir.parent().unwrap_or(Path::new("."));
        let tmp = tempfile::Builder::new()
            .prefix(".capture-")
            .tempdir_in(parent)?;
        let mut buf = vec![0u8; 1 << 20];
        for f in &self.files {
            let path = tmp.path().join(&f.path);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let mut off = 0;
            while off < f.len {
                let n = buf.len().min((f.len - off) as usize);
                f.read_exact_at(off, &mut buf[..n])?;
                out.write_all(&buf[..n])?;
                off += n as u64;
            }
            out.sync_all()?;
        }
        super::sync_dir(&tmp.path().join(&self.generation))?;
        super::sync_dir(tmp.path())?;
        std::fs::rename(tmp.keep(), dir)?;
        super::sync_dir(parent)?;
        Ok(())
    }
}

/// A test hook run at a named point of the store's code (`Store::set_failpoint`).
#[cfg(any(test, feature = "failpoints"))]
pub type Failpoint = Arc<dyn Fn(&Store) + Send + Sync>;

/// Files of a generation directory that grow (read up to their captured length).
const APPEND_FILES: [&str; 2] = ["wal.log", "delta.vocab"];

/// Meta files at the root, besides `CURRENT`, `dataset.json` and `prefixes.json`, that a
/// backup holds when present.
const OPTIONAL_META: [&str; 8] = [
    "text.json",
    "geo.json",
    "vector.json",
    "origin.json",
    crate::guard::config::CONFIG_FILE,
    crate::guard::config::SHACL_SHAPES_FILE,
    crate::guard::config::SHEX_SCHEMA_SHEXC_FILE,
    crate::guard::config::SHEX_SCHEMA_SHEXJ_FILE,
];

impl Store {
    /// Capture the current commit for a backup labelled `label` (the backup name, shown
    /// as `backup:<label>` in the history status while the lease is held).
    ///
    /// Contract:
    /// * persistent stores only; in-memory stores fail with `Error::Unsupported`;
    /// * the writer mutex is held only to read lengths, flush the delta vocabulary and
    ///   the commit catalog, clone the prefixes and add the lease (a few system calls;
    ///   the files are opened after it); a poisoned store fails with `Error::Poisoned`;
    /// * a commit catalog that cannot be flushed completely fails with
    ///   `Error::Conflict("catalog-lagging: …")` (retryable, `503 catalog-lagging`);
    /// * `CURRENT` read after the lock must name `generation`, else the capture is
    ///   retried once (a compaction raced it), then fails with `Error::Conflict`;
    /// * the WAL prefix ends exactly at the end of `commit`'s record and the
    ///   `commits.bin` prefix at its catalog record, so a restored store's head is
    ///   `commit.seq`.
    pub fn backup_capture(&self, label: &str) -> Result<BackupCapture> {
        if self.root.is_none() || self.history.is_none() {
            return Err(Error::unsupported(
                "backups of in-memory datasets are not supported",
            ));
        }
        if let Some(c) = self.capture_once(label)? {
            return Ok(c);
        }
        // a compaction or bulk commit switched CURRENT right after the lock: once more
        match self.capture_once(label)? {
            Some(c) => Ok(c),
            None => Err(Error::Conflict(
                "the database's generation changed twice during the backup capture; retry".into(),
            )),
        }
    }

    /// [`backup_capture`](Self::backup_capture) of a store opened only for the backup:
    /// the store stays open (its directory locked, so no other process writes it) until
    /// the capture's lease is dropped.
    pub fn into_backup_capture(self, label: &str) -> Result<BackupCapture> {
        let mut c = self.backup_capture(label)?;
        let release = c.lease.release.take();
        c.lease.release = Some(Box::new(move || {
            if let Some(r) = release {
                r();
            }
            drop(self);
        }));
        Ok(c)
    }

    /// One capture attempt: `None` if `CURRENT` no longer names the captured generation
    /// once the lock is released.
    fn capture_once(&self, label: &str) -> Result<Option<BackupCapture>> {
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::unsupported(
                "backups of in-memory datasets are not supported",
            ));
        };
        let w = self.writer.lock();
        let t0 = Instant::now();
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        // Under the lock only lengths, the prefixes and the lease: no write transaction is
        // open, and every commit flushed and synced its WAL records, so the WAL ends at the
        // head's commit record. The lease keeps the generation directory until dropped, so
        // its files can be opened after the lock (commits.bin is only ever appended to
        // while the store is open).
        let snap = self.snapshot();
        let head = w.head;
        let generation = snap.generation.name.clone();
        let gen_no = crate::commit::generation_number(&generation);
        let wal_len = match &w.wal {
            Some(wal) => wal.get_ref().metadata()?.len(),
            None => 0,
        };
        let dvocab_len = snap.generation.dvocab.flush()?;
        let catalog_len = self.catalog.lock().flushed_len()?;
        let prefixes = self.prefixes.lock().clone();
        self.failpoint("backup-capture-locked");
        let lease_id = hist.lock().lease(gen_no, label);
        drop(w);
        let lock_hold = t0.elapsed();
        // from here on, dropping the guard releases the lease
        let collector = self.collector();
        let lease = LeaseGuard {
            generation: gen_no,
            label: label.to_string(),
            release: Some(Box::new(move || {
                if let Some(c) = collector {
                    c.release(lease_id);
                }
            })),
        };
        let gdir = root.join(&generation);
        let mut gen_files: Vec<(String, File, u64)> = Vec::new();
        for e in std::fs::read_dir(&gdir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !e.file_type()?.is_file() || name.ends_with(".tmp") {
                continue;
            }
            let f = File::open(e.path())?;
            let len = match name.as_str() {
                "wal.log" => wal_len,
                "delta.vocab" => dvocab_len,
                // immutable: its whole length
                _ => f.metadata()?.len(),
            };
            gen_files.push((name, f, len));
        }
        let catalog = File::open(root.join("commits.bin"))?;
        self.failpoint("backup-capture-unlocked");

        let current = std::fs::read(root.join("CURRENT"))?;
        if String::from_utf8_lossy(&current).trim() != generation {
            return Ok(None);
        }
        let mut files = Vec::with_capacity(gen_files.len() + 8);
        gen_files.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, f, len) in gen_files {
            let kind = if APPEND_FILES.contains(&name.as_str()) {
                FileKind::Append
            } else {
                FileKind::Immutable
            };
            files.push(CapturedFile {
                path: format!("{generation}/{name}"),
                kind,
                len,
                src: FileSource::File(f),
            });
        }
        let wal = files.iter().find(|f| f.path.ends_with("/wal.log"));
        check_wal_prefix(wal, &gdir, &head)?;
        let catalog = CapturedFile {
            path: "commits.bin".into(),
            kind: FileKind::Append,
            len: catalog_len,
            src: FileSource::File(catalog),
        };
        check_catalog_prefix(&catalog, &head)?;
        files.push(catalog);
        let meta = |path: &str, bytes: Vec<u8>| CapturedFile {
            path: path.to_string(),
            kind: FileKind::Meta,
            len: bytes.len() as u64,
            src: FileSource::Bytes(bytes.into()),
        };
        files.push(meta("CURRENT", current));
        let dataset = std::fs::read(root.join("dataset.json"))?;
        match crate::commit::dataset_id_of(&dataset)? {
            id if id == self.dataset_id => {}
            id => {
                return Err(Error::Corrupt(format!(
                    "dataset.json names dataset {id}, not {}",
                    self.dataset_id
                )));
            }
        }
        files.push(meta("dataset.json", dataset));
        files.push(meta(
            "prefixes.json",
            serde_json::to_vec_pretty(&prefixes).unwrap(),
        ));
        for name in OPTIONAL_META {
            match std::fs::read(root.join(name)) {
                Ok(b) => files.push(meta(name, b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(BackupCapture {
            dataset_id: self.dataset_id,
            commit: head,
            generation,
            index_format: snap.generation.meta.format_version,
            files,
            lock_hold,
            lease,
        }))
    }

    /// Best-effort history collection: removes generations nothing needs any more if the
    /// writer mutex is free right now (`try_lock`), and otherwise does nothing (the next
    /// collection point — a compaction, a bulk commit, a snapshot or retention change,
    /// or the server's hourly tick — does it). Called when a backup lease is dropped.
    pub fn try_collect_history(&self) {
        if let Some(c) = self.collector() {
            c.try_collect();
        }
    }

    /// Install (`Some`) or remove (`None`) the test hook run at failpoint `name`:
    /// `backup-capture-locked` (writer lock held: the hook must not write) or
    /// `backup-capture-unlocked` (right after the lock is released, before `CURRENT`
    /// is read).
    #[cfg(any(test, feature = "failpoints"))]
    #[doc(hidden)]
    pub fn set_failpoint(&self, name: &'static str, hook: Option<Failpoint>) {
        let mut f = self.failpoints.lock();
        match hook {
            Some(h) => {
                f.insert(name, h);
            }
            None => {
                f.remove(name);
            }
        }
    }

    /// Make appends to the commit catalog fail (`on`) or work again, as after a write
    /// error: the catalog lags the commits until an append succeeds.
    #[cfg(any(test, feature = "failpoints"))]
    #[doc(hidden)]
    pub fn fail_catalog_writes(&self, on: bool) {
        self.catalog.lock().fail_writes = on;
    }

    #[inline]
    fn failpoint(&self, name: &'static str) {
        #[cfg(any(test, feature = "failpoints"))]
        {
            let hook = self.failpoints.lock().get(name).cloned();
            if let Some(h) = hook {
                h(self);
            }
        }
        #[cfg(not(any(test, feature = "failpoints")))]
        let _ = name;
    }
}

/// The WAL prefix must end with the head's commit record, or be empty when the head is
/// the generation's base commit.
fn check_wal_prefix(wal: Option<&CapturedFile>, gdir: &Path, head: &CommitInfo) -> Result<()> {
    let len = wal.map_or(0, |f| f.len);
    let bad = |m: String| Err(Error::Corrupt(format!("{}: {m}", gdir.display())));
    if len == 0 {
        return match crate::commit::read_gen_commit(gdir)? {
            Some((_, base, _)) if base.seq == head.seq => Ok(()),
            _ => bad(format!(
                "empty WAL, but the head {} is not its base",
                head.seq
            )),
        };
    }
    let rec_len = super::WAL_REC as u64;
    if !len.is_multiple_of(rec_len) {
        return bad(format!("WAL length {len} is not a whole number of records"));
    }
    let mut rec = [0u8; super::WAL_REC];
    wal.unwrap().read_exact_at(len - rec_len, &mut rec)?;
    if rec[0] != super::WAL_COMMIT {
        return bad("the WAL does not end with a commit record".into());
    }
    // version-2 records carry their seq (legacy ones do not)
    if rec[26] == crate::commit::WAL_COMMIT_V2 {
        let seq = u64::from_le_bytes(rec[9..17].try_into().unwrap());
        if seq != head.seq {
            return bad(format!(
                "the WAL ends at commit {seq}, not the head {}",
                head.seq
            ));
        }
    }
    Ok(())
}

/// The captured catalog must end with the head's record.
fn check_catalog_prefix(f: &CapturedFile, head: &CommitInfo) -> Result<()> {
    let rec = crate::commit::REC as u64;
    let bad = |m: String| Err(Error::Corrupt(format!("commits.bin: {m}")));
    if f.len < 2 * rec || !f.len.is_multiple_of(rec) {
        return bad(format!(
            "length {} is not a header and whole records",
            f.len
        ));
    }
    let mut r = [0u8; crate::commit::REC];
    f.read_exact_at(f.len - rec, &mut r)?;
    match crate::commit::decode_record(&r) {
        Some(c) if c.seq == head.seq => Ok(()),
        Some(c) => bad(format!(
            "ends at commit {}, not the head {}",
            c.seq, head.seq
        )),
        None => bad("the last record is damaged".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{At, Hold};
    use crate::sparql::QueryOptions;
    use crate::sparql::update::update;
    use crate::store::StoreOptions;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn upd(s: &Store, u: &str) {
        update(s, u, &QueryOptions::default()).unwrap();
    }

    fn ins(s: &Store, i: u64) {
        upd(s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> \"v{i}\" }}"));
    }

    fn lines(b: Vec<u8>) -> Vec<String> {
        let mut v: Vec<String> = String::from_utf8(b)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        v.sort();
        v
    }

    fn dump_at(s: &Store, seq: u64) -> Vec<String> {
        let mut out = Vec::new();
        s.dump_nquads_at(&At::Commit(seq), &mut out).unwrap();
        lines(out)
    }

    fn dump(s: &Store) -> Vec<String> {
        let mut out = Vec::new();
        s.dump_nquads(&mut out).unwrap();
        lines(out)
    }

    fn gens(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("gen-"))
            .collect();
        v.sort();
        v
    }

    fn leases(s: &Store) -> usize {
        s.history.as_ref().unwrap().lock().leases.len()
    }

    /// Materialize `c` into `dir`, check it, open it, and compare it with the source's
    /// state at the captured commit.
    fn restore_and_compare(src: &Store, c: &BackupCapture, dir: &Path) {
        c.write_to(dir).unwrap();
        let opts = crate::check::CheckOptions { quick: false };
        let report = crate::check::check(dir, &opts).unwrap();
        assert_eq!(
            report.status,
            crate::check::Status::Ok,
            "{}",
            serde_json::to_string_pretty(&report).unwrap()
        );
        let r = Store::open(dir, StoreOptions::default()).unwrap();
        assert_eq!(r.head_commit(), c.commit);
        assert_eq!(r.snapshot().len(), c.commit.quads);
        assert_eq!(r.dataset_id(), c.dataset_id);
        assert_eq!(dump(&r), dump_at(src, c.commit.seq));
    }

    #[test]
    fn capture_lists_files_and_pins_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        for i in 0..3 {
            ins(&s, i);
        }
        s.set_prefix("ex", "http://example.org/").unwrap();
        std::fs::write(root.join("validation-shapes.ttl"), b"# shapes\n").unwrap();
        std::fs::write(root.join("validation-schema.shex"), b"# schema\n").unwrap();
        std::fs::write(root.join("validation-schema.json"), b"{}").unwrap();
        std::fs::write(root.join("origin.json"), b"{}").unwrap();
        let c = s.backup_capture("b1").unwrap();
        assert_eq!(c.commit.seq, 3);
        assert_eq!(c.generation, "gen-0001");
        assert_eq!(c.index_format, crate::builder::FORMAT_VERSION);
        assert_eq!(c.lease.generation(), 1);
        assert_eq!(c.lease.label(), "b1");
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        let n = paths.iter().filter(|p| p.starts_with("gen-0001/")).count();
        assert!(n >= 20, "{paths:?}");
        assert!(paths[..n].is_sorted());
        for f in [
            "wal.log",
            "delta.vocab",
            "commit.json",
            "meta.json",
            "vocab.dat",
        ] {
            assert!(paths.contains(&format!("gen-0001/{f}").as_str()), "{f}");
        }
        assert_eq!(
            paths[n..],
            [
                "commits.bin",
                "CURRENT",
                "dataset.json",
                "prefixes.json",
                "origin.json",
                "validation-shapes.ttl",
                "validation-schema.shex",
                "validation-schema.json"
            ]
        );
        for f in &c.files {
            let want = match f.path.as_str() {
                "gen-0001/wal.log" | "gen-0001/delta.vocab" | "commits.bin" => FileKind::Append,
                p if p.starts_with("gen-") => FileKind::Immutable,
                _ => FileKind::Meta,
            };
            assert_eq!(f.kind, want, "{}", f.path);
        }
        // a header and the records 0..=3
        assert_eq!(c.file("commits.bin").unwrap().len, 5 * 64);
        let wal = c.file("gen-0001/wal.log").unwrap();
        let on_disk = std::fs::metadata(root.join("gen-0001/wal.log"))
            .unwrap()
            .len();
        assert_eq!(wal.len, on_disk);
        assert_eq!(wal.len % super::super::WAL_REC as u64, 0);
        let p = c.file("prefixes.json").unwrap();
        let mut b = vec![0u8; p.len as usize];
        p.read_exact_at(0, &mut b).unwrap();
        let p: BTreeMap<String, String> = serde_json::from_slice(&b).unwrap();
        assert_eq!(p["ex"], "http://example.org/");

        // later writes and prefix changes do not reach the capture
        let lens: Vec<u64> = c.files.iter().map(|f| f.len).collect();
        ins(&s, 10);
        s.set_prefix("ex2", "http://example.org/2/").unwrap();
        assert_eq!(c.files.iter().map(|f| f.len).collect::<Vec<_>>(), lens);
        let out = dir.path().join("restored");
        restore_and_compare(&s, &c, &out);
        assert_eq!(dump_at(&s, 3).len(), 3);
        assert!(out.join("validation-shapes.ttl").exists());
        assert_eq!(
            std::fs::read(out.join("validation-schema.shex")).unwrap(),
            b"# schema\n"
        );
        assert!(out.join("validation-schema.json").exists());
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.prefixes(), p);
    }

    #[test]
    fn in_memory_stores_are_unsupported() {
        let s = Store::in_memory(StoreOptions::default());
        assert!(matches!(s.backup_capture("b"), Err(Error::Unsupported(_))));
    }

    #[test]
    fn restored_capture_continues_the_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        ins(&s, 1);
        ins(&s, 2);
        let c = s.backup_capture("b").unwrap();
        ins(&s, 3);
        let out = dir.path().join("r");
        c.write_to(&out).unwrap();
        assert!(c.write_to(&out).is_err());
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.head_commit().seq, 2);
        ins(&r, 7);
        assert_eq!(r.head_commit().seq, 3);
        let page = r.commits(crate::commit::CommitRange::Latest, 10);
        assert_eq!(page.commits.len(), 4);
    }

    #[test]
    fn lock_hold_is_measured() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        s.set_failpoint(
            "backup-capture-locked",
            Some(Arc::new(|_: &Store| {
                std::thread::sleep(Duration::from_millis(30))
            })),
        );
        let c = s.backup_capture("b").unwrap();
        assert!(c.lock_hold >= Duration::from_millis(30));
        s.set_failpoint("backup-capture-locked", None);
        let c = s.backup_capture("b").unwrap();
        assert!(c.lock_hold < Duration::from_millis(30));
    }

    #[test]
    fn a_lagging_catalog_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        ins(&s, 1);
        s.fail_catalog_writes(true);
        ins(&s, 2);
        match s.backup_capture("b") {
            Err(Error::Conflict(m)) => assert!(m.starts_with("catalog-lagging"), "{m}"),
            r => panic!("{r:?}"),
        }
        // no lease is left behind
        assert_eq!(leases(&s), 0);
        s.fail_catalog_writes(false);
        let c = s.backup_capture("b").unwrap();
        assert_eq!(c.commit.seq, 2);
        assert_eq!(c.file("commits.bin").unwrap().len, 4 * 64);
    }

    #[test]
    fn current_is_rechecked_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        // a compaction right after the first attempt's lock: the second attempt captures
        // the new generation, and the first one's lease is gone with it
        let once = Arc::new(AtomicBool::new(true));
        let o = once.clone();
        s.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(move |s: &Store| {
                if o.swap(false, Ordering::SeqCst) {
                    s.compact().unwrap();
                }
            })),
        );
        let c = s.backup_capture("b").unwrap();
        assert_eq!(c.generation, "gen-0002");
        assert_eq!(c.commit.seq, 1);
        assert_eq!(gens(&root), ["gen-0002"]);
        assert_eq!(leases(&s), 1);
        restore_and_compare(&s, &c, &dir.path().join("r"));
        drop(c);
        // a compaction after every lock: give up after the retry
        s.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(|s: &Store| s.compact().unwrap())),
        );
        assert!(matches!(s.backup_capture("b"), Err(Error::Conflict(_))));
        s.set_failpoint("backup-capture-unlocked", None);
        assert_eq!(leases(&s), 0);
        assert_eq!(gens(&root), ["gen-0004"]);
    }

    /// Captures under a concurrent writer: every capture restores to exactly its
    /// commit, and the writer lock is held briefly.
    #[test]
    fn captures_are_consistent_under_concurrent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Arc::new(Store::open(&root, StoreOptions::default()).unwrap());
        ins(&s, 0);
        let stop = Arc::new(AtomicBool::new(false));
        let n = Arc::new(AtomicU64::new(1));
        let writer = {
            let (s, stop, n) = (s.clone(), stop.clone(), n.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let i = n.fetch_add(1, Ordering::SeqCst);
                    ins(&s, i);
                    if i % 3 == 0 {
                        let j = i - 1;
                        upd(
                            &s,
                            &format!("DELETE DATA {{ <urn:s{j}> <urn:p> \"v{j}\" }}"),
                        );
                    }
                }
            })
        };
        let mut holds = Vec::new();
        let mut captures = Vec::new();
        for k in 0..200 {
            let c = s.backup_capture(&format!("c{k}")).unwrap();
            holds.push(c.lock_hold);
            if k % 20 == 0 {
                captures.push(c);
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        assert!(s.head_commit().seq > captures.last().unwrap().commit.seq);
        let seqs: Vec<u64> = captures.iter().map(|c| c.commit.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] <= w[1]), "{seqs:?}");
        for (i, c) in captures.iter().enumerate() {
            restore_and_compare(&s, c, &dir.path().join(format!("r{i}")));
        }
        holds.sort();
        let p99 = holds[holds.len() * 99 / 100 - 1];
        assert!(p99 < Duration::from_millis(5), "p99 lock hold {p99:?}");
    }

    /// A compaction and a bulk commit while a backup reads the generation: the lease
    /// keeps it (shown in the history status) until dropped, then it is collected.
    #[test]
    fn a_lease_keeps_the_generation_across_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let opts = StoreOptions {
            bulk_threshold: 2,
            ..Default::default()
        };
        let s = Store::open(&root, opts).unwrap();
        ins(&s, 1);
        ins(&s, 2);
        let c = s.backup_capture("nightly").unwrap();
        let h = s.history();
        assert_eq!(
            h.generations[0].held_by,
            [Hold::Head, Hold::Lease("nightly".into())]
        );
        ins(&s, 3);
        s.compact().unwrap();
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
        let h = s.history();
        let g1 = h.generations.iter().find(|g| g.name == "gen-0001").unwrap();
        assert_eq!(g1.held_by, [Hold::Lease("nightly".into())]);
        assert_eq!(g1.held_by[0].to_string(), "backup:nightly");
        assert!(h.bytes > 0);
        // a bulk commit while leased
        let nt: String = (0..20)
            .map(|i| format!("<urn:x{i}> <urn:p> <urn:y{i}> .\n"))
            .collect();
        s.load(&[crate::io::Source::from_bytes(
            nt.into_bytes(),
            crate::io::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        assert_eq!(gens(&root), ["gen-0001", "gen-0003"]);
        restore_and_compare(&s, &c, &dir.path().join("r"));
        drop(c);
        assert_eq!(gens(&root), ["gen-0003"]);
        assert_eq!(s.history().bytes, 0);
    }

    #[test]
    fn a_lease_dropped_while_the_writer_is_busy_is_collected_later() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        let c = s.backup_capture("b").unwrap();
        s.compact().unwrap();
        {
            let _txn = s.write();
            drop(c);
        }
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
        assert!(s.history().generations[0].held_by.is_empty());
        s.try_collect_history();
        assert_eq!(gens(&root), ["gen-0002"]);
    }

    #[test]
    fn a_lease_outliving_its_store_does_not_collect() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        let c = s.backup_capture("b").unwrap();
        s.compact().unwrap();
        drop(s);
        drop(c);
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
        // the next open collects it
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(gens(&root), ["gen-0002"]);
        assert_eq!(s.head_commit().seq, 1);
    }

    #[test]
    fn an_owned_capture_keeps_the_store_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        let want = dump(&s);
        let c = s.into_backup_capture("offline").unwrap();
        assert!(Store::open(&root, StoreOptions::default()).is_err());
        let out = dir.path().join("r");
        c.write_to(&out).unwrap();
        drop(c);
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(dump(&r), want);
        assert!(Store::open(&root, StoreOptions::default()).is_ok());
    }

    /// A restore under a new identity: same commits and data, a new dataset id, the
    /// source recorded as `forkedFrom`, and the sequence continuing at s + 1.
    #[test]
    fn reidentify_makes_a_new_lineage() {
        use crate::commit::{CommitRange, ForkedFrom, reidentify};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        for i in 0..3 {
            ins(&s, i);
        }
        s.create_snapshot("v2", &At::Commit(2), None).unwrap();
        s.compact().unwrap();
        ins(&s, 3);
        let c = s.backup_capture("b").unwrap();
        let out = dir.path().join("r");
        c.write_to(&out).unwrap();
        // a leased history.json too (backups leave it out; reidentify handles it)
        std::fs::copy(root.join("history.json"), out.join("history.json")).unwrap();
        let old = s.dataset_id();
        let new = uuid::Uuid::new_v4();
        let from = ForkedFrom {
            id: old,
            seq: c.commit.seq,
        };
        reidentify(&out, new, from).unwrap();
        let (id, _) = crate::commit::read_catalog(&out.join("commits.bin"))
            .unwrap()
            .unwrap();
        assert_eq!(id, new);
        let opts = crate::check::CheckOptions { quick: false };
        let report = crate::check::check(&out, &opts).unwrap();
        assert_ne!(report.status, crate::check::Status::Error);
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.dataset_id(), new);
        assert_eq!(r.forked_from(), Some(from));
        assert_eq!(r.head_commit(), c.commit);
        assert_eq!(dump(&r), dump_at(&s, c.commit.seq));
        let page = r.commits(CommitRange::Latest, 100);
        assert_eq!(page.commits.len(), 5);
        assert!(page.complete);
        assert_eq!(page.commits, s.commits(CommitRange::Latest, 100).commits);
        assert_eq!(r.named_snapshot("v2").unwrap().seq, 2);
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("dataset.json")).unwrap()).unwrap();
        assert_eq!(info["origin"], "restore");
        assert_eq!(info["id"], new.to_string());
        ins(&r, 9);
        assert_eq!(r.head_commit().seq, c.commit.seq + 1);
        let receipt = update(
            &r,
            "INSERT DATA { <urn:z> <urn:p> 0 }",
            &QueryOptions::default(),
        );
        assert!(receipt.is_ok());
        assert_eq!(r.head_commit().seq, c.commit.seq + 2);
        drop(r);
        // and it opens again as the new lineage
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.dataset_id(), new);
        assert_eq!(r.head_commit().seq, c.commit.seq + 2);
    }

    #[test]
    fn reidentify_refuses_a_foreign_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        drop(Store::open(&a, StoreOptions::default()).unwrap());
        drop(Store::open(&b, StoreOptions::default()).unwrap());
        std::fs::copy(b.join("commits.bin"), a.join("commits.bin")).unwrap();
        let from = crate::commit::ForkedFrom {
            id: uuid::Uuid::new_v4(),
            seq: 0,
        };
        let e = crate::commit::reidentify(&a, uuid::Uuid::new_v4(), from).unwrap_err();
        assert!(matches!(e, Error::Corrupt(_)), "{e}");
        assert!(
            crate::commit::reidentify(&dir.path().join("none"), uuid::Uuid::new_v4(), from)
                .is_err()
        );
    }

    #[test]
    fn restored_from_round_trips() {
        use crate::commit::{RestoredFrom, read_restored_from, set_restored_from};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        assert_eq!(s.restored_from(), None);
        let id = s.dataset_id();
        drop(s);
        assert_eq!(read_restored_from(&root).unwrap(), None);
        let from = RestoredFrom {
            repository: "offsite".into(),
            backup: "wiki-20260930t140311z".into(),
            dataset_id: uuid::Uuid::new_v4(),
            seq: 41,
        };
        set_restored_from(&root, &from).unwrap();
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
        assert_eq!(info["restoredFrom"]["backup"], "wiki-20260930t140311z");
        assert_eq!(
            info["restoredFrom"]["datasetId"],
            from.dataset_id.to_string()
        );
        assert_eq!(read_restored_from(&root).unwrap(), Some(from.clone()));
        // a new identity keeps it
        let fork = crate::commit::ForkedFrom { id, seq: 1 };
        crate::commit::reidentify(&root, uuid::Uuid::new_v4(), fork).unwrap();
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.restored_from(), Some(from));
        assert_eq!(s.forked_from(), Some(fork));
    }
}
