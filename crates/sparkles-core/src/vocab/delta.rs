//! The persisted delta vocabulary: the terms SPARQL Update introduces, in `delta.vocab`.
//!
//! # File format
//!
//! A file written by this release is *framed*. It starts with a 16-byte header, the
//! magic bytes [`MAGIC`], the format version 2 as a little-endian `u32` and four zero
//! bytes. Chunks follow, one for each time the writer wrote out the terms added since
//! the last chunk. A chunk is a 24-byte header and a payload:
//!
//! | bytes | field |
//! |---|---|
//! | 0..4 | CRC-32 of header bytes 4..24 and of the payload |
//! | 4..8 | number of entries, at least 1 |
//! | 8..12 | payload length in bytes |
//! | 12..16 | zero |
//! | 16..24 | id of the chunk's first entry within the file (the entries before it) |
//!
//! The payload is the entries, each a little-endian `u32` key length (at least 1) and
//! the key. An entry's id is its position in the file, so the ids of a chunk's entries
//! continue those of the chunk before it.
//!
//! The checksummed chunks give the file a durable end. A reader takes chunks from the
//! start and stops at the first one whose header is zero, whose length runs past the
//! file, whose first id does not continue the entries before it, or whose checksum
//! fails. Everything after that point is the space preallocated for later chunks or a
//! chunk a crash cut short. A writer only writes a chunk through the direct path
//! described below when every chunk before it is durable, and a buffered chunk becomes
//! durable together with every chunk before it, so no durable chunk can follow one that
//! did not reach the disk.
//!
//! Files written by releases before this one are *legacy* files: the entries alone,
//! with no header. They are read as before, up to the first entry cut short or a tail
//! of zero bytes. The first write to a legacy file rewrites it framed (a new file
//! renamed over it). Before that, and before the first framed file of a dataset, the
//! dataset's `dataset.json` is updated to require reader [`FRAMED_READER`], so a
//! release that cannot read framed files refuses the dataset rather than misreading it.
//!
//! # Writing
//!
//! New terms stay in memory until a commit syncs them. The writer then encodes them as
//! one chunk and writes it at the file's logical end. The file grows in steps of zero
//! bytes, which the sync that grows it makes durable with the chunk. A chunk that fits
//! in the zeros, while every byte before it is durable, can be written with
//! `O_DIRECT | O_DSYNC` through a [`DirectLog`]. That write overwrites allocated blocks
//! and changes no file size, so it needs no journal commit, and on a device with FUA it
//! persists one block instead of flushing the device cache. The commit path runs it in
//! parallel with the write of the commit's WAL records. Any other chunk is a buffered
//! write followed by `fdatasync`.
//!
//! A crash can leave a commit's WAL records durable without its chunk, or the chunk
//! without the records. Replay treats a last commit that names ids the file lacks as a
//! torn tail, and terms without a commit are unused entries.

use super::{AppendVocab, LazySync, PendingSync};
use crate::error::{Error, Result};
use crate::store::wal::{self, DirectLog};
use parking_lot::{Mutex, RwLock};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The first bytes of a framed delta vocabulary file. A legacy file starts with the
/// length of its first key, which would have to be over a gigabyte to match.
pub const MAGIC: [u8; 8] = *b"\x89SDV\r\n\x1a\n";
/// The version of the framed format in its header.
pub const VERSION: u32 = 2;
/// Bytes of the file header.
pub const HEADER: usize = 16;
/// Bytes of a chunk header.
pub const CHUNK_HEADER: usize = 24;
/// The dataset reader (`minimumReader` of `dataset.json`) that reads framed files.
pub const FRAMED_READER: u32 = 4;
/// The most payload one chunk of a converted legacy file holds.
const CONVERT_CHUNK: usize = 1 << 20;
/// The most a delta vocabulary grows ahead of its chunks at once.
pub const MAX_PREALLOC: u64 = 1 << 20;

/// The format of a delta vocabulary file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaFormat {
    /// written by a release before framed files, or empty
    Legacy,
    /// a header and checksummed chunks
    Framed,
}

impl DeltaFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            DeltaFormat::Legacy => "legacy",
            DeltaFormat::Framed => "framed",
        }
    }
}

/// The complete entries of a delta vocabulary file.
pub struct ParsedDelta<'a> {
    pub keys: Vec<&'a [u8]>,
    /// where the complete entries end: past it lie zeros or a torn tail
    pub end: usize,
    pub format: DeltaFormat,
    /// chunks of a framed file
    pub chunks: usize,
}

/// Read a delta vocabulary file's content (see the module documentation). A framed
/// file of a later version is [`Error::Unsupported`], and a chunk whose checksum
/// matches but whose entries do not fill it exactly is [`Error::Corrupt`].
pub fn parse_delta(buf: &[u8]) -> Result<ParsedDelta<'_>> {
    parse_delta_with(buf, |_| {})
}

/// Where each chunk of a framed file ends, in order (none for a legacy file).
pub fn chunk_ends(buf: &[u8]) -> Result<Vec<usize>> {
    let mut ends = Vec::new();
    parse_delta_with(buf, |end| ends.push(end))?;
    Ok(ends)
}

fn parse_delta_with(buf: &[u8], mut on_chunk: impl FnMut(usize)) -> Result<ParsedDelta<'_>> {
    if buf.len() < MAGIC.len() || buf[..MAGIC.len()] != MAGIC {
        let (keys, end) = legacy_entries(buf);
        return Ok(ParsedDelta {
            keys,
            end,
            format: DeltaFormat::Legacy,
            chunks: 0,
        });
    }
    let mut out = ParsedDelta {
        keys: Vec::new(),
        end: 0,
        format: DeltaFormat::Framed,
        chunks: 0,
    };
    // the header is written with the first chunk, so a torn one holds no entries
    let version = match buf.get(8..12) {
        Some(v) => u32::from_le_bytes(v.try_into().unwrap()),
        None => return Ok(out),
    };
    match version {
        0 => return Ok(out),
        VERSION => {}
        v => {
            return Err(Error::Unsupported(format!(
                "a delta vocabulary of format {v}, written by a later release; this build reads format {VERSION}"
            )));
        }
    }
    let mut pos = HEADER;
    while let Some(h) = buf.get(pos..pos + CHUNK_HEADER) {
        let word = |at: usize| u32::from_le_bytes(h[at..at + 4].try_into().unwrap());
        let (crc, n, plen) = (word(0), word(4) as usize, word(8) as usize);
        let first = u64::from_le_bytes(h[16..24].try_into().unwrap());
        if n == 0 || first != out.keys.len() as u64 {
            break;
        }
        let Some(payload) = buf.get(pos + CHUNK_HEADER..pos + CHUNK_HEADER + plen) else {
            break;
        };
        if crc != chunk_crc(&h[4..], payload) {
            break;
        }
        let (mut p, before) = (0usize, out.keys.len());
        for _ in 0..n {
            let len = match payload.get(p..p + 4) {
                Some(l) => u32::from_le_bytes(l.try_into().unwrap()) as usize,
                None => break,
            };
            if len == 0 || p + 4 + len > plen {
                break;
            }
            out.keys.push(&payload[p + 4..p + 4 + len]);
            p += 4 + len;
        }
        if out.keys.len() - before != n || p != plen {
            return Err(Error::Corrupt(format!(
                "the delta vocabulary chunk at byte {pos} matches its checksum but does not hold {n} entries in {plen} bytes"
            )));
        }
        pos += CHUNK_HEADER + plen;
        out.chunks += 1;
        on_chunk(pos);
    }
    out.end = pos;
    Ok(out)
}

/// The complete entries of a legacy file (`u32` length, key), and where they end. An
/// entry cut short ends them, as does a tail of zero bytes: no key is empty, and a
/// crash can leave zeros where the file's length reached the disk before its data (on
/// file systems that do not order the two). Either is a torn tail.
fn legacy_entries(buf: &[u8]) -> (Vec<&[u8]>, usize) {
    let mut pos = 0;
    let mut keys = Vec::new();
    while pos + 4 <= buf.len() {
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        if pos + 4 + len > buf.len() || (len == 0 && buf[pos..].iter().all(|&b| b == 0)) {
            break;
        }
        keys.push(&buf[pos + 4..pos + 4 + len]);
        pos += 4 + len;
    }
    (keys, pos)
}

fn chunk_crc(header: &[u8], payload: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(header);
    c.update(payload);
    c.sum()
}

/// A chunk holding `keys`, whose first entry has id `first` within the file, preceded by
/// the file header when `header` is set.
fn encode_chunk(first: u64, keys: &[Arc<[u8]>], header: bool) -> Vec<u8> {
    let plen: usize = keys.iter().map(|k| 4 + k.len()).sum();
    let mut out = Vec::with_capacity(HEADER + CHUNK_HEADER + plen);
    if header {
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    let at = out.len();
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    out.extend_from_slice(&(plen as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&first.to_le_bytes());
    for k in keys {
        out.extend_from_slice(&(k.len() as u32).to_le_bytes());
        out.extend_from_slice(k);
    }
    let crc = chunk_crc(&out[at + 4..at + CHUNK_HEADER], &out[at + CHUNK_HEADER..]);
    out[at..at + 4].copy_from_slice(&crc.to_le_bytes());
    out
}

/// The end of a delta vocabulary at some moment (see [`DeltaVocab::mark`]).
#[derive(Clone, Copy, Debug)]
pub struct VocabMark {
    entries: u64,
    /// the file's entries written to it, and where they end
    written: u64,
    end: u64,
    /// the file was still a legacy one
    legacy: bool,
}

/// The persisted, append-only delta vocabulary (terms introduced by updates).
///
/// Readers and the single writer share it through an `RwLock`; ids only ever grow, so a
/// reader's snapshot remains valid while the writer appends.
pub struct DeltaVocab {
    inner: Arc<RwLock<AppendVocab>>,
    file: Option<Arc<Mutex<DeltaFile>>>,
}

/// The write side of a delta vocabulary file.
///
/// Lock order: the sync gate, then this mutex, then the vocabulary's `RwLock`.
pub(crate) struct DeltaFile {
    path: PathBuf,
    file: File,
    vocab: Arc<RwLock<AppendVocab>>,
    /// entries of the vocabulary before the file's first (a linked generation's
    /// upstream prefix)
    base: u64,
    format: DeltaFormat,
    /// where the entries of a legacy file end once it was rewritten framed
    converted_end: u64,
    /// the file's entries written to it
    written: u64,
    /// where they end: the file's logical end
    end: u64,
    /// the file's length: past `end` it holds zeros (or, until [`DeltaVocab::settle`],
    /// a torn tail)
    alloc: u64,
    /// the logical end known to be durable
    durable: u64,
    /// the most the file grows ahead of its chunks at once
    prealloc: u64,
    direct: wal::Direct,
    /// a write failed: the file's state past `durable` is unknown
    failed: bool,
    /// Serializes syncs and destructive rollback, not inserts or marks.
    sync_gate: Arc<Mutex<()>>,
    #[cfg(test)]
    sync_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    fail_next_sync: bool,
}

impl DeltaFile {
    /// Entries of the vocabulary not written to the file yet.
    fn pending(&self) -> Vec<Arc<[u8]>> {
        let v = self.vocab.read();
        let from = (self.base + self.written) as usize;
        v.keys.get(from..).map_or_else(Vec::new, <[_]>::to_vec)
    }

    fn needs_sync(&self) -> bool {
        self.durable < self.end || self.base + self.written < self.vocab.read().len()
    }

    /// Make the file framed before its first write: require the reader that reads
    /// framed files, then rewrite a legacy file's entries as chunks in a new file
    /// renamed over it. An empty file gets its header with the first chunk.
    fn ensure_framed(&mut self) -> Result<()> {
        if self.format == DeltaFormat::Framed {
            return Ok(());
        }
        require_framed_reader(&self.path)?;
        if self.written == 0 {
            // nothing to keep: the first chunk starts the file with the header
            self.end = 0;
            self.durable = 0;
            self.converted_end = 0;
            self.format = DeltaFormat::Framed;
            return Ok(());
        }
        let keys: Vec<Arc<[u8]>> = {
            let v = self.vocab.read();
            let from = self.base as usize;
            v.keys[from..from + self.written as usize].to_vec()
        };
        let mut bytes = Vec::new();
        let mut start = 0usize;
        while start < keys.len() {
            let (mut end, mut size) = (start, 0usize);
            while end < keys.len() && (end == start || size + 4 + keys[end].len() <= CONVERT_CHUNK)
            {
                size += 4 + keys[end].len();
                end += 1;
            }
            bytes.extend_from_slice(&encode_chunk(start as u64, &keys[start..end], start == 0));
            start = end;
        }
        let tmp = self.path.with_file_name("delta.vocab.tmp");
        crate::store::write_synced(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        crate::store::sync_dir(self.path.parent().unwrap_or(Path::new(".")))?;
        self.file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.end = bytes.len() as u64;
        self.alloc = self.end;
        self.durable = self.end;
        self.converted_end = self.end;
        // a handle of the replaced file must not take writes
        self.direct = wal::Direct::Untried;
        self.format = DeltaFormat::Framed;
        tracing::info!(target: "sparkles::store", "rewrote {} as a framed delta vocabulary ({} terms)", self.path.display(), keys.len());
        Ok(())
    }

    /// Write the pending entries as a chunk at the logical end, with a buffered write,
    /// growing the file by zero bytes when the chunk runs past its length. Not synced.
    fn write_pending(&mut self) -> Result<()> {
        let keys = self.pending();
        if keys.is_empty() {
            return Ok(());
        }
        self.ensure_framed()?;
        self.forget_direct_block();
        let chunk = encode_chunk(self.written, &keys, self.end == 0);
        if let Err(e) =
            wal::write_commit(&self.file, self.end, &mut self.alloc, &chunk, self.prealloc)
        {
            self.failed = true;
            return Err(e.into());
        }
        self.end += chunk.len() as u64;
        self.written += keys.len() as u64;
        Ok(())
    }

    /// A write other than through the write-through handle changes the bytes it keeps a
    /// copy of.
    fn forget_direct_block(&mut self) {
        if let wal::Direct::Open(d) = &mut self.direct {
            d.invalidate();
        }
    }

    /// Write the pending entries and make every written chunk durable with `fdatasync`.
    /// The caller holds the sync gate. The mutex is released for the `fdatasync`, so the
    /// writer can mark and insert meanwhile, and a later chunk written then is not
    /// counted durable.
    fn sync_gated(file: &Arc<Mutex<Self>>) -> Result<()> {
        let (fd, end) = {
            let mut f = file.lock();
            if !f.needs_sync() {
                return Ok(());
            }
            f.write_pending()?;
            (f.file.try_clone()?, f.end)
        };
        #[cfg(test)]
        Self::test_hook(file)?;
        // The exact descriptor keeps this generation alive. Later appenders can
        // proceed; syncing extra suffix bytes never publishes that suffix.
        if let Err(e) = fd.sync_data() {
            file.lock().failed = true;
            return Err(e.into());
        }
        let mut f = file.lock();
        debug_assert!(f.end >= end, "rollback must serialize with prefix sync");
        f.durable = f.durable.max(end);
        Ok(())
    }

    pub(crate) fn sync(file: &Arc<Mutex<Self>>) -> Result<()> {
        // Never acquire this gate while retaining the mutex. Rollback uses the same
        // order, so a captured prefix cannot be cut under its fence.
        let gate = file.lock().sync_gate.clone();
        let _fence = gate.lock();
        Self::sync_gated(file)
    }

    /// [`sync`](Self::sync) through the write-through handle when it can: the pending
    /// entries as one chunk that fits in the preallocated zeros, after a durable
    /// prefix. The mutex stays held for the write, so no buffered write can land in
    /// the block it rewrites.
    pub(crate) fn sync_direct(file: &Arc<Mutex<Self>>) -> Result<()> {
        let gate = file.lock().sync_gate.clone();
        let _fence = gate.lock();
        {
            let mut guard = file.lock();
            let f = &mut *guard;
            if !f.needs_sync() {
                return Ok(());
            }
            let keys = f.pending();
            if f.format == DeltaFormat::Framed
                && f.end >= HEADER as u64
                && f.durable == f.end
                && !keys.is_empty()
            {
                let chunk = encode_chunk(f.written, &keys, false);
                if DirectLog::fits(f.end, chunk.len(), f.alloc)
                    && let Some(direct) = f.direct.handle(Some(&f.path), &f.file)
                {
                    #[cfg(test)]
                    {
                        if let Some(hook) = f.sync_hook.clone() {
                            hook();
                        }
                    }
                    match direct.write(f.end, &chunk) {
                        Ok(true) => {
                            f.end += chunk.len() as u64;
                            f.written += keys.len() as u64;
                            #[cfg(test)]
                            {
                                if std::mem::take(&mut f.fail_next_sync) {
                                    f.failed = true;
                                    return Err(
                                        std::io::Error::other("injected sync failure").into()
                                    );
                                }
                            }
                            f.durable = f.end;
                            return Ok(());
                        }
                        // the file system refused direct I/O: buffered from now on
                        Ok(false) => f.direct = wal::Direct::Off,
                        Err(e) => {
                            f.failed = true;
                            return Err(e.into());
                        }
                    }
                }
            }
        }
        Self::sync_gated(file)
    }

    #[cfg(test)]
    fn test_hook(file: &Arc<Mutex<Self>>) -> Result<()> {
        let (hook, failed) = {
            let mut f = file.lock();
            (f.sync_hook.clone(), std::mem::take(&mut f.fail_next_sync))
        };
        if let Some(hook) = hook {
            hook();
        }
        if failed {
            return Err(std::io::Error::other("injected sync failure").into());
        }
        Ok(())
    }
}

/// Require the dataset reader that reads framed delta vocabularies in the store whose
/// generation holds the file at `path`, and in its dataset when the store is a branch.
/// A generation directory sits in its store's root.
fn require_framed_reader(path: &Path) -> Result<()> {
    let Some(root) = path.parent().and_then(Path::parent) else {
        return Ok(());
    };
    crate::commit::require_reader(root, FRAMED_READER)?;
    if root.join(crate::store::BRANCH_FILE_NAME).exists()
        && let Some(ds) = root.parent().and_then(Path::parent)
    {
        crate::commit::require_reader(ds, FRAMED_READER)?;
    }
    Ok(())
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut buf)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(buf)
}

fn corrupt_in(path: &Path, e: Error) -> Error {
    match e {
        Error::Corrupt(m) => Error::Corrupt(format!("{}: {m}", path.display())),
        Error::Unsupported(m) => Error::Unsupported(format!("{}: {m}", path.display())),
        e => e,
    }
}

impl DeltaVocab {
    pub(crate) fn fork_memory(&self, len: u64) -> DeltaVocab {
        let mut copy = AppendVocab::default();
        self.with(|v| {
            for n in 0..len {
                if let Some(k) = v.get(n) {
                    copy.push_dup(k);
                }
            }
        });
        DeltaVocab {
            inner: Arc::new(RwLock::new(copy)),
            file: None,
        }
    }

    pub fn in_memory() -> DeltaVocab {
        DeltaVocab {
            inner: Default::default(),
            file: None,
        }
    }

    /// The delta vocabulary of a sealed generation, for reading: no write handle, and
    /// anything past the complete entries is ignored.
    pub fn open_read_only(path: &Path) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        let buf = read_file(path)?;
        for key in parse_delta(&buf).map_err(|e| corrupt_in(path, e))?.keys {
            v.insert(key);
        }
        Ok(DeltaVocab {
            inner: Arc::new(RwLock::new(v)),
            file: None,
        })
    }

    /// The delta vocabulary of the generation being written. Nothing in the file
    /// changes until the first write or [`settle`](Self::settle).
    pub fn open(path: &Path) -> Result<DeltaVocab> {
        Self::open_after(AppendVocab::default(), path)
    }

    /// Open the file at `path` for writing, its entries following those of `v`.
    fn open_after(mut v: AppendVocab, path: &Path) -> Result<DeltaVocab> {
        let base = v.len();
        let buf = read_file(path)?;
        let parsed = parse_delta(&buf).map_err(|e| corrupt_in(path, e))?;
        for key in &parsed.keys {
            if base == 0 {
                v.insert(key);
            } else {
                // ids are positions: a key seen upstream still takes its position here
                v.push_dup(key);
            }
        }
        let written = parsed.keys.len() as u64;
        let (end, format) = (parsed.end as u64, parsed.format);
        drop(buf);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let alloc = file.metadata()?.len();
        let inner = Arc::new(RwLock::new(v));
        Ok(DeltaVocab {
            inner: inner.clone(),
            file: Some(Arc::new(Mutex::new(DeltaFile {
                path: path.to_path_buf(),
                file,
                vocab: inner,
                base,
                format,
                converted_end: 0,
                written,
                end,
                alloc,
                durable: end,
                prealloc: MAX_PREALLOC,
                direct: wal::Direct::Untried,
                failed: false,
                sync_gate: Default::default(),
                #[cfg(test)]
                sync_hook: None,
                #[cfg(test)]
                fail_next_sync: false,
            }))),
        })
    }

    /// The delta vocabulary of a linked generation (a branch that reads an upstream
    /// generation's files). Its first entries are those of the upstream files `prefix`,
    /// each `(path, end)` pair contributing the entries up to id `end` (a file's
    /// entries continue the ids of the pairs before it). Its own entries follow, from
    /// `own`, which takes the appends unless the generation is opened `read_only`.
    pub(crate) fn open_layered(
        prefix: &[(PathBuf, u64)],
        own: &Path,
        read_only: bool,
    ) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        for (path, end) in prefix {
            if v.len() >= *end {
                continue;
            }
            let buf = read_file(path)?;
            for key in parse_delta(&buf).map_err(|e| corrupt_in(path, e))?.keys {
                if v.len() >= *end {
                    break;
                }
                // ids are positions: a key seen before still takes its position here
                v.push_dup(key);
            }
            if v.len() < *end {
                return Err(Error::Corrupt(format!(
                    "{} has {} delta terms, fewer than the {end} a branch's link names",
                    path.display(),
                    v.len()
                )));
            }
        }
        if !read_only {
            return Self::open_after(v, own);
        }
        let buf = read_file(own)?;
        for key in parse_delta(&buf).map_err(|e| corrupt_in(own, e))?.keys {
            v.push_dup(key);
        }
        Ok(DeltaVocab {
            inner: Arc::new(RwLock::new(v)),
            file: None,
        })
    }

    pub fn len(&self) -> u64 {
        self.inner.read().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        self.inner.read().get(id).map(|k| k.to_vec())
    }
    pub fn find(&self, key: &[u8]) -> Option<u64> {
        self.inner.read().find(key)
    }
    pub fn with<R>(&self, f: impl FnOnce(&AppendVocab) -> R) -> R {
        f(&self.inner.read())
    }

    /// Insert (writer only). New keys stay in memory until the next
    /// [`sync`](Self::sync) or [`flush`](Self::flush) writes them.
    pub fn insert(&self, key: &[u8]) -> Result<u64> {
        Ok(self.inner.write().insert(key).0)
    }

    /// Where the vocabulary ends now, for a later [`rollback`](Self::rollback).
    pub fn mark(&self) -> VocabMark {
        match &self.file {
            Some(f) => {
                let f = f.lock();
                VocabMark {
                    entries: self.len(),
                    written: f.written,
                    end: f.end,
                    legacy: f.format == DeltaFormat::Legacy,
                }
            }
            None => VocabMark {
                entries: self.len(),
                written: 0,
                end: 0,
                legacy: false,
            },
        }
    }

    /// Remove every entry inserted since `m` (writer only, with no snapshot reaching
    /// those ids). Entries not written yet are dropped. When a chunk written since the
    /// mark holds some, the file is cut back to the mark with zeros, and they are synced
    /// before anything is written there again: a later chunk at the same place must not
    /// be mistaken for this one after a crash. The entries before the mark that the
    /// chunk held are written again with the next chunk.
    pub fn rollback(&self, m: &VocabMark) -> Result<()> {
        let gate = self.file.as_ref().map(|f| f.lock().sync_gate.clone());
        let _fence = gate.as_ref().map(|g| g.lock());
        let mut guard = self.file.as_ref().map(|f| f.lock());
        let mut inner = self.inner.write();
        if inner.len() <= m.entries {
            return Ok(());
        }
        inner.truncate(m.entries);
        drop(inner);
        let Some(f) = guard.as_deref_mut() else {
            return Ok(());
        };
        if f.base + f.written <= m.entries {
            return Ok(());
        }
        // A mark from before the file was rewritten framed: the rewrite held exactly
        // the entries written by then, which are the mark's.
        let to = if m.legacy { f.converted_end } else { m.end };
        f.forget_direct_block();
        let zeros = vec![0u8; (f.end - to) as usize];
        let cut = wal::write_commit(&f.file, to, &mut f.alloc, &zeros, 0)
            .and_then(|_| f.file.sync_data());
        if let Err(e) = cut {
            f.failed = true;
            return Err(e.into());
        }
        f.end = to;
        f.written = m.written;
        f.durable = to;
        Ok(())
    }

    /// Make every inserted entry durable. Without new entries since the last sync this
    /// does nothing: an `fdatasync` of a file with nothing to write still costs a device
    /// cache flush on some file systems, and every commit calls this.
    pub fn sync(&self) -> Result<()> {
        match &self.file {
            Some(file) => DeltaFile::sync(file),
            None => Ok(()),
        }
    }

    /// [`sync`](Self::sync), writing the entries through the write-through handle when
    /// they fit in the file's preallocated space (see the module documentation).
    pub fn sync_direct(&self) -> Result<()> {
        match &self.file {
            Some(file) => DeltaFile::sync_direct(file),
            None => Ok(()),
        }
    }

    /// Schedule this exact generation's sync on a reusable worker, through the
    /// write-through handle when `direct` is set. Publication still waits for
    /// completion; grouped writers may append a suffix.
    pub(crate) fn sync_on(&self, worker: &mut LazySync, direct: bool) -> Option<PendingSync> {
        self.file
            .as_ref()
            .and_then(|file| worker.start(file.clone(), direct))
    }

    /// Whether entries were inserted or written since the last [`sync`](Self::sync), that
    /// is, whether the next one writes and syncs anything.
    pub fn needs_sync(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.lock().needs_sync())
    }

    /// The most the file grows ahead of its chunks at once (0: it grows by each chunk).
    pub(crate) fn set_prealloc(&self, bytes: u64) {
        if let Some(f) = &self.file {
            f.lock().prealloc = bytes.min(MAX_PREALLOC);
        }
    }

    /// The file's length (0 for an in-memory vocabulary).
    pub(crate) fn allocated(&self) -> u64 {
        self.file.as_ref().map_or(0, |f| f.lock().alloc)
    }

    /// Bytes of zeros after the file's logical end.
    pub(crate) fn preallocated(&self) -> u64 {
        self.file.as_ref().map_or(0, |f| {
            let f = f.lock();
            f.alloc.saturating_sub(f.end)
        })
    }

    /// Whether the write-through handle is open, so that chunks that fit go through it.
    pub fn direct_active(&self) -> bool {
        self.file
            .as_ref()
            .is_some_and(|f| matches!(f.lock().direct, wal::Direct::Open(_)))
    }

    /// The file's format.
    pub fn format(&self) -> Option<DeltaFormat> {
        self.file.as_ref().map(|f| f.lock().format)
    }

    /// Make the file as the open found it the durable starting point of the writes to
    /// come, once the store's log was replayed: cut off anything past its complete
    /// entries, and sync it. A process that stopped without syncing can leave bytes that
    /// are only in the page cache, and a direct write after them must not be durable
    /// while they are not.
    pub(crate) fn settle(&self) -> Result<()> {
        let Some(f) = &self.file else { return Ok(()) };
        let mut f = f.lock();
        if f.alloc > f.end {
            f.file.set_len(f.end)?;
            f.alloc = f.end;
        }
        if f.end > 0 {
            f.file.sync_data()?;
        }
        f.durable = f.end;
        Ok(())
    }

    /// Cut the preallocated zeros off the file, when the store closes or moves on to
    /// another generation. Not after a failed write, whose bytes the next open sorts
    /// out, and not while written entries are not durable.
    pub(crate) fn trim(&self) {
        let Some(f) = &self.file else { return };
        let mut f = f.lock();
        if !f.failed && f.durable == f.end && f.alloc > f.end && f.file.set_len(f.end).is_ok() {
            f.alloc = f.end;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_sync_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        if let Some(f) = &self.file {
            f.lock().sync_hook = Some(hook);
        }
    }

    /// Make the next [`sync`](Self::sync) with new entries fail after writing them to
    /// the file, as a failed `fdatasync` would.
    #[cfg(test)]
    pub(crate) fn fail_next_sync(&self) {
        if let Some(f) = &self.file {
            f.lock().fail_next_sync = true;
        }
    }

    /// Write the entries not written yet to the file (without syncing them) and return
    /// the file's logical end: every entry inserted so far lies before it. 0 for an
    /// in-memory vocabulary.
    pub fn flush(&self) -> Result<u64> {
        match &self.file {
            Some(f) => {
                let mut f = f.lock();
                f.write_pending()?;
                Ok(f.end)
            }
            None => Ok(0),
        }
    }

    /// For a backup: [`flush`](Self::flush), and a read handle of the exact file it
    /// wrote to, which the backup reads up to the returned end.
    pub(crate) fn capture(&self) -> Result<Option<(File, u64)>> {
        match &self.file {
            Some(f) => {
                let mut f = f.lock();
                f.write_pending()?;
                Ok(Some((f.file.try_clone()?, f.end)))
            }
            None => Ok(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn file_handle(&self) -> Arc<Mutex<DeltaFile>> {
        self.file.as_ref().unwrap().clone()
    }

    #[cfg(test)]
    pub(crate) fn file_state(&self) -> (u64, u64, u64) {
        let f = self.file.as_ref().unwrap().lock();
        (f.end, f.durable, f.alloc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(k: &[u8]) -> Vec<u8> {
        [&(k.len() as u32).to_le_bytes()[..], k].concat()
    }

    #[test]
    fn delta_sync_only_when_entries_were_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dvocab.bin");
        let v = DeltaVocab::open(&path).unwrap();
        assert!(!v.needs_sync());
        assert_eq!(v.insert(b"<http://x/a>").unwrap(), 0);
        assert!(v.needs_sync());
        v.sync().unwrap();
        assert!(!v.needs_sync());
        // a key it already has appends nothing
        assert_eq!(v.insert(b"<http://x/a>").unwrap(), 0);
        assert!(!v.needs_sync());
        assert_eq!(v.insert(b"<http://x/b>").unwrap(), 1);
        v.sync().unwrap();
        drop(v);
        let v = DeltaVocab::open(&path).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v.get(1).unwrap(), b"<http://x/b>");
        assert_eq!(v.format(), Some(DeltaFormat::Framed));
    }

    /// Each sync writes one chunk after the header, and the file grows by zeros that a
    /// reader stops at.
    #[test]
    fn chunks_follow_the_header_and_zeros_end_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let v = DeltaVocab::open(&path).unwrap();
        v.set_prealloc(64 << 10);
        v.insert(b"<urn:a>").unwrap();
        v.insert(b"<urn:b>").unwrap();
        v.sync().unwrap();
        v.insert(b"<urn:c>").unwrap();
        v.sync_direct().unwrap();
        let (end, durable, alloc) = v.file_state();
        assert_eq!(durable, end);
        assert!(alloc >= 64 << 10);
        let buf = std::fs::read(&path).unwrap();
        assert_eq!(buf.len() as u64, alloc);
        assert_eq!(&buf[..8], &MAGIC);
        let p = parse_delta(&buf).unwrap();
        assert_eq!(p.format, DeltaFormat::Framed);
        assert_eq!(p.chunks, 2);
        assert_eq!(p.end as u64, end);
        assert_eq!(p.keys, vec![&b"<urn:a>"[..], b"<urn:b>", b"<urn:c>"]);
        assert!(buf[p.end..].iter().all(|&b| b == 0));
        // a damaged chunk ends the entries, and so does one that repeats the ids
        let second = HEADER + CHUNK_HEADER + 2 * (4 + 7);
        let mut bad = buf.clone();
        bad[second + CHUNK_HEADER + 5] ^= 1;
        assert_eq!(parse_delta(&bad).unwrap().keys.len(), 2);
        let mut bad = buf.clone();
        bad[second + 16] = 0;
        assert_eq!(parse_delta(&bad).unwrap().keys.len(), 2);
        // a later version is refused, not misread
        let mut later = buf.clone();
        later[8] = 3;
        assert!(matches!(parse_delta(&later), Err(Error::Unsupported(_))));
        // settle and trim cut the zeros, and the entries stay
        v.trim();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), end);
        drop(v);
        assert_eq!(DeltaVocab::open(&path).unwrap().len(), 3);
    }

    #[test]
    fn a_legacy_file_is_read_and_rewritten_framed_at_the_first_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let good = [entry(b"<http://x/a>"), entry(b"<http://x/b>")].concat();
        // the length of a crash's appends reached the disk, their data did not
        std::fs::write(&path, [&good[..], &[0u8; 23][..]].concat()).unwrap();
        let v = DeltaVocab::open(&path).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v.format(), Some(DeltaFormat::Legacy));
        assert!(!v.needs_sync());
        // the open changes nothing
        assert_eq!(std::fs::read(&path).unwrap().len(), good.len() + 23);
        assert_eq!(v.insert(b"<http://x/c>").unwrap(), 2);
        v.sync().unwrap();
        assert_eq!(v.format(), Some(DeltaFormat::Framed));
        drop(v);
        let v = DeltaVocab::open(&path).unwrap();
        assert_eq!(v.format(), Some(DeltaFormat::Framed));
        assert_eq!(v.len(), 3);
        assert_eq!(v.get(0).unwrap(), b"<http://x/a>");
        assert_eq!(v.get(2).unwrap(), b"<http://x/c>");
        // zeros followed by data are entries of a legacy file, as they always were read
        let odd = [&good[..], &entry(b"")[..], &entry(b"<http://x/d>")[..]].concat();
        std::fs::write(&path, &odd).unwrap();
        assert_eq!(DeltaVocab::open_read_only(&path).unwrap().len(), 4);
        assert_eq!(DeltaVocab::open(&path).unwrap().len(), 4);
        assert_eq!(std::fs::read(&path).unwrap(), odd);
    }

    /// A rollback past a written chunk cuts it back with zeros, and the entries before
    /// the mark that it held are written again.
    #[test]
    fn rollback_past_a_written_chunk_rewrites_the_kept_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let v = DeltaVocab::open(&path).unwrap();
        v.insert(b"<urn:a>").unwrap();
        v.sync().unwrap();
        // a term of a transaction that committed nothing, still pending
        v.insert(b"<urn:kept>").unwrap();
        let mark = v.mark();
        v.insert(b"<urn:dropped>").unwrap();
        v.flush().unwrap();
        v.rollback(&mark).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v.needs_sync());
        v.insert(b"<urn:next>").unwrap();
        v.sync_direct().unwrap();
        drop(v);
        let v = DeltaVocab::open(&path).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.get(1).unwrap(), b"<urn:kept>");
        assert_eq!(v.get(2).unwrap(), b"<urn:next>");
    }

    /// A linked generation's own file numbers its entries from 0 within the file, after
    /// the upstream prefix in memory.
    #[test]
    fn a_layered_vocabulary_writes_its_own_entries_after_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let up = dir.path().join("up.vocab");
        let own = dir.path().join("own.vocab");
        let v = DeltaVocab::open(&up).unwrap();
        for k in [&b"<urn:a>"[..], b"<urn:b>", b"<urn:c>"] {
            v.insert(k).unwrap();
        }
        v.sync().unwrap();
        drop(v);
        let prefix = [(up.clone(), 2u64)];
        let v = DeltaVocab::open_layered(&prefix, &own, false).unwrap();
        assert_eq!(v.len(), 2);
        // the upstream's third term is not in the prefix: here it is new
        assert_eq!(v.insert(b"<urn:c>").unwrap(), 2);
        assert_eq!(v.insert(b"<urn:a>").unwrap(), 0);
        v.sync_direct().unwrap();
        drop(v);
        let p = std::fs::read(&own).unwrap();
        assert_eq!(parse_delta(&p).unwrap().keys, vec![&b"<urn:c>"[..]]);
        let v = DeltaVocab::open_layered(&prefix, &own, true).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.get(2).unwrap(), b"<urn:c>");
    }
}
